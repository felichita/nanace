//! What travels between two daemons: a signed header plus the text it pins
//! by hash. The header carries everything the server may know about a
//! message; the body stays between the two machines.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use nanace_core::p2p::{
    self, AnswerHeader, RejectReason, RequestHeader, answer_signing_bytes, request_signing_bytes, sha256_hex,
};
use serde::{Deserialize, Serialize};

/// Protocol version of headers this build writes and reads.
pub const HEADER_VERSION: u32 = 1;
/// Hard ceiling for a body, whatever a policy says.
pub const MAX_BODY_BYTES: usize = nanace_core::policy::MAX_MESSAGE_BYTES as usize;
/// A header is small; anything larger is not one.
const MAX_HEADER_BYTES: usize = 8 * 1024;

/// A signed header with its body, as sent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Signed {
    /// base64url of the header bytes exactly as they were signed.
    pub header: String,
    /// base64url Ed25519 signature by the author's peer key.
    pub signature: String,
    pub body: String,
}

/// A message on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Wire {
    Request(Signed),
    Answer(Signed),
}

/// The immediate reply to a message: whether the other daemon took it. An
/// answer to a question arrives later as its own message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Ack {
    Accepted,
    Rejected(RejectReason),
}

/// The key a daemon signs with, and the peer id it gives.
#[derive(Clone)]
pub struct Identity {
    key: SigningKey,
    pub peer_id: String,
}

impl Identity {
    pub fn from_secret(secret: [u8; 32]) -> Self {
        let key = SigningKey::from_bytes(&secret);
        let peer_id = p2p::peer_id_from_ed25519(&key.verifying_key().to_bytes());
        Self { key, peer_id }
    }

    /// The 32-byte seed, for the libp2p keypair of the same identity.
    pub fn secret(&self) -> [u8; 32] {
        self.key.to_bytes()
    }

    pub fn sign(&self, message: &[u8]) -> String {
        URL_SAFE_NO_PAD.encode(self.key.sign(message).to_bytes())
    }

    pub fn sign_request(&self, header: &RequestHeader, body: String) -> Signed {
        let bytes = serde_json::to_vec(header).expect("a header serializes");
        Signed {
            signature: self.sign(&request_signing_bytes(&bytes)),
            header: URL_SAFE_NO_PAD.encode(bytes),
            body,
        }
    }

    pub fn sign_answer(&self, header: &AnswerHeader, body: String) -> Signed {
        let bytes = serde_json::to_vec(header).expect("a header serializes");
        Signed {
            signature: self.sign(&answer_signing_bytes(&bytes)),
            header: URL_SAFE_NO_PAD.encode(bytes),
            body,
        }
    }
}

fn signed_by(peer_id: &str, signature: &str, message: &[u8]) -> bool {
    let Ok(key) = p2p::peer_id(peer_id)
        .map_err(|_| ())
        .and_then(|k| VerifyingKey::from_bytes(&k).map_err(|_| ()))
    else {
        return false;
    };
    URL_SAFE_NO_PAD
        .decode(signature)
        .ok()
        .and_then(|bytes| Signature::from_slice(&bytes).ok())
        .is_some_and(|signature| key.verify_strict(message, &signature).is_ok())
}

fn header_bytes(signed: &Signed) -> Option<Vec<u8>> {
    if signed.header.len() > MAX_HEADER_BYTES * 2 || signed.body.len() > MAX_BODY_BYTES {
        return None;
    }
    URL_SAFE_NO_PAD
        .decode(&signed.header)
        .ok()
        .filter(|b| b.len() <= MAX_HEADER_BYTES)
}

/// A request whose signature and body hash hold.
pub struct OpenedRequest {
    pub header: RequestHeader,
    /// Hex sha256 of the header bytes; answers refer to it.
    pub hash: String,
}

/// Checks that `signed` is a request signed by the peer its header names as
/// the sender, and that the body is the one the header pins. `None` for
/// anything else; the caller answers `Invalid` without saying why.
pub fn open_request(signed: &Signed) -> Option<OpenedRequest> {
    let bytes = header_bytes(signed)?;
    let header: RequestHeader = serde_json::from_slice(&bytes).ok()?;
    let intact = header.v == HEADER_VERSION
        && signed_by(&header.from.peer_id, &signed.signature, &request_signing_bytes(&bytes))
        && header.body_hash == sha256_hex(signed.body.as_bytes())
        && usize::try_from(header.body_bytes).is_ok_and(|n| n == signed.body.len());
    intact.then(|| OpenedRequest {
        hash: sha256_hex(&bytes),
        header,
    })
}

/// Checks that `signed` is the answer to `request`, signed by the peer the
/// request was addressed to.
pub fn open_answer(signed: &Signed, request: &OpenedRequest) -> Option<AnswerHeader> {
    let bytes = header_bytes(signed)?;
    let header: AnswerHeader = serde_json::from_slice(&bytes).ok()?;
    let intact = header.v == HEADER_VERSION
        && header.request_id == request.header.request_id
        && header.request_hash == request.hash
        && header.answered_by.peer_id == request.header.to.peer_id
        && signed_by(
            &header.answered_by.peer_id,
            &signed.signature,
            &answer_signing_bytes(&bytes),
        )
        && header.body_hash == sha256_hex(signed.body.as_bytes())
        && usize::try_from(header.body_bytes).is_ok_and(|n| n == signed.body.len());
    intact.then_some(header)
}

