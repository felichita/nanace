//! End-to-end API tests against real PostgreSQL and Qdrant.
//!
//! Start dependencies with `docker compose up -d --wait`, then run
//! `cargo test -p nanace-server -- --ignored`. Override endpoints with
//! `NANACE_TEST_DATABASE_URL` / `NANACE_TEST_QDRANT_URL`.

use std::sync::Arc;

use actix_web::dev::{Service, ServiceResponse};
use actix_web::http::StatusCode;
use actix_web::{App, test, web};
use nanace_embed::HashingEmbedder;
use nanace_server::auth::JwtKeys;
use nanace_server::vectors::VectorStore;
use nanace_server::{AppState, api, db};
use secrecy::SecretString;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

async fn state(dev_login: bool) -> web::Data<AppState> {
    let db_url = std::env::var("NANACE_TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://nanace:nanace@127.0.0.1:5432/nanace".to_owned());
    let qdrant_url = std::env::var("NANACE_TEST_QDRANT_URL").unwrap_or_else(|_| "http://127.0.0.1:6334".to_owned());
    let pool = PgPool::connect(&db_url).await.expect("PostgreSQL is reachable");
    db::MIGRATOR.run(&pool).await.expect("migrations apply");
    assert!(
        !db::bypasses_rls(&pool).await.unwrap(),
        "tests must connect as a role subject to RLS (not a superuser)"
    );
    web::Data::new(AppState {
        pool,
        vectors: VectorStore::connect(&qdrant_url, 384).unwrap(),
        embedder: Arc::new(HashingEmbedder::new(384)),
        jwt: JwtKeys::new(
            &SecretString::from("test-secret-0123456789abcdef-0123456789".to_owned()),
            600,
        ),
        dev_login,
    })
}

struct Client<S> {
    app: S,
}

impl<S> Client<S>
where
    S: Service<actix_http::Request, Response = ServiceResponse, Error = actix_web::Error>,
{
    async fn call(&self, method: &str, uri: &str, token: Option<&str>, body: Option<Value>) -> (StatusCode, Value) {
        let mut req = match method {
            "GET" => test::TestRequest::get(),
            "POST" => test::TestRequest::post(),
            "PUT" => test::TestRequest::put(),
            "DELETE" => test::TestRequest::delete(),
            other => panic!("unsupported method {other}"),
        }
        .uri(uri);
        if let Some(token) = token {
            req = req.insert_header(("Authorization", format!("Bearer {token}")));
        }
        if let Some(body) = body {
            req = req.set_json(body);
        }
        let resp = test::call_service(&self.app, req.to_request()).await;
        let status = resp.status();
        let bytes = test::read_body(resp).await;
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into()))
        };
        (status, value)
    }

    async fn login(&self, email: &str) -> String {
        let (status, body) = self
            .call("POST", "/api/v1/auth/dev-login", None, Some(json!({ "email": email })))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["access_token"].as_str().unwrap().to_owned()
    }

    async fn ok(&self, method: &str, uri: &str, token: &str, body: Option<Value>, expected: StatusCode) -> Value {
        let (status, value) = self.call(method, uri, Some(token), body).await;
        assert_eq!(status, expected, "{method} {uri}: {value}");
        value
    }
}

fn unique(prefix: &str) -> String {
    format!("{prefix}-{}", &Uuid::new_v4().simple().to_string()[..12])
}

fn email(name: &str) -> String {
    format!("{name}.{}@example.com", &Uuid::new_v4().simple().to_string()[..8])
}

fn ids(hits: &Value) -> Vec<String> {
    hits.as_array()
        .unwrap()
        .iter()
        .map(|h| h["context_id"].as_str().unwrap().to_owned())
        .collect()
}

