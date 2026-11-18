//! Two or more daemons in one process, with a fake registry and an
//! in-memory link instead of the API and libp2p. Everything the daemon
//! decides is exercised here; the network itself is covered by the compose
//! end-to-end test.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use nanace_core::TeamRole;
use nanace_core::p2p::{
    AcceptMode, AnswerHeader, AnswerStatus, ClientKind, MessageKind, Party, RejectReason, RequestHeader, sha256_hex,
};
use nanace_core::policy::{CallRule, P2pPolicy};
use nanace_p2p::daemon::{AgentStatus, Daemon, DaemonError, InState, Notice, OutState, SendParams};
use nanace_p2p::envelope::{Ack, Identity, Signed, Wire};
use nanace_p2p::registry::{
    ExchangeReport, Me, PeerInfo, PolicyState, Presence, PresentPeer, Registry, RegistryError, RegistryResult,
    SharedTeam, TeamMembership,
};
use nanace_p2p::transport::{Connection, Transport, TransportError};
use uuid::Uuid;

const NOW: i64 = 1_800_000_000;

/// The server's knowledge, shared by every fake registry.
#[derive(Default)]
struct World {
    emails: HashMap<Uuid, String>,
    /// (team, user) -> role
    members: HashMap<(Uuid, Uuid), TeamRole>,
    /// peer -> user
    peers: HashMap<String, Uuid>,
    /// (team, peer, agent label)
    presence: HashSet<(Uuid, String, String)>,
    policies: HashMap<Uuid, P2pPolicy>,
    reports: Vec<(Uuid, ExchangeReport)>,
    down: bool,
}

struct FakeRegistry {
    world: Arc<Mutex<World>>,
    org: Uuid,
    user: Uuid,
}

impl FakeRegistry {
    fn world(&self) -> RegistryResult<std::sync::MutexGuard<'_, World>> {
        let world = self.world.lock().unwrap();
        if world.down {
            return Err(RegistryError::Unavailable("down".to_owned()));
        }
        Ok(world)
    }
}

#[async_trait]
impl Registry for FakeRegistry {
    async fn me(&self) -> RegistryResult<Me> {
        let world = self.world()?;
        Ok(Me {
            user_id: self.user,
            email: world.emails[&self.user].clone(),
            teams: world
                .members
                .iter()
                .filter(|((_, user), _)| *user == self.user)
                .map(|((team, _), role)| TeamMembership {
                    org_id: self.org,
                    team_id: *team,
                    team_slug: "team".to_owned(),
                    role: *role,
                })
                .collect(),
        })
    }

    async fn bind(&self, identity: &Identity, user_id: Uuid, _: ClientKind, _: &str) -> RegistryResult<()> {
        self.world()?.peers.insert(identity.peer_id.clone(), user_id);
        Ok(())
    }

    async fn lookup(&self, peer_id: &str) -> RegistryResult<Option<PeerInfo>> {
        let world = self.world()?;
        let Some(owner) = world.peers.get(peer_id).copied() else {
            return Ok(None);
        };
        let shared: Vec<SharedTeam> = world
            .members
            .iter()
            .filter(|((team, user), _)| {
                *user == owner
                    && world.members.contains_key(&(*team, self.user))
                    && world.policies.get(team).is_some_and(|p| p.enabled)
                    && world.presence.iter().any(|(t, p, _)| t == team && p == peer_id)
            })
            .map(|((team, _), role)| SharedTeam {
                org_id: self.org,
                team_id: *team,
                role: *role,
            })
            .collect();
        Ok((!shared.is_empty()).then(|| PeerInfo {
            user_id: owner,
            email: world.emails[&owner].clone(),
            shared,
        }))
    }

    async fn policy(&self, _: Uuid, team_id: Uuid, with_text: bool) -> RegistryResult<PolicyState> {
        let world = self.world()?;
        let policy = world.policies.get(&team_id).cloned();
        Ok(PolicyState {
            version: policy.as_ref().map(|p| format!("v{}", p.limits.max_hops)),
            policy,
            errors: vec![],
            text: with_text.then(|| "be kind".to_owned()),
        })
    }

    async fn peers(&self, _: Uuid, team_id: Uuid, _: Option<&str>) -> RegistryResult<Vec<PresentPeer>> {
        let world = self.world()?;
        Ok(world
            .presence
            .iter()
            .filter(|(team, _, _)| *team == team_id)
            .map(|(_, peer, label)| PresentPeer {
                peer_id: peer.clone(),
                agent_label: label.clone(),
                user_id: world.peers[peer],
                email: world.emails[&world.peers[peer]].clone(),
                capabilities: vec![],
                multiaddrs: vec![],
                accept_mode: AcceptMode::Ask,
                client_kind: ClientKind::Desktop,
            })
            .collect())
    }

    async fn announce(&self, _: Uuid, team_id: Uuid, presence: &Presence) -> RegistryResult<()> {
        self.world()?
            .presence
            .insert((team_id, presence.peer_id.clone(), presence.agent_label.clone()));
        Ok(())
    }

    async fn withdraw(&self, _: Uuid, team_id: Uuid, peer_id: &str, agent_label: &str) -> RegistryResult<()> {
        self.world()?
            .presence
            .remove(&(team_id, peer_id.to_owned(), agent_label.to_owned()));
        Ok(())
    }

    async fn report(&self, _: Uuid, team_id: Uuid, report: &ExchangeReport) -> RegistryResult<()> {
        self.world()?.reports.push((team_id, report.clone()));
        Ok(())
    }
}

/// Connects daemons directly: a send is a call to the other's `handle_wire`.
#[derive(Default)]
struct Hub {
    daemons: Mutex<HashMap<String, Arc<Daemon>>>,
    offline: Mutex<HashSet<String>>,
    disconnected: Mutex<Vec<(String, String)>>,
    connected: Mutex<HashSet<(String, String)>>,
}

struct Link {
    hub: Arc<Hub>,
    me: String,
}

#[async_trait]
impl Transport for Link {
    async fn send(&self, peer_id: &str, _: &[String], wire: Wire) -> Result<Ack, TransportError> {
        let target = {
            let offline = self.hub.offline.lock().unwrap();
            if offline.contains(peer_id) || offline.contains(&self.me) {
                return Err(TransportError::Unreachable("offline".to_owned()));
            }
            self.hub.daemons.lock().unwrap().get(peer_id).cloned()
        };
        let Some(target) = target else {
            return Err(TransportError::Unreachable("unknown".to_owned()));
        };
        self.hub
            .connected
            .lock()
            .unwrap()
            .insert((self.me.clone(), peer_id.to_owned()));
        Ok(target.handle_wire(&self.me, wire).await)
    }

    async fn disconnect(&self, peer_id: &str) {
        self.hub
            .connected
            .lock()
            .unwrap()
            .remove(&(self.me.clone(), peer_id.to_owned()));
        self.hub
            .disconnected
            .lock()
            .unwrap()
            .push((self.me.clone(), peer_id.to_owned()));
    }

    async fn connections(&self) -> Vec<Connection> {
        self.hub
            .connected
            .lock()
            .unwrap()
            .iter()
            .filter(|(me, _)| *me == self.me)
            .map(|(_, peer)| Connection {
                peer_id: peer.clone(),
                relayed: true,
            })
            .collect()
    }

