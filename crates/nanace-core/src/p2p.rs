//! Peer-to-peer registry domain logic: peer ids, the binding proof message and
//! validation of what a peer announces about itself.

use std::net::{Ipv4Addr, Ipv6Addr};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::validate::ValidationError;

pub const ED25519_PUBLIC_KEY_BYTES: usize = 32;
pub const MAX_AGENT_LABEL_BYTES: usize = 100;
pub const MAX_CAPABILITIES: usize = 16;
pub const MAX_CAPABILITY_BYTES: usize = 50;
pub const MAX_MULTIADDRS: usize = 8;
pub const MAX_MULTIADDR_BYTES: usize = 256;
pub const MAX_CLIENT_VERSION_BYTES: usize = 32;

/// Domain separator of the binding proof, so the signature cannot be reused
/// for anything else the peer key signs.
const BIND_CONTEXT: &[u8] = b"nanace-p2p-bind-v1\0";

fn invalid(field: &'static str, reason: &'static str) -> ValidationError {
    ValidationError { field, reason }
}

/// How a peer treats inbound requests, from most to least restrictive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcceptMode {
    Off,
    Ask,
    Trusted,
    Team,
}

impl AcceptMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Ask => "ask",
            Self::Trusted => "trusted",
            Self::Team => "team",
        }
    }
}

/// Where a peer runs: started by the desktop app, or on a server without it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientKind {
    Desktop,
    Server,
}

impl ClientKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Desktop => "desktop",
            Self::Server => "server",
        }
    }
}

/// The libp2p peer id of an Ed25519 public key: base58 of an identity
/// multihash over the protobuf-encoded key (`12D3KooW…`).
pub fn peer_id_from_ed25519(public_key: &[u8; ED25519_PUBLIC_KEY_BYTES]) -> String {
    // multihash: identity (0x00), length 36; protobuf PublicKey: field 1
    // (key type) = Ed25519 (1), field 2 (data) = 32 bytes.
    let mut bytes = Vec::with_capacity(6 + ED25519_PUBLIC_KEY_BYTES);
    bytes.extend_from_slice(&[0x00, 0x24, 0x08, 0x01, 0x12, 0x20]);
    bytes.extend_from_slice(public_key);
    bs58::encode(bytes).into_string()
}

/// The bytes a peer signs to prove it holds the key it binds to `user_id`.
/// Parts are separated by NUL, which none of them can contain.
pub fn bind_message(challenge: &str, peer_id: &str, user_id: Uuid) -> Vec<u8> {
    let mut message = BIND_CONTEXT.to_vec();
    for part in [challenge, peer_id, &user_id.to_string()] {
        message.extend_from_slice(part.as_bytes());
        message.push(0);
    }
    message
}

/// Labels name an agent inside a daemon, e.g. `alice/claude-code/7f3a`.
pub fn agent_label(value: &str) -> Result<(), ValidationError> {
    let valid_chars = value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/'));
    let valid_edges = !value.starts_with('/') && !value.ends_with('/') && !value.contains("//");
    if !value.is_empty() && value.len() <= MAX_AGENT_LABEL_BYTES && valid_chars && valid_edges {
        Ok(())
    } else {
        Err(invalid(
            "agent_label",
            "must be 1-100 chars of a-z, A-Z, 0-9, '-', '_', '.' or '/', without empty segments",
        ))
    }
}

/// Returns normalized capabilities: lowercased, deduplicated, sorted. They
/// are what a peer says about itself and are used for search only.
pub fn capabilities(values: &[String]) -> Result<Vec<String>, ValidationError> {
    if values.len() > MAX_CAPABILITIES {
        return Err(invalid("capabilities", "too many capabilities"));
    }
    let mut out = Vec::with_capacity(values.len());
    for raw in values {
        let capability = raw.trim().to_ascii_lowercase();
        let valid_chars = capability
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_' | b'.'));
        if capability.is_empty() || capability.len() > MAX_CAPABILITY_BYTES || !valid_chars {
            return Err(invalid(
                "capabilities",
                "each capability must be 1-50 chars of a-z, 0-9, '-', '_' or '.'",
            ));
        }
        out.push(capability);
    }
    out.sort();
    out.dedup();
    Ok(out)
}

