//! Google sign-in and personal access token tests against real PostgreSQL
//! and Qdrant, with a local mock of Google's token endpoint.
//!
//! Start dependencies with `docker compose up -d --wait`, then run
//! `cargo test -p nanace-server -- --ignored`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use actix_web::dev::{Service, ServiceResponse};
use actix_web::http::StatusCode;
use actix_web::http::header::{COOKIE, HeaderMap, LOCATION, SET_COOKIE};
use actix_web::{App, HttpResponse, HttpServer, test, web};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use nanace_embed::HashingEmbedder;
use nanace_server::auth::JwtKeys;
use nanace_server::config::GoogleConfig;
use nanace_server::oauth::{GoogleOAuth, pkce_challenge};
use nanace_server::vectors::VectorStore;
use nanace_server::{AppState, api, db};
use secrecy::SecretString;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

const CLIENT_ID: &str = "test-client.apps.googleusercontent.com";
const JWT_SECRET: &str = "auth-test-secret-0123456789abcdef-0123";
const FRONTEND: &str = "http://app.test/signed-in";

/// What the mock token endpoint returns and what it last received.
#[derive(Default)]
struct MockGoogle {
    claims: Value,
    last_form: HashMap<String, String>,
}

async fn start_mock_google(mock: Arc<Mutex<MockGoogle>>) -> String {
    let server = HttpServer::new(move || {
        let mock = mock.clone();
        App::new().route(
            "/token",
            web::post().to(move |form: web::Form<HashMap<String, String>>| {
                let mock = mock.clone();
                async move {
                    let mut mock = mock.lock().unwrap();
                    mock.last_form = form.into_inner();
                    if mock.last_form.get("code").map(String::as_str) == Some("bad-code") {
                        return HttpResponse::BadRequest().json(json!({ "error": "invalid_grant" }));
                    }
                    let enc = |v: &Value| URL_SAFE_NO_PAD.encode(serde_json::to_vec(v).unwrap());
                    let id_token = format!("{}.{}.sig", enc(&json!({ "alg": "RS256" })), enc(&mock.claims));
                    HttpResponse::Ok().json(json!({ "id_token": id_token, "access_token": "unused" }))
                }
            }),
        )
    })
    .workers(1)
    .bind(("127.0.0.1", 0))
    .unwrap();
    let addr = server.addrs()[0];
    actix_web::rt::spawn(server.run());
    format!("http://{addr}/token")
}

async fn state(google: Option<GoogleOAuth>) -> web::Data<AppState> {
    let db_url = std::env::var("NANACE_TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://nanace:nanace@127.0.0.1:5432/nanace".to_owned());
    let qdrant_url = std::env::var("NANACE_TEST_QDRANT_URL").unwrap_or_else(|_| "http://127.0.0.1:6334".to_owned());
    let pool = PgPool::connect(&db_url).await.expect("PostgreSQL is reachable");
    db::MIGRATOR.run(&pool).await.expect("migrations apply");
    assert!(
        !db::bypasses_rls(&pool).await.unwrap(),
        "tests must run as a role subject to RLS"
    );
    web::Data::new(AppState {
        pool,
        vectors: VectorStore::connect(&qdrant_url, 384).unwrap(),
        embedder: Arc::new(HashingEmbedder::new(384)),
        jwt: JwtKeys::new(&SecretString::from(JWT_SECRET.to_owned()), 600),
        dev_login: true,
        google,
        frontend_url: FRONTEND.to_owned(),
        secure_cookies: false,
    })
}

async fn send<S>(app: &S, req: test::TestRequest) -> (StatusCode, Value, HeaderMap)
where
    S: Service<actix_http::Request, Response = ServiceResponse, Error = actix_web::Error>,
{
    let resp = test::call_service(app, req.to_request()).await;
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = test::read_body(resp).await;
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null), headers)
}

fn location(headers: &HeaderMap) -> String {
    headers.get(LOCATION).unwrap().to_str().unwrap().to_owned()
}

fn query(url: &str) -> HashMap<String, String> {
    reqwest::Url::parse(url).unwrap().query_pairs().into_owned().collect()
}

fn unique() -> String {
    Uuid::new_v4().simple().to_string()[..10].to_owned()
}

