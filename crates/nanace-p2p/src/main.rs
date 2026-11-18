//! `nanace-p2p`: the network node and the per-user daemon.
//!
//! ```text
//! nanace-p2p node              run the relay node (server side)
//! nanace-p2p daemon            run the daemon (laptop sidecar, or agent container)
//! nanace-p2p peer-id           print the peer id of NANACE_P2P_KEY_FILE, creating the key if needed
//! nanace-p2p call <method> [json params] [--agent <label>]
//!                              call a running daemon over its local socket
//! ```
//!
//! Configuration comes from the environment; secrets come from files or
//! standard input, never from arguments. Logs go to standard error.
//!
//! Node: `NANACE_P2P_KEY_FILE`, `NANACE_P2P_LISTEN`, `NANACE_P2P_EXTERNAL_ADDRS`,
//! `NANACE_INTERNAL_URL`, `NANACE_P2P_NODE_SECRET_FILE`.
//!
//! Daemon: `NANACE_API_URL`, `NANACE_TOKEN_FILE`, `NANACE_P2P_DIR`,
//! `NANACE_P2P_BOOTSTRAP`, `NANACE_P2P_KIND` (`server` or `desktop`),
//! `NANACE_P2P_SOCKET`, `NANACE_P2P_SECRET_FILE`, `NANACE_P2P_LISTEN`.
//! A server daemon also reads `NANACE_P2P_KEY_FILE` and `NANACE_P2P_TEAMS`
//! (`<org id>:<team id>:<accept mode>`, comma separated). A desktop daemon
//! reads one JSON line from standard input, `{"supervisor_secret", "peer_key"}`,
//! and stops when standard input closes, that is, when the app is gone.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use libp2p::Multiaddr;
use nanace_core::p2p::{AcceptMode, ClientKind};
use nanace_p2p::daemon::Daemon;
use nanace_p2p::envelope::Identity;
use nanace_p2p::ipc::{IpcSecrets, listen};
use nanace_p2p::ipc_client::IpcClient;
use nanace_p2p::net::{Bootstrap, Net};
use nanace_p2p::node::{Limits, NodeConfig};
use nanace_p2p::registry::HttpRegistry;
use nanace_p2p::spool::write_private;
use secrecy::SecretString;
use serde::Deserialize;
use tokio::io::AsyncBufReadExt;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

const COMPONENT: &str = "nanace_p2p";

fn var(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

fn required(key: &str) -> anyhow::Result<String> {
    var(key).with_context(|| format!("{key} must be set"))
}

fn random_secret() -> anyhow::Result<[u8; 32]> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| anyhow::anyhow!("system RNG failed: {e}"))?;
    Ok(bytes)
}

fn decode_key(text: &str) -> anyhow::Result<[u8; 32]> {
    URL_SAFE_NO_PAD
        .decode(text.trim())
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .context("a key is 32 bytes, base64url")
}

/// Reads the Ed25519 seed in `path`, creating it (readable by the user
/// only) on first use.
fn load_or_create_key(path: &Path) -> anyhow::Result<[u8; 32]> {
    match std::fs::read_to_string(path) {
        Ok(text) => decode_key(&text).with_context(|| format!("{} is not a key file", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            let key = random_secret()?;
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            write_private(path, URL_SAFE_NO_PAD.encode(key).as_bytes())
                .with_context(|| format!("cannot write {}", path.display()))?;
            Ok(key)
        }
        Err(err) => Err(err).with_context(|| format!("cannot read {}", path.display())),
    }
}

fn multiaddrs(key: &str, default: &str) -> anyhow::Result<Vec<Multiaddr>> {
    var(key)
        .unwrap_or_else(|| default.to_owned())
        .split(',')
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .map(|a| a.parse().with_context(|| format!("{key}: `{a}` is not a multiaddr")))
        .collect()
}

async fn node() -> anyhow::Result<()> {
    let key = load_or_create_key(Path::new(
        &var("NANACE_P2P_KEY_FILE").unwrap_or_else(|| "node.key".to_owned()),
    ))?;
    let secret_file = required("NANACE_P2P_NODE_SECRET_FILE")?;
    let secret = std::fs::read_to_string(&secret_file).with_context(|| format!("cannot read {secret_file}"))?;
    nanace_p2p::node::run(NodeConfig {
        key,
        listen: multiaddrs(
            "NANACE_P2P_LISTEN",
            "/ip4/0.0.0.0/udp/4001/quic-v1,/ip4/0.0.0.0/tcp/4001",
        )?,
        external: multiaddrs("NANACE_P2P_EXTERNAL_ADDRS", "")?,
        internal_url: required("NANACE_INTERNAL_URL")?,
        secret: SecretString::from(secret.trim().to_owned()),
        limits: Limits::default(),
    })
    .await
}

#[derive(Deserialize)]
struct DesktopHandshake {
    supervisor_secret: String,
    /// base64url Ed25519 seed, kept by the app in the OS keyring.
    peer_key: String,
}

fn default_socket(dir: &Path) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(format!(r"\\.\pipe\nanace-p2p-{}", Uuid::new_v4().simple()))
    } else {
        dir.join("p2p.sock")
    }
}