pub fn client_version(value: &str) -> Result<(), ValidationError> {
    if !value.is_empty() && value.len() <= MAX_CLIENT_VERSION_BYTES && value.bytes().all(|b| b.is_ascii_graphic()) {
        Ok(())
    } else {
        Err(invalid("client_version", "must be 1-32 printable ASCII chars"))
    }
}

fn routable_v4(ip: Ipv4Addr) -> bool {
    !(ip.is_loopback() || ip.is_link_local() || ip.is_unspecified() || ip.is_multicast() || ip.is_broadcast())
}

fn routable_v6(ip: Ipv6Addr) -> bool {
    let link_local = ip.segments()[0] & 0xffc0 == 0xfe80;
    let mapped_ok = ip.to_ipv4_mapped().is_none_or(routable_v4);
    !(ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() || link_local) && mapped_ok
}

fn dns_name(value: &str) -> bool {
    let name = value.to_ascii_lowercase();
    let valid_chars = name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.'));
    let local = name == "localhost" || name.ends_with(".localhost") || name.ends_with(".local");
    !name.is_empty() && name.len() <= 253 && valid_chars && name.contains('.') && !local
}

/// Checks one text multiaddr a peer can be dialed at:
/// `/ip4|ip6|dns|dns4|dns6/<host>/tcp/<port>` or
/// `/…/udp/<port>/quic-v1`, optionally followed by `/p2p/<peer id>`.
/// Loopback, link-local, unspecified and multicast hosts are rejected; relay
/// circuit addresses are not accepted because every peer derives them itself.
fn multiaddr(value: &str) -> bool {
    if value.len() > MAX_MULTIADDR_BYTES {
        return false;
    }
    let mut parts = value.split('/');
    if parts.next() != Some("") {
        return false;
    }
    let host_ok = match (parts.next(), parts.next()) {
        (Some("ip4"), Some(host)) => host.parse().is_ok_and(routable_v4),
        (Some("ip6"), Some(host)) => host.parse().is_ok_and(routable_v6),
        (Some("dns" | "dns4" | "dns6"), Some(host)) => dns_name(host),
        _ => false,
    };
    let port_ok = |port: Option<&str>| port.is_some_and(|p| p.parse::<u16>().is_ok_and(|p| p != 0));
    let transport_ok = match parts.next() {
        Some("tcp") => port_ok(parts.next()),
        Some("udp") => port_ok(parts.next()) && parts.next() == Some("quic-v1"),
        _ => false,
    };
    let tail_ok = match (parts.next(), parts.next(), parts.next()) {
        (None, _, _) => true,
        (Some("p2p"), Some(id), None) => peer_id(id).is_ok(),
        _ => false,
    };
    host_ok && transport_ok && tail_ok
}

/// Returns the deduplicated addresses after checking each one.
pub fn multiaddrs(values: &[String]) -> Result<Vec<String>, ValidationError> {
    if values.len() > MAX_MULTIADDRS {
        return Err(invalid("multiaddrs", "too many addresses"));
    }
    let mut out: Vec<String> = Vec::with_capacity(values.len());
    for value in values {
        if !multiaddr(value) {
            return Err(invalid(
                "multiaddrs",
                "each address must be a dialable ip4/ip6/dns address over tcp or quic-v1, not loopback or link-local",
            ));
        }
        if !out.contains(value) {
            out.push(value.clone());
        }
    }
    Ok(out)
}