    async fn listen_addrs(&self) -> Vec<String> {
        vec!["/ip4/192.168.1.5/tcp/4001".to_owned()]
    }
}

struct Net {
    world: Arc<Mutex<World>>,
    hub: Arc<Hub>,
    clock: Arc<AtomicI64>,
    org: Uuid,
    team: Uuid,
    root: PathBuf,
}

struct Node {
    daemon: Arc<Daemon>,
    identity: Identity,
    user: Uuid,
    dir: PathBuf,
}

fn open_policy() -> P2pPolicy {
    let everyone = vec![TeamRole::Agent, TeamRole::Writer, TeamRole::Admin];
    serde_json::from_value(serde_json::json!({
        "version": 1,
        "enabled": true,
        "max_accept_mode": "team",
        "calls": [{ "from": everyone, "to": everyone, "kinds": ["ask", "status", "notify"] }],
        "summaries": { "required_for_kinds": ["ask"] },
    }))
    .unwrap()
}

impl Net {
    fn new() -> Self {
        let net = Self {
            world: Arc::default(),
            hub: Arc::default(),
            clock: Arc::new(AtomicI64::new(NOW)),
            org: Uuid::new_v4(),
            team: Uuid::new_v4(),
            // Short: a Unix socket path holds about a hundred bytes.
            root: std::env::temp_dir().join(format!("np-{}", &Uuid::new_v4().simple().to_string()[..8])),
        };
        net.world.lock().unwrap().policies.insert(net.team, open_policy());
        net
    }

    fn policy(&self, change: impl FnOnce(&mut P2pPolicy)) {
        change(self.world.lock().unwrap().policies.get_mut(&self.team).unwrap());
    }

    fn advance(&self, secs: i64) {
        self.clock.fetch_add(secs, Ordering::SeqCst);
    }

    /// A user with `role` in the team, and their daemon with one agent
    /// registered, the team enabled in `mode` and presence announced.
    async fn join(&self, name: &str, role: Option<TeamRole>, kind: ClientKind, mode: AcceptMode) -> Node {
        let user = Uuid::new_v4();
        {
            let mut world = self.world.lock().unwrap();
            world.emails.insert(user, format!("{name}@example.com"));
            if let Some(role) = role {
                world.members.insert((self.team, user), role);
            }
        }
        let mut secret = [0u8; 32];
        secret[..16].copy_from_slice(user.as_bytes());
        let node = self.start(name, Identity::from_secret(secret), user, kind).await;
        if role.is_some() {
            node.daemon.set_team(self.org, self.team, mode).await.unwrap();
        }
        node.daemon.register_agent(name, &[]).await.unwrap();
        node.daemon.set_supervisor(kind == ClientKind::Desktop).await;
        node.daemon.tick().await;
        node
    }

    async fn start(&self, name: &str, identity: Identity, user: Uuid, kind: ClientKind) -> Node {
        let dir = self.root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let clock = Arc::clone(&self.clock);
        let daemon = Daemon::start(
            &dir,
            identity.clone(),
            kind,
            Arc::new(FakeRegistry {
                world: Arc::clone(&self.world),
                org: self.org,
                user,
            }),
            Arc::new(Link {
                hub: Arc::clone(&self.hub),
                me: identity.peer_id.clone(),
            }),
            Arc::new(move || clock.load(Ordering::SeqCst)),
        )
        .await
        .unwrap();
        self.hub
            .daemons
            .lock()
            .unwrap()
            .insert(identity.peer_id.clone(), Arc::clone(&daemon));
        Node {
            daemon,
            identity,
            user,
            dir,
        }
    }

    fn ask(&self, to: &Node, body: &str) -> SendParams {
        SendParams {
            org_id: self.org,
            team_id: self.team,
            to_peer: to.identity.peer_id.clone(),
            to_agent: to.name(),
            kind: MessageKind::Ask,
            body: body.to_owned(),
            refs: vec![],
            trace_id: Some("TASK-1".to_owned()),
        }
    }

    /// A request header from `from` to `to`, valid unless changed.
    fn header(&self, from: &Node, to: &Node, body: &str) -> RequestHeader {
        let now = self.clock.load(Ordering::SeqCst);
        RequestHeader {
            v: 1,
            request_id: Uuid::new_v4(),
            trace_id: "TASK-1".to_owned(),
            org_id: self.org,
            team_id: self.team,
            from: Party {
                peer_id: from.identity.peer_id.clone(),
                agent_label: from.name(),
                user_id: from.user,
            },
            to: Party {
                peer_id: to.identity.peer_id.clone(),
                agent_label: to.name(),
                user_id: to.user,
            },
            kind: MessageKind::Ask,
            body_hash: sha256_hex(body.as_bytes()),
            body_bytes: u32::try_from(body.len()).unwrap(),
            refs: vec![],
            hop: 0,
            sent_at: now,
            deadline: now + 3600,
            policy_version: "v".to_owned(),
        }
    }

    fn reports(&self) -> Vec<ExchangeReport> {
        self.world
            .lock()
            .unwrap()
            .reports
            .iter()
            .map(|(_, r)| r.clone())
            .collect()
    }
}

impl Node {
    fn name(&self) -> String {
        self.dir.file_name().unwrap().to_string_lossy().into_owned()
    }

    async fn state_of(&self, request_id: Uuid) -> (OutState, Option<RejectReason>) {
        let reply = self.daemon.reply_of(request_id).await.unwrap();
        (reply.state, reply.reason)
    }
}

/// Lets spawned deliveries run.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(50)).await;
}

fn refusal(result: Result<impl std::fmt::Debug, DaemonError>) -> RejectReason {
    match result {
        Err(DaemonError::Refused { reason, .. }) => reason,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[tokio::test]
async fn question_waits_for_consent_then_is_answered_and_reported() {
    let net = Net::new();
    let alice = net
        .join("alice", Some(TeamRole::Writer), ClientKind::Desktop, AcceptMode::Ask)
        .await;
    let bob = net
        .join("bob", Some(TeamRole::Writer), ClientKind::Desktop, AcceptMode::Ask)
        .await;
    let mut notices = bob.daemon.subscribe();

    let question = "Which services depend on security group sg-123?";
    let sent = alice.daemon.send("alice", net.ask(&bob, question)).await.unwrap();
    assert_eq!(sent.trace_id, "TASK-1");
    settle().await;
    assert_eq!(alice.state_of(sent.request_id).await.0, OutState::Delivered);

    // Bob's user is asked first; the agent sees nothing until then.
    let prompt = loop {
        if let Notice::ConsentRequest { request } = notices.recv().await.unwrap() {
            break request;
        }
    };
    assert_eq!(prompt.body, question);
    assert_eq!(
        prompt.from_email, "alice@example.com",
        "identity comes from the registry"
    );
    assert_eq!(prompt.state, InState::PendingConsent);
    assert!(bob.daemon.inbox("bob").await.is_empty());
    assert_eq!(bob.daemon.pending().await.len(), 1);

    bob.daemon.decide_consent(sent.request_id, true, None).await.unwrap();
    let inbox = bob.daemon.inbox("bob").await;
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].body, question);
    assert!(bob.daemon.inbox("someone-else").await.is_empty());

    bob.daemon
        .reply("bob", sent.request_id, "billing-api and ledger".to_owned(), vec![])
        .await
        .unwrap();
    settle().await;
    let reply = alice.daemon.reply_of(sent.request_id).await.unwrap();
    assert_eq!(reply.state, OutState::Answered);
    assert_eq!(reply.answer.as_deref(), Some("billing-api and ledger"));
    assert!(reply.summary_required, "the policy asks for a summary after an ask");
    assert_eq!(reply.summary_title_prefix.as_deref(), Some("P2P: "));
    assert!(
        bob.daemon.inbox("bob").await.is_empty(),
        "an answered question leaves the inbox"
    );

    // Both sides report with signed headers; no report carries any text.
    alice.daemon.tick().await;
    bob.daemon.tick().await;
    let reports = net.reports();
    let final_reports: Vec<_> = reports.iter().filter(|r| r.answer_header.is_some()).collect();
    assert_eq!(final_reports.len(), 2, "{reports:#?}");
    let all = serde_json::to_string(&reports).unwrap();
    assert!(
        !all.contains("sg-123") && !all.contains("ledger"),
        "bodies must not be reported"
    );
    assert!(
        reports
            .iter()
            .any(|r| r.consent == Some(nanace_p2p::registry::Consent::UserApproved))
    );
}

