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
        trash_retention_days: 7,
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
            "PATCH" => test::TestRequest::patch(),
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
    // Probes need no credentials.
    for probe in ["/livez", "/readyz", "/healthz"] {
        let (status, body) = c.call("GET", probe, None, None).await;
        assert_eq!(status, StatusCode::OK, "{probe}");
        assert_eq!(body["status"], "ok");
    }
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

    // Roles: only admins change them, and the org keeps at least one admin.
    let carol_id = me(carol.clone()).await;
    let role = |r: &str| Some(json!({ "role": r }));
    let member = |id: &str| format!("{base}/members/{id}");
    c.ok("PUT", &member(&carol_id), &carol, role("admin"), StatusCode::FORBIDDEN)
        .await;
    c.ok(
        "PUT",
        &member(&carol_id),
        &alice,
        role("owner"),
        StatusCode::BAD_REQUEST,
    )
    .await;
    c.ok("PUT", &member(&bob_id), &alice, role("admin"), StatusCode::NOT_FOUND)
        .await;
    c.ok("PUT", &member(&carol_id), &alice, role("admin"), StatusCode::NO_CONTENT)
        .await;
    c.ok("GET", &format!("{base}/members"), &carol, None, StatusCode::OK)
        .await;
    c.ok(
        "PUT",
        &member(&alice_id),
        &carol,
        role("member"),
        StatusCode::NO_CONTENT,
    )
    .await;
    c.ok("PUT", &member(&carol_id), &carol, role("member"), StatusCode::CONFLICT)
        .await;
    c.ok("GET", &format!("{base}/members"), &alice, None, StatusCode::FORBIDDEN)
        .await;
    let org_uuid: Uuid = base.rsplit('/').next().unwrap().parse().unwrap();
    let role_changes: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE action = 'org.member.role' AND org_id = $1")
            .bind(org_uuid)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(role_changes, 2);

    let audited: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE action = 'org.member.remove' AND resource_id = $1")
            .bind(bob_id.parse::<Uuid>().unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(audited, 1);
}

#[actix_web::test]
#[ignore = "requires PostgreSQL and Qdrant (docker compose up -d --wait)"]
async fn reindex_rebuilds_qdrant_from_postgres() {
    let state = state(true).await;
    let pool = state.pool.clone();
    let qdrant_url = std::env::var("NANACE_TEST_QDRANT_URL").unwrap_or_else(|_| "http://127.0.0.1:6336".to_owned());
    let vectors = VectorStore::connect(&qdrant_url, 384).unwrap();
    let embedder = HashingEmbedder::new(384);
    let c = Client {
        app: test::init_service(App::new().app_data(state).configure(api::configure)).await,
    };
    let alice = c.login(&email("alice")).await;
    let org = c
        .ok(
            "POST",
            "/api/v1/orgs",
            &alice,
            Some(json!({ "slug": unique("reindex"), "name": "Reindex" })),
            StatusCode::CREATED,
        )
        .await;
    let org_id: Uuid = org["id"].as_str().unwrap().parse().unwrap();
    let base = format!("/api/v1/orgs/{org_id}");
    let mut teams = Vec::new();
    for slug in ["docs", "orphan"] {
        let team = c
            .ok(
                "POST",
                &format!("{base}/teams"),
                &alice,
                Some(json!({ "slug": slug, "name": slug })),
                StatusCode::CREATED,
            )
            .await;
        teams.push(team["id"].as_str().unwrap().to_owned());
    }
    let create = |team: &str, title: &str, content: &str| {
        let uri = format!("{base}/teams/{team}/contexts");
        let body = json!({ "title": title, "content": content });
        let c = &c;
        let alice = alice.clone();
        async move {
            c.ok("POST", &uri, &alice, Some(body), StatusCode::CREATED).await["id"]
                .as_str()
                .unwrap()
                .to_owned()
        }
    };
    let kept = create(&teams[0], "Kept", "glacier migration checklist").await;
    let trashed = create(&teams[0], "Trashed", "glacier migration rollback").await;
    create(&teams[1], "Orphaned", "glacier migration orphan").await;
    c.ok(
        "DELETE",
        &format!("{base}/contexts/{trashed}"),
        &alice,
        None,
        StatusCode::NO_CONTENT,
    )
    .await;
    // The orphan team loses its only member.
    let alice_id = c.ok("GET", "/api/v1/me", &alice, None, StatusCode::OK).await["user_id"].clone();
    c.ok(
        "DELETE",
        &format!("{base}/teams/{}/members/{}", teams[1], alice_id.as_str().unwrap()),
        &alice,
        None,
        StatusCode::NO_CONTENT,
    )
    .await;

    let search = || {
        let uri = format!("{base}/search");
        let c = &c;
        let alice = alice.clone();
        async move {
            ids(&c
                .ok(
                    "POST",
                    &uri,
                    &alice,
                    Some(json!({ "query": "glacier migration" })),
                    StatusCode::OK,
                )
                .await)
        }
    };
    assert!(search().await.contains(&kept));

    // An empty Qdrant (new server, lost volume): nothing is found.
    vectors.recreate_collection(org_id).await.unwrap();
    assert!(search().await.is_empty());

    let report = nanace_server::reindex::reindex(&pool, &vectors, &embedder, Some(org_id))
        .await
        .unwrap();
    assert_eq!(report.orgs, 1);
    assert_eq!(report.contexts, 1, "only the live context of the readable team");
    assert_eq!(report.skipped_teams, vec![(org_id, teams[1].parse().unwrap())]);
    assert_eq!(search().await, vec![kept.clone()]);

    let missing = nanace_server::reindex::reindex(&pool, &vectors, &embedder, Some(Uuid::new_v4())).await;
    assert!(missing.is_err());
}

