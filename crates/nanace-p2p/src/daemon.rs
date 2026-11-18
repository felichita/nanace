//! The daemon's rules: what may be sent, what is accepted, who must agree,
//! and when a message is given up. Everything here runs before a person or a
//! model sees a message, and none of it trusts what a peer says about
//! itself: identity, team roles and the policy all come from the registry.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use nanace_core::TeamRole;
use nanace_core::p2p::{
    self, AcceptMode, AnswerHeader, AnswerStatus, ClientKind, MAX_REFS, MessageKind, Party, RejectReason,
    RequestHeader, sha256_hex,
};
use nanace_core::policy::P2pPolicy;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, broadcast};
use uuid::Uuid;

use crate::envelope::{Ack, HEADER_VERSION, Identity, OpenedRequest, Signed, Wire, open_answer, open_request};
use crate::registry::{
    Consent, ExchangeReport, Me, PeerInfo, Presence, PresentPeer, Registry, RegistryError, ReportRole, ReportStatus,
};
use crate::secrets::SecretScanner;
use crate::spool::{Spool, write_private};
use crate::transport::{Connection, Transport};

/// How far a sender's clock may be from ours.
const CLOCK_SKEW_SECS: i64 = 120;
/// How long a policy or a peer lookup is trusted before asking again. This
/// bounds how long a revoked peer or a changed policy keeps working.
const CACHE_SECS: i64 = 30;
/// Presence lifetime; announced again at a third of it.
const PRESENCE_TTL_SECS: u32 = 60;
/// An answer is still delivered this long after the request's deadline, so
/// the sender learns how it ended.
const ANSWER_GRACE_SECS: i64 = 300;
/// Finished exchanges stay readable this long before they are removed.
const KEEP_FINISHED_SECS: i64 = 24 * 3600;
const MAX_RETRY_DELAY_SECS: i64 = 60;
/// The longest lifetime a policy can give a request.
const MAX_REQUEST_TTL_SECS: i64 = 86_400;

/// Why the daemon would not do what a local agent or the desktop asked.
#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error("{0}")]
    Invalid(String),
    /// A rule refused it; `detail` is safe to show (never message text).
    #[error("refused ({}): {detail}", reason.as_str())]
    Refused { reason: RejectReason, detail: String },
    #[error("not found")]
    NotFound,
    #[error(transparent)]
    Registry(#[from] RegistryError),
    #[error("storage failed: {0}")]
    Storage(#[from] std::io::Error),
}

pub type DaemonResult<T> = Result<T, DaemonError>;

fn refused<T>(reason: RejectReason, detail: impl Into<String>) -> DaemonResult<T> {
    Err(DaemonError::Refused {
        reason,
        detail: detail.into(),
    })
}

/// A team the user takes part in, and how inbound requests are treated there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamSettings {
    pub org_id: Uuid,
    pub team_id: Uuid,
    pub accept_mode: AcceptMode,
    /// Users whose requests are accepted without asking in `trusted` mode,
    /// with the Unix time the trust ends (`i64::MAX` for no end).
    #[serde(default)]
    pub trusted: HashMap<Uuid, i64>,
}

/// What survives a restart besides the spool.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub teams: Vec<TeamSettings>,
    /// Hold an agent's answer until the user confirms it (desktop only).
    #[serde(default)]
    pub review_answers: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutState {
    /// Not yet taken by the receiver; delivery is retried.
    Queued,
    /// The receiver took it; an answer may follow.
    Delivered,
    Answered,
    Rejected,
    Expired,
    Failed,
}

impl OutState {
    fn finished(self) -> bool {
        !matches!(self, Self::Queued | Self::Delivered)
    }
}

/// A request this daemon sent.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Outgoing {
    request: Signed,
    header: RequestHeader,
    hash: String,
    state: OutState,
    reason: Option<RejectReason>,
    answer: Option<Signed>,
    /// Set when the answer broke a secret rule; its body was dropped.
    answer_blocked: Option<String>,
    /// Addresses the registry listed for the receiver when it was sent.
    addrs: Vec<String>,
    attempts: u32,
    next_try: i64,
    finished_at: Option<i64>,
    /// The server has not been told about the latest state yet.
    report_due: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InState {
    /// Waiting for the user's decision.
    PendingConsent,
    /// Accepted; waiting for the agent.
    Inbox,
    /// The agent answered; waiting for the user to confirm the answer.
    PendingReview,
    /// An answer is signed and being delivered.
    AnswerQueued,
    Done,
}

/// A request this daemon received.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Incoming {
    request: Signed,
    header: RequestHeader,
    hash: String,
    from_email: String,
    state: InState,
    consent: Option<Consent>,
    received_at: i64,
    /// The agent's answer while it waits for review.
    draft: Option<(String, Vec<Uuid>)>,
    answer: Option<Signed>,
    attempts: u32,
    next_try: i64,
    finished_at: Option<i64>,
    report_due: bool,
}

/// What an agent tells the daemon about its own work, for `status` requests.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentStatus {
    pub step: String,
    #[serde(default)]
    pub findings: String,
    #[serde(default)]
    pub busy: bool,
}

struct Agent {
    capabilities: Vec<String>,
    status: AgentStatus,
}

struct TeamPolicy {
    version: String,
    policy: P2pPolicy,
    scanner: SecretScanner,
}

struct State {
    me: Me,
    me_at: i64,
    settings: Settings,
    agents: HashMap<String, Agent>,
    out: HashMap<Uuid, Outgoing>,
    inc: HashMap<Uuid, Incoming>,
    /// Request ids already seen, with the time they may be forgotten.
    seen: HashMap<Uuid, i64>,
    /// Requests per sending peer and team in the current minute.
    rates: HashMap<(String, Uuid), (i64, u32)>,
    policies: HashMap<Uuid, (i64, Option<Arc<TeamPolicy>>)>,
    lookups: HashMap<String, (i64, Option<PeerInfo>)>,
    /// A desktop app is attached and can show consent prompts.
    supervisor: bool,
    last_heartbeat: i64,
    stopped: bool,
}

/// Something the desktop app should show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Notice {
    /// A request waits for the user's decision.
    ConsentRequest { request: InboxItem },
    /// An agent's answer waits for the user's confirmation.
    ReviewRequest { request: InboxItem, answer: String },
    /// Queues or connections changed.
    Activity,
}

/// What to send.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SendParams {
    pub org_id: Uuid,
    pub team_id: Uuid,
    pub to_peer: String,
    pub to_agent: String,
    pub kind: MessageKind,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub refs: Vec<Uuid>,
    /// Ties the messages of one task together; generated when absent.
    pub trace_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Sent {
    pub request_id: Uuid,
    pub trace_id: String,
}

/// A sent request and, once there is one, its answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReplyView {
    pub request_id: Uuid,
    pub trace_id: String,
    pub team_id: Uuid,
    pub kind: MessageKind,
    pub state: OutState,
    pub reason: Option<RejectReason>,
    pub to_peer: String,
    pub to_agent: String,
    /// The answer text. `None` until answered, or when it was blocked.
    pub answer: Option<String>,
    pub answer_refs: Vec<Uuid>,
    /// Name of the secret rule the answer broke, if it was dropped.
    pub answer_blocked: Option<String>,
    /// The team policy asks for a summary page after this kind of exchange.
    pub summary_required: bool,
    pub summary_title_prefix: Option<String>,
}

