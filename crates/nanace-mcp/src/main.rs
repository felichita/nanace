//! `nanace-mcp`: stdio MCP server for Claude Desktop / Claude Code.
//!
//! Environment:
//! - `NANACE_API_URL`: API base URL (default `http://127.0.0.1:8080`).
//! - `NANACE_TOKEN`: bearer token for the API, or
//! - `NANACE_TOKEN_FILE`: a file holding it (re-read on every request), or
//! - `NANACE_DEV_EMAIL`: log in through the API's dev-only login instead.
//! - `NANACE_ORG_ID`: default organization (optional).
//! - `NANACE_ALLOW_WRITES`: `true` adds the `create_context` / `update_context`
//!   tools (the token also needs `write` scope). Read-only by default.

use anyhow::{Context, bail};
use nanace_mcp::{ApiClient, Credentials, NanaceMcp};
use rmcp::ServiceExt;
use secrecy::SecretString;
use tracing_subscriber::EnvFilter;

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

    let server = NanaceMcp::new(ApiClient::new(&base_url, credentials)?, default_org, allow_writes);
    let service = server
        .serve(rmcp::transport::stdio())
        .await
        .context("MCP initialization failed")?;
    service.waiting().await?;
    Ok(())
}
