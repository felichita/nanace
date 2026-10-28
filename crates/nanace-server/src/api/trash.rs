use actix_web::{HttpResponse, web};
use chrono::{DateTime, Utc};
use nanace_core::embedding_input_hash;
use serde::Serialize;
use uuid::Uuid;

use super::contexts::{ContextRow, embed_context, replace_links, vector_store_unavailable};
use crate::audit::{self, AuditEvent};
use crate::auth::{AuthUser, WriteUser};
use crate::error::{ApiError, ApiResult};
use crate::{AppState, access, db};

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/orgs/{org_id}/trash", web::get().to(list_trash))
        .route("/orgs/{org_id}/trash/{context_id}/restore", web::post().to(restore))
        .route("/orgs/{org_id}/trash/{context_id}", web::delete().to(purge));
}

const MAX_LISTED: i64 = 500;

#[derive(Serialize, sqlx::FromRow)]
struct TrashItem {
    id: Uuid,
    team_id: Uuid,
    title: String,
    tags: Vec<String>,
    deleted_at: DateTime<Utc>,
    deleted_by: Uuid,
    deleted_by_email: String,
    /// When the context will be deleted for good; null when trash is kept.
    purge_at: Option<DateTime<Utc>>,
}

/// Trashed contexts of the teams where the caller can write (RLS), newest
/// first. Titles only, like listing.
async fn list_trash(state: web::Data<AppState>, user: AuthUser, path: web::Path<Uuid>) -> ApiResult<HttpResponse> {
    let org_id = path.into_inner();
    access::require_org_member(&state.pool, org_id, user.user_id).await?;
    let mut tx = db::scoped_tx(&state.pool, org_id, user.user_id).await?;
    let items: Vec<TrashItem> = sqlx::query_as(
        "SELECT t.id, t.team_id, t.title, t.tags, t.deleted_at, t.deleted_by, u.email AS deleted_by_email, \
                CASE WHEN $2 > 0 THEN t.deleted_at + make_interval(days => $2) END AS purge_at \
         FROM context_trash t JOIN users u ON u.id = t.deleted_by \
         ORDER BY t.deleted_at DESC, t.id LIMIT $1",
    )
    .bind(MAX_LISTED)
    .bind(i32::try_from(state.trash_retention_days).map_err(anyhow::Error::from)?)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(HttpResponse::Ok().json(items))
}

/// Puts a trashed context back with its original id and author, re-embedding
/// its content and re-creating its links.
async fn restore(
    state: web::Data<AppState>,
    user: WriteUser,
    path: web::Path<(Uuid, Uuid)>,
) -> ApiResult<HttpResponse> {
    let (org_id, context_id) = path.into_inner();
    access::require_org_member(&state.pool, org_id, user.user_id).await?;

    // Trash rows are immutable (no UPDATE grant), so the content read here is
    // what the insert below copies.
    let mut tx = db::scoped_tx(&state.pool, org_id, user.user_id).await?;
    let row: Option<(Uuid, String, String)> =
        sqlx::query_as("SELECT team_id, title, content FROM context_trash WHERE id = $1")
            .bind(context_id)
            .fetch_optional(&mut *tx)
            .await?;
    tx.commit().await?;
    let (team_id, title, content) = row.ok_or(ApiError::NotFound)?;

    let chunks = embed_context(&state, &title, &content).await?;
    let chunk_count = i32::try_from(chunks.len()).map_err(anyhow::Error::from)?;
    let hash = embedding_input_hash(&title, &content, state.embedder.model_id());

    let mut tx = db::scoped_tx(&state.pool, org_id, user.user_id).await?;
    // Inserted while the trash row still exists: the INSERT policy accepts the
    // original `created_by` only for a context coming back from the trash.
    let restored: Option<ContextRow> = sqlx::query_as(
        "INSERT INTO contexts (id, org_id, team_id, title, content, tags, embedding_hash, embedding_model, \
                               chunk_count, created_by, created_at, updated_by) \
         SELECT id, org_id, team_id, title, content, tags, $2, $3, $4, created_by, created_at, $5 \
         FROM context_trash WHERE id = $1 \
         RETURNING id, org_id, team_id, title, content, tags, created_by, updated_by, created_at, updated_at",
    )
    .bind(context_id)
    .bind(&hash)
    .bind(state.embedder.model_id())
    .bind(chunk_count)
    .bind(user.user_id)
    .fetch_optional(&mut *tx)
    .await?;
    // Restored or purged concurrently.
    let restored = restored.ok_or(ApiError::NotFound)?;
    sqlx::query("DELETE FROM context_trash WHERE id = $1")
        .bind(context_id)
        .execute(&mut *tx)
        .await?;
    replace_links(&mut tx, org_id, context_id, &content).await?;
    audit::record(
        &mut *tx,
        AuditEvent {
            org_id: Some(org_id),
            actor: user.user_id,
            action: "context.restore",
            resource_type: "context",
            resource_id: Some(context_id),
            details: serde_json::json!({ "team_id": team_id, "chunks": chunk_count }),
        },
    )
    .await?;
    state
        .vectors
        .replace_context(org_id, team_id, context_id, chunks)
        .await
        .map_err(vector_store_unavailable)?;
    tx.commit().await?;
    Ok(HttpResponse::Ok().json(restored))
}

/// Deletes a trashed context permanently.
async fn purge(state: web::Data<AppState>, user: WriteUser, path: web::Path<(Uuid, Uuid)>) -> ApiResult<HttpResponse> {
    let (org_id, context_id) = path.into_inner();
    access::require_org_member(&state.pool, org_id, user.user_id).await?;
    let mut tx = db::scoped_tx(&state.pool, org_id, user.user_id).await?;
    let team_id: Option<Uuid> = sqlx::query_scalar("DELETE FROM context_trash WHERE id = $1 RETURNING team_id")
        .bind(context_id)
        .fetch_optional(&mut *tx)
        .await?;
    let team_id = team_id.ok_or(ApiError::NotFound)?;
    audit::record(
        &mut *tx,
        AuditEvent {
            org_id: Some(org_id),
            actor: user.user_id,
            action: "context.purge",
            resource_type: "context",
            resource_id: Some(context_id),
            details: serde_json::json!({ "team_id": team_id }),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(HttpResponse::NoContent().finish())
}
