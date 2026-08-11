//! Experiments: measuring what a flag's variants do to a metric.
//!
//! An experiment binds a flag to a conversion metric. Assignment reuses the
//! engine's deterministic bucketing — the same hash that makes a rollout sticky
//! makes an experiment's cohorts stable — so this module only has to describe
//! experiments and judge their numbers. Both halves are pure: the spec is data,
//! and the statistics are functions from counts to conclusions, testable
//! without a database or a clock.

use serde::{Deserialize, Serialize};

/// Confidence level used for intervals and the significance threshold, as the
/// conventional two-sided 95 %.
pub const ALPHA: f64 = 0.05;

/// z-score for a two-sided 95 % interval.
const Z_95: f64 = 1.959_963_984_540_054;

/// The part of an experiment an SDK needs: enough to attribute events, nothing
/// it could misuse. Travels in the environment snapshot while the experiment is
/// running.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ExperimentSpec {
    pub key: String,
    /// The flag whose variants are being compared.
    pub flag_key: String,
    /// Conversion events with this metric key count toward the experiment.
    pub metric_key: String,
    /// The baseline variant every other variant is compared against.
    pub control_variant: String,
    /// Monotonic counter bumped on every write, folded into the snapshot
    /// version so starting or stopping an experiment is a visible change.
    #[serde(default)]
    pub version: i64,
}

/// Raw tallies for one variant, as stored: exposures are evaluations of the
/// flag while the experiment ran, conversions are tracked metric events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct VariantCounts {
    pub variant: String,
    pub exposures: u64,
    pub conversions: u64,
}

/// A 95 % Wilson score interval around a conversion rate.
///
/// Wilson rather than the normal approximation because experiments spend their
/// early life exactly where the normal interval misbehaves: small samples and
/// rates near zero, where it happily reports negative lower bounds.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ConfidenceInterval {
    pub low: f64,
    pub high: f64,
}

/// A variant measured against the control.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Comparison {
    /// Absolute difference in conversion rate, variant minus control.
    pub lift: f64,
    /// Two-proportion z statistic, pooled variance.
    pub z: f64,
    /// Two-sided p-value for "the rates are equal".
    pub p_value: f64,
    /// Whether `p_value` clears [`ALPHA`]. Stored rather than recomputed so a
    /// reader of the wire format sees the verdict next to the evidence.
    pub significant: bool,
}

/// Everything the dashboard shows about one variant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct VariantResult {
    pub variant: String,
    pub exposures: u64,
    pub conversions: u64,
    /// Conversion rate, absent until the variant has an exposure. Conversions
    /// beyond the exposure count — possible when a metric is tracked for a
    /// context that never evaluated the flag — are clamped here but reported
    /// raw above, so the mismatch is visible instead of laundered.
    pub rate: Option<f64>,
    pub interval: Option<ConfidenceInterval>,
    /// Absent for the control itself and while either side has no exposures.
    pub vs_control: Option<Comparison>,
}

/// Judges every variant's counts against the control's.
///
/// Variants come back in input order. A control that is missing from `counts`
/// (or has no exposures yet) yields results with every comparison absent —
/// the numbers still render, the verdicts wait for data.
pub fn results(control_variant: &str, counts: &[VariantCounts]) -> Vec<VariantResult> {
    let control = counts.iter().find(|c| c.variant == control_variant).and_then(observed);

    counts
        .iter()
        .map(|c| {
            let own = observed(c);
            let vs_control = match (own, control) {
                // Comparing the control to itself would report certainty about
                // nothing, so it gets no comparison rather than a perfect one.
                _ if c.variant == control_variant => None,
                (Some(own), Some(control)) => two_proportion_z(own, control),
                _ => None,
            };

            VariantResult {
                variant: c.variant.clone(),
                exposures: c.exposures,
                conversions: c.conversions,
                rate: own.map(|(x, n)| x / n),
                interval: own.map(|(x, n)| wilson(x / n, n)),
                vs_control,
            }
        })
        .collect()
}

/// Clamped (conversions, exposures) as floats, or `None` without exposures.
fn observed(c: &VariantCounts) -> Option<(f64, f64)> {
    (c.exposures > 0).then(|| (c.conversions.min(c.exposures) as f64, c.exposures as f64))
}

