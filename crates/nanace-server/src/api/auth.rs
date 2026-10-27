use actix_web::{HttpResponse, web};
use nanace_core::validate;
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::auth::AuthUser;
use crate::error::{ApiError, ApiResult};
use crate::oauth::pkce_challenge;
use crate::secret::sha256_hex;
use crate::{AppState, LOG_COMPONENT};

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/auth/dev-login", web::post().to(dev_login))
        .route("/auth/exchange", web::post().to(exchange))
        .route("/me", web::get().to(me));
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DevLoginRequest {
    email: String,
    display_name: Option<String>,
}

#[derive(Serialize)]
struct TokenResponse {
    access_token: String,
    token_type: &'static str,
    expires_in: u64,
    user_id: Uuid,
}

/// Issues a token for any email without verification. Disabled unless
/// `NANACE_DEV_LOGIN=true`; stands in for Google OAuth during development.
async fn dev_login(state: web::Data<AppState>, body: web::Json<DevLoginRequest>) -> ApiResult<HttpResponse> {
    if !state.dev_login {
        return Err(ApiError::NotFound);
    }
    let email = validate::email(&body.email)?;
    let display_name = body
        .display_name
        .as_deref()
        .map(|n| validate::display_name("display_name", n))
        .transpose()?;

    let mut tx = state.pool.begin().await?;
    let user_id: Uuid = sqlx::query_scalar(
        "INSERT INTO users (id, email, display_name, last_login_at) VALUES ($1, $2, $3, now()) \
         ON CONFLICT (email) DO UPDATE \
         SET last_login_at = now(), display_name = COALESCE(EXCLUDED.display_name, users.display_name) \
         RETURNING id",
    )
    .bind(Uuid::new_v4())
    .bind(&email)
    .bind(display_name)
    .fetch_one(&mut *tx)
    .await?;
    audit::record(
        &mut *tx,
        AuditEvent {
            org_id: None,
            actor: user_id,
            action: "auth.dev_login",
            resource_type: "user",
            resource_id: Some(user_id),
            details: serde_json::json!({}),
        },
    )
    .await?;
    tx.commit().await?;

    tracing::info!(event = "dev_login", component = LOG_COMPONENT, user_id = %user_id, "dev login");
    Ok(HttpResponse::Ok().json(TokenResponse {
        access_token: state.jwt.issue(user_id, &email)?,
        token_type: "Bearer",
        expires_in: state.jwt.ttl_secs(),
        user_id,
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExchangeRequest {
    code: String,
    /// PKCE verifier; required for codes issued to the desktop app.
    code_verifier: Option<String>,
}

/// Trades a one-time login code from the Google callback for a session
/// token. Each code works once and expires after a minute; a wrong verifier
/// still consumes the code.
async fn exchange(state: web::Data<AppState>, body: web::Json<ExchangeRequest>) -> ApiResult<HttpResponse> {
    let row: Option<(Uuid, String, Option<String>)> = sqlx::query_as(
        "WITH used AS (DELETE FROM login_codes WHERE code_hash = $1 AND expires_at > now() \
                       RETURNING user_id, code_challenge) \
         SELECT u.id, u.email, used.code_challenge FROM used JOIN users u ON u.id = used.user_id",
    )
    .bind(sha256_hex(&body.code))
    .fetch_optional(&state.pool)
    .await?;
    let (user_id, email, challenge) = row.ok_or(ApiError::Unauthorized)?;
    if let Some(challenge) = challenge {
        let verifier = body.code_verifier.as_deref().ok_or(ApiError::Unauthorized)?;
        let matches: bool = pkce_challenge(verifier).as_bytes().ct_eq(challenge.as_bytes()).into();
        if !matches {
            return Err(ApiError::Unauthorized);
        }
    }
    Ok(HttpResponse::Ok().json(TokenResponse {
        access_token: state.jwt.issue(user_id, &email)?,
        token_type: "Bearer",
        expires_in: state.jwt.ttl_secs(),
        user_id,
    }))
}

#[derive(Serialize, sqlx::FromRow)]
struct MembershipRow {
    org_id: Uuid,
    org_slug: String,
    org_name: String,
    org_role: String,
    team_id: Option<Uuid>,
    team_slug: Option<String>,
    team_role: Option<String>,
}

async fn me(state: web::Data<AppState>, user: AuthUser) -> ApiResult<HttpResponse> {
    let memberships: Vec<MembershipRow> = sqlx::query_as(
        "SELECT o.id AS org_id, o.slug AS org_slug, o.name AS org_name, om.role AS org_role, \
                t.id AS team_id, t.slug AS team_slug, tm.role AS team_role \
         FROM org_members om \
         JOIN organizations o ON o.id = om.org_id \
         LEFT JOIN team_members tm ON tm.org_id = om.org_id AND tm.user_id = om.user_id \
         LEFT JOIN teams t ON t.id = tm.team_id \
         WHERE om.user_id = $1 \
         ORDER BY o.slug, t.slug",
    )
    .bind(user.user_id)
    .fetch_all(&state.pool)
    .await?;
    Ok(HttpResponse::Ok().json(serde_json::json!({
        "user_id": user.user_id,
        "email": user.email,
        "memberships": memberships,
    })))
}
