//! nanace HTTP API: multi-tenant markdown knowledge base with team RBAC and
//! semantic search.

pub mod access;
pub mod api;
pub mod audit;
pub mod auth;
pub mod config;
pub mod db;
pub mod error;
pub mod metrics;
pub mod oauth;
pub mod reindex;
pub mod secret;
pub mod upgrades;
pub mod vectors;

use std::sync::Arc;

use nanace_embed::Embedder;
use sqlx::PgPool;

use crate::auth::JwtKeys;
use crate::oauth::GoogleOAuth;
use crate::vectors::VectorStore;

pub const LOG_COMPONENT: &str = "nanace_server";

/// Chunk size for embedding input; ~375 tokens, under bge-small's 512 limit.
pub const CHUNK_MAX_BYTES: usize = 1500;

pub struct AppState {
    pub pool: PgPool,
    pub vectors: VectorStore,
    pub embedder: Arc<dyn Embedder>,
    pub jwt: JwtKeys,
    pub dev_login: bool,
    /// `None` when Google sign-in is not configured.
    pub google: Option<GoogleOAuth>,
    /// Browser destination after sign-in.
    pub frontend_url: String,
    /// Set `Secure` on cookies; true when the public URL is HTTPS.
    pub secure_cookies: bool,
}
