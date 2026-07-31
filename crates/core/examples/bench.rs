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
    AttributeValue, Condition, Distribution, EvaluationContext, Flag, Operator, Rule, TOTAL_WEIGHT,
    Variant, WeightedVariant, evaluate,
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

    for (label, flag) in [
        ("off (kill switch)", off_flag()),
        ("fallthrough, no rules", simple_flag()),
        ("percentage rollout", rollout_flag()),
        ("5 rules, last one matches", rules_flag(5)),
        ("20 rules, none match", rules_flag(20)),
    ] {
        report(label, &flag, &contexts);
    }
}

fn report(label: &str, flag: &Flag, contexts: &[EvaluationContext]) {
    // Warm the caches and the regex compilation so the first iteration is not
    // charged for everyone else's setup.
    for context in contexts.iter().take(100) {
        black_box(evaluate(flag, context, SALT));
    }

    let started = Instant::now();
    for i in 0..ITERATIONS {
        let context = &contexts[(i as usize) % contexts.len()];
        black_box(evaluate(black_box(flag), black_box(context), SALT));
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
        .map(|i| Rule {
            id: Uuid::from_u128(i as u128),
            description: None,
            conditions: vec![
                Condition::new(
                    "country",
                    Operator::In,
                    vec![AttributeValue::String(format!("XX{i}"))],
                ),
                Condition::new("seats", Operator::GreaterThan, vec![AttributeValue::Number(1e9)]),
            ],
            distribution: Distribution::fixed("on"),
        })
        .collect();

    rules.push(Rule {
        id: Uuid::from_u128(u128::MAX),
        description: None,
        conditions: vec![
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
        distribution: Distribution::fixed("on"),
    });

    Flag {
        variants: vec![Variant::new("on", true), Variant::new("off", false)],
        ..Flag::boolean("bench.rules").enabled(true)
    }
    .with_rules(rules)
}
