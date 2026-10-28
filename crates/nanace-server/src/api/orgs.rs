use actix_web::{HttpResponse, web};
use nanace_core::{OrgRole, validate};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::auth::{AuthUser, WriteUser};
use crate::error::{ApiError, ApiResult};
use crate::{AppState, access};

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/orgs", web::post().to(create_org))
        .route("/orgs/{org_id}/members", web::get().to(list_org_members))
        .route("/orgs/{org_id}/members/{user_id}", web::put().to(set_org_role))
        .route("/orgs/{org_id}/members/{user_id}", web::delete().to(remove_org_member));
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateOrgRequest {
    slug: String,
    name: String,
}

#[derive(Serialize)]
struct OrgResponse {
    id: Uuid,
    slug: String,
    name: String,
}

/// Any authenticated user can create an org and becomes its admin.
async fn create_org(
    state: web::Data<AppState>,
    user: WriteUser,
    body: web::Json<CreateOrgRequest>,
) -> ApiResult<HttpResponse> {
    validate::slug("slug", &body.slug)?;
    let name = validate::display_name("name", &body.name)?;
    let org_id = Uuid::new_v4();

    // Create the collection first: an orphan empty collection is harmless,
    // an org without one would fail its first write.
    state
        .vectors
        .ensure_collection(org_id)
        .await
        .map_err(|_| ApiError::Unavailable("vector store"))?;

    let mut tx = state.pool.begin().await?;
    sqlx::query("INSERT INTO organizations (id, slug, name) VALUES ($1, $2, $3)")
        .bind(org_id)
        .bind(&body.slug)
        .bind(&name)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO org_members (org_id, user_id, role) VALUES ($1, $2, 'admin')")
        .bind(org_id)
        .bind(user.user_id)
        .execute(&mut *tx)
        .await?;
    audit::record(
        &mut *tx,
        AuditEvent {
            org_id: Some(org_id),
            actor: user.user_id,
            action: "org.create",
            resource_type: "organization",
            resource_id: Some(org_id),
            details: serde_json::json!({ "slug": body.slug }),
        },
    )
    .await?;
    tx.commit().await?;

    Ok(HttpResponse::Created().json(OrgResponse {
        id: org_id,
        slug: body.slug.clone(),
        name,
    }))
}

#[derive(Serialize, sqlx::FromRow)]
struct OrgMemberRow {
    user_id: Uuid,
    email: String,
    display_name: Option<String>,
    role: String,
    /// Names of the teams the member belongs to.
    teams: Vec<String>,
}

