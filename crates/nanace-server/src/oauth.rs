//! Google sign-in: OpenID Connect authorization code flow with PKCE.
//!
//! The login state (CSRF `state`, `nonce`, PKCE verifier) travels in a
//! short-lived HMAC-signed cookie, so no server-side session store is needed.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, KeyInit, Mac};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::config::GoogleConfig;
use crate::secret::random_token;

pub const STATE_COOKIE: &str = "nanace_oauth";
pub const STATE_TTL: Duration = Duration::from_secs(600);

const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const ISSUERS: [&str; 2] = ["https://accounts.google.com", "accounts.google.com"];

/// Why a sign-in attempt was rejected. The code is shown to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LoginError {
    #[error("invalid_state")]
    InvalidState,
    #[error("access_denied")]
    AccessDenied,
    #[error("token_exchange_failed")]
    TokenExchange,
    #[error("invalid_id_token")]
    InvalidIdToken,
    #[error("email_not_verified")]
    EmailNotVerified,
    #[error("domain_not_allowed")]
    DomainNotAllowed,
    #[error("account_conflict")]
    AccountConflict,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LoginState {
    pub state: String,
    pub nonce: String,
    pub verifier: String,
    pub expires_at: u64,
}

/// Identity extracted from a validated Google ID token.
#[derive(Debug, PartialEq, Eq)]
pub struct GoogleIdentity {
    pub sub: String,
    pub email: String,
    pub name: Option<String>,
}

#[derive(Deserialize)]
struct IdTokenClaims {
    iss: String,
    aud: String,
    sub: String,
    exp: u64,
    nonce: Option<String>,
    email: Option<String>,
    email_verified: Option<bool>,
    hd: Option<String>,
    name: Option<String>,
}

pub struct GoogleOAuth {
    config: GoogleConfig,
    redirect_uri: String,
    state_key: Vec<u8>,
    http: reqwest::Client,
    token_url: String,
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

pub fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

impl GoogleOAuth {
    /// `signing_secret` is the server's JWT secret; the cookie key is derived
    /// from it so the two never share a raw key.
    pub fn new(config: GoogleConfig, public_url: &str, signing_secret: &SecretString) -> anyhow::Result<Self> {
        Self::with_token_url(config, public_url, signing_secret, TOKEN_URL.to_owned())
    }

    /// Like [`Self::new`] with a different token endpoint, for tests. The ID
    /// token signature is not checked (see [`Self::validate_id_token`]), so
    /// production code must only ever use Google's HTTPS endpoint.
    pub fn with_token_url(
        config: GoogleConfig,
        public_url: &str,
        signing_secret: &SecretString,
        token_url: String,
    ) -> anyhow::Result<Self> {
        let mut key = Sha256::new();
        key.update(b"nanace-oauth-state-v1\0");
        key.update(signing_secret.expose_secret().as_bytes());
        Ok(Self {
            config,
            redirect_uri: format!("{public_url}/api/v1/auth/google/callback"),
            state_key: key.finalize().to_vec(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .context("failed to build HTTP client")?,
            token_url,
        })
    }

    /// Starts a sign-in: returns Google's authorization URL and the signed
    /// cookie value that must accompany the callback.
    pub fn begin(&self) -> anyhow::Result<(String, String)> {
        let login = LoginState {
            state: random_token(32)?,
            nonce: random_token(32)?,
            verifier: random_token(48)?,
            expires_at: now_secs() + STATE_TTL.as_secs(),
        };
        let mut url = reqwest::Url::parse(AUTH_URL).context("invalid Google auth URL")?;
        url.query_pairs_mut()
            .append_pair("client_id", &self.config.client_id)
            .append_pair("redirect_uri", &self.redirect_uri)
            .append_pair("response_type", "code")
            .append_pair("scope", "openid email profile")
            .append_pair("state", &login.state)
            .append_pair("nonce", &login.nonce)
            .append_pair("code_challenge", &pkce_challenge(&login.verifier))
            .append_pair("code_challenge_method", "S256")
            .append_pair("prompt", "select_account");
        if let [domain] = self.config.allowed_domains.as_slice() {
            // UI hint only; the domain is enforced on the ID token.
            url.query_pairs_mut().append_pair("hd", domain);
        }
        Ok((url.into(), self.sign_state(&login)?))
    }

    fn mac(&self) -> Hmac<Sha256> {
        // HMAC accepts keys of any length.
        Hmac::<Sha256>::new_from_slice(&self.state_key).expect("HMAC accepts any key length")
    }

    fn sign_state(&self, login: &LoginState) -> anyhow::Result<String> {
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(login)?);
        let mut mac = self.mac();
        mac.update(payload.as_bytes());
        Ok(format!(
            "{payload}.{}",
            URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
        ))
    }