/// Checks that `value` is the peer id of some Ed25519 key and returns that key.
pub fn peer_id(value: &str) -> Result<[u8; ED25519_PUBLIC_KEY_BYTES], ValidationError> {
    let error = || invalid("peer_id", "must be an Ed25519 libp2p peer id");
    if value.len() > 64 {
        return Err(error());
    }
    let bytes = bs58::decode(value).into_vec().map_err(|_| error())?;
    let key: [u8; ED25519_PUBLIC_KEY_BYTES] = bytes
        .strip_prefix(&[0x00, 0x24, 0x08, 0x01, 0x12, 0x20])
        .and_then(|key| key.try_into().ok())
        .ok_or_else(error)?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_id_matches_libp2p_and_round_trips() {
        // Every Ed25519 peer id is 52 base58 chars and starts with this prefix.
        let id = peer_id_from_ed25519(&[0u8; 32]);
        assert!(id.starts_with("12D3KooW"), "{id}");
        assert_eq!(id.len(), 52);
        assert_eq!(peer_id(&id).unwrap(), [0u8; 32]);
        let other = peer_id_from_ed25519(&[7u8; 32]);
        assert_ne!(id, other);
        assert_eq!(peer_id(&other).unwrap(), [7u8; 32]);

        for bad in [
            "",
            "12D3KooW",
            "QmYwAPJzv5CZsnA625s3Xf2nemtYgPpHdWEz79ojWnPbdG",
            "not base58 !",
            &"1".repeat(65),
        ] {
            assert!(peer_id(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn bind_message_separates_its_parts() {
        let user = Uuid::nil();
        assert_ne!(bind_message("ab", "c", user), bind_message("a", "bc", user));
        assert!(bind_message("c", "p", user).starts_with(b"nanace-p2p-bind-v1\0"));
    }

    #[test]
    fn agent_label_rules() {
        for good in ["a", "alice/claude-code/session-1", "srv.agent_2"] {
            assert!(agent_label(good).is_ok(), "{good}");
        }
        for bad in ["", "/a", "a/", "a//b", "a b", "a\u{0}", "ж", &"a".repeat(101)] {
            assert!(agent_label(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn capabilities_are_normalized() {
        let caps = capabilities(&[" Infra".to_owned(), "review".to_owned(), "infra".to_owned()]).unwrap();
        assert_eq!(caps, ["infra", "review"]);
        assert!(capabilities(&["a b".to_owned()]).is_err());
        assert!(capabilities(&["*".to_owned()]).is_err());
        assert!(capabilities(&[String::new()]).is_err());
        assert!(capabilities(&vec!["a".to_owned(); 17]).is_err());
    }

    #[test]
    fn multiaddrs_must_be_dialable() {
        let id = peer_id_from_ed25519(&[1u8; 32]);
        let good = [
            "/ip4/203.0.113.7/tcp/4001".to_owned(),
            "/ip4/192.168.1.20/udp/4001/quic-v1".to_owned(),
            "/ip6/2001:db8::1/tcp/4001".to_owned(),
            "/dns4/node.example.com/tcp/443".to_owned(),
            format!("/ip4/203.0.113.7/tcp/4001/p2p/{id}"),
        ];
        assert_eq!(multiaddrs(&good).unwrap().len(), 5);
        assert_eq!(multiaddrs(&[good[0].clone(), good[0].clone()]).unwrap().len(), 1);

        for bad in [
            "",
            "ip4/203.0.113.7/tcp/4001",
            "/ip4/127.0.0.1/tcp/4001",
            "/ip4/169.254.1.1/tcp/4001",
            "/ip4/0.0.0.0/tcp/4001",
            "/ip4/224.0.0.1/tcp/4001",
            "/ip4/255.255.255.255/tcp/4001",
            "/ip6/::1/tcp/4001",
            "/ip6/fe80::1/tcp/4001",
            "/ip6/::ffff:127.0.0.1/tcp/4001",
            "/dns4/localhost/tcp/4001",
            "/dns4/printer.local/tcp/4001",
            "/ip4/203.0.113.7/tcp/0",
            "/ip4/203.0.113.7/tcp/70000",
            "/ip4/203.0.113.7/udp/4001",
            "/ip4/203.0.113.7/tcp/4001/p2p/nope",
            "/ip4/203.0.113.7/tcp/4001/p2p-circuit",
            "/ip4/203.0.113.7/tcp/4001/ws",
            "/unix/tmp/sock",
        ] {
            assert!(multiaddrs(&[bad.to_owned()]).is_err(), "{bad}");
        }
        assert!(multiaddrs(&vec![good[0].clone(); 9]).is_err());
    }

    #[test]
    fn accept_modes_are_ordered() {
        assert!(AcceptMode::Off < AcceptMode::Ask && AcceptMode::Ask < AcceptMode::Trusted);
        assert!(AcceptMode::Trusted < AcceptMode::Team);
        assert_eq!(AcceptMode::Ask.as_str(), "ask");
        assert_eq!(ClientKind::Server.as_str(), "server");
    }
}
