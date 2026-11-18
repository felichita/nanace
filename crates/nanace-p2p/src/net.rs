//! The daemon's libp2p side: QUIC and TCP with Noise, a relay client for
//! peers behind NAT, hole punching (DCUtR), and the three message
//! protocols. The network is closed by an allow list, not a shared key: a
//! connection is kept only with the relay node and with peers the registry
//! vouches for.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use futures_util::StreamExt;
use libp2p::allow_block_list;
use libp2p::multiaddr::Protocol;
use libp2p::request_response::{self, OutboundRequestId, ProtocolSupport, ResponseChannel};
use libp2p::swarm::SwarmEvent;
use libp2p::{Multiaddr, PeerId, StreamProtocol, Swarm, dcutr, identify, noise, ping, tcp, yamux};
use nanace_core::p2p::{MessageKind, RejectReason};
use serde::Deserialize;
use tokio::sync::{mpsc, oneshot};

use crate::daemon::Daemon;
use crate::envelope::{Ack, Identity, Wire};
use crate::transport::{Connection, Transport, TransportError};

pub const ASK_PROTOCOL: &str = "/nanace/p2p/ask/1.0.0";
pub const STATUS_PROTOCOL: &str = "/nanace/p2p/status/1.0.0";
pub const NOTIFY_PROTOCOL: &str = "/nanace/p2p/notify/1.0.0";
pub const IDENTIFY_PROTOCOL: &str = "/nanace/p2p/1.0.0";

/// How long the other daemon has to acknowledge a message. The answer to a
/// question is a separate message, so this is short.
const ACK_TIMEOUT: Duration = Duration::from_secs(20);
/// How often the connection to the relay node is checked and restored.
const BOOTSTRAP_INTERVAL: Duration = Duration::from_secs(5);

type Messages = request_response::json::Behaviour<Wire, Ack>;

// In a module of its own so that the `allow` covers nothing but the derive's
// output: `allow_block_list::Behaviour` emits `Infallible`, and compilers
// from 1.100 on report the generated code that forwards its events as
// unreachable.
#[allow(unreachable_code)]
mod behaviour {
    use libp2p::allow_block_list::{self, AllowedPeers};
    use libp2p::swarm::NetworkBehaviour;
    use libp2p::{dcutr, identify, ping, relay};

    use super::Messages;

    #[derive(NetworkBehaviour)]
    pub(super) struct Behaviour {
        pub(super) allowed: allow_block_list::Behaviour<AllowedPeers>,
        pub(super) relay: relay::client::Behaviour,
        pub(super) dcutr: dcutr::Behaviour,
        pub(super) identify: identify::Behaviour,
        pub(super) ping: ping::Behaviour,
        pub(super) ask: Messages,
        pub(super) status: Messages,
        pub(super) notify: Messages,
    }
}
use behaviour::{Behaviour, BehaviourEvent};

fn messages(protocol: &'static str) -> Messages {
    Messages::new(
        [(StreamProtocol::new(protocol), ProtocolSupport::Full)],
        request_response::Config::default().with_request_timeout(ACK_TIMEOUT),
    )
}

enum Command {
    Send {
        peer: PeerId,
        addrs: Vec<Multiaddr>,
        kind: MessageKind,
        wire: Wire,
        done: oneshot::Sender<Result<Ack, TransportError>>,
    },
    Disconnect(PeerId),
    Connections(oneshot::Sender<Vec<Connection>>),
    ListenAddrs(oneshot::Sender<Vec<String>>),
    Attach(Arc<Daemon>),
    /// The registry's answer about a peer that tried to connect.
    Vouched(PeerId, bool),
    /// The daemon's acknowledgement of an inbound message.
    Acked(MessageKind, ResponseChannel<Ack>, Ack),
    /// Close every connection and end the task.
    Shutdown,
}

/// Handle to the swarm task; this is the daemon's [`Transport`].
pub struct Net {
    commands: mpsc::Sender<Command>,
}

/// The relay node a daemon bootstraps from: its address, ending in
/// `/p2p/<node peer id>`.
#[derive(Clone)]
pub struct Bootstrap {
    pub addr: Multiaddr,
    pub peer: PeerId,
}

