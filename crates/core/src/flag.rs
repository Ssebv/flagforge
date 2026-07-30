//! The flag configuration model: what an environment serves and to whom.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::value::{AttributeValue, VariantValue};

/// Total weight of a percentage rollout, in hundredths of a percent.
///
/// Using 100 000 rather than 100 lets an operator ship to 0.001 % of traffic,
/// which matters when a "1 %" canary still means thousands of requests.
pub const TOTAL_WEIGHT: u32 = 100_000;

/// One of the possible values a flag can serve.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Variant {
    pub key: String,
    pub value: VariantValue,
}

impl Variant {
    pub fn new(key: impl Into<String>, value: impl Into<VariantValue>) -> Self {
        Self { key: key.into(), value: value.into() }
    }
}

/// How a matched rule (or the fallthrough) picks among the variants.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum Distribution {
    /// Everyone who reaches this point gets the same variant.
    Fixed { variant: String },
    /// Traffic is split deterministically by hashing the subject.
    Rollout {
        weights: Vec<WeightedVariant>,
        /// Context attribute to bucket on instead of the context key. Bucketing
        /// on e.g. `account_id` keeps every user of one account on the same
        /// side of a rollout.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bucket_by: Option<String>,
    },
}

impl Distribution {
    pub fn fixed(variant: impl Into<String>) -> Self {
        Self::Fixed { variant: variant.into() }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct WeightedVariant {
    pub variant: String,
    /// Share of traffic in hundredths of a percent; all weights in a rollout
    /// must sum to [`TOTAL_WEIGHT`].
    pub weight: u32,
}

/// Comparison applied between a context attribute and a condition's values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum Operator {
    /// Attribute equals any of the values.
    In,
    /// Attribute equals none of the values.
    NotIn,
    Contains,
    NotContains,
    StartsWith,
    EndsWith,
    GreaterThan,
    GreaterThanOrEqual,
    LessThan,
    LessThanOrEqual,
    /// Regular expression match (values are patterns).
    Matches,
    NotMatches,
    SemverEqual,
    SemverGreaterThan,
    SemverLessThan,
    /// Attribute is present in the context, regardless of its value.
    Exists,
    NotExists,
}

impl Operator {
    /// Negated operators must hold for *every* value rather than for any one
    /// of them; the engine relies on this to invert its matching loop.
    pub fn is_negated(self) -> bool {
        matches!(self, Self::NotIn | Self::NotContains | Self::NotMatches | Self::NotExists)
    }

    pub fn takes_values(self) -> bool {
        !matches!(self, Self::Exists | Self::NotExists)
    }
}

/// A single predicate over one context attribute.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Condition {
    pub attribute: String,
    pub operator: Operator,
    #[serde(default)]
    pub values: Vec<AttributeValue>,
}

impl Condition {
    pub fn new(
        attribute: impl Into<String>,
        operator: Operator,
        values: Vec<AttributeValue>,
    ) -> Self {
        Self { attribute: attribute.into(), operator, values }
    }
}

/// An ordered targeting rule. All of its conditions must hold for it to match.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Rule {
    pub id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub conditions: Vec<Condition>,
    pub distribution: Distribution,
}

/// A flag as configured for one environment.
///
/// The same flag key exists in every environment of a project, but each
/// environment carries its own `enabled` switch, rules and rollout — that is
/// what makes "on in staging, 5 % in production" expressible.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
// Named `FlagDefinition` in the OpenAPI document: the management API also
// exposes a `Flag` (the stored record), and two schemas sharing a name means
// one silently overwrites the other.
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema), schema(as = FlagDefinition))]
pub struct Flag {
    pub key: String,
    pub variants: Vec<Variant>,
    /// Master switch. When false the flag serves [`Flag::off_variant`] and no
    /// rule is even considered.
    pub enabled: bool,
    /// Served when the flag is disabled.
    pub off_variant: String,
    /// Served when the flag is enabled but no rule matched.
    pub fallthrough: Distribution,
    #[serde(default)]
    pub rules: Vec<Rule>,
    /// Monotonic counter bumped on every write; surfaced in evaluation
    /// responses so clients can tell which configuration produced a decision.
    #[serde(default)]
    pub version: i64,
}

impl Flag {
    /// A boolean flag with the conventional `on`/`off` variants, serving `off`.
    pub fn boolean(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            variants: vec![Variant::new("on", true), Variant::new("off", false)],
            enabled: false,
            off_variant: "off".to_owned(),
            fallthrough: Distribution::fixed("on"),
            rules: Vec::new(),
            version: 1,
        }
    }

    pub fn variant(&self, key: &str) -> Option<&Variant> {
        self.variants.iter().find(|v| v.key == key)
    }

    pub fn with_rules(mut self, rules: Vec<Rule>) -> Self {
        self.rules = rules;
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distribution_tagging_is_stable_on_the_wire() {
        let json = serde_json::to_string(&Distribution::fixed("on")).unwrap();
        assert_eq!(json, r#"{"kind":"fixed","variant":"on"}"#);

        let rollout = Distribution::Rollout {
            weights: vec![WeightedVariant { variant: "on".into(), weight: TOTAL_WEIGHT }],
            bucket_by: None,
        };
        let json = serde_json::to_string(&rollout).unwrap();
        assert_eq!(json, r#"{"kind":"rollout","weights":[{"variant":"on","weight":100000}]}"#);
    }

    #[test]
    fn boolean_helper_builds_a_valid_off_by_default_flag() {
        let flag = Flag::boolean("checkout.v2");
        assert!(!flag.enabled);
        assert_eq!(flag.variant("off").unwrap().value.as_bool(), Some(false));
    }
}
