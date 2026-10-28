//! Request authentication: session JWTs and personal access tokens.
//!
//! Both only identify the user. Org and team permissions are read from
//! PostgreSQL on every request, so membership changes take effect immediately.

use std::ops::Deref;
use std::time::{SystemTime, UNIX_EPOCH};

use actix_web::dev::Payload;
use actix_web::http::header::AUTHORIZATION;
use actix_web::{FromRequest, HttpRequest, web};
use anyhow::Context;
use futures_util::future::LocalBoxFuture;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::AppState;
use crate::error::{ApiError, ApiResult};
use crate::secret::sha256_hex;

const ISSUER: &str = "nanace";
const AUDIENCE: &str = "nanace-api";

#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    sub: Uuid,
    email: String,
    iss: String,
    aud: String,
    iat: u64,
    exp: u64,
}

pub struct JwtKeys {
    encoding: EncodingKey,
    decoding: DecodingKey,
    validation: Validation,
    ttl_secs: u64,
}

impl JwtKeys {
    pub fn new(secret: &SecretString, ttl_secs: u64) -> Self {
        let bytes = secret.expose_secret().as_bytes();
        let mut validation = Validation::new(Algorithm::HS256);
        validation.set_issuer(&[ISSUER]);
        validation.set_audience(&[AUDIENCE]);
        validation.leeway = 30;
        validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
        Self {
            encoding: EncodingKey::from_secret(bytes),
            decoding: DecodingKey::from_secret(bytes),
            validation,
            ttl_secs,
        }
    }

    pub fn ttl_secs(&self) -> u64 {
        self.ttl_secs
    }

    pub fn issue(&self, user_id: Uuid, email: &str) -> anyhow::Result<String> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock is before the Unix epoch")?
            .as_secs();
        let claims = Claims {
            sub: user_id,
            email: email.to_owned(),
            iss: ISSUER.to_owned(),
            aud: AUDIENCE.to_owned(),
            iat: now,
            exp: now + self.ttl_secs,
        };
        jsonwebtoken::encode(&Header::new(Algorithm::HS256), &claims, &self.encoding).context("failed to sign JWT")
    }

    fn verify(&self, token: &str) -> Option<AuthUser> {
        jsonwebtoken::decode::<Claims>(token, &self.decoding, &self.validation)
            .ok()
            .map(|data| AuthUser {
                user_id: data.claims.sub,
                email: data.claims.email,
                method: AuthMethod::Session,
            })
    }
}

/// Prefix of personal access tokens; lets the extractor route them without
/// trying JWT decoding, and makes leaked tokens recognizable to scanners.
pub const PAT_PREFIX: &str = "nnc_pat_";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenScope {
    /// GET requests and search.
    Read,
    /// Everything the user's roles allow.
    Write,
}

impl TokenScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMethod {
    /// Interactive sign-in (JWT).
    Session,
    /// Personal access token.
    Token { id: Uuid, scope: TokenScope },
}

/// The authenticated caller. Extracting it rejects requests without a valid
/// session JWT or personal access token. Any token scope may read; handlers
/// that change state take [`WriteUser`] instead.
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub user_id: Uuid,
    pub email: String,
    pub method: AuthMethod,
}

impl AuthUser {
    /// Token management needs an interactive session, so a leaked access
    /// token cannot mint or revoke tokens.
    pub fn require_session(&self) -> ApiResult<()> {
        match self.method {
            AuthMethod::Session => Ok(()),
            AuthMethod::Token { .. } => Err(ApiError::Forbidden),
        }
    }
}

