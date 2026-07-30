//! An immutable, self-contained view of one environment.
//!
//! Snapshots are what the API keeps in memory and hands to the evaluation
//! endpoint. Because a snapshot carries everything evaluation needs — flags
//! *and* the environment salt — serving a decision touches no database, and a
//! Postgres outage degrades to "flags are stale", not "flags are down".

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::context::EvaluationContext;
use crate::engine::{Evaluation, evaluate};
use crate::flag::Flag;
use crate::value::VariantValue;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct EnvironmentSnapshot {
    pub environment_id: Uuid,
    pub environment_key: String,
    /// Per-environment bucketing salt. Never serialized: it is a server-side
    /// secret, and leaking it would let anyone precompute who a rollout hits.
    #[serde(skip_serializing, default)]
    pub salt: String,
    /// Ordered so `evaluate_all` returns a stable sequence.
    pub flags: BTreeMap<String, Flag>,
    /// Highest `version` across the flags, used as an ETag-style cache token.
    pub version: i64,
    pub generated_at: DateTime<Utc>,
}

impl EnvironmentSnapshot {
    pub fn new(
        environment_id: Uuid,
        environment_key: impl Into<String>,
        salt: impl Into<String>,
        flags: impl IntoIterator<Item = Flag>,
        generated_at: DateTime<Utc>,
    ) -> Self {
        let flags: BTreeMap<String, Flag> = flags.into_iter().map(|f| (f.key.clone(), f)).collect();
        let version = flags.values().map(|f| f.version).max().unwrap_or(0);

        Self {
            environment_id,
            environment_key: environment_key.into(),
            salt: salt.into(),
            flags,
            version,
            generated_at,
        }
    }

    pub fn get(&self, flag_key: &str) -> Option<&Flag> {
        self.flags.get(flag_key)
    }

    pub fn len(&self) -> usize {
        self.flags.len()
    }

    pub fn is_empty(&self) -> bool {
        self.flags.is_empty()
    }

    /// Evaluates one flag. Unknown keys resolve to `fallback` rather than an
    /// error, mirroring how SDKs are expected to behave.
    pub fn evaluate(
        &self,
        flag_key: &str,
        ctx: &EvaluationContext,
        fallback: VariantValue,
    ) -> Evaluation {
        match self.get(flag_key) {
            Some(flag) => evaluate(flag, ctx, &self.salt),
            None => Evaluation::not_found(flag_key, fallback),
        }
    }

    /// Evaluates every flag in the environment — the call an SDK makes once at
    /// startup so it can answer locally afterwards.
    pub fn evaluate_all(&self, ctx: &EvaluationContext) -> Vec<Evaluation> {
        self.flags.values().map(|flag| evaluate(flag, ctx, &self.salt)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Reason;
    use crate::flag::Distribution;

    fn snapshot(flags: Vec<Flag>) -> EnvironmentSnapshot {
        EnvironmentSnapshot::new(Uuid::nil(), "production", "salt", flags, Utc::now())
    }

    #[test]
    fn version_tracks_the_newest_flag() {
        let snap = snapshot(vec![
            Flag { version: 3, ..Flag::boolean("a") },
            Flag { version: 9, ..Flag::boolean("b") },
        ]);
        assert_eq!(snap.version, 9);
        assert_eq!(snap.len(), 2);
    }

    #[test]
    fn unknown_flags_resolve_to_the_callers_fallback() {
        let snap = snapshot(vec![]);
        let out = snap.evaluate("nope", &EvaluationContext::new("u"), VariantValue::Bool(true));
        assert_eq!(out.reason, Reason::FlagNotFound);
        assert!(out.is_on(), "the caller's fallback must be preserved verbatim");
    }

    #[test]
    fn evaluate_all_is_ordered_and_complete() {
        let snap = snapshot(vec![
            Flag::boolean("zeta").enabled(true),
            Flag::boolean("alpha").enabled(false),
        ]);
        let out = snap.evaluate_all(&EvaluationContext::new("u"));
        assert_eq!(out.iter().map(|e| e.flag_key.as_str()).collect::<Vec<_>>(), ["alpha", "zeta"]);
        assert!(!out[0].is_on());
        assert!(out[1].is_on());
    }

    #[test]
    fn the_salt_never_leaks_into_serialized_output() {
        let snap =
            snapshot(vec![Flag { fallthrough: Distribution::fixed("on"), ..Flag::boolean("a") }]);
        let json = serde_json::to_string(&snap).unwrap();
        assert!(!json.contains("salt"), "{json}");
    }
}
