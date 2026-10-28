mod audit_logs;
mod auth;
mod complete_page;
pub(crate) mod contexts;
mod google;
mod graph;
mod orgs;
mod search;
mod teams;
mod tokens;
mod trash;

use actix_web::{HttpResponse, web};

use crate::AppState;
use crate::error::ApiResult;

/// Request bodies above this are rejected before deserialization. Leaves room
/// for the 256 KiB content limit plus JSON escaping.
const JSON_LIMIT_BYTES: usize = 1024 * 1024;

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.app_data(web::JsonConfig::default().limit(JSON_LIMIT_BYTES))
        // Liveness: the process answers. Readiness: dependencies answer too.
        // Probing liveness through dependencies would restart every replica
        // whenever PostgreSQL or Qdrant has a hiccup.
        .route("/livez", web::get().to(livez))
        .route("/readyz", web::get().to(healthz))
        .route("/healthz", web::get().to(healthz))
        .configure(complete_page::configure)
        .service(
            web::scope("/api/v1")
                .configure(auth::configure)
                .configure(google::configure)
                .configure(tokens::configure)
                .configure(orgs::configure)
                .configure(teams::configure)
                .configure(contexts::configure)
                .configure(trash::configure)
                .configure(search::configure)
                .configure(graph::configure)
                .configure(audit_logs::configure),
        );
}

async fn livez() -> HttpResponse {
    HttpResponse::Ok().json(serde_json::json!({ "status": "ok" }))
}

async fn healthz(state: web::Data<AppState>) -> ApiResult<HttpResponse> {
    sqlx::query("SELECT 1").execute(&state.pool).await?;
    state
        .vectors
        .health()
        .await
        .map_err(|_| crate::error::ApiError::Unavailable("vector store"))?;
    Ok(HttpResponse::Ok().json(serde_json::json!({ "status": "ok" })))
}