impl Bootstrap {
    pub fn parse(value: &str) -> anyhow::Result<Self> {
        let addr: Multiaddr = value.parse()?;
        let peer = addr
            .iter()
            .find_map(|p| match p {
                Protocol::P2p(peer) => Some(peer),
                _ => None,
            })
            .ok_or_else(|| anyhow::anyhow!("the bootstrap address must end in /p2p/<node peer id>"))?;
        Ok(Self { addr, peer })
    }

    /// The address of `peer` through this relay.
    fn circuit(&self, peer: PeerId) -> Multiaddr {
        self.addr.clone().with(Protocol::P2pCircuit).with(Protocol::P2p(peer))
    }
}

/// The message kind a wire message travels as: its own for a request, and
/// the `ask` protocol for answers.
fn kind_of(wire: &Wire) -> MessageKind {
    #[derive(Deserialize)]
    struct KindOnly {
        kind: MessageKind,
    }
    match wire {
        Wire::Request(signed) => URL_SAFE_NO_PAD
            .decode(&signed.header)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<KindOnly>(&bytes).ok())
            .map_or(MessageKind::Ask, |k| k.kind),
        Wire::Answer(_) => MessageKind::Ask,
    }
}

impl Net {
    /// Starts the swarm for `identity`, listening on `listen` and using
    /// `bootstrap` as relay. Inbound messages are refused until a daemon is
    /// attached.
    pub fn start(identity: &Identity, listen: &[Multiaddr], bootstrap: Bootstrap) -> anyhow::Result<Arc<Self>> {
        let keypair = libp2p::identity::Keypair::ed25519_from_bytes(identity.secret())?;
        let mut swarm = libp2p::SwarmBuilder::with_existing_identity(keypair)
            .with_tokio()
            .with_tcp(tcp::Config::default(), noise::Config::new, yamux::Config::default)?
            .with_quic()
            .with_relay_client(noise::Config::new, yamux::Config::default)?
            .with_behaviour(|key, relay| Behaviour {
                allowed: allow_block_list::Behaviour::default(),
                relay,
                dcutr: dcutr::Behaviour::new(key.public().to_peer_id()),
                identify: identify::Behaviour::new(identify::Config::new(IDENTIFY_PROTOCOL.to_owned(), key.public())),
                ping: ping::Behaviour::default(),
                ask: messages(ASK_PROTOCOL),
                status: messages(STATUS_PROTOCOL),
                notify: messages(NOTIFY_PROTOCOL),
            })?
            .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(120)))
            .build();
        for addr in listen {
            swarm.listen_on(addr.clone())?;
        }
        // The relay node is the one peer allowed without asking the registry.
        swarm.behaviour_mut().allowed.allow_peer(bootstrap.peer);
        let (commands, inbox) = mpsc::channel(256);
        tokio::spawn(run(swarm, bootstrap, inbox, commands.clone()));
        Ok(Arc::new(Self { commands }))
    }

    /// Leaves the network: every connection closes and nothing more is sent
    /// or received through this handle.
    pub async fn shutdown(&self) {
        let _ = self.commands.send(Command::Shutdown).await;
    }

    /// Gives inbound messages somewhere to go.
    pub async fn attach(&self, daemon: Arc<Daemon>) {
        let _ = self.commands.send(Command::Attach(daemon)).await;
    }

    async fn ask<T>(&self, command: impl FnOnce(oneshot::Sender<T>) -> Command) -> Option<T> {
        let (done, result) = oneshot::channel();
        self.commands.send(command(done)).await.ok()?;
        result.await.ok()
    }
}

#[async_trait]
impl Transport for Net {
    async fn send(&self, peer_id: &str, addrs: &[String], wire: Wire) -> Result<Ack, TransportError> {
        let peer: PeerId = peer_id
            .parse()
            .map_err(|_| TransportError::Unreachable("not a peer id".to_owned()))?;
        let addrs = addrs.iter().filter_map(|a| a.parse().ok()).collect();
        let kind = kind_of(&wire);
        self.ask(|done| Command::Send {
            peer,
            addrs,
            kind,
            wire,
            done,
        })
        .await
        .unwrap_or_else(|| Err(TransportError::Unreachable("the network task stopped".to_owned())))
    }

    async fn disconnect(&self, peer_id: &str) {
        if let Ok(peer) = peer_id.parse() {
            let _ = self.commands.send(Command::Disconnect(peer)).await;
        }
    }

    async fn connections(&self) -> Vec<Connection> {
        self.ask(Command::Connections).await.unwrap_or_default()
    }

