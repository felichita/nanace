//! How a message reaches another daemon. The daemon's rules do not depend on
//! libp2p: they talk to this trait, which the network layer implements and
//! tests replace with an in-memory link.

use async_trait::async_trait;
use serde::Serialize;

use crate::envelope::{Ack, Wire};

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// No connection could be made, directly or through the relay. Worth
    /// retrying until the message's deadline.
    #[error("peer is unreachable: {0}")]
    Unreachable(String),
}

/// One live connection, for the activity screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Connection {
    pub peer_id: String,
    /// True when the connection goes through the relay node.
    pub relayed: bool,
}

#[async_trait]
pub trait Transport: Send + Sync {
    /// Delivers `wire` to `peer_id` and returns that daemon's acknowledgement.
    /// `addrs` are addresses the registry lists for the peer; the relay is
    /// always tried as well.
    async fn send(&self, peer_id: &str, addrs: &[String], wire: Wire) -> Result<Ack, TransportError>;
    /// Closes any connection to `peer_id`.
    async fn disconnect(&self, peer_id: &str);
    /// Peers connected right now, the relay node excluded.
    async fn connections(&self) -> Vec<Connection>;
    /// Addresses this daemon can be dialed at directly, for its presence.
    async fn listen_addrs(&self) -> Vec<String>;
}
