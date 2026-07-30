//! Logging and metrics setup.

use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use crate::config::RuntimeEnvironment;

/// Initialises the global subscriber.
///
/// JSON in production because logs are read by machines there, and a
/// human-readable format locally because they are read by a person. `RUST_LOG`
/// overrides the default filter either way.
pub fn init_tracing(environment: RuntimeEnvironment) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("flagforge_api=info,flagforge_storage=info,tower_http=info,warn")
    });

    let registry = tracing_subscriber::registry().with(filter);

    if environment.is_production() {
        registry.with(tracing_subscriber::fmt::layer().json().flatten_event(true)).init();
    } else {
        registry.with(tracing_subscriber::fmt::layer().compact()).init();
    }
}

/// Installs the Prometheus recorder and returns the handle `/metrics` renders.
pub fn init_metrics() -> Result<PrometheusHandle, anyhow::Error> {
    let handle = PrometheusBuilder::new()
        // Latency buckets chosen around what this service should actually do:
        // a cached evaluation is sub-millisecond, and anything past a second
        // is already a problem worth seeing separately.
        .set_buckets(&[
            0.000_5, 0.001, 0.002_5, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0,
        ])?
        .install_recorder()?;

    Ok(handle)
}

/// A recorder that is built but *not* installed globally.
///
/// Integration tests construct a full application per test; installing a
/// global recorder more than once fails, and sharing one across tests would
/// make their counters interfere.
pub fn detached_metrics() -> PrometheusHandle {
    PrometheusBuilder::new().build_recorder().handle()
}

/// Records one served request.
pub fn record_request(method: &str, path: &str, status: u16, latency_secs: f64) {
    metrics::counter!(
        "flagforge_http_requests_total",
        "method" => method.to_owned(),
        "path" => path.to_owned(),
        "status" => status.to_string(),
    )
    .increment(1);

    metrics::histogram!(
        "flagforge_http_request_duration_seconds",
        "method" => method.to_owned(),
        "path" => path.to_owned(),
    )
    .record(latency_secs);
}
