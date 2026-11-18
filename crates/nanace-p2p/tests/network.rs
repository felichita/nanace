//! The real thing on one machine: the nanace API with its internal
//! listener, a relay node, and daemons that reach each other through libp2p.
//! Needs PostgreSQL and Qdrant like the other integration tests:
//! `cargo test -p nanace-p2p -- --ignored`.
//!
//! Everything listens on loopback, which daemons never announce, so peers
//! find each other through the relay as they would behind NAT.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use actix_web::{App, HttpServer, web};
use nanace_core::p2p::{AcceptMode, ClientKind, MessageKind, RejectReason};
use nanace_embed::HashingEmbedder;
use nanace_p2p::daemon::{Daemon, DaemonError, OutState, SendParams};
use nanace_p2p::envelope::Identity;
use nanace_p2p::net::{Bootstrap, Net};
use nanace_p2p::node::{Limits, NodeConfig};
use nanace_p2p::registry::HttpRegistry;
use nanace_server::auth::JwtKeys;
use nanace_server::p2p::P2pState;
use nanace_server::ratelimit::RateLimiter;
use nanace_server::vectors::VectorStore;
use nanace_server::{AppState, api, db};
use secrecy::SecretString;
use serde_json::{Value, json};
use uuid::Uuid;

const NODE_SECRET: &str = "node-secret-0123456789abcdef-0123456789";

/// Starts the API and its internal listener; returns their base URLs.
async fn start_api() -> (String, String) {
    let db_url = std::env::var("NANACE_TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://nanace:nanace@127.0.0.1:5432/nanace_test".to_owned());
    let qdrant_url = std::env::var("NANACE_TEST_QDRANT_URL").unwrap_or_else(|_| "http://127.0.0.1:6336".to_owned());
    let pool = sqlx::PgPool::connect(&db_url).await.expect("PostgreSQL is reachable");
    db::MIGRATOR.run(&pool).await.expect("migrations apply");
    assert!(
        !db::bypasses_rls(&pool).await.unwrap(),
        "tests must run as a role subject to RLS"
    );
    let secret = SecretString::from("p2p-net-test-secret-0123456789abcdef".to_owned());
    let state = web::Data::new(AppState {
        pool,
        vectors: VectorStore::connect(&qdrant_url, 384).unwrap(),
        embedder: Arc::new(HashingEmbedder::new(384)),
        jwt: JwtKeys::new(&secret, 3600),
        dev_login: true,
        google: None,
        frontend_url: "http://127.0.0.1/auth/complete".to_owned(),
        secure_cookies: false,
        trash_retention_days: 7,
        p2p: P2pState::new(&secret, 120, 10).with_node_secret(Some(SecretString::from(NODE_SECRET.to_owned()))),
        rate_limiter: RateLimiter::default(),
    });
    let public_state = state.clone();
    let public = HttpServer::new(move || App::new().app_data(public_state.clone()).configure(api::configure))
        .workers(1)
        .bind(("127.0.0.1", 0))
        .unwrap();
    let internal = HttpServer::new(move || App::new().app_data(state.clone()).configure(api::configure_internal))
        .workers(1)
        .bind(("127.0.0.1", 0))
        .unwrap();
    let urls = (
        format!("http://{}", public.addrs()[0]),
        format!("http://{}", internal.addrs()[0]),
    );
    actix_web::rt::spawn(public.run());
    actix_web::rt::spawn(internal.run());
    urls
}

struct Api {
    http: reqwest::Client,
    base: String,
}

