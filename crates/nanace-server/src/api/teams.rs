use actix_web::{HttpResponse, web};
use nanace_core::{OrgRole, TeamRole, validate};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::AppState;
use crate::access;
use crate::audit::{self, AuditEvent};
use crate::auth::{AuthUser, WriteUser};
use crate::error::{ApiError, ApiResult};

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/orgs/{org_id}/teams", web::post().to(create_team))
        .route("/orgs/{org_id}/teams", web::get().to(list_teams))
        .route("/orgs/{org_id}/teams/{team_id}/members", web::get().to(list_members))
        .route("/orgs/{org_id}/teams/{team_id}/members", web::put().to(put_member))
        .route(
            "/orgs/{org_id}/teams/{team_id}/members/{user_id}",
            web::delete().to(remove_member),
        );
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateTeamRequest {
    slug: String,
    name: String,
}

#[derive(Serialize, sqlx::FromRow)]
struct TeamRow {
    id: Uuid,
    slug: String,
    name: String,
    my_role: Option<String>,
}

/// Org admins create teams and become the team's first admin.
async fn create_team(
    state: web::Data<AppState>,
    user: WriteUser,
    path: web::Path<Uuid>,
    body: web::Json<CreateTeamRequest>,
) -> ApiResult<HttpResponse> {
    let org_id = path.into_inner();
    access::require_org_admin(&state.pool, org_id, user.user_id).await?;
    validate::slug("slug", &body.slug)?;
    let name = validate::display_name("name", &body.name)?;
    let team_id = Uuid::new_v4();

    let mut tx = state.pool.begin().await?;
    sqlx::query("INSERT INTO teams (id, org_id, slug, name) VALUES ($1, $2, $3, $4)")
        .bind(team_id)
        .bind(org_id)
        .bind(&body.slug)
        .bind(&name)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO team_members (org_id, team_id, user_id, role) VALUES ($1, $2, $3, 'admin')")
        .bind(org_id)
        .bind(team_id)
        .bind(user.user_id)
        .execute(&mut *tx)
        .await?;
    audit::record(
        &mut *tx,
        AuditEvent {
            org_id: Some(org_id),
            actor: user.user_id,
            action: "team.create",
            resource_type: "team",
            resource_id: Some(team_id),
            details: serde_json::json!({ "slug": body.slug }),
        },
    )
    .await?;
    tx.commit().await?;

    Ok(HttpResponse::Created().json(TeamRow {
        id: team_id,
        slug: body.slug.clone(),
        name,
        my_role: Some(TeamRole::Admin.as_str().to_owned()),
    }))
}

/// Lists every team in the org (names are not secret within an org) with the
/// caller's role in each.
async fn list_teams(state: web::Data<AppState>, user: AuthUser, path: web::Path<Uuid>) -> ApiResult<HttpResponse> {
    let org_id = path.into_inner();
    access::require_org_member(&state.pool, org_id, user.user_id).await?;
    let teams: Vec<TeamRow> = sqlx::query_as(
        "SELECT t.id, t.slug, t.name, m.role AS my_role FROM teams t \
         LEFT JOIN team_members m ON m.team_id = t.id AND m.user_id = $2 \
         WHERE t.org_id = $1 ORDER BY t.slug",
    )
    .bind(org_id)
    .bind(user.user_id)
    .fetch_all(&state.pool)
    .await?;
    Ok(HttpResponse::Ok().json(teams))
}

/// Org admins and team admins manage membership.
async fn require_member_manager(state: &AppState, org_id: Uuid, team_id: Uuid, user_id: Uuid) -> ApiResult<()> {
    match access::team_role(&state.pool, org_id, team_id, user_id).await? {
        (OrgRole::Admin, _) => Ok(()),
        (_, Some(role)) if role.can_manage_members() => Ok(()),
        _ => Err(ApiError::Forbidden),
    }
}

