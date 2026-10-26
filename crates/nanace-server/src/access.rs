//! Application-level RBAC checks. They give callers precise 403/404 answers;
//! the `contexts` RLS policies enforce the same rules again in PostgreSQL.

use nanace_core::{OrgRole, TeamRole};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{ApiError, ApiResult};

/// Returns the caller's org role. Non-members get `NotFound`, so org ids
/// cannot be probed.
pub async fn require_org_member(pool: &PgPool, org_id: Uuid, user_id: Uuid) -> ApiResult<OrgRole> {
    let role: Option<String> = sqlx::query_scalar("SELECT role FROM org_members WHERE org_id = $1 AND user_id = $2")
        .bind(org_id)
        .bind(user_id)
        .fetch_optional(pool)
        .await?;
    match role {
        Some(role) => Ok(role.parse().map_err(anyhow::Error::from)?),
        None => Err(ApiError::NotFound),
    }
}

pub async fn require_org_admin(pool: &PgPool, org_id: Uuid, user_id: Uuid) -> ApiResult<()> {
    match require_org_member(pool, org_id, user_id).await? {
        OrgRole::Admin => Ok(()),
        OrgRole::Member => Err(ApiError::Forbidden),
    }
}

/// Returns the caller's role in a team of the given org, or `None` when the
/// caller is not a member. Fails with `NotFound` when the team is not in the
/// org or the caller is not an org member.
pub async fn team_role(
    pool: &PgPool,
    org_id: Uuid,
    team_id: Uuid,
    user_id: Uuid,
) -> ApiResult<(OrgRole, Option<TeamRole>)> {
    let org_role = require_org_member(pool, org_id, user_id).await?;
    let row: Option<(Option<String>,)> = sqlx::query_as(
        "SELECT m.role FROM teams t \
         LEFT JOIN team_members m ON m.team_id = t.id AND m.user_id = $3 \
         WHERE t.org_id = $1 AND t.id = $2",
    )
    .bind(org_id)
    .bind(team_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    let Some((role,)) = row else {
        return Err(ApiError::NotFound);
    };
    let role = role
        .map(|r| r.parse::<TeamRole>())
        .transpose()
        .map_err(anyhow::Error::from)?;
    Ok((org_role, role))
}

/// Requires at least `min` in the team. Non-members of the team get
/// `NotFound` for read access and `Forbidden` otherwise.
pub async fn require_team_role(
    pool: &PgPool,
    org_id: Uuid,
    team_id: Uuid,
    user_id: Uuid,
    min: TeamRole,
) -> ApiResult<TeamRole> {
    match team_role(pool, org_id, team_id, user_id).await? {
        (_, Some(role)) if role >= min => Ok(role),
        (_, Some(_)) => Err(ApiError::Forbidden),
        (_, None) if min == TeamRole::Reader => Err(ApiError::NotFound),
        (_, None) => Err(ApiError::Forbidden),
    }
}

/// Team ids in the org the caller can read.
pub async fn readable_teams(pool: &PgPool, org_id: Uuid, user_id: Uuid) -> ApiResult<Vec<Uuid>> {
    Ok(
        sqlx::query_scalar("SELECT team_id FROM team_members WHERE org_id = $1 AND user_id = $2")
            .bind(org_id)
            .bind(user_id)
            .fetch_all(pool)
            .await?,
    )
}
