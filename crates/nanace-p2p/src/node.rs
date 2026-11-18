//! The network node: a public relay that daemons bootstrap from and are
//! reached through when they are behind NAT. It admits only the peer ids
//! the nanace API lists, holds no token, database access or message, and
//! relays traffic that stays encrypted between the two daemons.

use std::collections::{HashMap, HashSet};
use std::num::NonZeroU32;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use libp2p::allow_block_list;
use libp2p::swarm::SwarmEvent;
use libp2p::{Multiaddr, PeerId, autonat, connection_limits, identify, noise, ping, relay, tcp, yamux};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use tokio::sync::mpsc;

use crate::net::IDENTIFY_PROTOCOL;
use crate::throttle::{Throttle, source_of};

/// How often the allow list is fetched. A revoked peer is disconnected
/// within this time.
const SYNC_INTERVAL: Duration = Duration::from_secs(10);
/// After this many failed fetches in a row (half a minute) the last allow
/// list is dropped and every peer is turned away.
const MAX_FAILED_SYNCS: u32 = 3;
/// How long a peer the server refused is not asked about again, how many
/// such peers are remembered, and how many questions may be in flight.
const REFUSED_FOR: Duration = Duration::from_secs(30);
const MAX_REFUSED: usize = 4096;
const MAX_ASKING: usize = 16;

// In a module of its own so that the `allow` covers nothing but the derive's
// output: `allow_block_list::Behaviour` emits `Infallible`, and compilers
// from 1.100 on report the generated code that forwards its events as
// unreachable.
#[allow(unreachable_code)]
mod behaviour {
    use libp2p::allow_block_list::{self, AllowedPeers};
    use libp2p::swarm::NetworkBehaviour;
    use libp2p::{autonat, connection_limits, identify, ping, relay};

    #[derive(NetworkBehaviour)]
    pub(super) struct Behaviour {
        pub(super) limits: connection_limits::Behaviour,
        pub(super) allowed: allow_block_list::Behaviour<AllowedPeers>,
        pub(super) relay: relay::Behaviour,
        pub(super) autonat: autonat::Behaviour,
        pub(super) identify: identify::Behaviour,
        pub(super) ping: ping::Behaviour,
    }
}
use behaviour::{Behaviour, BehaviourEvent};

/// Relay limits, sized for text between agents.
pub struct Limits {
    pub max_reservations: usize,
    pub max_circuits: usize,
    pub max_circuits_per_peer: usize,
    pub max_circuit_secs: u64,
    pub max_circuit_bytes: u64,
    /// Inbound connections still shaking hands.
    pub max_pending_connections: u32,
    pub max_connections: u32,
    pub max_connections_per_peer: u32,
    /// Questions to the server about peers missing from the allow list,
    /// per minute: from one remote address and in all.
    pub allow_lookups_per_source: u32,
    pub allow_lookups: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_reservations: 1024,
            max_circuits: 1024,
            max_circuits_per_peer: 16,
            max_circuit_secs: 600,
            max_circuit_bytes: 2 * 1024 * 1024,
            max_pending_connections: 128,
            max_connections: 4096,
            max_connections_per_peer: 8,
            allow_lookups_per_source: 6,
            allow_lookups: 120,
        }
    }
}

pub struct NodeConfig {
    /// 32-byte Ed25519 seed of the node's identity.
    pub key: [u8; 32],
    pub listen: Vec<Multiaddr>,
    /// Addresses daemons reach the node at; advertised in reservations.
    pub external: Vec<Multiaddr>,
    /// Base URL of the nanace server's internal listener.
    pub internal_url: String,
    pub secret: SecretString,
    pub limits: Limits,
}

#[derive(Deserialize)]
struct AllowedBody {
    peers: Vec<String>,
}

#[derive(Deserialize)]
struct AllowBody {
    allowed: bool,
}

struct Api {
    http: reqwest::Client,
    url: String,
    secret: SecretString,
}

