use sqlx::PgExecutor;
use uuid::Uuid;

/// One audit record. Written in the same transaction as the change it
/// describes, so a committed change always has its audit entry.
pub struct AuditEvent<'a> {
    pub org_id: Option<Uuid>,
    pub actor: Uuid,
    pub action: &'a str,
    pub resource_type: &'a str,
    pub resource_id: Option<Uuid>,
    pub details: serde_json::Value,
}

pub async fn record(executor: impl PgExecutor<'_>, event: AuditEvent<'_>) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO audit_logs (org_id, actor_user_id, action, resource_type, resource_id, details) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(event.org_id)
    .bind(event.actor)
    .bind(event.action)
    .bind(event.resource_type)
    .bind(event.resource_id)
    .bind(event.details)
    .execute(executor)
    .await?;
    Ok(())
}
