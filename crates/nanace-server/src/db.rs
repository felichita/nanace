use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!();

/// Opens a transaction whose statements run as `nanace_tenant` with the
/// caller's org and user bound, so the `contexts` RLS policies apply. Settings
/// and role are transaction-local and reset when the connection returns to
/// the pool.
pub async fn scoped_tx(
    pool: &PgPool,
    org_id: Uuid,
    user_id: Uuid,
) -> Result<Transaction<'static, Postgres>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT set_config('nanace.org_id', $1, true), set_config('nanace.user_id', $2, true)")
        .bind(org_id.to_string())
        .bind(user_id.to_string())
        .execute(&mut *tx)
        .await?;
    sqlx::query("SET LOCAL ROLE nanace_tenant").execute(&mut *tx).await?;
    Ok(tx)
}

/// True when the connected role skips row-level security entirely, which
/// silently disables the `contexts` policies.
pub async fn bypasses_rls(pool: &PgPool) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT rolsuper OR rolbypassrls FROM pg_roles WHERE rolname = current_user")
        .fetch_one(pool)
        .await
}