    async fn listen_addrs(&self) -> Vec<String> {
        self.ask(Command::ListenAddrs).await.unwrap_or_default()
    }
}

struct Loop {
    swarm: Swarm<Behaviour>,
    bootstrap: Bootstrap,
    daemon: Option<Arc<Daemon>>,
    commands: mpsc::Sender<Command>,
    pending: HashMap<OutboundRequestId, oneshot::Sender<Result<Ack, TransportError>>>,
    /// Connections per peer: `true` for one through the relay.
    connections: HashMap<PeerId, Vec<bool>>,
    /// Peers the registry is being asked about.
    vouching: HashSet<PeerId>,
    /// A relay reservation was requested on the current node connection.
    reserved: bool,
}

async fn run(
    swarm: Swarm<Behaviour>,
    bootstrap: Bootstrap,
    mut inbox: mpsc::Receiver<Command>,
    commands: mpsc::Sender<Command>,
) {
    let mut state = Loop {
        swarm,
        bootstrap,
        daemon: None,
        commands,
        pending: HashMap::new(),
        connections: HashMap::new(),
        vouching: HashSet::new(),
        reserved: false,
    };
    let mut bootstrap_tick = tokio::time::interval(BOOTSTRAP_INTERVAL);
    loop {
        tokio::select! {
            event = state.swarm.select_next_some() => state.on_event(event),
            command = inbox.recv() => match command {
                Some(Command::Shutdown) | None => break,
                Some(command) => state.on_command(command),
            },
            _ = bootstrap_tick.tick() => state.keep_bootstrap(),
        }
    }
}

impl Loop {
    fn messages(&mut self, kind: MessageKind) -> &mut Messages {
        let behaviour = self.swarm.behaviour_mut();
        match kind {
            MessageKind::Ask => &mut behaviour.ask,
            MessageKind::Status => &mut behaviour.status,
            MessageKind::Notify => &mut behaviour.notify,
        }
    }

    /// Dials the relay node when not connected, and reserves a slot on it
    /// once connected, so peers can reach this daemon behind NAT.
    fn keep_bootstrap(&mut self) {
        if !self.swarm.is_connected(&self.bootstrap.peer) {
            self.reserved = false;
            // Errors mean a dial is already under way or the node is down;
            // either way the next tick tries again.
            let _ = self.swarm.dial(self.bootstrap.addr.clone());
        } else if !self.reserved {
            let circuit = self.bootstrap.addr.clone().with(Protocol::P2pCircuit);
            self.reserved = self.swarm.listen_on(circuit).is_ok();
        }
    }

    fn on_command(&mut self, command: Command) {
        match command {
            Command::Send {
                peer,
                addrs,
                kind,
                wire,
                done,
            } => {
                // The daemon only sends to peers the registry vouched for.
                self.swarm.behaviour_mut().allowed.allow_peer(peer);
                for addr in addrs {
                    self.swarm.add_peer_address(peer, addr);
                }
                let circuit = self.bootstrap.circuit(peer);
                self.swarm.add_peer_address(peer, circuit);
                let id = self.messages(kind).send_request(&peer, wire);
                self.pending.insert(id, done);
            }
            Command::Disconnect(peer) => {
                self.swarm.behaviour_mut().allowed.disallow_peer(peer);
                let _ = self.swarm.disconnect_peer_id(peer);
            }
            Command::Connections(done) => {
                let list = self
                    .connections
                    .iter()
                    .filter(|(peer, links)| **peer != self.bootstrap.peer && !links.is_empty())
                    .map(|(peer, links)| Connection {
                        peer_id: peer.to_string(),
                        // Direct once any connection bypasses the relay.
                        relayed: links.iter().all(|relayed| *relayed),
                    })
                    .collect();
                let _ = done.send(list);
            }
            Command::ListenAddrs(done) => {
                let addrs: Vec<String> = self
                    .swarm
                    .external_addresses()
                    .chain(self.swarm.listeners())
                    .filter(|a| !a.iter().any(|p| matches!(p, Protocol::P2pCircuit)))
                    .map(ToString::to_string)
                    // Only what the registry will accept: dialable, not loopback.
                    .filter(|a| nanace_core::p2p::multiaddrs(std::slice::from_ref(a)).is_ok())
                    .collect::<HashSet<_>>()
                    .into_iter()
                    .collect();
                let _ = done.send(addrs);
            }
            Command::Attach(daemon) => self.daemon = Some(daemon),
            Command::Vouched(peer, allowed) => {
                self.vouching.remove(&peer);
                if allowed {
                    // The peer retries; this time the connection is kept.
                    self.swarm.behaviour_mut().allowed.allow_peer(peer);
                }
            }
            // Handled by the loop itself.
            Command::Shutdown => {}
            Command::Acked(kind, channel, ack) => {
                // The other side may have gone; it will ask again.
                let _ = self.messages(kind).send_response(channel, ack);
            }
        }
    }

