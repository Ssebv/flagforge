//! An immutable, self-contained view of one environment.
//!
//! Snapshots are what the API keeps in memory and hands to the evaluation
//! endpoint. Because a snapshot carries everything evaluation needs — the
//! flags, the segments their rules reference, and the environment salt —
//! serving a decision touches no database, and a Postgres outage degrades to
//! "flags are stale", not "flags are down".

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::context::EvaluationContext;
use crate::engine::{Evaluation, EvaluationEnv, evaluate};
use crate::experiment::ExperimentSpec;
use crate::flag::Flag;
use crate::segment::{Segment, SegmentSet};
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
    /// The audiences the flags' rules reference. They travel with the flags
    /// because a rule that names one cannot be evaluated without it — an SDK
    /// holding flags but no segments would quietly stop matching.
    #[serde(default)]
    pub segments: SegmentSet,
    /// The experiments currently running, so an SDK can attribute exposures
    /// and conversions locally. Only running ones travel: a draft is not yet
    /// measuring and a stopped one must stop counting everywhere at once.
    #[serde(default)]
    pub experiments: Vec<ExperimentSpec>,
    /// Highest `version` across flags, segments *and* running experiments,
    /// used as an ETag-style cache token. Segments count because editing one
    /// changes what every flag referencing it serves, without touching any
    /// flag's own version; experiments count because starting or stopping one
    /// changes what SDKs should record.
    pub version: i64,
    pub generated_at: DateTime<Utc>,
}

impl EnvironmentSnapshot {
    pub fn new(
        environment_id: Uuid,
        environment_key: impl Into<String>,
        salt: impl Into<String>,
        flags: impl IntoIterator<Item = Flag>,
        segments: impl IntoIterator<Item = Segment>,
        generated_at: DateTime<Utc>,
    ) -> Self {
        let flags: BTreeMap<String, Flag> = flags.into_iter().map(|f| (f.key.clone(), f)).collect();
        let segments: SegmentSet = segments.into_iter().map(|s| (s.key.clone(), s)).collect();

        let version = flags
            .values()
            .map(|f| f.version)
            .chain(segments.values().map(|s| s.version))
            .max()
            .unwrap_or(0);

        Self {
            environment_id,
            environment_key: environment_key.into(),
            salt: salt.into(),
            flags,
            segments,
            experiments: Vec::new(),
            version,
            generated_at,
        }
    }

    /// Attaches the running experiments, folding their versions into the
    /// snapshot's so starting or stopping one is a visible change.
    pub fn with_experiments(mut self, experiments: Vec<ExperimentSpec>) -> Self {
        self.version =
            experiments.iter().map(|e| e.version).chain([self.version]).max().unwrap_or(0);
        self.experiments = experiments;
        self
    }

    pub fn get(&self, flag_key: &str) -> Option<&Flag> {
        self.flags.get(flag_key)
    }

    pub fn segment(&self, segment_key: &str) -> Option<&Segment> {
        self.segments.get(segment_key)
    }

    /// The environment half of an evaluation, ready to hand to [`evaluate`].
    pub fn env(&self) -> EvaluationEnv<'_> {
        EvaluationEnv::with_segments(&self.salt, &self.segments)
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
            Some(flag) => evaluate(flag, ctx, &self.env()),
            None => Evaluation::not_found(flag_key, fallback),
        }
    }

    /// Evaluates every flag in the environment — the call an SDK makes once at
    /// startup so it can answer locally afterwards.
    pub fn evaluate_all(&self, ctx: &EvaluationContext) -> Vec<Evaluation> {
        let env = self.env();
        self.flags.values().map(|flag| evaluate(flag, ctx, &env)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Reason;
    use crate::flag::Distribution;

    fn snapshot(flags: Vec<Flag>) -> EnvironmentSnapshot {
        EnvironmentSnapshot::new(Uuid::nil(), "production", "salt", flags, [], Utc::now())
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

    /// A segment edit has to move the snapshot version even when no flag was
    /// touched, or every cache keyed on that version would keep serving the
    /// audience the segment used to have.
    #[test]
    fn version_covers_segments_too() {
        let snap = EnvironmentSnapshot::new(
            Uuid::nil(),
            "production",
            "salt",
            vec![Flag { version: 3, ..Flag::boolean("a") }],
            vec![Segment { version: 11, ..Segment::new("beta") }],
            Utc::now(),
        );
        assert_eq!(snap.version, 11);
        assert!(snap.segment("beta").is_some());
    }

    #[test]
    fn a_rule_resolves_its_segment_through_the_snapshot() {
        use crate::flag::{Condition, Operator, Rule};
        use crate::segment::{SegmentMatch, SegmentRule};
        use uuid::Uuid as U;

        let beta = Segment::new("beta").with_rules(vec![SegmentRule::new(
            U::nil(),
            vec![Condition::new("plan", Operator::In, vec!["pro".into()])],
        )]);

        let flag = Flag::boolean("f").enabled(true).with_rules(vec![
            Rule::new(U::from_u128(7), vec![], Distribution::fixed("on"))
                .targeting(SegmentMatch::any_of(["beta"])),
        ]);
        let flag = Flag { fallthrough: Distribution::fixed("off"), ..flag };

        let snap = EnvironmentSnapshot::new(
            Uuid::nil(),
            "production",
            "salt",
            vec![flag],
            vec![beta],
            Utc::now(),
        );

        let pro = EvaluationContext::new("u").with("plan", "pro");
        assert!(snap.evaluate("f", &pro, VariantValue::Bool(false)).is_on());

        let free = EvaluationContext::new("u").with("plan", "free");
        assert!(!snap.evaluate("f", &free, VariantValue::Bool(false)).is_on());
    }

    /// Starting an experiment must move the snapshot version for the same
    /// reason a segment edit does: SDK behaviour changes without any flag
    /// having been written.
    #[test]
    fn version_covers_running_experiments_too() {
        use crate::experiment::ExperimentSpec;

        let snap =
            snapshot(vec![Flag { version: 3, ..Flag::boolean("a") }]).with_experiments(vec![
                ExperimentSpec {
                    key: "exp".into(),
                    flag_key: "a".into(),
                    metric_key: "converted".into(),
                    control_variant: "off".into(),
                    version: 8,
                },
            ]);
        assert_eq!(snap.version, 8);
        assert_eq!(snap.experiments.len(), 1);

        // And a snapshot serialized before experiments existed still loads.
        let old = r#"{"environment_id":"00000000-0000-0000-0000-000000000000",
                      "environment_key":"production","flags":{},"version":1,
                      "generated_at":"2026-01-01T00:00:00Z"}"#;
        let parsed: EnvironmentSnapshot = serde_json::from_str(old).unwrap();
        assert!(parsed.experiments.is_empty());
    }

    #[test]
    fn the_salt_never_leaks_into_serialized_output() {
        let snap =
            snapshot(vec![Flag { fallthrough: Distribution::fixed("on"), ..Flag::boolean("a") }]);
        let json = serde_json::to_string(&snap).unwrap();
        assert!(!json.contains("salt"), "{json}");
    }
}
