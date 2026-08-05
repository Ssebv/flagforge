//! Reusable audiences.
//!
//! A rule's conditions describe an audience inline, which is fine until the
//! same audience is wanted on a dozen flags: "our beta testers" then lives in
//! twelve places and drifts in eleven of them. A segment names that audience
//! once, per environment, and flag rules reference it by key.
//!
//! Membership is a pure function of `(segment, context, salt)` — the same
//! property the engine has, and for the same reason.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::flag::Condition;

/// The segments of one environment, keyed by segment key.
pub type SegmentSet = BTreeMap<String, Segment>;

/// A named audience, scoped to one environment.
///
/// Membership is decided in a fixed order — excluded, then included, then the
/// rules — so an exclusion is always the last word. That ordering is what makes
/// "everyone in the beta cohort *except* this one account" expressible without
/// rewriting the rules.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
// Named `SegmentDefinition` in the OpenAPI document, for the same reason
// `Flag` is renamed there: the management API exposes its own `Segment` (the
// stored record), and two schemas sharing a name means one silently
// overwrites the other.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema), schema(as = SegmentDefinition))]
pub struct Segment {
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Context keys that are always members, whatever the rules say.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub included: BTreeSet<String>,
    /// Context keys that are never members. Beats `included` and every rule.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub excluded: BTreeSet<String>,
    /// A context is a member when *any* rule matches — rules are alternatives,
    /// unlike the conditions within one rule, which must all hold.
    #[serde(default)]
    pub rules: Vec<SegmentRule>,
    /// Bumped on every write, like a flag's. Segments are part of what a
    /// snapshot's version has to cover: editing one changes evaluation for
    /// every flag that references it.
    #[serde(default)]
    pub version: i64,
}

impl Segment {
    /// An empty segment with the given key: nobody is a member yet.
    pub fn new(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            description: None,
            included: BTreeSet::new(),
            excluded: BTreeSet::new(),
            rules: Vec::new(),
            version: 1,
        }
    }

    pub fn with_rules(mut self, rules: Vec<SegmentRule>) -> Self {
        self.rules = rules;
        self
    }
}

/// One alternative path into a segment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SegmentRule {
    pub id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// All of these must hold. An empty list matches every context, which is
    /// how "a random 10 % of everyone" is expressed.
    #[serde(default)]
    pub conditions: Vec<Condition>,
    /// Narrows the matching population to a deterministic share of itself.
    /// Absent means all of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollout: Option<SegmentRollout>,
}

impl SegmentRule {
    pub fn new(id: Uuid, conditions: Vec<Condition>) -> Self {
        Self { id, description: None, conditions, rollout: None }
    }
}

/// A deterministic share of the contexts a segment rule matches.
///
/// Bucketing happens on the *segment* key rather than any flag's, so a context
/// that is in the cohort is in it for every flag that references the segment.
/// That is the whole point: a cohort that reshuffled per flag would not be a
/// cohort.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SegmentRollout {
    /// Share of the matching population, in hundredths of a percent out of
    /// [`crate::flag::TOTAL_WEIGHT`].
    pub percentage: u32,
    /// Attribute to bucket on instead of the context key, with the same
    /// meaning it has on a flag rollout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bucket_by: Option<String>,
}

/// Segment membership required by a flag rule, on top of its conditions.
///
/// Within one flag rule the conditions and the segments are ANDed, `any_of` is
/// an OR and `none_of` a NOR. There is deliberately no way to OR a segment
/// with a condition: a rule is a conjunction, and expressing alternatives is
/// what the *next* rule in the list is for.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SegmentMatch {
    /// The context must be in at least one of these. Empty means no positive
    /// requirement, which is what makes a `none_of`-only match useful.
    #[serde(default)]
    pub any_of: Vec<String>,
    /// …and in none of these.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub none_of: Vec<String>,
}

impl SegmentMatch {
    /// Requires membership in any one of `keys`.
    pub fn any_of<I, S>(keys: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self { any_of: keys.into_iter().map(Into::into).collect(), none_of: Vec::new() }
    }

    /// Excludes membership in any of `keys`.
    pub fn none_of<I, S>(keys: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self { any_of: Vec::new(), none_of: keys.into_iter().map(Into::into).collect() }
    }

    pub fn is_empty(&self) -> bool {
        self.any_of.is_empty() && self.none_of.is_empty()
    }

    /// Every segment key this refers to, for validation and dependency checks.
    pub fn referenced(&self) -> impl Iterator<Item = &str> {
        self.any_of.iter().chain(self.none_of.iter()).map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_collections_stay_out_of_the_wire_format() {
        let json = serde_json::to_string(&Segment::new("beta")).unwrap();
        assert_eq!(json, r#"{"key":"beta","rules":[],"version":1}"#);
    }

    #[test]
    fn a_segment_round_trips_through_json() {
        let segment = Segment {
            included: BTreeSet::from(["always-in".to_owned()]),
            excluded: BTreeSet::from(["never-in".to_owned()]),
            ..Segment::new("beta").with_rules(vec![SegmentRule {
                rollout: Some(SegmentRollout { percentage: 10_000, bucket_by: None }),
                ..SegmentRule::new(Uuid::nil(), vec![])
            }])
        };

        let json = serde_json::to_string(&segment).unwrap();
        assert_eq!(serde_json::from_str::<Segment>(&json).unwrap(), segment);
    }

    #[test]
    fn a_segment_match_lists_both_sides_of_its_reference() {
        let m = SegmentMatch { any_of: vec!["a".into()], none_of: vec!["b".into()] };
        assert_eq!(m.referenced().collect::<Vec<_>>(), ["a", "b"]);
        assert!(!m.is_empty());
        assert!(SegmentMatch::default().is_empty());
    }
}
