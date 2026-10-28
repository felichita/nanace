//! Upgrading an installation: SQL migrations, then application upgrades.
//!
//! SQL migrations (`migrations/`) change the schema. Application upgrades are
//! steps written in Rust for what SQL cannot do, such as re-deriving data
//! from content. Like a module version map, each upgrade has a version, runs
//! once in version order, and is recorded in `app_upgrades`. Besides the
//! versioned steps, a change of embedding model (recorded in `system_state`)
//! triggers a full reindex.
//!
//! `run` applies everything under a PostgreSQL advisory lock, so replicas
//! starting at the same time upgrade once. `status` only reports, for
//! `nanace-server migrate --check` and for servers started with
//! `NANACE_RUN_MIGRATIONS=false`, which refuse to start on a schema they do not
//! match.

use std::collections::HashSet;

use nanace_embed::Embedder;
use sqlx::PgPool;
use uuid::Uuid;

use crate::api::contexts::replace_links;
use crate::vectors::VectorStore;
use crate::{LOG_COMPONENT, db, reindex};

/// Serializes upgrades across processes (an arbitrary, fixed key).
const UPGRADE_LOCK: i64 = 0x6e61_6e61_6365_0001;
const MODEL_KEY: &str = "embedding_model";

/// Versioned application upgrades, in order. Append only; never renumber.
pub const UPGRADES: &[(i32, &str)] = &[(1, "backfill wiki links of contexts created before links existed")];

#[derive(Debug, Default)]
pub struct Status {
    /// SQL migrations in this binary that the database has not applied.
    pub pending_migrations: Vec<i64>,
    /// SQL migrations applied to the database that this binary does not know:
    /// the database belongs to a newer version.
    pub unknown_migrations: Vec<i64>,
    pub pending_upgrades: Vec<i32>,
    pub unknown_upgrades: Vec<i32>,
    /// Vectors were made by a different model (stored, configured).
    pub model_change: Option<(String, String)>,
}

impl Status {
    pub fn is_current(&self) -> bool {
        self.pending_migrations.is_empty()
            && self.unknown_migrations.is_empty()
            && self.pending_upgrades.is_empty()
            && self.unknown_upgrades.is_empty()
            && self.model_change.is_none()
    }

    /// The database is newer than this binary.
    pub fn is_newer(&self) -> bool {
        !self.unknown_migrations.is_empty() || !self.unknown_upgrades.is_empty()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ModelAction {
    /// Nothing recorded yet: remember the configured model.
    Record,
    /// Vectors come from another model: reindex, then remember.
    Reindex,
    Keep,
}

/// What to do about the embedding model, given what is recorded.
pub fn model_action(stored: Option<&str>, configured: &str) -> ModelAction {
    match stored {
        None => ModelAction::Record,
        Some(stored) if stored == configured => ModelAction::Keep,
        Some(_) => ModelAction::Reindex,
    }
}

async fn table_exists(pool: &PgPool, table: &str) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
        .bind(table)
        .fetch_one(pool)
        .await
}

/// Compares the database with this binary without changing anything.
/// `model_key` is the configured embedder's `<id>:<dimensions>`.
pub async fn status(pool: &PgPool, model_key: &str) -> anyhow::Result<Status> {
    let applied: Vec<i64> = if table_exists(pool, "_sqlx_migrations").await? {
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE success ORDER BY version")
            .fetch_all(pool)
            .await?
    } else {
        Vec::new()
    };
    let known: Vec<i64> = db::MIGRATOR.iter().map(|m| m.version).collect();
    let mut status = Status {
        pending_migrations: known.iter().copied().filter(|v| !applied.contains(v)).collect(),
        unknown_migrations: applied.iter().copied().filter(|v| !known.contains(v)).collect(),
        ..Status::default()
    };
    if !table_exists(pool, "app_upgrades").await? {
        status.pending_upgrades = UPGRADES.iter().map(|(v, _)| *v).collect();
        return Ok(status);
    }
    let done: HashSet<i32> = sqlx::query_scalar("SELECT version FROM app_upgrades")
        .fetch_all(pool)
        .await?
        .into_iter()
        .collect();
    let known: HashSet<i32> = UPGRADES.iter().map(|(v, _)| *v).collect();
    status.pending_upgrades = UPGRADES.iter().map(|(v, _)| *v).filter(|v| !done.contains(v)).collect();
    status.unknown_upgrades = done.difference(&known).copied().collect();
    status.unknown_upgrades.sort_unstable();
    let stored: Option<String> = sqlx::query_scalar("SELECT value FROM system_state WHERE key = $1")
        .bind(MODEL_KEY)
        .fetch_optional(pool)
        .await?;
    if model_action(stored.as_deref(), model_key) == ModelAction::Reindex {
        status.model_change = stored.map(|s| (s, model_key.to_owned()));
    }
    Ok(status)
}

#[derive(Debug, Default)]
pub struct Report {
    pub upgrades: Vec<i32>,
    pub reindexed: bool,
}

/// Applies pending SQL migrations and application upgrades.
pub async fn run(pool: &PgPool, vectors: &VectorStore, embedder: &dyn Embedder) -> anyhow::Result<Report> {
    db::MIGRATOR.run(pool).await?;
    // Session-level lock on one connection; the work itself uses the pool.
    let mut lock = pool.acquire().await?;
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(UPGRADE_LOCK)
        .execute(&mut *lock)
        .await?;
    let result = run_locked(pool, vectors, embedder).await;
    sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(UPGRADE_LOCK)
        .execute(&mut *lock)
        .await?;
    result
}

async fn run_locked(pool: &PgPool, vectors: &VectorStore, embedder: &dyn Embedder) -> anyhow::Result<Report> {
    let mut report = Report::default();
    let done: Vec<i32> = sqlx::query_scalar("SELECT version FROM app_upgrades")
        .fetch_all(pool)
        .await?;
    for (version, name) in UPGRADES {
        if done.contains(version) {
            continue;
        }
        let details = match version {
            1 => backfill_wiki_links(pool).await?,
            other => anyhow::bail!("no handler for application upgrade {other}"),
        };
        sqlx::query("INSERT INTO app_upgrades (version, name, details) VALUES ($1, $2, $3)")
            .bind(version)
            .bind(name)
            .bind(&details)
            .execute(pool)
            .await?;
        tracing::info!(
            event = "app_upgrade_applied",
            component = LOG_COMPONENT,
            version = version,
            details = %details,
            "application upgrade applied"
        );
        report.upgrades.push(*version);
    }

    let model_key = format!("{}:{}", embedder.model_id(), embedder.dimensions());
    let stored: Option<String> = sqlx::query_scalar("SELECT value FROM system_state WHERE key = $1")
        .bind(MODEL_KEY)
        .fetch_optional(pool)
        .await?;
    match model_action(stored.as_deref(), &model_key) {
        ModelAction::Keep => {}
        ModelAction::Record => record_embedding_model(pool, &model_key).await?,
        ModelAction::Reindex => {
            tracing::warn!(
                event = "embedding_model_changed",
                component = LOG_COMPONENT,
                from = stored.as_deref().unwrap_or_default(),
                to = %model_key,
                "embedding model changed; reindexing every organization"
            );
            reindex::reindex(pool, vectors, embedder, None).await?;
            record_embedding_model(pool, &model_key).await?;
            report.reindexed = true;
        }
    }
    Ok(report)
}

/// Records which model produced the vectors (after a reindex, or when an
/// installation starts tracking it).
pub async fn record_embedding_model(pool: &PgPool, model_key: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO system_state (key, value) VALUES ($1, $2) \
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = now()",
    )
    .bind(MODEL_KEY)
    .bind(model_key)
    .execute(pool)
    .await?;
    Ok(())
}

