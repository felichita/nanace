use actix_web::cookie::time::Duration as CookieDuration;
use actix_web::cookie::{Cookie, SameSite};
use actix_web::http::header::{CACHE_CONTROL, LOCATION};
use actix_web::{HttpRequest, HttpResponse, web};
use serde::Deserialize;
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::error::{ApiError, ApiResult};
use crate::oauth::{DesktopLogin, GoogleIdentity, GoogleOAuth, LoginError, LoginState, STATE_COOKIE, STATE_TTL};
use crate::secret::{random_token, sha256_hex};
use crate::{AppState, LOG_COMPONENT};

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/auth/google/login", web::get().to(login))
        .route("/auth/google/callback", web::get().to(callback));
}

/// One-time login codes are exchanged by the frontend right after redirect.
const LOGIN_CODE_TTL_SECS: i32 = 60;
const COOKIE_PATH: &str = "/api/v1/auth/google";

fn google(state: &AppState) -> ApiResult<&GoogleOAuth> {
    state.google.as_ref().ok_or(ApiError::NotFound)
}

fn state_cookie(value: String, secure: bool, max_age: CookieDuration) -> Cookie<'static> {
    Cookie::build(STATE_COOKIE, value)
        .path(COOKIE_PATH)
        .http_only(true)
        // Lax: the cookie must accompany Google's top-level redirect back.
        .same_site(SameSite::Lax)
        .secure(secure)
        .max_age(max_age)
        .finish()
}

/// Set by the desktop app to receive the login code on its loopback
/// listener; browsers omit both.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LoginQuery {
    redirect_port: Option<u16>,
    code_challenge: Option<String>,
}

async fn login(state: web::Data<AppState>, query: web::Query<LoginQuery>) -> ApiResult<HttpResponse> {
    let google = google(&state)?;
    let query = query.into_inner();
    let desktop = match (query.redirect_port, query.code_challenge) {
        (None, None) => None,
        (Some(port), Some(challenge)) => Some(DesktopLogin::new(port, challenge).ok_or_else(|| {
            ApiError::BadRequest("redirect_port must be >= 1024 and code_challenge an S256 challenge".to_owned())
        })?),
        _ => {
            return Err(ApiError::BadRequest(
                "redirect_port and code_challenge go together".to_owned(),
            ));
        }
    };
    let (url, cookie) = google.begin(desktop)?;
    let max_age = CookieDuration::seconds(i64::try_from(STATE_TTL.as_secs()).map_err(anyhow::Error::from)?);
    Ok(HttpResponse::Found()
        .insert_header((LOCATION, url))
        .insert_header((CACHE_CONTROL, "no-store"))
        .cookie(state_cookie(cookie, state.secure_cookies, max_age))
        .finish())
}

/// Google appends extra parameters (`scope`, `authuser`, `hd`, ...), so
/// unknown fields are allowed here.
#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

enum CallbackError {
    Login(LoginError),
    Internal(anyhow::Error),
}

impl From<LoginError> for CallbackError {
    fn from(err: LoginError) -> Self {
        Self::Login(err)
    }
}

impl From<sqlx::Error> for CallbackError {
    fn from(err: sqlx::Error) -> Self {
        Self::Internal(err.into())
    }
}

impl From<anyhow::Error> for CallbackError {
    fn from(err: anyhow::Error) -> Self {
        Self::Internal(err)
    }
}

