use actix_web::{HttpResponse, web};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::AppState;
use crate::access;
use crate::auth::AuthUser;
use crate::error::{ApiError, ApiResult};

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/orgs/{org_id}/audit-logs", web::get().to(list_audit_logs));
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AuditQuery {
    limit: Option<i64>,
    /// Keyset pagination: return entries with `id < before_id`.
    before_id: Option<i64>,
}

#[derive(Serialize, sqlx::FromRow)]
struct AuditRow {
    id: i64,
    actor_user_id: Option<Uuid>,
    action: String,
    resource_type: String,
    resource_id: Option<Uuid>,
    details: serde_json::Value,
    created_at: DateTime<Utc>,
}

async fn list_audit_logs(
    state: web::Data<AppState>,
    user: AuthUser,
    path: web::Path<Uuid>,
    query: web::Query<AuditQuery>,
) -> ApiResult<HttpResponse> {
    let org_id = path.into_inner();
    access::require_org_admin(&state.pool, org_id, user.user_id).await?;
    let limit = query.limit.unwrap_or(100);
    if !(1..=500).contains(&limit) {
        return Err(ApiError::BadRequest("limit must be 1-500".to_owned()));
    }
    let rows: Vec<AuditRow> = sqlx::query_as(
        "SELECT id, actor_user_id, action, resource_type, resource_id, details, created_at \
         FROM audit_logs WHERE org_id = $1 AND ($2::bigint IS NULL OR id < $2) \
         ORDER BY id DESC LIMIT $3",
    )
    .bind(org_id)
    .bind(query.before_id)
    .bind(limit)
    .fetch_all(&state.pool)
    .await?;
    Ok(HttpResponse::Ok().json(rows))
}
