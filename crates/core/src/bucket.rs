//! Deterministic traffic bucketing.
//!
//! Every rollout decision must be reproducible without any shared state: the
//! same (environment salt, flag key, subject) triple has to land in the same
//! bucket on every server, in every process, forever. A cryptographic hash
//! gives us that plus a uniform spread, and the per-environment salt keeps a
//! user from being unlucky in the same 5 % of *every* flag.

use sha2::{Digest, Sha256};

use crate::flag::{TOTAL_WEIGHT, WeightedVariant};

/// Maps a subject onto `[0, TOTAL_WEIGHT)`.
///
/// The salt is environment-scoped, so the same user buckets differently in
/// staging and production — otherwise a rollout validated in staging would
/// hit exactly the same people again in production.
pub fn bucket(salt: &str, flag_key: &str, subject: &str) -> u32 {
    let mut hasher = Sha256::new();
    // Length-prefixing keeps ("ab", "c") from colliding with ("a", "bc").
    for part in [salt, flag_key, subject] {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part.as_bytes());
    }
    let digest = hasher.finalize();

    let head = u64::from_be_bytes(digest[..8].try_into().expect("sha256 yields 32 bytes"));
    // Modulo bias here is on the order of 2^-44 — far below anything a
    // rollout percentage could express.
    (head % u64::from(TOTAL_WEIGHT)) as u32
}

/// Walks the cumulative weights and returns the variant owning `bucket`.
///
/// Returns `None` only when the weights are empty or sum to less than the
/// bucket, which [`crate::validate`] rejects before a flag is ever stored.
pub fn pick_weighted(weights: &[WeightedVariant], bucket: u32) -> Option<&str> {
    let mut cumulative: u64 = 0;
    for entry in weights {
        cumulative += u64::from(entry.weight);
        if u64::from(bucket) < cumulative {
            return Some(&entry.variant);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn weights(pairs: &[(&str, u32)]) -> Vec<WeightedVariant> {
        pairs
            .iter()
            .map(|(v, w)| WeightedVariant { variant: (*v).to_owned(), weight: *w })
            .collect()
    }

    #[test]
    fn bucketing_is_deterministic() {
        let a = bucket("salt", "flag", "user-1");
        let b = bucket("salt", "flag", "user-1");
        assert_eq!(a, b);
    }

    #[test]
    fn bucket_is_always_in_range() {
        for i in 0..5_000 {
            assert!(bucket("s", "f", &format!("user-{i}")) < TOTAL_WEIGHT);
        }
    }

    #[test]
    fn different_salts_move_a_subject() {
        // Not a guarantee for any single subject, but across a population the
        // two assignments must not be identical.
        let differing = (0..200)
            .filter(|i| {
                let subject = format!("user-{i}");
                bucket("staging", "f", &subject) != bucket("production", "f", &subject)
            })
            .count();
        assert!(differing > 190, "salts barely changed the assignment: {differing}/200");
    }

    #[test]
    fn boundaries_belong_to_the_lower_variant() {
        let w = weights(&[("a", 30_000), ("b", 70_000)]);
        assert_eq!(pick_weighted(&w, 0), Some("a"));
        assert_eq!(pick_weighted(&w, 29_999), Some("a"));
        assert_eq!(pick_weighted(&w, 30_000), Some("b"));
        assert_eq!(pick_weighted(&w, TOTAL_WEIGHT - 1), Some("b"));
    }

    #[test]
    fn zero_weight_variants_never_win() {
        let w = weights(&[("never", 0), ("always", TOTAL_WEIGHT)]);
        for b in [0, 1, 50_000, TOTAL_WEIGHT - 1] {
            assert_eq!(pick_weighted(&w, b), Some("always"));
        }
    }

    /// A rollout is only trustworthy if the hash spreads evenly: a "10 %"
    /// rollout that actually hits 3 % of users is a silent incident.
    #[test]
    fn hash_distributes_uniformly_across_deciles() {
        const N: u32 = 40_000;
        let mut deciles = [0u32; 10];
        for i in 0..N {
            let b = bucket("salt", "flag", &format!("user-{i}"));
            deciles[(b / (TOTAL_WEIGHT / 10)) as usize] += 1;
        }
        let expected = f64::from(N) / 10.0;
        for (i, count) in deciles.iter().enumerate() {
            let deviation = (f64::from(*count) - expected).abs() / expected;
            assert!(deviation < 0.05, "decile {i} off by {:.2}%", deviation * 100.0);
        }
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn bucket_stays_within_range(salt: String, flag: String, subject: String) {
            prop_assert!(bucket(&salt, &flag, &subject) < TOTAL_WEIGHT);
        }

        #[test]
        fn bucket_is_a_pure_function(salt: String, flag: String, subject: String) {
            prop_assert_eq!(
                bucket(&salt, &flag, &subject),
                bucket(&salt, &flag, &subject)
            );
        }

        /// Concatenation must not be ambiguous: distinct field splits of the
        /// same character stream have to produce distinct hashes.
        #[test]
        fn field_boundaries_are_unambiguous(a in "[a-z]{1,8}", b in "[a-z]{1,8}") {
            prop_assume!(!a.is_empty() && !b.is_empty());
            let joined = format!("{a}{b}");
            prop_assert_ne!(bucket("s", &a, &b), bucket("s", &joined, ""));
        }

        /// Any bucket must be claimed by exactly one variant when the weights
        /// form a full partition.
        #[test]
        fn full_partitions_always_resolve(split in 0u32..=TOTAL_WEIGHT, b in 0u32..TOTAL_WEIGHT) {
            let weights = vec![
                WeightedVariant { variant: "a".into(), weight: split },
                WeightedVariant { variant: "b".into(), weight: TOTAL_WEIGHT - split },
            ];
            let picked = pick_weighted(&weights, b);
            prop_assert!(picked.is_some());
            prop_assert_eq!(picked.unwrap(), if b < split { "a" } else { "b" });
        }
    }
}
