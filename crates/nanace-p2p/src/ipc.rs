//! The local channel between the daemon and the programs of the same user:
//! agents (through `nanace-mcp`) and the desktop app.
//!
//! JSON-RPC 2.0, one message per line. A session starts with `hello`, which
//! presents a secret and thereby a role:
//!
//! - **agent**: may register, send, read its inbox, answer and set its
//!   status. Its secret is in a file only the user can read.
//! - **supervisor** (the desktop app): may also answer consent prompts and
//!   change settings. Its secret is never on disk, so a session that only
//!   has the agent secret cannot approve its own inbound requests.

use std::sync::Arc;

use nanace_core::p2p::AcceptMode;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use serde_json::{Value, json};
use subtle::ConstantTimeEq;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use uuid::Uuid;

use crate::daemon::{AgentStatus, Daemon, DaemonError, OutState, SendParams};

/// Longest a call may hold the connection waiting for an answer.
const MAX_WAIT_SECS: u64 = 60;
/// A line longer than this is not a message.
const MAX_LINE_BYTES: usize = 256 * 1024;

const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const UNAUTHENTICATED: i64 = -32000;
const FORBIDDEN: i64 = -32001;
const REFUSED: i64 = -32002;
const NOT_FOUND: i64 = -32004;
const UNAVAILABLE: i64 = -32005;

pub struct IpcSecrets {
    pub agent: SecretString,
    /// `None` on a server, where there is no desktop app.
    pub supervisor: Option<SecretString>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Agent,
    Supervisor,
}

struct RpcError {
    code: i64,
    message: String,
    data: Option<Value>,
}

type RpcResult = Result<Value, RpcError>;

fn error(code: i64, message: impl Into<String>) -> RpcError {
    RpcError {
        code,
        message: message.into(),
        data: None,
    }
}

impl From<DaemonError> for RpcError {
    fn from(err: DaemonError) -> Self {
        let (code, data) = match &err {
            DaemonError::Invalid(_) => (INVALID_PARAMS, None),
            DaemonError::Refused { reason, .. } => (REFUSED, Some(json!({ "reason": reason }))),
            DaemonError::NotFound => (NOT_FOUND, None),
            DaemonError::Registry(_) | DaemonError::Storage(_) => (UNAVAILABLE, None),
        };
        Self {
            code,
            message: err.to_string(),
            data,
        }
    }
}

fn params<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, RpcError> {
    serde_json::from_value(value).map_err(|e| error(INVALID_PARAMS, format!("invalid params: {e}")))
}

