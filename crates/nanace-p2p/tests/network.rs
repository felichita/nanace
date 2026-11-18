//! The real thing on one machine: the nanace API with its internal
//! listener, a relay node, and daemons that reach each other through libp2p.
//! Needs PostgreSQL and Qdrant like the other integration tests:
//! `cargo test -p nanace-p2p -- --ignored`.
//!
//! Everything listens on loopback, which daemons never announce, so peers
//! find each other through the relay as they would behind NAT.

mod common;

use nanace_core::p2p::{AcceptMode, ClientKind, MessageKind, RejectReason};
use nanace_p2p::daemon::{DaemonError, OutState, SendParams};
use serde_json::json;
use uuid::Uuid;

use crate::common::{Api, Peer, daemon, eventually, start_api, start_node};

#[actix_web::test]
#[ignore = "requires PostgreSQL and Qdrant (docker compose up -d --wait)"]
async fn daemons_talk_through_the_relay_and_strangers_do_not() {
    let (public, internal) = start_api().await;
    let api = Api {
        http: reqwest::Client::new(),
        base: public,
    };
    let root = std::env::temp_dir().join(format!("np-net-{}", &Uuid::new_v4().simple().to_string()[..8]));

    let bootstrap = start_node(internal);

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

    let alice = daemon(&api, &alice_session, &root, "alice", ClientKind::Server, &bootstrap).await;
    let bob = daemon(&api, &bob_session, &root, "bob", ClientKind::Server, &bootstrap).await;
    let dave = daemon(&api, &dave_session, &root, "dave", ClientKind::Server, &bootstrap).await;
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
