//! Condition matching: does this context satisfy this predicate?

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

use regex::Regex;

use crate::context::EvaluationContext;
use crate::flag::{Condition, Operator, Rule};
use crate::value::AttributeValue;

/// A rule matches when *every* one of its conditions matches. A rule with no
/// conditions targets everyone, which is how "roll out to 10 % of all users"
/// is expressed.
pub fn rule_matches(rule: &Rule, ctx: &EvaluationContext) -> bool {
    rule.conditions.iter().all(|c| condition_matches(c, ctx))
}

/// Evaluates one predicate.
///
/// A missing attribute never matches — including for negated operators. That
/// is deliberate: `country not_in [US]` silently targeting every context that
/// forgot to send `country` is the kind of rule that causes incidents.
pub fn condition_matches(condition: &Condition, ctx: &EvaluationContext) -> bool {
    let present = ctx.attribute(&condition.attribute);

    match condition.operator {
        Operator::Exists => return present.is_some(),
        Operator::NotExists => return present.is_none(),
        _ => {}
    }

    let Some(actual) = present else { return false };
    if condition.values.is_empty() {
        return false;
    }

    let base = positive_counterpart(condition.operator);
    let any_match = actual
        .scalars()
        .iter()
        .any(|scalar| condition.values.iter().any(|expected| compare(base, scalar, expected)));

    if condition.operator.is_negated() { !any_match } else { any_match }
}

/// Negated operators reuse their positive twin and invert the result, so there
/// is exactly one implementation of each comparison.
fn positive_counterpart(op: Operator) -> Operator {
    match op {
        Operator::NotIn => Operator::In,
        Operator::NotContains => Operator::Contains,
        Operator::NotMatches => Operator::Matches,
        other => other,
    }
}

fn compare(op: Operator, actual: &AttributeValue, expected: &AttributeValue) -> bool {
    match op {
        Operator::In => equals(actual, expected),

        Operator::Contains => text_pair(actual, expected).is_some_and(|(a, e)| a.contains(&e)),
        Operator::StartsWith => text_pair(actual, expected).is_some_and(|(a, e)| a.starts_with(&e)),
        Operator::EndsWith => text_pair(actual, expected).is_some_and(|(a, e)| a.ends_with(&e)),

        Operator::GreaterThan => numeric_pair(actual, expected).is_some_and(|(a, e)| a > e),
        Operator::GreaterThanOrEqual => numeric_pair(actual, expected).is_some_and(|(a, e)| a >= e),
        Operator::LessThan => numeric_pair(actual, expected).is_some_and(|(a, e)| a < e),
        Operator::LessThanOrEqual => numeric_pair(actual, expected).is_some_and(|(a, e)| a <= e),

        Operator::Matches => match (actual.to_text(), expected.as_str()) {
            (Some(text), Some(pattern)) => compiled(pattern).is_some_and(|re| re.is_match(&text)),
            _ => false,
        },

        Operator::SemverEqual => semver_pair(actual, expected).is_some_and(|(a, e)| a == e),
        Operator::SemverGreaterThan => semver_pair(actual, expected).is_some_and(|(a, e)| a > e),
        Operator::SemverLessThan => semver_pair(actual, expected).is_some_and(|(a, e)| a < e),

        // Handled before we get here.
        Operator::Exists | Operator::NotExists => false,
        // Negated forms are rewritten by `positive_counterpart`.
        Operator::NotIn | Operator::NotContains | Operator::NotMatches => false,
    }
}

/// Equality across types: numbers compare numerically, everything else by its
/// text projection, so `"3"` from a JSON body still equals a numeric `3`.
fn equals(actual: &AttributeValue, expected: &AttributeValue) -> bool {
    if let (Some(a), Some(e)) = (actual.as_f64(), expected.as_f64()) {
        return a == e;
    }
    match (actual.to_text(), expected.to_text()) {
        (Some(a), Some(e)) => a == e,
        _ => false,
    }
}

