//! Measures the evaluation engine.
//!
//! Not a criterion suite — the point is a defensible number for the README and
//! a way to notice a regression, not a statistical study. Run it in release;
//! a debug build measures the optimiser being switched off.
//!
//! ```console
//! $ cargo run --release --example bench -p flagforge-core
//! ```

use std::hint::black_box;
use std::time::Instant;

use flagforge_core::{
    AttributeValue, Condition, Distribution, EvaluationContext, EvaluationEnv, Flag, Operator,
    Rule, Segment, SegmentMatch, SegmentRule, SegmentSet, TOTAL_WEIGHT, Variant, WeightedVariant,
    evaluate,
};
use uuid::Uuid;

const SALT: &str = "benchmark-salt";
const ITERATIONS: u32 = 200_000;

fn main() {
    let contexts: Vec<EvaluationContext> = (0..1_000)
        .map(|i| {
            EvaluationContext::new(format!("user-{i}"))
                .with("plan", if i % 3 == 0 { "pro" } else { "free" })
                .with("country", if i % 2 == 0 { "ES" } else { "US" })
                .with("seats", i64::from(i % 50))
                .with("app_version", "2.4.1")
        })
        .collect();

    let empty = SegmentSet::new();
    let plain = EvaluationEnv::with_segments(SALT, &empty);

    for (label, flag) in [
        ("off (kill switch)", off_flag()),
        ("fallthrough, no rules", simple_flag()),
        ("percentage rollout", rollout_flag()),
        ("5 rules, last one matches", rules_flag(5)),
        ("20 rules, none match", rules_flag(20)),
    ] {
        report(label, &flag, &contexts, &plain);
    }

    // Segments cost an extra lookup and a second condition pass, so they get
    // their own line rather than hiding inside the numbers above.
    let segments = segment_set();
    let with_segments = EvaluationEnv::with_segments(SALT, &segments);
    report("rule behind a segment", &segment_flag(), &contexts, &with_segments);
    report("segment with a rollout", &cohort_flag(), &contexts, &with_segments);
}

fn report(label: &str, flag: &Flag, contexts: &[EvaluationContext], env: &EvaluationEnv<'_>) {
    // Warm the caches and the regex compilation so the first iteration is not
    // charged for everyone else's setup.
    for context in contexts.iter().take(100) {
        black_box(evaluate(flag, context, env));
    }

    let started = Instant::now();
    for i in 0..ITERATIONS {
        let context = &contexts[(i as usize) % contexts.len()];
        black_box(evaluate(black_box(flag), black_box(context), black_box(env)));
    }
    let elapsed = started.elapsed();

    let per_call = elapsed.as_secs_f64() / f64::from(ITERATIONS);
    println!("{label:<28} {:>8.0} ns/eval   {:>10.0} evals/sec", per_call * 1e9, 1.0 / per_call);
}

fn off_flag() -> Flag {
    Flag::boolean("bench.off")
}

fn simple_flag() -> Flag {
    Flag::boolean("bench.simple").enabled(true)
}

fn rollout_flag() -> Flag {
    Flag {
        fallthrough: Distribution::Rollout {
            weights: vec![
                WeightedVariant { variant: "on".into(), weight: 25_000 },
                WeightedVariant { variant: "off".into(), weight: TOTAL_WEIGHT - 25_000 },
            ],
            bucket_by: None,
        },
        ..Flag::boolean("bench.rollout").enabled(true)
    }
}

/// `count` rules; only the last one can match, so every earlier rule is
/// evaluated in full. This is the worst realistic case.
fn rules_flag(count: usize) -> Flag {
    let mut rules: Vec<Rule> = (0..count.saturating_sub(1))
        .map(|i| {
            Rule::new(
                Uuid::from_u128(i as u128),
                vec![
                    Condition::new(
                        "country",
                        Operator::In,
                        vec![AttributeValue::String(format!("XX{i}"))],
                    ),
                    Condition::new(
                        "seats",
                        Operator::GreaterThan,
                        vec![AttributeValue::Number(1e9)],
                    ),
                ],
                Distribution::fixed("on"),
            )
        })
        .collect();

    rules.push(Rule::new(
        Uuid::from_u128(u128::MAX),
        vec![
            Condition::new("plan", Operator::In, vec![AttributeValue::String("pro".into())]),
            Condition::new(
                "app_version",
                Operator::SemverGreaterThan,
                vec![AttributeValue::String("2.0.0".into())],
            ),
            Condition::new(
                "country",
                Operator::Matches,
                vec![AttributeValue::String("^(ES|US|FR)$".into())],
            ),
        ],
        Distribution::fixed("on"),
    ));

    Flag {
        variants: vec![Variant::new("on", true), Variant::new("off", false)],
        ..Flag::boolean("bench.rules").enabled(true)
    }
    .with_rules(rules)
}

/// The audiences the segment benchmarks resolve against.
fn segment_set() -> SegmentSet {
    [
        Segment::new("pro-in-europe").with_rules(vec![SegmentRule::new(
            Uuid::from_u128(1),
            vec![
                Condition::new("plan", Operator::In, vec![AttributeValue::String("pro".into())]),
                Condition::new("country", Operator::In, vec![AttributeValue::String("ES".into())]),
            ],
        )]),
        Segment::new("canary").with_rules(vec![SegmentRule {
            rollout: Some(flagforge_core::SegmentRollout { percentage: 10_000, bucket_by: None }),
            ..SegmentRule::new(Uuid::from_u128(2), vec![])
        }]),
    ]
    .into_iter()
    .map(|s| (s.key.clone(), s))
    .collect()
}

/// One rule, gated on a segment whose membership needs two conditions.
fn segment_flag() -> Flag {
    Flag::boolean("bench.segment").enabled(true).with_rules(vec![
        Rule::new(Uuid::from_u128(1), vec![], Distribution::fixed("on"))
            .targeting(SegmentMatch::any_of(["pro-in-europe"])),
    ])
}

/// A segment whose membership is itself a percentage — the extra hash.
fn cohort_flag() -> Flag {
    Flag::boolean("bench.cohort").enabled(true).with_rules(vec![
        Rule::new(Uuid::from_u128(1), vec![], Distribution::fixed("on"))
            .targeting(SegmentMatch::any_of(["canary"])),
    ])
}
