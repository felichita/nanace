//! Minimal typed client for the nanace REST API.

use reqwest::{Method, StatusCode};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use tokio::sync::RwLock;
use uuid::Uuid;

pub enum Credentials {
    /// A bearer token issued by the API.
    Token(SecretString),
    /// Logs in through the development-only endpoint and logs in again when
    /// the token expires. Works only when the API has dev login enabled.
    DevEmail(String),
}

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("not authenticated: the nanace token is missing, invalid, or expired")]
    Unauthorized,
    #[error("permission denied")]
    Forbidden,
    #[error("not found (or not visible to you)")]
    NotFound,
    #[error("invalid request: {0}")]
    BadRequest(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("nanace API is unavailable: {0}")]
    Unavailable(String),
    #[error("unexpected nanace API response: {0}")]
    Unexpected(String),
}

#[derive(Debug, Deserialize)]
pub struct Me {
    pub email: String,
    pub memberships: Vec<Membership>,
}

/// One row per (org, team) membership; `team_*` are null for orgs where the
/// user belongs to no team.
#[derive(Debug, Deserialize)]
pub struct Membership {
    pub org_id: Uuid,
    pub org_slug: String,
    pub org_name: String,
    pub org_role: String,
    pub team_id: Option<Uuid>,
    pub team_slug: Option<String>,
    pub team_role: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SearchHit {
    pub context_id: Uuid,
    pub team_id: Uuid,
    pub title: String,
    pub tags: Vec<String>,
    pub score: f32,
    pub snippet: String,
    pub updated_at: String,
}

#[derive(Debug, Deserialize)]
pub struct Context {
    pub id: Uuid,
    pub team_id: Uuid,
    pub title: String,
    pub content: String,
    pub tags: Vec<String>,
    pub updated_at: String,
}

#[derive(Debug, Deserialize)]
pub struct ContextSummary {
    pub id: Uuid,
    pub title: String,
    pub tags: Vec<String>,
    pub updated_at: String,
}

#[derive(Deserialize)]
struct ErrorBody {
    message: String,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
}

pub struct ApiClient {
    http: reqwest::Client,
    base_url: String,
    credentials: Credentials,
    token: RwLock<Option<SecretString>>,
}

impl ApiClient {
    pub fn new(base_url: &str, credentials: Credentials) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .user_agent(concat!("nanace-mcp/", env!("CARGO_PKG_VERSION")))
            .build()?;
        let token = match &credentials {
            Credentials::Token(token) => Some(token.clone()),
            Credentials::DevEmail(_) => None,
        };
        Ok(Self {
            http,
            base_url: base_url.trim_end_matches('/').to_owned(),
            credentials,
            token: RwLock::new(token),
        })
    }

    pub async fn me(&self) -> Result<Me, ApiError> {
        self.request(Method::GET, "/api/v1/me", None, &[]).await
    }

    pub async fn search(
        &self,
        org_id: Uuid,
        query: &str,
        limit: Option<u64>,
        team_ids: Option<&[Uuid]>,
    ) -> Result<Vec<SearchHit>, ApiError> {
        let mut body = serde_json::json!({ "query": query });
        if let Some(limit) = limit {
            body["limit"] = limit.into();
        }
        if let Some(team_ids) = team_ids {
            body["team_ids"] = serde_json::json!(team_ids);
        }
        self.request(Method::POST, &format!("/api/v1/orgs/{org_id}/search"), Some(body), &[])
            .await
    }

    pub async fn context(&self, org_id: Uuid, context_id: Uuid) -> Result<Context, ApiError> {
        self.request(
            Method::GET,
            &format!("/api/v1/orgs/{org_id}/contexts/{context_id}"),
            None,
            &[],
        )
        .await
    }