/// Org admins see every member of the org with their teams.
async fn list_org_members(
    state: web::Data<AppState>,
    user: AuthUser,
    path: web::Path<Uuid>,
) -> ApiResult<HttpResponse> {
    let org_id = path.into_inner();
    access::require_org_admin(&state.pool, org_id, user.user_id).await?;
    let members: Vec<OrgMemberRow> = sqlx::query_as(
        "SELECT u.id AS user_id, u.email, u.display_name, om.role, \
                COALESCE(array_agg(t.name ORDER BY t.name) FILTER (WHERE t.id IS NOT NULL), '{}') AS teams \
         FROM org_members om \
         JOIN users u ON u.id = om.user_id \
         LEFT JOIN team_members tm ON tm.org_id = om.org_id AND tm.user_id = om.user_id \
         LEFT JOIN teams t ON t.id = tm.team_id \
         WHERE om.org_id = $1 \
         GROUP BY u.id, u.email, u.display_name, om.role \
         ORDER BY u.email",
    )
    .bind(org_id)
    .fetch_all(&state.pool)
    .await?;
    Ok(HttpResponse::Ok().json(members))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetOrgRoleRequest {
    role: OrgRole,
}

/// Makes a member an org admin or a plain member. Org admins manage teams and
/// membership but get no implicit access to team content. The last admin
/// cannot be demoted.
async fn set_org_role(
    state: web::Data<AppState>,
    user: WriteUser,
    path: web::Path<(Uuid, Uuid)>,
    body: web::Json<SetOrgRoleRequest>,
) -> ApiResult<HttpResponse> {
    let (org_id, member_id) = path.into_inner();
    access::require_org_admin(&state.pool, org_id, user.user_id).await?;
    let role = body.role;

    let mut tx = state.pool.begin().await?;
    // Same locking as removal: two admins cannot demote each other at once.
    let admins: Vec<Uuid> =
        sqlx::query_scalar("SELECT user_id FROM org_members WHERE org_id = $1 AND role = 'admin' FOR UPDATE")
            .bind(org_id)
            .fetch_all(&mut *tx)
            .await?;
    let current: Option<String> =
        sqlx::query_scalar("SELECT role FROM org_members WHERE org_id = $1 AND user_id = $2 FOR UPDATE")
            .bind(org_id)
            .bind(member_id)
            .fetch_optional(&mut *tx)
            .await?;
    let current = current.ok_or(ApiError::NotFound)?;
    if current == role.as_str() {
        return Ok(HttpResponse::NoContent().finish());
    }
    if current == "admin" && admins.len() <= 1 {
        return Err(ApiError::Conflict("cannot demote the last workspace admin".to_owned()));
    }
    sqlx::query("UPDATE org_members SET role = $3 WHERE org_id = $1 AND user_id = $2")
        .bind(org_id)
        .bind(member_id)
        .bind(role.as_str())
        .execute(&mut *tx)
        .await?;
    audit::record(
        &mut *tx,
        AuditEvent {
            org_id: Some(org_id),
            actor: user.user_id,
            action: "org.member.role",
            resource_type: "user",
            resource_id: Some(member_id),
            details: serde_json::json!({ "from": current, "to": role.as_str() }),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(HttpResponse::NoContent().finish())
}

/// Removes a member from the org and, through the foreign key, from every
/// team in it. Permissions are read per request, so access ends at once.
/// The last org admin cannot be removed.
async fn remove_org_member(
    state: web::Data<AppState>,
    user: WriteUser,
    path: web::Path<(Uuid, Uuid)>,
) -> ApiResult<HttpResponse> {
    let (org_id, member_id) = path.into_inner();
    access::require_org_admin(&state.pool, org_id, user.user_id).await?;

    let mut tx = state.pool.begin().await?;
    // Locks the org's admin rows so two admins cannot remove each other at
    // the same time and leave the org without one.
    let admins: Vec<Uuid> =
        sqlx::query_scalar("SELECT user_id FROM org_members WHERE org_id = $1 AND role = 'admin' FOR UPDATE")
            .bind(org_id)
            .fetch_all(&mut *tx)
            .await?;
    let role: Option<String> =
        sqlx::query_scalar("SELECT role FROM org_members WHERE org_id = $1 AND user_id = $2 FOR UPDATE")
            .bind(org_id)
            .bind(member_id)
            .fetch_optional(&mut *tx)
            .await?;
    let role = role.ok_or(ApiError::NotFound)?;
    if role == "admin" && admins.len() <= 1 {
        return Err(ApiError::Conflict("cannot remove the last workspace admin".to_owned()));
    }
    let teams: i64 = sqlx::query_scalar("SELECT count(*) FROM team_members WHERE org_id = $1 AND user_id = $2")
        .bind(org_id)
        .bind(member_id)
        .fetch_one(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM org_members WHERE org_id = $1 AND user_id = $2")
        .bind(org_id)
        .bind(member_id)
        .execute(&mut *tx)
        .await?;
    audit::record(
        &mut *tx,
        AuditEvent {
            org_id: Some(org_id),
            actor: user.user_id,
            action: "org.member.remove",
            resource_type: "user",
            resource_id: Some(member_id),
            details: serde_json::json!({ "role": role, "teams_removed": teams }),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(HttpResponse::NoContent().finish())
}
