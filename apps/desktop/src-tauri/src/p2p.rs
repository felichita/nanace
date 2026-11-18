//! The P2P daemon (`nanace-p2p`) as a sidecar of this app.
//!
//! The app starts the daemon, hands it the peer key and a one-time
//! supervisor secret over standard input, and keeps that pipe open: when the
//! app goes away the pipe closes and the daemon stops, so a laptop accepts
//! peer requests only while this app runs. The webview never sees the key,
//! the secret or the token; it reaches the daemon through the `p2p_call`
//! command, which allows a fixed set of methods.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use nanace_p2p::ipc_client::IpcClient;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri::{AppHandle, Emitter};
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, ChildStdin, Command};

/// Name of the daemon's token; reissuing revokes every earlier one.
pub const TOKEN_NAME: &str = "nanace p2p";
pub const TOKEN_DAYS: u32 = 30;
/// Event carrying a daemon notice (consent or review request) to the UI.
pub const NOTICE_EVENT: &str = "p2p-notice";
const KEYCHAIN_SERVICE: &str = "dev.nanace.desktop";

/// What the webview may ask the daemon. Everything else is refused here,
/// before it reaches the daemon.
const WEBVIEW_METHODS: [&str; 12] = [
    "whoami",
    "activity",
    "pending",
    "settings",
    "decide_consent",
    "decide_review",
    "set_team",
    "leave_team",
    "set_trusted",
    "set_review_answers",
    "stop_all",
    "find_peers",
];

pub struct Paths {
    /// `<app config>/p2p`: token, socket, secret and the daemon's own state.
    pub dir: PathBuf,
}

impl Paths {
    pub fn token_file(&self) -> PathBuf {
        self.dir.join("p2p.token")
    }

    pub fn secret_file(&self) -> PathBuf {
        self.dir.join("ipc.secret")
    }

    fn state_file(&self) -> PathBuf {
        self.dir.join("desktop.json")
    }

    /// Where the daemon listens. A named pipe on Windows, whose name is
    /// stored so Claude's MCP server can be pointed at it.
    pub fn socket(&self, saved: &Saved) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(format!(r"\\.\pipe\nanace-p2p-{}", saved.pipe_id))
        } else {
            self.dir.join("p2p.sock")
        }
    }
}

/// What this app remembers about P2P between runs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Saved {
    /// The user turned P2P on for `server_url`.
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub server_url: String,
    /// When the daemon's token was last issued or rotated (Unix seconds).
    #[serde(default)]
    pub token_at: Option<u64>,
    #[serde(default)]
    pipe_id: String,
}

impl Saved {
    pub fn load(paths: &Paths) -> Self {
        let mut saved: Self = std::fs::read(paths.state_file())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        if saved.pipe_id.is_empty() {
            saved.pipe_id = random_secret().map(|s| s[..16].to_owned()).unwrap_or_default();
        }
        saved
    }

    pub fn save(&self, paths: &Paths) -> Result<(), String> {
        private_dir(&paths.dir)?;
        let bytes = serde_json::to_vec_pretty(self).map_err(|err| err.to_string())?;
        std::fs::write(paths.state_file(), bytes).map_err(|err| format!("cannot save P2P settings: {err}"))
    }
}

fn private_dir(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|err| format!("cannot create {}: {err}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|err| format!("cannot protect {}: {err}", dir.display()))?;
    }
    Ok(())
}