fn to_value(value: impl serde::Serialize) -> RpcResult {
    serde_json::to_value(value).map_err(|e| error(UNAVAILABLE, e.to_string()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Hello {
    secret: String,
    /// The agent's label; required for the agent role.
    agent_label: Option<String>,
    #[serde(default)]
    capabilities: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TeamRef {
    org_id: Uuid,
    team_id: Uuid,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FindPeers {
    org_id: Uuid,
    team_id: Uuid,
    capability: Option<String>,
}

#[derive(Deserialize)]
struct Ask {
    #[serde(flatten)]
    send: SendParams,
    /// Wait this long for the answer before returning only the request id.
    #[serde(default)]
    wait_secs: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GetReply {
    request_id: Uuid,
    #[serde(default)]
    wait_secs: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    request_id: Uuid,
    body: String,
    #[serde(default)]
    refs: Vec<Uuid>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DecideConsent {
    request_id: Uuid,
    allow: bool,
    trust_hours: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DecideReview {
    request_id: Uuid,
    allow: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetTeam {
    org_id: Uuid,
    team_id: Uuid,
    accept_mode: AcceptMode,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LeaveTeam {
    team_id: Uuid,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetTrusted {
    team_id: Uuid,
    user_id: Uuid,
    trusted: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetReview {
    review: bool,
}

struct Session {
    role: Role,
    /// The agent this session speaks for.
    agent: Option<String>,
}

/// Waits until a sent request is finished or `wait_secs` pass.
async fn wait_reply(daemon: &Daemon, request_id: Uuid, wait_secs: u64) -> RpcResult {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(wait_secs.min(MAX_WAIT_SECS));
    loop {
        let reply = daemon.reply_of(request_id).await?;
        let finished = !matches!(reply.state, OutState::Queued | OutState::Delivered);
        if finished || tokio::time::Instant::now() >= deadline {
            return to_value(reply);
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

async fn dispatch(daemon: &Arc<Daemon>, session: &Session, method: &str, value: Value) -> RpcResult {
    // What every role may do.
    match method {
        "whoami" => return to_value(daemon.whoami().await),
        "activity" => return to_value(daemon.activity().await),
        "find_peers" => {
            let p: FindPeers = params(value)?;
            return to_value(daemon.find_peers(p.org_id, p.team_id, p.capability.as_deref()).await?);
        }
        "get_policy" => {
            let p: TeamRef = params(value)?;
            let (enabled, text) = daemon.policy_text(p.org_id, p.team_id).await?;
            return Ok(json!({ "enabled": enabled, "text": text }));
        }
        "get_reply" => {
            let p: GetReply = params(value)?;
            return wait_reply(daemon, p.request_id, p.wait_secs).await;
        }
        _ => {}
    }
    match (session.role, &session.agent, method) {
        (_, Some(agent), "ask") => {
            let p: Ask = params(value)?;
            let sent = daemon.send(agent, p.send).await?;
            if p.wait_secs == 0 {
                return to_value(sent);
            }
            wait_reply(daemon, sent.request_id, p.wait_secs).await
        }
        (_, Some(agent), "inbox") => to_value(daemon.inbox(agent).await),
        (_, Some(agent), "reply") => {
            let p: Reply = params(value)?;
            daemon.reply(agent, p.request_id, p.body, p.refs).await?;
            Ok(json!({ "ok": true }))
        }
        (_, Some(agent), "set_status") => {
            daemon.set_status(agent, params::<AgentStatus>(value)?).await?;
            Ok(json!({ "ok": true }))
        }
        (_, None, "ask" | "inbox" | "reply" | "set_status") => {
            Err(error(INVALID_REQUEST, "this session registered no agent in `hello`"))
        }
        (Role::Supervisor, _, "pending") => {
            let waiting: Vec<Value> = daemon
                .pending()
                .await
                .into_iter()
                .map(|(request, answer)| json!({ "request": request, "answer": answer }))
                .collect();
            Ok(Value::Array(waiting))
        }
        (Role::Supervisor, _, "decide_consent") => {
            let p: DecideConsent = params(value)?;
            daemon.decide_consent(p.request_id, p.allow, p.trust_hours).await?;
            Ok(json!({ "ok": true }))
        }
        (Role::Supervisor, _, "decide_review") => {
            let p: DecideReview = params(value)?;
            daemon.decide_review(p.request_id, p.allow).await?;
            Ok(json!({ "ok": true }))
        }
        (Role::Supervisor, _, "set_team") => {
            let p: SetTeam = params(value)?;
            daemon.set_team(p.org_id, p.team_id, p.accept_mode).await?;
            Ok(json!({ "ok": true }))
        }
        (Role::Supervisor, _, "leave_team") => {
            daemon.leave_team(params::<LeaveTeam>(value)?.team_id).await?;
            Ok(json!({ "ok": true }))
        }
        (Role::Supervisor, _, "set_trusted") => {
            let p: SetTrusted = params(value)?;
            daemon.set_trusted(p.team_id, p.user_id, p.trusted).await?;
            Ok(json!({ "ok": true }))
        }
        (Role::Supervisor, _, "set_review_answers") => {
            daemon.set_review_answers(params::<SetReview>(value)?.review).await?;
            Ok(json!({ "ok": true }))
        }
        (Role::Supervisor, _, "settings") => to_value(daemon.settings().await),
        (Role::Supervisor, _, "stop_all") => {
            daemon.stop_all().await;
            Ok(json!({ "ok": true }))
        }
        (
            Role::Agent,
            _,
            "pending" | "decide_consent" | "decide_review" | "set_team" | "leave_team" | "set_trusted"
            | "set_review_answers" | "settings" | "stop_all",
        ) => Err(error(FORBIDDEN, "only the desktop app may do this")),
        _ => Err(error(METHOD_NOT_FOUND, format!("unknown method `{method}`"))),
    }
}

fn response(id: Value, result: RpcResult) -> Value {
    match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(err) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": err.code, "message": err.message, "data": err.data },
        }),
    }
}

fn matches_secret(expected: &SecretString, presented: &str) -> bool {
    bool::from(expected.expose_secret().as_bytes().ct_eq(presented.as_bytes()))
}

/// Serves one connection until it closes. The agent a session registered
/// is unregistered when it ends, and the daemon learns when the desktop app
/// detaches.
pub async fn serve<S>(daemon: Arc<Daemon>, secrets: Arc<IpcSecrets>, stream: S)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (reader, mut writer) = tokio::io::split(stream);
    let mut lines = BufReader::new(reader).lines();
    let mut session: Option<Session> = None;
    let mut notices = daemon.subscribe();

    loop {
        let line = tokio::select! {
            line = lines.next_line() => match line {
                Ok(Some(line)) => line,
                _ => break,
            },
            notice = notices.recv() => {
                // Prompts go to the desktop app only.
                if let (Ok(notice), Some(Session { role: Role::Supervisor, .. })) = (notice, &session) {
                    let message = json!({ "jsonrpc": "2.0", "method": "notice", "params": notice });
                    if writer.write_all(format!("{message}\n").as_bytes()).await.is_err() {
                        break;
                    }
                }
                continue;
            }
        };
        if line.len() > MAX_LINE_BYTES {
            break;
        }
        let Ok(request) = serde_json::from_str::<Value>(&line) else {
            break;
        };
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let method = request.get("method").and_then(Value::as_str).unwrap_or_default();
        let value = request.get("params").cloned().unwrap_or(json!({}));

        let result = match (&session, method) {
            (None, "hello") => match params::<Hello>(value) {
                Err(err) => Err(err),
                Ok(hello) => {
                    let role = if secrets
                        .supervisor
                        .as_ref()
                        .is_some_and(|s| matches_secret(s, &hello.secret))
                    {
                        Some(Role::Supervisor)
                    } else if matches_secret(&secrets.agent, &hello.secret) {
                        Some(Role::Agent)
                    } else {
                        None
                    };
                    match role {
                        None => Err(error(UNAUTHENTICATED, "wrong secret")),
                        Some(Role::Agent) if hello.agent_label.is_none() => {
                            Err(error(INVALID_PARAMS, "an agent session needs `agent_label`"))
                        }
                        Some(role) => {
                            let registered = match &hello.agent_label {
                                Some(label) => daemon.register_agent(label, &hello.capabilities).await,
                                None => Ok(()),
                            };
                            match registered {
                                Err(err) => Err(err.into()),
                                Ok(()) => {
                                    if role == Role::Supervisor {
                                        daemon.set_supervisor(true).await;
                                    }
                                    session = Some(Session {
                                        role,
                                        agent: hello.agent_label,
                                    });
                                    Ok(json!({
                                        "role": if role == Role::Supervisor { "supervisor" } else { "agent" },
                                        "peer_id": daemon.peer_id(),
                                    }))
                                }
                            }
                        }
                    }
                }
            },
            (None, _) => Err(error(UNAUTHENTICATED, "say `hello` first")),
            (Some(_), "hello") => Err(error(INVALID_REQUEST, "already authenticated")),
            (Some(session), method) => dispatch(&daemon, session, method, value).await,
        };
        let failed_hello = session.is_none();
        if writer
            .write_all(format!("{}\n", response(id, result)).as_bytes())
            .await
            .is_err()
            || failed_hello
        {
            // One wrong secret ends the connection: no guessing in a loop.
            break;
        }
    }
    if let Some(session) = session {
        if let Some(agent) = &session.agent {
            daemon.unregister_agent(agent).await;
        }
        if session.role == Role::Supervisor {
            daemon.set_supervisor(false).await;
        }
    }
}

/// Accepts connections on a Unix socket only the user can reach: the socket
/// and its directory are private, and a connection from another uid is
/// dropped before a byte is read.
#[cfg(unix)]
pub async fn listen(daemon: Arc<Daemon>, secrets: Arc<IpcSecrets>, socket: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let dir = socket
        .parent()
        .ok_or_else(|| std::io::Error::other("the socket path has no directory"))?;
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    // A socket left by a daemon that is gone.
    let _ = std::fs::remove_file(socket);
    let listener = tokio::net::UnixListener::bind(socket)?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    let owner = std::fs::metadata(dir)?.uid();
    loop {
        let (stream, _) = listener.accept().await?;
        if stream.peer_cred().is_ok_and(|cred| cred.uid() == owner) {
            tokio::spawn(serve(Arc::clone(&daemon), Arc::clone(&secrets), stream));
        }
    }
}

/// Accepts connections on a named pipe that refuses remote clients. The
/// pipe's default access list lets other local accounts open it, so here the
/// secret in `hello` is what keeps them out.
#[cfg(windows)]
pub async fn listen(daemon: Arc<Daemon>, secrets: Arc<IpcSecrets>, pipe: &std::path::Path) -> std::io::Result<()> {
    use tokio::net::windows::named_pipe::ServerOptions;

    let mut server = ServerOptions::new()
        .first_pipe_instance(true)
        .reject_remote_clients(true)
        .create(pipe)?;
    loop {
        server.connect().await?;
        let connected = std::mem::replace(
            &mut server,
            ServerOptions::new().reject_remote_clients(true).create(pipe)?,
        );
        tokio::spawn(serve(Arc::clone(&daemon), Arc::clone(&secrets), connected));
    }
}
