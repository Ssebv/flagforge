//! The evaluation engine.
//!
//! Evaluation is a pure function of (flag, context, salt). It never fails: a
//! misconfigured flag degrades to its off variant with an explanatory reason,
//! because an SDK asking "is checkout.v2 on?" must always get an answer.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::bucket::{bucket, pick_weighted};
use crate::context::EvaluationContext;
use crate::flag::{Distribution, Flag};
use crate::matcher::rule_matches;
use crate::value::VariantValue;

/// Why the engine served what it served.
///
/// Surfacing this is what turns "the flag is wrong" into a two-minute
/// investigation: the caller can see whether a rule matched, which one, or
/// whether the flag was simply off.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum Reason {
    /// The flag's master switch is off.
    Off,
    /// Rule at `index` matched.
    TargetMatch { rule_id: Uuid, index: usize },
    /// The flag is on and no rule matched.
    Fallthrough,
    /// No flag with that key exists in the environment.
    FlagNotFound,
    /// The flag is internally inconsistent; the message says how.
    Error { message: String },
}

impl Reason {
    /// Whether the decision came from a healthy configuration. Used by the API
    /// layer to decide what to count as an error in metrics.
    pub fn is_healthy(&self) -> bool {
        !matches!(self, Self::Error { .. } | Self::FlagNotFound)
    }
}

/// One decision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Evaluation {
    pub flag_key: String,
    /// The variant key that was served, if one could be resolved.
    pub variant: Option<String>,
    pub value: VariantValue,
    pub reason: Reason,
    /// Configuration version that produced this decision.
    pub version: i64,
}

impl Evaluation {
    /// The answer for a key that is not configured in this environment.
    pub fn not_found(flag_key: impl Into<String>, fallback: VariantValue) -> Self {
        Self {
            flag_key: flag_key.into(),
            variant: None,
            value: fallback,
            reason: Reason::FlagNotFound,
            version: 0,
        }
    }

    /// Convenience for the common boolean case.
    pub fn is_on(&self) -> bool {
        self.value.as_bool().unwrap_or(false)
    }
}

/// Evaluates `flag` for `ctx` under the environment's `salt`.
pub fn evaluate(flag: &Flag, ctx: &EvaluationContext, salt: &str) -> Evaluation {
    if !flag.enabled {
        return serve(flag, &flag.off_variant, Reason::Off);
    }

    for (index, rule) in flag.rules.iter().enumerate() {
        if rule_matches(rule, ctx) {
            let reason = Reason::TargetMatch { rule_id: rule.id, index };
            return match resolve(flag, &rule.distribution, ctx, salt) {
                Ok(variant) => serve(flag, &variant, reason),
                Err(message) => degrade(flag, message),
            };
        }
    }

    match resolve(flag, &flag.fallthrough, ctx, salt) {
        Ok(variant) => serve(flag, &variant, Reason::Fallthrough),
        Err(message) => degrade(flag, message),
    }
}

/// Turns a distribution into a concrete variant key.
fn resolve(
    flag: &Flag,
    distribution: &Distribution,
    ctx: &EvaluationContext,
    salt: &str,
) -> Result<String, String> {
    match distribution {
        Distribution::Fixed { variant } => Ok(variant.clone()),
        Distribution::Rollout { weights, bucket_by } => {
            let Some(subject) = ctx.bucketing_subject(bucket_by.as_deref()) else {
                // We cannot bucket without a stable subject, and guessing would
                // reshuffle users on every request.
                return Err(format!(
                    "rollout buckets on attribute `{}`, which the context did not provide",
                    bucket_by.as_deref().unwrap_or("<key>")
                ));
            };
            let slot = bucket(salt, &flag.key, &subject);
            pick_weighted(weights, slot)
                .map(str::to_owned)
                .ok_or_else(|| format!("rollout weights do not cover bucket {slot}"))
        }
    }
}

/// Emits the decision for a resolved variant key.
fn serve(flag: &Flag, variant_key: &str, reason: Reason) -> Evaluation {
    match flag.variant(variant_key) {
        Some(variant) => Evaluation {
            flag_key: flag.key.clone(),
            variant: Some(variant.key.clone()),
            value: variant.value.clone(),
            reason,
            version: flag.version,
        },
        None => degrade(flag, format!("variant `{variant_key}` is not defined on this flag")),
    }
}