    /// Verifies the cookie signature and expiry and that the `state` query
    /// parameter matches. Any failure is `InvalidState`.
    pub fn verify_state(&self, cookie: &str, state_param: &str) -> Result<LoginState, LoginError> {
        let (payload, signature) = cookie.split_once('.').ok_or(LoginError::InvalidState)?;
        let signature = URL_SAFE_NO_PAD
            .decode(signature)
            .map_err(|_| LoginError::InvalidState)?;
        let mut mac = self.mac();
        mac.update(payload.as_bytes());
        mac.verify_slice(&signature).map_err(|_| LoginError::InvalidState)?;
        let login: LoginState = URL_SAFE_NO_PAD
            .decode(payload)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .ok_or(LoginError::InvalidState)?;
        let state_matches: bool = login.state.as_bytes().ct_eq(state_param.as_bytes()).into();
        if !state_matches || login.expires_at < now_secs() {
            return Err(LoginError::InvalidState);
        }
        Ok(login)
    }

    /// Exchanges the authorization code and validates the returned ID token.
    pub async fn complete(&self, code: &str, login: &LoginState) -> Result<GoogleIdentity, LoginError> {
        #[derive(Deserialize)]
        struct TokenResponse {
            id_token: String,
        }
        let resp = self
            .http
            .post(&self.token_url)
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("client_id", self.config.client_id.as_str()),
                ("client_secret", self.config.client_secret.expose_secret()),
                ("redirect_uri", self.redirect_uri.as_str()),
                ("code_verifier", login.verifier.as_str()),
            ])
            .send()
            .await
            .map_err(|_| LoginError::TokenExchange)?;
        if !resp.status().is_success() {
            return Err(LoginError::TokenExchange);
        }
        let token: TokenResponse = resp.json().await.map_err(|_| LoginError::TokenExchange)?;
        self.validate_id_token(&token.id_token, &login.nonce)
    }

    /// Validates ID token claims. The signature is not verified: the token
    /// came directly from Google's token endpoint over TLS, which OpenID
    /// Connect Core 1.0 §3.1.3.7 (step 6) accepts in place of a signature
    /// check. Never call this with a token received from a browser.
    pub fn validate_id_token(&self, id_token: &str, expected_nonce: &str) -> Result<GoogleIdentity, LoginError> {
        let mut parts = id_token.split('.');
        let (Some(_header), Some(payload), Some(_signature), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(LoginError::InvalidIdToken);
        };
        let claims: IdTokenClaims = URL_SAFE_NO_PAD
            .decode(payload)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .ok_or(LoginError::InvalidIdToken)?;

        let nonce_matches = claims
            .nonce
            .as_deref()
            .is_some_and(|n| bool::from(n.as_bytes().ct_eq(expected_nonce.as_bytes())));
        if !ISSUERS.contains(&claims.iss.as_str())
            || claims.aud != self.config.client_id
            || claims.exp + 60 < now_secs()
            || claims.sub.is_empty()
            || !nonce_matches
        {
            return Err(LoginError::InvalidIdToken);
        }
        let email = claims
            .email
            .as_deref()
            .and_then(|e| nanace_core::validate::email(e).ok())
            .ok_or(LoginError::InvalidIdToken)?;
        if claims.email_verified != Some(true) {
            return Err(LoginError::EmailNotVerified);
        }
        if !self.config.allowed_domains.is_empty() {
            // `hd` is set only for Workspace accounts; consumer accounts with
            // a matching email domain are not members of the Workspace.
            let hd = claims.hd.as_deref().map(str::to_ascii_lowercase);
            if !hd.is_some_and(|hd| self.config.allowed_domains.contains(&hd)) {
                return Err(LoginError::DomainNotAllowed);
            }
        }
        Ok(GoogleIdentity {
            sub: claims.sub,
            email,
            // Optional and user-controlled at Google: keep it only if valid.
            name: claims
                .name
                .and_then(|n| nanace_core::validate::display_name("name", &n).ok()),
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn oauth(domains: &[&str]) -> GoogleOAuth {
        GoogleOAuth::new(
            GoogleConfig {
                client_id: "client-123".to_owned(),
                client_secret: SecretString::from("secret".to_owned()),
                allowed_domains: domains.iter().map(|d| (*d).to_owned()).collect(),
            },
            "https://kb.example.com",
            &SecretString::from("jwt-secret-0123456789abcdef0123456789".to_owned()),
        )
        .unwrap()
    }

    pub fn id_token(claims: serde_json::Value) -> String {
        let enc = |v: serde_json::Value| URL_SAFE_NO_PAD.encode(serde_json::to_vec(&v).unwrap());
        format!("{}.{}.sig", enc(json!({ "alg": "RS256" })), enc(claims))
    }

    fn claims() -> serde_json::Value {
        json!({
            "iss": "https://accounts.google.com",
            "aud": "client-123",
            "sub": "1087",
            "exp": now_secs() + 300,
            "nonce": "n-1",
            "email": "Alice@Example.com",
            "email_verified": true,
            "hd": "example.com",
            "name": "Alice",
        })
    }

    #[test]
    fn pkce_challenge_is_s256() {
        // Independently computed with Python's hashlib + base64.
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mJ92K9qzjk1g1SqlTbdtt8Z4mBAoGM"),
            "UgDiIpN6DIhJZ_pwPjxhks8s8kaXj2JO6L08QiAKu68"
        );
    }

    #[test]
    fn begin_builds_url_and_verifiable_cookie() {
        let oauth = oauth(&["example.com"]);
        let (url, cookie) = oauth.begin().unwrap();
        let url = reqwest::Url::parse(&url).unwrap();
        let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(q["client_id"], "client-123");
        assert_eq!(q["redirect_uri"], "https://kb.example.com/api/v1/auth/google/callback");
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["hd"], "example.com");

        let login = oauth.verify_state(&cookie, &q["state"]).unwrap();
        assert_eq!(login.nonce, q["nonce"]);
        assert_eq!(pkce_challenge(&login.verifier), q["code_challenge"]);
    }

    #[test]
    fn state_rejects_tampering_mismatch_expiry_and_foreign_keys() {
        let oauth = oauth(&[]);
        let (url, cookie) = oauth.begin().unwrap();
        let state = reqwest::Url::parse(&url)
            .unwrap()
            .query_pairs()
            .find(|(k, _)| k == "state")
            .unwrap()
            .1
            .into_owned();

        assert_eq!(oauth.verify_state(&cookie, "other"), Err(LoginError::InvalidState));
        assert_eq!(oauth.verify_state("garbage", &state), Err(LoginError::InvalidState));
        let (payload, sig) = cookie.split_once('.').unwrap();
        let mut forged: LoginState = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).unwrap()).unwrap();
        forged.expires_at += 10_000;
        let forged = format!("{}.{sig}", URL_SAFE_NO_PAD.encode(serde_json::to_vec(&forged).unwrap()));
        assert_eq!(oauth.verify_state(&forged, &state), Err(LoginError::InvalidState));

        let other_server = GoogleOAuth::new(
            oauth.config.clone(),
            "https://kb.example.com",
            &SecretString::from("a-different-secret-0123456789abcdef".to_owned()),
        )
        .unwrap();
        assert_eq!(
            other_server.verify_state(&cookie, &state),
            Err(LoginError::InvalidState)
        );

        let expired = LoginState {
            state: "s".to_owned(),
            nonce: "n".to_owned(),
            verifier: "v".to_owned(),
            expires_at: now_secs() - 1,
        };
        assert_eq!(
            oauth.verify_state(&oauth.sign_state(&expired).unwrap(), "s"),
            Err(LoginError::InvalidState)
        );
    }

    #[test]
    fn id_token_happy_path_normalizes_email() {
        let identity = oauth(&["example.com"])
            .validate_id_token(&id_token(claims()), "n-1")
            .unwrap();
        assert_eq!(
            identity,
            GoogleIdentity {
                sub: "1087".to_owned(),
                email: "alice@example.com".to_owned(),
                name: Some("Alice".to_owned()),
            }
        );
    }

    #[test]
    fn id_token_rejections() {
        let oauth = oauth(&["example.com"]);
        let with = |key: &str, value: serde_json::Value| {
            let mut c = claims();
            c[key] = value;
            id_token(c)
        };
        let invalid = [
            with("iss", json!("https://evil.example.com")),
            with("aud", json!("someone-else")),
            with("exp", json!(now_secs() - 120)),
            with("nonce", json!("n-2")),
            with("nonce", json!(null)),
            with("sub", json!("")),
            with("email", json!("not-an-email")),
            with("email", json!(null)),
            "a.b".to_owned(),
            "a.!!!.c".to_owned(),
        ];
        for token in invalid {
            assert_eq!(
                oauth.validate_id_token(&token, "n-1"),
                Err(LoginError::InvalidIdToken),
                "{token}"
            );
        }
        assert_eq!(
            oauth.validate_id_token(&with("email_verified", json!(false)), "n-1"),
            Err(LoginError::EmailNotVerified)
        );
        assert_eq!(
            oauth.validate_id_token(&with("hd", json!("evil.com")), "n-1"),
            Err(LoginError::DomainNotAllowed)
        );
        // Consumer account (no hd) with a matching email domain.
        assert_eq!(
            oauth.validate_id_token(&with("hd", json!(null)), "n-1"),
            Err(LoginError::DomainNotAllowed)
        );
        let control_name = oauth
            .validate_id_token(&with("name", json!("a\u{0007}b")), "n-1")
            .unwrap();
        assert_eq!(control_name.name, None);
        // No domain restriction: consumer accounts are accepted.
        let open = self::oauth(&[]);
        assert!(open.validate_id_token(&with("hd", json!(null)), "n-1").is_ok());
    }
}
