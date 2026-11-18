//! MCP server exposing the nanace knowledge base to Claude.
//!
//! The server speaks MCP over stdio and calls the nanace REST API as the
//! configured user. It holds no database credentials: every tool result goes
//! through the API's RBAC, row-level security, and audit logging.

pub mod client;
pub mod p2p;
pub mod server;

pub use client::{ApiClient, Credentials};
pub use p2p::{P2pLink, P2pMode};
pub use server::NanaceMcp;