#[tokio::test]
async fn a_laptop_without_the_desktop_app_accepts_nothing() {
    let net = Net::new();
    let alice = net
        .join("alice", Some(TeamRole::Writer), ClientKind::Desktop, AcceptMode::Ask)
        .await;
    let bob = net
        .join("bob", Some(TeamRole::Writer), ClientKind::Desktop, AcceptMode::Team)
        .await;
    bob.daemon.set_supervisor(false).await;

    let sent = alice.daemon.send("alice", net.ask(&bob, "hello?")).await.unwrap();
    settle().await;
    assert_eq!(
        alice.state_of(sent.request_id).await,
        (OutState::Rejected, Some(RejectReason::Consent))
    );
    assert!(bob.daemon.inbox("bob").await.is_empty() && bob.daemon.pending().await.is_empty());
    // It can still ask others.
    let back = bob.daemon.send("bob", net.ask(&alice, "and you?")).await.unwrap();
    settle().await;
    assert_eq!(bob.state_of(back.request_id).await.0, OutState::Delivered);

    // A server peer needs no desktop: its mode comes from its configuration.
    let robo = net
        .join("robo", Some(TeamRole::Agent), ClientKind::Server, AcceptMode::Team)
        .await;
    let sent = alice
        .daemon
        .send("alice", net.ask(&robo, "status of the rollout?"))
        .await
        .unwrap();
    settle().await;
    assert_eq!(alice.state_of(sent.request_id).await.0, OutState::Delivered);
    assert_eq!(robo.daemon.inbox("robo").await.len(), 1);
}

#[tokio::test]
async fn a_request_to_an_offline_peer_waits_then_expires() {
    let net = Net::new();
    let alice = net
        .join("alice", Some(TeamRole::Writer), ClientKind::Desktop, AcceptMode::Ask)
        .await;
    let carol = net
        .join("carol", Some(TeamRole::Writer), ClientKind::Desktop, AcceptMode::Team)
        .await;
    net.hub.offline.lock().unwrap().insert(carol.identity.peer_id.clone());

    let first = alice
        .daemon
        .send("alice", net.ask(&carol, "are you there?"))
        .await
        .unwrap();
    settle().await;
    assert_eq!(alice.state_of(first.request_id).await.0, OutState::Queued);
    for _ in 0..5 {
        net.advance(120);
        alice.daemon.tick().await;
    }
    assert_eq!(
        alice.state_of(first.request_id).await.0,
        OutState::Queued,
        "still within its TTL"
    );

    // The laptop opens again in time for a second request, not for the first.
    let second = alice.daemon.send("alice", net.ask(&carol, "ping")).await.unwrap();
    net.advance(3100);
    alice.daemon.tick().await;
    assert_eq!(alice.state_of(first.request_id).await.0, OutState::Expired);
    net.hub.offline.lock().unwrap().clear();
    net.advance(70);
    alice.daemon.tick().await;
    assert_eq!(alice.state_of(second.request_id).await.0, OutState::Delivered);
    assert_eq!(carol.daemon.inbox("carol").await.len(), 1);
    alice.daemon.tick().await;
    assert!(
        net.reports()
            .iter()
            .any(|r| r.status == nanace_p2p::registry::ReportStatus::Expired)
    );

    // Delivered but never answered: it expires on both sides at the deadline.
    net.advance(3600);
    carol.daemon.tick().await;
    settle().await;
    alice.daemon.tick().await;
    assert_eq!(alice.state_of(second.request_id).await.0, OutState::Expired);
    assert!(carol.daemon.inbox("carol").await.is_empty());
}

#[tokio::test]
async fn peers_outside_the_team_are_neither_found_nor_heard() {
    let net = Net::new();
    let alice = net
        .join("alice", Some(TeamRole::Writer), ClientKind::Desktop, AcceptMode::Team)
        .await;
    let dave = net.join("dave", None, ClientKind::Desktop, AcceptMode::Team).await;

    // Dave cannot address Alice: the registry does not know her to him.
    let result = dave.daemon.send("dave", net.ask(&alice, "hi")).await;
    assert!(
        matches!(
            result,
            Err(DaemonError::Refused {
                reason: RejectReason::Policy,
                ..
            })
        ),
        "{result:?}"
    );
    // Nor does a hand-made, correctly signed request get in.
    let header = net.header(&dave, &alice, "hi");
    let wire = Wire::Request(dave.identity.sign_request(&header, "hi".to_owned()));
    assert_eq!(
        alice.daemon.handle_wire(&dave.identity.peer_id, wire).await,
        Ack::Rejected(RejectReason::Invalid)
    );
    assert!(alice.daemon.inbox("alice").await.is_empty());
    assert_eq!(alice.daemon.find_peers(net.org, net.team, None).await.unwrap(), vec![]);
}

