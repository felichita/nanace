//! nanace desktop: a Tauri shell around the React UI in `apps/desktop/src`.
//!
//! The webview never holds credentials. It calls the `api` command, which
//! attaches the personal access token kept by [`session::Session`].

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod claude;
mod loopback;
mod memory;
mod session;

use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Manager, State, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_opener::OpenerExt;
use tokio::net::TcpListener;

use session::{ApiResponse, Session, SessionInfo};

const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(300);

#[tauri::command]
async fn session_info(session: State<'_, Session>) -> Result<SessionInfo, String> {
    Ok(session.info().await)
}

#[tauri::command]
async fn set_server_url(session: State<'_, Session>, url: String) -> Result<SessionInfo, String> {
    session.set_server_url(&url).await
}

/// Google sign-in in the system browser with a loopback redirect and PKCE;
/// returns a short-lived session token.
async fn google_session(app: &AppHandle, session: &Session, server_url: &str) -> Result<String, String> {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|err| format!("failed to start the sign-in listener: {err}"))?;
    let port = listener.local_addr().map_err(|err| err.to_string())?.port();

    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret).map_err(|err| format!("no secure randomness: {err}"))?;
    let verifier = URL_SAFE_NO_PAD.encode(secret);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let login_url = url::Url::parse_with_params(
        &format!("{server_url}/api/v1/auth/google/login"),
        [("redirect_port", port.to_string()), ("code_challenge", challenge)],
    )
    .map_err(|err| err.to_string())?;
    app.opener()
        .open_url(login_url.as_str(), None::<&str>)
        .map_err(|err| format!("failed to open the browser: {err}"))?;

    let code = loopback::receive_code(listener, SIGN_IN_TIMEOUT).await?;
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.set_focus();
    }
    session.exchange_login_code(server_url, &code, &verifier).await
}

#[tauri::command]
async fn sign_in_google(app: AppHandle, session: State<'_, Session>) -> Result<SessionInfo, String> {
    let server_url = session.server_url().await;
    if !session.google_enabled(&server_url).await? {
        return Err("Google sign-in is not configured on this server.".to_owned());
    }
    let session_token = google_session(&app, &session, &server_url).await?;
    session.finish_sign_in(&server_url, &session_token).await
}

/// Unverified email sign-in, available only when the server enables
/// `NANACE_DEV_LOGIN`.
#[tauri::command]
async fn sign_in_dev(session: State<'_, Session>, email: String) -> Result<SessionInfo, String> {
    let server_url = session.server_url().await;
    let session_token = session.dev_login(&server_url, &email).await?;
    session.finish_sign_in(&server_url, &session_token).await
}

#[tauri::command]
async fn sign_out(session: State<'_, Session>) -> Result<SessionInfo, String> {
    Ok(session.sign_out().await)
}

#[tauri::command]
async fn api(
    session: State<'_, Session>,
    method: String,
    path: String,
    body: Option<Value>,
) -> Result<ApiResponse, String> {
    session.request(&method, &path, body).await
}

fn claude_paths(app: &AppHandle) -> Result<claude::Paths, String> {
    Ok(claude::Paths {
        home: app.path().home_dir().map_err(|err| err.to_string())?,
        config_dir: app.path().config_dir().map_err(|err| err.to_string())?,
    })
}

#[tauri::command]
fn claude_status(app: AppHandle) -> Result<claude::ClaudeStatus, String> {
    Ok(claude::status(&claude_paths(&app)?))
}

/// Gives Claude Code and/or Claude Desktop read access to `org_id` through the
/// bundled MCP server. Creating tokens needs a fresh interactive sign-in:
/// Google when the server has it, otherwise dev email login as `email`.
#[tauri::command]
async fn connect_claude(
    app: AppHandle,
    session: State<'_, Session>,
    targets: Vec<claude::Target>,
    org_id: String,
    email: String,
) -> Result<claude::ClaudeStatus, String> {
    if targets.is_empty() {
        return Err("choose Claude Code or Claude Desktop".to_owned());
    }
    if org_id.len() != 36 || !org_id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        return Err("invalid workspace id".to_owned());
    }
    let binary = claude::mcp_binary()
        .ok_or_else(|| "The MCP server is missing next to the app (build it with `make desktop-deps`).".to_owned())?;
    let paths = claude_paths(&app)?;
    let server_url = session.server_url().await;
    let session_token = if session.google_enabled(&server_url).await? {
        google_session(&app, &session, &server_url).await?
    } else {
        session.dev_login(&server_url, &email).await?
    };

    for target in targets {
        let previous = claude::existing_token(&paths, target);
        let token = session
            .create_token(&server_url, &session_token, target.token_name(), "read", 365)
            .await?;
        let env = claude::server_env(&server_url, &token, &org_id);
        let written = match target {
            claude::Target::Code => claude::connect_code(&paths, &binary, &env),
            claude::Target::Desktop => claude::connect_desktop(&paths, &binary, &env),
        };
        if let Err(err) = written {
            session.revoke_token(&server_url, &token).await;
            return Err(err);
        }
        // The replaced entry's token is no longer used anywhere.
        if let Some(old) = previous.filter(|old| *old != token) {
            session.revoke_token(&server_url, &old).await;
        }
    }
    Ok(claude::status(&paths))
}

