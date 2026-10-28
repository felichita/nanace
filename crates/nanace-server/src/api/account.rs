use actix_web::{HttpResponse, web};
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::auth::AuthUser;
use crate::error::{ApiError, ApiResult};
use crate::{AppState, LOG_COMPONENT};

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/me", web::delete().to(delete_account));
}

/// Deletes an organization with everything in it (teams, memberships,
/// contexts, links, trash, its audit history) inside `tx`. The caller deletes
/// the Qdrant collection after commit. Contexts are removed by the foreign-key
/// cascade, which PostgreSQL runs without row security.
pub(crate) async fn delete_org(tx: &mut sqlx::Transaction<'static, sqlx::Postgres>, org_id: Uuid) -> ApiResult<()> {
    sqlx::query("DELETE FROM audit_logs WHERE org_id = $1")
        .bind(org_id)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM organizations WHERE id = $1")
        .bind(org_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Erases the caller's account (GDPR): memberships, tokens and login codes go,
/// the user row is anonymized, and organizations where the caller is the only
/// member are deleted. Pages they wrote stay with their teams, attributed to a
/// deleted user. Refused while the caller is the last admin of an organization
/// that has other members. Needs an interactive session.
async fn delete_account(state: web::Data<AppState>, user: AuthUser) -> ApiResult<HttpResponse> {
    user.require_session()?;
    let mut tx = state.pool.begin().await?;
    sqlx::query("SELECT id FROM users WHERE id = $1 FOR UPDATE")
        .bind(user.user_id)
        .execute(&mut *tx)
        .await?;
    let orgs: Vec<(Uuid, String, String)> = sqlx::query_as(
        "SELECT o.id, o.name, m.role FROM org_members m JOIN organizations o ON o.id = m.org_id \
         WHERE m.user_id = $1 ORDER BY o.name",
    )
    .bind(user.user_id)
    .fetch_all(&mut *tx)
    .await?;

    let (mut sole, mut blocked) = (Vec::new(), Vec::new());
    for (org_id, name, role) in &orgs {
        // Locked like member removal, so admins cannot leave concurrently.
        let members: Vec<(Uuid, String)> =
            sqlx::query_as("SELECT user_id, role FROM org_members WHERE org_id = $1 FOR UPDATE")
                .bind(org_id)
                .fetch_all(&mut *tx)
                .await?;
        let admins = members.iter().filter(|(_, r)| r == "admin").count();
        if members.len() == 1 {
            sole.push(*org_id);
        } else if role == "admin" && admins == 1 {
            blocked.push(name.clone());
        }
    }
    if !blocked.is_empty() {
        return Err(ApiError::Conflict(format!(
            "make another member a workspace admin first: {}",
            blocked.join(", ")
        )));
    }

    for org_id in &sole {
        delete_org(&mut tx, *org_id).await?;
    }
    sqlx::query("DELETE FROM org_members WHERE user_id = $1")
        .bind(user.user_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM api_tokens WHERE user_id = $1")
        .bind(user.user_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM login_codes WHERE user_id = $1")
        .bind(user.user_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "UPDATE users SET email = 'deleted-' || id || '@deleted.invalid', display_name = NULL, google_sub = NULL, \
                last_login_at = NULL, deleted_at = now() WHERE id = $1",
    )
    .bind(user.user_id)
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut *tx,
        AuditEvent {
            org_id: None,
            actor: user.user_id,
            action: "user.delete",
            resource_type: "user",
            resource_id: Some(user.user_id),
            details: serde_json::json!({ "deleted_orgs": sole }),
        },
    )
    .await?;
    tx.commit().await?;

    for org_id in sole {
        if let Err(err) = state.vectors.delete_collection(org_id).await {
            tracing::warn!(
                event = "vector_collection_delete_failed",
                component = LOG_COMPONENT,
                org_id = %org_id,
                error = %format!("{err:#}"),
                "collection of a deleted organization was left behind"
            );
        }
    }
    Ok(HttpResponse::NoContent().finish())
}