fn text_pair(actual: &AttributeValue, expected: &AttributeValue) -> Option<(String, String)> {
    Some((actual.to_text()?, expected.to_text()?))
}

fn numeric_pair(actual: &AttributeValue, expected: &AttributeValue) -> Option<(f64, f64)> {
    Some((actual.as_f64()?, expected.as_f64()?))
}

fn semver_pair(
    actual: &AttributeValue,
    expected: &AttributeValue,
) -> Option<(semver::Version, semver::Version)> {
    let (a, e) = text_pair(actual, expected)?;
    Some((lenient_semver(&a)?, lenient_semver(&e)?))
}

/// Accepts `1`, `1.2` and `1.2.3`, because app versions in the wild are rarely
/// spelled out in full.
fn lenient_semver(raw: &str) -> Option<semver::Version> {
    if let Ok(v) = raw.parse::<semver::Version>() {
        return Some(v);
    }
    let padded = match raw.split('.').count() {
        1 => format!("{raw}.0.0"),
        2 => format!("{raw}.0"),
        _ => return None,
    };
    padded.parse().ok()
}

/// Bounded cache of compiled patterns.
///
/// Rules live in long-lived snapshots, so the working set is the number of
/// distinct patterns an organisation has configured — small. The cap only
/// exists so that a pathological config cannot grow the process without bound;
/// past it we simply stop caching rather than evicting.
const REGEX_CACHE_CAPACITY: usize = 1024;

fn regex_cache() -> &'static RwLock<HashMap<String, Option<Arc<Regex>>>> {
    static CACHE: OnceLock<RwLock<HashMap<String, Option<Arc<Regex>>>>> = OnceLock::new();
    CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Returns the compiled pattern, or `None` if it does not compile. An invalid
