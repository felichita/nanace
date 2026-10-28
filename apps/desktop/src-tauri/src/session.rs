//! Server URL and credentials.
//!
//! The desktop app signs in once, then keeps a write-scoped personal access
//! token in the OS keychain (one entry per server URL). The token never
//! reaches the webview: every API call goes through [`Session::request`].

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;

pub const DEFAULT_SERVER_URL: &str = "http://127.0.0.1:8080";
const KEYCHAIN_SERVICE: &str = "dev.nanace.desktop";
const TOKEN_NAME: &str = "nanace desktop";
const TOKEN_DAYS: u32 = 90;

#[derive(Serialize, Deserialize)]
struct Settings {
    server_url: String,
}

#[derive(Serialize, Clone)]
pub struct SessionInfo {
    pub server_url: String,
    pub signed_in: bool,
}

#[derive(Serialize)]
pub struct ApiResponse {
    pub status: u16,
    pub body: Value,
}

struct Inner {
    server_url: String,
    token: Option<String>,
}

pub struct Session {
    inner: Mutex<Inner>,
    http: reqwest::Client,
    settings_path: PathBuf,
}

/// Normalizes a user-entered server URL to `scheme://host[:port]`.
pub fn normalize_server_url(raw: &str) -> Result<String, String> {
    let url = url::Url::parse(raw.trim()).map_err(|_| "Enter a full URL, like http://127.0.0.1:8080".to_owned())?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err("The server URL must start with http:// or https://".to_owned());
    }
    if !url.username().is_empty() || url.password().is_some() || url.query().is_some() {
        return Err("The server URL must not contain credentials or a query".to_owned());
    }
    Ok(url.origin().ascii_serialization())
}

/// Builds the request URL for an API path from the webview, refusing
/// anything outside `<server>/api/v1/`.
fn api_url(server_url: &str, path: &str) -> Result<url::Url, String> {
    let well_formed = path.starts_with("/api/v1/")
        && !path.contains("..")
        && !path.contains("//")
        && !path.contains('#')
        && path.chars().all(|c| c.is_ascii_graphic());
    if !well_formed {
        return Err("invalid API path".to_owned());
    }
    let url = url::Url::parse(&format!("{server_url}{path}")).map_err(|_| "invalid API path".to_owned())?;
    let server = url::Url::parse(server_url).map_err(|_| "invalid server URL".to_owned())?;
    if url.origin() != server.origin() || !url.path().starts_with("/api/v1/") {
        return Err("invalid API path".to_owned());
    }
    Ok(url)
}

/// API paths the webview may call. Token management stays with this process:
/// rotating or listing tokens through the webview would hand a credential to
/// page JavaScript.
fn webview_may_call(path: &str) -> bool {
    !path.starts_with("/api/v1/me/tokens") && !path.starts_with("/api/v1/auth/")
}

fn keychain(server_url: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(KEYCHAIN_SERVICE, server_url).map_err(|err| format!("keychain unavailable: {err}"))
}

fn error_message(status: reqwest::StatusCode, body: &Value) -> String {
    match body.get("message").and_then(Value::as_str) {
        Some(msg) => format!("{msg} ({status})"),
        None => format!("server returned {status}"),
    }
}

impl Session {
    pub fn load(settings_path: PathBuf) -> Result<Self, String> {
        let server_url = std::fs::read(&settings_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Settings>(&bytes).ok())
            .and_then(|s| normalize_server_url(&s.server_url).ok())
            .unwrap_or_else(|| DEFAULT_SERVER_URL.to_owned());
        let token = keychain(&server_url).ok().and_then(|e| e.get_password().ok());
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            // The API never redirects; following one could leak the token.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|err| format!("failed to build HTTP client: {err}"))?;
        Ok(Self {
            inner: Mutex::new(Inner { server_url, token }),
            http,
            settings_path,
        })
    }

    pub async fn info(&self) -> SessionInfo {
        let inner = self.inner.lock().await;
        SessionInfo {
            server_url: inner.server_url.clone(),
            signed_in: inner.token.is_some(),
        }
    }

    pub async fn server_url(&self) -> String {
        self.inner.lock().await.server_url.clone()
    }

    pub async fn set_server_url(&self, raw: &str) -> Result<SessionInfo, String> {
        let server_url = normalize_server_url(raw)?;
        if let Some(dir) = self.settings_path.parent() {
            std::fs::create_dir_all(dir).map_err(|err| format!("failed to save settings: {err}"))?;
        }
        let settings = serde_json::to_vec_pretty(&Settings {
            server_url: server_url.clone(),
        })
        .map_err(|err| err.to_string())?;
        std::fs::write(&self.settings_path, settings).map_err(|err| format!("failed to save settings: {err}"))?;
        let token = keychain(&server_url).ok().and_then(|e| e.get_password().ok());
        let mut inner = self.inner.lock().await;
        inner.server_url = server_url;
        inner.token = token;
        Ok(SessionInfo {
            server_url: inner.server_url.clone(),
            signed_in: inner.token.is_some(),
        })
    }

