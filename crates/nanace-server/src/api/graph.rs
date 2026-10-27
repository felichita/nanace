use std::collections::{HashMap, HashSet};

use actix_web::{HttpResponse, web};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::contexts::DEFAULT_MIN_SCORE;
use crate::auth::AuthUser;
use crate::error::{ApiError, ApiResult};
use crate::{AppState, LOG_COMPONENT, access, db};

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/orgs/{org_id}/graph", web::get().to(graph));
}

/// Nodes returned per graph; the most recently updated win.
const MAX_NODES: i64 = 2000;
const MAX_NEIGHBORS: u64 = 10;
const DEFAULT_NEIGHBORS: u64 = 3;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GraphQuery {
    /// Restricts the graph to one team the caller can read.
    team_id: Option<Uuid>,
    /// Include similarity edges (default true).
    semantic: Option<bool>,
    /// Similar contexts per node (1-10, default 3).
    neighbors: Option<u64>,
    /// Minimum cosine similarity for a similarity edge (0-1).
    min_score: Option<f32>,
}

#[derive(Serialize, sqlx::FromRow)]
struct Node {
    id: Uuid,
    team_id: Uuid,
    title: String,
    tags: Vec<String>,
    updated_at: DateTime<Utc>,
    #[serde(skip)]
    chunk_count: i32,
}

#[derive(Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum EdgeKind {
    Link,
    Semantic,
}

#[derive(Serialize)]
struct Edge {
    source: Uuid,
    target: Uuid,
    kind: EdgeKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    score: Option<f32>,
}

#[derive(Serialize)]
struct Graph {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    /// More than `MAX_NODES` contexts matched; the oldest were left out.
    truncated: bool,
}

/// Unordered pair key, so A→B and B→A collapse into one similarity edge.
fn pair(a: Uuid, b: Uuid) -> (Uuid, Uuid) {
    if a < b { (a, b) } else { (b, a) }
}

/// Contexts the caller can read, with `[[link]]` edges and, optionally,
/// similarity edges. Returns titles only, like listing, so it is not audited
/// as a content read.
async fn graph(
    state: web::Data<AppState>,
    user: AuthUser,
    path: web::Path<Uuid>,
    query: web::Query<GraphQuery>,
) -> ApiResult<HttpResponse> {
    let org_id = path.into_inner();
    access::require_org_member(&state.pool, org_id, user.user_id).await?;
    let neighbors = query.neighbors.unwrap_or(DEFAULT_NEIGHBORS);
    if !(1..=MAX_NEIGHBORS).contains(&neighbors) {
        return Err(ApiError::BadRequest("neighbors must be 1-10".to_owned()));
    }
    let min_score = query.min_score.unwrap_or(DEFAULT_MIN_SCORE);
    if !(0.0..=1.0).contains(&min_score) {
        return Err(ApiError::BadRequest("min_score must be 0-1".to_owned()));
    }

    let readable = access::readable_teams(&state.pool, org_id, user.user_id).await?;
    let teams = match query.team_id {
        None => readable,
        Some(team) if readable.contains(&team) => vec![team],
        Some(_) => return Err(ApiError::NotFound),
    };
    if teams.is_empty() {
        return Ok(HttpResponse::Ok().json(Graph {
            nodes: Vec::new(),
            edges: Vec::new(),
            truncated: false,
        }));
    }

    let mut tx = db::scoped_tx(&state.pool, org_id, user.user_id).await?;
    let mut nodes: Vec<Node> = sqlx::query_as(
        "SELECT id, team_id, title, tags, updated_at, chunk_count FROM contexts \
         WHERE org_id = $1 AND team_id = ANY($2) ORDER BY updated_at DESC, id LIMIT $3",
    )
    .bind(org_id)
    .bind(&teams)
    .bind(MAX_NODES + 1)
    .fetch_all(&mut *tx)
    .await?;
    let truncated = nodes.len() as i64 > MAX_NODES;
    nodes.truncate(usize::try_from(MAX_NODES).map_err(anyhow::Error::from)?);
    let ids: Vec<Uuid> = nodes.iter().map(|n| n.id).collect();

    let links: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT DISTINCT l.source_id, c.id FROM context_links l \
         JOIN contexts c ON c.org_id = l.org_id AND lower(c.title) = l.target_key \
         WHERE l.source_id = ANY($1) AND c.id = ANY($1) AND c.id <> l.source_id",
    )
    .bind(&ids)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    let linked: HashSet<(Uuid, Uuid)> = links.iter().map(|&(a, b)| pair(a, b)).collect();
    let mut edges: Vec<Edge> = links
        .into_iter()
        .map(|(source, target)| Edge {
            source,
            target,
            kind: EdgeKind::Link,
            score: None,
        })
        .collect();

    if query.semantic.unwrap_or(true) {
        let embedded: Vec<Uuid> = nodes.iter().filter(|n| n.chunk_count > 0).map(|n| n.id).collect();
        let found = state
            .vectors
            .neighbors(org_id, &teams, &embedded, neighbors, min_score)
            .await
            .map_err(|err| {
                tracing::error!(event = "vector_search_failed", component = LOG_COMPONENT, error = %format!("{err:#}"), "neighbor search failed");
                ApiError::Unavailable("vector store")
            })?;
        // Only edges between nodes read from PostgreSQL above: stale points
        // and contexts outside the graph never surface.
        let node_ids: HashSet<Uuid> = ids.iter().copied().collect();
        let mut best: HashMap<(Uuid, Uuid), f32> = HashMap::new();
        for n in found {
            if !node_ids.contains(&n.target) || linked.contains(&pair(n.source, n.target)) {
                continue;
            }
            let score = best.entry(pair(n.source, n.target)).or_insert(n.score);
            *score = score.max(n.score);
        }
        let mut similar: Vec<_> = best.into_iter().collect();
        similar.sort_by_key(|(pair, _)| *pair);
        edges.extend(similar.into_iter().map(|((source, target), score)| Edge {
            source,
            target,
            kind: EdgeKind::Semantic,
            score: Some(score),
        }));
    }

    Ok(HttpResponse::Ok().json(Graph {
        nodes,
        edges,
        truncated,
    }))
}