#[tokio::test]
async fn bad_envelopes_are_refused_before_anyone_sees_them() {
    let net = Net::new();
    net.policy(|p| p.limits.max_message_bytes = 2048);
    let alice = net
        .join("alice", Some(TeamRole::Writer), ClientKind::Desktop, AcceptMode::Team)
        .await;
    let bob = net
        .join("bob", Some(TeamRole::Writer), ClientKind::Desktop, AcceptMode::Team)
        .await;
    let reader = net
        .join("reader", Some(TeamRole::Reader), ClientKind::Desktop, AcceptMode::Team)
        .await;
    let mallory = Identity::from_secret([9; 32]);
    let deliver = |from: &Identity, signed: Signed| {
        let (daemon, peer) = (Arc::clone(&bob.daemon), from.peer_id.clone());
        async move { daemon.handle_wire(&peer, Wire::Request(signed)).await }
    };
    let good = net.header(&alice, &bob, "ok");

    // Unsigned, re-signed by another key, or sent over someone else's
    // connection.
    let signed = alice.identity.sign_request(&good, "ok".to_owned());
    let unsigned = Signed {
        signature: String::new(),
        ..signed.clone()
    };
    assert_eq!(
        deliver(&alice.identity, unsigned).await,
        Ack::Rejected(RejectReason::Invalid)
    );
    let resigned = mallory.sign_request(&good, "ok".to_owned());
    assert_eq!(
        deliver(&alice.identity, resigned).await,
        Ack::Rejected(RejectReason::Invalid)
    );
    assert_eq!(
        deliver(&mallory, signed.clone()).await,
        Ack::Rejected(RejectReason::Invalid)
    );

    let variant = |change: fn(&mut RequestHeader)| {
        let mut header = net.header(&alice, &bob, "ok");
        change(&mut header);
        alice.identity.sign_request(&header, "ok".to_owned())
    };
    for (signed, expected, why) in [
        (
            variant(|h| h.to.agent_label = "nobody".to_owned()),
            RejectReason::NoAgent,
            "unknown agent",
        ),
        (
            variant(|h| h.from.user_id = Uuid::new_v4()),
            RejectReason::Invalid,
            "claims another user",
        ),
        (
            variant(|h| h.to.user_id = Uuid::new_v4()),
            RejectReason::Invalid,
            "addressed to another user",
        ),
        (
            variant(|h| h.deadline = h.sent_at + 200_000),
            RejectReason::Replay,
            "lives longer than a day",
        ),
        (variant(|h| h.sent_at += 600), RejectReason::Replay, "from the future"),
        (
            variant(|h| h.deadline = h.sent_at - 1),
            RejectReason::Replay,
            "already past its deadline",
        ),
        (variant(|h| h.hop = 2), RejectReason::Hops, "too many hops"),
        (
            variant(|h| h.team_id = Uuid::new_v4()),
            RejectReason::Policy,
            "a team we do not share",
        ),
        (
            variant(|h| h.trace_id = "bad trace".to_owned()),
            RejectReason::Invalid,
            "trace id",
        ),
    ] {
        assert_eq!(deliver(&alice.identity, signed).await, Ack::Rejected(expected), "{why}");
    }
    // Addressed to another peer.
    let misrouted = alice
        .identity
        .sign_request(&net.header(&alice, &reader, "ok"), "ok".to_owned());
    assert_eq!(
        deliver(&alice.identity, misrouted).await,
        Ack::Rejected(RejectReason::Invalid)
    );

    // Too large for the team, or carrying a secret.
    let big = "x".repeat(4096);
    let oversized = alice.identity.sign_request(&net.header(&alice, &bob, &big), big);
    assert_eq!(
        deliver(&alice.identity, oversized).await,
        Ack::Rejected(RejectReason::Size)
    );
    let leak = "use nnc_pat_AbCdEfGh12345678";
    let leaking = alice
        .identity
        .sign_request(&net.header(&alice, &bob, leak), leak.to_owned());
    assert_eq!(
        deliver(&alice.identity, leaking).await,
        Ack::Rejected(RejectReason::Pattern)
    );

    // The policy lists no rule for readers.
    let from_reader = reader
        .identity
        .sign_request(&net.header(&reader, &bob, "ok"), "ok".to_owned());
    assert_eq!(
        deliver(&reader.identity, from_reader).await,
        Ack::Rejected(RejectReason::Policy)
    );

    // None of that reached the agent. A good request does, once.
    assert!(bob.daemon.inbox("bob").await.is_empty());
    assert_eq!(deliver(&alice.identity, signed.clone()).await, Ack::Accepted);
    assert_eq!(
        deliver(&alice.identity, signed).await,
        Ack::Rejected(RejectReason::Replay)
    );
    assert_eq!(bob.daemon.inbox("bob").await.len(), 1);

    // Storm: the per-peer rate of the policy.
    net.policy(|p| p.limits.rate_per_peer_per_min = 3);
    net.advance(61);
    let mut acks = Vec::new();
    for _ in 0..5 {
        let signed = alice
            .identity
            .sign_request(&net.header(&alice, &bob, "ok"), "ok".to_owned());
        acks.push(deliver(&alice.identity, signed).await);
    }
    assert_eq!(acks.iter().filter(|a| **a == Ack::Accepted).count(), 3, "{acks:?}");
    assert_eq!(acks[4], Ack::Rejected(RejectReason::Rate));

    // Turning P2P off in the policy stops everything within the cache time.
    net.policy(|p| p.enabled = false);
    net.advance(31);
    let signed = alice
        .identity
        .sign_request(&net.header(&alice, &bob, "ok"), "ok".to_owned());
    assert_eq!(
        deliver(&alice.identity, signed).await,
        Ack::Rejected(RejectReason::Invalid)
    );
}