    /// Calls the API with the stored token. A 401 means the token was revoked
    /// or expired, so it is forgotten and the UI returns to sign-in.
    pub async fn request(&self, method: &str, path: &str, body: Option<Value>) -> Result<ApiResponse, String> {
        let (server_url, token) = {
            let inner = self.inner.lock().await;
            (inner.server_url.clone(), inner.token.clone())
        };
        let token = token.ok_or_else(|| "not signed in".to_owned())?;
        if !webview_may_call(path) {
            return Err("this API path is not available to the app UI".to_owned());
        }
        let url = api_url(&server_url, path)?;
        let method = match method {
            "GET" => reqwest::Method::GET,
            "POST" => reqwest::Method::POST,
            "PUT" => reqwest::Method::PUT,
            "DELETE" => reqwest::Method::DELETE,
            _ => return Err("unsupported method".to_owned()),
        };
        let mut req = self.http.request(method, url).bearer_auth(&token);
        if let Some(body) = body {
            req = req.json(&body);
        }
        let resp = req.send().await.map_err(|_| format!("cannot reach {server_url}"))?;
        let status = resp.status();
        let bytes = resp
            .bytes()
            .await
            .map_err(|err| format!("failed to read response: {err}"))?;
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into()))
        };
        if status == reqwest::StatusCode::UNAUTHORIZED {
            self.forget_token(&server_url, &token).await;
        }
        Ok(ApiResponse {
            status: status.as_u16(),
            body,
        })
    }

    async fn forget_token(&self, server_url: &str, token: &str) {
        let mut inner = self.inner.lock().await;
        // Only if nothing changed meanwhile (e.g. a fresh sign-in).
        if inner.server_url == server_url && inner.token.as_deref() == Some(token) {
            inner.token = None;
            if let Ok(entry) = keychain(server_url) {
                let _ = entry.delete_credential();
            }
        }
    }

    /// Posts without the stored token (sign-in endpoints).
    async fn post_anonymous(
        &self,
        server_url: &str,
        path: &str,
        body: Value,
        bearer: Option<&str>,
    ) -> Result<Value, String> {
        let url = api_url(server_url, path)?;
        let mut req = self.http.post(url).json(&body);
        if let Some(bearer) = bearer {
            req = req.bearer_auth(bearer);
        }
        let resp = req.send().await.map_err(|_| format!("cannot reach {server_url}"))?;
        let status = resp.status();
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        if status.is_success() {
            Ok(body)
        } else if status == reqwest::StatusCode::NOT_FOUND && path.ends_with("/dev-login") {
            Err("Email sign-in is disabled on this server (NANACE_DEV_LOGIN).".to_owned())
        } else {
            Err(error_message(status, &body))
        }
    }

    /// Whether the server has Google sign-in configured.
    pub async fn google_enabled(&self, server_url: &str) -> Result<bool, String> {
        let url = api_url(server_url, "/api/v1/auth/google/login")?;
        let resp = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|_| format!("cannot reach {server_url}"))?;
        Ok(resp.status() != reqwest::StatusCode::NOT_FOUND)
    }

    pub async fn exchange_login_code(&self, server_url: &str, code: &str, verifier: &str) -> Result<String, String> {
        let body = self
            .post_anonymous(
                server_url,
                "/api/v1/auth/exchange",
                json!({ "code": code, "code_verifier": verifier }),
                None,
            )
            .await?;
        session_token(&body)
    }

    pub async fn dev_login(&self, server_url: &str, email: &str) -> Result<String, String> {
        let body = self
            .post_anonymous(server_url, "/api/v1/auth/dev-login", json!({ "email": email }), None)
            .await?;
        session_token(&body)
    }

    /// Trades a short-lived session for a personal access token and stores
    /// it. The session token is dropped afterwards.
    pub async fn finish_sign_in(&self, server_url: &str, session: &str) -> Result<SessionInfo, String> {
        let token = self
            .create_token(server_url, session, TOKEN_NAME, "write", TOKEN_DAYS)
            .await?;
        keychain(server_url)?
            .set_password(&token)
            .map_err(|err| format!("failed to save the token in the keychain: {err}"))?;
        let mut inner = self.inner.lock().await;
        if inner.server_url != server_url {
            return Err("the server changed during sign-in".to_owned());
        }
        inner.token = Some(token);
        Ok(SessionInfo {
            server_url: inner.server_url.clone(),
            signed_in: true,
        })
    }

    /// Creates a personal access token; needs an interactive session.
    pub async fn create_token(
        &self,
        server_url: &str,
        session: &str,
        name: &str,
        scope: &str,
        days: u32,
    ) -> Result<String, String> {
        let body = self
            .post_anonymous(
                server_url,
                "/api/v1/me/tokens",
                json!({ "name": name, "scope": scope, "expires_in_days": days }),
                Some(session),
            )
            .await?;
        body.get("token")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| "server did not return a token".to_owned())
    }

    /// Replaces `token` with a new one of the same name, scope and lifetime.
    /// `Ok(None)` means the token is no longer valid (expired, revoked, or
    /// rotated elsewhere) and the user has to connect again.
    pub async fn rotate_token(&self, server_url: &str, token: &str) -> Result<Option<String>, String> {
        let url = api_url(server_url, "/api/v1/me/tokens/current/rotate")?;
        let resp = self
            .http
            .post(url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|_| format!("cannot reach {server_url}"))?;
        let status = resp.status();
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::CONFLICT {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(error_message(status, &body));
        }
        body.get("token")
            .and_then(Value::as_str)
            .map(|t| Some(t.to_owned()))
            .ok_or_else(|| "server did not return a token".to_owned())
    }

    /// Revokes every active token named `name` (needs an interactive
    /// session). Connecting again this way also ends any copy of an earlier
    /// token, including ones that were rotated since.
    pub async fn revoke_tokens_named(&self, server_url: &str, session: &str, name: &str) -> Result<usize, String> {
        let url = api_url(server_url, "/api/v1/me/tokens")?;
        let resp = self
            .http
            .get(url)
            .bearer_auth(session)
            .send()
            .await
            .map_err(|_| format!("cannot reach {server_url}"))?;
        let status = resp.status();
        let list: Value = resp.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            return Err(error_message(status, &list));
        }
        let mut revoked = 0;
        for token in list.as_array().into_iter().flatten() {
            let active = token.get("revoked_at").is_some_and(Value::is_null);
            let named = token.get("name").and_then(Value::as_str) == Some(name);
            let Some(id) = token.get("id").and_then(Value::as_str) else {
                continue;
            };
            if active && named {
                let url = api_url(server_url, &format!("/api/v1/me/tokens/{id}"))?;
                if self
                    .http
                    .delete(url)
                    .bearer_auth(session)
                    .send()
                    .await
                    .is_ok_and(|r| r.status().is_success())
                {
                    revoked += 1;
                }
            }
        }
        Ok(revoked)
    }

    /// Revokes `token` with itself (best effort); used for tokens handed to
    /// other apps, such as Claude.
    pub async fn revoke_token(&self, server_url: &str, token: &str) {
        if let Ok(url) = api_url(server_url, "/api/v1/me/tokens/current") {
            let _ = self.http.delete(url).bearer_auth(token).send().await;
        }
    }

    /// Revokes the token on the server (best effort) and forgets it locally.
    pub async fn sign_out(&self) -> SessionInfo {
        let (server_url, token) = {
            let mut inner = self.inner.lock().await;
            (inner.server_url.clone(), inner.token.take())
        };
        if let Some(token) = token {
            self.revoke_token(&server_url, &token).await;
            if let Ok(entry) = keychain(&server_url) {
                let _ = entry.delete_credential();
            }
        }
        SessionInfo {
            server_url,
            signed_in: false,
        }
    }
}

