//! `nanace-mcp`: stdio MCP server for Claude Desktop / Claude Code.
//!
//! Environment:
//! - `NANACE_API_URL`: API base URL (default `http://127.0.0.1:8080`).
//! - `NANACE_TOKEN`: bearer token for the API, or
//! - `NANACE_DEV_EMAIL`: log in through the API's dev-only login instead.
//! - `NANACE_ORG_ID`: default organization (optional).

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
    let credentials = match (
        std::env::var("NANACE_TOKEN").ok(),
        std::env::var("NANACE_DEV_EMAIL").ok(),
    ) {
        (Some(token), _) if !token.trim().is_empty() => Credentials::Token(SecretString::from(token.trim().to_owned())),
        (_, Some(email)) if !email.trim().is_empty() => Credentials::DevEmail(email.trim().to_owned()),
        _ => bail!("set NANACE_TOKEN (or NANACE_DEV_EMAIL for a development API)"),
    };
    let default_org = std::env::var("NANACE_ORG_ID")
        .ok()
        .map(|id| id.trim().parse())
        .transpose()
        .context("NANACE_ORG_ID must be a UUID")?;

    let server = NanaceMcp::new(ApiClient::new(&base_url, credentials)?, default_org);
    let service = server
        .serve(rmcp::transport::stdio())
        .await
        .context("MCP initialization failed")?;
    service.waiting().await?;
    Ok(())
}