    pub async fn create_context(
        &self,
        org_id: Uuid,
        team_id: Uuid,
        title: &str,
        content: &str,
        tags: &[String],
    ) -> Result<Context, ApiError> {
        self.request(
            Method::POST,
            &format!("/api/v1/orgs/{org_id}/teams/{team_id}/contexts"),
            Some(serde_json::json!({ "title": title, "content": content, "tags": tags })),
            &[],
        )
        .await
    }

    /// Replaces a context's title, content and tags. `base_updated_at` is the
    /// version the edit was based on; a newer version yields `Conflict`.
    pub async fn update_context(
        &self,
        org_id: Uuid,
        context_id: Uuid,
        title: &str,
        content: &str,
        tags: &[String],
        base_updated_at: &str,
    ) -> Result<Context, ApiError> {
        self.request(
            Method::PUT,
            &format!("/api/v1/orgs/{org_id}/contexts/{context_id}"),
            Some(serde_json::json!({
                "title": title,
                "content": content,
                "tags": tags,
                "base_updated_at": base_updated_at,
            })),
            &[],
        )
        .await
    }

    pub async fn list_contexts(
        &self,
        org_id: Uuid,
        team_id: Uuid,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<ContextSummary>, ApiError> {
        self.request(
            Method::GET,
            &format!("/api/v1/orgs/{org_id}/teams/{team_id}/contexts"),
            None,
            &[("limit", limit.to_string()), ("offset", offset.to_string())],
        )
        .await
    }

    async fn request<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<serde_json::Value>,
        query: &[(&str, String)],
    ) -> Result<T, ApiError> {
        let mut relogged = false;
        loop {
            let token = self.current_token().await?;
            let mut req = self
                .http
                .request(method.clone(), format!("{}{path}", self.base_url))
                .bearer_auth(token.expose_secret())
                .query(query);
            if let Some(body) = &body {
                req = req.json(body);
            }
            let resp = req
                .send()
                .await
                .map_err(|e| ApiError::Unavailable(e.without_url().to_string()))?;
            let status = resp.status();
            if status == StatusCode::UNAUTHORIZED && !relogged && matches!(self.credentials, Credentials::DevEmail(_)) {
                // Token expired: drop it and log in once more.
                *self.token.write().await = None;
                relogged = true;
                continue;
            }
            if status.is_success() {
                return resp
                    .json()
                    .await
                    .map_err(|e| ApiError::Unexpected(e.without_url().to_string()));
            }
            let message = resp
                .json::<ErrorBody>()
                .await
                .map(|b| b.message)
                .unwrap_or_else(|_| status.to_string());
            return Err(match status {
                StatusCode::UNAUTHORIZED => ApiError::Unauthorized,
                StatusCode::FORBIDDEN => ApiError::Forbidden,
                StatusCode::NOT_FOUND => ApiError::NotFound,
                StatusCode::BAD_REQUEST => ApiError::BadRequest(message),
                StatusCode::CONFLICT => ApiError::Conflict(message),
                s if s.is_server_error() => ApiError::Unavailable(message),
                _ => ApiError::Unexpected(message),
            });
        }
    }

    async fn current_token(&self) -> Result<SecretString, ApiError> {
        if let Some(token) = self.token.read().await.as_ref() {
            return Ok(token.clone());
        }
        let Credentials::DevEmail(email) = &self.credentials else {
            return Err(ApiError::Unauthorized);
        };
        let resp = self
            .http
            .post(format!("{}/api/v1/auth/dev-login", self.base_url))
            .json(&serde_json::json!({ "email": email }))
            .send()
            .await
            .map_err(|e| ApiError::Unavailable(e.without_url().to_string()))?;
        if !resp.status().is_success() {
            // 404 means the API has dev login disabled.
            return Err(ApiError::Unauthorized);
        }
        let token = SecretString::from(
            resp.json::<TokenResponse>()
                .await
                .map_err(|e| ApiError::Unexpected(e.without_url().to_string()))?
                .access_token,
        );
        *self.token.write().await = Some(token.clone());
        Ok(token)
    }
}
