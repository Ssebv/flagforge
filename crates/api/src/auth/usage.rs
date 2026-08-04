//! Throttling `last_used_at` writes.
//!
//! Recording when an SDK key was last used is worth doing — a key nobody has
//! touched in six months is one you can revoke — but it is bookkeeping about
//! the hot path, and it was costing the hot path a database round trip.
//!
//! The `UPDATE` only ever changes a row once a minute (its `WHERE` clause says
//! so), yet it was being *issued* on every evaluation: a statement, a pooled
//! connection and a network hop to almost always match zero rows. This tracker
//! remembers in memory when each key was last written and skips the call
//! entirely in between.
//!
//! The database keeps the final say. Every node runs its own tracker, so with
//! N replicas up to N writes per interval can still be attempted, and the SQL
//! predicate is what makes that harmless — this is an optimisation, not a new
//! source of truth.

use std::time::{Duration, Instant};

use dashmap::DashMap;
use dashmap::mapref::entry::Entry;
use uuid::Uuid;

/// How long to wait between writes for one key.
///
/// Matches the `INTERVAL` in [`flagforge_storage::api_keys::touch`]: a shorter
/// value here would issue statements the database throws away, and a longer
/// one would make `last_used_at` staler than the query is willing to allow.
pub const RECORD_INTERVAL: Duration = Duration::from_secs(60);

/// Remembers which keys have had their usage written recently.
#[derive(Debug)]
pub struct UsageTracker {
    /// Only ever gains an entry for a key that authenticated successfully, so
    /// it is bounded by the number of live API keys — a number the operator
    /// controls. That is why there is no eviction sweep here, unlike the rate
    /// limiter, whose keys come from unauthenticated callers.
    last_written: DashMap<Uuid, Instant>,
    interval: Duration,
}

impl Default for UsageTracker {
    fn default() -> Self {
        Self::new(RECORD_INTERVAL)
    }
}

impl UsageTracker {
    pub fn new(interval: Duration) -> Self {
        Self { last_written: DashMap::new(), interval }
    }

    /// Whether this key's usage is worth writing now.
    ///
    /// Claims the slot as a side effect, so two concurrent requests for the
    /// same key produce one write rather than two.
    pub fn should_record(&self, key_id: Uuid) -> bool {
        self.should_record_at(key_id, Instant::now())
    }

    /// Split out so the tests can move time without sleeping through it.
    fn should_record_at(&self, key_id: Uuid, now: Instant) -> bool {
        match self.last_written.entry(key_id) {
            // First time this process has seen the key: worth recording, and
            // it is also how a restarted node repopulates `last_used_at`.
            Entry::Vacant(slot) => {
                slot.insert(now);
                true
            }
            Entry::Occupied(mut slot) => {
                if now.duration_since(*slot.get()) < self.interval {
                    return false;
                }
                slot.insert(now);
                true
            }
        }
    }

    pub fn tracked_keys(&self) -> usize {
        self.last_written.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_use_of_a_key_is_always_recorded() {
        let tracker = UsageTracker::default();
        assert!(tracker.should_record(Uuid::from_u128(1)));
    }

    #[test]
    fn a_burst_of_requests_produces_one_write() {
        let tracker = UsageTracker::default();
        let key = Uuid::from_u128(1);

        assert!(tracker.should_record(key));
        // This is the whole point: an SDK evaluating on every inbound request
        // must not put a statement on the database for each one.
        for _ in 0..10_000 {
            assert!(!tracker.should_record(key));
        }
        assert_eq!(tracker.tracked_keys(), 1);
    }

    #[test]
    fn a_key_is_recorded_again_once_the_interval_has_passed() {
        let tracker = UsageTracker::new(Duration::from_secs(60));
        let key = Uuid::from_u128(1);
        let start = Instant::now();

        assert!(tracker.should_record_at(key, start));
        assert!(!tracker.should_record_at(key, start + Duration::from_secs(59)));
        assert!(tracker.should_record_at(key, start + Duration::from_secs(60)));
        // ...and the clock restarts from the write, not from the first sight.
        assert!(!tracker.should_record_at(key, start + Duration::from_secs(119)));
    }

    #[test]
    fn keys_do_not_throttle_each_other() {
        let tracker = UsageTracker::default();

        assert!(tracker.should_record(Uuid::from_u128(1)));
        // A busy key must not suppress the record of a quiet one.
        assert!(tracker.should_record(Uuid::from_u128(2)));
        assert_eq!(tracker.tracked_keys(), 2);
    }

    #[test]
    fn the_interval_matches_what_the_query_enforces() {
        // If these drift apart the tracker either issues statements the
        // database discards, or holds writes back longer than it allows.
        assert_eq!(RECORD_INTERVAL, Duration::from_secs(60));
    }
}
