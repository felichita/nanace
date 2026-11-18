//! P2P registry tests against real PostgreSQL: binding, presence, lookup,
//! team isolation, row-level security and the paths that end a presence.
//!
//! Run like `tests/api.rs`: `cargo test -p nanace-server -- --ignored`.

use std::sync::Arc;

use actix_web::dev::{Service, ServiceResponse};
use actix_web::http::StatusCode;
use actix_web::{App, test, web};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::{Signer, SigningKey};
use nanace_core::p2p::{bind_message, peer_id_from_ed25519};
use nanace_embed::HashingEmbedder;
use nanace_server::auth::JwtKeys;
use nanace_server::p2p::{P2pState, sweep_presence};
use nanace_server::ratelimit::RateLimiter;
use nanace_server::vectors::VectorStore;
use nanace_server::{AppState, api, db};
use secrecy::SecretString;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

const JWT_SECRET: &str = "test-secret-0123456789abcdef-0123456789";

async fn state() -> web::Data<AppState> {
    let db_url = std::env::var("NANACE_TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://nanace:nanace@127.0.0.1:5432/nanace_test".to_owned());
    let qdrant_url = std::env::var("NANACE_TEST_QDRANT_URL").unwrap_or_else(|_| "http://127.0.0.1:6336".to_owned());
    let pool = PgPool::connect(&db_url).await.expect("PostgreSQL is reachable");
    db::MIGRATOR.run(&pool).await.expect("migrations apply");
    assert!(
        !db::bypasses_rls(&pool).await.unwrap(),
        "tests must connect as a role subject to RLS (not a superuser)"
    );
    let secret = SecretString::from(JWT_SECRET.to_owned());
    web::Data::new(AppState {
        pool,
        vectors: VectorStore::connect(&qdrant_url, 384).unwrap(),
        embedder: Arc::new(HashingEmbedder::new(384)),
        jwt: JwtKeys::new(&secret, 600),
        dev_login: true,
        google: None,
        frontend_url: "http://127.0.0.1/auth/complete".to_owned(),
        secure_cookies: false,
        trash_retention_days: 7,
        p2p: P2pState::new(&secret, 120, 3),
        rate_limiter: RateLimiter::default(),
    })
}

struct Client<S> {
    app: S,
}