impl Api {
    async fn allowed(&self) -> anyhow::Result<HashSet<PeerId>> {
        let body: AllowedBody = self
            .http
            .get(format!("{}/internal/p2p/allowed", self.url))
            .bearer_auth(self.secret.expose_secret())
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(body.peers.iter().filter_map(|p| p.parse().ok()).collect())
    }

    async fn allow(&self, peer: PeerId) -> anyhow::Result<bool> {
        let body: AllowBody = self
            .http
            .get(format!("{}/internal/p2p/allow", self.url))
            .query(&[("peer_id", peer.to_string())])
            .bearer_auth(self.secret.expose_secret())
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(body.allowed)
    }
}

/// The peer id a node with this key has, for bootstrap addresses.
pub fn peer_id(key: [u8; 32]) -> anyhow::Result<PeerId> {
    Ok(libp2p::identity::Keypair::ed25519_from_bytes(key)?
        .public()
        .to_peer_id())
}

/// Runs the node until the process stops.
pub async fn run(config: NodeConfig) -> anyhow::Result<()> {
    let keypair = libp2p::identity::Keypair::ed25519_from_bytes(config.key)?;
    let limits = config.limits;
    let mut swarm = libp2p::SwarmBuilder::with_existing_identity(keypair)
        .with_tokio()
        .with_tcp(tcp::Config::default(), noise::Config::new, yamux::Config::default)?
        .with_quic()
        .with_behaviour(|key| {
            let peer = key.public().to_peer_id();
            Behaviour {
                limits: connection_limits::Behaviour::new(
                    connection_limits::ConnectionLimits::default()
                        .with_max_pending_incoming(Some(limits.max_pending_connections))
                        .with_max_established_incoming(Some(limits.max_connections))
                        .with_max_established_per_peer(Some(limits.max_connections_per_peer)),
                ),
                allowed: allow_block_list::Behaviour::default(),
                relay: relay::Behaviour::new(
                    peer,
                    relay::Config {
                        max_reservations: limits.max_reservations,
                        max_reservations_per_peer: 1,
                        max_circuits: limits.max_circuits,
                        max_circuits_per_peer: limits.max_circuits_per_peer,
                        max_circuit_duration: Duration::from_secs(limits.max_circuit_secs),
                        max_circuit_bytes: limits.max_circuit_bytes,
                        // Replaces the default of one reservation per two
                        // minutes: a daemon that reconnects asks again at
                        // once. The allow list, not the rate, is what keeps
                        // strangers out.
                        reservation_rate_limiters: Vec::new(),
                        ..relay::Config::default()
                    }
                    .reservation_rate_per_peer(NonZeroU32::new(120).expect("120 > 0"), Duration::from_secs(60)),
                ),
                autonat: autonat::Behaviour::new(peer, autonat::Config::default()),
                identify: identify::Behaviour::new(identify::Config::new(IDENTIFY_PROTOCOL.to_owned(), key.public())),
                ping: ping::Behaviour::default(),
            }
        })?
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(120)))
        .build();
    for addr in &config.listen {
        swarm.listen_on(addr.clone())?;
    }
    for addr in config.external {
        swarm.add_external_address(addr);
    }
    tracing::info!(event = "node_start", component = "nanace_p2p", peer = %swarm.local_peer_id(), "relay node listening");

    let api = std::sync::Arc::new(Api {
        http: reqwest::Client::builder().timeout(Duration::from_secs(10)).build()?,
        url: config.internal_url.trim_end_matches('/').to_owned(),
        secret: config.secret,
    });
    let (answers, mut answered) = mpsc::channel::<(PeerId, bool)>(256);
    let mut asking: HashSet<PeerId> = HashSet::new();
    // Peers the server said no about, so a retry does not ask again at once.
    let mut refused: HashMap<PeerId, Instant> = HashMap::new();
    let mut lookups = Throttle::new(
        limits.allow_lookups_per_source,
        limits.allow_lookups,
        Duration::from_secs(60),
    );
    let mut failed_syncs = 0u32;
    let mut sync = tokio::time::interval(SYNC_INTERVAL);
    let (mut reservations, mut circuits) = (0u64, 0u64);
    loop {
        tokio::select! {
            _ = sync.tick() => match api.allowed().await {
                Ok(allowed) => {
                    failed_syncs = 0;
                    refused.retain(|_, at| at.elapsed() < REFUSED_FOR);
                    let current: Vec<PeerId> = swarm.behaviour().allowed.allowed_peers().iter().copied().collect();
                    for peer in current {
                        if !allowed.contains(&peer) {
                            // Also closes its connections, and with them its
                            // reservation and circuits.
                            swarm.behaviour_mut().allowed.disallow_peer(peer);
                        }
                    }
                    for peer in allowed {
                        swarm.behaviour_mut().allowed.allow_peer(peer);
                    }
                }
                // Keep the last list over a short outage, then fail closed:
                // without the server nobody can be shown to be allowed, and a
                // peer revoked meanwhile must not keep its relay access.
                Err(err) => {
                    failed_syncs += 1;
                    tracing::warn!(event = "allow_list_failed", component = "nanace_p2p", failures = failed_syncs, error = %format!("{err:#}"), "could not fetch the allow list");
                    if failed_syncs == MAX_FAILED_SYNCS {
                        let current: Vec<PeerId> = swarm.behaviour().allowed.allowed_peers().iter().copied().collect();
                        for peer in current {
                            swarm.behaviour_mut().allowed.disallow_peer(peer);
                        }
                        tracing::error!(event = "allow_list_expired", component = "nanace_p2p", "the allow list is too old: every peer is turned away until the server answers");
                    }
                }
            },
            Some((peer, allowed)) = answered.recv() => {
                asking.remove(&peer);
                if allowed {
                    swarm.behaviour_mut().allowed.allow_peer(peer);
                } else {
                    if refused.len() >= MAX_REFUSED {
                        refused.clear();
                    }
                    refused.insert(peer, Instant::now());
                }
            }
            event = swarm.select_next_some() => match event {
                // A peer not in the last snapshot: ask about it once, so a
                // daemon that just announced itself gets in on its retry.
                // A new key costs a stranger nothing and each question is a
                // database query on the server, so questions are bounded:
                // per remote address, in all, in flight, and not repeated
                // for a peer just refused.
                SwarmEvent::IncomingConnectionError { peer_id: Some(peer), send_back_addr, .. }
                    if failed_syncs < MAX_FAILED_SYNCS
                        && asking.len() < MAX_ASKING
                        && !asking.contains(&peer)
                        && refused.get(&peer).is_none_or(|at| at.elapsed() >= REFUSED_FOR)
                        && lookups.allow(source_of(&send_back_addr), Instant::now()) =>
                {
                    asking.insert(peer);
                    let (api, answers) = (std::sync::Arc::clone(&api), answers.clone());
                    tokio::spawn(async move {
                        let allowed = api.allow(peer).await.unwrap_or(false);
                        let _ = answers.send((peer, allowed)).await;
                    });
                }
                SwarmEvent::Behaviour(BehaviourEvent::Relay(relay::Event::ReservationReqAccepted { .. })) => {
                    reservations += 1;
                    tracing::debug!(event = "relay_reservation", component = "nanace_p2p", total = reservations, "reservation accepted");
                }
                SwarmEvent::Behaviour(BehaviourEvent::Relay(relay::Event::CircuitReqAccepted { .. })) => {
                    circuits += 1;
                    tracing::debug!(event = "relay_circuit", component = "nanace_p2p", total = circuits, "circuit opened");
                }
                SwarmEvent::NewListenAddr { address, .. } => {
                    tracing::info!(event = "node_listen", component = "nanace_p2p", address = %address, "listening");
                }
                _ => {}
            },
        }
    }
}