#[actix_web::test]
#[ignore = "requires PostgreSQL and Qdrant (docker compose up -d --wait)"]
async fn team_isolation_rbac_and_search() {
    let state = state(true).await;
    let pool = state.pool.clone();
    let c = Client {
        app: test::init_service(App::new().app_data(state).configure(api::configure)).await,
    };
    let (alice_email, bob_email, carol_email) = (email("alice"), email("bob"), email("carol"));
    let alice = c.login(&alice_email).await;
    let bob = c.login(&bob_email).await;
    let carol = c.login(&carol_email).await;
    let dave = c.login(&email("dave")).await;

    let org = c
        .ok(
            "POST",
            "/api/v1/orgs",
            &alice,
            Some(json!({ "slug": unique("example"), "name": "Example" })),
            StatusCode::CREATED,
        )
        .await;
    let org_id = org["id"].as_str().unwrap();
    let base = format!("/api/v1/orgs/{org_id}");

    let dev = c
        .ok(
            "POST",
            &format!("{base}/teams"),
            &alice,
            Some(json!({ "slug": "dev", "name": "Developers" })),
            StatusCode::CREATED,
        )
        .await;
    let qa = c
        .ok(
            "POST",
            &format!("{base}/teams"),
            &alice,
            Some(json!({ "slug": "qa", "name": "QA" })),
            StatusCode::CREATED,
        )
        .await;
    let (dev_id, qa_id) = (dev["id"].as_str().unwrap(), qa["id"].as_str().unwrap());

    // Only org admins create teams.
    c.ok(
        "POST",
        &format!("{base}/teams"),
        &bob,
        Some(json!({ "slug": "ops", "name": "Ops" })),
        StatusCode::NOT_FOUND,
    )
    .await;

    c.ok(
        "PUT",
        &format!("{base}/teams/{qa_id}/members"),
        &alice,
        Some(json!({ "email": bob_email, "role": "writer" })),
        StatusCode::OK,
    )
    .await;
    c.ok(
        "PUT",
        &format!("{base}/teams/{dev_id}/members"),
        &alice,
        Some(json!({ "email": carol_email, "role": "reader" })),
        StatusCode::OK,
    )
    .await;
    // Bob is a QA writer, not a team admin: cannot manage QA membership.
    c.ok(
        "PUT",
        &format!("{base}/teams/{qa_id}/members"),
        &bob,
        Some(json!({ "email": carol_email, "role": "admin" })),
        StatusCode::FORBIDDEN,
    )
    .await;

    let dev_ctx = c
        .ok(
            "POST",
            &format!("{base}/teams/{dev_id}/contexts"),
            &alice,
            Some(json!({
                "title": "API Authentication",
                "content": "## API Authentication\n\nPOST /auth/login\n\nReturns JWT token used to authenticate users.",
                "tags": ["API", "auth"],
            })),
            StatusCode::CREATED,
        )
        .await;
    let dev_ctx_id = dev_ctx["id"].as_str().unwrap().to_owned();
    assert_eq!(dev_ctx["tags"], json!(["api", "auth"]));

    let qa_ctx = c
        .ok(
            "POST",
            &format!("{base}/teams/{qa_id}/contexts"),
            &bob,
            Some(json!({ "title": "Regression plan", "content": "Run the regression suite before each release." })),
            StatusCode::CREATED,
        )
        .await;
    let qa_ctx_id = qa_ctx["id"].as_str().unwrap().to_owned();

    // QA cannot see or touch developer contexts.
    let ctx_uri = format!("{base}/contexts/{dev_ctx_id}");
    c.ok("GET", &ctx_uri, &bob, None, StatusCode::NOT_FOUND).await;
    c.ok(
        "GET",
        &format!("{base}/teams/{dev_id}/contexts"),
        &bob,
        None,
        StatusCode::NOT_FOUND,
    )
    .await;
    c.ok("DELETE", &ctx_uri, &bob, None, StatusCode::NOT_FOUND).await;
    c.ok(
        "POST",
        &format!("{base}/teams/{dev_id}/contexts"),
        &bob,
        Some(json!({ "title": "x", "content": "y" })),
        StatusCode::FORBIDDEN,
    )
    .await;
    let hits = c
        .ok(
            "POST",
            &format!("{base}/search"),
            &bob,
            Some(json!({ "query": "authenticate users JWT token" })),
            StatusCode::OK,
        )
        .await;
    assert!(
        !ids(&hits).contains(&dev_ctx_id),
        "QA search leaked a dev context: {hits}"
    );
    c.ok(
        "POST",
        &format!("{base}/search"),
        &bob,
        Some(json!({ "query": "jwt", "team_ids": [dev_id] })),
        StatusCode::FORBIDDEN,
    )
    .await;

    // Readers read and search but cannot write.
    c.ok("GET", &ctx_uri, &carol, None, StatusCode::OK).await;
    let hits = c
        .ok(
            "POST",
            &format!("{base}/search"),
            &carol,
            Some(json!({ "query": "authenticate users JWT token" })),
            StatusCode::OK,
        )
        .await;
    assert_eq!(ids(&hits).first(), Some(&dev_ctx_id), "{hits}");
    assert!(!ids(&hits).contains(&qa_ctx_id));
    let update = json!({ "title": "API Authentication", "content": "Use OAuth device flow for CLI clients." });
    c.ok("PUT", &ctx_uri, &carol, Some(update.clone()), StatusCode::FORBIDDEN)
        .await;
    c.ok("DELETE", &ctx_uri, &carol, None, StatusCode::FORBIDDEN).await;

    // Updates re-embed: the new wording becomes searchable.
    c.ok("PUT", &ctx_uri, &alice, Some(update), StatusCode::OK).await;
    let hits = c
        .ok(
            "POST",
            &format!("{base}/search"),
            &carol,
            Some(json!({ "query": "OAuth device flow" })),
            StatusCode::OK,
        )
        .await;
    assert_eq!(ids(&hits).first(), Some(&dev_ctx_id), "{hits}");
    assert!(hits[0]["snippet"].as_str().unwrap().contains("device flow"));

    // Outsiders cannot see the org at all.
    c.ok("GET", &format!("{base}/teams"), &dave, None, StatusCode::NOT_FOUND)
        .await;
    c.ok(
        "POST",
        &format!("{base}/search"),
        &dave,
        Some(json!({ "query": "jwt" })),
        StatusCode::NOT_FOUND,
    )
    .await;

    // Removing a member revokes access immediately, despite a valid token.
    let carol_id: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE email = $1")
        .bind(&carol_email)
        .fetch_one(&pool)
        .await
        .unwrap();
    c.ok(
        "DELETE",
        &format!("{base}/teams/{dev_id}/members/{carol_id}"),
        &alice,
        None,
        StatusCode::NO_CONTENT,
    )
    .await;
    c.ok("GET", &ctx_uri, &carol, None, StatusCode::NOT_FOUND).await;

    c.ok("DELETE", &ctx_uri, &alice, None, StatusCode::NO_CONTENT).await;
    c.ok("GET", &ctx_uri, &alice, None, StatusCode::NOT_FOUND).await;
    let hits = c
        .ok(
            "POST",
            &format!("{base}/search"),
            &alice,
            Some(json!({ "query": "OAuth device flow" })),
            StatusCode::OK,
        )
        .await;
    assert!(!ids(&hits).contains(&dev_ctx_id));

    // Audit trail: admins only, and it recorded the lifecycle.
    c.ok("GET", &format!("{base}/audit-logs"), &bob, None, StatusCode::FORBIDDEN)
        .await;
    let logs = c
        .ok("GET", &format!("{base}/audit-logs"), &alice, None, StatusCode::OK)
        .await;
    let actions: Vec<&str> = logs
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["action"].as_str().unwrap())
        .collect();
    for expected in [
        "org.create",
        "team.create",
        "team.member.put",
        "team.member.remove",
        "context.create",
        "context.read",
        "context.update",
        "context.delete",
        "context.search",
    ] {
        assert!(
            actions.contains(&expected),
            "missing audit action {expected}: {actions:?}"
        );
    }
}