/// A received request, as shown to the agent or in a consent prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InboxItem {
    pub request_id: Uuid,
    pub trace_id: String,
    pub org_id: Uuid,
    pub team_id: Uuid,
    pub kind: MessageKind,
    pub state: InState,
    pub from_peer: String,
    pub from_agent: String,
    pub from_user: Uuid,
    /// From the registry, never from the message.
    pub from_email: String,
    pub to_agent: String,
    pub body: String,
    pub refs: Vec<Uuid>,
    pub deadline: i64,
    pub received_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WhoAmI {
    pub peer_id: String,
    pub user_id: Uuid,
    pub email: String,
    pub client_kind: ClientKind,
    pub agents: Vec<String>,
    pub teams: Vec<TeamView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TeamView {
    pub org_id: Uuid,
    pub team_id: Uuid,
    pub team_slug: Option<String>,
    pub role: Option<TeamRole>,
    /// What the user chose.
    pub accept_mode: AcceptMode,
    /// What applies: the choice capped by the policy, `off` without it or
    /// without the desktop app.
    pub effective_accept_mode: AcceptMode,
    pub p2p_enabled: bool,
    pub policy_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Activity {
    pub connections: Vec<Connection>,
    pub outgoing: Vec<ReplyView>,
    pub incoming: Vec<InboxItem>,
}

pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

pub struct Daemon {
    identity: Identity,
    kind: ClientKind,
    registry: Arc<dyn Registry>,
    transport: Arc<dyn Transport>,
    spool: Spool,
    settings_file: PathBuf,
    clock: Clock,
    state: Mutex<State>,
    notices: broadcast::Sender<Notice>,
}

fn item(record: &Incoming) -> InboxItem {
    InboxItem {
        request_id: record.header.request_id,
        trace_id: record.header.trace_id.clone(),
        org_id: record.header.org_id,
        team_id: record.header.team_id,
        kind: record.header.kind,
        state: record.state,
        from_peer: record.header.from.peer_id.clone(),
        from_agent: record.header.from.agent_label.clone(),
        from_user: record.header.from.user_id,
        from_email: record.from_email.clone(),
        to_agent: record.header.to.agent_label.clone(),
        body: record.request.body.clone(),
        refs: record.header.refs.clone(),
        deadline: record.header.deadline,
        received_at: record.received_at,
    }
}

fn retry_delay(attempts: u32) -> i64 {
    (1_i64 << attempts.min(6)).min(MAX_RETRY_DELAY_SECS)
}

impl Daemon {
    /// Starts a daemon in `dir`: identifies the user, binds the peer id,
    /// and reloads settings and unfinished messages.
    pub async fn start(
        dir: &Path,
        identity: Identity,
        kind: ClientKind,
        registry: Arc<dyn Registry>,
        transport: Arc<dyn Transport>,
        clock: Clock,
    ) -> DaemonResult<Arc<Self>> {
        let me = registry.me().await?;
        registry
            .bind(&identity, me.user_id, kind, env!("CARGO_PKG_VERSION"))
            .await?;
        let spool = Spool::open(&dir.join("spool"))?;
        let settings_file = dir.join("settings.json");
        let settings = match std::fs::read(&settings_file) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| DaemonError::Invalid(format!("{} is not readable: {e}", settings_file.display())))?,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Settings::default(),
            Err(err) => return Err(err.into()),
        };
        let (out, skipped_out) = spool.load::<Outgoing>("out")?;
        let (inc, skipped_in) = spool.load::<Incoming>("in")?;
        for file in skipped_out.iter().chain(&skipped_in) {
            tracing::warn!(event = "spool_record_skipped", component = "nanace_p2p", file = %file, "unreadable spool record skipped");
        }
        let now = clock();
        let seen = out
            .iter()
            .map(|o| (o.header.request_id, o.header.deadline))
            .chain(inc.iter().map(|i| (i.header.request_id, i.header.deadline)))
            .collect();
        let (notices, _) = broadcast::channel(64);
        Ok(Arc::new(Self {
            identity,
            kind,
            registry,
            transport,
            spool,
            settings_file,
            clock,
            state: Mutex::new(State {
                me,
                me_at: now,
                settings,
                agents: HashMap::new(),
                out: out.into_iter().map(|o| (o.header.request_id, o)).collect(),
                inc: inc.into_iter().map(|i| (i.header.request_id, i)).collect(),
                seen,
                rates: HashMap::new(),
                policies: HashMap::new(),
                lookups: HashMap::new(),
                supervisor: false,
                last_heartbeat: 0,
                stopped: false,
            }),
            notices,
        }))
    }

    pub fn peer_id(&self) -> &str {
        &self.identity.peer_id
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Notice> {
        self.notices.subscribe()
    }

    fn notify(&self, notice: Notice) {
        // Nobody listening is fine: the state is still there to be read.
        let _ = self.notices.send(notice);
    }

    fn now(&self) -> i64 {
        (self.clock)()
    }

    fn save_out(&self, record: &Outgoing) {
        if let Err(err) = self.spool.save("out", record.header.request_id, record) {
            tracing::error!(event = "spool_write_failed", component = "nanace_p2p", error = %err, "could not store an outgoing message");
        }
    }

    fn save_in(&self, record: &Incoming) {
        if let Err(err) = self.spool.save("in", record.header.request_id, record) {
            tracing::error!(event = "spool_write_failed", component = "nanace_p2p", error = %err, "could not store an incoming message");
        }
    }

    fn save_settings(&self, settings: &Settings) -> DaemonResult<()> {
        let bytes = serde_json::to_vec_pretty(settings).map_err(std::io::Error::other)?;
        Ok(write_private(&self.settings_file, &bytes)?)
    }

    /// The team's policy when P2P is on there. Cached briefly; a registry
    /// failure is an error, not "off", so nothing is refused for good by a
    /// network blip.
    async fn policy(&self, org_id: Uuid, team_id: Uuid) -> Result<Option<Arc<TeamPolicy>>, RegistryError> {
        let now = self.now();
        if let Some((at, cached)) = self.state.lock().await.policies.get(&team_id)
            && now - at < CACHE_SECS
        {
            return Ok(cached.clone());
        }
        let served = self.registry.policy(org_id, team_id, false).await?;
        let policy = match (served.version, served.policy) {
            (Some(version), Some(policy)) if policy.enabled => {
                match SecretScanner::new(&policy.data.deny_outbound_patterns) {
                    Ok(scanner) => Some(Arc::new(TeamPolicy {
                        version,
                        policy,
                        scanner,
                    })),
                    // A pattern this build cannot compile: fail closed.
                    Err(_) => None,
                }
            }
            _ => None,
        };
        self.state.lock().await.policies.insert(team_id, (now, policy.clone()));
        Ok(policy)
    }

    /// Who a peer is according to the registry, cached briefly.
    async fn lookup(&self, peer_id: &str) -> Result<Option<PeerInfo>, RegistryError> {
        let now = self.now();
        if let Some((at, cached)) = self.state.lock().await.lookups.get(peer_id)
            && now - at < CACHE_SECS
        {
            return Ok(cached.clone());
        }
        let info = self.registry.lookup(peer_id).await?;
        self.state
            .lock()
            .await
            .lookups
            .insert(peer_id.to_owned(), (now, info.clone()));
        Ok(info)
    }

    /// The accept mode that applies in a team right now.
    fn effective_mode(&self, state: &State, team_id: Uuid, policy: Option<&P2pPolicy>) -> AcceptMode {
        let chosen = state
            .settings
            .teams
            .iter()
            .find(|t| t.team_id == team_id)
            .map_or(AcceptMode::Off, |t| t.accept_mode);
        // On a laptop nothing is accepted without the desktop app.
        if state.stopped || (self.kind == ClientKind::Desktop && !state.supervisor) {
            return AcceptMode::Off;
        }
        policy.map_or(AcceptMode::Off, |p| p.effective_accept_mode(chosen))
    }

    // ----- agents ---------------------------------------------------------

    pub async fn register_agent(&self, label: &str, capabilities: &[String]) -> DaemonResult<()> {
        p2p::agent_label(label).map_err(|e| DaemonError::Invalid(e.to_string()))?;
        let capabilities = p2p::capabilities(capabilities).map_err(|e| DaemonError::Invalid(e.to_string()))?;
        let mut state = self.state.lock().await;
        state.agents.entry(label.to_owned()).or_insert(Agent {
            capabilities: Vec::new(),
            status: AgentStatus::default(),
        });
        if let Some(agent) = state.agents.get_mut(label) {
            agent.capabilities = capabilities;
        }
        // Announce it on the next tick rather than waiting for a heartbeat.
        state.last_heartbeat = 0;
        Ok(())
    }

    /// Called when an agent's session ends; its presence is withdrawn.
    pub async fn unregister_agent(&self, label: &str) {
        let teams = {
            let mut state = self.state.lock().await;
            if state.agents.remove(label).is_none() {
                return;
            }
            state.settings.teams.clone()
        };
        for team in teams {
            let _ = self
                .registry
                .withdraw(team.org_id, team.team_id, &self.identity.peer_id, label)
                .await;
        }
    }

    pub async fn set_status(&self, label: &str, status: AgentStatus) -> DaemonResult<()> {
        if status.step.len() > 500 || status.findings.len() > 4000 {
            return Err(DaemonError::Invalid(
                "status: step is at most 500 bytes and findings 4000".to_owned(),
            ));
        }
        let mut state = self.state.lock().await;
        let agent = state.agents.get_mut(label).ok_or(DaemonError::NotFound)?;
        agent.status = status;
        Ok(())
    }

    // ----- sending --------------------------------------------------------

    /// Checks a message against the team policy and the secret rules, signs
    /// it and queues it for delivery. Nothing leaves the machine unless every
    /// check passes.
    pub async fn send(self: &Arc<Self>, agent: &str, params: SendParams) -> DaemonResult<Sent> {
        let trace_id = params
            .trace_id
            .unwrap_or_else(|| format!("t-{}", &Uuid::new_v4().simple().to_string()[..12]));
        p2p::trace_id(&trace_id).map_err(|e| DaemonError::Invalid(e.to_string()))?;
        p2p::agent_label(&params.to_agent).map_err(|e| DaemonError::Invalid(e.to_string()))?;
        if params.refs.len() > MAX_REFS {
            return Err(DaemonError::Invalid(format!("refs: at most {MAX_REFS}")));
        }
        if params.kind != MessageKind::Status && params.body.trim().is_empty() {
            return Err(DaemonError::Invalid("body must not be empty".to_owned()));
        }
        if params.to_peer == self.identity.peer_id {
            return Err(DaemonError::Invalid("a peer does not message itself".to_owned()));
        }
        let (me, hop) = {
            let state = self.state.lock().await;
            if state.stopped {
                return refused(RejectReason::Consent, "P2P is stopped on this machine");
            }
            if !state.agents.contains_key(agent) {
                return Err(DaemonError::Invalid(format!("agent `{agent}` is not registered")));
            }
            if !state.settings.teams.iter().any(|t| t.team_id == params.team_id) {
                return refused(
                    RejectReason::Policy,
                    "this machine does not take part in P2P in that team",
                );
            }
            // A message sent while handling a request continues its chain.
            let hop = state
                .inc
                .values()
                .filter(|i| i.header.to.agent_label == agent && i.header.trace_id == trace_id)
                .map(|i| i.header.hop + 1)
                .max()
                .unwrap_or(0);
            (state.me.clone(), hop)
        };
        let Some(team) = self.policy(params.org_id, params.team_id).await? else {
            return refused(RejectReason::Policy, "P2P is not enabled in that team");
        };
        let limits = &team.policy.limits;
        if params.body.len() > limits.max_message_bytes as usize {
            return refused(
                RejectReason::Size,
                format!("the team allows messages of at most {} bytes", limits.max_message_bytes),
            );
        }
        if let Some(rule) = team.scanner.find(&params.body) {
            return refused(
                RejectReason::Pattern,
                format!("the text looks like it contains a secret (rule `{rule}`); remove it and send again"),
            );
        }
        if hop >= limits.max_hops {
            return refused(
                RejectReason::Hops,
                format!(
                    "this conversation already passed through {hop} agents; the team allows {}",
                    limits.max_hops
                ),
            );
        }
        let Some(peer) = self.lookup(&params.to_peer).await? else {
            return Err(DaemonError::NotFound);
        };
        let (Some(my_role), Some(their_role)) = (me.role_in(params.team_id), peer.role_in(params.team_id)) else {
            return refused(RejectReason::Policy, "the peer is not present in that team");
        };
        if !team.policy.allows_call(my_role, their_role, params.kind) {
            return refused(
                RejectReason::Policy,
                format!(
                    "the team policy does not let a {my_role} send `{}` to a {their_role}",
                    params.kind.as_str()
                ),
            );
        }
        // Direct addresses, if the peer shares any; the relay works without.
        let addrs = self
            .registry
            .peers(params.org_id, params.team_id, None)
            .await
            .unwrap_or_default()
            .into_iter()
            .find(|p| p.peer_id == params.to_peer)
            .map(|p| p.multiaddrs)
            .unwrap_or_default();

        let now = self.now();
        let header = RequestHeader {
            v: HEADER_VERSION,
            request_id: Uuid::new_v4(),
            trace_id: trace_id.clone(),
            org_id: params.org_id,
            team_id: params.team_id,
            from: Party {
                peer_id: self.identity.peer_id.clone(),
                agent_label: agent.to_owned(),
                user_id: me.user_id,
            },
            to: Party {
                peer_id: params.to_peer,
                agent_label: params.to_agent,
                user_id: peer.user_id,
            },
            kind: params.kind,
            body_hash: sha256_hex(params.body.as_bytes()),
            body_bytes: u32::try_from(params.body.len()).map_err(|_| DaemonError::Invalid("body".to_owned()))?,
            refs: params.refs,
            hop,
            sent_at: now,
            deadline: now + i64::from(limits.request_ttl_secs),
            policy_version: team.version.clone(),
        };
        let request = self.identity.sign_request(&header, params.body);
        let hash = open_request(&request).map(|o| o.hash).unwrap_or_default();
        let record = Outgoing {
            request,
            hash,
            state: OutState::Queued,
            reason: None,
            answer: None,
            answer_blocked: None,
            addrs,
            attempts: 0,
            next_try: now,
            finished_at: None,
            report_due: false,
            header,
        };
        let request_id = record.header.request_id;
        self.save_out(&record);
        {
            let mut state = self.state.lock().await;
            state.seen.insert(request_id, record.header.deadline);
            state.out.insert(request_id, record);
        }
        let daemon = Arc::clone(self);
        tokio::spawn(async move { daemon.deliver_request(request_id).await });
        self.notify(Notice::Activity);
        Ok(Sent { request_id, trace_id })
    }

    /// One delivery attempt of a queued request.
    async fn deliver_request(&self, request_id: Uuid) {
        let Some((request, peer, addrs)) = ({
            let state = self.state.lock().await;
            state
                .out
                .get(&request_id)
                .filter(|o| o.state == OutState::Queued)
                .map(|o| (o.request.clone(), o.header.to.peer_id.clone(), o.addrs.clone()))
        }) else {
            return;
        };
        let outcome = self.transport.send(&peer, &addrs, Wire::Request(request)).await;
        let now = self.now();
        let mut state = self.state.lock().await;
        let Some(record) = state.out.get_mut(&request_id).filter(|o| o.state == OutState::Queued) else {
            return;
        };
        match outcome {
            Ok(Ack::Accepted) => {
                record.state = OutState::Delivered;
                record.report_due = true;
            }
            Ok(Ack::Rejected(reason)) => {
                record.state = OutState::Rejected;
                record.reason = Some(reason);
                record.finished_at = Some(now);
                record.report_due = true;
            }
            Err(_) => {
                record.attempts += 1;
                record.next_try = now + retry_delay(record.attempts);
            }
        }
        let record = record.clone();
        drop(state);
        self.save_out(&record);
        self.notify(Notice::Activity);
    }

    /// A sent request and its answer, if any.
    pub async fn reply_of(&self, request_id: Uuid) -> DaemonResult<ReplyView> {
        let record = {
            let state = self.state.lock().await;
            state.out.get(&request_id).cloned().ok_or(DaemonError::NotFound)?
        };
        Ok(self.reply_view(&record).await)
    }

    async fn reply_view(&self, record: &Outgoing) -> ReplyView {
        let answer = record.answer.as_ref().and_then(|signed| {
            let opened = OpenedRequest {
                header: record.header.clone(),
                hash: record.hash.clone(),
            };
            open_answer(signed, &opened).map(|header| (signed.body.clone(), header.refs))
        });
        // Best effort: a missing policy just means no reminder.
        let summaries = self
            .policy(record.header.org_id, record.header.team_id)
            .await
            .ok()
            .flatten()
            .map(|t| t.policy.summaries.clone());
        let answered = record.state == OutState::Answered && record.answer_blocked.is_none();
        ReplyView {
            request_id: record.header.request_id,
            trace_id: record.header.trace_id.clone(),
            team_id: record.header.team_id,
            kind: record.header.kind,
            state: record.state,
            reason: record.reason,
            to_peer: record.header.to.peer_id.clone(),
            to_agent: record.header.to.agent_label.clone(),
            answer: answer.as_ref().filter(|_| answered).map(|(body, _)| body.clone()),
            answer_refs: answer.filter(|_| answered).map(|(_, refs)| refs).unwrap_or_default(),
            answer_blocked: record.answer_blocked.clone(),
            summary_required: answered
                && summaries
                    .as_ref()
                    .is_some_and(|s| s.required_for_kinds.contains(&record.header.kind)),
            summary_title_prefix: summaries.map(|s| s.title_prefix),
        }
    }

    // ----- receiving ------------------------------------------------------

    /// A message from the network. `from_peer` is the peer the connection
    /// authenticated, which the message must agree with.
    pub async fn handle_wire(self: &Arc<Self>, from_peer: &str, wire: Wire) -> Ack {
        match wire {
            Wire::Request(signed) => match self.receive_request(from_peer, signed).await {
                Ok(()) => Ack::Accepted,
                Err(reason) => Ack::Rejected(reason),
            },
            Wire::Answer(signed) => match self.receive_answer(from_peer, signed).await {
                Ok(()) => Ack::Accepted,
                Err(reason) => Ack::Rejected(reason),
            },
        }
    }

    async fn receive_request(self: &Arc<Self>, from_peer: &str, signed: Signed) -> Result<(), RejectReason> {
        // 1. A request signed by the peer on the other end, addressed to us.
        let opened = open_request(&signed).ok_or(RejectReason::Invalid)?;
        let header = &opened.header;
        if header.from.peer_id != from_peer
            || header.to.peer_id != self.identity.peer_id
            || p2p::agent_label(&header.from.agent_label).is_err()
            || p2p::agent_label(&header.to.agent_label).is_err()
            || p2p::trace_id(&header.trace_id).is_err()
            || header.refs.len() > MAX_REFS
        {
            return Err(RejectReason::Invalid);
        }
        // 2. Alive, and not seen before.
        let now = self.now();
        // A request may wait in the sender's queue for as long as it lives,
        // so its age is not a reason to refuse it. Its deadline is, and so
        // is a lifetime longer than any policy allows: request ids are
        // remembered until the deadline, which must therefore be bounded.
        if header.sent_at - now > CLOCK_SKEW_SECS
            || header.deadline <= now
            || header.deadline - header.sent_at > MAX_REQUEST_TTL_SECS
        {
            return Err(RejectReason::Replay);
        }
        // 3. The registry agrees about who the sender is and that we share
        //    this team. A registry failure refuses for now; the sender retries.
        let sender = self
            .lookup(from_peer)
            .await
            .ok()
            .flatten()
            .filter(|info| info.user_id == header.from.user_id)
            .ok_or(RejectReason::Invalid)?;
        let sender_role = sender.role_in(header.team_id).ok_or(RejectReason::Policy)?;
        // 4. Our own, current policy for the team.
        let team = self
            .policy(header.org_id, header.team_id)
            .await
            .ok()
            .flatten()
            .ok_or(RejectReason::Policy)?;
        let limits = &team.policy.limits;

        let mut state = self.state.lock().await;
        if state.me.user_id != header.to.user_id {
            return Err(RejectReason::Invalid);
        }
        if state.seen.contains_key(&header.request_id) {
            return Err(RejectReason::Replay);
        }
        let my_role = state.me.role_in(header.team_id).ok_or(RejectReason::Policy)?;
        if !team.policy.allows_call(sender_role, my_role, header.kind) {
            return Err(RejectReason::Policy);
        }
        if signed.body.len() > limits.max_message_bytes as usize {
            return Err(RejectReason::Size);
        }
        if header.hop >= limits.max_hops {
            return Err(RejectReason::Hops);
        }
        let rate = state
            .rates
            .entry((from_peer.to_owned(), header.team_id))
            .or_insert((now, 0));
        if now - rate.0 >= 60 {
            *rate = (now, 0);
        }
        rate.1 += 1;
        if rate.1 > limits.rate_per_peer_per_min {
            return Err(RejectReason::Rate);
        }
        // 5. No secrets reach a person or a model.
        if team.scanner.find(&signed.body).is_some() {
            return Err(RejectReason::Pattern);
        }
        // 6. Someone to give it to, and their consent.
        if !state.agents.contains_key(&header.to.agent_label) {
            return Err(RejectReason::NoAgent);
        }
        let trusted = state
            .settings
            .teams
            .iter()
            .find(|t| t.team_id == header.team_id)
            .and_then(|t| t.trusted.get(&header.from.user_id))
            .is_some_and(|until| *until > now);
        let (in_state, consent) = match self.effective_mode(&state, header.team_id, Some(&team.policy)) {
            AcceptMode::Off => return Err(RejectReason::Consent),
            AcceptMode::Team => (InState::Inbox, Some(Consent::Auto)),
            AcceptMode::Trusted if trusted => (InState::Inbox, Some(Consent::Auto)),
            AcceptMode::Ask | AcceptMode::Trusted => (InState::PendingConsent, None),
        };
        let record = Incoming {
            hash: opened.hash,
            from_email: sender.email,
            state: in_state,
            consent,
            received_at: now,
            draft: None,
            answer: None,
            attempts: 0,
            next_try: now,
            finished_at: None,
            report_due: true,
            header: opened.header,
            request: signed,
        };
        let request_id = record.header.request_id;
        state.seen.insert(request_id, record.header.deadline);
        state.inc.insert(request_id, record.clone());
        drop(state);
        self.save_in(&record);
        match in_state {
            InState::PendingConsent => self.notify(Notice::ConsentRequest { request: item(&record) }),
            _ => self.accepted(request_id).await,
        }
        self.notify(Notice::Activity);
        Ok(())
    }

    /// What happens once a request is accepted: a status request is answered
    /// from the agent's own record without waking a model, and a
    /// notification needs no answer.
    async fn accepted(self: &Arc<Self>, request_id: Uuid) {
        let status = {
            let state = self.state.lock().await;
            let Some(record) = state.inc.get(&request_id) else {
                return;
            };
            if record.header.kind != MessageKind::Status {
                return;
            }
            state
                .agents
                .get(&record.header.to.agent_label)
                .map(|a| a.status.clone())
                .unwrap_or_default()
        };
        let body = serde_json::to_string(&status).unwrap_or_default();
        self.finish(request_id, AnswerStatus::Answered, None, body, Vec::new())
            .await;
    }

    /// The user's decision on a request that waited for consent. Desktop only.
    pub async fn decide_consent(
        self: &Arc<Self>,
        request_id: Uuid,
        allow: bool,
        trust_hours: Option<u32>,
    ) -> DaemonResult<()> {
        let now = self.now();
        let settings = {
            let mut state = self.state.lock().await;
            let record = state
                .inc
                .get_mut(&request_id)
                .filter(|r| r.state == InState::PendingConsent)
                .ok_or(DaemonError::NotFound)?;
            record.consent = Some(if allow {
                Consent::UserApproved
            } else {
                Consent::UserDenied
            });
            record.report_due = true;
            if allow {
                record.state = InState::Inbox;
            }
            let (team_id, user) = (record.header.team_id, record.header.from.user_id);
            let record = record.clone();
            self.save_in(&record);
            match trust_hours.filter(|_| allow) {
                Some(hours) => {
                    if let Some(team) = state.settings.teams.iter_mut().find(|t| t.team_id == team_id) {
                        team.trusted.insert(user, now + i64::from(hours) * 3600);
                    }
                    Some(state.settings.clone())
                }
                None => None,
            }
        };
        if let Some(settings) = settings {
            self.save_settings(&settings)?;
        }
        if allow {
            self.accepted(request_id).await;
        } else {
            self.finish(
                request_id,
                AnswerStatus::Rejected,
                Some(RejectReason::Consent),
                String::new(),
                Vec::new(),
            )
            .await;
        }
        self.notify(Notice::Activity);
        Ok(())
    }

    /// Requests the agent may read now. Notifications are handed over once.
    pub async fn inbox(&self, agent: &str) -> Vec<InboxItem> {
        let mut state = self.state.lock().await;
        let now = self.now();
        let mut items: Vec<InboxItem> = state
            .inc
            .values()
            .filter(|r| r.state == InState::Inbox && r.header.to.agent_label == agent)
            .map(item)
            .collect();
        items.sort_by_key(|i| i.received_at);
        for record in state.inc.values_mut() {
            if record.state == InState::Inbox
                && record.header.to.agent_label == agent
                && record.header.kind == MessageKind::Notify
            {
                record.state = InState::Done;
                record.finished_at = Some(now);
                self.save_in(record);
            }
        }
        items
    }

    /// The agent's answer to a question in its inbox. Checked like any
    /// outgoing text; on a laptop it may first wait for the user.
    pub async fn reply(
        self: &Arc<Self>,
        agent: &str,
        request_id: Uuid,
        body: String,
        refs: Vec<Uuid>,
    ) -> DaemonResult<()> {
        if body.trim().is_empty() {
            return Err(DaemonError::Invalid("body must not be empty".to_owned()));
        }
        if refs.len() > MAX_REFS {
            return Err(DaemonError::Invalid(format!("refs: at most {MAX_REFS}")));
        }
        let (org_id, team_id) = {
            let state = self.state.lock().await;
            let record = state
                .inc
                .get(&request_id)
                .filter(|r| {
                    r.state == InState::Inbox && r.header.to.agent_label == agent && r.header.kind == MessageKind::Ask
                })
                .ok_or(DaemonError::NotFound)?;
            (record.header.org_id, record.header.team_id)
        };
        let Some(team) = self.policy(org_id, team_id).await? else {
            return refused(RejectReason::Policy, "P2P is not enabled in that team");
        };
        if body.len() > team.policy.limits.max_message_bytes as usize {
            return refused(
                RejectReason::Size,
                format!(
                    "the team allows messages of at most {} bytes",
                    team.policy.limits.max_message_bytes
                ),
            );
        }
        if let Some(rule) = team.scanner.find(&body) {
            return refused(
                RejectReason::Pattern,
                format!("the answer looks like it contains a secret (rule `{rule}`); remove it and answer again"),
            );
        }
        let review = {
            let mut state = self.state.lock().await;
            let review = self.kind == ClientKind::Desktop && state.settings.review_answers;
            let record = state
                .inc
                .get_mut(&request_id)
                .filter(|r| r.state == InState::Inbox)
                .ok_or(DaemonError::NotFound)?;
            if review {
                record.state = InState::PendingReview;
                record.draft = Some((body.clone(), refs.clone()));
                self.save_in(record);
                Some(item(record))
            } else {
                None
            }
        };
        match review {
            Some(request) => self.notify(Notice::ReviewRequest { request, answer: body }),
            None => self.finish(request_id, AnswerStatus::Answered, None, body, refs).await,
        }
        Ok(())
    }

    /// The user's decision on an answer that waited for review. A refused
    /// answer goes back to the agent's inbox.
    pub async fn decide_review(self: &Arc<Self>, request_id: Uuid, allow: bool) -> DaemonResult<()> {
        let draft = {
            let mut state = self.state.lock().await;
            let record = state
                .inc
                .get_mut(&request_id)
                .filter(|r| r.state == InState::PendingReview)
                .ok_or(DaemonError::NotFound)?;
            let draft = record.draft.take();
            record.state = InState::Inbox;
            self.save_in(record);
            draft
        };
        if let (true, Some((body, refs))) = (allow, draft) {
            self.finish(request_id, AnswerStatus::Answered, None, body, refs).await;
        }
        self.notify(Notice::Activity);
        Ok(())
    }

    /// Signs the answer to a received request and starts delivering it.
    async fn finish(
        self: &Arc<Self>,
        request_id: Uuid,
        status: AnswerStatus,
        reason: Option<RejectReason>,
        body: String,
        refs: Vec<Uuid>,
    ) {
        let now = self.now();
        {
            let mut state = self.state.lock().await;
            let user_id = state.me.user_id;
            let Some(record) = state
                .inc
                .get_mut(&request_id)
                .filter(|r| !matches!(r.state, InState::AnswerQueued | InState::Done))
            else {
                return;
            };
            // A notification has no answer to send.
            if record.header.kind == MessageKind::Notify {
                record.state = InState::Done;
                record.finished_at = Some(now);
                self.save_in(record);
                return;
            }
            let header = AnswerHeader {
                v: HEADER_VERSION,
                request_id,
                request_hash: record.hash.clone(),
                status,
                reason,
                answered_by: Party {
                    peer_id: self.identity.peer_id.clone(),
                    agent_label: record.header.to.agent_label.clone(),
                    user_id,
                },
                body_hash: sha256_hex(body.as_bytes()),
                body_bytes: u32::try_from(body.len()).unwrap_or(u32::MAX),
                refs,
                sent_at: now,
            };
            record.answer = Some(self.identity.sign_answer(&header, body));
            record.state = InState::AnswerQueued;
            record.attempts = 0;
            record.next_try = now;
            record.report_due = true;
            self.save_in(record);
        }
        let daemon = Arc::clone(self);
        tokio::spawn(async move { daemon.deliver_answer(request_id).await });
    }

    async fn deliver_answer(&self, request_id: Uuid) {
        let Some((answer, peer)) = ({
            let state = self.state.lock().await;
            state
                .inc
                .get(&request_id)
                .filter(|r| r.state == InState::AnswerQueued)
                .and_then(|r| Some((r.answer.clone()?, r.header.from.peer_id.clone())))
        }) else {
            return;
        };
        let outcome = self.transport.send(&peer, &[], Wire::Answer(answer)).await;
        let now = self.now();
        let mut state = self.state.lock().await;
        let Some(record) = state
            .inc
            .get_mut(&request_id)
            .filter(|r| r.state == InState::AnswerQueued)
        else {
            return;
        };
        match outcome {
            // Taken or refused, the sender has its outcome either way.
            Ok(_) => {
                record.state = InState::Done;
                record.finished_at = Some(now);
                record.report_due = true;
            }
            Err(_) => {
                record.attempts += 1;
                record.next_try = now + retry_delay(record.attempts);
            }
        }
        let record = record.clone();
        drop(state);
        self.save_in(&record);
        self.notify(Notice::Activity);
    }

    async fn receive_answer(&self, from_peer: &str, signed: Signed) -> Result<(), RejectReason> {
        // Only the request id is read before the signature is checked.
        #[derive(Deserialize)]
        struct IdOnly {
            request_id: Uuid,
        }
        use base64::Engine;
        let request_id = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&signed.header)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<IdOnly>(&bytes).ok())
            .ok_or(RejectReason::Invalid)?
            .request_id;
        let (opened, org_id, team_id) = {
            let state = self.state.lock().await;
            let record = state
                .out
                .get(&request_id)
                .filter(|o| o.header.to.peer_id == from_peer)
                .ok_or(RejectReason::Invalid)?;
            if record.state.finished() {
                return Err(RejectReason::Replay);
            }
            (
                OpenedRequest {
                    header: record.header.clone(),
                    hash: record.hash.clone(),
                },
                record.header.org_id,
                record.header.team_id,
            )
        };
        let header = open_answer(&signed, &opened).ok_or(RejectReason::Invalid)?;
        // The answer is scanned before a model sees it; a blocked answer is
        // recorded as answered with its text dropped.
        let blocked = match self.policy(org_id, team_id).await {
            Ok(Some(team)) => team.scanner.find(&signed.body),
            _ => SecretScanner::new(&[]).ok().and_then(|s| s.find(&signed.body)),
        };
        let now = self.now();
        let mut state = self.state.lock().await;
        let record = state
            .out
            .get_mut(&request_id)
            .filter(|o| !o.state.finished())
            .ok_or(RejectReason::Replay)?;
        record.state = match header.status {
            AnswerStatus::Answered => OutState::Answered,
            AnswerStatus::Rejected => OutState::Rejected,
            AnswerStatus::Expired => OutState::Expired,
            AnswerStatus::Error => OutState::Failed,
        };
        record.reason = header.reason;
        record.answer = Some(match &blocked {
            Some(_) => Signed {
                body: String::new(),
                ..signed
            },
            None => signed,
        });
        record.answer_blocked = blocked;
        record.finished_at = Some(now);
        record.report_due = true;
        let record = record.clone();
        drop(state);
        self.save_out(&record);
        self.notify(Notice::Activity);
        Ok(())
    }

    // ----- periodic work --------------------------------------------------

    /// Retries deliveries, gives up what is past its deadline, reports to
    /// the server, renews presence and drops peers the registry no longer
    /// vouches for. Called about once a second.
    pub async fn tick(self: &Arc<Self>) {
        let now = self.now();
        self.refresh_me(now).await;
        self.expire(now).await;

        let (requests, answers) = {
            let state = self.state.lock().await;
            (
                state
                    .out
                    .values()
                    .filter(|o| o.state == OutState::Queued && o.next_try <= now)
                    .map(|o| o.header.request_id)
                    .collect::<Vec<_>>(),
                state
                    .inc
                    .values()
                    .filter(|r| r.state == InState::AnswerQueued && r.next_try <= now)
                    .map(|r| r.header.request_id)
                    .collect::<Vec<_>>(),
            )
        };
        for id in requests {
            self.deliver_request(id).await;
        }
        for id in answers {
            self.deliver_answer(id).await;
        }
        self.report(now).await;
        self.heartbeat(now).await;
        self.forget(now).await;
    }

    async fn refresh_me(&self, now: i64) {
        if now - self.state.lock().await.me_at < 60 {
            return;
        }
        // Roles change; a failure keeps the last known ones a little longer.
        if let Ok(me) = self.registry.me().await {
            let mut state = self.state.lock().await;
            state.me = me;
            state.me_at = now;
        }
    }

    async fn expire(self: &Arc<Self>, now: i64) {
        let (expired_out, expired_in) = {
            let mut state = self.state.lock().await;
            let mut expired_out = Vec::new();
            for record in state.out.values_mut() {
                if !record.state.finished() && record.header.deadline <= now {
                    record.state = OutState::Expired;
                    record.finished_at = Some(now);
                    record.report_due = true;
                    expired_out.push(record.clone());
                }
            }
            let consent_timeouts: HashMap<Uuid, i64> = state
                .policies
                .iter()
                .filter_map(|(team, (_, p))| Some((*team, i64::from(p.as_ref()?.policy.limits.consent_timeout_secs))))
                .collect();
            let expired_in: Vec<Uuid> = state
                .inc
                .values()
                .filter(|r| match r.state {
                    InState::PendingConsent => {
                        let timeout = consent_timeouts.get(&r.header.team_id).copied().unwrap_or(600);
                        r.header.deadline <= now || now - r.received_at >= timeout
                    }
                    InState::Inbox | InState::PendingReview => r.header.deadline <= now,
                    InState::AnswerQueued | InState::Done => false,
                })
                .map(|r| r.header.request_id)
                .collect();
            // An answer that could not be delivered is dropped after a grace.
            for record in state.inc.values_mut() {
                if record.state == InState::AnswerQueued && record.header.deadline + ANSWER_GRACE_SECS <= now {
                    record.state = InState::Done;
                    record.finished_at = Some(now);
                    record.report_due = true;
                    self.save_in(record);
                }
            }
            (expired_out, expired_in)
        };
        for record in &expired_out {
            self.save_out(record);
        }
        for id in expired_in {
            self.finish(id, AnswerStatus::Expired, None, String::new(), Vec::new())
                .await;
        }
        if !expired_out.is_empty() {
            self.notify(Notice::Activity);
        }
    }

    /// Tells the server how exchanges stand: signed headers, never bodies.
    async fn report(&self, _now: i64) {
        let due: Vec<(Uuid, Uuid, Uuid, ExchangeReport, bool)> = {
            let state = self.state.lock().await;
            let out = state.out.values().filter(|o| o.report_due).map(|o| {
                let answer = o.answer.as_ref();
                let report = ExchangeReport {
                    role: ReportRole::Sender,
                    request_header: o.request.header.clone(),
                    request_signature: o.request.signature.clone(),
                    status: match o.state {
                        OutState::Rejected => ReportStatus::Rejected,
                        OutState::Expired => ReportStatus::Expired,
                        OutState::Failed => ReportStatus::Failed,
                        OutState::Queued | OutState::Delivered | OutState::Answered => ReportStatus::Delivered,
                    },
                    reject_reason: o.reason,
                    consent: None,
                    answer_header: answer.map(|a| a.header.clone()),
                    answer_signature: answer.map(|a| a.signature.clone()),
                };
                (o.header.request_id, o.header.org_id, o.header.team_id, report, true)
            });
            let inc = state.inc.values().filter(|r| r.report_due).map(|r| {
                let answer = r.answer.as_ref().filter(|_| r.state == InState::Done);
                let report = ExchangeReport {
                    role: ReportRole::Receiver,
                    request_header: r.request.header.clone(),
                    request_signature: r.request.signature.clone(),
                    status: ReportStatus::Delivered,
                    reject_reason: None,
                    consent: r.consent,
                    answer_header: answer.map(|a| a.header.clone()),
                    answer_signature: answer.map(|a| a.signature.clone()),
                };
                (r.header.request_id, r.header.org_id, r.header.team_id, report, false)
            });
            out.chain(inc).collect()
        };
        for (id, org_id, team_id, report, outgoing) in due {
            match self.registry.report(org_id, team_id, &report).await {
                // Refused reports are not retried: the server will not change
                // its mind, and the exchange itself is unaffected.
                Ok(()) | Err(RegistryError::Refused(_)) => {}
                Err(_) => continue,
            }
            let mut state = self.state.lock().await;
            if outgoing {
                if let Some(record) = state.out.get_mut(&id) {
                    // Another state change may have happened meanwhile.
                    record.report_due = report.answer_header.is_none() && record.answer.is_some();
                    self.save_out(record);
                }
            } else if let Some(record) = state.inc.get_mut(&id) {
                record.report_due =
                    report.answer_header.is_none() && record.state == InState::Done && record.answer.is_some();
                self.save_in(record);
            }
        }
    }

    /// Announces every registered agent in every team the user enabled.
    async fn heartbeat(&self, now: i64) {
        let (teams, agents) = {
            let mut state = self.state.lock().await;
            if state.stopped || now - state.last_heartbeat < i64::from(PRESENCE_TTL_SECS / 3) {
                return;
            }
            state.last_heartbeat = now;
            (
                state.settings.teams.clone(),
                state
                    .agents
                    .iter()
                    .map(|(label, a)| (label.clone(), a.capabilities.clone()))
                    .collect::<Vec<_>>(),
            )
        };
        if agents.is_empty() {
            return;
        }
        let addrs = self.transport.listen_addrs().await;
        for team in teams {
            let Ok(Some(policy)) = self.policy(team.org_id, team.team_id).await else {
                continue;
            };
            let mode = {
                let state = self.state.lock().await;
                self.effective_mode(&state, team.team_id, Some(&policy.policy))
            };
            let multiaddrs: Vec<String> = addrs
                .iter()
                .filter(|a| policy.policy.discovery.lan_addrs || !p2p::is_private_multiaddr(a))
                .take(p2p::MAX_MULTIADDRS)
                .cloned()
                .collect();
            for (label, capabilities) in &agents {
                let presence = Presence {
                    peer_id: self.identity.peer_id.clone(),
                    agent_label: label.clone(),
                    capabilities: capabilities.clone(),
                    multiaddrs: multiaddrs.clone(),
                    accept_mode: mode,
                    ttl_secs: PRESENCE_TTL_SECS,
                };
                if let Err(err) = self.registry.announce(team.org_id, team.team_id, &presence).await {
                    tracing::debug!(event = "presence_failed", component = "nanace_p2p", error = %err, "presence was not announced");
                }
            }
        }
    }

    /// Drops what no longer matters: old finished exchanges, expired
    /// request ids, and connections to peers the registry stopped vouching
    /// for (revoked, left the team, P2P turned off).
    async fn forget(&self, now: i64) {
        {
            let mut state = self.state.lock().await;
            let old_out: Vec<Uuid> = state
                .out
                .values()
                .filter(|o| o.finished_at.is_some_and(|at| now - at > KEEP_FINISHED_SECS) && !o.report_due)
                .map(|o| o.header.request_id)
                .collect();
            for id in old_out {
                state.out.remove(&id);
                self.spool.remove("out", id);
            }
            let old_in: Vec<Uuid> = state
                .inc
                .values()
                .filter(|r| r.finished_at.is_some_and(|at| now - at > KEEP_FINISHED_SECS) && !r.report_due)
                .map(|r| r.header.request_id)
                .collect();
            for id in old_in {
                state.inc.remove(&id);
                self.spool.remove("in", id);
            }
            state
                .seen
                .retain(|_, deadline| *deadline + CLOCK_SKEW_SECS + KEEP_FINISHED_SECS > now);
            state.rates.retain(|_, (start, _)| now - *start < 120);
            state.lookups.retain(|_, (at, _)| now - *at < CACHE_SECS);
        }
        for connection in self.transport.connections().await {
            if let Ok(None) = self.lookup(&connection.peer_id).await {
                self.transport.disconnect(&connection.peer_id).await;
            }
        }
    }

    // ----- desktop --------------------------------------------------------

    /// The desktop app attached or detached. Without it a laptop accepts
    /// nothing, so presence is announced again at once with the new mode.
    pub async fn set_supervisor(&self, attached: bool) {
        let mut state = self.state.lock().await;
        state.supervisor = attached;
        state.last_heartbeat = 0;
    }

    /// Takes part in a team with this accept mode, or changes the mode.
    pub async fn set_team(&self, org_id: Uuid, team_id: Uuid, accept_mode: AcceptMode) -> DaemonResult<()> {
        let settings = {
            let mut state = self.state.lock().await;
            if state.me.role_in(team_id).is_none() {
                return Err(DaemonError::NotFound);
            }
            match state.settings.teams.iter_mut().find(|t| t.team_id == team_id) {
                Some(team) => team.accept_mode = accept_mode,
                None => state.settings.teams.push(TeamSettings {
                    org_id,
                    team_id,
                    accept_mode,
                    trusted: HashMap::new(),
                }),
            }
            state.stopped = false;
            state.last_heartbeat = 0;
            state.settings.clone()
        };
        self.save_settings(&settings)
    }

    /// Stops taking part in a team and withdraws presence there.
    pub async fn leave_team(&self, team_id: Uuid) -> DaemonResult<()> {
        let (settings, left, agents) = {
            let mut state = self.state.lock().await;
            let left = state.settings.teams.iter().find(|t| t.team_id == team_id).cloned();
            state.settings.teams.retain(|t| t.team_id != team_id);
            (
                state.settings.clone(),
                left,
                state.agents.keys().cloned().collect::<Vec<_>>(),
            )
        };
        self.save_settings(&settings)?;
        if let Some(team) = left {
            for label in agents {
                let _ = self
                    .registry
                    .withdraw(team.org_id, team.team_id, &self.identity.peer_id, &label)
                    .await;
            }
        }
        Ok(())
    }

    pub async fn set_trusted(&self, team_id: Uuid, user_id: Uuid, trusted: bool) -> DaemonResult<()> {
        let settings = {
            let mut state = self.state.lock().await;
            let team = state
                .settings
                .teams
                .iter_mut()
                .find(|t| t.team_id == team_id)
                .ok_or(DaemonError::NotFound)?;
            if trusted {
                team.trusted.insert(user_id, i64::MAX);
            } else {
                team.trusted.remove(&user_id);
            }
            state.settings.clone()
        };
        self.save_settings(&settings)
    }

    pub async fn set_review_answers(&self, review: bool) -> DaemonResult<()> {
        let settings = {
            let mut state = self.state.lock().await;
            state.settings.review_answers = review;
            state.settings.clone()
        };
        self.save_settings(&settings)
    }

    pub async fn settings(&self) -> Settings {
        self.state.lock().await.settings.clone()
    }

    /// "Stop everything": refuses what is waiting, withdraws presence and
    /// closes connections. Sending and accepting stay off until a team is
    /// enabled again.
    pub async fn stop_all(self: &Arc<Self>) {
        let (waiting, teams, agents) = {
            let mut state = self.state.lock().await;
            state.stopped = true;
            for record in state.out.values_mut() {
                if !record.state.finished() {
                    record.state = OutState::Failed;
                    record.finished_at = Some(self.now());
                    record.report_due = true;
                    self.save_out(record);
                }
            }
            (
                state
                    .inc
                    .values()
                    .filter(|r| {
                        matches!(
                            r.state,
                            InState::PendingConsent | InState::Inbox | InState::PendingReview
                        )
                    })
                    .map(|r| r.header.request_id)
                    .collect::<Vec<_>>(),
                state.settings.teams.clone(),
                state.agents.keys().cloned().collect::<Vec<_>>(),
            )
        };
        for id in waiting {
            self.finish(
                id,
                AnswerStatus::Rejected,
                Some(RejectReason::Consent),
                String::new(),
                Vec::new(),
            )
            .await;
        }
        for team in &teams {
            for label in &agents {
                let _ = self
                    .registry
                    .withdraw(team.org_id, team.team_id, &self.identity.peer_id, label)
                    .await;
            }
        }
        // Give the refusals a moment to leave before the connections close.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        for connection in self.transport.connections().await {
            self.transport.disconnect(&connection.peer_id).await;
        }
        self.notify(Notice::Activity);
    }

    // ----- views ----------------------------------------------------------

    pub async fn whoami(&self) -> WhoAmI {
        let (me, settings, agents) = {
            let state = self.state.lock().await;
            (
                state.me.clone(),
                state.settings.clone(),
                state.agents.keys().cloned().collect::<Vec<_>>(),
            )
        };
        let mut teams = Vec::new();
        for team in &settings.teams {
            let policy = self.policy(team.org_id, team.team_id).await.ok().flatten();
            let membership = me.teams.iter().find(|t| t.team_id == team.team_id);
            let effective = {
                let state = self.state.lock().await;
                self.effective_mode(&state, team.team_id, policy.as_ref().map(|p| &p.policy))
            };
            teams.push(TeamView {
                org_id: team.org_id,
                team_id: team.team_id,
                team_slug: membership.map(|m| m.team_slug.clone()),
                role: membership.map(|m| m.role),
                accept_mode: team.accept_mode,
                effective_accept_mode: effective,
                p2p_enabled: policy.is_some(),
                policy_version: policy.map(|p| p.version.clone()),
            });
        }
        WhoAmI {
            peer_id: self.identity.peer_id.clone(),
            user_id: me.user_id,
            email: me.email,
            client_kind: self.kind,
            agents,
            teams,
        }
    }

    /// Peers of a team, by the registry's visibility rules.
    pub async fn find_peers(
        &self,
        org_id: Uuid,
        team_id: Uuid,
        capability: Option<&str>,
    ) -> DaemonResult<Vec<PresentPeer>> {
        let capability = match capability {
            Some(c) => Some(
                p2p::capabilities(&[c.to_owned()])
                    .map_err(|e| DaemonError::Invalid(e.to_string()))?
                    .remove(0),
            ),
            None => None,
        };
        let peers = self.registry.peers(org_id, team_id, capability.as_deref()).await?;
        Ok(peers
            .into_iter()
            .filter(|p| p.peer_id != self.identity.peer_id)
            .collect())
    }

    /// The guidance a team wrote for agents, and whether P2P is on.
    pub async fn policy_text(&self, org_id: Uuid, team_id: Uuid) -> DaemonResult<(bool, String)> {
        let served = self.registry.policy(org_id, team_id, true).await?;
        let enabled = served.policy.as_ref().is_some_and(|p| p.enabled);
        Ok((enabled, served.text.unwrap_or_else(|| served.errors.join("; "))))
    }

    /// Requests waiting for the user: consent first, then answers to review.
    pub async fn pending(&self) -> Vec<(InboxItem, Option<String>)> {
        let state = self.state.lock().await;
        let mut waiting: Vec<_> = state
            .inc
            .values()
            .filter(|r| matches!(r.state, InState::PendingConsent | InState::PendingReview))
            .map(|r| (item(r), r.draft.as_ref().map(|(body, _)| body.clone())))
            .collect();
        waiting.sort_by_key(|(i, _)| i.received_at);
        waiting
    }

    pub async fn activity(&self) -> Activity {
        let (out, mut incoming) = {
            let state = self.state.lock().await;
            (
                state.out.values().cloned().collect::<Vec<_>>(),
                state.inc.values().map(item).collect::<Vec<_>>(),
            )
        };
        let mut outgoing = Vec::new();
        for record in &out {
            let mut view = self.reply_view(record).await;
            // The activity list shows what happened, not what was said.
            view.answer = None;
            outgoing.push(view);
        }
        outgoing.sort_by_key(|o| o.request_id);
        incoming.sort_by_key(|i| i.received_at);
        for i in &mut incoming {
            i.body.clear();
        }
        Activity {
            connections: self.transport.connections().await,
            outgoing,
            incoming,
        }
    }
}
