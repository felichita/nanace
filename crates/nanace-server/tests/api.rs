//! End-to-end API tests against real PostgreSQL and Qdrant.
//!
//! Start dependencies with `docker compose up -d --wait`, then run
//! `cargo test -p nanace-server -- --ignored`. Override endpoints with
//! `NANACE_TEST_DATABASE_URL` / `NANACE_TEST_QDRANT_URL`. The defaults are the
//! `nanace_test` database and the `qdrant-test` service, never the dev data.

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
        .unwrap_or_else(|_| "postgres://nanace:nanace@127.0.0.1:5432/nanace_test".to_owned());
    let qdrant_url = std::env::var("NANACE_TEST_QDRANT_URL").unwrap_or_else(|_| "http://127.0.0.1:6336".to_owned());
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
        google: None,
        frontend_url: "http://127.0.0.1/auth/complete".to_owned(),
        secure_cookies: false,
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

fn edge_set(graph: &Value, kind: &str) -> Vec<(String, String)> {
    let mut edges: Vec<(String, String)> = graph["edges"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == kind)
        .map(|e| {
            (
                e["source"].as_str().unwrap().to_owned(),
                e["target"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    edges.sort();
    edges
}

fn titles(list: &Value) -> Vec<String> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|c| c["title"].as_str().unwrap().to_owned())
        .collect()
}

#[actix_web::test]
#[ignore = "requires PostgreSQL and Qdrant (docker compose up -d --wait)"]
async fn wiki_links_and_graph_respect_team_access() {
    let state = state(true).await;
    let pool = state.pool.clone();
    let c = Client {
        app: test::init_service(App::new().app_data(state).configure(api::configure)).await,
    };
    let bob_email = email("bob");
    let alice = c.login(&email("alice")).await;
    let bob = c.login(&bob_email).await;
    let org = c
        .ok(
            "POST",
            "/api/v1/orgs",
            &alice,
            Some(json!({ "slug": unique("graph"), "name": "Graph" })),
            StatusCode::CREATED,
        )
        .await;
    let base = format!("/api/v1/orgs/{}", org["id"].as_str().unwrap());
    let mut team_ids = Vec::new();
    for slug in ["docs", "secret"] {
        let team = c
            .ok(
                "POST",
                &format!("{base}/teams"),
                &alice,
                Some(json!({ "slug": slug, "name": slug })),
                StatusCode::CREATED,
            )
            .await;
        team_ids.push(team["id"].as_str().unwrap().to_owned());
    }
    let (docs, secret) = (team_ids[0].clone(), team_ids[1].clone());
    c.ok(
        "PUT",
        &format!("{base}/teams/{docs}/members"),
        &alice,
        Some(json!({ "email": bob_email, "role": "reader" })),
        StatusCode::OK,
    )
    .await;

    let create = |team: String, title: &'static str, content: String| {
        let uri = format!("{base}/teams/{team}/contexts");
        let alice = alice.clone();
        let c = &c;
        async move {
            let body = c
                .ok(
                    "POST",
                    &uri,
                    &alice,
                    Some(json!({ "title": title, "content": content })),
                    StatusCode::CREATED,
                )
                .await;
            body["id"].as_str().unwrap().to_owned()
        }
    };
    let auth = create(
        docs.clone(),
        "Auth Guide",
        "See [[Deploy Guide]] and [[Secret Plan|the plan]]. `[[Not A Link]]` [[Future Page]]".to_owned(),
    )
    .await;
    let deploy = create(
        docs.clone(),
        "Deploy Guide",
        "Deploying. Back to [[ auth guide ]].".to_owned(),
    )
    .await;
    let plan = create(
        secret.clone(),
        "Secret Plan",
        "Classified, see [[Auth Guide]].".to_owned(),
    )
    .await;
    let twin_body = "kubernetes rollout canary traffic split health probes rollback window ".repeat(20);
    let twin_a = create(docs.clone(), "Rollout A", twin_body.clone()).await;
    let twin_b = create(docs.clone(), "Rollout B", twin_body).await;

    let graph_uri = |extra: &str| format!("{base}/graph?min_score=0.9{extra}");
    let pairs = |pairs: &[(&String, &String)]| {
        let mut v: Vec<(String, String)> = pairs.iter().map(|(a, b)| ((*a).clone(), (*b).clone())).collect();
        v.sort();
        v
    };

    // Alice reads both teams: every link resolves, case-insensitively.
    let graph = c.ok("GET", &graph_uri(""), &alice, None, StatusCode::OK).await;
    assert_eq!(graph["nodes"].as_array().unwrap().len(), 5);
    assert_eq!(
        edge_set(&graph, "link"),
        pairs(&[(&auth, &deploy), (&deploy, &auth), (&auth, &plan), (&plan, &auth)])
    );
    let semantic = edge_set(&graph, "semantic");
    assert_eq!(semantic.len(), 1, "{graph}");
    assert!(semantic[0] == (twin_a.clone(), twin_b.clone()) || semantic[0] == (twin_b.clone(), twin_a.clone()));

    // Bob reads only docs: the secret context and its links never appear.
    let graph = c.ok("GET", &graph_uri(""), &bob, None, StatusCode::OK).await;
    let rendered = graph.to_string();
    assert!(
        !rendered.contains(&plan) && !rendered.contains("Secret Plan"),
        "{rendered}"
    );
    assert_eq!(graph["nodes"].as_array().unwrap().len(), 4);
    assert_eq!(edge_set(&graph, "link"), pairs(&[(&auth, &deploy), (&deploy, &auth)]));
    c.ok(
        "GET",
        &graph_uri(&format!("&team_id={secret}")),
        &bob,
        None,
        StatusCode::NOT_FOUND,
    )
    .await;
    let only_docs = c
        .ok(
            "GET",
            &graph_uri(&format!("&team_id={docs}&semantic=false")),
            &alice,
            None,
            StatusCode::OK,
        )
        .await;
    assert_eq!(only_docs["nodes"].as_array().unwrap().len(), 4);
    assert!(edge_set(&only_docs, "semantic").is_empty());
    for bad in ["&neighbors=0", "&neighbors=11", "&min_score=2", "&unknown=1"] {
        c.ok("GET", &graph_uri(bad), &alice, None, StatusCode::BAD_REQUEST)
            .await;
    }

    let links = c
        .ok(
            "GET",
            &format!("{base}/contexts/{auth}/links"),
            &bob,
            None,
            StatusCode::OK,
        )
        .await;
    assert_eq!(titles(&links["outgoing"]), ["Deploy Guide"]);
    assert_eq!(titles(&links["backlinks"]), ["Deploy Guide"]);
    let links = c
        .ok(
            "GET",
            &format!("{base}/contexts/{auth}/links"),
            &alice,
            None,
            StatusCode::OK,
        )
        .await;
    assert_eq!(titles(&links["outgoing"]), ["Deploy Guide", "Secret Plan"]);
    assert_eq!(titles(&links["backlinks"]), ["Deploy Guide", "Secret Plan"]);
    let links = c
        .ok(
            "GET",
            &format!("{base}/contexts/{twin_a}/links"),
            &bob,
            None,
            StatusCode::OK,
        )
        .await;
    assert_eq!(titles(&links["related"])[0], "Rollout B");
    c.ok(
        "GET",
        &format!("{base}/contexts/{plan}/links"),
        &bob,
        None,
        StatusCode::NOT_FOUND,
    )
    .await;

    // RLS on context_links holds below the application: a reader sees no
    // links from unreadable contexts and cannot write links.
    let org_id: Uuid = org["id"].as_str().unwrap().parse().unwrap();
    let bob_id: Uuid = c.ok("GET", "/api/v1/me", &bob, None, StatusCode::OK).await["user_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let mut tx = db::scoped_tx(&pool, org_id, bob_id).await.unwrap();
    let visible: i64 = sqlx::query_scalar("SELECT count(*) FROM context_links WHERE source_id = ANY($1)")
        .bind([plan.parse::<Uuid>().unwrap(), auth.parse::<Uuid>().unwrap()])
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(visible, 3, "auth's three links only, none of the plan's");
    let insert = sqlx::query("INSERT INTO context_links (org_id, source_id, target_key) VALUES ($1, $2, 'x')")
        .bind(org_id)
        .bind(auth.parse::<Uuid>().unwrap())
        .execute(&mut *tx)
        .await;
    assert!(insert.is_err(), "readers cannot insert links");
    drop(tx);

    // A link to a page that does not exist yet resolves once it is created.
    let future = create(docs.clone(), "Future Page", String::new()).await; // blank pages are allowed
    let graph = c
        .ok("GET", &graph_uri("&semantic=false"), &bob, None, StatusCode::OK)
        .await;
    assert!(edge_set(&graph, "link").contains(&(auth.clone(), future.clone())));

    // Edits based on a stale version are rejected.
    let current = c
        .ok(
            "GET",
            &format!("{base}/contexts/{deploy}"),
            &alice,
            None,
            StatusCode::OK,
        )
        .await;
    let edit = |base_updated_at: &Value| {
        Some(json!({
            "title": "Deploy Guide",
            "content": "Deploying, no links now.",
            "base_updated_at": base_updated_at,
        }))
    };
    let updated = c
        .ok(
            "PUT",
            &format!("{base}/contexts/{deploy}"),
            &alice,
            edit(&current["updated_at"]),
            StatusCode::OK,
        )
        .await;
    assert_ne!(updated["updated_at"], current["updated_at"]);
    c.ok(
        "PUT",
        &format!("{base}/contexts/{deploy}"),
        &alice,
        edit(&current["updated_at"]),
        StatusCode::CONFLICT,
    )
    .await;
    let graph = c
        .ok("GET", &graph_uri("&semantic=false"), &alice, None, StatusCode::OK)
        .await;
    assert!(
        !edge_set(&graph, "link").contains(&(deploy.clone(), auth.clone())),
        "edit replaces links"
    );

    // Deleting a context removes its outgoing links with it.
    c.ok(
        "DELETE",
        &format!("{base}/contexts/{auth}"),
        &alice,
        None,
        StatusCode::NO_CONTENT,
    )
    .await;
    let graph = c
        .ok("GET", &graph_uri("&semantic=false"), &alice, None, StatusCode::OK)
        .await;
    assert_eq!(edge_set(&graph, "link"), pairs(&[]));
}

#[actix_web::test]
#[ignore = "requires PostgreSQL and Qdrant (docker compose up -d --wait)"]
async fn trash_restore_and_purge() {
    let c = Client {
        app: test::init_service(App::new().app_data(state(true).await).configure(api::configure)).await,
    };
    let (bob_email, carol_email) = (email("bob"), email("carol"));
    let alice = c.login(&email("alice")).await;
    let bob = c.login(&bob_email).await;
    let carol = c.login(&carol_email).await;
    let alice_id = c.ok("GET", "/api/v1/me", &alice, None, StatusCode::OK).await["user_id"].clone();
    let carol_id = c.ok("GET", "/api/v1/me", &carol, None, StatusCode::OK).await["user_id"].clone();
    let org = c
        .ok(
            "POST",
            "/api/v1/orgs",
            &alice,
            Some(json!({ "slug": unique("trash"), "name": "Trash" })),
            StatusCode::CREATED,
        )
        .await;
    let base = format!("/api/v1/orgs/{}", org["id"].as_str().unwrap());
    let team = c
        .ok(
            "POST",
            &format!("{base}/teams"),
            &alice,
            Some(json!({ "slug": "docs", "name": "Docs" })),
            StatusCode::CREATED,
        )
        .await;
    let team = team["id"].as_str().unwrap();
    for (who, role) in [(&bob_email, "reader"), (&carol_email, "writer")] {
        c.ok(
            "PUT",
            &format!("{base}/teams/{team}/members"),
            &alice,
            Some(json!({ "email": who, "role": role })),
            StatusCode::OK,
        )
        .await;
    }
    let create = |title: &str, content: &str| Some(json!({ "title": title, "content": content, "tags": ["kept"] }));
    let alpha = c
        .ok(
            "POST",
            &format!("{base}/teams/{team}/contexts"),
            &alice,
            create("Alpha", "zebra migration notes, see [[Beta]]"),
            StatusCode::CREATED,
        )
        .await;
    let alpha = alpha["id"].as_str().unwrap().to_owned();
    c.ok(
        "POST",
        &format!("{base}/teams/{team}/contexts"),
        &alice,
        create("Beta", "other"),
        StatusCode::CREATED,
    )
    .await;
    let search = |token: String| {
        let uri = format!("{base}/search");
        let c = &c;
        async move {
            ids(&c
                .ok(
                    "POST",
                    &uri,
                    &token,
                    Some(json!({ "query": "zebra migration" })),
                    StatusCode::OK,
                )
                .await)
        }
    };
    let links = |token: String| {
        let uri = format!("{base}/graph?semantic=false");
        let c = &c;
        async move { edge_set(&c.ok("GET", &uri, &token, None, StatusCode::OK).await, "link") }
    };

    // Readers cannot delete; a writer moves the page to the trash.
    let (status, _) = c
        .call("DELETE", &format!("{base}/contexts/{alpha}"), Some(&bob), None)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    c.ok(
        "DELETE",
        &format!("{base}/contexts/{alpha}"),
        &carol,
        None,
        StatusCode::NO_CONTENT,
    )
    .await;
    c.ok(
        "GET",
        &format!("{base}/contexts/{alpha}"),
        &alice,
        None,
        StatusCode::NOT_FOUND,
    )
    .await;
    assert!(!search(alice.clone()).await.contains(&alpha));
    assert!(links(alice.clone()).await.is_empty());

    // Only writers see the trash.
    let trash = c.ok("GET", &format!("{base}/trash"), &bob, None, StatusCode::OK).await;
    assert_eq!(trash, json!([]));
    let trash = c
        .ok("GET", &format!("{base}/trash"), &alice, None, StatusCode::OK)
        .await;
    assert_eq!(trash.as_array().unwrap().len(), 1);
    assert_eq!(trash[0]["title"], "Alpha");
    assert_eq!(trash[0]["deleted_by_email"], carol_email);
    assert!(trash.to_string().find("zebra").is_none(), "trash lists titles only");
    let outsider = c.login(&email("eve")).await;
    c.ok("GET", &format!("{base}/trash"), &outsider, None, StatusCode::NOT_FOUND)
        .await;

    // Restore keeps the id, author, tags, links and search.
    c.ok(
        "POST",
        &format!("{base}/trash/{alpha}/restore"),
        &bob,
        None,
        StatusCode::NOT_FOUND,
    )
    .await;
    let restored = c
        .ok(
            "POST",
            &format!("{base}/trash/{alpha}/restore"),
            &carol,
            None,
            StatusCode::OK,
        )
        .await;
    assert_eq!(restored["id"], alpha);
    assert_eq!(restored["created_by"], alice_id);
    assert_eq!(restored["updated_by"], carol_id);
    assert_eq!(restored["tags"], json!(["kept"]));
    assert!(search(bob.clone()).await.contains(&alpha));
    assert_eq!(links(bob.clone()).await.len(), 1);
    c.ok(
        "POST",
        &format!("{base}/trash/{alpha}/restore"),
        &carol,
        None,
        StatusCode::NOT_FOUND,
    )
    .await;

    // Purge is permanent.
    c.ok(
        "DELETE",
        &format!("{base}/contexts/{alpha}"),
        &alice,
        None,
        StatusCode::NO_CONTENT,
    )
    .await;
    c.ok(
        "DELETE",
        &format!("{base}/trash/{alpha}"),
        &bob,
        None,
        StatusCode::NOT_FOUND,
    )
    .await;
    c.ok(
        "DELETE",
        &format!("{base}/trash/{alpha}"),
        &alice,
        None,
        StatusCode::NO_CONTENT,
    )
    .await;
    c.ok(
        "POST",
        &format!("{base}/trash/{alpha}/restore"),
        &alice,
        None,
        StatusCode::NOT_FOUND,
    )
    .await;
    assert_eq!(
        c.ok("GET", &format!("{base}/trash"), &alice, None, StatusCode::OK)
            .await,
        json!([])
    );
}

#[actix_web::test]
#[ignore = "requires PostgreSQL and Qdrant (docker compose up -d --wait)"]
async fn org_admins_remove_members_from_the_org() {
    let state = state(true).await;
    let pool = state.pool.clone();
    let c = Client {
        app: test::init_service(App::new().app_data(state).configure(api::configure)).await,
    };
    let (bob_email, carol_email) = (email("bob"), email("carol"));
    let alice = c.login(&email("alice")).await;
    let bob = c.login(&bob_email).await;
    let carol = c.login(&carol_email).await;
    let eve = c.login(&email("eve")).await;
    let me = |token: String| {
        let c = &c;
        async move {
            c.ok("GET", "/api/v1/me", &token, None, StatusCode::OK).await["user_id"]
                .as_str()
                .unwrap()
                .to_owned()
        }
    };
    let (alice_id, bob_id) = (me(alice.clone()).await, me(bob.clone()).await);
    let org = c
        .ok(
            "POST",
            "/api/v1/orgs",
            &alice,
            Some(json!({ "slug": unique("members"), "name": "Members" })),
            StatusCode::CREATED,
        )
        .await;
    let base = format!("/api/v1/orgs/{}", org["id"].as_str().unwrap());
    let team = c
        .ok(
            "POST",
            &format!("{base}/teams"),
            &alice,
            Some(json!({ "slug": "docs", "name": "Docs" })),
            StatusCode::CREATED,
        )
        .await;
    let team = team["id"].as_str().unwrap();
    for (who, role) in [(&bob_email, "reader"), (&carol_email, "writer")] {
        c.ok(
            "PUT",
            &format!("{base}/teams/{team}/members"),
            &alice,
            Some(json!({ "email": who, "role": role })),
            StatusCode::OK,
        )
        .await;
    }
    let page = c
        .ok(
            "POST",
            &format!("{base}/teams/{team}/contexts"),
            &alice,
            Some(json!({ "title": "Plan", "content": "secret plan" })),
            StatusCode::CREATED,
        )
        .await;
    let page_uri = format!("{base}/contexts/{}", page["id"].as_str().unwrap());
    c.ok("GET", &page_uri, &bob, None, StatusCode::OK).await;

    // Only org admins list and remove members; outsiders cannot probe the org.
    c.ok("GET", &format!("{base}/members"), &carol, None, StatusCode::FORBIDDEN)
        .await;
    c.ok(
        "DELETE",
        &format!("{base}/members/{bob_id}"),
        &carol,
        None,
        StatusCode::FORBIDDEN,
    )
    .await;
    c.ok("GET", &format!("{base}/members"), &eve, None, StatusCode::NOT_FOUND)
        .await;
    let members = c
        .ok("GET", &format!("{base}/members"), &alice, None, StatusCode::OK)
        .await;
    let bob_row = members
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["user_id"] == bob_id.as_str())
        .unwrap();
    assert_eq!(bob_row["role"], "member");
    assert_eq!(bob_row["teams"], json!(["Docs"]));
    assert_eq!(members.as_array().unwrap().len(), 3);

    // Removal ends access at once, including team membership.
    c.ok(
        "DELETE",
        &format!("{base}/members/{bob_id}"),
        &alice,
        None,
        StatusCode::NO_CONTENT,
    )
    .await;
    c.ok("GET", &page_uri, &bob, None, StatusCode::NOT_FOUND).await;
    c.ok("GET", &format!("{base}/teams"), &bob, None, StatusCode::NOT_FOUND)
        .await;
    let team_members = c
        .ok(
            "GET",
            &format!("{base}/teams/{team}/members"),
            &alice,
            None,
            StatusCode::OK,
        )
        .await;
    assert!(!team_members.to_string().contains(&bob_email));
    c.ok(
        "DELETE",
        &format!("{base}/members/{bob_id}"),
        &alice,
        None,
        StatusCode::NOT_FOUND,
    )
    .await;

    // The last admin stays.
    c.ok(
        "DELETE",
        &format!("{base}/members/{alice_id}"),
        &alice,
        None,
        StatusCode::CONFLICT,
    )
    .await;

    let audited: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE action = 'org.member.remove' AND resource_id = $1")
            .bind(bob_id.parse::<Uuid>().unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(audited, 1);
}