impl<S> Client<S>
where
    S: Service<actix_http::Request, Response = ServiceResponse, Error = actix_web::Error>,
{
    async fn call(&self, method: &str, uri: &str, token: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut req = match method {
            "GET" => test::TestRequest::get(),
            "POST" => test::TestRequest::post(),
            "PUT" => test::TestRequest::put(),
            "DELETE" => test::TestRequest::delete(),
            other => panic!("unsupported method {other}"),
        }
        .uri(uri);
        if !token.is_empty() {
            req = req.insert_header(("Authorization", format!("Bearer {token}")));
        }
        if let Some(body) = body {
            req = req.set_json(body);
        }
        let resp = test::call_service(&self.app, req.to_request()).await;
        let status = resp.status();
        let bytes = test::read_body(resp).await;
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    async fn ok(&self, method: &str, uri: &str, token: &str, body: Option<Value>, expected: StatusCode) -> Value {
        let (status, value) = self.call(method, uri, token, body).await;
        assert_eq!(status, expected, "{method} {uri}: {value}");
        value
    }

    /// Signs in and returns `(session, user id)`.
    async fn login(&self, name: &str) -> (String, Uuid) {
        let email = format!("{name}.{}@example.com", &Uuid::new_v4().simple().to_string()[..8]);
        let body = self
            .ok(
                "POST",
                "/api/v1/auth/dev-login",
                "",
                Some(json!({ "email": email })),
                StatusCode::OK,
            )
            .await;
        let session = body["access_token"].as_str().unwrap().to_owned();
        let me = self.ok("GET", "/api/v1/me", &session, None, StatusCode::OK).await;
        (session, me["user_id"].as_str().unwrap().parse().unwrap())
    }

    async fn email_of(&self, session: &str) -> String {
        let me = self.ok("GET", "/api/v1/me", session, None, StatusCode::OK).await;
        me["email"].as_str().unwrap().to_owned()
    }

    async fn org(&self, admin: &str) -> (Uuid, String) {
        let slug = format!("p2p-{}", &Uuid::new_v4().simple().to_string()[..12]);
        let org = self
            .ok(
                "POST",
                "/api/v1/orgs",
                admin,
                Some(json!({ "slug": slug, "name": "P2P" })),
                StatusCode::CREATED,
            )
            .await;
        (org["id"].as_str().unwrap().parse().unwrap(), slug)
    }

    async fn team(&self, admin: &str, org: Uuid, slug: &str) -> Uuid {
        let team = self
            .ok(
                "POST",
                &format!("/api/v1/orgs/{org}/teams"),
                admin,
                Some(json!({ "slug": slug, "name": slug })),
                StatusCode::CREATED,
            )
            .await;
        team["id"].as_str().unwrap().parse().unwrap()
    }

    async fn add(&self, admin: &str, org: Uuid, team: Uuid, member: &str, role: &str) {
        let email = self.email_of(member).await;
        self.ok(
            "PUT",
            &format!("/api/v1/orgs/{org}/teams/{team}/members"),
            admin,
            Some(json!({ "email": email, "role": role })),
            StatusCode::OK,
        )
        .await;
    }

    async fn challenge(&self, token: &str) -> String {
        self.ok("POST", "/api/v1/me/peers/challenge", token, None, StatusCode::OK)
            .await["challenge"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    async fn bind(&self, token: &str, user: Uuid, key: &SigningKey) -> String {
        let body = bind_request(&self.challenge(token).await, user, key);
        self.ok("POST", "/api/v1/me/peers", token, Some(body), StatusCode::CREATED)
            .await;
        peer_of(key)
    }

    async fn announce(&self, token: &str, org: Uuid, team: Uuid, peer: &str, label: &str) -> (StatusCode, Value) {
        self.call(
            "PUT",
            &format!("/api/v1/orgs/{org}/teams/{team}/presence"),
            token,
            Some(presence(peer, label)),
        )
        .await
    }
}

fn new_key() -> SigningKey {
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).unwrap();
    SigningKey::from_bytes(&seed)
}

fn peer_of(key: &SigningKey) -> String {
    peer_id_from_ed25519(&key.verifying_key().to_bytes())
}

/// A bind request answering `challenge` with `key` for `user`.
fn bind_request(challenge: &str, user: Uuid, key: &SigningKey) -> Value {
    let peer = peer_of(key);
    let signature = URL_SAFE_NO_PAD.encode(key.sign(&bind_message(challenge, &peer, user)).to_bytes());
    json!({
        "peer_id": peer,
        "challenge": challenge,
        "signature": signature,
        "client_kind": "desktop",
        "client_version": "0.1.4",
    })
}

fn presence(peer: &str, label: &str) -> Value {
    json!({
        "peer_id": peer,
        "agent_label": label,
        "capabilities": ["Infra", "review"],
        "multiaddrs": ["/ip4/203.0.113.7/udp/4001/quic-v1"],
        "accept_mode": "ask",
        "ttl_secs": 60,
    })
}

/// Rows of `table` for `peer`, read with the system scope (the pool role sees
/// nothing under forced row-level security).
async fn rows(pool: &PgPool, table: &str, peer: &str) -> i64 {
    let mut tx = db::p2p_system_tx(pool).await.unwrap();
    let sql = match table {
        "p2p_peers" => "SELECT count(*) FROM p2p_peers WHERE peer_id = $1",
        "p2p_presence" => "SELECT count(*) FROM p2p_presence WHERE peer_id = $1",
        other => panic!("unknown table {other}"),
    };
    sqlx::query_scalar(sql).bind(peer).fetch_one(&mut *tx).await.unwrap()
}

async fn audited(pool: &PgPool, action: &str, peer: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE action = $1 AND details->>'peer_id' = $2")
        .bind(action)
        .bind(peer)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[actix_web::test]
#[ignore = "requires PostgreSQL and Qdrant (docker compose up -d --wait)"]
async fn binding_needs_proof_of_the_key() {
    let state = state().await;
    let pool = state.pool.clone();
    let c = Client {
        app: test::init_service(App::new().app_data(state).configure(api::configure)).await,
    };
    let (alice, alice_id) = c.login("alice").await;
    let (bob, bob_id) = c.login("bob").await;
    let key = new_key();
    let peer = peer_of(&key);
    // A challenge is valid for a minute and may be answered more than once.
    let (alice_challenge, bob_challenge) = (c.challenge(&alice).await, c.challenge(&bob).await);
    let as_alice = |key: &SigningKey| bind_request(&alice_challenge, alice_id, key);
    let as_bob = |key: &SigningKey| bind_request(&bob_challenge, bob_id, key);

    // A signature by another key, for another user, or over a challenge
    // issued to someone else proves nothing.
    let mut wrong_key = as_alice(&new_key());
    wrong_key["peer_id"] = json!(peer);
    let wrong_user = bind_request(&alice_challenge, bob_id, &key);
    let foreign_challenge = bind_request(&bob_challenge, alice_id, &key);
    let mut unknown_field = as_alice(&key);
    unknown_field["user_id"] = json!(bob_id);
    let mut bad_peer = as_alice(&key);
    bad_peer["peer_id"] = json!("QmYwAPJzv5CZsnA625s3Xf2nemtYgPpHdWEz79ojWnPbdG");
    for body in [wrong_key, wrong_user, foreign_challenge, unknown_field, bad_peer] {
        c.ok("POST", "/api/v1/me/peers", &alice, Some(body), StatusCode::BAD_REQUEST)
            .await;
    }
    assert_eq!(rows(&pool, "p2p_peers", &peer).await, 0);

    // A read-scoped token cannot bind or ask for a challenge.
    let read = c
        .ok(
            "POST",
            "/api/v1/me/tokens",
            &alice,
            Some(json!({ "name": "read", "scope": "read" })),
            StatusCode::CREATED,
        )
        .await["token"]
        .as_str()
        .unwrap()
        .to_owned();
    c.ok("POST", "/api/v1/me/peers/challenge", &read, None, StatusCode::FORBIDDEN)
        .await;
    c.ok("POST", "/api/v1/me/peers/challenge", "", None, StatusCode::UNAUTHORIZED)
        .await;

    c.ok(
        "POST",
        "/api/v1/me/peers",
        &alice,
        Some(as_alice(&key)),
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(audited(&pool, "p2p.peer_bind", &peer).await, 1);
    // Binding again is idempotent and not audited twice.
    let again = as_alice(&key);
    let row = c
        .ok("POST", "/api/v1/me/peers", &alice, Some(again), StatusCode::OK)
        .await;
    assert_eq!(row["peer_id"], peer);
    assert_eq!(audited(&pool, "p2p.peer_bind", &peer).await, 1);

    // Bob holds the key too (say, a copied key file) but the peer is Alice's.
    // He shares no team with her, so the row is invisible to him and the
    // conflict comes from the insert.
    let stolen = as_bob(&key);
    c.ok("POST", "/api/v1/me/peers", &bob, Some(stolen), StatusCode::CONFLICT)
        .await;

    let mine = c.ok("GET", "/api/v1/me/peers", &read, None, StatusCode::OK).await;
    assert_eq!(mine.as_array().unwrap().len(), 1);
    assert_eq!(
        c.ok("GET", "/api/v1/me/peers", &bob, None, StatusCode::OK).await,
        json!([])
    );

    // The per-user cap (3 in this test) counts unrevoked peers only.
    let third = new_key();
    for key in [&new_key(), &third] {
        c.ok(
            "POST",
            "/api/v1/me/peers",
            &alice,
            Some(as_alice(key)),
            StatusCode::CREATED,
        )
        .await;
    }
    let fourth = as_alice(&new_key());
    c.ok(
        "POST",
        "/api/v1/me/peers",
        &alice,
        Some(fourth.clone()),
        StatusCode::CONFLICT,
    )
    .await;

    // Only the owner revokes; a revoked peer id is never bound again.
    let uri = format!("/api/v1/me/peers/{peer}");
    c.ok("DELETE", &uri, &bob, None, StatusCode::NOT_FOUND).await;
    c.ok("DELETE", &uri, &alice, None, StatusCode::NO_CONTENT).await;
    c.ok("DELETE", &uri, &alice, None, StatusCode::NOT_FOUND).await;
    assert_eq!(audited(&pool, "p2p.peer_revoke", &peer).await, 1);
    for (token, body) in [(&alice, as_alice(&key)), (&bob, as_bob(&key))] {
        c.ok("POST", "/api/v1/me/peers", token, Some(body), StatusCode::CONFLICT)
            .await;
    }
    c.ok("POST", "/api/v1/me/peers", &alice, Some(fourth), StatusCode::CREATED)
        .await;

    // Below the API: a revocation is final and a peer keeps its owner.
    let mut tx = db::user_tx(&pool, alice_id).await.unwrap();
    let restore = sqlx::query("UPDATE p2p_peers SET revoked_at = NULL WHERE peer_id = $1")
        .bind(&peer)
        .execute(&mut *tx)
        .await;
    assert!(restore.is_err(), "a revoked peer cannot be restored");
    drop(tx);
    let mut tx = db::user_tx(&pool, alice_id).await.unwrap();
    let give_away = sqlx::query("UPDATE p2p_peers SET user_id = $2 WHERE peer_id = $1")
        .bind(peer_of(&third))
        .bind(bob_id)
        .execute(&mut *tx)
        .await;
    assert!(give_away.is_err(), "a peer cannot change its owner");
    drop(tx);
    let mut tx = db::user_tx(&pool, bob_id).await.unwrap();
    let forged = sqlx::query(
        "INSERT INTO p2p_peers (peer_id, user_id, public_key, client_kind, client_version) \
         VALUES ($1, $2, $3, 'desktop', 'x')",
    )
    .bind(peer_of(&new_key()))
    .bind(alice_id)
    .bind(vec![0u8; 32])
    .execute(&mut *tx)
    .await;
    assert!(forged.is_err(), "RLS must reject a peer bound for another user");
}

#[actix_web::test]
#[ignore = "requires PostgreSQL and Qdrant (docker compose up -d --wait)"]
async fn presence_is_scoped_to_the_team() {
    let state = state().await;
    let pool = state.pool.clone();
    let c = Client {
        app: test::init_service(App::new().app_data(state).configure(api::configure)).await,
    };
    let (alice, alice_id) = c.login("alice").await;
    let (bob, bob_id) = c.login("bob").await;
    let (carol, carol_id) = c.login("carol").await;
    let (dave, dave_id) = c.login("dave").await;
    let (org, _) = c.org(&alice).await;
    let dev = c.team(&alice, org, "dev").await;
    let qa = c.team(&alice, org, "qa").await;
    c.add(&alice, org, dev, &bob, "reader").await;
    c.add(&alice, org, qa, &carol, "writer").await;
    // Dave is in another organization entirely.
    let (other_org, _) = c.org(&dave).await;
    let other_team = c.team(&dave, other_org, "dev").await;

    let alice_peer = c.bind(&alice, alice_id, &new_key()).await;
    let bob_peer = c.bind(&bob, bob_id, &new_key()).await;
    let carol_peer = c.bind(&carol, carol_id, &new_key()).await;
    let dave_peer = c.bind(&dave, dave_id, &new_key()).await;
    let label = "alice/claude-code/s1";
    let presence_uri = format!("/api/v1/orgs/{org}/teams/{dev}/presence");
    let peers_uri = format!("/api/v1/orgs/{org}/teams/{dev}/peers");

    // Bad input is rejected before anything is stored.
    for (field, value) in [
        ("multiaddrs", json!(["/ip4/127.0.0.1/tcp/4001"])),
        ("multiaddrs", json!(["/ip4/203.0.113.7/tcp/4001/p2p-circuit"])),
        ("agent_label", json!("/leading")),
        ("capabilities", json!(["has space"])),
        ("accept_mode", json!("always")),
        ("ttl_secs", json!(121)),
        ("ttl_secs", json!(5)),
        ("peer_id", json!("nope")),
    ] {
        let mut body = presence(&alice_peer, label);
        body[field] = value;
        let (status, value) = c.call("PUT", &presence_uri, &alice, Some(body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{field}: {value}");
    }

    let (status, body) = c.announce(&alice, org, dev, &alice_peer, label).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["heartbeat_secs"], 20);
    // A heartbeat extends the presence without another audit entry.
    assert_eq!(c.announce(&alice, org, dev, &alice_peer, label).await.0, StatusCode::OK);
    assert_eq!(audited(&pool, "p2p.presence_put", &alice_peer).await, 1);
    assert_eq!(rows(&pool, "p2p_presence", &alice_peer).await, 1);

    // Nobody announces a peer that is not theirs, or in a team they are not in.
    assert_eq!(
        c.announce(&bob, org, dev, &alice_peer, "b").await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        c.announce(&carol, org, dev, &carol_peer, "c").await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        c.announce(&dave, org, dev, &dave_peer, "d").await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        c.announce(&alice, other_org, other_team, &alice_peer, "a").await.0,
        StatusCode::NOT_FOUND
    );
    // A reader can be present: presence is not a page write.
    assert_eq!(c.announce(&bob, org, dev, &bob_peer, "bob/cc").await.0, StatusCode::OK);
    assert_eq!(
        c.announce(&carol, org, qa, &carol_peer, "carol/cc").await.0,
        StatusCode::OK
    );

    // Team members see the team's peers; everyone else gets 404.
    let peers = c.ok("GET", &peers_uri, &bob, None, StatusCode::OK).await;
    let listed: Vec<&str> = peers
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["peer_id"].as_str().unwrap())
        .collect();
    assert_eq!(listed.len(), 2);
    assert!(listed.contains(&alice_peer.as_str()) && listed.contains(&bob_peer.as_str()));
    let alice_row = peers
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["peer_id"] == alice_peer.as_str())
        .unwrap();
    assert_eq!(alice_row["capabilities"], json!(["infra", "review"]));
    assert_eq!(alice_row["agent_label"], label);
    assert_eq!(alice_row["client_kind"], "desktop");
    let infra = c
        .ok(
            "GET",
            &format!("{peers_uri}?capability=INFRA"),
            &bob,
            None,
            StatusCode::OK,
        )
        .await;
    assert_eq!(infra.as_array().unwrap().len(), 2);
    let none = c
        .ok(
            "GET",
            &format!("{peers_uri}?capability=frontend"),
            &bob,
            None,
            StatusCode::OK,
        )
        .await;
    assert_eq!(none, json!([]));
    c.ok("GET", &peers_uri, &carol, None, StatusCode::NOT_FOUND).await;
    c.ok("GET", &peers_uri, &dave, None, StatusCode::NOT_FOUND).await;
    c.ok(
        "GET",
        &format!("{peers_uri}?limit=0"),
        &bob,
        None,
        StatusCode::BAD_REQUEST,
    )
    .await;

    // Lookup answers only for a peer present in a team the caller shares.
    let lookup = |peer: &str| format!("/api/v1/me/peers/lookup/{peer}");
    let found = c.ok("GET", &lookup(&alice_peer), &bob, None, StatusCode::OK).await;
    assert_eq!(found["user_id"], alice_id.to_string());
    assert_eq!(
        found["shared"],
        json!([{ "org_id": org, "team_id": dev, "role": "admin" }])
    );
    assert_eq!(
        URL_SAFE_NO_PAD
            .decode(found["public_key"].as_str().unwrap())
            .unwrap()
            .len(),
        32
    );
    for (who, peer) in [
        (&carol, &alice_peer),
        (&dave, &alice_peer),
        (&bob, &carol_peer),
        (&bob, &dave_peer),
        (&bob, &peer_of(&new_key())),
    ] {
        c.ok("GET", &lookup(peer), who, None, StatusCode::NOT_FOUND).await;
    }

    // Below the API: Carol's scope sees no dev presence, Bob cannot write
    // presence for Alice's peer, and an unscoped connection sees nothing.
    let mut tx = db::scoped_tx(&pool, org, carol_id).await.unwrap();
    let visible: i64 = sqlx::query_scalar("SELECT count(*) FROM p2p_presence WHERE team_id = $1")
        .bind(dev)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(visible, 0);
    let stolen = sqlx::query("DELETE FROM p2p_presence WHERE peer_id = $1")
        .bind(&alice_peer)
        .execute(&mut *tx)
        .await
        .unwrap()
        .rows_affected();
    assert_eq!(stolen, 0);
    drop(tx);
    let mut tx = db::scoped_tx(&pool, org, bob_id).await.unwrap();
    let hijacked = sqlx::query("UPDATE p2p_presence SET accept_mode = 'team' WHERE peer_id = $1")
        .bind(&alice_peer)
        .execute(&mut *tx)
        .await
        .unwrap()
        .rows_affected();
    assert_eq!(hijacked, 0, "a teammate cannot change another user's presence");
    let forged = sqlx::query(
        "INSERT INTO p2p_presence (org_id, team_id, user_id, peer_id, agent_label, accept_mode, expires_at) \
         VALUES ($1, $2, $3, $4, 'forged', 'team', now() + interval '1 minute')",
    )
    .bind(org)
    .bind(dev)
    .bind(bob_id)
    .bind(&alice_peer)
    .execute(&mut *tx)
    .await;
    assert!(forged.is_err(), "RLS must reject presence for another user's peer");
    drop(tx);
    // Dave's user scope spans his own orgs only.
    let mut tx = db::user_tx(&pool, dave_id).await.unwrap();
    let visible: i64 = sqlx::query_scalar("SELECT count(*) FROM p2p_presence WHERE org_id = $1")
        .bind(org)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(visible, 0);
    let peers: i64 = sqlx::query_scalar("SELECT count(*) FROM p2p_peers WHERE peer_id = $1")
        .bind(&alice_peer)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(peers, 0);
    drop(tx);
    for sql in ["SELECT count(*) FROM p2p_presence", "SELECT count(*) FROM p2p_peers"] {
        let unscoped: i64 = sqlx::query_scalar(sql).fetch_one(&pool).await.unwrap();
        assert_eq!(unscoped, 0, "unscoped connection must not read P2P tables");
    }

    // Leaving: only the owner, then the presence is gone for everyone.
    let leave = format!("{presence_uri}/{alice_peer}/{label}");
    c.ok("DELETE", &leave, &bob, None, StatusCode::NOT_FOUND).await;
    c.ok("DELETE", &leave, &alice, None, StatusCode::NO_CONTENT).await;
    c.ok("DELETE", &leave, &alice, None, StatusCode::NOT_FOUND).await;
    assert_eq!(audited(&pool, "p2p.presence_delete", &alice_peer).await, 1);
    c.ok("GET", &lookup(&alice_peer), &bob, None, StatusCode::NOT_FOUND)
        .await;

    // Revoking a peer ends its presence at once and it cannot be announced.
    assert_eq!(c.announce(&alice, org, dev, &alice_peer, label).await.0, StatusCode::OK);
    c.ok(
        "DELETE",
        &format!("/api/v1/me/peers/{alice_peer}"),
        &alice,
        None,
        StatusCode::NO_CONTENT,
    )
    .await;
    assert_eq!(rows(&pool, "p2p_presence", &alice_peer).await, 0);
    assert_eq!(
        c.announce(&alice, org, dev, &alice_peer, label).await.0,
        StatusCode::NOT_FOUND
    );
}

#[actix_web::test]
#[ignore = "requires PostgreSQL and Qdrant (docker compose up -d --wait)"]
async fn presence_ends_with_token_membership_account_and_org() {
    let state = state().await;
    let pool = state.pool.clone();
    let c = Client {
        app: test::init_service(App::new().app_data(state).configure(api::configure)).await,
    };
    let (alice, alice_id) = c.login("alice").await;
    let (bob, bob_id) = c.login("bob").await;
    let (org, slug) = c.org(&alice).await;
    let dev = c.team(&alice, org, "dev").await;
    c.add(&alice, org, dev, &bob, "writer").await;
    let alice_peer = c.bind(&alice, alice_id, &new_key()).await;
    let bob_peer = c.bind(&bob, bob_id, &new_key()).await;

    // Revoking the token that announced a presence ends it; a presence
    // announced with another credential stays.
    let token = c
        .ok(
            "POST",
            "/api/v1/me/tokens",
            &bob,
            Some(json!({ "name": "nanace p2p", "scope": "write" })),
            StatusCode::CREATED,
        )
        .await;
    let pat = token["token"].as_str().unwrap();
    assert_eq!(c.announce(pat, org, dev, &bob_peer, "by-token").await.0, StatusCode::OK);
    assert_eq!(
        c.announce(&bob, org, dev, &bob_peer, "by-session").await.0,
        StatusCode::OK
    );
    c.ok(
        "DELETE",
        &format!("/api/v1/me/tokens/{}", token["id"].as_str().unwrap()),
        &bob,
        None,
        StatusCode::NO_CONTENT,
    )
    .await;
    let mut tx = db::p2p_system_tx(&pool).await.unwrap();
    let left: Vec<String> = sqlx::query_scalar("SELECT agent_label FROM p2p_presence WHERE peer_id = $1")
        .bind(&bob_peer)
        .fetch_all(&mut *tx)
        .await
        .unwrap();
    drop(tx);
    assert_eq!(left, ["by-session"]);

    // Removing a member ends their presence in that team.
    c.ok(
        "DELETE",
        &format!("/api/v1/orgs/{org}/teams/{dev}/members/{bob_id}"),
        &alice,
        None,
        StatusCode::NO_CONTENT,
    )
    .await;
    assert_eq!(rows(&pool, "p2p_presence", &bob_peer).await, 0);
    assert_eq!(rows(&pool, "p2p_peers", &bob_peer).await, 1, "the binding stays");

    // Deleting the account removes its peers, though the user row remains.
    c.add(&alice, org, dev, &bob, "writer").await;
    assert_eq!(c.announce(&bob, org, dev, &bob_peer, "again").await.0, StatusCode::OK);
    c.ok("DELETE", "/api/v1/me", &bob, None, StatusCode::NO_CONTENT).await;
    assert_eq!(rows(&pool, "p2p_presence", &bob_peer).await, 0);
    assert_eq!(rows(&pool, "p2p_peers", &bob_peer).await, 0);

    // Deleting the organization removes its presence; the binding is the
    // user's and stays.
    assert_eq!(c.announce(&alice, org, dev, &alice_peer, "a").await.0, StatusCode::OK);
    c.ok(
        "DELETE",
        &format!("/api/v1/orgs/{org}"),
        &alice,
        Some(json!({ "confirm": slug })),
        StatusCode::NO_CONTENT,
    )
    .await;
    assert_eq!(rows(&pool, "p2p_presence", &alice_peer).await, 0);
    assert_eq!(rows(&pool, "p2p_peers", &alice_peer).await, 1);
}

#[actix_web::test]
#[ignore = "requires PostgreSQL and Qdrant (docker compose up -d --wait)"]
async fn expired_presence_is_hidden_then_swept() {
    let state = state().await;
    let pool = state.pool.clone();
    let c = Client {
        app: test::init_service(App::new().app_data(state).configure(api::configure)).await,
    };
    let (alice, alice_id) = c.login("alice").await;
    let (bob, _) = c.login("bob").await;
    let (org, _) = c.org(&alice).await;
    let dev = c.team(&alice, org, "dev").await;
    c.add(&alice, org, dev, &bob, "reader").await;
    let peer = c.bind(&alice, alice_id, &new_key()).await;
    assert_eq!(c.announce(&alice, org, dev, &peer, "live").await.0, StatusCode::OK);
    assert_eq!(c.announce(&alice, org, dev, &peer, "stale").await.0, StatusCode::OK);

    // The system scope deletes nothing that is still live.
    let mut tx = db::p2p_system_tx(&pool).await.unwrap();
    let deleted = sqlx::query("DELETE FROM p2p_presence WHERE peer_id = $1")
        .bind(&peer)
        .execute(&mut *tx)
        .await
        .unwrap()
        .rows_affected();
    assert_eq!(deleted, 0, "system scope must not delete live presence");
    drop(tx);

    let mut tx = db::scoped_tx(&pool, org, alice_id).await.unwrap();
    sqlx::query(
        "UPDATE p2p_presence SET expires_at = now() - interval '1 second' WHERE peer_id = $1 AND agent_label = 'stale'",
    )
    .bind(&peer)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();

    // Reads filter on expiry without waiting for the sweep.
    let peers = c
        .ok(
            "GET",
            &format!("/api/v1/orgs/{org}/teams/{dev}/peers"),
            &bob,
            None,
            StatusCode::OK,
        )
        .await;
    let labels: Vec<&str> = peers
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["agent_label"].as_str().unwrap())
        .collect();
    assert_eq!(labels, ["live"]);
    assert_eq!(rows(&pool, "p2p_presence", &peer).await, 2);

    sweep_presence(&pool).await.unwrap();
    assert_eq!(rows(&pool, "p2p_presence", &peer).await, 1);
    assert_eq!(audited(&pool, "p2p.presence_expire", &peer).await, 1);

    // Announcing the expired label again starts a new presence.
    assert_eq!(c.announce(&alice, org, dev, &peer, "stale").await.0, StatusCode::OK);
    assert_eq!(audited(&pool, "p2p.presence_put", &peer).await, 3);
}

#[actix_web::test]
#[ignore = "requires PostgreSQL and Qdrant (docker compose up -d --wait)"]
async fn challenges_are_rate_limited() {
    let state = state().await;
    let c = Client {
        app: test::init_service(App::new().app_data(state).configure(api::configure)).await,
    };
    let (alice, _) = c.login("alice").await;
    let (bob, _) = c.login("bob").await;
    for _ in 0..10 {
        c.ok("POST", "/api/v1/me/peers/challenge", &alice, None, StatusCode::OK)
            .await;
    }
    let (status, body) = c.call("POST", "/api/v1/me/peers/challenge", &alice, None).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert_eq!(body["error"], "rate_limited");
    // Limits are per user.
    c.ok("POST", "/api/v1/me/peers/challenge", &bob, None, StatusCode::OK)
        .await;
}
