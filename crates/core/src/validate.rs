//! Configuration validation.
//!
//! Every check here is a mistake we refuse to persist. A flag that reaches the
//! database is guaranteed evaluable, so [`crate::engine::Reason::Error`] should
//! only ever be seen in tests.

use std::collections::{BTreeSet, HashSet};

use serde::{Deserialize, Serialize};

use crate::flag::{Condition, Distribution, Flag, Operator, TOTAL_WEIGHT};
use crate::segment::{Segment, SegmentMatch};

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

        check_conditions(&rule.conditions, &format!("rules[{i}]"), &mut issues);

        if let Some(required) = &rule.segments {
            check_segment_match(required, &format!("rules[{i}].segments"), &mut issues);
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

/// Rejects anything that would make a segment undecidable.
///
/// Separate from [`validate`] because a segment is stored on its own, not as
/// part of a flag: the two are written by different endpoints and each has to
/// be rejectable without the other.
pub fn validate_segment(segment: &Segment) -> Result<(), Vec<ValidationIssue>> {
    let mut issues = Vec::new();

    if !is_valid_key(&segment.key) {
        issues.push(ValidationIssue::new(
            "key",
            format!(
                "must be 1-{MAX_KEY_LEN} characters of [a-zA-Z0-9], `.`, `_` or `-`, got `{}`",
                segment.key
            ),
        ));
    }

    // Both lists holding the same context key is not ambiguous — exclusion wins
    // — but it is always a mistake, and silently honouring it hides the moment
    // someone adds an account to the wrong list.
    for key in segment.included.intersection(&segment.excluded) {
        issues.push(ValidationIssue::new(
            "excluded",
            format!("`{key}` is both included and excluded"),
        ));
    }

    let mut rule_ids = HashSet::new();
    for (i, rule) in segment.rules.iter().enumerate() {
        if !rule_ids.insert(rule.id) {
            issues.push(ValidationIssue::new(
                format!("rules[{i}].id"),
                format!("duplicate rule id `{}`", rule.id),
            ));
        }

        check_conditions(&rule.conditions, &format!("rules[{i}]"), &mut issues);

        if let Some(rollout) = &rule.rollout {
            let path = format!("rules[{i}].rollout");
            if rollout.percentage > TOTAL_WEIGHT {
                issues.push(ValidationIssue::new(
                    format!("{path}.percentage"),
                    format!("must be between 0 and {TOTAL_WEIGHT} (got {})", rollout.percentage),
                ));
            }
            if let Some(attribute) = &rollout.bucket_by
                && attribute.trim().is_empty()
            {
                issues.push(ValidationIssue::new(
                    format!("{path}.bucket_by"),
                    "bucketing attribute cannot be empty",
                ));
            }
        }
    }

    if issues.is_empty() { Ok(()) } else { Err(issues) }
}

/// Rejects rules pointing at segments the environment does not define.
///
/// This cannot live in [`validate`]: a dangling reference is only detectable
/// against the environment's segments, and the shape check is deliberately a
/// pure function of the flag alone. Takes the keys rather than the segments
/// because that is all it needs, and the caller that has only the keys is the
/// one on the write path.
pub fn validate_references(
    flag: &Flag,
    known: &BTreeSet<String>,
) -> Result<(), Vec<ValidationIssue>> {
    let mut issues = Vec::new();

    for (i, rule) in flag.rules.iter().enumerate() {
        let Some(required) = &rule.segments else { continue };
        for key in required.referenced() {
            if !known.contains(key) {
                issues.push(ValidationIssue::new(
                    format!("rules[{i}].segments"),
                    format!("no segment `{key}` in this environment"),
                ));
            }
        }
    }

    if issues.is_empty() { Ok(()) } else { Err(issues) }
}

fn check_segment_match(required: &SegmentMatch, path: &str, issues: &mut Vec<ValidationIssue>) {
    if required.is_empty() {
        issues.push(ValidationIssue::new(
            path,
            "a segment requirement must name at least one segment",
        ));
        return;
    }

    for key in required.referenced() {
        if key.trim().is_empty() {
            issues.push(ValidationIssue::new(path, "segment key cannot be empty"));
        }
    }

    // Requiring membership and forbidding it at once makes the rule dead code.
    let forbidden: HashSet<&str> = required.none_of.iter().map(String::as_str).collect();
    for key in required.any_of.iter().filter(|k| forbidden.contains(k.as_str())) {
        issues.push(ValidationIssue::new(
            path,
            format!("`{key}` is both required and excluded, so the rule can never match"),
        ));
    }
}

/// The per-condition checks, shared by flag rules and segment rules — the two
/// use the same [`Condition`] type and must reject the same mistakes.
fn check_conditions(conditions: &[Condition], prefix: &str, issues: &mut Vec<ValidationIssue>) {
    for (j, condition) in conditions.iter().enumerate() {
        let path = format!("{prefix}.conditions[{j}]");

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
            segments: None,
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
            segments: None,
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
            segments: None,
            distribution: Distribution::fixed("on"),
        }]);
        assert!(validate(&flag).is_ok());
    }

    // ------------------------------------------------------------ segments --

    use crate::segment::{SegmentRollout, SegmentRule};
    use std::collections::BTreeSet;

    fn segment_with(rules: Vec<SegmentRule>) -> Segment {
        Segment::new("beta").with_rules(rules)
    }

    #[test]
    fn an_empty_segment_is_valid() {
        assert!(validate_segment(&Segment::new("beta")).is_ok());
    }

    #[test]
    fn rejects_a_context_key_that_is_both_included_and_excluded() {
        let segment = Segment {
            included: BTreeSet::from(["u".to_owned()]),
            excluded: BTreeSet::from(["u".to_owned()]),
            ..Segment::new("beta")
        };
        let issues = validate_segment(&segment).unwrap_err();
        assert_eq!(paths(&issues), ["excluded"]);
    }

    #[test]
    fn rejects_a_segment_rollout_above_the_total() {
        let segment = segment_with(vec![SegmentRule {
            rollout: Some(SegmentRollout { percentage: TOTAL_WEIGHT + 1, bucket_by: None }),
            ..SegmentRule::new(Uuid::nil(), vec![])
        }]);
        assert_eq!(
            paths(&validate_segment(&segment).unwrap_err()),
            ["rules[0].rollout.percentage"]
        );
    }

    #[test]
    fn segment_rules_get_the_same_condition_checks_as_flag_rules() {
        let segment = segment_with(vec![SegmentRule::new(
            Uuid::nil(),
            vec![Condition::new(
                "email",
                Operator::Matches,
                vec![AttributeValue::String("([a-z".into())],
            )],
        )]);
        assert_eq!(
            paths(&validate_segment(&segment).unwrap_err()),
            ["rules[0].conditions[0].values[0]"]
        );
    }

    #[test]
    fn rejects_duplicate_segment_rule_ids() {
        let segment = segment_with(vec![
            SegmentRule::new(Uuid::nil(), vec![]),
            SegmentRule::new(Uuid::nil(), vec![]),
        ]);
        assert_eq!(paths(&validate_segment(&segment).unwrap_err()), ["rules[1].id"]);
    }

    #[test]
    fn rejects_a_segment_requirement_that_names_nothing() {
        let flag = Flag::boolean("f").with_rules(vec![
            Rule::new(Uuid::nil(), vec![], Distribution::fixed("on"))
                .targeting(SegmentMatch::default()),
        ]);
        assert_eq!(paths(&validate(&flag).unwrap_err()), ["rules[0].segments"]);
    }

    #[test]
    fn rejects_a_rule_that_both_requires_and_excludes_one_segment() {
        let flag = Flag::boolean("f").with_rules(vec![
            Rule::new(Uuid::nil(), vec![], Distribution::fixed("on")).targeting(SegmentMatch {
                any_of: vec!["beta".into()],
                none_of: vec!["beta".into()],
            }),
        ]);
        let issues = validate(&flag).unwrap_err();
        assert_eq!(paths(&issues), ["rules[0].segments"]);
        assert!(issues[0].message.contains("never match"));
    }

    #[test]
    fn rejects_references_to_segments_the_environment_does_not_define() {
        let flag = Flag::boolean("f").with_rules(vec![
            Rule::new(Uuid::nil(), vec![], Distribution::fixed("on"))
                .targeting(SegmentMatch::any_of(["ghost"])),
        ]);

        // The shape is fine on its own — only the environment can say otherwise.
        assert!(validate(&flag).is_ok());

        let none = BTreeSet::new();
        assert_eq!(paths(&validate_references(&flag, &none).unwrap_err()), ["rules[0].segments"]);

        let known = BTreeSet::from(["ghost".to_owned()]);
        assert!(validate_references(&flag, &known).is_ok());
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
