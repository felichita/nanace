//! Server side of the P2P registry that is not a request handler: the
//! challenge a peer signs to prove it holds its key, and presence cleanup.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, TimeZone, Utc};
use ed25519_dalek::{Signature, VerifyingKey};
use hmac::{Hmac, KeyInit, Mac};
use nanace_core::p2p::bind_message;
use nanace_core::policy::{MAX_POLICY_BLOCK_BYTES, P2pPolicy, policy_block};
use secrecy::{ExposeSecret, SecretString};
use sha2::Sha256;
use sqlx::PgPool;
use uuid::Uuid;

use crate::secret::sha256_hex;
use crate::{db, metrics};

/// How long a challenge can be answered.
const CHALLENGE_TTL_SECS: i64 = 60;
const NONCE_BYTES: usize = 16;
const CHALLENGE_CONTEXT: &[u8] = b"nanace-p2p-challenge-v1\0";
const KEY_CONTEXT: &[u8] = b"nanace-p2p-challenge-key-v1";

/// P2P registry settings and the key that authenticates binding challenges.
pub struct P2pState {
    /// Derived from the JWT secret, so challenges need no storage and every
    /// replica accepts the others' challenges.
    challenge_key: [u8; 32],
    /// Upper bound for a presence TTL; clients heartbeat at a third of it.
    pub presence_max_ttl_secs: u32,
    /// Unrevoked peers one user may have bound.
    pub max_peers_per_user: u32,
    /// Shared secret of the network node, when one is configured.
    pub node_secret: Option<SecretString>,
}

pub struct Challenge {
    pub challenge: String,
    pub expires_at: DateTime<Utc>,
}

impl P2pState {
    pub fn new(jwt_secret: &SecretString, presence_max_ttl_secs: u32, max_peers_per_user: u32) -> Self {
        let mut mac =
            Hmac::<Sha256>::new_from_slice(jwt_secret.expose_secret().as_bytes()).expect("HMAC accepts any key length");
        mac.update(KEY_CONTEXT);
        Self {
            challenge_key: mac.finalize().into_bytes().into(),
            presence_max_ttl_secs,
            max_peers_per_user,
            node_secret: None,
        }
    }

    pub fn with_node_secret(mut self, secret: Option<SecretString>) -> Self {
        self.node_secret = secret;
        self
    }

    fn mac(&self, user_id: Uuid, nonce_and_expiry: &[u8]) -> Hmac<Sha256> {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.challenge_key).expect("HMAC accepts any key length");
        mac.update(CHALLENGE_CONTEXT);
        mac.update(user_id.as_bytes());
        mac.update(nonce_and_expiry);
        mac
    }

    /// A challenge only `user_id` can use, valid for a minute. It is not
    /// single-use: binding is idempotent, so a replay repeats the same bind.
    pub fn issue_challenge(&self, user_id: Uuid) -> anyhow::Result<Challenge> {
        let expires = Utc::now().timestamp() + CHALLENGE_TTL_SECS;
        let mut payload = [0u8; NONCE_BYTES + 8];
        getrandom::fill(&mut payload[..NONCE_BYTES]).map_err(|e| anyhow::anyhow!("system RNG failed: {e}"))?;
        payload[NONCE_BYTES..].copy_from_slice(&expires.to_be_bytes());
        let tag = self.mac(user_id, &payload).finalize().into_bytes();
        Ok(Challenge {
            challenge: format!("{}.{}", URL_SAFE_NO_PAD.encode(payload), URL_SAFE_NO_PAD.encode(tag)),
            expires_at: Utc
                .timestamp_opt(expires, 0)
                .single()
                .ok_or_else(|| anyhow::anyhow!("challenge expiry out of range"))?,
        })
    }

    /// True when `challenge` was issued to `user_id` and has not expired.
    pub fn challenge_is_valid(&self, user_id: Uuid, challenge: &str) -> bool {
        let Some((payload, tag)) = challenge.split_once('.') else {
            return false;
        };
        let (Ok(payload), Ok(tag)) = (URL_SAFE_NO_PAD.decode(payload), URL_SAFE_NO_PAD.decode(tag)) else {
            return false;
        };
        let Some(expiry) = payload.get(NONCE_BYTES..).and_then(|e| <[u8; 8]>::try_from(e).ok()) else {
            return false;
        };
        // Constant-time comparison.
        self.mac(user_id, &payload).verify_slice(&tag).is_ok() && i64::from_be_bytes(expiry) > Utc::now().timestamp()
    }
}