#[tokio::test]
async fn nothing_secret_leaves_or_arrives() {
    let net = Net::new();
    net.policy(|p| {
        p.limits.max_message_bytes = 1024;
        p.data.deny_outbound_patterns = vec!["PROJECT-ZEUS".to_owned()];
        p.calls = vec![CallRule {
            from: vec![TeamRole::Writer],
            to: vec![TeamRole::Writer],
            kinds: vec![MessageKind::Ask],
        }];
    });
    let alice = net
        .join("alice", Some(TeamRole::Writer), ClientKind::Desktop, AcceptMode::Team)
        .await;
    let bob = net
        .join("bob", Some(TeamRole::Writer), ClientKind::Desktop, AcceptMode::Team)
        .await;
    let robo = net
        .join("robo", Some(TeamRole::Agent), ClientKind::Server, AcceptMode::Team)
        .await;

    // Questions are checked before they are signed.
    for (body, reason) in [
        ("my token is nnc_pat_AbCdEfGh12345678", RejectReason::Pattern),
        ("postgres://app:hunter2@db.internal/app is down", RejectReason::Pattern),
        ("what about PROJECT-ZEUS?", RejectReason::Pattern),
        (&"x".repeat(2000), RejectReason::Size),
    ] {
        assert_eq!(
            refusal(alice.daemon.send("alice", net.ask(&bob, body)).await),
            reason,
            "{body}"
        );
    }
    // The policy lets writers ask writers, nothing else.
    assert_eq!(
        refusal(alice.daemon.send("alice", net.ask(&robo, "hi")).await),
        RejectReason::Policy
    );
    let mut notify = net.ask(&bob, "fyi");
    notify.kind = MessageKind::Notify;
    assert_eq!(refusal(alice.daemon.send("alice", notify).await), RejectReason::Policy);
    assert!(matches!(
        alice.daemon.send("ghost", net.ask(&bob, "hi")).await,
        Err(DaemonError::Invalid(_))
    ));
    assert!(net.reports().is_empty(), "nothing was sent, so nothing is reported");

    // Answers are checked the same way, and the agent is told which rule.
    let sent = alice
        .daemon
        .send("alice", net.ask(&bob, "what is the db url?"))
        .await
        .unwrap();
    settle().await;
    let leak = bob
        .daemon
        .reply(
            "bob",
            sent.request_id,
            "it is postgres://app:hunter2@db.internal/app".to_owned(),
            vec![],
        )
        .await;
    match leak {
        Err(DaemonError::Refused { reason, detail }) => {
            assert_eq!(reason, RejectReason::Pattern);
            assert!(
                detail.contains("url_credentials") && !detail.contains("hunter2"),
                "{detail}"
            );
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        alice.state_of(sent.request_id).await.0,
        OutState::Delivered,
        "nothing was sent"
    );

    // A peer that skips its own check still cannot get a secret through:
    // the receiver drops the text and says so.
    let request = alice.daemon.reply_of(sent.request_id).await.unwrap();
    let inbox = bob.daemon.inbox("bob").await;
    let item = inbox.iter().find(|i| i.request_id == request.request_id).unwrap();
    let opened = {
        // Rebuild the request header the way Alice signed it.
        let mut header = net.header(&alice, &bob, &item.body);
        header.request_id = item.request_id;
        header
    };
    let _ = opened;
    let body = "password: Tr0ub4dor&3xyz".to_owned();
    // Bob's daemon will not sign that, so sign it by hand with Bob's key.
    let request_hash = alice_request_hash(&alice, sent.request_id).await;
    let answer = AnswerHeader {
        v: 1,
        request_id: sent.request_id,
        request_hash,
        status: AnswerStatus::Answered,
        reason: None,
        answered_by: Party {
            peer_id: bob.identity.peer_id.clone(),
            agent_label: "bob".to_owned(),
            user_id: bob.user,
        },
        body_hash: sha256_hex(body.as_bytes()),
        body_bytes: u32::try_from(body.len()).unwrap(),
        refs: vec![],
        sent_at: NOW,
    };
    let wire = Wire::Answer(bob.identity.sign_answer(&answer, body));
    assert_eq!(
        alice.daemon.handle_wire(&bob.identity.peer_id, wire.clone()).await,
        Ack::Accepted
    );
    let reply = alice.daemon.reply_of(sent.request_id).await.unwrap();
    assert_eq!(reply.state, OutState::Answered);
    assert_eq!(reply.answer, None, "the text was dropped");
    assert_eq!(reply.answer_blocked.as_deref(), Some("secret_assignment"));
    assert!(!reply.summary_required);
    // An answer is taken once, and only from the peer that was asked.
    assert_eq!(
        alice.daemon.handle_wire(&bob.identity.peer_id, wire.clone()).await,
        Ack::Rejected(RejectReason::Replay)
    );
    assert_eq!(
        alice.daemon.handle_wire(&robo.identity.peer_id, wire).await,
        Ack::Rejected(RejectReason::Invalid)
    );
}

/// The hash of a request Alice sent, recovered from her spool.
async fn alice_request_hash(alice: &Node, request_id: Uuid) -> String {
    let path = alice.dir.join("spool").join("out").join(format!("{request_id}.json"));
    let record: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    // Once delivered, the sender keeps the request's hash but not its text.
    assert_eq!(record["request"]["body"], "");
    record["hash"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn consent_times_out_and_trust_skips_it() {
    let net = Net::new();
    net.policy(|p| p.limits.consent_timeout_secs = 120);
    let alice = net
        .join("alice", Some(TeamRole::Writer), ClientKind::Desktop, AcceptMode::Ask)
        .await;
    let bob = net
        .join("bob", Some(TeamRole::Writer), ClientKind::Desktop, AcceptMode::Trusted)
        .await;

    // Nobody decides: the request expires on both sides.
    let ignored = alice.daemon.send("alice", net.ask(&bob, "anyone?")).await.unwrap();
    settle().await;
    assert_eq!(bob.daemon.pending().await.len(), 1);
    net.advance(121);
    bob.daemon.tick().await;
    settle().await;
    assert_eq!(alice.state_of(ignored.request_id).await.0, OutState::Expired);
    assert!(bob.daemon.pending().await.is_empty());

    // The user declines: the sender is told, the agent never sees it.
    let declined = alice.daemon.send("alice", net.ask(&bob, "please?")).await.unwrap();
    settle().await;
    bob.daemon
        .decide_consent(declined.request_id, false, None)
        .await
        .unwrap();
    settle().await;
    assert_eq!(
        alice.state_of(declined.request_id).await,
        (OutState::Rejected, Some(RejectReason::Consent))
    );
    assert!(bob.daemon.inbox("bob").await.is_empty());
    assert!(matches!(
        bob.daemon.decide_consent(declined.request_id, true, None).await,
        Err(DaemonError::NotFound)
    ));

    // "Allow this peer for an hour": the next request needs no prompt, and
    // after the hour it does again.
    let first = alice.daemon.send("alice", net.ask(&bob, "one")).await.unwrap();
    settle().await;
    bob.daemon
        .decide_consent(first.request_id, true, Some(1))
        .await
        .unwrap();
    alice.daemon.send("alice", net.ask(&bob, "two")).await.unwrap();
    settle().await;
    assert_eq!(bob.daemon.inbox("bob").await.len(), 2);
    assert!(bob.daemon.pending().await.is_empty());
    net.advance(3601);
    bob.daemon.tick().await;
    alice.daemon.tick().await;
    alice.daemon.send("alice", net.ask(&bob, "three")).await.unwrap();
    settle().await;
    assert_eq!(bob.daemon.pending().await.len(), 1);

    // The policy caps the mode: `team` chosen, `ask` allowed.
    net.policy(|p| p.max_accept_mode = AcceptMode::Ask);
    let carol = net
        .join("carol", Some(TeamRole::Writer), ClientKind::Desktop, AcceptMode::Team)
        .await;
    let who = carol.daemon.whoami().await;
    assert_eq!(who.teams[0].accept_mode, AcceptMode::Team);
    assert_eq!(who.teams[0].effective_accept_mode, AcceptMode::Ask);
    alice.daemon.send("alice", net.ask(&carol, "hello")).await.unwrap();
    settle().await;
    assert_eq!(carol.daemon.pending().await.len(), 1);
}

#[tokio::test]
async fn status_is_answered_from_the_agents_record_and_answers_can_be_reviewed() {
    let net = Net::new();
    let alice = net
        .join("alice", Some(TeamRole::Writer), ClientKind::Desktop, AcceptMode::Team)
        .await;
    let bob = net
        .join("bob", Some(TeamRole::Writer), ClientKind::Desktop, AcceptMode::Team)
        .await;
    bob.daemon
        .set_status(
            "bob",
            AgentStatus {
                step: "checking ingress rules".to_owned(),
                findings: "2 of 5 services done".to_owned(),
                busy: true,
            },
        )
        .await
        .unwrap();

    let mut status = net.ask(&bob, "");
    status.kind = MessageKind::Status;
    let sent = alice.daemon.send("alice", status).await.unwrap();
    settle().await;
    let reply = alice.daemon.reply_of(sent.request_id).await.unwrap();
    assert_eq!(reply.state, OutState::Answered);
    let answer: AgentStatus = serde_json::from_str(reply.answer.as_deref().unwrap()).unwrap();
    assert_eq!(answer.step, "checking ingress rules");
    assert!(
        bob.daemon.inbox("bob").await.is_empty(),
        "no model was woken for a status request"
    );

    // A notification is handed to the agent once and needs no answer.
    let mut notify = net.ask(&bob, "summary written to [[P2P: sg-123 (TASK-1)]]");
    notify.kind = MessageKind::Notify;
    let sent = alice.daemon.send("alice", notify).await.unwrap();
    settle().await;
    assert_eq!(bob.daemon.inbox("bob").await.len(), 1);
    assert!(bob.daemon.inbox("bob").await.is_empty());
    assert!(matches!(
        bob.daemon
            .reply("bob", sent.request_id, "thanks".to_owned(), vec![])
            .await,
        Err(DaemonError::NotFound)
    ));

    // "Show the answer before sending": the user confirms the agent's text.
    bob.daemon.set_review_answers(true).await.unwrap();
    let sent = alice.daemon.send("alice", net.ask(&bob, "which hosts?")).await.unwrap();
    settle().await;
    bob.daemon
        .reply("bob", sent.request_id, "host-a and host-b".to_owned(), vec![])
        .await
        .unwrap();
    settle().await;
    assert_eq!(
        alice.state_of(sent.request_id).await.0,
        OutState::Delivered,
        "held for review"
    );
    let waiting = bob.daemon.pending().await;
    assert_eq!(waiting[0].1.as_deref(), Some("host-a and host-b"));
    // Refused: back to the agent, who answers differently; then confirmed.
    bob.daemon.decide_review(sent.request_id, false).await.unwrap();
    assert_eq!(bob.daemon.inbox("bob").await.len(), 1);
    bob.daemon
        .reply(
            "bob",
            sent.request_id,
            "two hosts in the ops inventory".to_owned(),
            vec![],
        )
        .await
        .unwrap();
    bob.daemon.decide_review(sent.request_id, true).await.unwrap();
    settle().await;
    assert_eq!(
        alice.daemon.reply_of(sent.request_id).await.unwrap().answer.as_deref(),
        Some("two hosts in the ops inventory")
    );
}

#[tokio::test]
async fn chains_stop_at_the_hop_limit() {
    let net = Net::new();
    let a = net
        .join("a", Some(TeamRole::Writer), ClientKind::Server, AcceptMode::Team)
        .await;
    let b = net
        .join("b", Some(TeamRole::Writer), ClientKind::Server, AcceptMode::Team)
        .await;
    let c = net
        .join("c", Some(TeamRole::Writer), ClientKind::Server, AcceptMode::Team)
        .await;

    a.daemon.send("a", net.ask(&b, "who knows?")).await.unwrap();
    settle().await;
    // While handling A's question (same trace), B asks C: one hop further.
    b.daemon.send("b", net.ask(&c, "do you know?")).await.unwrap();
    settle().await;
    assert_eq!(c.daemon.inbox("c").await.len(), 1);
    // C would pass it on again: the policy allows two hops, so it stops,
    // which also ends any A -> B -> C -> A loop.
    assert_eq!(
        refusal(c.daemon.send("c", net.ask(&a, "back to you?")).await),
        RejectReason::Hops
    );
    // A new trace starts a new chain.
    let mut fresh = net.ask(&a, "unrelated");
    fresh.trace_id = Some("TASK-2".to_owned());
    c.daemon.send("c", fresh).await.unwrap();
}

#[tokio::test]
async fn messages_and_settings_survive_a_restart_and_stop_all_stops() {
    let net = Net::new();
    let alice = net
        .join("alice", Some(TeamRole::Writer), ClientKind::Desktop, AcceptMode::Ask)
        .await;
    let bob = net
        .join("bob", Some(TeamRole::Writer), ClientKind::Desktop, AcceptMode::Ask)
        .await;
    let sent = alice
        .daemon
        .send("alice", net.ask(&bob, "before the restart"))
        .await
        .unwrap();
    settle().await;
    bob.daemon.decide_consent(sent.request_id, true, None).await.unwrap();

    // Bob's daemon restarts (the laptop slept, the app was reopened).
    drop(bob.daemon);
    let bob = net
        .start("bob", bob.identity.clone(), bob.user, ClientKind::Desktop)
        .await;
    assert_eq!(bob.daemon.settings().await.teams[0].accept_mode, AcceptMode::Ask);
    bob.daemon.register_agent("bob", &[]).await.unwrap();
    bob.daemon.set_supervisor(true).await;
    let inbox = bob.daemon.inbox("bob").await;
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].body, "before the restart");
    // The same request delivered again after the restart is still a replay.
    let path = alice
        .dir
        .join("spool")
        .join("out")
        .join(format!("{}.json", sent.request_id));
    let record: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let mut signed: Signed = serde_json::from_value(record["request"].clone()).unwrap();
    // Alice dropped the text once it was delivered; a replay carries it.
    assert_eq!(signed.body, "");
    signed.body = "before the restart".to_owned();
    assert_eq!(
        bob.daemon
            .handle_wire(&alice.identity.peer_id, Wire::Request(signed))
            .await,
        Ack::Rejected(RejectReason::Replay)
    );

    // "Stop everything": what waits is refused, presence goes, nothing new
    // is sent or accepted.
    let waiting = alice.daemon.send("alice", net.ask(&bob, "second")).await.unwrap();
    settle().await;
    bob.daemon.stop_all().await;
    settle().await;
    for id in [sent.request_id, waiting.request_id] {
        assert_eq!(
            alice.state_of(id).await,
            (OutState::Rejected, Some(RejectReason::Consent))
        );
    }
    assert!(
        !net.world
            .lock()
            .unwrap()
            .presence
            .iter()
            .any(|(_, peer, _)| *peer == bob.identity.peer_id)
    );
    assert_eq!(
        refusal(bob.daemon.send("bob", net.ask(&alice, "hi")).await),
        RejectReason::Consent
    );
    net.advance(31);
    alice.daemon.tick().await;
    assert!(matches!(
        alice.daemon.send("alice", net.ask(&bob, "third")).await,
        Err(DaemonError::NotFound)
    ));
}

#[tokio::test]
async fn connections_to_peers_the_registry_drops_are_closed() {
    let net = Net::new();
    let alice = net
        .join("alice", Some(TeamRole::Writer), ClientKind::Desktop, AcceptMode::Team)
        .await;
    let bob = net
        .join("bob", Some(TeamRole::Writer), ClientKind::Desktop, AcceptMode::Team)
        .await;
    alice.daemon.send("alice", net.ask(&bob, "hello")).await.unwrap();
    settle().await;
    assert_eq!(alice.daemon.activity().await.connections.len(), 1);

    // Bob's peer is revoked: the registry stops vouching for it.
    net.world.lock().unwrap().peers.remove(&bob.identity.peer_id);
    alice.daemon.tick().await;
    assert!(
        net.hub.disconnected.lock().unwrap().is_empty(),
        "the cached answer is still fresh"
    );
    net.advance(31);
    alice.daemon.tick().await;
    assert_eq!(
        *net.hub.disconnected.lock().unwrap(),
        [(alice.identity.peer_id.clone(), bob.identity.peer_id.clone())]
    );
    assert!(alice.daemon.activity().await.connections.is_empty());
    // And a registry outage refuses new messages instead of trusting a cache.
    net.world.lock().unwrap().down = true;
    net.advance(31);
    assert!(matches!(
        alice.daemon.send("alice", net.ask(&bob, "still there?")).await,
        Err(DaemonError::Registry(_))
    ));
}

#[tokio::test]
async fn one_sender_cannot_fill_the_queue_or_the_consent_prompts() {
    let net = Net::new();
    net.policy(|p| p.limits.rate_per_peer_per_min = 100);
    let alice = net
        .join("alice", Some(TeamRole::Writer), ClientKind::Desktop, AcceptMode::Team)
        .await;
    // Bob is asked about every request: three of Alice's may wait at once.
    let bob = net
        .join("bob", Some(TeamRole::Writer), ClientKind::Desktop, AcceptMode::Ask)
        .await;
    let mut sent = Vec::new();
    for n in 0..4 {
        sent.push(
            alice
                .daemon
                .send("alice", net.ask(&bob, &format!("question {n}")))
                .await
                .unwrap(),
        );
        settle().await;
    }
    assert_eq!(bob.daemon.pending().await.len(), 3);
    assert_eq!(
        alice.state_of(sent[3].request_id).await,
        (OutState::Rejected, Some(RejectReason::Rate))
    );
    // Deciding one makes room for the next.
    bob.daemon
        .decide_consent(sent[0].request_id, false, None)
        .await
        .unwrap();
    let again = alice.daemon.send("alice", net.ask(&bob, "question 4")).await.unwrap();
    settle().await;
    assert_eq!(alice.state_of(again.request_id).await.0, OutState::Delivered);

    // Carol accepts without asking: twenty unanswered requests of one user
    // are kept, the next is refused.
    let carol = net
        .join("carol", Some(TeamRole::Writer), ClientKind::Server, AcceptMode::Team)
        .await;
    let mut last = None;
    for n in 0..21 {
        last = Some(
            alice
                .daemon
                .send("alice", net.ask(&carol, &format!("task {n}")))
                .await
                .unwrap(),
        );
        settle().await;
    }
    assert_eq!(carol.daemon.inbox("carol").await.len(), 20);
    assert_eq!(
        alice.state_of(last.unwrap().request_id).await,
        (OutState::Rejected, Some(RejectReason::Rate))
    );
    // Another user is not affected by Alice's backlog.
    let dave = net
        .join("dave", Some(TeamRole::Writer), ClientKind::Desktop, AcceptMode::Team)
        .await;
    let other = dave.daemon.send("dave", net.ask(&carol, "from dave")).await.unwrap();
    settle().await;
    assert_eq!(dave.state_of(other.request_id).await.0, OutState::Delivered);
}

/// A stored record of `node`, as it is on disk.
fn spooled(node: &Node, kind: &str, request_id: Uuid) -> serde_json::Value {
    let path = node.dir.join("spool").join(kind).join(format!("{request_id}.json"));
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

#[tokio::test]
async fn text_is_not_kept_once_an_exchange_is_over() {
    let net = Net::new();
    let alice = net
        .join("alice", Some(TeamRole::Writer), ClientKind::Desktop, AcceptMode::Team)
        .await;
    let bob = net
        .join("bob", Some(TeamRole::Writer), ClientKind::Desktop, AcceptMode::Team)
        .await;
    let sent = alice
        .daemon
        .send("alice", net.ask(&bob, "which hosts are affected?"))
        .await
        .unwrap();
    settle().await;
    // Delivered: Alice no longer holds the question, Bob does until he answers.
    assert_eq!(spooled(&alice, "out", sent.request_id)["request"]["body"], "");
    assert_eq!(
        spooled(&bob, "in", sent.request_id)["request"]["body"],
        "which hosts are affected?"
    );
    bob.daemon
        .reply("bob", sent.request_id, "host-a and host-b".to_owned(), vec![])
        .await
        .unwrap();
    settle().await;
    let kept = spooled(&bob, "in", sent.request_id);
    assert_eq!(
        (&kept["request"]["body"], &kept["answer"]["body"]),
        (&serde_json::json!(""), &serde_json::json!("")),
        "the receiver keeps headers only"
    );
    assert!(bob.daemon.activity().await.incoming.iter().all(|i| i.body.is_empty()));

    // Alice's agent reads the answer; a little later its text is gone too.
    assert_eq!(
        spooled(&alice, "out", sent.request_id)["answer"]["body"],
        "host-a and host-b",
        "kept until it is read"
    );
    let reply = alice.daemon.reply_of(sent.request_id).await.unwrap();
    assert_eq!(reply.answer.as_deref(), Some("host-a and host-b"));
    net.advance(300);
    alice.daemon.tick().await;
    assert_eq!(
        alice.daemon.reply_of(sent.request_id).await.unwrap().answer.as_deref(),
        Some("host-a and host-b"),
        "still there for a second look"
    );
    net.advance(400);
    alice.daemon.tick().await;
    assert_eq!(spooled(&alice, "out", sent.request_id)["answer"]["body"], "");
    let reply = alice.daemon.reply_of(sent.request_id).await.unwrap();
    assert_eq!(
        (reply.state, reply.answer, reply.answer_dropped),
        (OutState::Answered, None, true)
    );

    // A notification is dropped as soon as the agent has it.
    let mut note = net.ask(&bob, "summary written");
    note.kind = MessageKind::Notify;
    let note = alice.daemon.send("alice", note).await.unwrap();
    settle().await;
    assert_eq!(bob.daemon.inbox("bob").await[0].body, "summary written");
    assert_eq!(spooled(&bob, "in", note.request_id)["request"]["body"], "");
}

#[tokio::test]
async fn a_status_is_checked_like_any_other_text() {
    let net = Net::new();
    let alice = net
        .join("alice", Some(TeamRole::Writer), ClientKind::Desktop, AcceptMode::Team)
        .await;
    let bob = net
        .join("bob", Some(TeamRole::Writer), ClientKind::Desktop, AcceptMode::Team)
        .await;
    // A secret is refused when the agent records it.
    let secret = AgentStatus {
        step: "rotating keys".to_owned(),
        findings: "password: Tr0ub4dor&3xyz".to_owned(),
        busy: true,
    };
    assert_eq!(
        refusal(bob.daemon.set_status("bob", secret).await),
        RejectReason::Pattern
    );
    // The team's own patterns apply when the status is sent.
    bob.daemon
        .set_status(
            "bob",
            AgentStatus {
                step: "working on ACME-4411".to_owned(),
                findings: String::new(),
                busy: true,
            },
        )
        .await
        .unwrap();
    net.policy(|p| p.data.deny_outbound_patterns = vec![r"ACME-\d+".to_owned()]);
    net.advance(31);
    let mut status = net.ask(&bob, "");
    status.kind = MessageKind::Status;
    let sent = alice.daemon.send("alice", status).await.unwrap();
    settle().await;
    let reply = alice.daemon.reply_of(sent.request_id).await.unwrap();
    assert_eq!(
        (reply.state, reply.reason, reply.answer),
        (OutState::Rejected, Some(RejectReason::Pattern), None)
    );
}

#[tokio::test]
async fn answers_and_addresses_are_checked_against_the_registry() {
    let net = Net::new();
    let alice = net
        .join("alice", Some(TeamRole::Writer), ClientKind::Desktop, AcceptMode::Team)
        .await;
    let bob = net
        .join("bob", Some(TeamRole::Writer), ClientKind::Desktop, AcceptMode::Team)
        .await;
    // Only a peer id is looked up: nothing else reaches the registry's URL.
    for bad in ["../../orgs", "12D3KooW?x=1", ""] {
        let mut params = net.ask(&bob, "hello");
        params.to_peer = bad.to_owned();
        assert!(
            matches!(alice.daemon.send("alice", params).await, Err(DaemonError::Invalid(_))),
            "{bad:?}"
        );
    }

    let sent = alice.daemon.send("alice", net.ask(&bob, "still there?")).await.unwrap();
    settle().await;
    let body = "yes".to_owned();
    let answer = AnswerHeader {
        v: 1,
        request_id: sent.request_id,
        request_hash: alice_request_hash(&alice, sent.request_id).await,
        status: AnswerStatus::Answered,
        reason: None,
        answered_by: Party {
            peer_id: bob.identity.peer_id.clone(),
            agent_label: "bob".to_owned(),
            user_id: bob.user,
        },
        body_hash: sha256_hex(body.as_bytes()),
        body_bytes: u32::try_from(body.len()).unwrap(),
        refs: vec![],
        sent_at: NOW,
    };
    let wire = Wire::Answer(bob.identity.sign_answer(&answer, body));
    // Bob's peer was revoked after the question was delivered: his answer,
    // though correctly signed, is not taken.
    net.world.lock().unwrap().peers.remove(&bob.identity.peer_id);
    net.advance(31);
    assert_eq!(
        alice.daemon.handle_wire(&bob.identity.peer_id, wire).await,
        Ack::Rejected(RejectReason::Invalid)
    );
    assert_eq!(alice.state_of(sent.request_id).await.0, OutState::Delivered);
}

#[cfg(unix)]
#[tokio::test]
async fn local_ipc_separates_agents_from_the_desktop_app() {
    use nanace_p2p::ipc::{IpcSecrets, listen};
    use nanace_p2p::ipc_client::{IpcClient, IpcError};
    use secrecy::SecretString;
    use serde_json::json;

    let net = Net::new();
    let alice = net
        .join("alice", Some(TeamRole::Writer), ClientKind::Desktop, AcceptMode::Ask)
        .await;
    let bob = net
        .join("bob", Some(TeamRole::Writer), ClientKind::Desktop, AcceptMode::Ask)
        .await;
    // Bob's daemon starts detached; the IPC sessions below attach to it.
    bob.daemon.unregister_agent("bob").await;
    bob.daemon.set_supervisor(false).await;
    let socket = bob.dir.join("s");
    let secrets = Arc::new(IpcSecrets {
        agent: SecretString::from("agent-secret".to_owned()),
        supervisor: Some(SecretString::from("desktop-secret".to_owned())),
    });
    {
        let (daemon, socket) = (Arc::clone(&bob.daemon), socket.clone());
        tokio::spawn(async move { listen(daemon, secrets, &socket).await });
    }
    settle().await;
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777, 0o600);

    // A wrong secret gets nothing, and an agent session must name its agent.
    assert!(matches!(
        IpcClient::connect(&socket, "guess", Some("bob"), &[]).await,
        Err(IpcError::Rpc { code: -32000, .. })
    ));
    assert!(IpcClient::connect(&socket, "agent-secret", None, &[]).await.is_err());
    assert!(matches!(
        IpcClient::connect(&bob.dir.join("missing"), "agent-secret", Some("bob"), &[]).await,
        Err(IpcError::Unreachable(_))
    ));

    let mut agent = IpcClient::connect(&socket, "agent-secret", Some("bob"), &["Infra".to_owned()])
        .await
        .unwrap();
    let who = agent.call("whoami", json!({})).await.unwrap();
    assert_eq!(who["peer_id"], bob.identity.peer_id.as_str());
    assert_eq!(who["agents"], json!(["bob"]));
    assert_eq!(
        who["teams"][0]["effective_accept_mode"], "off",
        "no desktop app attached yet"
    );

    // The desktop app attaches: now inbound is possible and prompts arrive.
    let mut desktop = IpcClient::connect(&socket, "desktop-secret", None, &[]).await.unwrap();
    let who = desktop.call("whoami", json!({})).await.unwrap();
    assert_eq!(who["teams"][0]["effective_accept_mode"], "ask");
    bob.daemon.tick().await;
    let sent = alice.daemon.send("alice", net.ask(&bob, "which hosts?")).await.unwrap();
    let notice = loop {
        let notice = desktop.next_notice().await.unwrap();
        if notice["event"] == "consent_request" {
            break notice;
        }
    };
    assert_eq!(notice["request"]["body"], "which hosts?");
    assert_eq!(notice["request"]["from_email"], "alice@example.com");

    // The agent cannot approve it, see what is pending, or change settings.
    for (method, params) in [
        (
            "decide_consent",
            json!({ "request_id": sent.request_id, "allow": true }),
        ),
        ("pending", json!({})),
        (
            "set_team",
            json!({ "org_id": net.org, "team_id": net.team, "accept_mode": "team" }),
        ),
        ("stop_all", json!({})),
    ] {
        assert!(
            matches!(
                agent.call(method, params).await,
                Err(IpcError::Rpc { code: -32001, .. })
            ),
            "{method}"
        );
    }
    assert_eq!(agent.call("inbox", json!({})).await.unwrap(), json!([]));

    desktop
        .call(
            "decide_consent",
            json!({ "request_id": sent.request_id, "allow": true }),
        )
        .await
        .unwrap();
    let inbox = agent.call("inbox", json!({})).await.unwrap();
    assert_eq!(inbox[0]["body"], "which hosts?");
    agent
        .call("reply", json!({ "request_id": sent.request_id, "body": "host-a" }))
        .await
        .unwrap();
    settle().await;
    assert_eq!(
        alice.daemon.reply_of(sent.request_id).await.unwrap().answer.as_deref(),
        Some("host-a")
    );

    // A refusal comes back with its reason, not as a generic failure.
    let leak = agent
        .call(
            "ask",
            json!({
                "org_id": net.org, "team_id": net.team, "to_peer": alice.identity.peer_id, "to_agent": "alice",
                "kind": "ask", "body": "token: 9f8e7d6c5b4a3f2e1d0c",
            }),
        )
        .await;
    assert!(
        matches!(&leak, Err(IpcError::Rpc { code: -32002, reason: Some(reason), .. }) if reason == "pattern"),
        "{leak:?}"
    );
    // `ask` with a wait returns the answer in one call.
    alice
        .daemon
        .set_team(net.org, net.team, AcceptMode::Team)
        .await
        .unwrap();
    net.policy(|p| p.max_accept_mode = AcceptMode::Team);
    alice
        .daemon
        .set_status(
            "alice",
            AgentStatus {
                step: "idle".to_owned(),
                ..AgentStatus::default()
            },
        )
        .await
        .unwrap();
    net.advance(31);
    let answered = agent
        .call(
            "ask",
            json!({
                "org_id": net.org, "team_id": net.team, "to_peer": alice.identity.peer_id, "to_agent": "alice",
                "kind": "status", "wait_secs": 5,
            }),
        )
        .await
        .unwrap();
    assert_eq!(answered["state"], "answered");
    assert!(answered["answer"].as_str().unwrap().contains("idle"));
    assert!(agent.call("no_such_method", json!({})).await.is_err());

    // Sessions end: the agent is unregistered, and without the desktop app
    // the laptop accepts nothing again.
    drop(agent);
    drop(desktop);
    settle().await;
    let who = bob.daemon.whoami().await;
    assert!(who.agents.is_empty());
    assert_eq!(who.teams[0].effective_accept_mode, AcceptMode::Off);
}
