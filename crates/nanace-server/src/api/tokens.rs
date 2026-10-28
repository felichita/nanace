use actix_web::{HttpResponse, web};
use chrono::{DateTime, Utc};
use nanace_core::validate;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::AppState;
use crate::audit::{self, AuditEvent};
use crate::auth::{AuthMethod, AuthUser, PAT_PREFIX, TokenScope};
use crate::error::{ApiError, ApiResult};
use crate::secret::{random_token, sha256_hex};

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/me/tokens", web::post().to(create_token))
        .route("/me/tokens", web::get().to(list_tokens))
        // Before `{token_id}`, which would otherwise match "current".
        .route("/me/tokens/current", web::delete().to(revoke_current_token))
        .route("/me/tokens/current/rotate", web::post().to(rotate_current_token))
        .route("/me/tokens/{token_id}", web::delete().to(revoke_token));
}

const DEFAULT_EXPIRY_DAYS: i32 = 90;
const MAX_EXPIRY_DAYS: i32 = 365;
const MAX_ACTIVE_TOKENS: i64 = 50;
/// `nnc_pat_` plus four secret characters: enough to tell tokens apart.
const DISPLAY_PREFIX_LEN: usize = PAT_PREFIX.len() + 4;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateTokenRequest {
    name: String,
    scope: TokenScope,
    expires_in_days: Option<i32>,
}

#[derive(Serialize, sqlx::FromRow)]
struct TokenRow {
    id: Uuid,
    name: String,
    token_prefix: String,
    scope: String,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    last_used_at: Option<DateTime<Utc>>,
    revoked_at: Option<DateTime<Utc>>,
}

#[derive(Serialize)]
struct CreatedToken {
    #[serde(flatten)]
    row: TokenRow,
    /// Shown once; only its hash is stored.
    token: String,
}

