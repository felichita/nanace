//! Internal API for the P2P network node, served on its own listener and
//! authenticated with the node secret. The node has no database access: this
//! is all it learns, and it is only which peer ids may connect.

use actix_web::http::header::AUTHORIZATION;
use actix_web::{HttpRequest, HttpResponse, web};
use secrecy::ExposeSecret;
use serde::Deserialize;
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::error::{ApiError, ApiResult};
use crate::ratelimit::RateClass;
use crate::{AppState, db};

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/internal/p2p/allowed", web::get().to(allowed_peers))
        .route("/internal/p2p/allow", web::get().to(allow_peer));
}

/// Fails closed: no configured secret means nobody is the node.
fn require_node(state: &AppState, req: &HttpRequest) -> ApiResult<()> {
    let presented = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    match (&state.p2p.node_secret, presented) {
        (Some(secret), Some(presented))
            if bool::from(secret.expose_secret().as_bytes().ct_eq(presented.as_bytes())) =>
        {
            Ok(())
        }
        _ => Err(ApiError::Unauthorized),
    }
}

/// A peer may use the node while it is bound to a live account, not revoked,
/// and present in at least one team whose policy enables P2P.
const ALLOWED_PEERS: &str = "SELECT DISTINCT p.peer_id FROM p2p_peers p \
     JOIN users u ON u.id = p.user_id AND u.deleted_at IS NULL \
     JOIN p2p_presence pr ON pr.peer_id = p.peer_id AND pr.expires_at > now() \
     JOIN p2p_policies pp ON pp.team_id = pr.team_id AND pp.valid AND (pp.parsed->>'enabled')::boolean \
     WHERE p.revoked_at IS NULL AND ($1::text IS NULL OR p.peer_id = $1) ORDER BY p.peer_id";

async fn allowed(state: &AppState, only: Option<&str>) -> ApiResult<Vec<String>> {
    let mut tx = db::p2p_system_tx(&state.pool).await?;
    let peers: Vec<String> = sqlx::query_scalar(ALLOWED_PEERS).bind(only).fetch_all(&mut *tx).await?;
    tx.commit().await?;
    Ok(peers)
}

/// Every peer id the node should admit right now.
async fn allowed_peers(state: web::Data<AppState>, req: HttpRequest) -> ApiResult<HttpResponse> {
    require_node(&state, &req)?;
    let peers = allowed(&state, None).await?;
    Ok(HttpResponse::Ok().json(serde_json::json!({ "peers": peers })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AllowQuery {
    peer_id: String,
}

/// Whether one peer id may connect, for a peer the node has not seen in its
/// last snapshot.
async fn allow_peer(
    state: web::Data<AppState>,
    req: HttpRequest,
    query: web::Query<AllowQuery>,
) -> ApiResult<HttpResponse> {
    require_node(&state, &req)?;
    // Each question is a database query that strangers connecting to the
    // node can cause; the node bounds them too.
    state.rate_limiter.check(Uuid::nil(), RateClass::P2pNodeAllow)?;
    if nanace_core::p2p::peer_id(&query.peer_id).is_err() {
        return Ok(HttpResponse::Ok().json(serde_json::json!({ "allowed": false })));
    }
    let allowed = !allowed(&state, Some(&query.peer_id)).await?.is_empty();
    Ok(HttpResponse::Ok().json(serde_json::json!({ "allowed": allowed })))
}
