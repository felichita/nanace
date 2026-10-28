//! Registers the bundled `nanace-mcp` server with Claude Code and Claude
//! Desktop.
//!
//! Claude Code is configured through its CLI (`claude mcp add --scope user`),
//! never by editing `~/.claude.json` directly. Claude Desktop has no CLI, so
//! its `claude_desktop_config.json` is merged in place, keeping every other
//! setting. Each app gets its own token: read-scoped, or write-scoped with
//! `NANACE_ALLOW_WRITES=true` when the user lets Claude create and edit pages.
//!
//! The token itself never appears in a config file or on a command line: it
//! lives in a file only the user can read (`<app config>/claude/*.token`), and
//! the configs carry `NANACE_TOKEN_FILE`. `nanace-mcp` re-reads that file on
//! every request, so rotating a token only rewrites the file.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

pub const SERVER_NAME: &str = "nanace";

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    Code,
    Desktop,
}

impl Target {
    pub fn token_name(self) -> &'static str {
        match self {
            Self::Code => "Claude Code (MCP)",
            Self::Desktop => "Claude Desktop (MCP)",
        }
    }
}

#[derive(Serialize)]
pub struct TargetStatus {
    /// The CLI (Code) or the app (Desktop) was found.
    pub installed: bool,
    pub connected: bool,
    /// Server URL in the existing entry, if connected.
    pub api_url: Option<String>,
    /// Claude may create and edit pages through this connection.
    pub writes: bool,
    /// When the token was last issued or rotated by this app (Unix seconds).
    pub rotated_at: Option<u64>,
}

#[derive(Serialize)]
pub struct ClaudeStatus {
    pub code: TargetStatus,
    pub desktop: TargetStatus,
    /// Path of the bundled MCP server, if present.
    pub mcp_binary: Option<String>,
}

pub struct Paths {
    pub home: PathBuf,
    /// Directory holding `Claude/claude_desktop_config.json`.
    pub config_dir: PathBuf,
    /// This app's directory for the token files.
    pub token_dir: PathBuf,
}

impl Paths {
    pub fn token_file(&self, target: Target) -> PathBuf {
        self.token_dir.join(match target {
            Target::Code => "claude-code.token",
            Target::Desktop => "claude-desktop.token",
        })
    }

    fn code_config(&self) -> PathBuf {
        self.home.join(".claude.json")
    }

    fn desktop_config(&self) -> PathBuf {
        self.config_dir.join("Claude").join("claude_desktop_config.json")
    }

    fn desktop_installed(&self) -> bool {
        let apps = [
            PathBuf::from("/Applications/Claude.app"),
            self.home.join("Applications/Claude.app"),
        ];
        apps.iter().any(|p| p.exists()) || self.config_dir.join("Claude").is_dir()
    }

    /// The `claude` CLI. GUI apps on macOS get a minimal PATH, so the usual
    /// install locations are checked as well.
    pub fn claude_cli(&self) -> Option<PathBuf> {
        let mut candidates: Vec<PathBuf> = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).map(|d| d.join("claude")).collect())
            .unwrap_or_default();
        candidates.extend([
            self.home.join(".local/bin/claude"),
            self.home.join(".claude/local/claude"),
            PathBuf::from("/opt/homebrew/bin/claude"),
            PathBuf::from("/usr/local/bin/claude"),
        ]);
        candidates.into_iter().find(|p| p.is_file())
    }
}

/// The MCP server shipped next to the app executable (Tauri sidecar).
pub fn mcp_binary() -> Option<PathBuf> {
    let name = if cfg!(windows) { "nanace-mcp.exe" } else { "nanace-mcp" };
    let path = std::env::current_exe().ok()?.with_file_name(name);
    path.is_file().then_some(path)
}

fn read_json(path: &Path) -> Result<Option<Value>, String> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|err| format!("{} is not valid JSON ({err}); fix or remove it first", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(format!("cannot read {}: {err}", path.display())),
    }
}

fn entry(config: Option<&Value>) -> Option<&Value> {
    config?.get("mcpServers")?.get(SERVER_NAME)
}

fn env_value(entry: Option<&Value>, key: &str) -> Option<String> {
    entry?.get("env")?.get(key)?.as_str().map(str::to_owned)
}

fn status_of(installed: bool, config: Option<&Value>) -> TargetStatus {
    let entry = entry(config);
    TargetStatus {
        installed,
        connected: entry.is_some(),
        api_url: env_value(entry, "NANACE_API_URL"),
        writes: env_value(entry, "NANACE_ALLOW_WRITES").as_deref() == Some("true"),
        rotated_at: None,
    }
}

