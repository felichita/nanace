//! Trash retention: contexts stay in the trash for
//! `NANACE_TRASH_RETENTION_DAYS` (default 7) and are then deleted for good.
//!
//! The purge runs under RLS like any other trash access: for each team it
//! deletes as one of the team's writers (only writers may purge). Teams with
//! no writer keep their trash until they have one. Every purge is audited as
//! `system.trash_purge`. Running it from several replicas is harmless: a
//! second run finds nothing to delete.

use sqlx::PgPool;
use uuid::Uuid;

use crate::{audit, db};

/// Deletes trashed contexts older than `retention_days` days, for `only_org`
/// or every organization. Returns how many were deleted.
pub async fn purge_expired(pool: &PgPool, retention_days: u32, only_org: Option<Uuid>) -> anyhow::Result<u64> {
    let days = i32::try_from(retention_days)?;
    let teams: Vec<(Uuid, Uuid, Option<Uuid>)> = sqlx::query_as(
        "SELECT t.org_id, t.id, (SELECT m.user_id FROM team_members m \
                                 WHERE m.team_id = t.id AND m.role IN ('writer', 'admin') \
                                 ORDER BY m.user_id LIMIT 1) \
         FROM teams t WHERE $1::uuid IS NULL OR t.org_id = $1 ORDER BY t.org_id, t.id",
    )
    .bind(only_org)
    .fetch_all(pool)
    .await?;
    let mut purged = 0;
    for (org_id, team_id, writer) in teams {
        let Some(writer) = writer else { continue };
        let mut tx = db::scoped_tx(pool, org_id, writer).await?;
        let deleted = sqlx::query(
            "DELETE FROM context_trash WHERE team_id = $1 AND deleted_at < now() - make_interval(days => $2)",
        )
        .bind(team_id)
        .bind(days)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if deleted > 0 {
            audit::record_system(
                &mut *tx,
                Some(org_id),
                "system.trash_purge",
                serde_json::json!({ "team_id": team_id, "purged": deleted, "retention_days": retention_days }),
            )
            .await?;
        }
        tx.commit().await?;
        purged += deleted;
    }
    Ok(purged)
}
