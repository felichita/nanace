//! P2P registry: binding a libp2p peer to its user, team presence, and peer
//! lookup. Message bodies never pass through the server.

use actix_web::{HttpResponse, web};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use nanace_core::p2p::{self, AcceptMode, AnswerHeader, AnswerStatus, ClientKind, RejectReason, RequestHeader};
use nanace_core::policy::{self, P2pPolicy};
use nanace_core::{OrgRole, TeamRole};
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::auth::{AnyScopeUser, AuthMethod, AuthUser, PeerUser};
use crate::error::{ApiError, ApiResult};
use crate::p2p::{bind_signature_is_valid, parse_policy_page, signature_is_valid};
use crate::ratelimit::RateClass;
use crate::{AppState, LOG_COMPONENT, access, db, metrics};

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/me/peers/challenge", web::post().to(challenge))
        .route("/me/peers", web::post().to(bind_peer))
        .route("/me/peers", web::get().to(my_peers))
        .route("/me/peers/lookup/{peer_id}", web::get().to(lookup_peer))
        .route("/me/peers/{peer_id}", web::delete().to(revoke_peer))
        .route("/orgs/{org_id}/teams/{team_id}/presence", web::put().to(put_presence))
        // An agent label may contain `/`, so it takes the rest of the path.
        .route(
            "/orgs/{org_id}/teams/{team_id}/presence/{peer_id}/{agent_label:.*}",
            web::delete().to(delete_presence),
        )
        .route("/orgs/{org_id}/teams/{team_id}/peers", web::get().to(list_peers))
        .route("/orgs/{org_id}/teams/{team_id}/p2p/policy", web::get().to(get_policy))
        .route(
            "/orgs/{org_id}/teams/{team_id}/p2p/exchanges",
            web::post().to(report_exchange),
        )
        .route(
            "/orgs/{org_id}/teams/{team_id}/p2p/exchanges",
            web::get().to(list_exchanges),
        )
        .route("/p2p/policy-template", web::get().to(policy_template));
}

/// Lowest presence TTL a client may ask for.
const MIN_PRESENCE_TTL_SECS: u32 = 15;
/// Agents one peer may announce in one team.
const MAX_AGENTS_PER_PEER: i64 = 16;
const DEFAULT_PEERS_LIMIT: i64 = 50;
const MAX_PEERS_LIMIT: i64 = 200;

