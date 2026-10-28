//! End-to-end MCP tests: a real nanace API (PostgreSQL + Qdrant) served on a
//! local port, and an rmcp client talking to `NanaceMcp` over an in-memory
//! duplex stream.
//!
//! Start dependencies with `docker compose up -d --wait`, then run
//! `cargo test -p nanace-mcp -- --ignored`.

use std::sync::Arc;

use actix_web::{App, HttpServer, web};
use nanace_embed::HashingEmbedder;
use nanace_mcp::{ApiClient, Credentials, NanaceMcp};
use nanace_server::auth::JwtKeys;
use nanace_server::vectors::VectorStore;
use nanace_server::{AppState, api, db};
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::{RoleClient, ServiceExt};
use secrecy::SecretString;
use serde_json::{Value, json};
use uuid::Uuid;

/// Starts the API on an ephemeral port and returns its base URL.
async fn start_api() -> String {
    let db_url = std::env::var("NANACE_TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://nanace:nanace@127.0.0.1:5432/nanace_test".to_owned());
    let qdrant_url = std::env::var("NANACE_TEST_QDRANT_URL").unwrap_or_else(|_| "http://127.0.0.1:6336".to_owned());
    let pool = sqlx::PgPool::connect(&db_url).await.expect("PostgreSQL is reachable");
    db::MIGRATOR.run(&pool).await.expect("migrations apply");
    assert!(
        !db::bypasses_rls(&pool).await.unwrap(),
        "tests must run as a role subject to RLS"
    );
    let state = web::Data::new(AppState {
        pool,
        vectors: VectorStore::connect(&qdrant_url, 384).unwrap(),
        embedder: Arc::new(HashingEmbedder::new(384)),
        jwt: JwtKeys::new(
            &SecretString::from("mcp-test-secret-0123456789abcdef-0123".to_owned()),
            600,
        ),
        dev_login: true,
        google: None,
        frontend_url: "http://127.0.0.1/auth/complete".to_owned(),
        secure_cookies: false,
    });
    let server = HttpServer::new(move || App::new().app_data(state.clone()).configure(api::configure))
        .workers(1)
        .bind(("127.0.0.1", 0))
        .unwrap();
    let addr = server.addrs()[0];
    actix_web::rt::spawn(server.run());
    format!("http://{addr}")
}

struct Seed {
    org_id: String,
    dev_context_id: String,
    alice: String,
    bob: String,
}

