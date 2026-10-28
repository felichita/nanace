use std::collections::HashMap;

use actix_web::{HttpResponse, web};
use chrono::{DateTime, Utc};
use nanace_core::{TeamRole, embedding_input_hash, validate, wiki_links};
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::auth::{AuthUser, WriteUser};
use crate::error::{ApiError, ApiResult};
use crate::vectors::{self, Chunk};
use crate::{AppState, LOG_COMPONENT, access, db};

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route(
        "/orgs/{org_id}/teams/{team_id}/contexts",
        web::post().to(create_context),
    )
    .route("/orgs/{org_id}/teams/{team_id}/contexts", web::get().to(list_contexts))
    .route("/orgs/{org_id}/contexts/{context_id}", web::get().to(get_context))
    .route("/orgs/{org_id}/contexts/{context_id}", web::put().to(update_context))
    .route("/orgs/{org_id}/contexts/{context_id}", web::delete().to(delete_context))
    .route(
        "/orgs/{org_id}/contexts/{context_id}/links",
        web::get().to(context_links),
    );
}

/// Semantic neighbors shown as related contexts.
const RELATED_LIMIT: u64 = 5;
/// Minimum cosine similarity for a related context. bge-small scores
/// unrelated English text around 0.5-0.62, so lower values are mostly noise.
pub(crate) const DEFAULT_MIN_SCORE: f32 = 0.65;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ContextInput {
    title: String,
    content: String,
    #[serde(default)]
    tags: Vec<String>,
    /// Update only: the `updated_at` the edit was based on. A mismatch means
    /// someone else changed the context, and the update is rejected with 409.
    base_updated_at: Option<DateTime<Utc>>,
}

struct ValidContext {
    title: String,
    content: String,
    tags: Vec<String>,
    base_updated_at: Option<DateTime<Utc>>,
}

impl ContextInput {
    fn validate(self) -> ApiResult<ValidContext> {
        validate::content(&self.content)?;
        Ok(ValidContext {
            title: validate::title(&self.title)?,
            tags: validate::tags(&self.tags)?,
            content: self.content,
            base_updated_at: self.base_updated_at,
        })
    }
}

#[derive(Serialize, sqlx::FromRow)]
pub(crate) struct ContextRow {
    id: Uuid,
    org_id: Uuid,
    team_id: Uuid,
    title: String,
    content: String,
    tags: Vec<String>,
    created_by: Uuid,
    updated_by: Uuid,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Serialize, sqlx::FromRow)]
struct ContextSummary {
    id: Uuid,
    team_id: Uuid,
    title: String,
    tags: Vec<String>,
    updated_by: Uuid,
    updated_at: DateTime<Utc>,
}

/// [`vectors::embed_context`] with API errors.
pub(crate) async fn embed_context(state: &AppState, title: &str, content: &str) -> ApiResult<Vec<Chunk>> {
    vectors::embed_context(state.embedder.as_ref(), title, content)
        .await
        .map_err(|err| {
            tracing::error!(event = "embedding_failed", component = LOG_COMPONENT, error = %err, "embedding failed");
            ApiError::Unavailable("embedding service")
        })
}

/// Team of a context the caller can read; `NotFound` otherwise.
async fn readable_team_of(tx: &mut Transaction<'static, Postgres>, context_id: Uuid) -> ApiResult<Uuid> {
    let team_id: Option<Uuid> = sqlx::query_scalar("SELECT team_id FROM contexts WHERE id = $1")
        .bind(context_id)
        .fetch_optional(&mut **tx)
        .await?;
    team_id.ok_or(ApiError::NotFound)
}