fn google_claims(sub: &str, email: &str, nonce: &str) -> Value {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    json!({
        "iss": "https://accounts.google.com",
        "aud": CLIENT_ID,
        "sub": sub,
        "exp": now + 300,
        "iat": now,
        "nonce": nonce,
        "email": email,
        "email_verified": true,
        "hd": "example.test",
        "name": "Test User",
    })
}

/// Runs login -> Google -> callback and returns the callback redirect target.
async fn google_round_trip<S>(
    app: &S,
    mock: &Arc<Mutex<MockGoogle>>,
    claims: impl FnOnce(&str) -> Value,
    tamper_state: bool,
) -> String
where
    S: Service<actix_http::Request, Response = ServiceResponse, Error = actix_web::Error>,
{
    let (status, _, resp) = send(app, test::TestRequest::get().uri("/api/v1/auth/google/login")).await;
    assert_eq!(status, StatusCode::FOUND);
    let auth = query(&location(&resp));
    let cookie = resp.get(SET_COOKIE).unwrap().to_str().unwrap();
    assert!(
        cookie.contains("HttpOnly") && cookie.contains("SameSite=Lax"),
        "{cookie}"
    );
    let cookie_pair = cookie.split(';').next().unwrap().to_owned();
    mock.lock().unwrap().claims = claims(&auth["nonce"]);

    let state = if tamper_state {
        "forged".to_owned()
    } else {
        auth["state"].clone()
    };
    let (status, _, resp) = send(
        app,
        test::TestRequest::get()
            .uri(&format!(
                "/api/v1/auth/google/callback?code=good-code&state={state}&scope=openid"
            ))
            .insert_header((COOKIE, cookie_pair)),
    )
    .await;
    assert_eq!(status, StatusCode::FOUND);
    let cleared = resp.get(SET_COOKIE).unwrap().to_str().unwrap();
    assert!(cleared.contains("Max-Age=0"), "state cookie must be cleared: {cleared}");

    if !tamper_state && mock.lock().unwrap().last_form.get("code").map(String::as_str) == Some("good-code") {
        // PKCE: the verifier sent to Google matches the challenge in the URL.
        let form = mock.lock().unwrap().last_form.clone();
        assert_eq!(pkce_challenge(&form["code_verifier"]), auth["code_challenge"]);
        assert_eq!(form["client_id"], CLIENT_ID);
    }
    location(&resp)
}

