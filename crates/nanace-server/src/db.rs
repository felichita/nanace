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

/// Switches an open transaction to `nanace_tenant` with only the user bound
/// and no organization. Statements before this call ran as the pool role;
/// everything after it is subject to row-level security.
pub async fn enter_user_scope(tx: &mut Transaction<'static, Postgres>, user_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT set_config('nanace.org_id', '', true), set_config('nanace.user_id', $1, true)")
        .bind(user_id.to_string())
        .execute(&mut **tx)
        .await?;
    sqlx::query("SET LOCAL ROLE nanace_tenant").execute(&mut **tx).await?;
    Ok(())
}

/// Opens a transaction as `nanace_tenant` with only the user bound, for data
/// that belongs to a user rather than an organization (their P2P peers).
/// Org-scoped policies match nothing in it.
pub async fn user_tx(pool: &PgPool, user_id: Uuid) -> Result<Transaction<'static, Postgres>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    enter_user_scope(&mut tx, user_id).await?;
    Ok(tx)
}

/// Opens a transaction as `nanace_tenant` for the server's own P2P
/// maintenance, with no user or organization. The P2P policies let it read
/// peers and presence and delete presence that has expired; nothing else.
/// Never use it on behalf of a request's caller.
pub async fn p2p_system_tx(pool: &PgPool) -> Result<Transaction<'static, Postgres>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        "SELECT set_config('nanace.org_id', '', true), set_config('nanace.user_id', '', true), \
                set_config('nanace.p2p_system', 'on', true)",
    )
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