/// 95 % Wilson score interval for `rate` observed over `n` trials.
fn wilson(rate: f64, n: f64) -> ConfidenceInterval {
    let z2 = Z_95 * Z_95;
    let denominator = 1.0 + z2 / n;
    let center = (rate + z2 / (2.0 * n)) / denominator;
    let half_width = (Z_95 / denominator) * (rate * (1.0 - rate) / n + z2 / (4.0 * n * n)).sqrt();

    // The clamps also absorb float rounding at the boundaries: at an observed
    // rate of exactly 1 the algebra gives `high = 1` but the arithmetic can
    // land one ulp below, and an interval excluding its own point estimate is
    // a downstream rendering bug waiting to happen.
    ConfidenceInterval {
        low: (center - half_width).clamp(0.0, rate),
        high: (center + half_width).clamp(rate, 1.0),
    }
}

/// Pooled two-proportion z-test of `(x, n)` against the control's `(x, n)`.
///
/// Returns `None` when the pooled rate is 0 or 1 — the standard error is zero
/// there, and with no observed variance the test has nothing to say.
fn two_proportion_z(own: (f64, f64), control: (f64, f64)) -> Option<Comparison> {
    let (x1, n1) = own;
    let (x2, n2) = control;

    let pooled = (x1 + x2) / (n1 + n2);
    let variance = pooled * (1.0 - pooled) * (1.0 / n1 + 1.0 / n2);
    if variance <= 0.0 {
        return None;
    }

    let lift = x1 / n1 - x2 / n2;
    let z = lift / variance.sqrt();
    let p_value = 2.0 * (1.0 - phi(z.abs()));

    Some(Comparison { lift, z, p_value, significant: p_value < ALPHA })
}

/// Standard normal CDF.
fn phi(x: f64) -> f64 {
    0.5 * (1.0 + erf(x / std::f64::consts::SQRT_2))
}

