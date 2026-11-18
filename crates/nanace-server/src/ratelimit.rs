//! Per-user request limits for endpoints that are cheap to call and costly
//! to serve unbounded (P2P heartbeats, challenges, lookups).
//!
//! A token bucket per `(user, class)` held in memory. Each server replica
//! counts on its own, so this bounds abuse; it is not a quota.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use uuid::Uuid;

use crate::error::{ApiError, ApiResult};

/// Buckets kept before idle ones are dropped.
const PRUNE_ABOVE: usize = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RateClass {
    P2pChallenge,
    P2pBind,
    P2pPresence,
    P2pLookup,
    P2pExchange,
}

impl RateClass {
    /// Requests allowed per minute, which is also the burst size.
    const fn per_minute(self) -> f64 {
        match self {
            Self::P2pChallenge => 10.0,
            Self::P2pBind => 30.0,
            Self::P2pPresence | Self::P2pLookup => 120.0,
            Self::P2pExchange => 240.0,
        }
    }
}

struct Bucket {
    tokens: f64,
    updated: Instant,
}

#[derive(Default)]
pub struct RateLimiter {
    buckets: Mutex<HashMap<(Uuid, RateClass), Bucket>>,
}

impl RateLimiter {
    /// Takes one request from the caller's bucket, or fails with `429`.
    pub fn check(&self, user_id: Uuid, class: RateClass) -> ApiResult<()> {
        self.check_at(user_id, class, Instant::now())
            .map_err(|wait| ApiError::TooManyRequests {
                retry_after_secs: wait.as_secs().max(1),
            })
    }

    /// Returns how long to wait when the bucket is empty.
    fn check_at(&self, user_id: Uuid, class: RateClass, now: Instant) -> Result<(), Duration> {
        let capacity = class.per_minute();
        let per_sec = capacity / 60.0;
        // A poisoned lock only means another request panicked mid-update.
        let mut buckets = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        if buckets.len() > PRUNE_ABOVE {
            // A bucket idle for a minute is full again and carries no state.
            buckets.retain(|_, b| now.saturating_duration_since(b.updated) < Duration::from_secs(60));
        }
        let bucket = buckets.entry((user_id, class)).or_insert(Bucket {
            tokens: capacity,
            updated: now,
        });
        let elapsed = now.saturating_duration_since(bucket.updated).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * per_sec).min(capacity);
        bucket.updated = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Ok(())
        } else {
            Err(Duration::from_secs_f64((1.0 - bucket.tokens) / per_sec))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_empties_refills_and_is_per_user_and_class() {
        let limiter = RateLimiter::default();
        let (alice, bob) = (Uuid::new_v4(), Uuid::new_v4());
        let start = Instant::now();
        for _ in 0..10 {
            assert!(limiter.check_at(alice, RateClass::P2pChallenge, start).is_ok());
        }
        let wait = limiter.check_at(alice, RateClass::P2pChallenge, start).unwrap_err();
        assert_eq!(wait.as_secs(), 6, "one token returns every six seconds");

        assert!(limiter.check_at(bob, RateClass::P2pChallenge, start).is_ok());
        assert!(limiter.check_at(alice, RateClass::P2pLookup, start).is_ok());

        let later = start + Duration::from_secs(6);
        assert!(limiter.check_at(alice, RateClass::P2pChallenge, later).is_ok());
        assert!(limiter.check_at(alice, RateClass::P2pChallenge, later).is_err());

        let much_later = start + Duration::from_secs(3600);
        for _ in 0..10 {
            assert!(limiter.check_at(alice, RateClass::P2pChallenge, much_later).is_ok());
        }
        assert!(limiter.check_at(alice, RateClass::P2pChallenge, much_later).is_err());
    }

    #[test]
    fn rejection_is_a_429_with_at_least_one_second() {
        let limiter = RateLimiter::default();
        let user = Uuid::new_v4();
        let rejected = (0..200).find_map(|_| limiter.check(user, RateClass::P2pPresence).err());
        assert!(matches!(
            rejected,
            Some(ApiError::TooManyRequests { retry_after_secs }) if retry_after_secs >= 1
        ));
    }
}