#[tauri::command]
async fn disconnect_claude(
    app: AppHandle,
    session: State<'_, Session>,
    target: claude::Target,
) -> Result<claude::ClaudeStatus, String> {
    let paths = claude_paths(&app)?;
    let before = claude::status(&paths);
    let api_url = match target {
        claude::Target::Code => before.code.api_url,
        claude::Target::Desktop => before.desktop.api_url,
    }
    .unwrap_or(session.server_url().await);
    let token = claude::existing_token(&paths, target);
    match target {
        claude::Target::Code => claude::disconnect_code(&paths)?,
        claude::Target::Desktop => claude::disconnect_desktop(&paths)?,
    }
    if let Some(token) = token {
        session.revoke_token(&api_url, &token).await;
    }
    Ok(claude::status(&paths))
}

/// Claude Code memory files for the import dialog (read-only, fixed location).
#[tauri::command]
async fn scan_claude_memory(app: AppHandle) -> Result<Vec<memory::MemoryProject>, String> {
    let home = app.path().home_dir().map_err(|err| err.to_string())?;
    tauri::async_runtime::spawn_blocking(move || memory::scan(&home))
        .await
        .map_err(|err| err.to_string())?
}

fn is_external(url: &url::Url) -> bool {
    matches!(url.scheme(), "http" | "https" | "mailto") && !is_app_url(url)
}

/// Pages that belong to the app itself; everything else is opened in the
/// system browser instead of navigating the webview.
fn is_app_url(url: &url::Url) -> bool {
    match url.scheme() {
        "tauri" => true,
        "http" | "https" => {
            url.host_str() == Some("tauri.localhost")
                || (cfg!(debug_assertions) && url.host_str() == Some("localhost") && url.port() == Some(1420))
        }
        _ => false,
    }
}

#[tauri::command]
fn open_external(app: AppHandle, url: String) -> Result<(), String> {
    let parsed = url::Url::parse(&url).map_err(|_| "invalid URL".to_owned())?;
    if !is_external(&parsed) {
        return Err("only http, https and mailto links open externally".to_owned());
    }
    app.opener()
        .open_url(parsed.as_str(), None::<&str>)
        .map_err(|err| err.to_string())
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let settings = app.path().app_config_dir()?.join("settings.json");
            app.manage(Session::load(settings)?);

            let handle = app.handle().clone();
            let builder = WebviewWindowBuilder::new(app, "main", WebviewUrl::default())
                .title("nanace")
                .inner_size(1280.0, 820.0)
                .min_inner_size(860.0, 560.0)
                .on_navigation(move |url| {
                    if is_app_url(url) {
                        return true;
                    }
                    if is_external(url) {
                        let _ = handle.opener().open_url(url.as_str(), None::<&str>);
                    }
                    false
                });
            #[cfg(target_os = "macos")]
            let builder = builder
                .title_bar_style(tauri::TitleBarStyle::Overlay)
                .hidden_title(true);
            builder.build()?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            session_info,
            set_server_url,
            sign_in_google,
            sign_in_dev,
            sign_out,
            api,
            open_external,
            scan_claude_memory,
            claude_status,
            connect_claude,
            disconnect_claude
        ])
        .run(tauri::generate_context!())
        .unwrap_or_else(|err| {
            eprintln!("nanace desktop failed to start: {err}");
            std::process::exit(1);
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_web_links_leave_the_app() {
        let url = |s: &str| url::Url::parse(s).expect("url");
        assert!(is_app_url(&url("tauri://localhost/index.html")));
        assert!(is_app_url(&url("http://tauri.localhost/")));
        assert!(is_external(&url("https://docs.rs/tauri")));
        assert!(is_external(&url("mailto:a@b.c")));
        assert!(!is_external(&url("file:///etc/passwd")));
        assert!(!is_external(&url("javascript:alert(1)")));
        assert!(!is_app_url(&url("https://evil.test/")));
    }
}