#[cfg(test)]
mod tests {
    use nanace_core::p2p::{AnswerStatus, MessageKind, Party};
    use uuid::Uuid;

    use super::*;

    fn party(identity: &Identity, label: &str) -> Party {
        Party {
            peer_id: identity.peer_id.clone(),
            agent_label: label.to_owned(),
            user_id: Uuid::new_v4(),
        }
    }

    fn request(from: &Identity, to: &Identity, body: &str) -> RequestHeader {
        RequestHeader {
            v: HEADER_VERSION,
            request_id: Uuid::new_v4(),
            trace_id: "T-1".to_owned(),
            org_id: Uuid::new_v4(),
            team_id: Uuid::new_v4(),
            from: party(from, "a"),
            to: party(to, "b"),
            kind: MessageKind::Ask,
            body_hash: sha256_hex(body.as_bytes()),
            body_bytes: u32::try_from(body.len()).unwrap(),
            refs: vec![],
            hop: 0,
            sent_at: 1_000,
            deadline: 2_000,
            policy_version: "v".to_owned(),
        }
    }

    #[test]
    fn requests_open_only_when_signed_by_their_sender_and_untouched() {
        let (alice, bob, mallory) = (
            Identity::from_secret([1; 32]),
            Identity::from_secret([2; 32]),
            Identity::from_secret([3; 32]),
        );
        let header = request(&alice, &bob, "hello");
        let signed = alice.sign_request(&header, "hello".to_owned());
        let opened = open_request(&signed).expect("valid");
        assert_eq!(opened.header, header);

        // Unsigned, re-signed by someone else, or changed after signing.
        let unsigned = Signed {
            signature: String::new(),
            ..signed.clone()
        };
        let resigned = mallory.sign_request(&header, "hello".to_owned());
        let other_body = Signed {
            body: "hellp".to_owned(),
            ..signed.clone()
        };
        let mut changed = header.clone();
        changed.kind = MessageKind::Notify;
        let swapped_header = Signed {
            header: URL_SAFE_NO_PAD.encode(serde_json::to_vec(&changed).unwrap()),
            ..signed.clone()
        };
        let old_version = alice.sign_request(&RequestHeader { v: 2, ..header.clone() }, "hello".to_owned());
        let as_answer = Signed {
            signature: alice.sign(&answer_signing_bytes(&URL_SAFE_NO_PAD.decode(&signed.header).unwrap())),
            ..signed.clone()
        };
        let huge = Signed {
            body: "x".repeat(MAX_BODY_BYTES + 1),
            ..signed.clone()
        };
        for (bad, why) in [
            (unsigned, "unsigned"),
            (resigned, "re-signed"),
            (other_body, "body"),
            (swapped_header, "header"),
            (old_version, "version"),
            (as_answer, "answer-domain signature"),
            (huge, "size"),
            (
                Signed {
                    header: "!!".to_owned(),
                    ..signed.clone()
                },
                "encoding",
            ),
        ] {
            assert!(open_request(&bad).is_none(), "{why}");
        }
    }

    #[test]
    fn answers_open_only_for_their_request_and_receiver() {
        let (alice, bob, mallory) = (
            Identity::from_secret([1; 32]),
            Identity::from_secret([2; 32]),
            Identity::from_secret([3; 32]),
        );
        let header = request(&alice, &bob, "q");
        let opened = open_request(&alice.sign_request(&header, "q".to_owned())).unwrap();
        let answer = AnswerHeader {
            v: HEADER_VERSION,
            request_id: header.request_id,
            request_hash: opened.hash.clone(),
            status: AnswerStatus::Answered,
            reason: None,
            answered_by: header.to.clone(),
            body_hash: sha256_hex(b"a"),
            body_bytes: 1,
            refs: vec![],
            sent_at: 1_500,
        };
        let signed = bob.sign_answer(&answer, "a".to_owned());
        assert_eq!(open_answer(&signed, &opened), Some(answer.clone()));

        let by_mallory = mallory.sign_answer(&answer, "a".to_owned());
        let as_mallory = mallory.sign_answer(
            &AnswerHeader {
                answered_by: party(&mallory, "b"),
                ..answer.clone()
            },
            "a".to_owned(),
        );
        let other_request = bob.sign_answer(
            &AnswerHeader {
                request_hash: sha256_hex(b"x"),
                ..answer.clone()
            },
            "a".to_owned(),
        );
        let other_body = Signed {
            body: "b".to_owned(),
            ..signed.clone()
        };
        for (bad, why) in [
            (by_mallory, "signed by another peer"),
            (as_mallory, "answered by another peer"),
            (other_request, "another request"),
            (other_body, "body"),
        ] {
            assert!(open_answer(&bad, &opened).is_none(), "{why}");
        }
    }

    #[test]
    fn wire_format_is_strict() {
        let ack: Ack = serde_json::from_str(r#"{"rejected":"policy"}"#).unwrap();
        assert_eq!(ack, Ack::Rejected(RejectReason::Policy));
        assert_eq!(serde_json::to_string(&Ack::Accepted).unwrap(), r#""accepted""#);
        assert!(serde_json::from_str::<Ack>(r#"{"rejected":"because"}"#).is_err());
        assert!(serde_json::from_str::<Wire>(r#"{"request":{"header":"","signature":"","body":"","x":1}}"#).is_err());
    }
}