pub fn status(paths: &Paths, rotation: &RotationState) -> ClaudeStatus {
    let mut status = ClaudeStatus {
        code: status_of(
            paths.claude_cli().is_some(),
            read_json(&paths.code_config()).ok().flatten().as_ref(),
        ),
        desktop: status_of(
            paths.desktop_installed(),
            read_json(&paths.desktop_config()).ok().flatten().as_ref(),
        ),
        mcp_binary: mcp_binary().map(|p| p.display().to_string()),
    };
    if status.code.connected {
        status.code.rotated_at = rotation.code;
    }
    if status.desktop.connected {
        status.desktop.rotated_at = rotation.desktop;
    }
    status
}

/// Token of the current entry for `target`, so it can be revoked when the
/// entry is replaced or removed: from its token file, or inline in entries
/// written before token files existed.
pub fn existing_token(paths: &Paths, target: Target) -> Option<String> {
    let (_, env) = current_entry(paths, target)?;
    if let Some(token) = env.get("NANACE_TOKEN") {
        return Some(token.clone());
    }
    let token = fs::read_to_string(env.get("NANACE_TOKEN_FILE")?).ok()?;
    Some(token.trim().to_owned()).filter(|t| !t.is_empty())
}

/// Stores `token` in `target`'s token file (mode 0600, directory 0700) and
/// returns its path.
pub fn write_token(paths: &Paths, target: Target, token: &str) -> Result<PathBuf, String> {
    fs::create_dir_all(&paths.token_dir)
        .map_err(|err| format!("cannot create {}: {err}", paths.token_dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&paths.token_dir, fs::Permissions::from_mode(0o700));
    }
    let path = paths.token_file(target);
    let tmp = path.with_extension("token.tmp");
    write_private(&tmp, token.as_bytes()).map_err(|err| format!("cannot write {}: {err}", tmp.display()))?;
    fs::rename(&tmp, &path).map_err(|err| format!("cannot write {}: {err}", path.display()))?;
    Ok(path)
}

pub fn remove_token(paths: &Paths, target: Target) {
    let _ = fs::remove_file(paths.token_file(target));
}

/// Command and environment of the current nanace entry for `target`.
pub fn current_entry(paths: &Paths, target: Target) -> Option<(PathBuf, Env)> {
    let path = match target {
        Target::Code => paths.code_config(),
        Target::Desktop => paths.desktop_config(),
    };
    let config = read_json(&path).ok().flatten();
    let entry = entry(config.as_ref())?;
    let command = PathBuf::from(entry.get("command")?.as_str()?);
    let env = entry
        .get("env")?
        .as_object()?
        .iter()
        .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
        .collect();
    Some((command, env))
}

/// Swaps the token of an existing entry. Usually only the token file changes;
/// an entry that still carries the token inline (or points elsewhere) is
/// rewritten once to use this app's token file, keeping its other settings.
pub fn replace_token(paths: &Paths, target: Target, token: &str) -> Result<(), String> {
    let (command, mut env) = current_entry(paths, target).ok_or_else(|| "not connected".to_owned())?;
    let file = write_token(paths, target, token)?;
    let file_value = file.display().to_string();
    if env.get("NANACE_TOKEN_FILE") == Some(&file_value) && !env.contains_key("NANACE_TOKEN") {
        return Ok(());
    }
    env.remove("NANACE_TOKEN");
    env.insert("NANACE_TOKEN_FILE".to_owned(), file_value);
    match target {
        Target::Code => connect_code(paths, &command, &env),
        Target::Desktop => connect_desktop(paths, &command, &env),
    }
}

/// When each target's token was last issued or rotated (Unix seconds), kept in
/// the app's config directory.
#[derive(Serialize, Deserialize, Default, Clone, Copy)]
pub struct RotationState {
    pub code: Option<u64>,
    pub desktop: Option<u64>,
}

impl RotationState {
    pub fn load(path: &Path) -> Self {
        fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|err| err.to_string())?;
        }
        fs::write(path, serde_json::to_vec(self).map_err(|err| err.to_string())?).map_err(|err| err.to_string())
    }

    pub fn get(&self, target: Target) -> Option<u64> {
        match target {
            Target::Code => self.code,
            Target::Desktop => self.desktop,
        }
    }

    pub fn set(&mut self, target: Target, at: Option<u64>) {
        match target {
            Target::Code => self.code = at,
            Target::Desktop => self.desktop = at,
        }
    }
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

pub type Env = BTreeMap<String, String>;

pub fn server_env(api_url: &str, token_file: &Path, org_id: &str, allow_writes: bool) -> Env {
    let mut env = Env::from([
        ("NANACE_API_URL".to_owned(), api_url.to_owned()),
        ("NANACE_TOKEN_FILE".to_owned(), token_file.display().to_string()),
        ("NANACE_ORG_ID".to_owned(), org_id.to_owned()),
    ]);
    if allow_writes {
        env.insert("NANACE_ALLOW_WRITES".to_owned(), "true".to_owned());
    }
    env
}