fn session_token(body: &Value) -> Result<String, String> {
    body.get("access_token")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "server did not return a session".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_url_is_normalized_to_an_origin() {
        assert_eq!(
            normalize_server_url(" http://127.0.0.1:8080/ ").unwrap(),
            "http://127.0.0.1:8080"
        );
        assert_eq!(
            normalize_server_url("https://kb.example.com/app").unwrap(),
            "https://kb.example.com"
        );
        for bad in [
            "kb.example.com",
            "ftp://kb.example.com",
            "https://u:p@kb.example.com",
            "https://kb.example.com/?x=1",
        ] {
            assert!(normalize_server_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn webview_cannot_reach_token_or_sign_in_endpoints() {
        for blocked in [
            "/api/v1/me/tokens",
            "/api/v1/me/tokens/current/rotate",
            "/api/v1/me/tokens/current",
            "/api/v1/auth/exchange",
        ] {
            assert!(!webview_may_call(blocked), "{blocked}");
        }
        for allowed in ["/api/v1/me", "/api/v1/orgs/x/graph", "/api/v1/orgs/x/contexts/y"] {
            assert!(webview_may_call(allowed), "{allowed}");
        }
    }

    #[test]
    fn api_paths_stay_under_the_api_prefix() {
        let server = "http://127.0.0.1:8080";
        assert!(api_url(server, "/api/v1/me").is_ok());
        assert!(api_url(server, "/api/v1/orgs/x/graph?semantic=false&min_score=0.7").is_ok());
        for bad in [
            "/healthz",
            "api/v1/me",
            "/api/v1/../healthz",
            "/api/v1/%2e%2e/x",
            "//evil.test/api/v1/me",
            "/api/v1//evil.test",
            "/api/v1/me#x",
            "/api/v1/me x",
            "@evil.test/api/v1/me",
        ] {
            // `%2e%2e` is a dot segment to URL parsers; the check after
            // parsing catches it.
            assert!(api_url(server, bad).is_err(), "{bad}");
        }
    }
}