async fn create_token(
    state: web::Data<AppState>,
    user: AuthUser,
    body: web::Json<CreateTokenRequest>,
) -> ApiResult<HttpResponse> {
    user.require_session()?;
    let name = validate::display_name("name", &body.name)?;
    let days = body.expires_in_days.unwrap_or(DEFAULT_EXPIRY_DAYS);
    if !(1..=MAX_EXPIRY_DAYS).contains(&days) {
        return Err(ApiError::BadRequest("expires_in_days must be 1-365".to_owned()));
    }
    let token = format!("{PAT_PREFIX}{}", random_token(32)?);

    let mut tx = state.pool.begin().await?;
    // Serializes concurrent creations by the same user for the count check.
    sqlx::query("SELECT id FROM users WHERE id = $1 FOR UPDATE")
        .bind(user.user_id)
        .execute(&mut *tx)
        .await?;
    let active: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM api_tokens WHERE user_id = $1 AND revoked_at IS NULL AND expires_at > now()",
    )
    .bind(user.user_id)
    .fetch_one(&mut *tx)
    .await?;
    if active >= MAX_ACTIVE_TOKENS {
        return Err(ApiError::Conflict(
            "too many active tokens; revoke one first".to_owned(),
        ));
    }
    let row: TokenRow = sqlx::query_as(
        "INSERT INTO api_tokens (id, user_id, name, token_hash, token_prefix, scope, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, now() + make_interval(days => $7)) \
         RETURNING id, name, token_prefix, scope, created_at, expires_at, last_used_at, revoked_at",
    )
    .bind(Uuid::new_v4())
    .bind(user.user_id)
    .bind(&name)
    .bind(sha256_hex(&token))
    .bind(&token[..DISPLAY_PREFIX_LEN])
    .bind(body.scope.as_str())
    .bind(days)
    .fetch_one(&mut *tx)
    .await?;
    audit::record(
        &mut *tx,
        AuditEvent {
            org_id: None,
            actor: user.user_id,
            action: "token.create",
            resource_type: "api_token",
            resource_id: Some(row.id),
            details: serde_json::json!({ "scope": body.scope, "expires_in_days": days }),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(HttpResponse::Created().json(CreatedToken { row, token }))
}

async fn list_tokens(state: web::Data<AppState>, user: AuthUser) -> ApiResult<HttpResponse> {
    let rows: Vec<TokenRow> = sqlx::query_as(
        "SELECT id, name, token_prefix, scope, created_at, expires_at, last_used_at, revoked_at \
         FROM api_tokens WHERE user_id = $1 ORDER BY created_at DESC",
    )
    .bind(user.user_id)
    .fetch_all(&state.pool)
    .await?;
    Ok(HttpResponse::Ok().json(rows))
}

async fn revoke_token(state: web::Data<AppState>, user: AuthUser, path: web::Path<Uuid>) -> ApiResult<HttpResponse> {
    user.require_session()?;
    revoke(&state, &user, path.into_inner()).await
}

/// Revokes the token that authenticates this request (sign-out for clients
/// such as the desktop app). Any scope may revoke itself.
async fn revoke_current_token(state: web::Data<AppState>, user: AuthUser) -> ApiResult<HttpResponse> {
    let AuthMethod::Token { id, .. } = user.method else {
        return Err(ApiError::BadRequest(
            "request is not authenticated with a token".to_owned(),
        ));
    };
    revoke(&state, &user, id).await
}

/// How long a rotated token keeps working, so clients that already loaded it
/// (for example a running Claude session) are not cut off mid-task.
const ROTATION_GRACE_HOURS: i32 = 24;

/// Replaces the token that authenticates this request with a new one of the
/// same name, scope and lifetime. Lets clients rotate credentials without an
/// interactive session; the rotation chain still ends one year after the
/// first token was issued, and a token can be rotated only once.
async fn rotate_current_token(state: web::Data<AppState>, user: AuthUser) -> ApiResult<HttpResponse> {
    let AuthMethod::Token { id, .. } = user.method else {
        return Err(ApiError::BadRequest(
            "request is not authenticated with a token".to_owned(),
        ));
    };
    let token = format!("{PAT_PREFIX}{}", random_token(32)?);

    let mut tx = state.pool.begin().await?;
    let rotated: Option<Option<chrono::DateTime<Utc>>> = sqlx::query_scalar(
        "SELECT rotated_at FROM api_tokens \
         WHERE id = $1 AND user_id = $2 AND revoked_at IS NULL AND expires_at > now() FOR UPDATE",
    )
    .bind(id)
    .bind(user.user_id)
    .fetch_optional(&mut *tx)
    .await?;
    match rotated {
        None => return Err(ApiError::Unauthorized),
        Some(Some(_)) => return Err(ApiError::Conflict("token was already rotated".to_owned())),
        Some(None) => {}
    }
    // Same lifetime as the old token, capped at one year after the chain began.
    let row: Option<TokenRow> = sqlx::query_as(
        "INSERT INTO api_tokens (id, user_id, name, token_hash, token_prefix, scope, expires_at, chain_started_at) \
         SELECT $1, user_id, name, $2, $3, scope, \
                LEAST(now() + (expires_at - created_at), chain_started_at + interval '365 days'), chain_started_at \
         FROM api_tokens \
         WHERE id = $4 AND chain_started_at + interval '365 days' > now() + interval '1 day' \
         RETURNING id, name, token_prefix, scope, created_at, expires_at, last_used_at, revoked_at",
    )
    .bind(Uuid::new_v4())
    .bind(sha256_hex(&token))
    .bind(&token[..DISPLAY_PREFIX_LEN])
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let row = row.ok_or_else(|| {
        ApiError::Conflict("token is at its maximum age; sign in again to issue a new one".to_owned())
    })?;
    sqlx::query(
        "UPDATE api_tokens SET rotated_at = now(), \
                expires_at = LEAST(expires_at, now() + make_interval(hours => $2)) WHERE id = $1",
    )
    .bind(id)
    .bind(ROTATION_GRACE_HOURS)
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut *tx,
        AuditEvent {
            org_id: None,
            actor: user.user_id,
            action: "token.rotate",
            resource_type: "api_token",
            resource_id: Some(row.id),
            details: serde_json::json!({ "previous_id": id }),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(HttpResponse::Created().json(CreatedToken { row, token }))
}

async fn revoke(state: &AppState, user: &AuthUser, token_id: Uuid) -> ApiResult<HttpResponse> {
    let mut tx = state.pool.begin().await?;
    let revoked =
        sqlx::query("UPDATE api_tokens SET revoked_at = now() WHERE id = $1 AND user_id = $2 AND revoked_at IS NULL")
            .bind(token_id)
            .bind(user.user_id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
    if revoked == 0 {
        return Err(ApiError::NotFound);
    }
    audit::record(
        &mut *tx,
        AuditEvent {
            org_id: None,
            actor: user.user_id,
            action: "token.revoke",
            resource_type: "api_token",
            resource_id: Some(token_id),
            details: serde_json::json!({}),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(HttpResponse::NoContent().finish())
}