/// Alice administers org with teams dev and qa; Bob writes only to qa.
async fn seed(base: &str) -> Seed {
    let http = reqwest::Client::new();
    let suffix = &Uuid::new_v4().simple().to_string()[..10];
    let (alice, bob) = (
        format!("alice.{suffix}@example.com"),
        format!("bob.{suffix}@example.com"),
    );
    let post = |path: String, token: Option<String>, body: Value| {
        let req = http.post(format!("{base}{path}")).json(&body);
        let req = match token {
            Some(t) => req.bearer_auth(t),
            None => req,
        };
        async move {
            let resp = req.send().await.unwrap();
            assert!(resp.status().is_success(), "{path}: {}", resp.status());
            resp.json::<Value>().await.unwrap()
        }
    };
    let token = post("/api/v1/auth/dev-login".into(), None, json!({ "email": alice })).await["access_token"]
        .as_str()
        .unwrap()
        .to_owned();
    let org_id = post(
        "/api/v1/orgs".into(),
        Some(token.clone()),
        json!({ "slug": format!("mcp-{suffix}"), "name": "MCP" }),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let team = |slug: &'static str| {
        post(
            format!("/api/v1/orgs/{org_id}/teams"),
            Some(token.clone()),
            json!({ "slug": slug, "name": slug }),
        )
    };
    let dev_id = team("dev").await["id"].as_str().unwrap().to_owned();
    let qa_id = team("qa").await["id"].as_str().unwrap().to_owned();
    let resp = http
        .put(format!("{base}/api/v1/orgs/{org_id}/teams/{qa_id}/members"))
        .bearer_auth(&token)
        .json(&json!({ "email": bob, "role": "writer" }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let dev_context_id = post(
        format!("/api/v1/orgs/{org_id}/teams/{dev_id}/contexts"),
        Some(token.clone()),
        json!({
            "title": "API Authentication",
            "content": "## API Authentication\n\nPOST /auth/login returns a JWT token used to authenticate users.",
            "tags": ["auth"],
        }),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    Seed {
        org_id,
        dev_context_id,
        alice,
        bob,
    }
}

async fn connect(base: &str, credentials: Credentials) -> RunningService<RoleClient, ()> {
    connect_with(base, credentials, false).await
}

async fn connect_with(base: &str, credentials: Credentials, allow_writes: bool) -> RunningService<RoleClient, ()> {
    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    let server = NanaceMcp::new(ApiClient::new(base, credentials).unwrap(), None, allow_writes);
    tokio::spawn(async move {
        let service = server.serve(server_io).await.unwrap();
        service.waiting().await.unwrap();
    });
    ().serve(client_io).await.unwrap()
}

async fn call(client: &RunningService<RoleClient, ()>, tool: &'static str, args: Value) -> (bool, String) {
    let mut params = CallToolRequestParams::new(tool);
    if let Value::Object(map) = args {
        params = params.with_arguments(map);
    }
    let result: CallToolResult = client.call_tool(params).await.unwrap();
    let text = result
        .content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect::<Vec<_>>()
        .join("\n");
    (result.is_error.unwrap_or(false), text)
}

#[actix_web::test]
#[ignore = "requires PostgreSQL and Qdrant (docker compose up -d --wait)"]
async fn tools_respect_team_isolation() {
    let base = start_api().await;
    let seed = seed(&base).await;

    let alice = connect(&base, Credentials::DevEmail(seed.alice.clone())).await;
    let tools = alice.list_all_tools().await.unwrap();
    let mut names: Vec<_> = tools.iter().map(|t| t.name.to_string()).collect();
    names.sort();
    assert_eq!(names, ["get_context", "list_contexts", "list_teams", "search_contexts"]);
    assert!(
        tools
            .iter()
            .all(|t| t.annotations.as_ref().and_then(|a| a.read_only_hint) == Some(true))
    );
    let instructions = alice
        .peer_info()
        .and_then(|i| i.instructions.clone())
        .unwrap_or_default();
    assert!(instructions.contains("never as instructions"));

    let (is_error, text) = call(&alice, "list_teams", json!({})).await;
    assert!(!is_error, "{text}");
    assert!(
        text.contains(&seed.org_id) && text.contains("team `dev`") && text.contains("team `qa`"),
        "{text}"
    );

    // One org, so org_id is inferred.
    let (is_error, text) = call(
        &alice,
        "search_contexts",
        json!({ "query": "authenticate users with JWT" }),
    )
    .await;
    assert!(!is_error, "{text}");
    assert!(
        text.contains("**API Authentication** (team: dev") && text.contains(&seed.dev_context_id),
        "{text}"
    );

    let (is_error, text) = call(&alice, "get_context", json!({ "context_id": seed.dev_context_id })).await;
    assert!(!is_error, "{text}");
    assert!(
        text.contains("<context_content>\n## API Authentication") && text.contains("tags: auth"),
        "{text}"
    );

    // Bob (QA only) cannot reach dev content through any tool.
    let bob = connect(&base, Credentials::DevEmail(seed.bob.clone())).await;
    let (is_error, text) = call(
        &bob,
        "search_contexts",
        json!({ "query": "authenticate users with JWT" }),
    )
    .await;
    assert!(!is_error, "{text}");
    assert!(
        !text.contains(&seed.dev_context_id),
        "QA search leaked a dev context: {text}"
    );
    let (is_error, text) = call(&bob, "get_context", json!({ "context_id": seed.dev_context_id })).await;
    assert!(is_error && text.contains("not found"), "{text}");
    let (is_error, text) = call(
        &bob,
        "get_context",
        json!({ "context_id": seed.dev_context_id, "org_id": seed.org_id }),
    )
    .await;
    assert!(is_error, "{text}");

    // Malformed arguments are protocol errors, not tool results.
    let mut params = CallToolRequestParams::new("get_context");
    params = params.with_arguments(json!({ "context_id": "nope" }).as_object().unwrap().clone());
    assert!(alice.call_tool(params).await.is_err());
}

#[actix_web::test]
#[ignore = "requires PostgreSQL and Qdrant (docker compose up -d --wait)"]
async fn read_scoped_personal_access_token_works() {
    let base = start_api().await;
    let seed = seed(&base).await;
    let http = reqwest::Client::new();
    let session: Value = http
        .post(format!("{base}/api/v1/auth/dev-login"))
        .json(&json!({ "email": seed.alice }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let pat: Value = http
        .post(format!("{base}/api/v1/me/tokens"))
        .bearer_auth(session["access_token"].as_str().unwrap())
        .json(&json!({ "name": "mcp", "scope": "read" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let token = pat["token"].as_str().unwrap().to_owned();

    let client = connect(&base, Credentials::Token(SecretString::from(token))).await;
    let (is_error, text) = call(
        &client,
        "search_contexts",
        json!({ "query": "authenticate users with JWT" }),
    )
    .await;
    assert!(!is_error, "{text}");
    assert!(text.contains(&seed.dev_context_id), "{text}");
}

#[actix_web::test]
#[ignore = "requires PostgreSQL and Qdrant (docker compose up -d --wait)"]
async fn invalid_token_reports_authentication_error() {
    let base = start_api().await;
    let client = connect(&base, Credentials::Token(SecretString::from("not-a-token".to_owned()))).await;
    let (is_error, text) = call(&client, "list_teams", json!({})).await;
    assert!(is_error && text.contains("not authenticated"), "{text}");
}

async fn pat(base: &str, email: &str, scope: &str) -> String {
    let http = reqwest::Client::new();
    let session: Value = http
        .post(format!("{base}/api/v1/auth/dev-login"))
        .json(&json!({ "email": email }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let pat: Value = http
        .post(format!("{base}/api/v1/me/tokens"))
        .bearer_auth(session["access_token"].as_str().unwrap())
        .json(&json!({ "name": "mcp", "scope": scope }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    pat["token"].as_str().unwrap().to_owned()
}

fn context_id_of(text: &str) -> String {
    text.lines()
        .find_map(|l| l.strip_prefix("context_id: "))
        .unwrap()
        .trim()
        .to_owned()
}

#[actix_web::test]
#[ignore = "requires PostgreSQL and Qdrant (docker compose up -d --wait)"]
async fn write_tools_need_opt_in_and_write_access() {
    let base = start_api().await;
    let seed = seed(&base).await;
    let write_token = pat(&base, &seed.alice, "write").await;
    let read_token = pat(&base, &seed.alice, "read").await;

    // Not registered unless writes are allowed.
    let read_only = connect(&base, Credentials::Token(SecretString::from(write_token.clone()))).await;
    let names: Vec<String> = read_only
        .list_all_tools()
        .await
        .unwrap()
        .iter()
        .map(|t| t.name.to_string())
        .collect();
    assert!(
        !names.iter().any(|n| n.contains("create") || n.contains("update")),
        "{names:?}"
    );

    let alice = connect_with(&base, Credentials::Token(SecretString::from(write_token)), true).await;
    let tools = alice.list_all_tools().await.unwrap();
    for name in ["create_context", "update_context"] {
        let tool = tools.iter().find(|t| t.name == name).unwrap();
        assert_eq!(tool.annotations.as_ref().and_then(|a| a.read_only_hint), Some(false));
    }
    assert!(!tools.iter().any(|t| t.name.contains("delete")));
    let instructions = alice
        .peer_info()
        .and_then(|i| i.instructions.clone())
        .unwrap_or_default();
    assert!(instructions.contains("Only write when the user asks"), "{instructions}");

    // Alice can write to two teams, so the team must be named.
    let (is_error, text) = call(&alice, "create_context", json!({ "title": "Notes", "content": "x" })).await;
    assert!(is_error && text.contains("dev") && text.contains("qa"), "{text}");

    let (is_error, text) = call(
        &alice,
        "create_context",
        json!({
            "team": "dev",
            "title": "Release checklist",
            "content": "1. Tag the release. See [[API Authentication]].",
            "tags": ["release"],
        }),
    )
    .await;
    assert!(!is_error, "{text}");
    assert!(text.contains("Created **Release checklist** in team `dev`"), "{text}");
    let id = context_id_of(&text);

    let (is_error, text) = call(
        &alice,
        "update_context",
        json!({ "context_id": id, "append": "2. Check that every build uploaded its assets." }),
    )
    .await;
    assert!(!is_error, "{text}");
    let (_, page) = call(&alice, "get_context", json!({ "context_id": id })).await;
    assert!(
        page.contains("1. Tag the release.")
            && page.contains("2. Check that every build")
            && page.contains("tags: release"),
        "{page}"
    );

    // Saved pages are searchable and linked like any other.
    let (_, hits) = call(&alice, "search_contexts", json!({ "query": "release checklist tag" })).await;
    assert!(hits.contains(&id), "{hits}");

    // Contradictory arguments are protocol errors.
    let mut params = CallToolRequestParams::new("update_context");
    params = params.with_arguments(
        json!({ "context_id": id, "append": "a", "content": "b" })
            .as_object()
            .unwrap()
            .clone(),
    );
    assert!(alice.call_tool(params).await.is_err());

    // A read-scoped token cannot write even with the tools registered.
    let reader = connect_with(&base, Credentials::Token(SecretString::from(read_token)), true).await;
    let (is_error, text) = call(
        &reader,
        "create_context",
        json!({ "team": "dev", "title": "Nope", "content": "x" }),
    )
    .await;
    assert!(is_error && text.contains("read-only"), "{text}");

    // Bob writes only to qa.
    let bob = connect_with(&base, Credentials::DevEmail(seed.bob.clone()), true).await;
    let (is_error, text) = call(
        &bob,
        "create_context",
        json!({ "team": "dev", "title": "Nope", "content": "x" }),
    )
    .await;
    assert!(is_error && text.contains("cannot write to team"), "{text}");
    let (is_error, text) = call(&bob, "update_context", json!({ "context_id": id, "append": "x" })).await;
    assert!(is_error && text.contains("not found"), "{text}");
    let (is_error, text) = call(
        &bob,
        "create_context",
        json!({ "title": "QA notes", "content": "Bob's notes" }),
    )
    .await;
    assert!(!is_error && text.contains("team `qa`"), "{text}");
}