#[actix_web::test]
#[ignore = "requires PostgreSQL and Qdrant (docker compose up -d --wait)"]
async fn app_upgrades_backfill_links_and_report_status() {
    use nanace_server::upgrades;

    let state = state(true).await;
    let pool = state.pool.clone();
    let qdrant_url = std::env::var("NANACE_TEST_QDRANT_URL").unwrap_or_else(|_| "http://127.0.0.1:6336".to_owned());
    let vectors = VectorStore::connect(&qdrant_url, 384).unwrap();
    let embedder = HashingEmbedder::new(384);
    let model_key = "hashing-v1:384";
    // The test database tracks the hashing model, so no reindex is triggered.
    upgrades::record_embedding_model(&pool, model_key).await.unwrap();
    let c = Client {
        app: test::init_service(App::new().app_data(state).configure(api::configure)).await,
    };
    let alice = c.login(&email("alice")).await;
    let alice_id: Uuid = c.ok("GET", "/api/v1/me", &alice, None, StatusCode::OK).await["user_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let org = c
        .ok(
            "POST",
            "/api/v1/orgs",
            &alice,
            Some(json!({ "slug": unique("upgrade"), "name": "Upgrade" })),
            StatusCode::CREATED,
        )
        .await;
    let org_id: Uuid = org["id"].as_str().unwrap().parse().unwrap();
    let base = format!("/api/v1/orgs/{org_id}");
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
    let mut ids = Vec::new();
    for (title, content) in [("Alpha", "see [[Beta]]"), ("Beta", "plain")] {
        let page = c
            .ok(
                "POST",
                &format!("{base}/teams/{team}/contexts"),
                &alice,
                Some(json!({ "title": title, "content": content })),
                StatusCode::CREATED,
            )
            .await;
        ids.push(page["id"].as_str().unwrap().to_owned());
    }
    let links = || {
        let uri = format!("{base}/graph?semantic=false");
        let c = &c;
        let alice = alice.clone();
        async move { edge_set(&c.ok("GET", &uri, &alice, None, StatusCode::OK).await, "link") }
    };
    assert_eq!(links().await.len(), 1);

    // A context from before links existed: its links were never stored.
    let mut tx = db::scoped_tx(&pool, org_id, alice_id).await.unwrap();
    sqlx::query("DELETE FROM context_links WHERE source_id = $1")
        .bind(ids[0].parse::<Uuid>().unwrap())
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(links().await.is_empty());

    sqlx::query("DELETE FROM app_upgrades WHERE version = 1")
        .execute(&pool)
        .await
        .unwrap();
    let status = upgrades::status(&pool, model_key).await.unwrap();
    assert_eq!(status.pending_upgrades, vec![1]);
    assert!(status.pending_migrations.is_empty() && !status.is_newer());

    let report = upgrades::run(&pool, &vectors, &embedder).await.unwrap();
    assert_eq!(report.upgrades, vec![1]);
    assert!(!report.reindexed);
    assert_eq!(links().await, vec![(ids[0].clone(), ids[1].clone())]);
    assert!(upgrades::status(&pool, model_key).await.unwrap().is_current());
    // Applied once: a second run does nothing.
    assert!(
        upgrades::run(&pool, &vectors, &embedder)
            .await
            .unwrap()
            .upgrades
            .is_empty()
    );

    // A different configured model is reported as a pending reindex.
    let status = upgrades::status(&pool, "other-model:768").await.unwrap();
    assert_eq!(
        status.model_change,
        Some((model_key.to_owned(), "other-model:768".to_owned()))
    );

    // A database upgraded by a newer binary is recognised as newer.
    sqlx::query("INSERT INTO app_upgrades (version, name) VALUES (9999, 'from the future') ON CONFLICT DO NOTHING")
        .execute(&pool)
        .await
        .unwrap();
    let status = upgrades::status(&pool, model_key).await.unwrap();
    sqlx::query("DELETE FROM app_upgrades WHERE version = 9999")
        .execute(&pool)
        .await
        .unwrap();
    assert!(status.is_newer() && status.unknown_upgrades == vec![9999]);
}

#[actix_web::test]
#[ignore = "requires PostgreSQL and Qdrant (docker compose up -d --wait)"]
async fn rls_pins_context_identity_and_trash_rows() {
    let state = state(true).await;
    let pool = state.pool.clone();
    let c = Client {
        app: test::init_service(App::new().app_data(state).configure(api::configure)).await,
    };
    let bob_email = email("bob");
    let alice = c.login(&email("alice")).await;
    let bob = c.login(&bob_email).await;
    let user_id = |token: String| {
        let c = &c;
        async move {
            c.ok("GET", "/api/v1/me", &token, None, StatusCode::OK).await["user_id"]
                .as_str()
                .unwrap()
                .parse::<Uuid>()
                .unwrap()
        }
    };
    let (alice_id, bob_id) = (user_id(alice.clone()).await, user_id(bob.clone()).await);
    let org = c
        .ok(
            "POST",
            "/api/v1/orgs",
            &alice,
            Some(json!({ "slug": unique("pin"), "name": "Pin" })),
            StatusCode::CREATED,
        )
        .await;
    let org_id: Uuid = org["id"].as_str().unwrap().parse().unwrap();
    let base = format!("/api/v1/orgs/{org_id}");
    let mut teams = Vec::new();
    for slug in ["dev", "qa"] {
        let team = c
            .ok(
                "POST",
                &format!("{base}/teams"),
                &alice,
                Some(json!({ "slug": slug, "name": slug })),
                StatusCode::CREATED,
            )
            .await;
        teams.push(team["id"].as_str().unwrap().parse::<Uuid>().unwrap());
    }
    c.ok(
        "PUT",
        &format!("{base}/teams/{}/members", teams[0]),
        &alice,
        Some(json!({ "email": bob_email, "role": "writer" })),
        StatusCode::OK,
    )
    .await;
    let ctx = c
        .ok(
            "POST",
            &format!("{base}/teams/{}/contexts", teams[0]),
            &alice,
            Some(json!({ "title": "Mine", "content": "by alice" })),
            StatusCode::CREATED,
        )
        .await;
    let ctx_id: Uuid = ctx["id"].as_str().unwrap().parse().unwrap();

    // Alice writes to both teams, yet SQL cannot move or re-attribute a context.
    for (sql, value) in [
        (
            "UPDATE contexts SET team_id = $2, updated_by = $3 WHERE id = $1",
            teams[1],
        ),
        (
            "UPDATE contexts SET created_by = $2, updated_by = $3 WHERE id = $1",
            bob_id,
        ),
    ] {
        let mut tx = db::scoped_tx(&pool, org_id, alice_id).await.unwrap();
        let changed = sqlx::query(sql)
            .bind(ctx_id)
            .bind(value)
            .bind(alice_id)
            .execute(&mut *tx)
            .await;
        assert!(changed.is_err(), "{sql}");
    }

    // A trash row must mirror a live context, so it cannot name another
    // author (for a fresh id or for Alice's own context).
    for id in [Uuid::new_v4(), ctx_id] {
        let mut tx = db::scoped_tx(&pool, org_id, bob_id).await.unwrap();
        let forged = sqlx::query(
            "INSERT INTO context_trash (id, org_id, team_id, title, content, tags, created_by, created_at, \
                                        updated_by, updated_at, deleted_by) \
             VALUES ($1, $2, $3, 't', 'c', '{}', $4, now(), $4, now(), $5)",
        )
        .bind(id)
        .bind(org_id)
        .bind(teams[0])
        .bind(if id == ctx_id { bob_id } else { alice_id })
        .bind(bob_id)
        .execute(&mut *tx)
        .await;
        assert!(forged.is_err(), "forged trash row for {id}");
    }

    // The API's own trash and restore still work.
    c.ok(
        "DELETE",
        &format!("{base}/contexts/{ctx_id}"),
        &bob,
        None,
        StatusCode::NO_CONTENT,
    )
    .await;
    let restored = c
        .ok(
            "POST",
            &format!("{base}/trash/{ctx_id}/restore"),
            &bob,
            None,
            StatusCode::OK,
        )
        .await;
    assert_eq!(restored["created_by"], alice_id.to_string());
}

#[actix_web::test]
#[ignore = "requires PostgreSQL and Qdrant (docker compose up -d --wait)"]
async fn expired_trash_is_purged() {
    let state = state(true).await;
    let pool = state.pool.clone();
    let c = Client {
        app: test::init_service(App::new().app_data(state).configure(api::configure)).await,
    };
    let alice = c.login(&email("alice")).await;
    let org = c
        .ok(
            "POST",
            "/api/v1/orgs",
            &alice,
            Some(json!({ "slug": unique("purge"), "name": "Purge" })),
            StatusCode::CREATED,
        )
        .await;
    let org_id: Uuid = org["id"].as_str().unwrap().parse().unwrap();
    let base = format!("/api/v1/orgs/{org_id}");
    let team = c
        .ok(
            "POST",
            &format!("{base}/teams"),
            &alice,
            Some(json!({ "slug": "docs", "name": "Docs" })),
            StatusCode::CREATED,
        )
        .await;
    let page = c
        .ok(
            "POST",
            &format!("{base}/teams/{}/contexts", team["id"].as_str().unwrap()),
            &alice,
            Some(json!({ "title": "Old", "content": "gone soon" })),
            StatusCode::CREATED,
        )
        .await;
    let page_id = page["id"].as_str().unwrap();
    c.ok(
        "DELETE",
        &format!("{base}/contexts/{page_id}"),
        &alice,
        None,
        StatusCode::NO_CONTENT,
    )
    .await;

    // The trash shows when each page goes for good.
    let trash = c
        .ok("GET", &format!("{base}/trash"), &alice, None, StatusCode::OK)
        .await;
    let deleted_at: chrono::DateTime<chrono::Utc> = trash[0]["deleted_at"].as_str().unwrap().parse().unwrap();
    let purge_at: chrono::DateTime<chrono::Utc> = trash[0]["purge_at"].as_str().unwrap().parse().unwrap();
    assert_eq!(purge_at - deleted_at, chrono::Duration::days(7));

    // Within the retention period nothing is purged; past it, the page is.
    assert_eq!(
        nanace_server::retention::purge_expired(&pool, 7, Some(org_id))
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        nanace_server::retention::purge_expired(&pool, 0, Some(org_id))
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        c.ok("GET", &format!("{base}/trash"), &alice, None, StatusCode::OK)
            .await,
        json!([])
    );
    c.ok(
        "POST",
        &format!("{base}/trash/{page_id}/restore"),
        &alice,
        None,
        StatusCode::NOT_FOUND,
    )
    .await;
    let audited: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE action = 'system.trash_purge' AND org_id = $1")
            .bind(org_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(audited, 1);
}

#[actix_web::test]
#[ignore = "requires PostgreSQL and Qdrant (docker compose up -d --wait)"]
async fn orgs_are_renamed_and_deleted_and_accounts_erased() {
    let state = state(true).await;
    let pool = state.pool.clone();
    let c = Client {
        app: test::init_service(App::new().app_data(state).configure(api::configure)).await,
    };
    let (bob_email, carol_email, erin_email) = (email("bob"), email("carol"), email("erin"));
    let alice = c.login(&email("alice")).await;
    let bob = c.login(&bob_email).await;
    let new_org = |token: String, name: &'static str| {
        let c = &c;
        async move {
            let org = c
                .ok(
                    "POST",
                    "/api/v1/orgs",
                    &token,
                    Some(json!({ "slug": unique("del"), "name": name })),
                    StatusCode::CREATED,
                )
                .await;
            (
                org["id"].as_str().unwrap().to_owned(),
                org["slug"].as_str().unwrap().to_owned(),
            )
        }
    };
    let new_team = |token: String, org: String| {
        let c = &c;
        async move {
            c.ok(
                "POST",
                &format!("/api/v1/orgs/{org}/teams"),
                &token,
                Some(json!({ "slug": "docs", "name": "Docs" })),
                StatusCode::CREATED,
            )
            .await["id"]
                .as_str()
                .unwrap()
                .to_owned()
        }
    };
    let invite = |token: String, org: String, team: String, who: String| {
        let c = &c;
        async move {
            c.ok(
                "PUT",
                &format!("/api/v1/orgs/{org}/teams/{team}/members"),
                &token,
                Some(json!({ "email": who, "role": "writer" })),
                StatusCode::OK,
            )
            .await;
        }
    };
    let page = |token: String, org: String, team: String| {
        let c = &c;
        async move {
            c.ok(
                "POST",
                &format!("/api/v1/orgs/{org}/teams/{team}/contexts"),
                &token,
                Some(json!({ "title": "Notes", "content": "kept" })),
                StatusCode::CREATED,
            )
            .await["id"]
                .as_str()
                .unwrap()
                .to_owned()
        }
    };

    // Rename and delete an organization.
    let (org, slug) = new_org(alice.clone(), "Before").await;
    let team = new_team(alice.clone(), org.clone()).await;
    invite(alice.clone(), org.clone(), team.clone(), bob_email.clone()).await;
    let page_id = page(alice.clone(), org.clone(), team.clone()).await;
    let base = format!("/api/v1/orgs/{org}");
    c.ok(
        "PATCH",
        &base,
        &bob,
        Some(json!({ "name": "Nope" })),
        StatusCode::FORBIDDEN,
    )
    .await;
    let renamed = c
        .ok("PATCH", &base, &alice, Some(json!({ "name": "After" })), StatusCode::OK)
        .await;
    assert_eq!(renamed["name"], "After");
    let me = c.ok("GET", "/api/v1/me", &bob, None, StatusCode::OK).await;
    assert!(me.to_string().contains("\"org_name\":\"After\""), "{me}");
    c.ok(
        "DELETE",
        &base,
        &alice,
        Some(json!({ "confirm": "wrong" })),
        StatusCode::BAD_REQUEST,
    )
    .await;
    c.ok(
        "DELETE",
        &base,
        &bob,
        Some(json!({ "confirm": slug })),
        StatusCode::FORBIDDEN,
    )
    .await;
    c.ok(
        "DELETE",
        &base,
        &alice,
        Some(json!({ "confirm": slug })),
        StatusCode::NO_CONTENT,
    )
    .await;
    c.ok(
        "GET",
        &format!("{base}/contexts/{page_id}"),
        &alice,
        None,
        StatusCode::NOT_FOUND,
    )
    .await;
    c.ok("GET", &format!("{base}/teams"), &bob, None, StatusCode::NOT_FOUND)
        .await;
    let org_uuid: Uuid = org.parse().unwrap();
    let (history, deletion): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM audit_logs WHERE org_id = $1), \
                (SELECT count(*) FROM audit_logs WHERE action = 'org.delete' AND resource_id = $1)",
    )
    .bind(org_uuid)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((history, deletion), (0, 1));

    // Account deletion.
    let carol = c.login(&carol_email).await;
    let erin = c.login(&erin_email).await;
    let carol_id = c.ok("GET", "/api/v1/me", &carol, None, StatusCode::OK).await["user_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let erin_id = c.ok("GET", "/api/v1/me", &erin, None, StatusCode::OK).await["user_id"]
        .as_str()
        .unwrap()
        .to_owned();
    // Carol alone in one org, a writer in Bob's org, sole admin of a shared org.
    let (solo, _) = new_org(carol.clone(), "Solo").await;
    new_team(carol.clone(), solo.clone()).await;
    let (shared, _) = new_org(bob.clone(), "Shared").await;
    let shared_team = new_team(bob.clone(), shared.clone()).await;
    invite(bob.clone(), shared.clone(), shared_team.clone(), carol_email.clone()).await;
    let carol_page = page(carol.clone(), shared.clone(), shared_team.clone()).await;
    let (owned, _) = new_org(carol.clone(), "Owned").await;
    let owned_team = new_team(carol.clone(), owned.clone()).await;
    invite(carol.clone(), owned.clone(), owned_team, erin_email.clone()).await;
    let carol_pat = c
        .ok(
            "POST",
            "/api/v1/me/tokens",
            &carol,
            Some(json!({ "name": "cli", "scope": "write" })),
            StatusCode::CREATED,
        )
        .await["token"]
        .as_str()
        .unwrap()
        .to_owned();

    c.ok("DELETE", "/api/v1/me", &carol_pat, None, StatusCode::FORBIDDEN)
        .await;
    let (status, body) = c.call("DELETE", "/api/v1/me", Some(&carol), None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body.to_string().contains("Owned"), "{body}");
    c.ok(
        "PUT",
        &format!("/api/v1/orgs/{owned}/members/{erin_id}"),
        &carol,
        Some(json!({ "role": "admin" })),
        StatusCode::NO_CONTENT,
    )
    .await;
    c.ok("DELETE", "/api/v1/me", &carol, None, StatusCode::NO_CONTENT).await;

    for token in [&carol, &carol_pat] {
        c.ok("GET", "/api/v1/me", token, None, StatusCode::UNAUTHORIZED).await;
    }
    let solo_left: i64 = sqlx::query_scalar("SELECT count(*) FROM organizations WHERE id = $1")
        .bind(solo.parse::<Uuid>().unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(solo_left, 0, "an org with no other members is deleted");
    let kept = c
        .ok(
            "GET",
            &format!("/api/v1/orgs/{shared}/contexts/{carol_page}"),
            &bob,
            None,
            StatusCode::OK,
        )
        .await;
    assert_eq!(kept["created_by"], carol_id, "pages stay with their team");
    let erased: (String, Option<String>) = sqlx::query_as("SELECT email, google_sub FROM users WHERE id = $1")
        .bind(carol_id.parse::<Uuid>().unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(erased.0.starts_with("deleted-") && erased.1.is_none(), "{erased:?}");
    let members = c
        .ok(
            "GET",
            &format!("/api/v1/orgs/{owned}/members"),
            &erin,
            None,
            StatusCode::OK,
        )
        .await;
    assert!(!members.to_string().contains(&carol_email));

    // Signing in again with the same email starts a fresh, empty account.
    let again = c.login(&carol_email).await;
    let fresh = c.ok("GET", "/api/v1/me", &again, None, StatusCode::OK).await;
    assert_ne!(fresh["user_id"], carol_id);
    assert_eq!(fresh["memberships"], json!([]));
}

#[actix_web::test]
#[ignore = "requires PostgreSQL and Qdrant (docker compose up -d --wait)"]
async fn search_snippets_never_show_stale_vector_text() {
    let qdrant_url = std::env::var("NANACE_TEST_QDRANT_URL").unwrap_or_else(|_| "http://127.0.0.1:6336".to_owned());
    let vectors = VectorStore::connect(&qdrant_url, 384).unwrap();
    let embedder = HashingEmbedder::new(384);
    let c = Client {
        app: test::init_service(App::new().app_data(state(true).await).configure(api::configure)).await,
    };
    let alice = c.login(&email("alice")).await;
    let org = c
        .ok(
            "POST",
            "/api/v1/orgs",
            &alice,
            Some(json!({ "slug": unique("stale"), "name": "Stale" })),
            StatusCode::CREATED,
        )
        .await;
    let org_id: Uuid = org["id"].as_str().unwrap().parse().unwrap();
    let base = format!("/api/v1/orgs/{org_id}");
    let team = c
        .ok(
            "POST",
            &format!("{base}/teams"),
            &alice,
            Some(json!({ "slug": "ops", "name": "Ops" })),
            StatusCode::CREATED,
        )
        .await;
    let team_id: Uuid = team["id"].as_str().unwrap().parse().unwrap();
    let old = "Database runbook. The root password is hunter2 for now.";
    let page = c
        .ok(
            "POST",
            &format!("{base}/teams/{team_id}/contexts"),
            &alice,
            Some(json!({ "title": "Runbook", "content": old })),
            StatusCode::CREATED,
        )
        .await;
    let page_id: Uuid = page["id"].as_str().unwrap().parse().unwrap();
    let current = c
        .ok(
            "GET",
            &format!("{base}/contexts/{page_id}"),
            &alice,
            None,
            StatusCode::OK,
        )
        .await;
    c.ok(
        "PUT",
        &format!("{base}/contexts/{page_id}"),
        &alice,
        Some(json!({
            "title": "Runbook",
            "content": "Database runbook. Passwords live in the vault.",
            "base_updated_at": current["updated_at"],
        })),
        StatusCode::OK,
    )
    .await;
    // Vectors left over from the old text (as after a failed commit).
    let stale = nanace_server::vectors::embed_context(&embedder, "Runbook", old)
        .await
        .unwrap();
    vectors.replace_context(org_id, team_id, page_id, stale).await.unwrap();

    let hits = c
        .ok(
            "POST",
            &format!("{base}/search"),
            &alice,
            Some(json!({ "query": "root password hunter2" })),
            StatusCode::OK,
        )
        .await;
    assert_eq!(hits[0]["context_id"], page_id.to_string());
    let snippet = hits[0]["snippet"].as_str().unwrap();
    assert!(!snippet.contains("hunter2") && snippet.contains("vault"), "{snippet}");
}
