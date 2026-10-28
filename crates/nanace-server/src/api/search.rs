use std::collections::HashMap;

use actix_web::{HttpResponse, web};
use chrono::{DateTime, Utc};
use nanace_core::chunk_markdown;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::auth::AuthUser;
use crate::error::{ApiError, ApiResult};
use crate::{AppState, CHUNK_MAX_BYTES, LOG_COMPONENT, access, db};

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/orgs/{org_id}/search", web::post().to(search));
}

const MAX_QUERY_BYTES: usize = 2048;
const MAX_LIMIT: u64 = 50;
/// Chunks fetched per requested result; several chunks of one context can
/// rank highly, and results are grouped per context.
const CHUNK_OVERFETCH: u64 = 4;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchRequest {
    query: String,
    limit: Option<u64>,
    /// Restricts the search to these teams; the caller must be able to read
    /// each one.
    team_ids: Option<Vec<Uuid>>,
}

#[derive(Serialize)]
struct SearchHit {
    context_id: Uuid,
    team_id: Uuid,
    title: String,
    tags: Vec<String>,
    score: f32,
    snippet: String,
    updated_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct HitRow {
    id: Uuid,
    team_id: Uuid,
    title: String,
    tags: Vec<String>,
    updated_at: DateTime<Utc>,
    content: String,
}

/// The snippet shown for a hit, always taken from the current content in
/// PostgreSQL: the matched chunk when it is still part of the page, else the
/// page's first chunk. Text stored with the vectors can be stale (a failed
/// commit, a reindex racing an edit) and must never resurface.
fn snippet_for(content: &str, matched_chunk: &str) -> String {
    let matched = matched_chunk.trim();
    if !matched.is_empty() && content.contains(matched) {
        return matched.to_owned();
    }
    chunk_markdown(content, CHUNK_MAX_BYTES)
        .into_iter()
        .next()
        .unwrap_or_default()
}

async fn search(
    state: web::Data<AppState>,
    user: AuthUser,
    path: web::Path<Uuid>,
    body: web::Json<SearchRequest>,
) -> ApiResult<HttpResponse> {
    let org_id = path.into_inner();
    access::require_org_member(&state.pool, org_id, user.user_id).await?;

    let query = body.query.trim();
    if query.is_empty() || query.len() > MAX_QUERY_BYTES {
        return Err(ApiError::BadRequest("query must be 1-2048 bytes".to_owned()));
    }
    let limit = body.limit.unwrap_or(10);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(ApiError::BadRequest("limit must be 1-50".to_owned()));
    }

    let readable = access::readable_teams(&state.pool, org_id, user.user_id).await?;
    let teams = match &body.team_ids {
        None => readable,
        Some(requested) => {
            if requested.iter().any(|t| !readable.contains(t)) {
                return Err(ApiError::Forbidden);
            }
            requested.clone()
        }
    };
    if teams.is_empty() {
        return Ok(HttpResponse::Ok().json(Vec::<SearchHit>::new()));
    }

    let vector = state.embedder.embed_query(query).await.map_err(|err| {
        tracing::error!(event = "embedding_failed", component = LOG_COMPONENT, error = %err, "query embedding failed");
        ApiError::Unavailable("embedding service")
    })?;
    let chunk_hits = state
        .vectors
        .search(org_id, &teams, vector, limit * CHUNK_OVERFETCH)
        .await
        .map_err(|err| {
            tracing::error!(event = "vector_search_failed", component = LOG_COMPONENT, error = %format!("{err:#}"), "vector search failed");
            ApiError::Unavailable("vector store")
        })?;

    // Best chunk per context, in score order.
    let mut best: Vec<(Uuid, f32, String)> = Vec::new();
    for hit in chunk_hits {
        if !best.iter().any(|(id, _, _)| *id == hit.context_id) {
            best.push((hit.context_id, hit.score, hit.text));
        }
    }
    let ids: Vec<Uuid> = best.iter().map(|(id, _, _)| *id).collect();

    // Re-check every hit under RLS: drops points for deleted contexts and
    // anything the caller can no longer read.
    let mut tx = db::scoped_tx(&state.pool, org_id, user.user_id).await?;
    let rows: Vec<HitRow> =
        sqlx::query_as("SELECT id, team_id, title, tags, updated_at, content FROM contexts WHERE id = ANY($1)")
            .bind(&ids)
            .fetch_all(&mut *tx)
            .await?;
    let mut rows: HashMap<Uuid, HitRow> = rows.into_iter().map(|r| (r.id, r)).collect();
    let hits: Vec<SearchHit> = best
        .into_iter()
        .filter_map(|(id, score, snippet)| {
            let row = rows.remove(&id).filter(|r| teams.contains(&r.team_id))?;
            Some(SearchHit {
                context_id: id,
                team_id: row.team_id,
                title: row.title,
                tags: row.tags,
                score,
                snippet: snippet_for(&row.content, &snippet),
                updated_at: row.updated_at,
            })
        })
        .take(usize::try_from(limit).map_err(anyhow::Error::from)?)
        .collect();

    audit::record(
        &mut *tx,
        AuditEvent {
            org_id: Some(org_id),
            actor: user.user_id,
            action: "context.search",
            resource_type: "organization",
            resource_id: Some(org_id),
            details: serde_json::json!({
                "team_ids": teams,
                "result_ids": hits.iter().map(|h| h.context_id).collect::<Vec<_>>(),
            }),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(HttpResponse::Ok().json(hits))
}

#[cfg(test)]
mod tests {
    use super::snippet_for;

    #[test]
    fn snippets_come_from_current_content() {
        let content = "# Deploy\n\nRoll out with a canary.";
        assert_eq!(
            snippet_for(content, "Roll out with a canary."),
            "Roll out with a canary."
        );
        // A stale chunk (text no longer in the page) is never shown.
        let stale = snippet_for(content, "The root password is hunter2.");
        assert!(!stale.contains("hunter2") && stale.contains("canary"), "{stale}");
        assert_eq!(snippet_for("", "old text"), "");
    }
}