fn run_cli(cli: &Path, args: &[String]) -> Result<(), String> {
    let out = Command::new(cli)
        .args(args)
        .output()
        .map_err(|err| format!("failed to run {}: {err}", cli.display()))?;
    if out.status.success() {
        Ok(())
    } else {
        // Its output may echo the command line; keep it out of messages.
        Err(format!("`claude mcp` failed with {}", out.status))
    }
}

pub fn connect_code(paths: &Paths, binary: &Path, env: &Env) -> Result<(), String> {
    let cli = paths
        .claude_cli()
        .ok_or_else(|| "Claude Code CLI (`claude`) not found".to_owned())?;
    // Replacing an existing entry: `add` refuses duplicates.
    let _ = run_cli(&cli, &["mcp", "remove", SERVER_NAME, "-s", "user"].map(String::from));
    let mut args: Vec<String> = ["mcp", "add", SERVER_NAME, "--scope", "user"].map(String::from).into();
    for (key, value) in env {
        args.push("--env".to_owned());
        args.push(format!("{key}={value}"));
    }
    args.push("--".to_owned());
    args.push(binary.display().to_string());
    run_cli(&cli, &args)
}

pub fn disconnect_code(paths: &Paths) -> Result<(), String> {
    let cli = paths
        .claude_cli()
        .ok_or_else(|| "Claude Code CLI (`claude`) not found".to_owned())?;
    run_cli(&cli, &["mcp", "remove", SERVER_NAME, "-s", "user"].map(String::from))?;
    remove_token(paths, Target::Code);
    Ok(())
}

/// Sets (or with `None`, removes) the nanace entry in a Claude Desktop config,
/// keeping everything else.
pub fn update_desktop_config(path: &Path, server: Option<Value>) -> Result<(), String> {
    let mut config = read_json(path)?.unwrap_or_else(|| json!({}));
    let root = config
        .as_object_mut()
        .ok_or_else(|| format!("{} is not a JSON object", path.display()))?;
    let servers = root
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| "mcpServers is not a JSON object".to_owned())?;
    match server {
        Some(server) => {
            servers.insert(SERVER_NAME.to_owned(), server);
        }
        None => {
            servers.remove(SERVER_NAME);
        }
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|err| format!("cannot create {}: {err}", dir.display()))?;
    }
    let text = serde_json::to_string_pretty(&config).map_err(|err| err.to_string())? + "\n";
    // Write-then-rename so Claude never reads a half-written file.
    let tmp = path.with_extension("json.nanace-tmp");
    write_private(&tmp, text.as_bytes()).map_err(|err| format!("cannot write {}: {err}", tmp.display()))?;
    fs::rename(&tmp, path).map_err(|err| format!("cannot write {}: {err}", path.display()))
}

/// Writes a file readable only by the user (it holds a token), with no window
/// in which other users could read it.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let _ = fs::remove_file(path);
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(bytes)
}

pub fn connect_desktop(paths: &Paths, binary: &Path, env: &Env) -> Result<(), String> {
    update_desktop_config(
        &paths.desktop_config(),
        Some(json!({ "command": binary.display().to_string(), "env": env })),
    )
}