/// Error function via Abramowitz & Stegun 7.1.26.
///
/// Hand-rolled on purpose: the alternative is a statistics crate an order of
/// magnitude larger than this module, and the approximation's maximum absolute
/// error (1.5 × 10⁻⁷) is noise four decimal places below any p-value cutoff a
/// dashboard reader would act on.
fn erf(x: f64) -> f64 {
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs();

    let t = 1.0 / (1.0 + 0.327_591_1 * x);
    let polynomial = t
        * (0.254_829_592
            + t * (-0.284_496_736
                + t * (1.421_413_741 + t * (-1.453_152_027 + t * 1.061_405_429))));

    sign * (1.0 - polynomial * (-x * x).exp())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(variant: &str, exposures: u64, conversions: u64) -> VariantCounts {
        VariantCounts { variant: variant.into(), exposures, conversions }
    }

    #[test]
    fn the_spec_wire_format_is_stable() {
        let spec = ExperimentSpec {
            key: "checkout-cta".into(),
            flag_key: "checkout.v2".into(),
            metric_key: "checkout.completed".into(),
            control_variant: "off".into(),
            version: 3,
        };
        let json = serde_json::to_string(&spec).unwrap();
        assert_eq!(
            json,
            r#"{"key":"checkout-cta","flag_key":"checkout.v2","metric_key":"checkout.completed","control_variant":"off","version":3}"#
        );
        // And a payload from before `version` existed still deserializes.
        let old = r#"{"key":"k","flag_key":"f","metric_key":"m","control_variant":"off"}"#;
        assert_eq!(serde_json::from_str::<ExperimentSpec>(old).unwrap().version, 0);
    }

    #[test]
    fn erf_matches_published_values() {
        // Abramowitz & Stegun table values; the approximation is good to 1.5e-7.
        for (x, expected) in [(0.5, 0.520_499_878), (1.0, 0.842_700_793), (2.0, 0.995_322_265)] {
            assert!((erf(x) - expected).abs() < 1e-6, "erf({x}) = {}", erf(x));
            assert!((erf(-x) + expected).abs() < 1e-6, "erf must be odd");
        }
    }

    #[test]
    fn a_real_difference_is_called_significant() {
        // 10 % vs 15 % over a thousand exposures each: z ≈ 3.38, p ≈ 0.0007.
        let out = results("control", &[counts("control", 1000, 100), counts("treat", 1000, 150)]);

        let treat = &out[1];
        let cmp = treat.vs_control.unwrap();
        assert!((cmp.lift - 0.05).abs() < 1e-12);
        assert!((3.3..3.5).contains(&cmp.z), "z = {}", cmp.z);
        assert!(cmp.p_value < 0.001);
        assert!(cmp.significant);
    }

    #[test]
    fn identical_rates_are_not_significant() {
        let out = results("a", &[counts("a", 500, 50), counts("b", 500, 50)]);
        let cmp = out[1].vs_control.unwrap();
        assert_eq!(cmp.lift, 0.0);
        // Tolerance bounded by the erf approximation's error, not by ulps.
        assert!((cmp.p_value - 1.0).abs() < 1e-6);
        assert!(!cmp.significant);
    }

    #[test]
    fn the_comparison_is_antisymmetric() {
        let a = counts("a", 800, 120);
        let b = counts("b", 900, 90);
        let ab = results("b", &[a.clone(), b.clone()])[0].vs_control.unwrap();
        let ba = results("a", &[a, b])[1].vs_control.unwrap();

        assert!((ab.z + ba.z).abs() < 1e-12);
        assert!((ab.p_value - ba.p_value).abs() < 1e-12);
    }

    #[test]
    fn the_control_gets_numbers_but_no_verdict_about_itself() {
        let out = results("control", &[counts("control", 100, 30)]);
        assert_eq!(out[0].rate, Some(0.3));
        assert!(out[0].interval.is_some());
        assert!(out[0].vs_control.is_none());
    }

    #[test]
    fn without_exposures_everything_waits_for_data() {
        let out = results("control", &[counts("control", 0, 0), counts("treat", 50, 5)]);

        assert_eq!(out[0].rate, None);
        assert_eq!(out[0].interval, None);
        // The treatment has data, but there is nothing to compare against.
        assert_eq!(out[1].rate, Some(0.1));
        assert!(out[1].vs_control.is_none());
    }

    #[test]
    fn a_missing_control_disables_comparisons_not_results() {
        let out = results("ghost", &[counts("treat", 50, 5)]);
        assert_eq!(out[0].rate, Some(0.1));
        assert!(out[0].vs_control.is_none());
    }

    #[test]
    fn excess_conversions_are_reported_raw_but_clamped_in_the_rate() {
        // Trackable without evaluating: a conversion can outnumber exposures.
        let out = results("c", &[counts("c", 10, 25)]);
        assert_eq!(out[0].conversions, 25, "the raw count must stay visible");
        assert_eq!(out[0].rate, Some(1.0));
        let interval = out[0].interval.unwrap();
        assert!(interval.high <= 1.0);
    }

    #[test]
    fn wilson_intervals_stay_inside_the_unit_range_and_cover_the_rate() {
        for (n, x) in [(1_u64, 0_u64), (1, 1), (10, 0), (10, 10), (20, 1), (1000, 500)] {
            let out = results("only", &[counts("only", n, x)]);
            let rate = out[0].rate.unwrap();
            let interval = out[0].interval.unwrap();
            assert!(
                (0.0..=1.0).contains(&interval.low) && (0.0..=1.0).contains(&interval.high),
                "n={n} x={x}: {interval:?}"
            );
            assert!(
                interval.low <= rate && rate <= interval.high,
                "n={n} x={x}: {rate} outside {interval:?}"
            );
        }
    }

    #[test]
    fn all_or_nothing_pooled_rates_yield_no_verdict() {
        // Nobody converted anywhere: zero variance, so no test.
        let out = results("a", &[counts("a", 100, 0), counts("b", 100, 0)]);
        assert!(out[1].vs_control.is_none());
        // Everybody converted everywhere: same.
        let out = results("a", &[counts("a", 100, 100), counts("b", 100, 100)]);
        assert!(out[1].vs_control.is_none());
    }
}