fn bearer(req: &HttpRequest) -> Option<String> {
    req.headers()
        .get(AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(|t| t.trim().to_owned())
}

async fn authenticate_pat(state: &AppState, token: &str) -> ApiResult<AuthUser> {
    let row: Option<(Uuid, Uuid, String, String)> = sqlx::query_as(
        "SELECT t.id, t.user_id, u.email, t.scope FROM api_tokens t JOIN users u ON u.id = t.user_id \
         WHERE t.token_hash = $1 AND t.revoked_at IS NULL AND t.expires_at > now() AND u.deleted_at IS NULL",
    )
    .bind(sha256_hex(token))
    .fetch_optional(&state.pool)
    .await?;
    let (id, user_id, email, scope) = row.ok_or(ApiError::Unauthorized)?;
    let scope = match scope.as_str() {
        "read" => TokenScope::Read,
        "write" => TokenScope::Write,
        other => return Err(anyhow::anyhow!("unknown token scope `{other}`").into()),
    };
    // Coarse last-use tracking: at most one write per token per minute.
    sqlx::query(
        "UPDATE api_tokens SET last_used_at = now() \
         WHERE id = $1 AND (last_used_at IS NULL OR last_used_at < now() - interval '1 minute')",
    )
    .bind(id)
    .execute(&state.pool)
    .await?;
    Ok(AuthUser {
        user_id,
        email,
        method: AuthMethod::Token { id, scope },
    })
}

impl FromRequest for AuthUser {
    type Error = ApiError;
    type Future = LocalBoxFuture<'static, Result<Self, Self::Error>>;

    fn from_request(req: &HttpRequest, _: &mut Payload) -> Self::Future {
        let state = req.app_data::<web::Data<AppState>>().cloned();
        let token = bearer(req);
        Box::pin(async move {
            let (Some(state), Some(token)) = (state, token) else {
                return Err(ApiError::Unauthorized);
            };
            if token.starts_with(PAT_PREFIX) {
                authenticate_pat(&state, &token).await
            } else {
                let user = state.jwt.verify(&token).ok_or(ApiError::Unauthorized)?;
                // Sessions are stateless; a deleted account must still stop
                // working before its session expires.
                let active: Option<bool> = sqlx::query_scalar("SELECT deleted_at IS NULL FROM users WHERE id = $1")
                    .bind(user.user_id)
                    .fetch_optional(&state.pool)
                    .await?;
                if active != Some(true) {
                    return Err(ApiError::Unauthorized);
                }
                Ok(user)
            }
        })
    }
}

/// An authenticated caller allowed to change state: a session, or a token
/// with `write` scope. Role checks still apply on top.
#[derive(Debug, Clone)]
pub struct WriteUser(pub AuthUser);

impl Deref for WriteUser {
    type Target = AuthUser;

    fn deref(&self) -> &AuthUser {
        &self.0
    }
}

impl FromRequest for WriteUser {
    type Error = ApiError;
    type Future = LocalBoxFuture<'static, Result<Self, Self::Error>>;

    fn from_request(req: &HttpRequest, payload: &mut Payload) -> Self::Future {
        let user = AuthUser::from_request(req, payload);
        Box::pin(async move {
            let user = user.await?;
            match user.method {
                AuthMethod::Token {
                    scope: TokenScope::Read,
                    ..
                } => Err(ApiError::Forbidden),
                _ => Ok(Self(user)),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(secret: &str, ttl: u64) -> JwtKeys {
        JwtKeys::new(&SecretString::from(secret.to_owned()), ttl)
    }

    #[test]
    fn issued_token_verifies() {
        let keys = keys("0123456789abcdef0123456789abcdef", 3600);
        let id = Uuid::new_v4();
        let user = keys.verify(&keys.issue(id, "a@b.com").unwrap()).unwrap();
        assert_eq!(user.user_id, id);
        assert_eq!(user.email, "a@b.com");
    }

    #[test]
    fn rejects_foreign_tampered_and_expired_tokens() {
        let ours = keys("0123456789abcdef0123456789abcdef", 3600);
        let theirs = keys("fedcba9876543210fedcba9876543210", 3600);
        let id = Uuid::new_v4();

        assert!(ours.verify(&theirs.issue(id, "a@b.com").unwrap()).is_none());

        let mut tampered = ours.issue(id, "a@b.com").unwrap();
        tampered.pop();
        assert!(ours.verify(&tampered).is_none());
        assert!(ours.verify("not-a-jwt").is_none());

        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let expired = Claims {
            sub: id,
            email: "a@b.com".to_owned(),
            iss: ISSUER.to_owned(),
            aud: AUDIENCE.to_owned(),
            iat: now - 7200,
            exp: now - 3600,
        };
        let token = jsonwebtoken::encode(&Header::new(Algorithm::HS256), &expired, &ours.encoding).unwrap();
        assert!(ours.verify(&token).is_none());

        let wrong_aud = Claims {
            aud: "other".to_owned(),
            exp: now + 3600,
            ..expired
        };
        let token = jsonwebtoken::encode(&Header::new(Algorithm::HS256), &wrong_aud, &ours.encoding).unwrap();
        assert!(ours.verify(&token).is_none());
    }
}
