//! The daemon's view of the nanace API: who it is, who a peer is, and what
//! the team's policy says. The API is the only source of these facts; a
//! daemon never takes them from another peer.

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use nanace_core::TeamRole;
use nanace_core::p2p::{AcceptMode, ClientKind, RejectReason, bind_message};
use nanace_core::policy::P2pPolicy;
use reqwest::{Method, StatusCode};
use secrecy::{ExposeSecret, SecretString};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::envelope::Identity;

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("the nanace token is missing, invalid, expired or not allowed to do this")]
    Unauthorized,
    /// The API understood the request and said no; the text is its reason.
    #[error("{0}")]
    Refused(String),
    #[error("nanace API is unavailable: {0}")]
    Unavailable(String),
}

pub type RegistryResult<T> = Result<T, RegistryError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamMembership {
    pub org_id: Uuid,
    pub team_id: Uuid,
    pub team_slug: String,
    pub role: TeamRole,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Me {
    pub user_id: Uuid,
    pub email: String,
    pub teams: Vec<TeamMembership>,
}

impl Me {
    pub fn role_in(&self, team_id: Uuid) -> Option<TeamRole> {
        self.teams.iter().find(|t| t.team_id == team_id).map(|t| t.role)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SharedTeam {
    pub org_id: Uuid,
    pub team_id: Uuid,
    /// The peer owner's role in the team.
    pub role: TeamRole,
}

/// A peer the caller shares a team with, as the registry knows it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PeerInfo {
    pub user_id: Uuid,
    pub email: String,
    pub shared: Vec<SharedTeam>,
}

impl PeerInfo {
    pub fn role_in(&self, team_id: Uuid) -> Option<TeamRole> {
        self.shared.iter().find(|t| t.team_id == team_id).map(|t| t.role)
    }
}

/// A team's policy as served. `policy` is `None` when P2P is off because
/// there is no valid policy.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PolicyState {
    pub version: Option<String>,
    pub policy: Option<P2pPolicy>,
    #[serde(default)]
    pub errors: Vec<String>,
    #[serde(default)]
    pub text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresentPeer {
    pub peer_id: String,
    pub agent_label: String,
    pub user_id: Uuid,
    pub email: String,
    pub capabilities: Vec<String>,
    pub multiaddrs: Vec<String>,
    pub accept_mode: AcceptMode,
    pub client_kind: ClientKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Presence {
    pub peer_id: String,
    pub agent_label: String,
    pub capabilities: Vec<String>,
    pub multiaddrs: Vec<String>,
    pub accept_mode: AcceptMode,
    pub ttl_secs: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportRole {
    Sender,
    Receiver,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportStatus {
    Delivered,
    Rejected,
    Expired,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Consent {
    Auto,
    UserApproved,
    UserDenied,
}

/// One side's report of an exchange: signed headers only, never a body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExchangeReport {
    pub role: ReportRole,
    pub request_header: String,
    pub request_signature: String,
    pub status: ReportStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reject_reason: Option<RejectReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consent: Option<Consent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer_header: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer_signature: Option<String>,
}

#[async_trait]
pub trait Registry: Send + Sync {
    async fn me(&self) -> RegistryResult<Me>;
    /// Binds the daemon's peer id to the user, proving it holds the key.
    async fn bind(&self, identity: &Identity, user_id: Uuid, kind: ClientKind, version: &str) -> RegistryResult<()>;
    /// `None` when the peer is unknown, revoked, or shares no team with the
    /// caller where it is present and P2P is on.
    async fn lookup(&self, peer_id: &str) -> RegistryResult<Option<PeerInfo>>;
    async fn policy(&self, org_id: Uuid, team_id: Uuid, with_text: bool) -> RegistryResult<PolicyState>;
    async fn peers(&self, org_id: Uuid, team_id: Uuid, capability: Option<&str>) -> RegistryResult<Vec<PresentPeer>>;
    async fn announce(&self, org_id: Uuid, team_id: Uuid, presence: &Presence) -> RegistryResult<()>;
    async fn withdraw(&self, org_id: Uuid, team_id: Uuid, peer_id: &str, agent_label: &str) -> RegistryResult<()>;
    async fn report(&self, org_id: Uuid, team_id: Uuid, report: &ExchangeReport) -> RegistryResult<()>;
}

/// The registry over HTTPS, as the user whose token is in `token_file`. The
/// file is read on every request, so a rotated token is picked up at once.
pub struct HttpRegistry {
    http: reqwest::Client,
    base_url: String,
    token_file: PathBuf,
}

#[derive(Deserialize)]
struct ErrorBody {
    message: String,
}

#[derive(Deserialize)]
struct MeBody {
    user_id: Uuid,
    email: String,
    memberships: Vec<MembershipBody>,
}

#[derive(Deserialize)]
struct MembershipBody {
    org_id: Uuid,
    team_id: Option<Uuid>,
    team_slug: Option<String>,
    team_role: Option<TeamRole>,
}

#[derive(Deserialize)]
struct ChallengeBody {
    challenge: String,
}

#[derive(Deserialize)]
struct PeersBody {
    peers: Vec<PresentPeer>,
}

impl HttpRegistry {
    pub fn new(base_url: &str, token_file: PathBuf) -> anyhow::Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder().timeout(Duration::from_secs(15)).build()?,
            base_url: base_url.trim_end_matches('/').to_owned(),
            token_file,
        })
    }

    async fn token(&self) -> RegistryResult<SecretString> {
        let raw = tokio::fs::read_to_string(&self.token_file)
            .await
            .map_err(|_| RegistryError::Unauthorized)?;
        let token = raw.trim();
        if token.is_empty() {
            return Err(RegistryError::Unauthorized);
        }
        Ok(SecretString::from(token.to_owned()))
    }

    /// `Ok(None)` for `404`, which the API uses for everything the caller
    /// may not know about.
    async fn call<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> RegistryResult<Option<T>> {
        let token = self.token().await?;
        let mut request = self
            .http
            .request(method, format!("{}{path}", self.base_url))
            .bearer_auth(token.expose_secret());
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request
            .send()
            .await
            .map_err(|e| RegistryError::Unavailable(e.without_url().to_string()))?;
        let status = response.status();
        if status == StatusCode::NO_CONTENT {
            // Callers of bodiless endpoints ask for `serde_json::Value`.
            return Ok(serde_json::from_value(serde_json::Value::Null).ok());
        }
        if status.is_success() {
            return response
                .json()
                .await
                .map(Some)
                .map_err(|e| RegistryError::Unavailable(e.without_url().to_string()));
        }
        let message = response
            .json::<ErrorBody>()
            .await
            .map(|b| b.message)
            .unwrap_or_else(|_| status.to_string());
        Err(match status {
            StatusCode::NOT_FOUND => return Ok(None),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => RegistryError::Unauthorized,
            s if s.is_server_error() || s == StatusCode::TOO_MANY_REQUESTS => RegistryError::Unavailable(message),
            _ => RegistryError::Refused(message),
        })
    }

    async fn required<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> RegistryResult<T> {
        self.call(method, path, body)
            .await?
            .ok_or_else(|| RegistryError::Refused("not found, or not visible to this account".to_owned()))
    }

    /// For endpoints that answer without a body.
    async fn done(&self, method: Method, path: &str, body: Option<serde_json::Value>) -> RegistryResult<()> {
        self.call::<serde_json::Value>(method, path, body).await.map(|_| ())
    }
}

#[async_trait]
impl Registry for HttpRegistry {
    async fn me(&self) -> RegistryResult<Me> {
        let body: MeBody = self.required(Method::GET, "/api/v1/me", None).await?;
        Ok(Me {
            user_id: body.user_id,
            email: body.email,
            teams: body
                .memberships
                .into_iter()
                .filter_map(|m| {
                    Some(TeamMembership {
                        org_id: m.org_id,
                        team_id: m.team_id?,
                        team_slug: m.team_slug?,
                        role: m.team_role?,
                    })
                })
                .collect(),
        })
    }

    async fn bind(&self, identity: &Identity, user_id: Uuid, kind: ClientKind, version: &str) -> RegistryResult<()> {
        let challenge: ChallengeBody = self.required(Method::POST, "/api/v1/me/peers/challenge", None).await?;
        let signature = identity.sign(&bind_message(&challenge.challenge, &identity.peer_id, user_id));
        let body = serde_json::json!({
            "peer_id": identity.peer_id,
            "challenge": challenge.challenge,
            "signature": signature,
            "client_kind": kind,
            "client_version": version,
        });
        self.required::<serde_json::Value>(Method::POST, "/api/v1/me/peers", Some(body))
            .await
            .map(|_| ())
    }

    async fn lookup(&self, peer_id: &str) -> RegistryResult<Option<PeerInfo>> {
        self.call(Method::GET, &format!("/api/v1/me/peers/lookup/{peer_id}"), None)
            .await
    }

    async fn policy(&self, org_id: Uuid, team_id: Uuid, with_text: bool) -> RegistryResult<PolicyState> {
        let text = if with_text { "?text=true" } else { "" };
        self.required(
            Method::GET,
            &format!("/api/v1/orgs/{org_id}/teams/{team_id}/p2p/policy{text}"),
            None,
        )
        .await
    }

    async fn peers(&self, org_id: Uuid, team_id: Uuid, capability: Option<&str>) -> RegistryResult<Vec<PresentPeer>> {
        // Capabilities are validated labels, safe in a query string.
        let query = capability.map(|c| format!("?capability={c}")).unwrap_or_default();
        let body: PeersBody = self
            .required(
                Method::GET,
                &format!("/api/v1/orgs/{org_id}/teams/{team_id}/peers{query}"),
                None,
            )
            .await?;
        Ok(body.peers)
    }

    async fn announce(&self, org_id: Uuid, team_id: Uuid, presence: &Presence) -> RegistryResult<()> {
        let body = serde_json::to_value(presence).map_err(|e| RegistryError::Refused(e.to_string()))?;
        self.required::<serde_json::Value>(
            Method::PUT,
            &format!("/api/v1/orgs/{org_id}/teams/{team_id}/presence"),
            Some(body),
        )
        .await
        .map(|_| ())
    }

    async fn withdraw(&self, org_id: Uuid, team_id: Uuid, peer_id: &str, agent_label: &str) -> RegistryResult<()> {
        self.done(
            Method::DELETE,
            &format!("/api/v1/orgs/{org_id}/teams/{team_id}/presence/{peer_id}/{agent_label}"),
            None,
        )
        .await
    }

    async fn report(&self, org_id: Uuid, team_id: Uuid, report: &ExchangeReport) -> RegistryResult<()> {
        let body = serde_json::to_value(report).map_err(|e| RegistryError::Refused(e.to_string()))?;
        self.done(
            Method::POST,
            &format!("/api/v1/orgs/{org_id}/teams/{team_id}/p2p/exchanges"),
            Some(body),
        )
        .await
    }
}
