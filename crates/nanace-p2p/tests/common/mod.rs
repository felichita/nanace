//! Shared by the tests that run the real API, a relay node and daemons over
//! libp2p on loopback. Needs PostgreSQL and Qdrant like the other
//! integration tests.

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use actix_web::{App, HttpServer, web};
use nanace_core::p2p::ClientKind;
use nanace_embed::HashingEmbedder;
use nanace_p2p::daemon::Daemon;
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

pub const NODE_SECRET: &str = "node-secret-0123456789abcdef-0123456789";

/// Starts the API and its internal listener; returns their base URLs.
pub async fn start_api() -> (String, String) {
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

pub struct Api {
    pub http: reqwest::Client,
    pub base: String,
}

impl Api {
    pub async fn call(&self, method: reqwest::Method, path: &str, token: &str, body: Option<Value>) -> (u16, Value) {
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

    pub async fn ok(&self, method: reqwest::Method, path: &str, token: &str, body: Option<Value>) -> Value {
        let (status, value) = self.call(method.clone(), path, token, body).await;
        assert!((200..300).contains(&status), "{method} {path}: {status} {value}");
        value
    }

    /// Signs a new user in; returns `(session, email)`.
    pub async fn login(&self, name: &str) -> (String, String) {
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
    pub async fn team(&self, admin: &str, server_peers: &[&str]) -> (Uuid, Uuid) {
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

pub struct Peer {
    pub daemon: Arc<Daemon>,
    pub net: Arc<Net>,
    pub peer_id: String,
    /// The periodic work; aborting it is "the laptop was closed".
    pub ticks: actix_web::rt::task::JoinHandle<()>,
}

/// A daemon with a `p2p`-scoped token, as an agent container would run it.
pub async fn daemon(
    api: &Api,
    session: &str,
    root: &std::path::Path,
    name: &str,
    kind: ClientKind,
    bootstrap: &Bootstrap,
) -> Peer {
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
        kind,
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
    // The test plays the desktop app for a laptop daemon.
    daemon.set_supervisor(kind == ClientKind::Desktop).await;
    let ticks = {
        let daemon = Arc::clone(&daemon);
        actix_web::rt::spawn(async move {
            loop {
                daemon.tick().await;
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
        })
    };
    Peer {
        daemon,
        net,
        peer_id: identity.peer_id,
        ticks,
    }
}

/// Polls until `check` holds, or fails after `secs`.
pub async fn eventually<F, Fut>(what: &str, secs: u64, mut check: F)
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

pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Starts a relay node that admits only peers the API lists, and returns
/// the address daemons bootstrap from.
pub fn start_node(internal_url: String) -> Bootstrap {
    let mut key = [0u8; 32];
    key[..16].copy_from_slice(Uuid::new_v4().as_bytes());
    let port = free_port();
    let peer = nanace_p2p::node::peer_id(key).unwrap();
    actix_web::rt::spawn(nanace_p2p::node::run(NodeConfig {
        key,
        listen: vec![format!("/ip4/127.0.0.1/tcp/{port}").parse().unwrap()],
        external: vec![format!("/ip4/127.0.0.1/tcp/{port}").parse().unwrap()],
        internal_url,
        secret: SecretString::from(NODE_SECRET.to_owned()),
        limits: Limits::default(),
    }));
    Bootstrap::parse(&format!("/ip4/127.0.0.1/tcp/{port}/p2p/{peer}")).unwrap()
}
