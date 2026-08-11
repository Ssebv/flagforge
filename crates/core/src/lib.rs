//! # flagforge-core
//!
//! The feature-flag domain: the configuration model, the targeting matcher and
//! the evaluation engine.
//!
//! This crate has no async runtime, no database and no HTTP. That is the point
//! — evaluation is the part that has to be *correct*, and keeping it a pure
//! function of `(flag, context, environment)` means it can be tested
//! exhaustively and reasoned about without spinning anything up.
//!
//! ```
//! use flagforge_core::{
//!     Distribution, EvaluationContext, EvaluationEnv, Flag, WeightedVariant, evaluate,
//! };
//!
//! let flag = Flag {
//!     fallthrough: Distribution::Rollout {
//!         weights: vec![
//!             WeightedVariant { variant: "on".into(), weight: 25_000 },
//!             WeightedVariant { variant: "off".into(), weight: 75_000 },
//!         ],
//!         bucket_by: None,
//!     },
//!     ..Flag::boolean("checkout.v2").enabled(true)
//! };
//!
//! let ctx = EvaluationContext::new("user-42").with("plan", "pro");
//! let env = EvaluationEnv::new("production-salt");
//! let decision = evaluate(&flag, &ctx, &env);
//!
//! // Same input, same answer — every time, on every node.
//! assert_eq!(decision, evaluate(&flag, &ctx, &env));
//! ```

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod bucket;
pub mod context;
pub mod engine;
pub mod experiment;
pub mod flag;
pub mod matcher;
pub mod segment;
pub mod snapshot;
pub mod validate;
pub mod value;

pub use bucket::{bucket, pick_weighted};
pub use context::EvaluationContext;
pub use engine::{Evaluation, EvaluationEnv, Reason, evaluate};
pub use experiment::{
    Comparison, ConfidenceInterval, ExperimentSpec, VariantCounts, VariantResult, results,
};
pub use flag::{
    Condition, Distribution, Flag, Operator, Rule, TOTAL_WEIGHT, Variant, WeightedVariant,
};
pub use matcher::{condition_matches, in_segment, rule_matches, segment_contains};
pub use segment::{Segment, SegmentMatch, SegmentRollout, SegmentRule, SegmentSet};
pub use snapshot::EnvironmentSnapshot;
pub use validate::{ValidationIssue, validate, validate_references, validate_segment};
pub use value::{AttributeValue, VariantValue};
