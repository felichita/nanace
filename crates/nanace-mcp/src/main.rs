//! `nanace-mcp`: stdio MCP server for Claude Desktop / Claude Code.
//!
//! Environment:
//! - `NANACE_API_URL`: API base URL (default `http://127.0.0.1:8080`).
//! - `NANACE_TOKEN`: bearer token for the API, or
//! - `NANACE_TOKEN_FILE`: a file holding it (re-read on every request), or
//! - `NANACE_DEV_EMAIL`: log in through the API's dev-only login instead.
//! - `NANACE_ORG_ID`: default organization (optional).
//! - `NANACE_ALLOW_P2P`: `true` adds the `p2p_*` tools when the user's P2P
//!   daemon answers on `NANACE_P2P_SOCKET` with the secret in
//!   `NANACE_P2P_SECRET_FILE`. `NANACE_P2P_AGENT` names this session's agent
//!   and `NANACE_P2P_CAPABILITIES` (comma separated) what it announces.
//! - `NANACE_ALLOW_WRITES`: `true` adds the `create_context` / `update_context`
//!   tools (the token also needs `write` scope). Read-only by default.

use anyhow::{Context, bail};
use nanace_mcp::{ApiClient, Credentials, NanaceMcp, P2pLink, P2pMode};
use rmcp::ServiceExt;
use secrecy::SecretString;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // stdout carries the MCP protocol; logs must go to stderr.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")))
        .init();

    let base_url = std::env::var("NANACE_API_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_owned());
    let var = |key: &str| std::env::var(key).ok().filter(|v| !v.trim().is_empty());
    let credentials = if let Some(token) = var("NANACE_TOKEN") {
        Credentials::Token(SecretString::from(token.trim().to_owned()))
    } else if let Some(path) = var("NANACE_TOKEN_FILE") {
        Credentials::TokenFile(path.trim().into())
    } else if let Some(email) = var("NANACE_DEV_EMAIL") {
        Credentials::DevEmail(email.trim().to_owned())
    } else {
        bail!("set NANACE_TOKEN or NANACE_TOKEN_FILE (or NANACE_DEV_EMAIL for a development API)")
    };
    let default_org = std::env::var("NANACE_ORG_ID")
        .ok()
        .map(|id| id.trim().parse())
        .transpose()
        .context("NANACE_ORG_ID must be a UUID")?;

    let allow_writes = std::env::var("NANACE_ALLOW_WRITES").is_ok_and(|v| matches!(v.trim(), "true" | "1"));

    // P2P tools exist only when asked for and the user's daemon answers.
    let p2p =
        if std::env::var("NANACE_ALLOW_P2P").is_ok_and(|v| matches!(v.trim(), "true" | "1")) {
            match (var("NANACE_P2P_SOCKET"), var("NANACE_P2P_SECRET_FILE")) {
            (Some(socket), Some(secret_file)) => {
                // Unique per session, so two sessions on one machine are two agents.
                let label = var("NANACE_P2P_AGENT")
                    .unwrap_or_else(|| format!("claude/{}", &Uuid::new_v4().simple().to_string()[..8]));
                let capabilities = var("NANACE_P2P_CAPABILITIES")
                    .map(|c| c.split(',').map(|c| c.trim().to_owned()).filter(|c| !c.is_empty()).collect())
                    .unwrap_or_default();
                match P2pLink::connect(socket.trim().into(), secret_file.trim().into(), label, capabilities).await {
                    Ok(link) => P2pMode::On(Box::new(link)),
                    Err(err) => P2pMode::Unavailable(format!(
                        "the nanace P2P daemon did not answer ({err}). The user turns P2P on in the nanace app."
                    )),
                }
            }
            _ => P2pMode::Unavailable(
                "NANACE_P2P_SOCKET and NANACE_P2P_SECRET_FILE are not set. The user turns P2P on in the nanace app."
                    .to_owned(),
            ),
        }
        } else {
            P2pMode::Off
        };

    let server = NanaceMcp::with_p2p(ApiClient::new(&base_url, credentials)?, default_org, allow_writes, p2p);
    let service = server
        .serve(rmcp::transport::stdio())
        .await
        .context("MCP initialization failed")?;
    service.waiting().await?;
    Ok(())
}