/// Completes sign-in and redirects the browser to the frontend (or the
/// desktop app's loopback listener) with either `?code=<one-time login code>`
/// or `?error=<reason>`.
async fn callback(
    req: HttpRequest,
    state: web::Data<AppState>,
    query: web::Query<CallbackQuery>,
) -> ApiResult<HttpResponse> {
    let google = google(&state)?;
    // Verified first so that errors also reach the client that started the
    // sign-in; an unverifiable state goes to the web frontend.
    let login = match (req.cookie(STATE_COOKIE), &query.state) {
        (Some(cookie), Some(state_param)) => google.verify_state(cookie.value(), state_param),
        _ => Err(LoginError::InvalidState),
    };
    let base = match &login {
        Ok(LoginState {
            desktop: Some(desktop), ..
        }) => desktop.redirect_uri(),
        _ => state.frontend_url.clone(),
    };
    let result = match login {
        _ if query.error.is_some() => Err(LoginError::AccessDenied.into()),
        Ok(login) => complete(&state, google, &query, &login).await,
        Err(err) => Err(err.into()),
    };
    let target = match result {
        Ok(code) => format!("{base}?code={code}"),
        Err(CallbackError::Login(err)) => {
            tracing::warn!(event = "google_login_rejected", component = LOG_COMPONENT, reason = %err, "sign-in rejected");
            format!("{base}?error={err}")
        }
        Err(CallbackError::Internal(err)) => {
            tracing::error!(
                event = "google_login_failed",
                component = LOG_COMPONENT,
                error = %format!("{err:#}"),
                "sign-in failed"
            );
            format!("{base}?error=server_error")
        }
    };
    Ok(HttpResponse::Found()
        .insert_header((LOCATION, target))
        .insert_header((CACHE_CONTROL, "no-store"))
        .cookie(state_cookie(String::new(), state.secure_cookies, CookieDuration::ZERO))
        .finish())
}

async fn complete(
    state: &AppState,
    google: &GoogleOAuth,
    query: &CallbackQuery,
    login: &LoginState,
) -> Result<String, CallbackError> {
    let Some(code) = &query.code else {
        return Err(LoginError::InvalidState.into());
    };
    let identity = google.complete(code, login).await?;

    let mut tx = state.pool.begin().await?;
    let user_id = link_user(&mut tx, &identity).await?;
    let login_code = random_token(32)?;
    sqlx::query("DELETE FROM login_codes WHERE expires_at < now()")
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO login_codes (code_hash, user_id, expires_at, code_challenge) \
         VALUES ($1, $2, now() + make_interval(secs => $3), $4)",
    )
    .bind(sha256_hex(&login_code))
    .bind(user_id)
    .bind(LOGIN_CODE_TTL_SECS)
    .bind(login.desktop.as_ref().map(|d| &d.code_challenge))
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut *tx,
        AuditEvent {
            org_id: None,
            actor: user_id,
            action: "auth.google_login",
            resource_type: "user",
            resource_id: Some(user_id),
            details: serde_json::json!({}),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(login_code)
}

/// Finds the user for a Google identity: by Google subject, else by email
/// for users who were invited before their first sign-in. An email already
/// linked to a different Google account is a conflict, never a takeover.
async fn link_user(
    tx: &mut sqlx::Transaction<'static, sqlx::Postgres>,
    identity: &GoogleIdentity,
) -> Result<Uuid, CallbackError> {
    let by_sub: Option<Uuid> = sqlx::query_scalar(
        "UPDATE users SET last_login_at = now(), display_name = COALESCE(display_name, $2) \
         WHERE google_sub = $1 RETURNING id",
    )
    .bind(&identity.sub)
    .bind(&identity.name)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(id) = by_sub {
        return Ok(id);
    }

    let by_email: Option<(Uuid, Option<String>)> =
        sqlx::query_as("SELECT id, google_sub FROM users WHERE email = $1 FOR UPDATE")
            .bind(&identity.email)
            .fetch_optional(&mut **tx)
            .await?;
    match by_email {
        Some((_, Some(_))) => Err(LoginError::AccountConflict.into()),
        Some((id, None)) => {
            sqlx::query(
                "UPDATE users SET google_sub = $2, last_login_at = now(), \
                 display_name = COALESCE(display_name, $3) WHERE id = $1",
            )
            .bind(id)
            .bind(&identity.sub)
            .bind(&identity.name)
            .execute(&mut **tx)
            .await?;
            Ok(id)
        }
        None => Ok(sqlx::query_scalar(
            "INSERT INTO users (id, email, display_name, google_sub, last_login_at) \
             VALUES ($1, $2, $3, $4, now()) RETURNING id",
        )
        .bind(Uuid::new_v4())
        .bind(&identity.email)
        .bind(&identity.name)
        .bind(&identity.sub)
        .fetch_one(&mut **tx)
        .await?),
    }
}
