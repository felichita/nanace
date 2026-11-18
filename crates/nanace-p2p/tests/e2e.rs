//! The scenarios the P2P layer was built for, end to end: the real API, a
//! relay node, daemons over libp2p, and stub agents in place of models (they
//! answer with fixed text). Needs PostgreSQL and Qdrant:
//! `cargo test -p nanace-p2p -- --ignored`.
//!
//! 1. Alice's agent asks Bob's; Bob is asked, allows it, his agent answers,
//!    Alice gets the answer and records a summary page.
//! 2. Alice's agent asks Carol's, whose laptop is closed: the request waits,
//!    then expires.
//! 3. Dave, in another organization, neither finds Alice nor reaches her.
//! 4. The verdict agent, which has no P2P at all, reads the summary by its
//!    task and writes `Verdict: TASK-1`.

mod common;

use std::sync::Arc;
use std::time::Duration;

use nanace_core::p2p::{AcceptMode, ClientKind, MessageKind};
use nanace_p2p::daemon::{Daemon, DaemonError, OutState, SendParams};
use nanace_p2p::transport::Transport;
use reqwest::Method;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::common::{Api, Peer, daemon, eventually, start_api, start_node};

const ANSWER: &str = "billing-api and ledger depend on sg-123 (stub answer).";

/// A stand-in for a model with the P2P tools: answers every question in its
/// inbox with a fixed text, as an agent would through `p2p_reply`.
fn stub_agent(daemon: Arc<Daemon>, label: &'static str) {
    actix_web::rt::spawn(async move {
        loop {
            for item in daemon.inbox(label).await {
                if item.kind == MessageKind::Ask {
                    let _ = daemon.reply(label, item.request_id, ANSWER.to_owned(), vec![]).await;
                }
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    });
}

fn question(to: &Peer, org: Uuid, team: Uuid, to_agent: &str, body: &str) -> SendParams {
    SendParams {
        org_id: org,
        team_id: team,
        to_peer: to.peer_id.clone(),
        to_agent: to_agent.to_owned(),
        kind: MessageKind::Ask,
        body: body.to_owned(),
        refs: vec![],
        trace_id: Some("TASK-1".to_owned()),
    }
}

async fn member(api: &Api, admin: &str, org: Uuid, team: Uuid, email: &str, role: &str) {
    api.ok(
        Method::PUT,
        &format!("/api/v1/orgs/{org}/teams/{team}/members"),
        admin,
        Some(json!({ "email": email, "role": role })),
    )
    .await;
}

#[actix_web::test]
#[ignore = "requires PostgreSQL and Qdrant (docker compose up -d --wait)"]
async fn scenarios_end_to_end() {
    let (public, internal) = start_api().await;
    let api = Api {
        http: reqwest::Client::new(),
        base: public,
    };
    let bootstrap = start_node(internal);
    let root = std::env::temp_dir().join(format!("np-e2e-{}", &Uuid::new_v4().simple().to_string()[..8]));

    // One team: Alice owns it, Bob and Carol edit, and a service account
    // with the `agent` role will give the verdict. Dave is elsewhere.
    let (alice_session, _) = api.login("alice").await;
    let (bob_session, bob_email) = api.login("bob").await;
    let (carol_session, carol_email) = api.login("carol").await;
    let (verdict_session, verdict_email) = api.login("verdict").await;
    let (dave_session, dave_email) = api.login("dave").await;
    let (org, team) = api.team(&alice_session, &[]).await;
    for (email, role) in [
        (&bob_email, "writer"),
        (&carol_email, "writer"),
        (&verdict_email, "agent"),
    ] {
        member(&api, &alice_session, org, team, email, role).await;
    }
    let (dave_org, dave_team) = api.team(&dave_session, &[&dave_email]).await;
    // The owner shortens how long a request lives, so the test can watch one
    // expire. This also shows the policy page being edited through the API.
    let pages = api
        .ok(
            Method::GET,
            &format!("/api/v1/orgs/{org}/teams/{team}/contexts"),
            &alice_session,
            None,
        )
        .await;
    let policy_page = pages[0]["id"].as_str().unwrap().to_owned();
    let policy = nanace_core::policy::TEMPLATE.replace("request_ttl_secs: 3600", "request_ttl_secs: 60");
    api.ok(
        Method::PUT,
        &format!("/api/v1/orgs/{org}/contexts/{policy_page}"),
        &alice_session,
        Some(json!({ "title": "Policy: P2P", "content": policy })),
    )
    .await;

    // Laptops: the daemon runs with the desktop app attached (the test plays
    // the app). Each has one agent session, a stub in place of a model.
    let alice = daemon(&api, &alice_session, &root, "alice", ClientKind::Desktop, &bootstrap).await;
    let bob = daemon(&api, &bob_session, &root, "bob", ClientKind::Desktop, &bootstrap).await;
    let carol = daemon(&api, &carol_session, &root, "carol", ClientKind::Desktop, &bootstrap).await;
    let dave = daemon(&api, &dave_session, &root, "dave", ClientKind::Server, &bootstrap).await;
    for peer in [&alice, &bob, &carol] {
        peer.daemon.set_team(org, team, AcceptMode::Ask).await.unwrap();
    }
    dave.daemon
        .set_team(dave_org, dave_team, AcceptMode::Team)
        .await
        .unwrap();
    stub_agent(Arc::clone(&bob.daemon), "bob");
    eventually("bob and carol to be present for alice", 30, || async {
        alice.daemon.find_peers(org, team, None).await.is_ok_and(|peers| {
            [&bob.peer_id, &carol.peer_id]
                .iter()
                .all(|id| peers.iter().any(|p| p.peer_id == **id))
        })
    })
    .await;

    // ----- Scenario 2 starts: Carol closes her laptop ----------------------
    carol.ticks.abort();
    carol.net.shutdown().await;
    let to_carol = alice
        .daemon
        .send(
            "alice",
            question(&carol, org, team, "carol", "Carol, is the migration done?"),
        )
        .await
        .unwrap();

    // ----- Scenario 1: ask, consent, answer, summary -----------------------
    let asked = "Which services depend on security group sg-123?";
    let to_bob = alice
        .daemon
        .send("alice", question(&bob, org, team, "bob", asked))
        .await
        .unwrap();
    // Bob's consent window: the request waits there, and his agent has not
    // seen it.
    eventually("the consent prompt on bob's laptop", 90, || async {
        !bob.daemon.pending().await.is_empty()
    })
    .await;
    let (prompt, _) = bob.daemon.pending().await.remove(0);
    assert_eq!(prompt.body, asked);
    assert_eq!(prompt.kind, MessageKind::Ask);
    assert_eq!(
        alice.daemon.reply_of(to_bob.request_id).await.unwrap().state,
        OutState::Delivered
    );
    // Bob clicks "Allow"; his agent (the stub) answers.
    bob.daemon.decide_consent(to_bob.request_id, true, None).await.unwrap();
    eventually("alice to get the answer", 30, || async {
        alice
            .daemon
            .reply_of(to_bob.request_id)
            .await
            .is_ok_and(|r| r.state == OutState::Answered)
    })
    .await;
    let reply = alice.daemon.reply_of(to_bob.request_id).await.unwrap();
    assert_eq!(reply.answer.as_deref(), Some(ANSWER));
    assert!(reply.summary_required, "the team's policy asks for a summary");

    // Alice's agent records the summary, as the reminder in `p2p_get_reply`
    // tells a model to: a page titled by the policy's prefix, tagged with
    // the trace.
    let summary_title = format!(
        "{}sg-123 dependants ({})",
        reply.summary_title_prefix.as_deref().unwrap(),
        reply.trace_id
    );
    let summary = api
        .ok(
            Method::POST,
            &format!("/api/v1/orgs/{org}/teams/{team}/contexts"),
            &alice_session,
            Some(json!({
                "title": summary_title,
                "content": format!(
                    "> Asked which services depend on sg-123.\n\n## Participants\n\n- Asked by: alice\n- Answered by: {bob_email}\n\n## Answer\n\n{ANSWER}\n"
                ),
                "tags": ["p2p", "trace-task-1", "task-1"],
            })),
        )
        .await;
    // The server's journal has both sides of the exchange, verified, with
    // consent recorded and the summary noticed. It holds no message text.
    let journal_uri = format!("/api/v1/orgs/{org}/teams/{team}/p2p/exchanges?trace_id=TASK-1");
    eventually("both sides of the exchange in the journal", 30, || async {
        let journal = api.ok(Method::GET, &journal_uri, &alice_session, None).await;
        let rows: Vec<&Value> = journal
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["request_id"] == to_bob.request_id.to_string())
            .collect();
        rows.len() == 2
            && rows
                .iter()
                .all(|r| r["status"] == "answered" && r["answer_verified"] == true && r["has_summary"] == true)
    })
    .await;
    let journal = api
        .ok(Method::GET, &journal_uri, &alice_session, None)
        .await
        .to_string();
    assert!(journal.contains("user_approved"), "{journal}");
    assert!(
        !journal.contains("sg-123") && !journal.contains("stub answer"),
        "the journal must hold no text"
    );

    // ----- Scenario 3: another organization -------------------------------
    // Dave does not find Alice's team or her peer through the registry...
    let found = dave.daemon.find_peers(org, team, None).await;
    assert!(matches!(found, Err(DaemonError::Registry(_))), "{found:?}");
    let (status, _) = api
        .call(
            Method::GET,
            &format!("/api/v1/orgs/{org}/teams/{team}/peers"),
            &dave_session,
            None,
        )
        .await;
    assert_eq!(status, 404);
    let (status, _) = api
        .call(
            Method::GET,
            &format!("/api/v1/me/peers/lookup/{}", alice.peer_id),
            &dave_session,
            None,
        )
        .await;
    assert_eq!(status, 404);
    // ...his daemon refuses to address her, and a message pushed straight at
    // her peer through the relay is dropped with the connection.
    let refused = dave
        .daemon
        .send("dave", question(&alice, dave_org, dave_team, "alice", "hello?"))
        .await;
    assert!(refused.is_err(), "{refused:?}");
    let pushed = dave
        .net
        .send(
            &alice.peer_id,
            &[],
            nanace_p2p::envelope::Wire::Request(nanace_p2p::envelope::Signed {
                header: String::new(),
                signature: String::new(),
                body: "hello".to_owned(),
            }),
        )
        .await;
    assert!(pushed.is_err(), "{pushed:?}");
    assert!(alice.daemon.pending().await.is_empty() && alice.daemon.inbox("alice").await.is_empty());

    // ----- Scenario 4: the verdict agent ----------------------------------
    // It works through the knowledge base only: no daemon, no peer bound.
    let peers = api.ok(Method::GET, "/api/v1/me/peers", &verdict_session, None).await;
    assert_eq!(peers, json!([]));
    let by_task = api
        .ok(
            Method::GET,
            &format!("/api/v1/orgs/{org}/teams/{team}/contexts?tag=trace-task-1"),
            &verdict_session,
            None,
        )
        .await;
    assert_eq!(by_task.as_array().unwrap().len(), 1);
    assert_eq!(by_task[0]["title"], summary_title.as_str());
    let read = api
        .ok(
            Method::GET,
            &format!("/api/v1/orgs/{org}/contexts/{}", by_task[0]["id"].as_str().unwrap()),
            &verdict_session,
            None,
        )
        .await;
    assert!(read["content"].as_str().unwrap().contains(ANSWER));
    let verdict = api
        .ok(
            Method::POST,
            &format!("/api/v1/orgs/{org}/teams/{team}/contexts"),
            &verdict_session,
            Some(json!({
                "title": "Verdict: TASK-1",
                "content": format!(
                    "> **Status:** accepted. The dependants of sg-123 are recorded.\n\n## Based on\n\n- [[{summary_title}]]\n"
                ),
                "tags": ["verdict", "task-1", "accepted"],
            })),
        )
        .await;
    let verdicts = api
        .ok(
            Method::GET,
            &format!("/api/v1/orgs/{org}/teams/{team}/contexts?tag=verdict"),
            &alice_session,
            None,
        )
        .await;
    assert_eq!(verdicts[0]["title"], "Verdict: TASK-1");
    let links = api
        .ok(
            Method::GET,
            &format!("/api/v1/orgs/{org}/contexts/{}/links", verdict["id"].as_str().unwrap()),
            &alice_session,
            None,
        )
        .await;
    assert_eq!(links["outgoing"][0]["title"], summary_title.as_str());
    // Its role lets it write its own notes and nothing else: not the summary
    // it judged, and not the team's rules.
    for (uri, title, error) in [
        (
            format!("/api/v1/orgs/{org}/contexts/{}", summary["id"].as_str().unwrap()),
            summary_title.as_str(),
            "not_author",
        ),
        (
            format!("/api/v1/orgs/{org}/contexts/{policy_page}"),
            "Policy: P2P",
            "protected_page",
        ),
    ] {
        let (status, body) = api
            .call(
                Method::PUT,
                &uri,
                &verdict_session,
                Some(json!({ "title": title, "content": "rewritten by the verdict agent" })),
            )
            .await;
        assert_eq!((status, body["error"].as_str()), (403, Some(error)), "{uri}");
    }

    // ----- Scenario 2 ends: the request to Carol expires ------------------
    assert_eq!(
        alice.daemon.reply_of(to_carol.request_id).await.unwrap().state,
        OutState::Queued,
        "it waits in alice's queue while carol is away"
    );
    eventually("the request to carol to expire", 90, || async {
        alice
            .daemon
            .reply_of(to_carol.request_id)
            .await
            .is_ok_and(|r| r.state == OutState::Expired)
    })
    .await;
    assert!(carol.daemon.pending().await.is_empty() && carol.daemon.inbox("carol").await.is_empty());
    eventually("the expiry in the journal", 30, || async {
        let journal = api.ok(Method::GET, &journal_uri, &alice_session, None).await;
        journal
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["request_id"] == to_carol.request_id.to_string() && r["status"] == "expired")
    })
    .await;

    // What an organization admin sees afterwards: who bound a peer, who was
    // present, who sent what kind of message with which outcome, and who
    // changed the policy.
    let audit = api
        .ok(
            Method::GET,
            &format!("/api/v1/orgs/{org}/audit-logs?limit=500"),
            &alice_session,
            None,
        )
        .await
        .to_string();
    for action in [
        "p2p.presence_put",
        "p2p.policy_update",
        "p2p.exchange",
        "context.agent_denied",
        "context.protected_denied",
    ] {
        assert!(audit.contains(action), "{action} missing from the audit log");
    }
    assert!(!audit.contains("sg-123"), "the audit log must hold no message text");
    let _ = std::fs::remove_dir_all(root);
}
