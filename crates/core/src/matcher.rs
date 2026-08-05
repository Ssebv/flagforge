//! Condition matching: does this context satisfy this predicate?

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

use regex::Regex;

use crate::bucket::bucket;
use crate::context::EvaluationContext;
use crate::engine::EvaluationEnv;
use crate::flag::{Condition, Operator, Rule};
use crate::segment::{Segment, SegmentMatch, SegmentRule};
use crate::value::AttributeValue;

/// A rule matches when *every* one of its conditions matches and its segment
/// requirement holds. A rule with neither targets everyone, which is how "roll
/// out to 10 % of all users" is expressed.
pub fn rule_matches(rule: &Rule, ctx: &EvaluationContext, env: &EvaluationEnv<'_>) -> bool {
    rule.conditions.iter().all(|c| condition_matches(c, ctx))
        && rule.segments.as_ref().is_none_or(|m| segment_match_holds(m, ctx, env))
}

/// Whether `ctx` satisfies a rule's segment requirement.
pub fn segment_match_holds(
    required: &SegmentMatch,
    ctx: &EvaluationContext,
    env: &EvaluationEnv<'_>,
) -> bool {
    let positive =
        required.any_of.is_empty() || required.any_of.iter().any(|k| in_segment(k, ctx, env));

    positive && !required.none_of.iter().any(|k| in_segment(k, ctx, env))
}

/// Whether `ctx` is a member of the segment named `key`.
///
/// A key the environment does not define contains nobody, so an `any_of`
/// reference to it never matches and a `none_of` reference never excludes.
/// Validation stops dangling references from being stored; this only decides
/// the moment a snapshot lags a deletion, and failing closed is the safe
/// direction there.
pub fn in_segment(key: &str, ctx: &EvaluationContext, env: &EvaluationEnv<'_>) -> bool {
    env.segments.get(key).is_some_and(|segment| segment_contains(segment, ctx, env.salt))
}

/// Membership in one segment, decided exclusions first.
///
/// The order is the contract: an exclusion beats an inclusion, and both beat
/// the rules. Anything else would make "everyone in the cohort except this
/// account" impossible to express without rewriting the cohort.
pub fn segment_contains(segment: &Segment, ctx: &EvaluationContext, salt: &str) -> bool {
    if segment.excluded.contains(&ctx.key) {
        return false;
    }
    if segment.included.contains(&ctx.key) {
        return true;
    }
    segment.rules.iter().any(|rule| segment_rule_matches(rule, &segment.key, ctx, salt))
}

fn segment_rule_matches(
    rule: &SegmentRule,
    segment_key: &str,
    ctx: &EvaluationContext,
    salt: &str,
) -> bool {
    if !rule.conditions.iter().all(|c| condition_matches(c, ctx)) {
        return false;
    }

    let Some(rollout) = &rule.rollout else { return true };
    let Some(subject) = ctx.bucketing_subject(rollout.bucket_by.as_deref()) else {
        // Without a stable subject membership could not be reproduced, and
        // guessing would move people in and out of the cohort per request.
        // Not a member is the answer that cannot widen an audience by accident.
        return false;
    };

    bucket(salt, &segment_namespace(segment_key), &subject) < rollout.percentage
}