/// Upgrade 1: contexts created before `context_links` existed only got their
/// links on their next edit. Stores them now. Links are written under RLS as a
/// writer of the team; teams without one are reported.
async fn backfill_wiki_links(pool: &PgPool) -> anyhow::Result<serde_json::Value> {
    let teams: Vec<(Uuid, Uuid, Option<Uuid>)> = sqlx::query_as(
        "SELECT t.org_id, t.id, (SELECT m.user_id FROM team_members m \
                                 WHERE m.team_id = t.id AND m.role IN ('writer', 'admin') \
                                 ORDER BY m.user_id LIMIT 1) \
         FROM teams t ORDER BY t.org_id, t.id",
    )
    .fetch_all(pool)
    .await?;
    let (mut contexts, mut skipped_teams) = (0, 0);
    for (org_id, team_id, writer) in teams {
        let Some(writer) = writer else {
            skipped_teams += 1;
            continue;
        };
        let mut tx = db::scoped_tx(pool, org_id, writer).await?;
        let rows: Vec<(Uuid, String)> = sqlx::query_as("SELECT id, content FROM contexts WHERE team_id = $1")
            .bind(team_id)
            .fetch_all(&mut *tx)
            .await?;
        for (context_id, content) in &rows {
            replace_links(&mut tx, org_id, *context_id, content)
                .await
                .map_err(|err| anyhow::anyhow!("storing links of context {context_id} failed: {err}"))?;
        }
        tx.commit().await?;
        contexts += rows.len();
    }
    Ok(serde_json::json!({ "contexts": contexts, "skipped_teams": skipped_teams }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_changes_trigger_a_reindex_once_recorded() {
        assert_eq!(model_action(None, "bge:384"), ModelAction::Record);
        assert_eq!(model_action(Some("bge:384"), "bge:384"), ModelAction::Keep);
        assert_eq!(model_action(Some("bge:384"), "e5:384"), ModelAction::Reindex);
        assert_eq!(model_action(Some("bge:384"), "bge:768"), ModelAction::Reindex);
    }

    #[test]
    fn upgrade_versions_are_unique_and_ordered() {
        let versions: Vec<i32> = UPGRADES.iter().map(|(v, _)| *v).collect();
        let mut sorted = versions.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(versions, sorted);
    }
}