/// Last-resort answer for an inconsistent flag: the off variant if it still
/// resolves, otherwise `null`.
fn degrade(flag: &Flag, message: String) -> Evaluation {
    let value =
        flag.variant(&flag.off_variant).map(|v| v.value.clone()).unwrap_or_else(VariantValue::null);

    Evaluation {
        flag_key: flag.key.clone(),
        variant: None,
        value,
        reason: Reason::Error { message },
        version: flag.version,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flag::{Condition, Operator, Rule, TOTAL_WEIGHT, Variant, WeightedVariant};

    const SALT: &str = "env-salt";

    fn rule(conditions: Vec<Condition>, distribution: Distribution) -> Rule {
        Rule { id: Uuid::nil(), description: None, conditions, distribution }
    }

    #[test]
    fn a_disabled_flag_serves_off_and_skips_every_rule() {
        let flag = Flag::boolean("f")
            .with_rules(vec![rule(vec![], Distribution::fixed("on"))])
            .enabled(false);

        let out = evaluate(&flag, &EvaluationContext::new("u"), SALT);
        assert_eq!(out.reason, Reason::Off);
        assert!(!out.is_on());
    }

    #[test]
    fn an_enabled_flag_without_rules_falls_through() {
        let flag = Flag::boolean("f").enabled(true);
        let out = evaluate(&flag, &EvaluationContext::new("u"), SALT);
        assert_eq!(out.reason, Reason::Fallthrough);
        assert!(out.is_on());
    }

    #[test]
    fn the_first_matching_rule_wins() {
        let first = Uuid::from_u128(1);
        let second = Uuid::from_u128(2);
        let flag = Flag {
            variants: vec![
                Variant::new("a", "A"),
                Variant::new("b", "B"),
                Variant::new("off", false),
            ],
            rules: vec![
                Rule {
                    id: first,
                    description: None,
                    conditions: vec![Condition::new("plan", Operator::In, vec!["pro".into()])],
                    distribution: Distribution::fixed("a"),
                },
                Rule {
                    id: second,
                    description: None,
                    conditions: vec![],
                    distribution: Distribution::fixed("b"),
                },
            ],
            ..Flag::boolean("f").enabled(true)
        };

        let pro = EvaluationContext::new("u").with("plan", "pro");
        let out = evaluate(&flag, &pro, SALT);
        assert_eq!(out.variant.as_deref(), Some("a"));
        assert_eq!(out.reason, Reason::TargetMatch { rule_id: first, index: 0 });

        let free = EvaluationContext::new("u").with("plan", "free");
        let out = evaluate(&flag, &free, SALT);
        assert_eq!(out.variant.as_deref(), Some("b"));
        assert_eq!(out.reason, Reason::TargetMatch { rule_id: second, index: 1 });
    }

    #[test]
    fn rollouts_are_sticky_per_subject() {
        let flag = Flag {
            fallthrough: Distribution::Rollout {
                weights: vec![
                    WeightedVariant { variant: "on".into(), weight: 50_000 },
                    WeightedVariant { variant: "off".into(), weight: 50_000 },
                ],
                bucket_by: None,
            },
            ..Flag::boolean("f").enabled(true)
        };

        let ctx = EvaluationContext::new("user-42");
        let first = evaluate(&flag, &ctx, SALT);
        for _ in 0..50 {
            assert_eq!(evaluate(&flag, &ctx, SALT).variant, first.variant);
        }
    }

    #[test]
    fn a_fifty_percent_rollout_hits_roughly_half_the_population() {
        let flag = Flag {
            fallthrough: Distribution::Rollout {
                weights: vec![
                    WeightedVariant { variant: "on".into(), weight: 50_000 },
                    WeightedVariant { variant: "off".into(), weight: 50_000 },
                ],
                bucket_by: None,
            },
            ..Flag::boolean("f").enabled(true)
        };

        let on = (0..10_000)
            .filter(|i| evaluate(&flag, &EvaluationContext::new(format!("u-{i}")), SALT).is_on())
            .count();
        assert!((4_800..5_200).contains(&on), "expected ~5000 enabled, got {on}");
    }

    #[test]
    fn bucketing_by_an_attribute_keeps_a_group_together() {
        let flag = Flag {
            fallthrough: Distribution::Rollout {
                weights: vec![
                    WeightedVariant { variant: "on".into(), weight: 50_000 },
                    WeightedVariant { variant: "off".into(), weight: 50_000 },
                ],
                bucket_by: Some("account_id".into()),
            },
            ..Flag::boolean("f").enabled(true)
        };

        let decisions: Vec<_> = (0..25)
            .map(|i| {
                let ctx = EvaluationContext::new(format!("user-{i}")).with("account_id", "acme");
                evaluate(&flag, &ctx, SALT).is_on()
            })
            .collect();

        assert!(
            decisions.iter().all(|d| *d == decisions[0]),
            "everyone in one account must land on the same side"
        );
    }

    #[test]
    fn a_rollout_without_its_bucketing_attribute_reports_an_error() {
        let flag = Flag {
            fallthrough: Distribution::Rollout {
                weights: vec![WeightedVariant { variant: "on".into(), weight: TOTAL_WEIGHT }],
                bucket_by: Some("account_id".into()),
            },
            ..Flag::boolean("f").enabled(true)
        };

        let out = evaluate(&flag, &EvaluationContext::new("u"), SALT);
        assert!(matches!(out.reason, Reason::Error { .. }));
        // Degrading to the off value is what keeps a bad context from turning
        // a half-built feature on.
        assert!(!out.is_on());
    }

    #[test]
    fn an_unknown_variant_degrades_instead_of_panicking() {
        let flag =
            Flag { fallthrough: Distribution::fixed("ghost"), ..Flag::boolean("f").enabled(true) };
        let out = evaluate(&flag, &EvaluationContext::new("u"), SALT);
        assert!(matches!(out.reason, Reason::Error { .. }));
        assert_eq!(out.variant, None);
        assert!(!out.is_on());
    }

    #[test]
    fn changing_the_salt_reshuffles_the_population() {
        let flag = Flag {
            fallthrough: Distribution::Rollout {
                weights: vec![
                    WeightedVariant { variant: "on".into(), weight: 50_000 },
                    WeightedVariant { variant: "off".into(), weight: 50_000 },
                ],
                bucket_by: None,
            },
            ..Flag::boolean("f").enabled(true)
        };

        let moved = (0..500)
            .filter(|i| {
                let ctx = EvaluationContext::new(format!("u-{i}"));
                evaluate(&flag, &ctx, "staging").is_on()
                    != evaluate(&flag, &ctx, "production").is_on()
            })
            .count();
        assert!((200..300).contains(&moved), "expected ~half to move, got {moved}");
    }
}
