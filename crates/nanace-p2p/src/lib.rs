//! Peer-to-peer messaging between agents of a nanace team.
//!
//! The crate builds one binary with two modes, the network node and the
//! per-user daemon, and exposes the daemon's local IPC client to the other
//! nanace programs. Message bodies travel between daemons only; the nanace
//! server sees signed headers.

pub mod ipc_client;

#[cfg(feature = "daemon")]
pub mod daemon;
#[cfg(feature = "daemon")]
pub mod envelope;
#[cfg(feature = "daemon")]
pub mod ipc;
#[cfg(feature = "daemon")]
pub mod net;
#[cfg(feature = "daemon")]
pub mod node;
#[cfg(feature = "daemon")]
pub mod registry;
#[cfg(feature = "daemon")]
pub mod secrets;
#[cfg(feature = "daemon")]
pub mod spool;
#[cfg(feature = "daemon")]
pub mod throttle;
#[cfg(feature = "daemon")]
pub mod transport;