pub fn disconnect_desktop(paths: &Paths) -> Result<(), String> {
    update_desktop_config(&paths.desktop_config(), None)?;
    remove_token(paths, Target::Desktop);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_paths(tag: &str) -> Paths {
        let home = std::env::temp_dir().join(format!("nanace-claude-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        fs::create_dir_all(&home).expect("mkdir");
        Paths {
            config_dir: home.join("config"),
            token_dir: home.join("app/claude"),
            home,
        }
    }

    #[test]
    fn desktop_config_is_merged_and_removed_without_touching_other_settings() {
        let paths = temp_paths("desktop");
        let file = paths.desktop_config();
        fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
        fs::write(&file, r#"{"theme":"dark","mcpServers":{"other":{"command":"x"}}}"#).expect("write");

        let token_file = write_token(&paths, Target::Desktop, "nnc_pat_secret").expect("token");
        let env = server_env("http://127.0.0.1:8080", &token_file, "org-1", true);
        connect_desktop(&paths, Path::new("/opt/nanace-mcp"), &env).expect("connect");
        assert!(
            !fs::read_to_string(&file).expect("read").contains("nnc_pat"),
            "no token in the config"
        );
        let config = read_json(&file).expect("read").expect("exists");
        assert_eq!(config["theme"], "dark");
        assert_eq!(config["mcpServers"]["other"]["command"], "x");
        assert_eq!(config["mcpServers"]["nanace"]["command"], "/opt/nanace-mcp");
        assert_eq!(
            existing_token(&paths, Target::Desktop).as_deref(),
            Some("nnc_pat_secret")
        );
        let status = status(&paths, &RotationState::default());
        assert!(status.desktop.connected);
        assert_eq!(status.desktop.api_url.as_deref(), Some("http://127.0.0.1:8080"));
        assert!(status.desktop.writes);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| fs::metadata(p).expect("meta").permissions().mode() & 0o777;
            assert_eq!(mode(&file), 0o600);
            assert_eq!(mode(&token_file), 0o600);
            assert_eq!(mode(&paths.token_dir), 0o700);
        }

        disconnect_desktop(&paths).expect("disconnect");
        assert!(!token_file.exists(), "disconnect removes the token file");
        let config = read_json(&file).expect("read").expect("exists");
        assert!(config["mcpServers"].get("nanace").is_none());
        assert_eq!(config["mcpServers"]["other"]["command"], "x");
        let _ = fs::remove_dir_all(&paths.home);
    }

    #[test]
    fn tokens_are_replaced_in_place_and_rotation_state_round_trips() {
        let paths = temp_paths("rotate");
        // An entry from before token files, with the token inline.
        let mut legacy = server_env("http://127.0.0.1:8080", Path::new("/unused"), "org-1", true);
        legacy.remove("NANACE_TOKEN_FILE");
        legacy.insert("NANACE_TOKEN".to_owned(), "nnc_pat_old".to_owned());
        connect_desktop(&paths, Path::new("/opt/nanace-mcp"), &legacy).expect("connect");
        assert_eq!(existing_token(&paths, Target::Desktop).as_deref(), Some("nnc_pat_old"));

        // Rotation moves it to a token file, keeping the other settings.
        replace_token(&paths, Target::Desktop, "nnc_pat_new").expect("replace");
        let (command, env) = current_entry(&paths, Target::Desktop).expect("entry");
        assert_eq!(command, PathBuf::from("/opt/nanace-mcp"));
        assert!(!env.contains_key("NANACE_TOKEN"));
        assert_eq!(
            env["NANACE_TOKEN_FILE"],
            paths.token_file(Target::Desktop).display().to_string()
        );
        assert_eq!(env["NANACE_ALLOW_WRITES"], "true");
        assert_eq!(env["NANACE_ORG_ID"], "org-1");
        assert_eq!(existing_token(&paths, Target::Desktop).as_deref(), Some("nnc_pat_new"));

        // Later rotations only rewrite the file.
        let config_before = fs::read(paths.desktop_config()).expect("read");
        replace_token(&paths, Target::Desktop, "nnc_pat_newer").expect("replace");
        assert_eq!(fs::read(paths.desktop_config()).expect("read"), config_before);
        assert_eq!(
            existing_token(&paths, Target::Desktop).as_deref(),
            Some("nnc_pat_newer")
        );

        let file = paths.home.join("state/claude-rotation.json");
        let mut state = RotationState::default();
        state.set(Target::Desktop, Some(42));
        state.save(&file).expect("save");
        assert_eq!(RotationState::load(&file).get(Target::Desktop), Some(42));
        assert_eq!(RotationState::load(&file).get(Target::Code), None);
        assert_eq!(status(&paths, &state).desktop.rotated_at, Some(42));
        let _ = fs::remove_dir_all(&paths.home);
    }

    #[test]
    fn invalid_config_is_never_overwritten() {
        let paths = temp_paths("invalid");
        let file = paths.desktop_config();
        fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
        fs::write(&file, "{ not json").expect("write");
        let env = server_env("http://x", Path::new("/t"), "o", false);
        assert!(connect_desktop(&paths, Path::new("/bin/x"), &env).is_err());
        assert_eq!(fs::read_to_string(&file).expect("read"), "{ not json");
        let _ = fs::remove_dir_all(&paths.home);
    }

    #[test]
    fn code_status_reads_the_user_entry() {
        let paths = temp_paths("code");
        fs::write(
            paths.home.join(".claude.json"),
            r#"{"mcpServers":{"nanace":{"command":"/x","env":{"NANACE_API_URL":"http://a","NANACE_TOKEN":"nnc_pat_1"}}}}"#,
        )
        .expect("write");
        let status = status(&paths, &RotationState::default());
        assert!(status.code.connected);
        assert_eq!(status.code.api_url.as_deref(), Some("http://a"));
        assert_eq!(existing_token(&paths, Target::Code).as_deref(), Some("nnc_pat_1"));
        assert!(!status.code.writes);
        assert!(!status.desktop.connected);
        let _ = fs::remove_dir_all(&paths.home);
    }
}
