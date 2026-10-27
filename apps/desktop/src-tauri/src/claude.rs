//! Registers the bundled `nanace-mcp` server with Claude Code and Claude
//! Desktop.
//!
//! Claude Code is configured through its CLI (`claude mcp add --scope user`),
//! never by editing `~/.claude.json` directly. Claude Desktop has no CLI, so
//! its `claude_desktop_config.json` is merged in place, keeping every other
//! setting. Each app gets its own read-scoped token.

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
}

impl Paths {
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
    }
}

pub fn status(paths: &Paths) -> ClaudeStatus {
    ClaudeStatus {
        code: status_of(
            paths.claude_cli().is_some(),
            read_json(&paths.code_config()).ok().flatten().as_ref(),
        ),
        desktop: status_of(
            paths.desktop_installed(),
            read_json(&paths.desktop_config()).ok().flatten().as_ref(),
        ),
        mcp_binary: mcp_binary().map(|p| p.display().to_string()),
    }
}

/// Token in the current entry for `target`, so it can be revoked when the
/// entry is replaced or removed.
pub fn existing_token(paths: &Paths, target: Target) -> Option<String> {
    let path = match target {
        Target::Code => paths.code_config(),
        Target::Desktop => paths.desktop_config(),
    };
    let config = read_json(&path).ok().flatten();
    env_value(entry(config.as_ref()), "NANACE_TOKEN")
}

pub fn server_env(api_url: &str, token: &str, org_id: &str) -> BTreeMap<&'static str, String> {
    BTreeMap::from([
        ("NANACE_API_URL", api_url.to_owned()),
        ("NANACE_TOKEN", token.to_owned()),
        ("NANACE_ORG_ID", org_id.to_owned()),
    ])
}

fn run_cli(cli: &Path, args: &[String]) -> Result<(), String> {
    let out = Command::new(cli)
        .args(args)
        .output()
        .map_err(|err| format!("failed to run {}: {err}", cli.display()))?;
    if out.status.success() {
        Ok(())
    } else {
        // The CLI echoes the command on errors, which includes the token.
        Err(format!("`claude mcp` failed with {}", out.status))
    }
}

pub fn connect_code(paths: &Paths, binary: &Path, env: &BTreeMap<&'static str, String>) -> Result<(), String> {
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
    run_cli(&cli, &["mcp", "remove", SERVER_NAME, "-s", "user"].map(String::from))
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
    fs::write(&tmp, text).map_err(|err| format!("cannot write {}: {err}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // The file holds a token.
        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
    }
    fs::rename(&tmp, path).map_err(|err| format!("cannot write {}: {err}", path.display()))
}

pub fn connect_desktop(paths: &Paths, binary: &Path, env: &BTreeMap<&'static str, String>) -> Result<(), String> {
    update_desktop_config(
        &paths.desktop_config(),
        Some(json!({ "command": binary.display().to_string(), "env": env })),
    )
}

pub fn disconnect_desktop(paths: &Paths) -> Result<(), String> {
    update_desktop_config(&paths.desktop_config(), None)
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
            home,
        }
    }

    #[test]
    fn desktop_config_is_merged_and_removed_without_touching_other_settings() {
        let paths = temp_paths("desktop");
        let file = paths.desktop_config();
        fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
        fs::write(&file, r#"{"theme":"dark","mcpServers":{"other":{"command":"x"}}}"#).expect("write");

        let env = server_env("http://127.0.0.1:8080", "nnc_pat_secret", "org-1");
        connect_desktop(&paths, Path::new("/opt/nanace-mcp"), &env).expect("connect");
        let config = read_json(&file).expect("read").expect("exists");
        assert_eq!(config["theme"], "dark");
        assert_eq!(config["mcpServers"]["other"]["command"], "x");
        assert_eq!(config["mcpServers"]["nanace"]["command"], "/opt/nanace-mcp");
        assert_eq!(
            existing_token(&paths, Target::Desktop).as_deref(),
            Some("nnc_pat_secret")
        );
        let status = status(&paths);
        assert!(status.desktop.connected);
        assert_eq!(status.desktop.api_url.as_deref(), Some("http://127.0.0.1:8080"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&file).expect("meta").permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        disconnect_desktop(&paths).expect("disconnect");
        let config = read_json(&file).expect("read").expect("exists");
        assert!(config["mcpServers"].get("nanace").is_none());
        assert_eq!(config["mcpServers"]["other"]["command"], "x");
        let _ = fs::remove_dir_all(&paths.home);
    }

    #[test]
    fn invalid_config_is_never_overwritten() {
        let paths = temp_paths("invalid");
        let file = paths.desktop_config();
        fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
        fs::write(&file, "{ not json").expect("write");
        let env = server_env("http://x", "t", "o");
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
        let status = status(&paths);
        assert!(status.code.connected);
        assert_eq!(status.code.api_url.as_deref(), Some("http://a"));
        assert_eq!(existing_token(&paths, Target::Code).as_deref(), Some("nnc_pat_1"));
        assert!(!status.desktop.connected);
        let _ = fs::remove_dir_all(&paths.home);
    }
}
