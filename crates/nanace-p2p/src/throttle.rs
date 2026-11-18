//! Bounds how often strangers can make this process ask the server about
//! them. A connection from an unknown peer id costs a registry lookup, and a
//! new key is free, so lookups are counted per remote address and in total.

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

use libp2p::Multiaddr;
use libp2p::multiaddr::Protocol;

/// At most `per_source` uses by one address and `total` by everyone in each
/// window. Connections without an address of their own (through the relay)
/// share one source.
pub struct Throttle {
    per_source: u32,
    total: u32,
    window: Duration,
    started: Instant,
    used: u32,
    sources: HashMap<Option<IpAddr>, u32>,
}

impl Throttle {
    pub fn new(per_source: u32, total: u32, window: Duration) -> Self {
        Self {
            per_source,
            total,
            window,
            started: Instant::now(),
            used: 0,
            sources: HashMap::new(),
        }
    }

    /// Takes one use for `source`, or says no.
    pub fn allow(&mut self, source: Option<IpAddr>, now: Instant) -> bool {
        if now.saturating_duration_since(self.started) >= self.window {
            self.started = now;
            self.used = 0;
            self.sources.clear();
        }
        if self.used >= self.total {
            return false;
        }
        let by_source = self.sources.entry(source).or_insert(0);
        if *by_source >= self.per_source {
            return false;
        }
        *by_source += 1;
        self.used += 1;
        true
    }
}

/// The remote IP address of a direct connection; `None` for one through a
/// relay, whose address is the relay's.
pub fn source_of(addr: &Multiaddr) -> Option<IpAddr> {
    if addr.iter().any(|p| matches!(p, Protocol::P2pCircuit)) {
        return None;
    }
    addr.iter().find_map(|p| match p {
        Protocol::Ip4(ip) => Some(IpAddr::V4(ip)),
        Protocol::Ip6(ip) => Some(IpAddr::V6(ip)),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_source_cannot_use_the_whole_budget() {
        let start = Instant::now();
        let mut throttle = Throttle::new(2, 3, Duration::from_secs(60));
        let (a, b, c) = (
            Some("10.0.0.1".parse().unwrap()),
            Some("10.0.0.2".parse().unwrap()),
            None,
        );
        assert!(throttle.allow(a, start));
        assert!(throttle.allow(a, start));
        assert!(!throttle.allow(a, start), "per source");
        assert!(throttle.allow(b, start));
        assert!(!throttle.allow(c, start), "total");
        // A new window starts empty.
        let later = start + Duration::from_secs(61);
        assert!(throttle.allow(a, later));
        assert!(throttle.allow(c, later));
    }

    #[test]
    fn sources_are_remote_addresses_and_relayed_ones_have_none() {
        let direct: Multiaddr = "/ip4/192.168.1.7/udp/4001/quic-v1".parse().unwrap();
        assert_eq!(source_of(&direct), Some("192.168.1.7".parse().unwrap()));
        let v6: Multiaddr = "/ip6/fe80::1/tcp/4001".parse().unwrap();
        assert_eq!(source_of(&v6), Some("fe80::1".parse().unwrap()));
        let relayed: Multiaddr =
            "/ip4/203.0.113.5/tcp/4001/p2p/12D3KooWGjMCwAKBeDBE5Sy5qBkPMkKxhfdmQbBRZy5GuGAxHnWf/p2p-circuit"
                .parse()
                .unwrap();
        assert_eq!(source_of(&relayed), None);
    }
}