/// True when `signature` (base64url) is the peer key's signature over the
/// binding message for this challenge, peer and user.
pub fn bind_signature_is_valid(
    public_key: &[u8; 32],
    signature: &str,
    challenge: &str,
    peer_id: &str,
    user_id: Uuid,
) -> bool {
    signature_is_valid(public_key, signature, &bind_message(challenge, peer_id, user_id))
}

/// Deletes exchange reports older than `retention_days`, or older than the
/// shorter period a team's policy sets. Returns how many were deleted.
pub async fn sweep_exchanges(pool: &PgPool, retention_days: u32) -> anyhow::Result<u64> {
    let days = i32::try_from(retention_days)?;
    let mut tx = db::p2p_system_tx(pool).await?;
    let deleted = sqlx::query(
        "DELETE FROM p2p_exchanges e WHERE e.created_at < now() - make_interval(days => LEAST($1, COALESCE( \
             (SELECT (pp.parsed->'retention'->>'exchange_metadata_days')::integer FROM p2p_policies pp \
              WHERE pp.team_id = e.team_id AND pp.valid), $1)))",
    )
    .bind(days)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    tx.commit().await?;
    Ok(deleted)
}

/// A `Policy: P2P` page as the server understands it.
pub struct ParsedPolicy {
    /// Hash of the page content; changes with every edit.
    pub version: String,
    /// The policy, or what is wrong with the page in words for its owner.
    pub policy: Result<P2pPolicy, Vec<String>>,
}

/// Parses and checks the policy block of a page.
pub fn parse_policy_page(content: &str) -> ParsedPolicy {
    let policy = match policy_block(content) {
        None => Err(vec![
            "the page needs a closed ```yaml p2p-policy block with the team's rules".to_owned(),
        ]),
        Some(block) if block.len() > MAX_POLICY_BLOCK_BYTES => Err(vec![format!(
            "the policy block is larger than {MAX_POLICY_BLOCK_BYTES} bytes"
        )]),
        Some(block) => match serde_norway::from_str::<P2pPolicy>(block) {
            Err(err) => Err(vec![format!("policy block: {err}")]),
            Ok(policy) => {
                let problems = policy.problems();
                if problems.is_empty() { Ok(policy) } else { Err(problems) }
            }
        },
    };
    ParsedPolicy {
        version: sha256_hex(content),
        policy,
    }
}

/// True when `signature` (base64url) is the key's Ed25519 signature over
/// `message`.
pub fn signature_is_valid(public_key: &[u8; 32], signature: &str, message: &[u8]) -> bool {
    let Ok(key) = VerifyingKey::from_bytes(public_key) else {
        return false;
    };
    URL_SAFE_NO_PAD
        .decode(signature)
        .ok()
        .and_then(|bytes| Signature::from_slice(&bytes).ok())
        .is_some_and(|signature| key.verify_strict(message, &signature).is_ok())
}