#[actix_web::test]
#[ignore = "requires PostgreSQL and Qdrant (docker compose up -d --wait)"]
async fn rls_enforces_isolation_below_the_application() {
    let state = state(true).await;
    let pool = state.pool.clone();
    let c = Client {
        app: test::init_service(App::new().app_data(state).configure(api::configure)).await,
    };
    let bob_email = email("bob");
    let alice = c.login(&email("alice")).await;
    c.login(&bob_email).await;
    let org = c
        .ok(
            "POST",
            "/api/v1/orgs",
            &alice,
            Some(json!({ "slug": unique("rls"), "name": "RLS" })),
            StatusCode::CREATED,
        )
        .await;
    let org_id: Uuid = org["id"].as_str().unwrap().parse().unwrap();
    let base = format!("/api/v1/orgs/{org_id}");
    let dev = c
        .ok(
            "POST",
            &format!("{base}/teams"),
            &alice,
            Some(json!({ "slug": "dev", "name": "Dev" })),
            StatusCode::CREATED,
        )
        .await;
    let qa = c
        .ok(
            "POST",
            &format!("{base}/teams"),
            &alice,
            Some(json!({ "slug": "qa", "name": "QA" })),
            StatusCode::CREATED,
        )
        .await;
    let dev_id: Uuid = dev["id"].as_str().unwrap().parse().unwrap();
    let qa_id = qa["id"].as_str().unwrap();
    let bob_id = c
        .ok(
            "PUT",
            &format!("{base}/teams/{qa_id}/members"),
            &alice,
            Some(json!({ "email": bob_email, "role": "writer" })),
            StatusCode::OK,
        )
        .await["user_id"]
        .as_str()
        .unwrap()
        .parse::<Uuid>()
        .unwrap();
    let ctx = c
        .ok(
            "POST",
            &format!("{base}/teams/{dev_id}/contexts"),
            &alice,
            Some(json!({ "title": "Secret", "content": "dev only" })),
            StatusCode::CREATED,
        )
        .await;
    let ctx_id: Uuid = ctx["id"].as_str().unwrap().parse().unwrap();

    // Bob's scoped transaction sees no dev rows even with an unfiltered query.
    let mut tx = db::scoped_tx(&pool, org_id, bob_id).await.unwrap();
    let visible: i64 = sqlx::query_scalar("SELECT count(*) FROM contexts WHERE team_id = $1")
        .bind(dev_id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(visible, 0);
    let updated = sqlx::query("UPDATE contexts SET title = 'pwned' WHERE id = $1")
        .bind(ctx_id)
        .execute(&mut *tx)
        .await
        .unwrap()
        .rows_affected();
    assert_eq!(updated, 0);
    let insert = sqlx::query(
        "INSERT INTO contexts (id, org_id, team_id, title, content, embedding_hash, embedding_model, chunk_count, created_by, updated_by) \
         VALUES ($1, $2, $3, 't', 'c', 'h', 'm', 0, $4, $4)",
    )
    .bind(Uuid::new_v4())
    .bind(org_id)
    .bind(dev_id)
    .bind(bob_id)
    .execute(&mut *tx)
    .await;
    assert!(insert.is_err(), "RLS must reject writes to another team");
    drop(tx);

    // FORCE RLS: a connection that skips `scoped_tx` sees no contexts at all.
    let unscoped: i64 = sqlx::query_scalar("SELECT count(*) FROM contexts WHERE id = $1")
        .bind(ctx_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(unscoped, 0, "unscoped connection must not read contexts");

    // A scope naming another org sees nothing either.
    let mut tx = db::scoped_tx(&pool, Uuid::new_v4(), bob_id).await.unwrap();
    let visible: i64 = sqlx::query_scalar("SELECT count(*) FROM contexts")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(visible, 0);
}

#[actix_web::test]
#[ignore = "requires PostgreSQL and Qdrant (docker compose up -d --wait)"]
async fn auth_is_required_and_dev_login_is_opt_in() {
    let c = Client {
        app: test::init_service(App::new().app_data(state(false).await).configure(api::configure)).await,
    };
    let (status, _) = c
        .call(
            "POST",
            "/api/v1/auth/dev-login",
            None,
            Some(json!({ "email": "a@example.com" })),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = c.call("GET", "/api/v1/me", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = c.call("GET", "/api/v1/me", Some("garbage"), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, body) = c.call("GET", "/healthz", None, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}