    fn on_event(&mut self, event: SwarmEvent<BehaviourEvent>) {
        match event {
            SwarmEvent::Behaviour(BehaviourEvent::Ask(event)) => self.on_message(MessageKind::Ask, event),
            SwarmEvent::Behaviour(BehaviourEvent::Status(event)) => self.on_message(MessageKind::Status, event),
            SwarmEvent::Behaviour(BehaviourEvent::Notify(event)) => self.on_message(MessageKind::Notify, event),
            SwarmEvent::ConnectionEstablished { peer_id, endpoint, .. } => {
                let relayed = endpoint
                    .get_remote_address()
                    .iter()
                    .any(|p| matches!(p, Protocol::P2pCircuit));
                self.connections.entry(peer_id).or_default().push(relayed);
                tracing::debug!(event = "connection_established", component = "nanace_p2p", peer = %peer_id, relayed, "connected");
                if peer_id == self.bootstrap.peer {
                    self.keep_bootstrap();
                }
            }
            SwarmEvent::ConnectionClosed {
                peer_id,
                endpoint,
                num_established,
                ..
            } => {
                let relayed = endpoint
                    .get_remote_address()
                    .iter()
                    .any(|p| matches!(p, Protocol::P2pCircuit));
                if let Some(links) = self.connections.get_mut(&peer_id) {
                    if let Some(i) = links.iter().position(|r| *r == relayed) {
                        links.remove(i);
                    }
                    if num_established == 0 {
                        self.connections.remove(&peer_id);
                    }
                }
                if peer_id == self.bootstrap.peer && num_established == 0 {
                    self.reserved = false;
                }
            }
            // A peer the allow list turned away: ask the registry about it,
            // so that its next attempt is let in if it belongs to a team of
            // ours.
            SwarmEvent::IncomingConnectionError {
                peer_id: Some(peer), ..
            } => self.vouch(peer),
            SwarmEvent::Behaviour(BehaviourEvent::Dcutr(event)) => {
                tracing::debug!(event = "hole_punch", component = "nanace_p2p", peer = %event.remote_peer_id, direct = event.result.is_ok(), "direct connection attempt");
            }
            _ => {}
        }
    }

    fn vouch(&mut self, peer: PeerId) {
        let Some(daemon) = self.daemon.clone() else {
            return;
        };
        if !self.vouching.insert(peer) {
            return;
        }
        let commands = self.commands.clone();
        tokio::spawn(async move {
            let allowed = daemon.vouches_for(&peer.to_string()).await;
            let _ = commands.send(Command::Vouched(peer, allowed)).await;
        });
    }

    fn on_message(&mut self, kind: MessageKind, event: request_response::Event<Wire, Ack>) {
        match event {
            request_response::Event::Message {
                peer,
                message: request_response::Message::Request { request, channel, .. },
                ..
            } => {
                let (Some(daemon), commands) = (self.daemon.clone(), self.commands.clone()) else {
                    let _ = self
                        .messages(kind)
                        .send_response(channel, Ack::Rejected(RejectReason::Consent));
                    return;
                };
                // The daemon may consult the registry; never block the swarm.
                tokio::spawn(async move {
                    let ack = daemon.handle_wire(&peer.to_string(), request).await;
                    let _ = commands.send(Command::Acked(kind, channel, ack)).await;
                });
            }
            request_response::Event::Message {
                message: request_response::Message::Response { request_id, response },
                ..
            } => {
                if let Some(done) = self.pending.remove(&request_id) {
                    let _ = done.send(Ok(response));
                }
            }
            request_response::Event::OutboundFailure { request_id, error, .. } => {
                if let Some(done) = self.pending.remove(&request_id) {
                    let _ = done.send(Err(TransportError::Unreachable(error.to_string())));
                }
            }
            _ => {}
        }
    }
}
