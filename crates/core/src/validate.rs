//! Configuration validation.
//!
//! Every check here is a mistake we refuse to persist. A flag that reaches the
//! database is guaranteed evaluable, so [`crate::engine::Reason::Error`] should
//! only ever be seen in tests.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::flag::{Distribution, Flag, Operator, TOTAL_WEIGHT};

/// Maximum length of a flag or variant key.
pub const MAX_KEY_LEN: usize = 128;

/// A single rejected thing, addressed by a JSON-pointer-ish path so an API can
/// point a form field at it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ValidationIssue {
    /// e.g. `rules[1].distribution.weights`
    pub path: String,
    pub message: String,
}

impl ValidationIssue {
    fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self { path: path.into(), message: message.into() }
    }
}

impl std::fmt::Display for ValidationIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path, self.message)
    }
}

/// Rejects anything the engine could not evaluate deterministically.
pub fn validate(flag: &Flag) -> Result<(), Vec<ValidationIssue>> {
    let mut issues = Vec::new();

    if !is_valid_key(&flag.key) {
        issues.push(ValidationIssue::new(
            "key",
            format!(
                "must be 1-{MAX_KEY_LEN} characters of [a-zA-Z0-9], `.`, `_` or `-`, got `{}`",
                flag.key
            ),
        ));
    }

    let mut variant_keys = HashSet::new();
    if flag.variants.is_empty() {
        issues.push(ValidationIssue::new("variants", "a flag needs at least one variant"));
    }
    for (i, variant) in flag.variants.iter().enumerate() {
        if !is_valid_key(&variant.key) {
            issues.push(ValidationIssue::new(
                format!("variants[{i}].key"),
                format!("invalid variant key `{}`", variant.key),
            ));
        }
        if !variant_keys.insert(variant.key.as_str()) {
            issues.push(ValidationIssue::new(
                format!("variants[{i}].key"),
                format!("duplicate variant key `{}`", variant.key),
            ));
        }
    }

    if !variant_keys.contains(flag.off_variant.as_str()) {
        issues.push(ValidationIssue::new(
            "off_variant",
            format!("`{}` is not one of the flag's variants", flag.off_variant),
        ));
    }

    check_distribution(&flag.fallthrough, "fallthrough", &variant_keys, &mut issues);

    let mut rule_ids = HashSet::new();
    for (i, rule) in flag.rules.iter().enumerate() {
        if !rule_ids.insert(rule.id) {
            issues.push(ValidationIssue::new(
                format!("rules[{i}].id"),
                format!("duplicate rule id `{}`", rule.id),
            ));
        }

        for (j, condition) in rule.conditions.iter().enumerate() {
            let path = format!("rules[{i}].conditions[{j}]");

            if condition.attribute.trim().is_empty() {
                issues.push(ValidationIssue::new(
                    format!("{path}.attribute"),
                    "attribute name cannot be empty",
                ));
            }

            if condition.operator.takes_values() && condition.values.is_empty() {
                issues.push(ValidationIssue::new(
                    format!("{path}.values"),
                    format!("operator `{:?}` requires at least one value", condition.operator),
                ));
            }

            if matches!(condition.operator, Operator::Matches | Operator::NotMatches) {
                for (k, value) in condition.values.iter().enumerate() {
                    match value.as_str() {
                        Some(pattern) => {
                            if let Err(err) = regex::Regex::new(pattern) {
                                issues.push(ValidationIssue::new(
                                    format!("{path}.values[{k}]"),
                                    format!("invalid regular expression: {err}"),
                                ));
                            }
                        }
                        None => issues.push(ValidationIssue::new(
                            format!("{path}.values[{k}]"),
                            "regex operators take string patterns",
                        )),
                    }
                }
            }
        }

        check_distribution(
            &rule.distribution,
            &format!("rules[{i}].distribution"),
            &variant_keys,
            &mut issues,
        );
    }

    if issues.is_empty() { Ok(()) } else { Err(issues) }
}

fn check_distribution(
    distribution: &Distribution,
    path: &str,
    variants: &HashSet<&str>,
    issues: &mut Vec<ValidationIssue>,
) {
    match distribution {
        Distribution::Fixed { variant } => {
            if !variants.contains(variant.as_str()) {
                issues.push(ValidationIssue::new(
                    format!("{path}.variant"),
                    format!("`{variant}` is not one of the flag's variants"),
                ));
            }
        }
        Distribution::Rollout { weights, bucket_by } => {
            if weights.is_empty() {
                issues.push(ValidationIssue::new(
                    format!("{path}.weights"),
                    "a rollout needs at least one weighted variant",
                ));
                return;
            }

            let mut seen = HashSet::new();
            for (i, entry) in weights.iter().enumerate() {
                if !variants.contains(entry.variant.as_str()) {
                    issues.push(ValidationIssue::new(
                        format!("{path}.weights[{i}].variant"),
                        format!("`{}` is not one of the flag's variants", entry.variant),
                    ));
                }
                if !seen.insert(entry.variant.as_str()) {
                    issues.push(ValidationIssue::new(
                        format!("{path}.weights[{i}].variant"),
                        format!("`{}` appears twice in the same rollout", entry.variant),
                    ));
                }
            }

            // Summing as u64 so a crafted config cannot overflow its way to a
            // "valid" total.
            let total: u64 = weights.iter().map(|w| u64::from(w.weight)).sum();
            if total != u64::from(TOTAL_WEIGHT) {
                issues.push(ValidationIssue::new(
                    format!("{path}.weights"),
                    format!("weights must sum to {TOTAL_WEIGHT} (got {total})"),
                ));
            }

            if let Some(attribute) = bucket_by
                && attribute.trim().is_empty()
            {
                issues.push(ValidationIssue::new(
                    format!("{path}.bucket_by"),
                    "bucketing attribute cannot be empty",
                ));
            }
        }
    }
}