async fn challenge(state: web::Data<AppState>, user: PeerUser) -> ApiResult<HttpResponse> {
    state.rate_limiter.check(user.user_id, RateClass::P2pChallenge)?;
    let issued = state.p2p.issue_challenge(user.user_id)?;
    Ok(HttpResponse::Ok().json(serde_json::json!({
        "challenge": issued.challenge,
        "expires_at": issued.expires_at,
    })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BindRequest {
    peer_id: String,
    /// From `POST /me/peers/challenge`.
    challenge: String,
    /// base64url Ed25519 signature over `nanace_core::p2p::bind_message`.
    signature: String,
    client_kind: ClientKind,
    client_version: String,
}

#[derive(Serialize, sqlx::FromRow)]
struct PeerRow {
    peer_id: String,
    client_kind: String,
    client_version: String,
    created_at: DateTime<Utc>,
    last_seen_at: Option<DateTime<Utc>>,
    revoked_at: Option<DateTime<Utc>>,
}

/// Binds a peer id to the caller after checking that the caller holds its
/// key. Binding the same peer again only refreshes its client fields.
async fn bind_peer(
    state: web::Data<AppState>,
    user: PeerUser,
    body: web::Json<BindRequest>,
) -> ApiResult<HttpResponse> {
    state.rate_limiter.check(user.user_id, RateClass::P2pBind)?;
    let body = body.into_inner();
    let public_key = p2p::peer_id(&body.peer_id)?;
    p2p::client_version(&body.client_version)?;
    if !state.p2p.challenge_is_valid(user.user_id, &body.challenge) {
        return Err(ApiError::BadRequest("challenge is invalid or expired".to_owned()));
    }
    if !bind_signature_is_valid(
        &public_key,
        &body.signature,
        &body.challenge,
        &body.peer_id,
        user.user_id,
    ) {
        return Err(ApiError::BadRequest(
            "signature does not prove ownership of the peer key".to_owned(),
        ));
    }

    let mut tx = db::user_tx(&state.pool, user.user_id).await?;
    // Sees the caller's peers and their teammates'; anyone else's bound peer
    // surfaces as a unique violation on insert.
    let existing: Option<(Uuid, Option<DateTime<Utc>>)> =
        sqlx::query_as("SELECT user_id, revoked_at FROM p2p_peers WHERE peer_id = $1 FOR UPDATE")
            .bind(&body.peer_id)
            .fetch_optional(&mut *tx)
            .await?;
    let bound_elsewhere = || ApiError::Conflict("peer id is bound to another account".to_owned());
    match existing {
        Some((owner, _)) if owner != user.user_id => return Err(bound_elsewhere()),
        Some((_, Some(_))) => {
            return Err(ApiError::Conflict("peer id was revoked; generate a new key".to_owned()));
        }
        Some((_, None)) => {
            let row: PeerRow = sqlx::query_as(
                "UPDATE p2p_peers SET client_kind = $2, client_version = $3 WHERE peer_id = $1 \
                 RETURNING peer_id, client_kind, client_version, created_at, last_seen_at, revoked_at",
            )
            .bind(&body.peer_id)
            .bind(body.client_kind.as_str())
            .bind(&body.client_version)
            .fetch_one(&mut *tx)
            .await?;
            tx.commit().await?;
            return Ok(HttpResponse::Ok().json(row));
        }
        None => {}
    }

    let active: i64 = sqlx::query_scalar("SELECT count(*) FROM p2p_peers WHERE user_id = $1 AND revoked_at IS NULL")
        .bind(user.user_id)
        .fetch_one(&mut *tx)
        .await?;
    if active >= i64::from(state.p2p.max_peers_per_user) {
        return Err(ApiError::Conflict("too many bound peers; revoke one first".to_owned()));
    }
    let inserted: Result<PeerRow, sqlx::Error> = sqlx::query_as(
        "INSERT INTO p2p_peers (peer_id, user_id, public_key, client_kind, client_version) \
         VALUES ($1, $2, $3, $4, $5) \
         RETURNING peer_id, client_kind, client_version, created_at, last_seen_at, revoked_at",
    )
    .bind(&body.peer_id)
    .bind(user.user_id)
    .bind(public_key.as_slice())
    .bind(body.client_kind.as_str())
    .bind(&body.client_version)
    .fetch_one(&mut *tx)
    .await;
    let row = match inserted {
        Ok(row) => row,
        Err(err) if err.as_database_error().is_some_and(|db| db.is_unique_violation()) => {
            return Err(bound_elsewhere());
        }
        Err(err) => return Err(err.into()),
    };
    audit::record(
        &mut *tx,
        AuditEvent {
            org_id: None,
            actor: user.user_id,
            action: "p2p.peer_bind",
            resource_type: "p2p_peer",
            resource_id: None,
            details: serde_json::json!({ "peer_id": body.peer_id, "client_kind": body.client_kind }),
        },
    )
    .await?;
    tx.commit().await?;
    metrics::p2p_peer_bound();
    Ok(HttpResponse::Created().json(row))
}

async fn my_peers(state: web::Data<AppState>, user: AnyScopeUser) -> ApiResult<HttpResponse> {
    let mut tx = db::user_tx(&state.pool, user.user_id).await?;
    let mine: Vec<PeerRow> = sqlx::query_as(
        "SELECT peer_id, client_kind, client_version, created_at, last_seen_at, revoked_at \
         FROM p2p_peers WHERE user_id = $1 ORDER BY created_at DESC, peer_id",
    )
    .bind(user.user_id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(HttpResponse::Ok().json(mine))
}

/// Revokes one of the caller's peers for good and ends its presence
/// everywhere. Someone else's peer is `404`, like one that does not exist.
async fn revoke_peer(state: web::Data<AppState>, user: PeerUser, path: web::Path<String>) -> ApiResult<HttpResponse> {
    let peer_id = path.into_inner();
    let mut tx = db::user_tx(&state.pool, user.user_id).await?;
    let revoked = sqlx::query(
        "UPDATE p2p_peers SET revoked_at = now() WHERE peer_id = $1 AND user_id = $2 AND revoked_at IS NULL",
    )
    .bind(&peer_id)
    .bind(user.user_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if revoked == 0 {
        return Err(ApiError::NotFound);
    }
    let presence_removed = sqlx::query("DELETE FROM p2p_presence WHERE peer_id = $1 AND user_id = $2")
        .bind(&peer_id)
        .bind(user.user_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    audit::record(
        &mut *tx,
        AuditEvent {
            org_id: None,
            actor: user.user_id,
            action: "p2p.peer_revoke",
            resource_type: "p2p_peer",
            resource_id: None,
            details: serde_json::json!({ "peer_id": peer_id, "presence_removed": presence_removed }),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(HttpResponse::NoContent().finish())
}

#[derive(Serialize, sqlx::FromRow)]
struct SharedTeam {
    org_id: Uuid,
    team_id: Uuid,
    /// The peer owner's role in the team.
    role: String,
}

#[derive(Serialize)]
struct PeerLookup {
    peer_id: String,
    user_id: Uuid,
    email: String,
    /// base64url Ed25519 public key.
    public_key: String,
    /// Teams the caller shares with the peer where the peer is present now.
    shared: Vec<SharedTeam>,
}

/// Who a peer is, for a caller that shares a team with it where the peer is
/// present right now. Everything else is `404`, so peer ids cannot be probed.
async fn lookup_peer(
    state: web::Data<AppState>,
    user: AnyScopeUser,
    path: web::Path<String>,
) -> ApiResult<HttpResponse> {
    state.rate_limiter.check(user.user_id, RateClass::P2pLookup)?;
    let peer_id = path.into_inner();
    let mut tx = db::user_tx(&state.pool, user.user_id).await?;
    let owner: Option<(Uuid, String, Vec<u8>)> = sqlx::query_as(
        "SELECT p.user_id, u.email, p.public_key FROM p2p_peers p JOIN users u ON u.id = p.user_id \
         WHERE p.peer_id = $1 AND p.revoked_at IS NULL AND u.deleted_at IS NULL",
    )
    .bind(&peer_id)
    .fetch_optional(&mut *tx)
    .await?;
    let (user_id, email, public_key) = owner.ok_or(ApiError::NotFound)?;
    // Row security limits presence to the caller's teams; the join keeps the
    // teams whose policy enables P2P.
    let shared: Vec<SharedTeam> = sqlx::query_as(
        "SELECT DISTINCT pr.org_id, pr.team_id, m.role FROM p2p_presence pr \
         JOIN team_members m ON m.team_id = pr.team_id AND m.user_id = pr.user_id \
         JOIN p2p_policies pp ON pp.team_id = pr.team_id AND pp.valid AND (pp.parsed->>'enabled')::boolean \
         WHERE pr.peer_id = $1 AND pr.expires_at > now() ORDER BY pr.org_id, pr.team_id",
    )
    .bind(&peer_id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    if shared.is_empty() {
        return Err(ApiError::NotFound);
    }
    Ok(HttpResponse::Ok().json(PeerLookup {
        peer_id,
        user_id,
        email,
        public_key: URL_SAFE_NO_PAD.encode(public_key),
        shared,
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PresenceRequest {
    peer_id: String,
    agent_label: String,
    #[serde(default)]
    capabilities: Vec<String>,
    #[serde(default)]
    multiaddrs: Vec<String>,
    accept_mode: AcceptMode,
    /// Seconds until the presence expires unless announced again.
    ttl_secs: u32,
}

/// Announces or extends the caller's presence in a team. Called repeatedly
/// as a heartbeat; only the start of a presence is audited.
async fn put_presence(
    state: web::Data<AppState>,
    user: PeerUser,
    path: web::Path<(Uuid, Uuid)>,
    body: web::Json<PresenceRequest>,
) -> ApiResult<HttpResponse> {
    let (org_id, team_id) = path.into_inner();
    access::require_team_role(&state.pool, org_id, team_id, user.user_id, TeamRole::Reader).await?;
    state.rate_limiter.check(user.user_id, RateClass::P2pPresence)?;
    let body = body.into_inner();
    p2p::peer_id(&body.peer_id)?;
    p2p::agent_label(&body.agent_label)?;
    let capabilities = p2p::capabilities(&body.capabilities)?;
    let multiaddrs = p2p::multiaddrs(&body.multiaddrs)?;
    let max_ttl = state.p2p.presence_max_ttl_secs;
    if !(MIN_PRESENCE_TTL_SECS..=max_ttl).contains(&body.ttl_secs) {
        return Err(ApiError::BadRequest(format!(
            "ttl_secs must be {MIN_PRESENCE_TTL_SECS}-{max_ttl}"
        )));
    }
    // Recorded so that revoking the token's chain ends this presence.
    let chain_id: Option<Uuid> = match user.method {
        AuthMethod::Session => None,
        AuthMethod::Token { id, .. } => {
            sqlx::query_scalar("SELECT chain_id FROM api_tokens WHERE id = $1")
                .bind(id)
                .fetch_optional(&state.pool)
                .await?
        }
    };

    let mut tx = db::scoped_tx(&state.pool, org_id, user.user_id).await?;
    // The caller's own, unrevoked peer; anything else looks like no peer.
    let client_kind: Option<String> = sqlx::query_scalar(
        "SELECT client_kind FROM p2p_peers WHERE peer_id = $1 AND user_id = $2 AND revoked_at IS NULL FOR UPDATE",
    )
    .bind(&body.peer_id)
    .bind(user.user_id)
    .fetch_optional(&mut *tx)
    .await?;
    let client_kind = client_kind.ok_or(ApiError::NotFound)?;
    // No valid, enabled policy means no P2P in this team.
    let refuse = |reason: &'static str, error: ApiError| {
        metrics::p2p_policy_denied(reason);
        Err(error)
    };
    let Some(policy) = load_policy(&mut tx, team_id).await?.filter(|p| p.enabled) else {
        return refuse(
            "disabled",
            ApiError::Conflict(
                "P2P is not enabled for this team: its owner must add a valid `Policy: P2P` page".to_owned(),
            ),
        );
    };
    if body.accept_mode > policy.max_accept_mode {
        return refuse(
            "accept_mode",
            ApiError::BadRequest(format!(
                "accept_mode: the team policy allows at most `{}`",
                policy.max_accept_mode.as_str()
            )),
        );
    }
    if client_kind == ClientKind::Server.as_str() && !policy.server_peers.users.contains(&user.email) {
        return refuse(
            "server_peer",
            ApiError::Conflict(
                "this account is not listed in `server_peers` of the team policy, so it cannot run a peer \
                 without the desktop app"
                    .to_owned(),
            ),
        );
    }
    if !policy.discovery.lan_addrs && multiaddrs.iter().any(|a| p2p::is_private_multiaddr(a)) {
        return refuse(
            "lan_addr",
            ApiError::BadRequest("multiaddrs: the team policy does not share private addresses".to_owned()),
        );
    }
    let live: Option<bool> = sqlx::query_scalar(
        "SELECT expires_at > now() FROM p2p_presence WHERE team_id = $1 AND peer_id = $2 AND agent_label = $3",
    )
    .bind(team_id)
    .bind(&body.peer_id)
    .bind(&body.agent_label)
    .fetch_optional(&mut *tx)
    .await?;
    let starting = live != Some(true);
    if live.is_none() {
        let agents: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM p2p_presence WHERE team_id = $1 AND peer_id = $2 AND expires_at > now()",
        )
        .bind(team_id)
        .bind(&body.peer_id)
        .fetch_one(&mut *tx)
        .await?;
        if agents >= MAX_AGENTS_PER_PEER {
            return Err(ApiError::Conflict("too many agents announced by this peer".to_owned()));
        }
    }
    let expires_at: DateTime<Utc> = sqlx::query_scalar(
        "INSERT INTO p2p_presence (org_id, team_id, user_id, peer_id, agent_label, capabilities, multiaddrs, \
                                   accept_mode, auth_chain_id, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, now() + make_interval(secs => $10)) \
         ON CONFLICT (team_id, peer_id, agent_label) DO UPDATE SET \
             capabilities = EXCLUDED.capabilities, multiaddrs = EXCLUDED.multiaddrs, \
             accept_mode = EXCLUDED.accept_mode, auth_chain_id = EXCLUDED.auth_chain_id, \
             started_at = CASE WHEN p2p_presence.expires_at > now() THEN p2p_presence.started_at ELSE now() END, \
             updated_at = now(), expires_at = EXCLUDED.expires_at \
         RETURNING expires_at",
    )
    .bind(org_id)
    .bind(team_id)
    .bind(user.user_id)
    .bind(&body.peer_id)
    .bind(&body.agent_label)
    .bind(&capabilities)
    .bind(&multiaddrs)
    .bind(body.accept_mode.as_str())
    .bind(chain_id)
    .bind(f64::from(body.ttl_secs))
    .fetch_one(&mut *tx)
    .await?;
    if starting {
        sqlx::query("UPDATE p2p_peers SET last_seen_at = now() WHERE peer_id = $1")
            .bind(&body.peer_id)
            .execute(&mut *tx)
            .await?;
        audit::record(
            &mut *tx,
            AuditEvent {
                org_id: Some(org_id),
                actor: user.user_id,
                action: "p2p.presence_put",
                resource_type: "team",
                resource_id: Some(team_id),
                details: serde_json::json!({
                    "peer_id": body.peer_id,
                    "agent_label": body.agent_label,
                    "accept_mode": body.accept_mode,
                }),
            },
        )
        .await?;
    }
    tx.commit().await?;
    if starting {
        metrics::p2p_presence_started();
    }
    Ok(HttpResponse::Ok().json(serde_json::json!({
        "expires_at": expires_at,
        "heartbeat_secs": (body.ttl_secs / 3).max(5),
    })))
}

/// Ends one of the caller's presences in a team.
async fn delete_presence(
    state: web::Data<AppState>,
    user: PeerUser,
    path: web::Path<(Uuid, Uuid, String, String)>,
) -> ApiResult<HttpResponse> {
    let (org_id, team_id, peer_id, agent_label) = path.into_inner();
    access::require_team_role(&state.pool, org_id, team_id, user.user_id, TeamRole::Reader).await?;
    let mut tx = db::scoped_tx(&state.pool, org_id, user.user_id).await?;
    let removed = sqlx::query(
        "DELETE FROM p2p_presence WHERE team_id = $1 AND peer_id = $2 AND agent_label = $3 AND user_id = $4",
    )
    .bind(team_id)
    .bind(&peer_id)
    .bind(&agent_label)
    .bind(user.user_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if removed == 0 {
        return Err(ApiError::NotFound);
    }
    audit::record(
        &mut *tx,
        AuditEvent {
            org_id: Some(org_id),
            actor: user.user_id,
            action: "p2p.presence_delete",
            resource_type: "team",
            resource_id: Some(team_id),
            details: serde_json::json!({ "peer_id": peer_id, "agent_label": agent_label }),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(HttpResponse::NoContent().finish())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PeersQuery {
    capability: Option<String>,
    limit: Option<i64>,
}

#[derive(Serialize, sqlx::FromRow)]
struct PresentPeer {
    peer_id: String,
    agent_label: String,
    user_id: Uuid,
    email: String,
    capabilities: Vec<String>,
    multiaddrs: Vec<String>,
    accept_mode: String,
    client_kind: String,
    expires_at: DateTime<Utc>,
}

/// Peers present in a team right now, optionally those announcing a
/// capability. Visible to every member of the team.
async fn list_peers(
    state: web::Data<AppState>,
    user: AnyScopeUser,
    path: web::Path<(Uuid, Uuid)>,
    query: web::Query<PeersQuery>,
) -> ApiResult<HttpResponse> {
    let (org_id, team_id) = path.into_inner();
    let role = access::require_team_role(&state.pool, org_id, team_id, user.user_id, TeamRole::Reader).await?;
    let limit = query.limit.unwrap_or(DEFAULT_PEERS_LIMIT);
    if !(1..=MAX_PEERS_LIMIT).contains(&limit) {
        return Err(ApiError::BadRequest(format!("limit must be 1-{MAX_PEERS_LIMIT}")));
    }
    let capability = query.capability.as_ref().map(|c| c.trim().to_ascii_lowercase());

    let mut tx = db::scoped_tx(&state.pool, org_id, user.user_id).await?;
    let stored = stored_policy(&mut tx, team_id).await?;
    let policy = stored.as_ref().and_then(|s| s.policy.as_ref());
    let summary = PolicySummary {
        valid: policy.is_some(),
        enabled: policy.is_some_and(|p| p.enabled),
        version: stored.as_ref().map(|s| s.version.clone()),
    };
    // Default deny: without a policy that shows peers to this role, nobody
    // is listed.
    if !policy.is_some_and(|p| p.visible_to(role)) {
        tx.commit().await?;
        return Ok(HttpResponse::Ok().json(PeerList {
            policy: summary,
            peers: Vec::new(),
        }));
    }
    let peers: Vec<PresentPeer> = sqlx::query_as(
        "SELECT pr.peer_id, pr.agent_label, pr.user_id, u.email, pr.capabilities, pr.multiaddrs, pr.accept_mode, \
                p.client_kind, pr.expires_at \
         FROM p2p_presence pr JOIN p2p_peers p ON p.peer_id = pr.peer_id JOIN users u ON u.id = pr.user_id \
         WHERE pr.org_id = $1 AND pr.team_id = $2 AND pr.expires_at > now() AND p.revoked_at IS NULL \
           AND u.deleted_at IS NULL AND ($3::text IS NULL OR $3 = ANY(pr.capabilities)) \
         ORDER BY pr.updated_at DESC, pr.peer_id, pr.agent_label LIMIT $4",
    )
    .bind(org_id)
    .bind(team_id)
    .bind(capability)
    .bind(limit)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(HttpResponse::Ok().json(PeerList { policy: summary, peers }))
}

#[derive(Serialize)]
struct PolicySummary {
    valid: bool,
    enabled: bool,
    version: Option<String>,
}

#[derive(Serialize)]
struct PeerList {
    policy: PolicySummary,
    peers: Vec<PresentPeer>,
}

/// A team's policy row.
struct StoredPolicy {
    context_id: Uuid,
    version: String,
    /// `None` when the page did not validate.
    policy: Option<P2pPolicy>,
    errors: Vec<String>,
}

/// The team's stored policy, if it has a `Policy: P2P` page the server
/// parsed. A stored policy this version cannot read counts as invalid, so
/// P2P stays off until the owner saves the page again.
async fn stored_policy(tx: &mut Transaction<'static, Postgres>, team_id: Uuid) -> ApiResult<Option<StoredPolicy>> {
    let row: Option<(Uuid, String, Option<serde_json::Value>, serde_json::Value)> =
        sqlx::query_as("SELECT context_id, version, parsed, errors FROM p2p_policies WHERE team_id = $1")
            .bind(team_id)
            .fetch_optional(&mut **tx)
            .await?;
    let Some((context_id, version, parsed, errors)) = row else {
        return Ok(None);
    };
    let mut errors: Vec<String> = serde_json::from_value(errors).map_err(anyhow::Error::from)?;
    let policy = match parsed.map(serde_json::from_value::<P2pPolicy>) {
        Some(Ok(policy)) => Some(policy),
        Some(Err(err)) => {
            tracing::warn!(
                event = "p2p_policy_unreadable",
                component = LOG_COMPONENT,
                team_id = %team_id,
                error = %err,
                "stored P2P policy does not match this version; treating it as invalid"
            );
            errors.push("the policy was saved by another version of nanace; save the page again".to_owned());
            None
        }
        None => None,
    };
    Ok(Some(StoredPolicy {
        context_id,
        version,
        policy,
        errors,
    }))
}

/// The team's policy when it is valid.
async fn load_policy(tx: &mut Transaction<'static, Postgres>, team_id: Uuid) -> ApiResult<Option<P2pPolicy>> {
    Ok(stored_policy(tx, team_id).await?.and_then(|s| s.policy))
}

/// Refuses a `Policy: P2P` page that would not validate, before anything is
/// written. Other titles pass.
pub(crate) fn check_policy_page(title: &str, content: &str) -> ApiResult<()> {
    if !policy::is_policy_page_title(title) {
        return Ok(());
    }
    match parse_policy_page(content).policy {
        Ok(_) => Ok(()),
        Err(errors) => Err(ApiError::BadRequest(format!(
            "Policy: P2P is not valid: {}",
            errors.join("; ")
        ))),
    }
}

/// What a write did to a page that is, was or becomes the team's policy.
pub(crate) enum PolicyChange<'a> {
    /// The page now holds this content under the policy title.
    Saved(&'a str),
    /// The page was trashed or renamed away from the policy title.
    Removed,
}

/// Keeps `p2p_policies` in step with the team's `Policy: P2P` page. Runs in
/// the transaction of the page write and audits `p2p.policy_update`. A
/// second policy page in a team is a conflict. Invalid content is stored as
/// such (callers that must refuse it call [`check_policy_page`] first), which
/// is what a restore from the trash relies on.
pub(crate) async fn sync_policy(
    tx: &mut Transaction<'static, Postgres>,
    org_id: Uuid,
    team_id: Uuid,
    user_id: Uuid,
    context_id: Uuid,
    change: PolicyChange<'_>,
) -> ApiResult<()> {
    let existing: Option<(Uuid, String)> =
        sqlx::query_as("SELECT context_id, version FROM p2p_policies WHERE team_id = $1 FOR UPDATE")
            .bind(team_id)
            .fetch_optional(&mut **tx)
            .await?;
    let old_version = existing.as_ref().map(|(_, version)| version.clone());
    let owner = existing.map(|(owner, _)| owner);
    let (new_version, valid) = match change {
        PolicyChange::Removed => {
            if owner != Some(context_id) {
                return Ok(());
            }
            sqlx::query("DELETE FROM p2p_policies WHERE team_id = $1")
                .bind(team_id)
                .execute(&mut **tx)
                .await?;
            (None, false)
        }
        PolicyChange::Saved(content) => {
            if owner.is_some_and(|owner| owner != context_id) {
                return Err(ApiError::Conflict(
                    "this team already has a `Policy: P2P` page; edit that one".to_owned(),
                ));
            }
            let parsed = parse_policy_page(content);
            let (policy, errors) = match parsed.policy {
                Ok(policy) => (
                    Some(serde_json::to_value(policy).map_err(anyhow::Error::from)?),
                    Vec::new(),
                ),
                Err(errors) => (None, errors),
            };
            let valid = policy.is_some();
            sqlx::query(
                "INSERT INTO p2p_policies (team_id, org_id, context_id, version, parsed, valid, errors, updated_by) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
                 ON CONFLICT (team_id) DO UPDATE SET version = EXCLUDED.version, parsed = EXCLUDED.parsed, \
                     valid = EXCLUDED.valid, errors = EXCLUDED.errors, updated_by = EXCLUDED.updated_by, \
                     updated_at = now()",
            )
            .bind(team_id)
            .bind(org_id)
            .bind(context_id)
            .bind(&parsed.version)
            .bind(policy)
            .bind(valid)
            .bind(serde_json::json!(errors))
            .bind(user_id)
            .execute(&mut **tx)
            .await?;
            (Some(parsed.version), valid)
        }
    };
    audit::record(
        &mut **tx,
        AuditEvent {
            org_id: Some(org_id),
            actor: user_id,
            action: "p2p.policy_update",
            resource_type: "team",
            resource_id: Some(team_id),
            details: serde_json::json!({
                "context_id": context_id,
                "old_version": old_version,
                "new_version": new_version,
                "valid": valid,
            }),
        },
    )
    .await?;
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyQuery {
    /// Also return the page's guidance for agents; a content read, audited.
    text: Option<bool>,
}

#[derive(Serialize)]
struct PolicyResponse {
    team_id: Uuid,
    /// Hash of the policy page; null when the team has none.
    version: Option<String>,
    valid: bool,
    enabled: bool,
    policy: Option<P2pPolicy>,
    errors: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
}

/// The team's parsed P2P policy, for any member. A team without a valid
/// policy gets `valid: false` and the reasons, which means P2P is off.
async fn get_policy(
    state: web::Data<AppState>,
    user: AnyScopeUser,
    path: web::Path<(Uuid, Uuid)>,
    query: web::Query<PolicyQuery>,
) -> ApiResult<HttpResponse> {
    let (org_id, team_id) = path.into_inner();
    access::require_team_role(&state.pool, org_id, team_id, user.user_id, TeamRole::Reader).await?;
    let mut tx = db::scoped_tx(&state.pool, org_id, user.user_id).await?;
    let stored = stored_policy(&mut tx, team_id).await?;
    let mut response = PolicyResponse {
        team_id,
        version: None,
        valid: false,
        enabled: false,
        policy: None,
        errors: Vec::new(),
        text: None,
    };
    match stored {
        Some(stored) => {
            if query.text == Some(true) {
                let content: Option<String> = sqlx::query_scalar("SELECT content FROM contexts WHERE id = $1")
                    .bind(stored.context_id)
                    .fetch_optional(&mut *tx)
                    .await?;
                response.text = content.as_deref().map(policy::guidance_text);
                audit::record(
                    &mut *tx,
                    AuditEvent {
                        org_id: Some(org_id),
                        actor: user.user_id,
                        action: "context.read",
                        resource_type: "context",
                        resource_id: Some(stored.context_id),
                        details: serde_json::json!({ "team_id": team_id, "via": "p2p_policy" }),
                    },
                )
                .await?;
            }
            response.version = Some(stored.version);
            response.valid = stored.policy.is_some();
            response.enabled = stored.policy.as_ref().is_some_and(|p| p.enabled);
            response.policy = stored.policy;
            response.errors = stored.errors;
        }
        None => {
            // A page from before policies were validated has no row.
            let titles: Vec<String> = sqlx::query_scalar(
                "SELECT title FROM contexts WHERE org_id = $1 AND team_id = $2 AND nanace_is_protected_title(title)",
            )
            .bind(org_id)
            .bind(team_id)
            .fetch_all(&mut *tx)
            .await?;
            response
                .errors
                .push(if titles.iter().any(|t| policy::is_policy_page_title(t)) {
                    "the `Policy: P2P` page was saved before policies were checked; its owner must save it again"
                        .to_owned()
                } else {
                    "this team has no `Policy: P2P` page, so P2P is off".to_owned()
                });
        }
    }
    tx.commit().await?;
    Ok(HttpResponse::Ok().json(response))
}

/// A starting point for a team's `Policy: P2P` page, with strict defaults.
async fn policy_template(_user: AuthUser) -> HttpResponse {
    HttpResponse::Ok().json(serde_json::json!({
        "title": "Policy: P2P",
        "content": policy::TEMPLATE,
    }))
}

/// Which end of an exchange is reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ReportRole {
    Sender,
    Receiver,
}

/// How an exchange stands, when the report carries no signed answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ReportedStatus {
    /// The receiver took the request; no answer yet.
    Delivered,
    /// Refused before it left, or by the receiver without a signed answer.
    Rejected,
    /// The deadline passed.
    Expired,
    /// It could not be delivered or processed.
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Consent {
    Auto,
    UserApproved,
    UserDenied,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExchangeReport {
    role: ReportRole,
    /// base64url of the request header bytes exactly as they were signed.
    request_header: String,
    /// base64url Ed25519 signature of the sender's peer key over them.
    request_signature: String,
    status: ReportedStatus,
    reject_reason: Option<RejectReason>,
    consent: Option<Consent>,
    /// base64url of the signed answer header, once there is an answer.
    answer_header: Option<String>,
    answer_signature: Option<String>,
}

fn decode(field: &'static str, value: &str) -> ApiResult<Vec<u8>> {
    URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| ApiError::BadRequest(format!("{field}: must be base64url")))
}

/// Records one side's view of an exchange. The request header is the
/// sender's signed statement of who sent what kind of message to whom, so
/// the row's facts come from that header, not from the reporter. Reporting
/// again updates the status (delivered, then answered).
async fn report_exchange(
    state: web::Data<AppState>,
    user: PeerUser,
    path: web::Path<(Uuid, Uuid)>,
    body: web::Json<ExchangeReport>,
) -> ApiResult<HttpResponse> {
    let (org_id, team_id) = path.into_inner();
    access::require_team_role(&state.pool, org_id, team_id, user.user_id, TeamRole::Reader).await?;
    state.rate_limiter.check(user.user_id, RateClass::P2pExchange)?;
    let report = body.into_inner();
    let invalid = |what: &str| ApiError::BadRequest(format!("exchange report: {what}"));

    let header_bytes = decode("request_header", &report.request_header)?;
    let header: RequestHeader =
        serde_json::from_slice(&header_bytes).map_err(|_| invalid("request_header is not a request header"))?;
    let sender_key = p2p::peer_id(&header.from.peer_id)?;
    let receiver_key = p2p::peer_id(&header.to.peer_id)?;
    p2p::agent_label(&header.from.agent_label)?;
    p2p::agent_label(&header.to.agent_label)?;
    p2p::trace_id(&header.trace_id)?;
    if !signature_is_valid(
        &sender_key,
        &report.request_signature,
        &p2p::request_signing_bytes(&header_bytes),
    ) {
        return Err(invalid("request_signature is not the sender's signature"));
    }
    if header.org_id != org_id || header.team_id != team_id {
        return Err(invalid("the request belongs to another team"));
    }
    let reporter_is_party = match report.role {
        ReportRole::Sender => header.from.user_id == user.user_id,
        ReportRole::Receiver => header.to.user_id == user.user_id,
    };
    if !reporter_is_party {
        return Err(invalid("you are not that side of the exchange"));
    }
    let sent_at = DateTime::<Utc>::from_timestamp(header.sent_at, 0).ok_or_else(|| invalid("sent_at"))?;
    let request_hash = p2p::sha256_hex(&header_bytes);

    // A signed answer decides the status; without one the reporter states it.
    let answer = match (&report.answer_header, &report.answer_signature) {
        (Some(answer), Some(signature)) => {
            let bytes = decode("answer_header", answer)?;
            let answer: AnswerHeader =
                serde_json::from_slice(&bytes).map_err(|_| invalid("answer_header is not an answer header"))?;
            if !signature_is_valid(&receiver_key, signature, &p2p::answer_signing_bytes(&bytes)) {
                return Err(invalid("answer_signature is not the receiver's signature"));
            }
            if answer.request_id != header.request_id
                || answer.request_hash != request_hash
                || answer.answered_by.peer_id != header.to.peer_id
            {
                return Err(invalid("the answer belongs to another request"));
            }
            Some(answer)
        }
        (None, None) => None,
        _ => return Err(invalid("answer_header and answer_signature go together")),
    };
    let (status, reason, bytes_in) = match &answer {
        Some(answer) => (
            match answer.status {
                AnswerStatus::Answered => "answered",
                AnswerStatus::Rejected => "rejected",
                AnswerStatus::Expired => "expired",
                AnswerStatus::Error => "failed",
            },
            answer.reason,
            answer.body_bytes,
        ),
        None => (
            match report.status {
                ReportedStatus::Delivered => "delivered",
                ReportedStatus::Rejected => "rejected",
                ReportedStatus::Expired => "expired",
                ReportedStatus::Failed => "failed",
            },
            report.reject_reason,
            0,
        ),
    };
    let consent = report.consent.map(|c| match c {
        Consent::Auto => "auto",
        Consent::UserApproved => "user_approved",
        Consent::UserDenied => "user_denied",
    });
    let (role, bytes_out) = (
        match report.role {
            ReportRole::Sender => "sender",
            ReportRole::Receiver => "receiver",
        },
        i32::try_from(header.body_bytes).map_err(|_| invalid("body_bytes"))?,
    );

    let mut tx = db::scoped_tx(&state.pool, org_id, user.user_id).await?;
    // Both peers must be bound to the users the header names. Row security
    // shows the caller its own peers and its teammates'.
    let bound: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM p2p_peers WHERE (peer_id = $1 AND user_id = $2) OR (peer_id = $3 AND user_id = $4)",
    )
    .bind(&header.from.peer_id)
    .bind(header.from.user_id)
    .bind(&header.to.peer_id)
    .bind(header.to.user_id)
    .fetch_one(&mut *tx)
    .await?;
    if bound != 2 {
        return Err(invalid("a peer in the request is not bound to the user it names"));
    }
    sqlx::query(
        "INSERT INTO p2p_exchanges (id, org_id, team_id, request_id, reported_by, reporter_user, trace_id, \
             from_peer, from_agent, from_user, to_peer, to_agent, to_user, kind, status, reject_reason, consent, \
             hop, bytes_out, bytes_in, request_hash, answer_verified, policy_version, sent_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21, \
                 $22, $23, $24) \
         ON CONFLICT (request_id, reported_by) DO UPDATE SET status = EXCLUDED.status, \
             reject_reason = EXCLUDED.reject_reason, consent = COALESCE(EXCLUDED.consent, p2p_exchanges.consent), \
             bytes_in = EXCLUDED.bytes_in, answer_verified = EXCLUDED.answer_verified, updated_at = now()",
    )
    .bind(Uuid::new_v4())
    .bind(org_id)
    .bind(team_id)
    .bind(header.request_id)
    .bind(role)
    .bind(user.user_id)
    .bind(&header.trace_id)
    .bind(&header.from.peer_id)
    .bind(&header.from.agent_label)
    .bind(header.from.user_id)
    .bind(&header.to.peer_id)
    .bind(&header.to.agent_label)
    .bind(header.to.user_id)
    .bind(header.kind.as_str())
    .bind(status)
    .bind(reason.map(RejectReason::as_str))
    .bind(consent)
    .bind(i32::try_from(header.hop).map_err(|_| invalid("hop"))?)
    .bind(bytes_out)
    .bind(i32::try_from(bytes_in).map_err(|_| invalid("answer body_bytes"))?)
    .bind(&request_hash)
    .bind(answer.is_some())
    .bind(&header.policy_version)
    .bind(sent_at)
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut *tx,
        AuditEvent {
            org_id: Some(org_id),
            actor: user.user_id,
            action: "p2p.exchange",
            resource_type: "team",
            resource_id: Some(team_id),
            details: serde_json::json!({
                "request_id": header.request_id,
                "trace_id": header.trace_id,
                "reported_by": role,
                "kind": header.kind,
                "status": status,
                "reject_reason": reason,
                "from_user": header.from.user_id,
                "to_user": header.to.user_id,
            }),
        },
    )
    .await?;
    tx.commit().await?;
    metrics::p2p_exchange_reported(header.kind.as_str(), status);
    Ok(HttpResponse::NoContent().finish())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExchangesQuery {
    trace_id: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Serialize, sqlx::FromRow)]
struct ExchangeRow {
    request_id: Uuid,
    reported_by: String,
    trace_id: String,
    from_peer: String,
    from_agent: String,
    from_user: Uuid,
    from_email: String,
    to_peer: String,
    to_agent: String,
    to_user: Uuid,
    to_email: String,
    kind: String,
    status: String,
    reject_reason: Option<String>,
    consent: Option<String>,
    hop: i32,
    bytes_out: i32,
    bytes_in: i32,
    answer_verified: bool,
    policy_version: String,
    sent_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    /// A page of the team is tagged `trace-<trace id>`. Always false for an
    /// org admin who is not in the team, who cannot see its pages.
    has_summary: bool,
}

/// The team's exchange journal, newest first: metadata only. For team
/// admins and org admins.
async fn list_exchanges(
    state: web::Data<AppState>,
    user: AuthUser,
    path: web::Path<(Uuid, Uuid)>,
    query: web::Query<ExchangesQuery>,
) -> ApiResult<HttpResponse> {
    let (org_id, team_id) = path.into_inner();
    match access::team_role(&state.pool, org_id, team_id, user.user_id).await? {
        (OrgRole::Admin, _) | (_, Some(TeamRole::Admin)) => {}
        (_, Some(_)) => return Err(ApiError::Forbidden),
        (_, None) => return Err(ApiError::NotFound),
    }
    let limit = query.limit.unwrap_or(100);
    let offset = query.offset.unwrap_or(0);
    if !(1..=500).contains(&limit) || offset < 0 {
        return Err(ApiError::BadRequest(
            "limit must be 1-500 and offset non-negative".to_owned(),
        ));
    }
    let mut tx = db::scoped_tx(&state.pool, org_id, user.user_id).await?;
    let rows: Vec<ExchangeRow> = sqlx::query_as(
        "SELECT e.request_id, e.reported_by, e.trace_id, e.from_peer, e.from_agent, e.from_user, \
                fu.email AS from_email, e.to_peer, e.to_agent, e.to_user, tu.email AS to_email, e.kind, e.status, \
                e.reject_reason, e.consent, e.hop, e.bytes_out, e.bytes_in, e.answer_verified, e.policy_version, \
                e.sent_at, e.updated_at, \
                EXISTS (SELECT 1 FROM contexts c WHERE c.team_id = e.team_id \
                        AND ('trace-' || lower(e.trace_id)) = ANY(c.tags)) AS has_summary \
         FROM p2p_exchanges e JOIN users fu ON fu.id = e.from_user JOIN users tu ON tu.id = e.to_user \
         WHERE e.org_id = $1 AND e.team_id = $2 AND ($3::text IS NULL OR e.trace_id = $3) \
         ORDER BY e.created_at DESC, e.id LIMIT $4 OFFSET $5",
    )
    .bind(org_id)
    .bind(team_id)
    .bind(query.trace_id.as_deref())
    .bind(limit)
    .bind(offset)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(HttpResponse::Ok().json(rows))
}
