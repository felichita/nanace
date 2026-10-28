//! Rebuilds Qdrant collections from PostgreSQL, the source of truth: after
//! moving to a new or empty Qdrant, after changing the embedding model, or to
//! drop vectors that no longer belong to any context.
//!
//! Each org's collection is recreated (with the current model's dimensions)
//! and every context is chunked and embedded again with the same code as the
//! API. Contexts are read through `db::scoped_tx` as a member of their team,
//! so RLS applies as usual; teams without members cannot be read and are
//! reported instead. Trashed contexts are not indexed.
//!
//! Search for an org returns partial results while it is being reindexed, and
//! an edit made during the run may be indexed with its previous text until
//! the page is saved again: run it when the org is quiet.

use anyhow::Context;
use nanace_embed::Embedder;
use sqlx::PgPool;
use uuid::Uuid;

use crate::vectors::{VectorStore, embed_context};
use crate::{LOG_COMPONENT, db};

#[derive(Debug, Default)]
pub struct ReindexReport {
    pub orgs: usize,
    pub contexts: usize,
    pub chunks: usize,
    /// (org, team) pairs skipped because the team has no members.
    pub skipped_teams: Vec<(Uuid, Uuid)>,
}

/// Reindexes `only_org`, or every organization when `None`.
pub async fn reindex(
    pool: &PgPool,
    vectors: &VectorStore,
    embedder: &dyn Embedder,
    only_org: Option<Uuid>,
) -> anyhow::Result<ReindexReport> {
    let orgs: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM organizations WHERE $1::uuid IS NULL OR id = $1 ORDER BY id")
            .bind(only_org)
            .fetch_all(pool)
            .await?;
    if let Some(org) = only_org
        && orgs.is_empty()
    {
        anyhow::bail!("organization {org} does not exist");
    }

    let mut report = ReindexReport::default();
    for org_id in orgs {
        vectors.recreate_collection(org_id).await?;
        // Any member can read a team's contexts under RLS.
        let teams: Vec<(Uuid, Option<Uuid>)> = sqlx::query_as(
            "SELECT t.id, (SELECT m.user_id FROM team_members m WHERE m.team_id = t.id ORDER BY m.user_id LIMIT 1) \
             FROM teams t WHERE t.org_id = $1 ORDER BY t.id",
        )
        .bind(org_id)
        .fetch_all(pool)
        .await?;
        for (team_id, member) in teams {
            let Some(member) = member else {
                tracing::warn!(
                    event = "reindex_team_skipped",
                    component = LOG_COMPONENT,
                    org_id = %org_id,
                    team_id = %team_id,
                    "team has no members; its contexts cannot be read and were not indexed"
                );
                report.skipped_teams.push((org_id, team_id));
                continue;
            };
            let mut tx = db::scoped_tx(pool, org_id, member).await?;
            let contexts: Vec<(Uuid, String, String)> =
                sqlx::query_as("SELECT id, title, content FROM contexts WHERE team_id = $1 ORDER BY id")
                    .bind(team_id)
                    .fetch_all(&mut *tx)
                    .await?;
            tx.commit().await?;
            for (context_id, title, content) in contexts {
                let chunks = embed_context(embedder, &title, &content)
                    .await
                    .with_context(|| format!("embedding context {context_id} failed"))?;
                report.chunks += chunks.len();
                vectors.replace_context(org_id, team_id, context_id, chunks).await?;
                report.contexts += 1;
            }
        }
        report.orgs += 1;
        tracing::info!(event = "reindex_org_done", component = LOG_COMPONENT, org_id = %org_id, "organization reindexed");
    }
    Ok(report)
}