fn is_valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= MAX_KEY_LEN
        && key.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flag::{Condition, Rule, Variant, WeightedVariant};
    use crate::value::AttributeValue;
    use uuid::Uuid;

    fn paths(issues: &[ValidationIssue]) -> Vec<&str> {
        issues.iter().map(|i| i.path.as_str()).collect()
    }

    #[test]
    fn a_default_boolean_flag_is_valid() {
        assert!(validate(&Flag::boolean("checkout.v2")).is_ok());
    }

    #[test]
    fn rejects_unknown_variant_references() {
        let flag = Flag { fallthrough: Distribution::fixed("ghost"), ..Flag::boolean("f") };
        let issues = validate(&flag).unwrap_err();
        assert_eq!(paths(&issues), ["fallthrough.variant"]);
    }

    #[test]
    fn rejects_weights_that_do_not_sum_to_the_total() {
        let flag = Flag {
            fallthrough: Distribution::Rollout {
                weights: vec![
                    WeightedVariant { variant: "on".into(), weight: 10 },
                    WeightedVariant { variant: "off".into(), weight: 10 },
                ],
                bucket_by: None,
            },
            ..Flag::boolean("f")
        };
        let issues = validate(&flag).unwrap_err();
        assert_eq!(paths(&issues), ["fallthrough.weights"]);
        assert!(issues[0].message.contains("100000"));
    }

    #[test]
    fn rejects_overflowing_weights() {
        let flag = Flag {
            fallthrough: Distribution::Rollout {
                weights: vec![
                    WeightedVariant { variant: "on".into(), weight: u32::MAX },
                    WeightedVariant { variant: "off".into(), weight: u32::MAX },
                ],
                bucket_by: None,
            },
            ..Flag::boolean("f")
        };
        assert!(validate(&flag).is_err());
    }

    #[test]
    fn rejects_duplicate_variant_keys() {
        let flag = Flag {
            variants: vec![
                Variant::new("on", true),
                Variant::new("on", false),
                Variant::new("off", false),
            ],
            ..Flag::boolean("f")
        };
        let issues = validate(&flag).unwrap_err();
        assert!(paths(&issues).contains(&"variants[1].key"));
    }

    #[test]
    fn rejects_invalid_keys() {
        let flag = Flag { key: "not a key".into(), ..Flag::boolean("f") };
        assert_eq!(paths(&validate(&flag).unwrap_err()), ["key"]);

        let flag = Flag { key: "a".repeat(MAX_KEY_LEN + 1), ..Flag::boolean("f") };
        assert_eq!(paths(&validate(&flag).unwrap_err()), ["key"]);
    }

    #[test]
    fn rejects_uncompilable_regexes_before_they_reach_the_engine() {
        let flag = Flag::boolean("f").with_rules(vec![Rule {
            id: Uuid::nil(),
            description: None,
            conditions: vec![Condition::new(
                "email",
                Operator::Matches,
                vec![AttributeValue::String("([a-z".into())],
            )],
            distribution: Distribution::fixed("on"),
        }]);
        let issues = validate(&flag).unwrap_err();
        assert_eq!(paths(&issues), ["rules[0].conditions[0].values[0]"]);
    }

    #[test]
    fn rejects_value_taking_operators_without_values() {
        let flag = Flag::boolean("f").with_rules(vec![Rule {
            id: Uuid::nil(),
            description: None,
            conditions: vec![Condition::new("plan", Operator::In, vec![])],
            distribution: Distribution::fixed("on"),
        }]);
        assert_eq!(paths(&validate(&flag).unwrap_err()), ["rules[0].conditions[0].values"]);
    }

    #[test]
    fn exists_does_not_need_values() {
        let flag = Flag::boolean("f").with_rules(vec![Rule {
            id: Uuid::nil(),
            description: None,
            conditions: vec![Condition::new("plan", Operator::Exists, vec![])],
            distribution: Distribution::fixed("on"),
        }]);
        assert!(validate(&flag).is_ok());
    }

    #[test]
    fn reports_every_problem_at_once() {
        let flag = Flag {
            key: "bad key".into(),
            off_variant: "nope".into(),
            fallthrough: Distribution::fixed("ghost"),
            ..Flag::boolean("f")
        };
        let issues = validate(&flag).unwrap_err();
        assert_eq!(issues.len(), 3, "{issues:?}");
    }
}