async fn daemon() -> anyhow::Result<()> {
    let dir = PathBuf::from(required("NANACE_P2P_DIR")?);
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let kind = match var("NANACE_P2P_KIND").as_deref() {
        None | Some("server") => ClientKind::Server,
        Some("desktop") => ClientKind::Desktop,
        Some(other) => bail!("NANACE_P2P_KIND must be server or desktop, got `{other}`"),
    };
    let mut stdin = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    let (key, supervisor) = match kind {
        ClientKind::Server => {
            let file = var("NANACE_P2P_KEY_FILE").map_or_else(|| dir.join("peer.key"), PathBuf::from);
            (load_or_create_key(&file)?, None)
        }
        ClientKind::Desktop => {
            let line = stdin
                .next_line()
                .await?
                .context("a desktop daemon reads its handshake from standard input")?;
            let handshake: DesktopHandshake =
                serde_json::from_str(&line).context("the handshake on standard input is not valid")?;
            if handshake.supervisor_secret.len() < 32 {
                bail!("supervisor_secret must be at least 32 characters");
            }
            (
                decode_key(&handshake.peer_key)?,
                Some(SecretString::from(handshake.supervisor_secret)),
            )
        }
    };
    let identity = Identity::from_secret(key);
    let registry = Arc::new(HttpRegistry::new(
        &var("NANACE_API_URL").unwrap_or_else(|| "http://127.0.0.1:8080".to_owned()),
        PathBuf::from(required("NANACE_TOKEN_FILE")?),
    )?);
    let net = Net::start(
        &identity,
        &multiaddrs("NANACE_P2P_LISTEN", "/ip4/0.0.0.0/udp/0/quic-v1,/ip4/0.0.0.0/tcp/0")?,
        Bootstrap::parse(&required("NANACE_P2P_BOOTSTRAP")?)?,
    )?;
    let daemon = Daemon::start(
        &dir,
        identity,
        kind,
        registry,
        net.clone(),
        Arc::new(|| chrono::Utc::now().timestamp()),
    )
    .await
    .context("the daemon could not start")?;
    net.attach(Arc::clone(&daemon)).await;
    tracing::info!(event = "daemon_start", component = COMPONENT, peer = %daemon.peer_id(), kind = kind.as_str(), "daemon started");

    // A server has no desktop app to choose teams: they come from its
    // configuration.
    for entry in var("NANACE_P2P_TEAMS")
        .unwrap_or_default()
        .split(',')
        .filter(|e| !e.trim().is_empty())
    {
        let parts: Vec<&str> = entry.trim().split(':').collect();
        let [org, team, mode] = parts.as_slice() else {
            bail!("NANACE_P2P_TEAMS: `{entry}` is not <org id>:<team id>:<accept mode>");
        };
        let mode: AcceptMode = serde_json::from_value(serde_json::Value::String((*mode).to_owned()))
            .with_context(|| format!("NANACE_P2P_TEAMS: `{mode}` is not off, ask, trusted or team"))?;
        daemon
            .set_team(org.parse()?, team.parse()?, mode)
            .await
            .with_context(|| format!("NANACE_P2P_TEAMS: cannot take part in team {team}"))?;
    }

    // The agent secret lets programs of this user reach the daemon.
    let agent_secret = URL_SAFE_NO_PAD.encode(random_secret()?);
    let secret_file = var("NANACE_P2P_SECRET_FILE").map_or_else(|| dir.join("ipc.secret"), PathBuf::from);
    write_private(&secret_file, agent_secret.as_bytes())
        .with_context(|| format!("cannot write {}", secret_file.display()))?;
    let secrets = Arc::new(IpcSecrets {
        agent: SecretString::from(agent_secret),
        supervisor,
    });
    let socket = var("NANACE_P2P_SOCKET").map_or_else(|| default_socket(&dir), PathBuf::from);
    {
        let (daemon, socket) = (Arc::clone(&daemon), socket.clone());
        tokio::spawn(async move {
            if let Err(err) = listen(daemon, secrets, &socket).await {
                tracing::error!(event = "ipc_failed", component = COMPONENT, error = %err, "the local socket stopped");
            }
        });
    }

    let mut ticks = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            _ = ticks.tick() => daemon.tick().await,
            _ = tokio::signal::ctrl_c() => break,
            _ = terminated() => break,
            // The desktop app is gone: nothing may be accepted without it.
            line = stdin.next_line(), if kind == ClientKind::Desktop => {
                if !matches!(line, Ok(Some(_))) {
                    break;
                }
            }
        }
    }
    daemon.withdraw_all().await;
    let _ = std::fs::remove_file(&secret_file);
    #[cfg(unix)]
    let _ = std::fs::remove_file(&socket);
    tracing::info!(event = "daemon_stop", component = COMPONENT, "daemon stopped");
    Ok(())
}