impl Api {
    async fn call(&self, method: reqwest::Method, path: &str, token: &str, body: Option<Value>) -> (u16, Value) {
        let mut request = self.http.request(method, format!("{}{path}", self.base));
        if !token.is_empty() {
            request = request.bearer_auth(token);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.unwrap();
        let status = response.status().as_u16();
        (status, response.json().await.unwrap_or(Value::Null))
    }

    async fn ok(&self, method: reqwest::Method, path: &str, token: &str, body: Option<Value>) -> Value {
        let (status, value) = self.call(method.clone(), path, token, body).await;
        assert!((200..300).contains(&status), "{method} {path}: {status} {value}");
        value
    }

    /// Signs a new user in; returns `(session, email)`.
    async fn login(&self, name: &str) -> (String, String) {
        let email = format!("{name}.{}@example.com", &Uuid::new_v4().simple().to_string()[..8]);
        let body = self
            .ok(
                reqwest::Method::POST,
                "/api/v1/auth/dev-login",
                "",
                Some(json!({ "email": email })),
            )
            .await;
        (body["access_token"].as_str().unwrap().to_owned(), email)
    }

    /// An org with one team whose policy lets these accounts run peers
    /// without the desktop app.
    async fn team(&self, admin: &str, server_peers: &[&str]) -> (Uuid, Uuid) {
        let slug = format!("p2pnet-{}", &Uuid::new_v4().simple().to_string()[..10]);
        let org = self
            .ok(
                reqwest::Method::POST,
                "/api/v1/orgs",
                admin,
                Some(json!({ "slug": slug, "name": "P2P" })),
            )
            .await;
        let org_id: Uuid = org["id"].as_str().unwrap().parse().unwrap();
        let team = self
            .ok(
                reqwest::Method::POST,
                &format!("/api/v1/orgs/{org_id}/teams"),
                admin,
                Some(json!({ "slug": "dev", "name": "Dev" })),
            )
            .await;
        let team_id: Uuid = team["id"].as_str().unwrap().parse().unwrap();
        let policy =
            nanace_core::policy::TEMPLATE.replace("users: []", &format!("users: [{}]", server_peers.join(", ")));
        self.ok(
            reqwest::Method::POST,
            &format!("/api/v1/orgs/{org_id}/teams/{team_id}/contexts"),
            admin,
            Some(json!({ "title": "Policy: P2P", "content": policy })),
        )
        .await;
        (org_id, team_id)
    }
}

struct Peer {
    daemon: Arc<Daemon>,
    net: Arc<Net>,
    peer_id: String,
}

/// A daemon with a `p2p`-scoped token, as an agent container would run it.
async fn daemon(api: &Api, session: &str, root: &std::path::Path, name: &str, bootstrap: &Bootstrap) -> Peer {
    let dir: PathBuf = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let token = api
        .ok(
            reqwest::Method::POST,
            "/api/v1/me/tokens",
            session,
            Some(json!({ "name": "nanace p2p", "scope": "p2p" })),
        )
        .await["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let token_file = dir.join("token");
    std::fs::write(&token_file, token).unwrap();
    let mut seed = [0u8; 32];
    seed[..16].copy_from_slice(Uuid::new_v4().as_bytes());
    let identity = Identity::from_secret(seed);
    let listen = [
        "/ip4/127.0.0.1/tcp/0".parse().unwrap(),
        "/ip4/127.0.0.1/udp/0/quic-v1".parse().unwrap(),
    ];
    let net = Net::start(&identity, &listen, bootstrap.clone()).unwrap();
    let daemon = Daemon::start(
        &dir,
        identity.clone(),
        ClientKind::Server,
        Arc::new(HttpRegistry::new(&api.base, token_file).unwrap()),
        net.clone(),
        Arc::new(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64
        }),
    )
    .await
    .unwrap();
    net.attach(Arc::clone(&daemon)).await;
    daemon.register_agent(name, &["infra".to_owned()]).await.unwrap();
    {
        let daemon = Arc::clone(&daemon);
        actix_web::rt::spawn(async move {
            loop {
                daemon.tick().await;
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
        });
    }
    Peer {
        daemon,
        net,
        peer_id: identity.peer_id,
    }
}

/// Polls until `check` holds, or fails after `secs`.
async fn eventually<F, Fut>(what: &str, secs: u64, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    while tokio::time::Instant::now() < deadline {
        if check().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("timed out waiting for: {what}");
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[actix_web::test]
#[ignore = "requires PostgreSQL and Qdrant (docker compose up -d --wait)"]
async fn daemons_talk_through_the_relay_and_strangers_do_not() {
    let (public, internal) = start_api().await;
    let api = Api {
        http: reqwest::Client::new(),
        base: public,
    };
    let root = std::env::temp_dir().join(format!("np-net-{}", &Uuid::new_v4().simple().to_string()[..8]));

    // The relay node, admitting only peers the API lists.
    let mut node_key = [0u8; 32];
    node_key[..16].copy_from_slice(Uuid::new_v4().as_bytes());
    let port = free_port();
    let node_peer = nanace_p2p::node::peer_id(node_key).unwrap();
    actix_web::rt::spawn(nanace_p2p::node::run(NodeConfig {
        key: node_key,
        listen: vec![format!("/ip4/127.0.0.1/tcp/{port}").parse().unwrap()],
        external: vec![format!("/ip4/127.0.0.1/tcp/{port}").parse().unwrap()],
        internal_url: internal,
        secret: SecretString::from(NODE_SECRET.to_owned()),
        limits: Limits::default(),
    }));
    let bootstrap = Bootstrap::parse(&format!("/ip4/127.0.0.1/tcp/{port}/p2p/{node_peer}")).unwrap();

    let (alice_session, alice_email) = api.login("alice").await;
    let (bob_session, bob_email) = api.login("bob").await;
    let (dave_session, dave_email) = api.login("dave").await;
    let (org, team) = api.team(&alice_session, &[&alice_email, &bob_email]).await;
    api.ok(
        reqwest::Method::PUT,
        &format!("/api/v1/orgs/{org}/teams/{team}/members"),
        &alice_session,
        Some(json!({ "email": bob_email, "role": "writer" })),
    )
    .await;
    // Dave has his own organization, with P2P on, and nothing in common.
    let (dave_org, dave_team) = api.team(&dave_session, &[&dave_email]).await;

    let alice = daemon(&api, &alice_session, &root, "alice", &bootstrap).await;
    let bob = daemon(&api, &bob_session, &root, "bob", &bootstrap).await;
    let dave = daemon(&api, &dave_session, &root, "dave", &bootstrap).await;
    alice.daemon.set_team(org, team, AcceptMode::Ask).await.unwrap();
    bob.daemon.set_team(org, team, AcceptMode::Ask).await.unwrap();
    dave.daemon
        .set_team(dave_org, dave_team, AcceptMode::Ask)
        .await
        .unwrap();

    // Presence reaches the registry, and each sees the other by capability.
    eventually("bob listed for alice", 30, || async {
        alice
            .daemon
            .find_peers(org, team, Some("infra"))
            .await
            .is_ok_and(|peers| peers.iter().any(|p| p.peer_id == bob.peer_id && p.agent_label == "bob"))
    })
    .await;

    // Alice asks Bob. The message waits in her queue until the node and Bob
    // have both admitted her, then arrives and waits for consent.
    let question = "Which services depend on security group sg-123?";
    let ask = |to: &Peer, org: Uuid, team: Uuid, to_agent: &str| SendParams {
        org_id: org,
        team_id: team,
        to_peer: to.peer_id.clone(),
        to_agent: to_agent.to_owned(),
        kind: MessageKind::Ask,
        body: question.to_owned(),
        refs: vec![],
        trace_id: Some("TASK-1".to_owned()),
    };
    let sent = alice.daemon.send("alice", ask(&bob, org, team, "bob")).await.unwrap();
    eventually("the question to reach bob", 90, || async {
        !bob.daemon.pending().await.is_empty()
    })
    .await;
    let prompt = bob.daemon.pending().await.remove(0).0;
    assert_eq!(prompt.body, question);
    assert_eq!(prompt.from_email, alice_email);
    assert_eq!(
        alice.daemon.reply_of(sent.request_id).await.unwrap().state,
        OutState::Delivered
    );
    let links = alice.daemon.activity().await.connections;
    assert!(links.iter().any(|c| c.peer_id == bob.peer_id), "{links:?}");

    bob.daemon.decide_consent(sent.request_id, true, None).await.unwrap();
    assert_eq!(bob.daemon.inbox("bob").await.len(), 1);
    bob.daemon
        .reply("bob", sent.request_id, "billing-api and ledger".to_owned(), vec![])
        .await
        .unwrap();
    eventually("the answer to reach alice", 30, || async {
        alice
            .daemon
            .reply_of(sent.request_id)
            .await
            .is_ok_and(|r| r.state == OutState::Answered)
    })
    .await;
    let reply = alice.daemon.reply_of(sent.request_id).await.unwrap();
    assert_eq!(reply.answer.as_deref(), Some("billing-api and ledger"));

    // The server's journal has both sides, verified, and no text.
    let journal_uri = format!("/api/v1/orgs/{org}/teams/{team}/p2p/exchanges");
    eventually("both reports in the journal", 30, || async {
        let journal = api.ok(reqwest::Method::GET, &journal_uri, &alice_session, None).await;
        journal.as_array().is_some_and(|rows| {
            rows.len() == 2
                && rows
                    .iter()
                    .all(|r| r["status"] == "answered" && r["answer_verified"] == true)
        })
    })
    .await;
    let journal = api.ok(reqwest::Method::GET, &journal_uri, &alice_session, None).await;
    assert!(!journal.to_string().contains("sg-123") && !journal.to_string().contains("ledger"));
    assert!(journal[0]["consent"] == "user_approved" || journal[1]["consent"] == "user_approved");

    // Dave is on the same node but in no team of theirs. His daemon will
    // not address Alice, and a message pushed at her peer goes nowhere.
    let refused = dave
        .daemon
        .send("dave", ask(&alice, dave_org, dave_team, "alice"))
        .await;
    assert!(
        matches!(
            refused,
            Err(DaemonError::NotFound)
                | Err(DaemonError::Refused {
                    reason: RejectReason::Policy,
                    ..
                })
        ),
        "{refused:?}"
    );
    use nanace_p2p::transport::Transport;
    let forged = nanace_p2p::envelope::Wire::Request(nanace_p2p::envelope::Signed {
        header: String::new(),
        signature: String::new(),
        body: "hello".to_owned(),
    });
    let pushed = dave.net.send(&alice.peer_id, &[], forged).await;
    assert!(
        pushed.is_err(),
        "alice must not keep a connection with dave: {pushed:?}"
    );
    assert!(alice.daemon.inbox("alice").await.is_empty() && alice.daemon.pending().await.is_empty());

    // Bob's peer is revoked: Alice drops the connection, and a new request
    // is not sent.
    api.ok(
        reqwest::Method::DELETE,
        &format!("/api/v1/me/peers/{}", bob.peer_id),
        &bob_session,
        None,
    )
    .await;
    eventually("alice to drop the revoked peer", 60, || async {
        !alice
            .daemon
            .activity()
            .await
            .connections
            .iter()
            .any(|c| c.peer_id == bob.peer_id)
    })
    .await;
    let after = alice.daemon.send("alice", ask(&bob, org, team, "bob")).await;
    assert!(matches!(after, Err(DaemonError::NotFound)), "{after:?}");
    let _ = std::fs::remove_dir_all(root);
}