/// Stores the daemon's token readable by the user only.
pub fn write_token(paths: &Paths, token: &str) -> Result<(), String> {
    use std::io::Write;
    private_dir(&paths.dir)?;
    let path = paths.token_file();
    let tmp = path.with_extension("token.tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(&tmp)
        .and_then(|mut file| file.write_all(token.as_bytes()))
        .and_then(|()| std::fs::rename(&tmp, &path))
        .map_err(|err| format!("cannot write {}: {err}", path.display()))
}

pub fn read_token(paths: &Paths) -> Option<String> {
    let token = std::fs::read_to_string(paths.token_file()).ok()?;
    Some(token.trim().to_owned()).filter(|t| !t.is_empty())
}

pub fn remove_token(paths: &Paths) {
    let _ = std::fs::remove_file(paths.token_file());
}

/// 32 random bytes, base64url (43 characters).
fn random_secret() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|err| format!("system RNG failed: {err}"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// The peer key for `server_url`, kept in the OS keychain and created on
/// first use. One key per server: a peer id is bound to one account.
fn peer_key(server_url: &str) -> Result<String, String> {
    let entry = keyring::Entry::new(KEYCHAIN_SERVICE, &format!("p2p-peer-key {server_url}"))
        .map_err(|err| format!("keychain unavailable: {err}"))?;
    match entry.get_password() {
        Ok(key) => Ok(key),
        Err(keyring::Error::NoEntry) => {
            let key = random_secret()?;
            entry
                .set_password(&key)
                .map_err(|err| format!("cannot store the peer key in the keychain: {err}"))?;
            Ok(key)
        }
        Err(err) => Err(format!("cannot read the peer key from the keychain: {err}")),
    }
}

/// Forgets the peer key, so turning P2P on again starts with a new peer id.
pub fn forget_peer_key(server_url: &str) {
    if let Ok(entry) = keyring::Entry::new(KEYCHAIN_SERVICE, &format!("p2p-peer-key {server_url}")) {
        let _ = entry.delete_credential();
    }
}

/// The daemon shipped next to the app executable (Tauri sidecar).
pub fn binary() -> Option<PathBuf> {
    let name = if cfg!(windows) { "nanace-p2p.exe" } else { "nanace-p2p" };
    let path = std::env::current_exe().ok()?.with_file_name(name);
    path.is_file().then_some(path)
}

/// Environment that points Claude's MCP server at this daemon.
pub fn mcp_env(paths: &Paths, saved: &Saved) -> [(String, String); 3] {
    [
        ("NANACE_ALLOW_P2P".to_owned(), "true".to_owned()),
        (
            "NANACE_P2P_SOCKET".to_owned(),
            paths.socket(saved).display().to_string(),
        ),
        (
            "NANACE_P2P_SECRET_FILE".to_owned(),
            paths.secret_file().display().to_string(),
        ),
    ]
}

struct Running {
    child: Child,
    /// Held open for the daemon's lifetime: closing it stops the daemon.
    _stdin: ChildStdin,
    calls: IpcClient,
}

#[derive(Default)]
pub struct P2p {
    running: tokio::sync::Mutex<Option<Running>>,
}

impl P2p {
    pub async fn is_running(&self) -> bool {
        let mut running = self.running.lock().await;
        // Exited on its own (crashed, or the token was refused)?
        let exited = running
            .as_mut()
            .is_some_and(|daemon| !matches!(daemon.child.try_wait(), Ok(None)));
        if exited {
            *running = None;
        }
        running.is_some()
    }

    /// Starts the daemon for `server_url` and attaches to it as the desktop
    /// app. Does nothing when it already runs.
    pub async fn start(&self, app: &AppHandle, paths: &Paths, server_url: &str, bootstrap: &str) -> Result<(), String> {
        if self.is_running().await {
            return Ok(());
        }
        let binary = binary().ok_or_else(|| {
            "The P2P daemon is missing next to the app (build it with `make desktop-deps`).".to_owned()
        })?;
        let saved = Saved::load(paths);
        private_dir(&paths.dir)?;
        let socket = paths.socket(&saved);
        let supervisor_secret = random_secret()?;
        let handshake = json!({ "supervisor_secret": supervisor_secret, "peer_key": peer_key(server_url)? });

        let mut child = Command::new(binary)
            .arg("daemon")
            .env("NANACE_API_URL", server_url)
            .env("NANACE_TOKEN_FILE", paths.token_file())
            .env("NANACE_P2P_DIR", paths.dir.join("daemon"))
            .env("NANACE_P2P_BOOTSTRAP", bootstrap)
            .env("NANACE_P2P_KIND", "desktop")
            .env("NANACE_P2P_SOCKET", &socket)
            .env("NANACE_P2P_SECRET_FILE", paths.secret_file())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .map_err(|err| format!("cannot start the P2P daemon: {err}"))?;
        let mut stdin = child.stdin.take().ok_or("the P2P daemon has no standard input")?;
        stdin
            .write_all(format!("{handshake}\n").as_bytes())
            .await
            .map_err(|err| format!("cannot reach the P2P daemon: {err}"))?;

        // The socket appears once the daemon has identified itself to the
        // server and bound its peer id.
        let mut attached = None;
        for _ in 0..75 {
            tokio::time::sleep(Duration::from_millis(200)).await;
            if !matches!(child.try_wait(), Ok(None)) {
                return Err(
                    "the P2P daemon stopped while starting: check that the server is reachable and that P2P is \
                     allowed for this account"
                        .to_owned(),
                );
            }
            if let Ok(client) = IpcClient::connect(&socket, &supervisor_secret, None, &[]).await {
                attached = Some(client);
                break;
            }
        }
        let calls = attached.ok_or("the P2P daemon did not answer in time")?;
        // A second session only listens, so a prompt arrives while a call
        // is in progress.
        if let Ok(mut notices) = IpcClient::connect(&socket, &supervisor_secret, None, &[]).await {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                while let Ok(notice) = notices.next_notice().await {
                    let _ = app.emit(NOTICE_EVENT, notice);
                }
            });
        }
        *self.running.lock().await = Some(Running {
            child,
            _stdin: stdin,
            calls,
        });
        Ok(())
    }

    /// Stops the daemon. Closing its input lets it withdraw presence first;
    /// it is killed if that takes too long.
    pub async fn stop(&self) {
        let Some(Running {
            mut child,
            _stdin,
            calls,
        }) = self.running.lock().await.take()
        else {
            return;
        };
        drop(calls);
        drop(_stdin);
        if tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .is_err()
        {
            let _ = child.kill().await;
        }
    }

    /// One call from the webview to the daemon.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        if !WEBVIEW_METHODS.contains(&method) {
            return Err(format!("`{method}` is not available to the app's pages"));
        }
        let mut running = self.running.lock().await;
        let daemon = running.as_mut().ok_or("P2P is not running")?;
        daemon.calls.call(method, params).await.map_err(|err| err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_file_is_private_and_settings_round_trip() {
        let mut dir = std::env::temp_dir();
        dir.push(format!("nanace-p2p-desktop-{}", std::process::id()));
        let paths = Paths { dir: dir.clone() };
        assert!(read_token(&paths).is_none());
        write_token(&paths, "nnc_pat_first").expect("write");
        write_token(&paths, "nnc_pat_second\n").expect("replace");
        assert_eq!(read_token(&paths).as_deref(), Some("nnc_pat_second"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&paths.token_file()), 0o600);
            assert_eq!(mode(&dir), 0o700);
        }

        let mut saved = Saved::load(&paths);
        assert!(!saved.enabled && !saved.pipe_id.is_empty());
        saved.enabled = true;
        saved.server_url = "https://kb.example.com".to_owned();
        saved.token_at = Some(42);
        saved.save(&paths).expect("save");
        let again = Saved::load(&paths);
        assert!(again.enabled);
        assert_eq!(again.pipe_id, saved.pipe_id, "the pipe name is stable across runs");
        assert_eq!(paths.socket(&again), paths.socket(&saved));

        // Claude's MCP server gets paths only, never a secret.
        let env = mcp_env(&paths, &again);
        assert!(env.iter().all(|(_, value)| !value.contains("nnc_pat_")));
        assert_eq!(env[0], ("NANACE_ALLOW_P2P".to_owned(), "true".to_owned()));

        remove_token(&paths);
        assert!(read_token(&paths).is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn webview_reaches_only_listed_daemon_methods() {
        // Agent-side methods would let page JavaScript send or answer
        // messages as the user; they stay with Claude's MCP server.
        for method in ["ask", "reply", "inbox", "set_status", "hello", "get_reply"] {
            assert!(!WEBVIEW_METHODS.contains(&method), "{method}");
        }
        for method in ["decide_consent", "stop_all", "set_team"] {
            assert!(WEBVIEW_METHODS.contains(&method), "{method}");
        }
    }
}