#[cfg(unix)]
async fn terminated() {
    use tokio::signal::unix::{SignalKind, signal};
    match signal(SignalKind::terminate()) {
        Ok(mut term) => {
            term.recv().await;
        }
        Err(_) => std::future::pending().await,
    }
}

#[cfg(not(unix))]
async fn terminated() {
    std::future::pending::<()>().await;
}

/// `nanace-p2p call <method> [json] [--agent <label>]`: one call to a
/// running daemon, for checking a setup by hand.
async fn call(args: &[String]) -> anyhow::Result<()> {
    let mut positional = Vec::new();
    let mut agent = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--agent" {
            agent = Some(iter.next().context("--agent needs a label")?.clone());
        } else {
            positional.push(arg.clone());
        }
    }
    let method = positional
        .first()
        .context("usage: nanace-p2p call <method> [json params] [--agent <label>]")?;
    let params: serde_json::Value = match positional.get(1) {
        Some(text) => serde_json::from_str(text).context("params must be JSON")?,
        None => serde_json::json!({}),
    };
    let dir = var("NANACE_P2P_DIR").map(PathBuf::from);
    let socket = var("NANACE_P2P_SOCKET")
        .map(PathBuf::from)
        .or_else(|| dir.as_ref().map(|d| default_socket(d)))
        .context("set NANACE_P2P_SOCKET or NANACE_P2P_DIR")?;
    let secret_file = var("NANACE_P2P_SECRET_FILE")
        .map(PathBuf::from)
        .or_else(|| dir.map(|d| d.join("ipc.secret")))
        .context("set NANACE_P2P_SECRET_FILE or NANACE_P2P_DIR")?;
    let secret =
        std::fs::read_to_string(&secret_file).with_context(|| format!("cannot read {}", secret_file.display()))?;
    let label = agent.unwrap_or_else(|| "cli".to_owned());
    let mut client = IpcClient::connect(&socket, secret.trim(), Some(&label), &[]).await?;
    let result = client.call(method, params).await?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    // Standard output belongs to `call`; everything else is logged to stderr.
    match std::env::var("NANACE_LOG_FORMAT").as_deref() {
        Ok("json") => tracing_subscriber::fmt()
            .json()
            .with_writer(std::io::stderr)
            .with_env_filter(filter)
            .init(),
        Ok("text") | Err(_) => tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_env_filter(filter)
            .init(),
        Ok(other) => bail!("NANACE_LOG_FORMAT must be text or json, got `{other}`"),
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("node") => node().await,
        Some("daemon") => daemon().await,
        Some("peer-id") => {
            let file = var("NANACE_P2P_KEY_FILE").unwrap_or_else(|| "node.key".to_owned());
            println!("{}", nanace_p2p::node::peer_id(load_or_create_key(Path::new(&file))?)?);
            Ok(())
        }
        Some("call") => call(&args[1..]).await,
        _ => bail!("usage: nanace-p2p <node | daemon | peer-id | call <method> [json params] [--agent <label>]>"),
    }
}