/// pattern makes its condition fail closed instead of failing the request.
fn compiled(pattern: &str) -> Option<Arc<Regex>> {
    let cache = regex_cache();

    if let Ok(guard) = cache.read()
        && let Some(entry) = guard.get(pattern)
    {
        return entry.clone();
    }

    let compiled = Regex::new(pattern).ok().map(Arc::new);

    if let Ok(mut guard) = cache.write()
        && guard.len() < REGEX_CACHE_CAPACITY
    {
        guard.insert(pattern.to_owned(), compiled.clone());
    }

    compiled
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flag::Distribution;
    use uuid::Uuid;

    fn ctx() -> EvaluationContext {
        EvaluationContext::new("user-1")
            .with("email", "ada@example.com")
            .with("plan", "pro")
            .with("age", 34i64)
            .with("app_version", "2.4")
            .with("beta", true)
    }

    fn cond(attr: &str, op: Operator, values: Vec<AttributeValue>) -> Condition {
        Condition::new(attr, op, values)
    }

    #[test]
    fn membership_operators() {
        assert!(condition_matches(
            &cond("plan", Operator::In, vec!["pro".into(), "team".into()]),
            &ctx()
        ));
        assert!(!condition_matches(&cond("plan", Operator::In, vec!["free".into()]), &ctx()));
        assert!(condition_matches(&cond("plan", Operator::NotIn, vec!["free".into()]), &ctx()));
    }

    #[test]
    fn text_operators() {
        let c = ctx();
        assert!(condition_matches(
            &cond("email", Operator::EndsWith, vec!["@example.com".into()]),
            &c
        ));
        assert!(condition_matches(&cond("email", Operator::Contains, vec!["ada".into()]), &c));
        assert!(condition_matches(&cond("email", Operator::StartsWith, vec!["ada".into()]), &c));
        assert!(!condition_matches(
            &cond("email", Operator::EndsWith, vec!["@other.com".into()]),
            &c
        ));
    }

    #[test]
    fn numeric_operators_ignore_unparseable_values() {
        let c = ctx();
        assert!(condition_matches(&cond("age", Operator::GreaterThan, vec![30i64.into()]), &c));
        assert!(condition_matches(&cond("age", Operator::LessThanOrEqual, vec![34i64.into()]), &c));
        assert!(!condition_matches(&cond("age", Operator::GreaterThan, vec!["thirty".into()]), &c));
        assert!(!condition_matches(&cond("email", Operator::GreaterThan, vec![1i64.into()]), &c));
    }

    #[test]
    fn semver_accepts_partial_versions() {
        let c = ctx();
        assert!(condition_matches(
            &cond("app_version", Operator::SemverGreaterThan, vec!["2.3.9".into()]),
            &c
        ));
        assert!(condition_matches(
            &cond("app_version", Operator::SemverLessThan, vec!["3".into()]),
            &c
        ));
        assert!(condition_matches(
            &cond("app_version", Operator::SemverEqual, vec!["2.4.0".into()]),
            &c
        ));
    }

    #[test]
    fn regex_operators_and_invalid_patterns() {
        let c = ctx();
        assert!(condition_matches(&cond("email", Operator::Matches, vec![r"^ada@".into()]), &c));
        assert!(condition_matches(&cond("email", Operator::NotMatches, vec![r"^bob@".into()]), &c));
        // An unclosed group must not match rather than blow up.
        assert!(!condition_matches(&cond("email", Operator::Matches, vec!["([a-z".into()]), &c));
    }

    #[test]
    fn missing_attributes_never_match_even_when_negated() {
        let c = ctx();
        assert!(!condition_matches(&cond("country", Operator::In, vec!["US".into()]), &c));
        assert!(!condition_matches(&cond("country", Operator::NotIn, vec!["US".into()]), &c));
        assert!(condition_matches(&cond("country", Operator::NotExists, vec![]), &c));
        assert!(condition_matches(&cond("plan", Operator::Exists, vec![]), &c));
    }

    #[test]
    fn list_attributes_match_on_any_element() {
        let c = EvaluationContext::new("u")
            .with("roles", AttributeValue::List(vec!["admin".into(), "billing".into()]));
        assert!(condition_matches(&cond("roles", Operator::In, vec!["billing".into()]), &c));
        assert!(!condition_matches(&cond("roles", Operator::In, vec!["support".into()]), &c));
        // Negation over a list means "none of the elements".
        assert!(condition_matches(&cond("roles", Operator::NotIn, vec!["support".into()]), &c));
        assert!(!condition_matches(&cond("roles", Operator::NotIn, vec!["admin".into()]), &c));
    }

    #[test]
    fn numbers_and_their_string_spellings_compare_equal() {
        let c = EvaluationContext::new("u").with("tier", 3i64);
        assert!(condition_matches(&cond("tier", Operator::In, vec!["3".into()]), &c));
    }

    #[test]
    fn a_rule_requires_all_conditions() {
        let rule = Rule {
            id: Uuid::nil(),
            description: None,
            conditions: vec![
                cond("plan", Operator::In, vec!["pro".into()]),
                cond("age", Operator::GreaterThanOrEqual, vec![18i64.into()]),
            ],
            distribution: Distribution::fixed("on"),
        };
        assert!(rule_matches(&rule, &ctx()));

        let too_young = EvaluationContext::new("u").with("plan", "pro").with("age", 12i64);
        assert!(!rule_matches(&rule, &too_young));
    }

    #[test]
    fn a_rule_without_conditions_targets_everyone() {
        let rule = Rule {
            id: Uuid::nil(),
            description: None,
            conditions: vec![],
            distribution: Distribution::fixed("on"),
        };
        assert!(rule_matches(&rule, &EvaluationContext::new("anyone")));
    }

    #[test]
    fn conditions_with_no_values_never_match() {
        assert!(!condition_matches(&cond("plan", Operator::In, vec![]), &ctx()));
    }
}
