use actix_web::{HttpResponse, web};
use nanace_core::validate;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::AppState;
use crate::audit::{self, AuditEvent};
use crate::auth::AuthUser;
use crate::error::{ApiError, ApiResult};

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/orgs", web::post().to(create_org));
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
    user: AuthUser,
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