/// Deletes presence that has expired, auditing each ended presence as
/// `p2p.presence_expire`, and refreshes the active-peers gauge. Reads never
/// depend on this: they filter on `expires_at` themselves. Safe to run from
/// several replicas; a second run finds nothing.
pub async fn sweep_presence(pool: &PgPool) -> anyhow::Result<u64> {
    let mut tx = db::p2p_system_tx(pool).await?;
    let expired = sqlx::query(
        "WITH gone AS ( \
             DELETE FROM p2p_presence WHERE expires_at <= now() \
             RETURNING org_id, team_id, user_id, peer_id, agent_label \
         ) \
         INSERT INTO audit_logs (org_id, actor_user_id, action, resource_type, resource_id, details) \
         SELECT org_id, NULL, 'p2p.presence_expire', 'team', team_id, \
                jsonb_build_object('user_id', user_id, 'peer_id', peer_id, 'agent_label', agent_label) \
         FROM gone",
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    let active: i64 = sqlx::query_scalar("SELECT count(DISTINCT peer_id) FROM p2p_presence WHERE expires_at > now()")
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    metrics::set_p2p_active_peers(active);
    Ok(expired)
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::{Signer, SigningKey};
    use nanace_core::p2p::peer_id_from_ed25519;

    use super::*;

    fn state(secret: &str) -> P2pState {
        P2pState::new(&SecretString::from(secret.to_owned()), 120, 10)
    }

    #[test]
    fn policy_pages_parse_or_explain() {
        let template = parse_policy_page(nanace_core::policy::TEMPLATE);
        let policy = template.policy.expect("the template is a valid policy");
        assert!(policy.enabled && policy.problems().is_empty());
        assert_eq!(template.version.len(), 64);
        assert_ne!(template.version, parse_policy_page("other").version);

        let page = |block: &str| format!("intro\n```yaml p2p-policy\n{block}\n```\n");
        let errors = |content: &str| parse_policy_page(content).policy.unwrap_err();
        assert!(errors("no block here")[0].contains("p2p-policy"));
        assert!(errors(&page("version: [unclosed"))[0].starts_with("policy block:"));
        // Unknown keys and wrong types are errors, not ignored.
        assert!(errors(&page("version: 1\nenabled: true\nmax_accept_mode: ask\nextra: 1"))[0].contains("extra"));
        assert!(errors(&page("version: 1\nenabled: yes please\nmax_accept_mode: ask"))[0].contains("enabled"));
        assert!(errors(&page("version: 1\nenabled: true\nmax_accept_mode: always"))[0].contains("always"));
        assert!(errors(&page("version: 1\nenabled: true"))[0].contains("max_accept_mode"));
        assert!(
            errors(&page(
                "version: 1\nenabled: true\nmax_accept_mode: ask\nlimits:\n  max_hops: 40"
            ))[0]
                .contains("max_hops")
        );
        let big = page(&format!("version: 1\n# {}", "x".repeat(MAX_POLICY_BLOCK_BYTES)));
        assert!(errors(&big)[0].contains("larger"));
        // The smallest valid policy: everything else has a strict default.
        let minimal = parse_policy_page(&page("version: 1\nenabled: false\nmax_accept_mode: off"))
            .policy
            .expect("valid");
        assert!(minimal.calls.is_empty() && minimal.limits.max_hops == 2);
    }

    #[test]
    fn challenge_is_bound_to_its_user_and_key() {
        let p2p = state("0123456789abcdef0123456789abcdef");
        let (alice, bob) = (Uuid::new_v4(), Uuid::new_v4());
        let issued = p2p.issue_challenge(alice).unwrap();
        assert!(issued.expires_at > Utc::now());
        assert!(p2p.challenge_is_valid(alice, &issued.challenge));
        assert!(
            !p2p.challenge_is_valid(bob, &issued.challenge),
            "another user's challenge"
        );
        assert!(!state("fedcba9876543210fedcba9876543210").challenge_is_valid(alice, &issued.challenge));
        for bad in ["", "a", "a.b", ".", &format!("{}x", issued.challenge)] {
            assert!(!p2p.challenge_is_valid(alice, bad), "{bad}");
        }
    }

    #[test]
    fn expired_and_forged_challenges_are_rejected() {
        let p2p = state("0123456789abcdef0123456789abcdef");
        let user = Uuid::new_v4();
        let forge = |expires: i64, sign: bool| {
            let mut payload = [7u8; NONCE_BYTES + 8];
            payload[NONCE_BYTES..].copy_from_slice(&expires.to_be_bytes());
            let tag = if sign {
                p2p.mac(user, &payload).finalize().into_bytes().to_vec()
            } else {
                vec![0u8; 32]
            };
            format!("{}.{}", URL_SAFE_NO_PAD.encode(payload), URL_SAFE_NO_PAD.encode(tag))
        };
        let now = Utc::now().timestamp();
        assert!(p2p.challenge_is_valid(user, &forge(now + 30, true)));
        assert!(!p2p.challenge_is_valid(user, &forge(now - 1, true)), "expired");
        assert!(!p2p.challenge_is_valid(user, &forge(now + 30, false)), "unsigned");
    }

    #[test]
    fn bind_signature_covers_challenge_peer_and_user() {
        let key = SigningKey::from_bytes(&[9u8; 32]);
        let public = key.verifying_key().to_bytes();
        let peer_id = peer_id_from_ed25519(&public);
        let user = Uuid::new_v4();
        let sign = |challenge: &str, peer: &str, user: Uuid| {
            URL_SAFE_NO_PAD.encode(key.sign(&bind_message(challenge, peer, user)).to_bytes())
        };
        let signature = sign("c1", &peer_id, user);
        assert!(bind_signature_is_valid(&public, &signature, "c1", &peer_id, user));
        assert!(!bind_signature_is_valid(&public, &signature, "c2", &peer_id, user));
        assert!(!bind_signature_is_valid(
            &public,
            &signature,
            "c1",
            &peer_id,
            Uuid::new_v4()
        ));
        assert!(!bind_signature_is_valid(&public, &signature, "c1", "other", user));
        let other = SigningKey::from_bytes(&[3u8; 32]).verifying_key().to_bytes();
        assert!(!bind_signature_is_valid(&other, &signature, "c1", &peer_id, user));
        assert!(!bind_signature_is_valid(&public, "not base64 !", "c1", &peer_id, user));
        assert!(!bind_signature_is_valid(&public, "", "c1", &peer_id, user));
    }
}
