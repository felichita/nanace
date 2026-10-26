//! nanace HTTP API: multi-tenant markdown knowledge base with team RBAC and
//! semantic search.

pub mod access;
pub mod api;
pub mod audit;
pub mod auth;
pub mod config;
pub mod db;
pub mod error;
pub mod vectors;

use std::sync::Arc;

use nanace_embed::Embedder;
use sqlx::PgPool;

use crate::auth::JwtKeys;
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
}