#[actix_web::test]
#[ignore = "requires PostgreSQL and Qdrant (docker compose up -d --wait)"]
async fn google_sign_in_flow() {
    let mock = Arc::new(Mutex::new(MockGoogle::default()));
    let token_url = start_mock_google(mock.clone()).await;
    let google = GoogleOAuth::with_token_url(
        GoogleConfig {
            client_id: CLIENT_ID.to_owned(),
            client_secret: SecretString::from("client-secret".to_owned()),
            allowed_domains: vec!["example.test".to_owned()],
        },
        "http://127.0.0.1:8080",
        &SecretString::from(JWT_SECRET.to_owned()),
        token_url,
    )
    .unwrap();
    let app = test::init_service(App::new().app_data(state(Some(google)).await).configure(api::configure)).await;

    // An admin invites Bob by email before his first sign-in.
    let id = unique();
    let bob_email = format!("bob.{id}@example.test");
    let (_, body, _) = send(
        &app,
        test::TestRequest::post()
            .uri("/api/v1/auth/dev-login")
            .set_json(json!({ "email": format!("admin.{id}@example.test") })),
    )
    .await;
    let admin = body["access_token"].as_str().unwrap().to_owned();
    let auth = |req: test::TestRequest, token: &str| req.insert_header(("Authorization", format!("Bearer {token}")));
    let (_, org, _) = send(
        &app,
        auth(test::TestRequest::post().uri("/api/v1/orgs"), &admin)
            .set_json(json!({ "slug": format!("g-{id}"), "name": "G" })),
    )
    .await;
    let org_id = org["id"].as_str().unwrap();
    let (_, team, _) = send(
        &app,
        auth(
            test::TestRequest::post().uri(&format!("/api/v1/orgs/{org_id}/teams")),
            &admin,
        )
        .set_json(json!({ "slug": "dev", "name": "Dev" })),
    )
    .await;
    let team_id = team["id"].as_str().unwrap();
    let (status, invited, _) = send(
        &app,
        auth(
            test::TestRequest::put().uri(&format!("/api/v1/orgs/{org_id}/teams/{team_id}/members")),
            &admin,
        )
        .set_json(json!({ "email": bob_email, "role": "reader" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Bob signs in with Google; his account links to the invitation.
    let sub = format!("google-{id}");
    let target = google_round_trip(
        &app,
        &mock,
        |n| google_claims(&sub, &bob_email.to_uppercase(), n),
        false,
    )
    .await;
    assert!(target.starts_with(&format!("{FRONTEND}?code=")), "{target}");
    let code = query(&target)["code"].clone();

    let (status, body, _) = send(
        &app,
        test::TestRequest::post()
            .uri("/api/v1/auth/exchange")
            .set_json(json!({ "code": code })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let session = body["access_token"].as_str().unwrap().to_owned();
    assert_eq!(
        body["user_id"], invited["user_id"],
        "Google sign-in must link the invited user"
    );

    // Login codes are single-use.
    let (status, _, _) = send(
        &app,
        test::TestRequest::post()
            .uri("/api/v1/auth/exchange")
            .set_json(json!({ "code": code })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, me, _) = send(&app, auth(test::TestRequest::get().uri("/api/v1/me"), &session)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["email"], bob_email);
    assert_eq!(me["memberships"][0]["team_role"], "reader");

    // Signing in again by subject keeps the same user.
    let target = google_round_trip(&app, &mock, |n| google_claims(&sub, &bob_email, n), false).await;
    assert!(target.contains("?code="), "{target}");

    // Rejections land on the frontend with an error code.
    let target = google_round_trip(&app, &mock, |n| google_claims(&sub, &bob_email, n), true).await;
    assert_eq!(target, format!("{FRONTEND}?error=invalid_state"));
    let other = format!("other-{id}");
    let target = google_round_trip(
        &app,
        &mock,
        |n| {
            let mut c = google_claims(&other, &format!("eve.{id}@evil.test"), n);
            c["hd"] = json!("evil.test");
            c
        },
        false,
    )
    .await;
    assert_eq!(target, format!("{FRONTEND}?error=domain_not_allowed"));
    let target = google_round_trip(&app, &mock, |n| google_claims(&other, &bob_email, n), false).await;
    assert_eq!(
        target,
        format!("{FRONTEND}?error=account_conflict"),
        "email linked to another Google account"
    );
    let target = google_round_trip(
        &app,
        &mock,
        |n| {
            let mut c = google_claims(&other, &format!("x.{id}@example.test"), n);
            c["email_verified"] = json!(false);
            c
        },
        false,
    )
    .await;
    assert_eq!(target, format!("{FRONTEND}?error=email_not_verified"));

    // A callback without the state cookie is rejected.
    let (status, _, resp) = send(
        &app,
        test::TestRequest::get().uri("/api/v1/auth/google/callback?code=good-code&state=whatever"),
    )
    .await;
    assert_eq!(status, StatusCode::FOUND);
    assert_eq!(location(&resp), format!("{FRONTEND}?error=invalid_state"));
    let (_, _, resp) = send(
        &app,
        test::TestRequest::get().uri("/api/v1/auth/google/callback?error=access_denied"),
    )
    .await;
    assert_eq!(location(&resp), format!("{FRONTEND}?error=access_denied"));
}

#[actix_web::test]
#[ignore = "requires PostgreSQL and Qdrant (docker compose up -d --wait)"]
async fn google_endpoints_404_when_not_configured() {
    let app = test::init_service(App::new().app_data(state(None).await).configure(api::configure)).await;
    let (status, _, _) = send(&app, test::TestRequest::get().uri("/api/v1/auth/google/login")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = send(
        &app,
        test::TestRequest::get().uri("/api/v1/auth/google/callback?code=x&state=y"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _, resp) = send(&app, test::TestRequest::get().uri("/auth/complete")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(resp.get("referrer-policy").unwrap(), "no-referrer");
    assert!(
        resp.get("content-security-policy")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("script-src 'self'")
    );
}

#[actix_web::test]
#[ignore = "requires PostgreSQL and Qdrant (docker compose up -d --wait)"]
async fn personal_access_tokens() {
    let state = state(None).await;
    let pool = state.pool.clone();
    let app = test::init_service(App::new().app_data(state).configure(api::configure)).await;
    let id = unique();
    let (_, body, _) = send(
        &app,
        test::TestRequest::post()
            .uri("/api/v1/auth/dev-login")
            .set_json(json!({ "email": format!("pat.{id}@example.com") })),
    )
    .await;
    let session = body["access_token"].as_str().unwrap().to_owned();
    let user_id: Uuid = body["user_id"].as_str().unwrap().parse().unwrap();
    let auth = |req: test::TestRequest, token: &str| req.insert_header(("Authorization", format!("Bearer {token}")));

    let create = |scope: &str, days: Value| {
        auth(test::TestRequest::post().uri("/api/v1/me/tokens"), &session)
            .set_json(json!({ "name": format!("{scope} token"), "scope": scope, "expires_in_days": days }))
    };
    for (scope, days) in [("read", json!(0)), ("read", json!(366)), ("admin", json!(30))] {
        let (status, body, _) = send(&app, create(scope, days)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{scope}/{body}");
    }
    let (status, read, _) = send(&app, create("read", json!(30))).await;
    assert_eq!(status, StatusCode::CREATED, "{read}");
    let read_token = read["token"].as_str().unwrap().to_owned();
    assert!(read_token.starts_with("nnc_pat_") && read_token.starts_with(read["token_prefix"].as_str().unwrap()));
    let (_, write, _) = send(&app, create("write", Value::Null)).await;
    let write_token = write["token"].as_str().unwrap().to_owned();

    // Both scopes authenticate reads.
    for token in [&read_token, &write_token] {
        let (status, me, _) = send(&app, auth(test::TestRequest::get().uri("/api/v1/me"), token)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(me["user_id"], user_id.to_string());
    }

    // Write scope can change state; read scope cannot.
    let new_org = |token: &str, slug: String| {
        auth(test::TestRequest::post().uri("/api/v1/orgs"), token).set_json(json!({ "slug": slug, "name": "PAT" }))
    };
    let (status, _, _) = send(&app, new_org(&read_token, format!("r-{id}"))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, org, _) = send(&app, new_org(&write_token, format!("w-{id}"))).await;
    assert_eq!(status, StatusCode::CREATED);
    let org_id = org["id"].as_str().unwrap();
    let (status, _, _) = send(
        &app,
        auth(
            test::TestRequest::post().uri(&format!("/api/v1/orgs/{org_id}/search")),
            &read_token,
        )
        .set_json(json!({ "query": "anything" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "search is a read");

    // Tokens cannot manage tokens, even with write scope.
    for token in [&read_token, &write_token] {
        let (status, _, _) = send(
            &app,
            auth(test::TestRequest::post().uri("/api/v1/me/tokens"), token)
                .set_json(json!({ "name": "x", "scope": "write" })),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _, _) = send(
            &app,
            auth(
                test::TestRequest::delete().uri(&format!("/api/v1/me/tokens/{}", read["id"].as_str().unwrap())),
                token,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    // Listing never exposes the secret.
    let (_, list, _) = send(&app, auth(test::TestRequest::get().uri("/api/v1/me/tokens"), &session)).await;
    let rendered = list.to_string();
    assert!(!rendered.contains(&read_token) && !rendered.contains(&write_token));
    assert_eq!(list.as_array().unwrap().len(), 2);

    // Revocation and expiry take effect immediately.
    let (status, _, _) = send(
        &app,
        auth(
            test::TestRequest::delete().uri(&format!("/api/v1/me/tokens/{}", read["id"].as_str().unwrap())),
            &session,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _, _) = send(&app, auth(test::TestRequest::get().uri("/api/v1/me"), &read_token)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    sqlx::query("UPDATE api_tokens SET expires_at = now() - interval '1 second' WHERE id = $1")
        .bind(write["id"].as_str().unwrap().parse::<Uuid>().unwrap())
        .execute(&pool)
        .await
        .unwrap();
    let (status, _, _) = send(&app, auth(test::TestRequest::get().uri("/api/v1/me"), &write_token)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = send(
        &app,
        auth(test::TestRequest::get().uri("/api/v1/me"), "nnc_pat_unknown"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let actions: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM audit_logs WHERE actor_user_id = $1 AND resource_type = 'api_token' ORDER BY id",
    )
    .bind(user_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(actions, ["token.create", "token.create", "token.revoke"]);
}