/// Replaces the context's stored wiki links with those in `content`.
pub(crate) async fn replace_links(
    tx: &mut Transaction<'static, Postgres>,
    org_id: Uuid,
    context_id: Uuid,
    content: &str,
) -> ApiResult<()> {
    sqlx::query("DELETE FROM context_links WHERE source_id = $1")
        .bind(context_id)
        .execute(&mut **tx)
        .await?;
    let targets = wiki_links(content);
    if !targets.is_empty() {
        sqlx::query(
            "INSERT INTO context_links (org_id, source_id, target_key) \
             SELECT $1, $2, lower(t) FROM unnest($3::text[]) AS t ON CONFLICT DO NOTHING",
        )
        .bind(org_id)
        .bind(context_id)
        .bind(&targets)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

pub(crate) fn vector_store_unavailable(err: anyhow::Error) -> ApiError {
    tracing::error!(event = "vector_write_failed", component = LOG_COMPONENT, error = %format!("{err:#}"), "vector store write failed");
    ApiError::Unavailable("vector store")
}

async fn create_context(
    state: web::Data<AppState>,
    user: WriteUser,
    path: web::Path<(Uuid, Uuid)>,
    body: web::Json<ContextInput>,
) -> ApiResult<HttpResponse> {
    let (org_id, team_id) = path.into_inner();
    access::require_team_role(&state.pool, org_id, team_id, user.user_id, TeamRole::Writer).await?;
    let input = body.into_inner().validate()?;
    let context_id = Uuid::new_v4();

    let chunks = embed_context(&state, &input.title, &input.content).await?;
    let chunk_count = i32::try_from(chunks.len()).map_err(anyhow::Error::from)?;
    let hash = embedding_input_hash(&input.title, &input.content, state.embedder.model_id());

    let mut tx = db::scoped_tx(&state.pool, org_id, user.user_id).await?;
    let row: ContextRow = sqlx::query_as(
        "INSERT INTO contexts (id, org_id, team_id, title, content, tags, embedding_hash, embedding_model, \
                               chunk_count, created_by, updated_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $10) \
         RETURNING id, org_id, team_id, title, content, tags, created_by, updated_by, created_at, updated_at",
    )
    .bind(context_id)
    .bind(org_id)
    .bind(team_id)
    .bind(&input.title)
    .bind(&input.content)
    .bind(&input.tags)
    .bind(&hash)
    .bind(state.embedder.model_id())
    .bind(chunk_count)
    .bind(user.user_id)
    .fetch_one(&mut *tx)
    .await?;
    replace_links(&mut tx, org_id, context_id, &input.content).await?;
    audit::record(
        &mut *tx,
        AuditEvent {
            org_id: Some(org_id),
            actor: user.user_id,
            action: "context.create",
            resource_type: "context",
            resource_id: Some(context_id),
            details: serde_json::json!({ "team_id": team_id, "chunks": chunk_count }),
        },
    )
    .await?;
    // Vectors are written before commit: if this fails nothing is committed.
    // If the commit fails afterwards the orphan points are harmless because
    // search results are re-checked against PostgreSQL.
    state
        .vectors
        .replace_context(org_id, team_id, context_id, chunks)
        .await
        .map_err(vector_store_unavailable)?;
    tx.commit().await?;

    Ok(HttpResponse::Created().json(row))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListQuery {
    limit: Option<i64>,
    offset: Option<i64>,
}

async fn list_contexts(
    state: web::Data<AppState>,
    user: AuthUser,
    path: web::Path<(Uuid, Uuid)>,
    query: web::Query<ListQuery>,
) -> ApiResult<HttpResponse> {
    let (org_id, team_id) = path.into_inner();
    access::require_team_role(&state.pool, org_id, team_id, user.user_id, TeamRole::Reader).await?;
    let limit = query.limit.unwrap_or(50);
    let offset = query.offset.unwrap_or(0);
    if !(1..=200).contains(&limit) || offset < 0 {
        return Err(ApiError::BadRequest(
            "limit must be 1-200 and offset non-negative".to_owned(),
        ));
    }

    let mut tx = db::scoped_tx(&state.pool, org_id, user.user_id).await?;
    let rows: Vec<ContextSummary> = sqlx::query_as(
        "SELECT id, team_id, title, tags, updated_by, updated_at FROM contexts \
         WHERE org_id = $1 AND team_id = $2 ORDER BY updated_at DESC, id LIMIT $3 OFFSET $4",
    )
    .bind(org_id)
    .bind(team_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(HttpResponse::Ok().json(rows))
}

async fn get_context(
    state: web::Data<AppState>,
    user: AuthUser,
    path: web::Path<(Uuid, Uuid)>,
) -> ApiResult<HttpResponse> {
    let (org_id, context_id) = path.into_inner();
    access::require_org_member(&state.pool, org_id, user.user_id).await?;

    let mut tx = db::scoped_tx(&state.pool, org_id, user.user_id).await?;
    let row: Option<ContextRow> = sqlx::query_as(
        "SELECT id, org_id, team_id, title, content, tags, created_by, updated_by, created_at, updated_at \
         FROM contexts WHERE id = $1",
    )
    .bind(context_id)
    .fetch_optional(&mut *tx)
    .await?;
    let row = row.ok_or(ApiError::NotFound)?;
    audit::record(
        &mut *tx,
        AuditEvent {
            org_id: Some(org_id),
            actor: user.user_id,
            action: "context.read",
            resource_type: "context",
            resource_id: Some(context_id),
            details: serde_json::json!({ "team_id": row.team_id }),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(HttpResponse::Ok().json(row))
}

async fn update_context(
    state: web::Data<AppState>,
    user: WriteUser,
    path: web::Path<(Uuid, Uuid)>,
    body: web::Json<ContextInput>,
) -> ApiResult<HttpResponse> {
    let (org_id, context_id) = path.into_inner();
    access::require_org_member(&state.pool, org_id, user.user_id).await?;
    let input = body.into_inner().validate()?;

    let mut tx = db::scoped_tx(&state.pool, org_id, user.user_id).await?;
    let team_id = readable_team_of(&mut tx, context_id).await?;
    access::require_team_role(&state.pool, org_id, team_id, user.user_id, TeamRole::Writer).await?;
    // Serializes concurrent updates so PostgreSQL and Qdrant end up holding
    // the same version. Taken after the role check: under RLS, FOR UPDATE
    // also applies the UPDATE policy and would hide the row from readers.
    let current: Option<(String, DateTime<Utc>)> =
        sqlx::query_as("SELECT embedding_hash, updated_at FROM contexts WHERE id = $1 FOR UPDATE")
            .bind(context_id)
            .fetch_optional(&mut *tx)
            .await?;
    // A concurrent delete may have committed while this request waited.
    let (old_hash, updated_at) = current.ok_or(ApiError::NotFound)?;
    if input.base_updated_at.is_some_and(|base| base != updated_at) {
        return Err(ApiError::Conflict("context was changed since it was loaded".to_owned()));
    }

    let new_hash = embedding_input_hash(&input.title, &input.content, state.embedder.model_id());
    let chunks = if new_hash == old_hash {
        None
    } else {
        Some(embed_context(&state, &input.title, &input.content).await?)
    };

    let row: ContextRow = sqlx::query_as(
        "UPDATE contexts SET title = $2, content = $3, tags = $4, embedding_hash = $5, embedding_model = $6, \
                chunk_count = COALESCE($7, chunk_count), updated_by = $8, updated_at = now() \
         WHERE id = $1 \
         RETURNING id, org_id, team_id, title, content, tags, created_by, updated_by, created_at, updated_at",
    )
    .bind(context_id)
    .bind(&input.title)
    .bind(&input.content)
    .bind(&input.tags)
    .bind(&new_hash)
    .bind(state.embedder.model_id())
    .bind(
        chunks
            .as_ref()
            .map(|c| i32::try_from(c.len()))
            .transpose()
            .map_err(anyhow::Error::from)?,
    )
    .bind(user.user_id)
    .fetch_one(&mut *tx)
    .await?;
    replace_links(&mut tx, org_id, context_id, &input.content).await?;
    audit::record(
        &mut *tx,
        AuditEvent {
            org_id: Some(org_id),
            actor: user.user_id,
            action: "context.update",
            resource_type: "context",
            resource_id: Some(context_id),
            details: serde_json::json!({ "team_id": team_id, "reembedded": chunks.is_some() }),
        },
    )
    .await?;
    if let Some(chunks) = chunks {
        state
            .vectors
            .replace_context(org_id, team_id, context_id, chunks)
            .await
            .map_err(vector_store_unavailable)?;
    }
    tx.commit().await?;
    Ok(HttpResponse::Ok().json(row))
}

async fn delete_context(
    state: web::Data<AppState>,
    user: WriteUser,
    path: web::Path<(Uuid, Uuid)>,
) -> ApiResult<HttpResponse> {
    let (org_id, context_id) = path.into_inner();
    access::require_org_member(&state.pool, org_id, user.user_id).await?;

    let mut tx = db::scoped_tx(&state.pool, org_id, user.user_id).await?;
    let team_id = readable_team_of(&mut tx, context_id).await?;
    access::require_team_role(&state.pool, org_id, team_id, user.user_id, TeamRole::Writer).await?;

    // Moves the context to the trash; restoring it re-embeds the content.
    let trashed = sqlx::query(
        "INSERT INTO context_trash (id, org_id, team_id, title, content, tags, created_by, created_at, \
                                    updated_by, updated_at, deleted_by) \
         SELECT id, org_id, team_id, title, content, tags, created_by, created_at, updated_by, updated_at, $2 \
         FROM contexts WHERE id = $1",
    )
    .bind(context_id)
    .bind(user.user_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    // A concurrent delete may have committed while this request waited.
    if trashed == 0 {
        return Err(ApiError::NotFound);
    }
    sqlx::query("DELETE FROM contexts WHERE id = $1")
        .bind(context_id)
        .execute(&mut *tx)
        .await?;
    audit::record(
        &mut *tx,
        AuditEvent {
            org_id: Some(org_id),
            actor: user.user_id,
            action: "context.delete",
            resource_type: "context",
            resource_id: Some(context_id),
            details: serde_json::json!({ "team_id": team_id, "trashed": true }),
        },
    )
    .await?;
    tx.commit().await?;

    // After commit: leftover points are filtered out by search, whereas
    // deleting them first could leave a live context unsearchable.
    if let Err(err) = state.vectors.delete_context(org_id, context_id).await {
        tracing::warn!(
            event = "vector_delete_failed",
            component = LOG_COMPONENT,
            context_id = %context_id,
            error = %format!("{err:#}"),
            "orphan vectors left after context delete"
        );
    }
    Ok(HttpResponse::NoContent().finish())
}

#[derive(Serialize, sqlx::FromRow)]
pub(crate) struct LinkedContext {
    pub id: Uuid,
    pub team_id: Uuid,
    pub title: String,
}

#[derive(Serialize)]
struct RelatedContext {
    #[serde(flatten)]
    context: LinkedContext,
    score: f32,
}

#[derive(Serialize)]
struct ContextLinks {
    /// Contexts this one links to with `[[Title]]`.
    outgoing: Vec<LinkedContext>,
    /// Contexts that link to this one.
    backlinks: Vec<LinkedContext>,
    /// Semantically similar contexts.
    related: Vec<RelatedContext>,
}

/// Links and related contexts for one context. Returns titles only, like
/// listing, so it is not audited as a content read.
async fn context_links(
    state: web::Data<AppState>,
    user: AuthUser,
    path: web::Path<(Uuid, Uuid)>,
) -> ApiResult<HttpResponse> {
    let (org_id, context_id) = path.into_inner();
    access::require_org_member(&state.pool, org_id, user.user_id).await?;
    let teams = access::readable_teams(&state.pool, org_id, user.user_id).await?;

    let mut tx = db::scoped_tx(&state.pool, org_id, user.user_id).await?;
    readable_team_of(&mut tx, context_id).await?;
    let outgoing: Vec<LinkedContext> = sqlx::query_as(
        "SELECT DISTINCT c.id, c.team_id, c.title FROM context_links l \
         JOIN contexts c ON c.org_id = l.org_id AND lower(c.title) = l.target_key \
         WHERE l.source_id = $1 AND c.id <> $1 ORDER BY c.title, c.id",
    )
    .bind(context_id)
    .fetch_all(&mut *tx)
    .await?;
    let backlinks: Vec<LinkedContext> = sqlx::query_as(
        "SELECT DISTINCT s.id, s.team_id, s.title FROM contexts t \
         JOIN context_links l ON l.org_id = t.org_id AND l.target_key = lower(t.title) \
         JOIN contexts s ON s.id = l.source_id \
         WHERE t.id = $1 AND s.id <> $1 ORDER BY s.title, s.id",
    )
    .bind(context_id)
    .fetch_all(&mut *tx)
    .await?;

    let neighbors = if teams.is_empty() {
        Vec::new()
    } else {
        state
            .vectors
            .neighbors(org_id, &teams, &[context_id], RELATED_LIMIT, DEFAULT_MIN_SCORE)
            .await
            .map_err(|err| {
                tracing::error!(event = "vector_search_failed", component = LOG_COMPONENT, error = %format!("{err:#}"), "neighbor search failed");
                ApiError::Unavailable("vector store")
            })?
    };
    // Re-check neighbors under RLS before returning their titles.
    let ids: Vec<Uuid> = neighbors.iter().map(|n| n.target).collect();
    let rows: Vec<LinkedContext> = sqlx::query_as("SELECT id, team_id, title FROM contexts WHERE id = ANY($1)")
        .bind(&ids)
        .fetch_all(&mut *tx)
        .await?;
    tx.commit().await?;
    let mut rows: HashMap<Uuid, LinkedContext> = rows.into_iter().map(|r| (r.id, r)).collect();
    let related = neighbors
        .into_iter()
        .filter_map(|n| {
            let context = rows.remove(&n.target).filter(|r| teams.contains(&r.team_id))?;
            Some(RelatedContext {
                context,
                score: n.score,
            })
        })
        .collect();

    Ok(HttpResponse::Ok().json(ContextLinks {
        outgoing,
        backlinks,
        related,
    }))
}