#[derive(Serialize, sqlx::FromRow)]
struct MemberRow {
    user_id: Uuid,
    email: String,
    display_name: Option<String>,
    role: String,
    source: String,
}

async fn list_members(
    state: web::Data<AppState>,
    user: AuthUser,
    path: web::Path<(Uuid, Uuid)>,
) -> ApiResult<HttpResponse> {
    let (org_id, team_id) = path.into_inner();
    match access::team_role(&state.pool, org_id, team_id, user.user_id).await? {
        (OrgRole::Admin, _) | (_, Some(_)) => {}
        _ => return Err(ApiError::Forbidden),
    }
    let members: Vec<MemberRow> = sqlx::query_as(
        "SELECT u.id AS user_id, u.email, u.display_name, m.role, m.source \
         FROM team_members m JOIN users u ON u.id = m.user_id \
         WHERE m.team_id = $1 ORDER BY u.email",
    )
    .bind(team_id)
    .fetch_all(&state.pool)
    .await?;
    Ok(HttpResponse::Ok().json(members))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PutMemberRequest {
    email: String,
    role: TeamRole,
}

/// Adds or updates a team member by email. Unknown emails get a user row that
/// is linked on their first login; the user also joins the org as a member.
async fn put_member(
    state: web::Data<AppState>,
    user: WriteUser,
    path: web::Path<(Uuid, Uuid)>,
    body: web::Json<PutMemberRequest>,
) -> ApiResult<HttpResponse> {
    let (org_id, team_id) = path.into_inner();
    require_member_manager(&state, org_id, team_id, user.user_id).await?;
    let email = validate::email(&body.email)?;

    let mut tx = state.pool.begin().await?;
    let member_id: Uuid = sqlx::query_scalar(
        "INSERT INTO users (id, email) VALUES ($1, $2) \
         ON CONFLICT (email) DO UPDATE SET email = EXCLUDED.email RETURNING id",
    )
    .bind(Uuid::new_v4())
    .bind(&email)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO org_members (org_id, user_id, role) VALUES ($1, $2, 'member') ON CONFLICT DO NOTHING")
        .bind(org_id)
        .bind(member_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO team_members (org_id, team_id, user_id, role, source) VALUES ($1, $2, $3, $4, 'manual') \
         ON CONFLICT (team_id, user_id) DO UPDATE SET role = EXCLUDED.role, source = 'manual'",
    )
    .bind(org_id)
    .bind(team_id)
    .bind(member_id)
    .bind(body.role.as_str())
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut *tx,
        AuditEvent {
            org_id: Some(org_id),
            actor: user.user_id,
            action: "team.member.put",
            resource_type: "team",
            resource_id: Some(team_id),
            details: serde_json::json!({ "user_id": member_id, "role": body.role }),
        },
    )
    .await?;
    tx.commit().await?;

    Ok(HttpResponse::Ok().json(serde_json::json!({ "user_id": member_id, "role": body.role })))
}

async fn remove_member(
    state: web::Data<AppState>,
    user: WriteUser,
    path: web::Path<(Uuid, Uuid, Uuid)>,
) -> ApiResult<HttpResponse> {
    let (org_id, team_id, member_id) = path.into_inner();
    require_member_manager(&state, org_id, team_id, user.user_id).await?;

    let mut tx = state.pool.begin().await?;
    let removed = sqlx::query("DELETE FROM team_members WHERE org_id = $1 AND team_id = $2 AND user_id = $3")
        .bind(org_id)
        .bind(team_id)
        .bind(member_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if removed == 0 {
        return Err(ApiError::NotFound);
    }
    audit::record(
        &mut *tx,
        AuditEvent {
            org_id: Some(org_id),
            actor: user.user_id,
            action: "team.member.remove",
            resource_type: "team",
            resource_id: Some(team_id),
            details: serde_json::json!({ "user_id": member_id }),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(HttpResponse::NoContent().finish())
}