/// Segment rollouts share [`bucket`] with flag rollouts, so they need a
/// namespace no flag key can occupy — otherwise a segment and a flag sharing a
/// key would put every subject in the same place in both. Flag keys are
/// restricted to `[a-zA-Z0-9._-]` by [`crate::validate`], so a colon is enough.
fn segment_namespace(segment_key: &str) -> String {
    format!("segment:{segment_key}")
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

    fn env() -> EvaluationEnv<'static> {
        EvaluationEnv::new("salt")
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
            segments: None,
            distribution: Distribution::fixed("on"),
        };
        assert!(rule_matches(&rule, &ctx(), &env()));

        let too_young = EvaluationContext::new("u").with("plan", "pro").with("age", 12i64);
        assert!(!rule_matches(&rule, &too_young, &env()));
    }

    #[test]
    fn a_rule_without_conditions_targets_everyone() {
        let rule = Rule {
            id: Uuid::nil(),
            description: None,
            conditions: vec![],
            segments: None,
            distribution: Distribution::fixed("on"),
        };
        assert!(rule_matches(&rule, &EvaluationContext::new("anyone"), &env()));
    }

    #[test]
    fn conditions_with_no_values_never_match() {
        assert!(!condition_matches(&cond("plan", Operator::In, vec![]), &ctx()));
    }

    // ------------------------------------------------------------ segments --

    use crate::flag::TOTAL_WEIGHT;
    use crate::segment::{SegmentRollout, SegmentSet};
    use std::collections::BTreeSet;

    fn set(segments: Vec<Segment>) -> SegmentSet {
        segments.into_iter().map(|s| (s.key.clone(), s)).collect()
    }

    /// Members are exactly the `pro` plan.
    fn pro_segment(key: &str) -> Segment {
        Segment::new(key).with_rules(vec![SegmentRule::new(
            Uuid::nil(),
            vec![cond("plan", Operator::In, vec!["pro".into()])],
        )])
    }

    #[test]
    fn segment_rules_are_alternatives_unlike_a_rules_conditions() {
        let segment = Segment::new("reachable").with_rules(vec![
            SegmentRule::new(
                Uuid::from_u128(1),
                vec![cond("plan", Operator::In, vec!["pro".into()])],
            ),
            SegmentRule::new(
                Uuid::from_u128(2),
                vec![cond("beta", Operator::In, vec![true.into()])],
            ),
        ]);

        // Neither condition holds on its own here, but the second rule does.
        let beta_only = EvaluationContext::new("u").with("plan", "free").with("beta", true);
        assert!(segment_contains(&segment, &beta_only, "salt"));

        let neither = EvaluationContext::new("u").with("plan", "free").with("beta", false);
        assert!(!segment_contains(&segment, &neither, "salt"));
    }

    #[test]
    fn exclusion_beats_inclusion_and_both_beat_the_rules() {
        let segment = Segment {
            included: BTreeSet::from(["invited".to_owned(), "banned".to_owned()]),
            excluded: BTreeSet::from(["banned".to_owned()]),
            ..pro_segment("beta")
        };

        let free = |key: &str| EvaluationContext::new(key).with("plan", "free");

        // In `included` and matching no rule: a member anyway.
        assert!(segment_contains(&segment, &free("invited"), "salt"));
        // In both lists: the exclusion wins.
        assert!(!segment_contains(&segment, &free("banned"), "salt"));
        // Matching the rule but excluded: still out.
        let banned_pro = EvaluationContext::new("banned").with("plan", "pro");
        assert!(!segment_contains(&segment, &banned_pro, "salt"));
    }

    #[test]
    fn any_of_is_an_or_and_none_of_a_nor() {
        let segments = set(vec![
            pro_segment("a"),
            Segment { included: BTreeSet::from(["u".to_owned()]), ..Segment::new("b") },
        ]);
        let env = EvaluationEnv::with_segments("salt", &segments);

        let pro = EvaluationContext::new("someone").with("plan", "pro");
        let in_b = EvaluationContext::new("u").with("plan", "free");
        let neither = EvaluationContext::new("someone").with("plan", "free");

        let either = SegmentMatch::any_of(["a", "b"]);
        assert!(segment_match_holds(&either, &pro, &env));
        assert!(segment_match_holds(&either, &in_b, &env));
        assert!(!segment_match_holds(&either, &neither, &env));

        // `none_of` alone is a useful requirement: everyone who is not in `a`.
        let outsiders = SegmentMatch::none_of(["a"]);
        assert!(!segment_match_holds(&outsiders, &pro, &env));
        assert!(segment_match_holds(&outsiders, &neither, &env));

        // And the two sides are ANDed.
        let in_b_but_not_a = SegmentMatch { any_of: vec!["b".into()], none_of: vec!["a".into()] };
        assert!(segment_match_holds(&in_b_but_not_a, &in_b, &env));

        // Key `u` puts this context in `b`, `plan: pro` also in `a` — so the
        // exclusion has to veto it.
        let in_both = EvaluationContext::new("u").with("plan", "pro");
        assert!(!segment_match_holds(&in_b_but_not_a, &in_both, &env));
    }

    /// Validation refuses to store a dangling reference, so this only happens
    /// while a snapshot lags a deletion — and then it must fail closed.
    #[test]
    fn an_unknown_segment_contains_nobody() {
        let env = EvaluationEnv::new("salt");
        let ctx = EvaluationContext::new("u").with("plan", "pro");

        assert!(!in_segment("ghost", &ctx, &env));
        // So requiring it never matches …
        assert!(!segment_match_holds(&SegmentMatch::any_of(["ghost"]), &ctx, &env));
        // … and forbidding it never excludes.
        assert!(segment_match_holds(&SegmentMatch::none_of(["ghost"]), &ctx, &env));
    }

    #[test]
    fn a_rules_conditions_and_its_segment_are_both_required() {
        let segments = set(vec![pro_segment("beta")]);
        let env = EvaluationEnv::with_segments("salt", &segments);
        let rule = Rule::new(
            Uuid::nil(),
            vec![cond("country", Operator::In, vec!["ES".into()])],
            Distribution::fixed("on"),
        )
        .targeting(SegmentMatch::any_of(["beta"]));

        let both = EvaluationContext::new("u").with("plan", "pro").with("country", "ES");
        assert!(rule_matches(&rule, &both, &env));

        let wrong_country = EvaluationContext::new("u").with("plan", "pro").with("country", "US");
        assert!(!rule_matches(&rule, &wrong_country, &env));

        let not_a_member = EvaluationContext::new("u").with("plan", "free").with("country", "ES");
        assert!(!rule_matches(&rule, &not_a_member, &env));
    }

    #[test]
    fn a_segment_rollout_admits_roughly_its_share() {
        let segment = Segment::new("canary").with_rules(vec![SegmentRule {
            rollout: Some(SegmentRollout { percentage: 20_000, bucket_by: None }),
            ..SegmentRule::new(Uuid::nil(), vec![])
        }]);

        let members = (0..10_000)
            .filter(|i| {
                segment_contains(&segment, &EvaluationContext::new(format!("u-{i}")), "salt")
            })
            .count();
        assert!((1_800..2_200).contains(&members), "expected ~2000 members, got {members}");
    }

    /// The point of a cohort: whoever is in it is in it for every flag that
    /// asks. Bucketing on the segment key rather than the flag's is what buys
    /// that, so it is worth pinning down.
    #[test]
    fn segment_membership_does_not_depend_on_which_flag_asks() {
        let segments = set(vec![Segment::new("canary").with_rules(vec![SegmentRule {
            rollout: Some(SegmentRollout { percentage: 50_000, bucket_by: None }),
            ..SegmentRule::new(Uuid::nil(), vec![])
        }])]);
        let env = EvaluationEnv::with_segments("salt", &segments);

        let gated = |key: &str| {
            Rule::new(Uuid::nil(), vec![], Distribution::fixed("on"))
                .targeting(SegmentMatch::any_of([key]))
        };

        for i in 0..200 {
            let ctx = EvaluationContext::new(format!("u-{i}"));
            // Same segment, asked via two different rules: the same answer.
            assert_eq!(
                rule_matches(&gated("canary"), &ctx, &env),
                in_segment("canary", &ctx, &env)
            );
        }
    }

    #[test]
    fn a_segment_rollout_without_its_attribute_admits_nobody() {
        let segment = Segment::new("by-account").with_rules(vec![SegmentRule {
            rollout: Some(SegmentRollout {
                percentage: TOTAL_WEIGHT,
                bucket_by: Some("account_id".into()),
            }),
            ..SegmentRule::new(Uuid::nil(), vec![])
        }]);

        let with_account = EvaluationContext::new("u").with("account_id", "acme");
        assert!(segment_contains(&segment, &with_account, "salt"));

        // A context missing the attribute cannot be placed, and widening the
        // audience on a guess is the failure that matters.
        assert!(!segment_contains(&segment, &EvaluationContext::new("u"), "salt"));
    }

    #[test]
    fn a_segment_and_a_flag_sharing_a_key_bucket_independently() {
        let segments = set(vec![Segment::new("checkout").with_rules(vec![SegmentRule {
            rollout: Some(SegmentRollout { percentage: 50_000, bucket_by: None }),
            ..SegmentRule::new(Uuid::nil(), vec![])
        }])]);

        let differing = (0..500)
            .filter(|i| {
                let subject = format!("u-{i}");
                let ctx = EvaluationContext::new(&subject);
                let in_cohort = segment_contains(&segments["checkout"], &ctx, "salt");
                let in_flag_half = bucket("salt", "checkout", &subject) < 50_000;
                in_cohort != in_flag_half
            })
            .count();

        assert!(differing > 200, "segment and flag bucketing are aliased: {differing}/500");
    }
}
