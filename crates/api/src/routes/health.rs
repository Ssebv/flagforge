//! Liveness, readiness and metrics.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use crate::state::AppState;

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct Health {
    pub status: &'static str,
    pub version: &'static str,
}

/// Liveness: is the process running?
///
/// Deliberately checks nothing else. A liveness probe that fails when the
/// database is down gets the orchestrator to restart every replica during an
/// outage, which never helps and usually hurts.
#[utoipa::path(
    get,
    path = "/health",
    tag = "health",
    responses((status = 200, description = "The process is alive", body = Health))
)]
pub async fn live() -> Json<Health> {
    Json(Health { status: "ok", version: env!("CARGO_PKG_VERSION") })
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct Readiness {
    pub status: &'static str,
    pub database: &'static str,
    /// Environments currently held in memory.
    pub cached_environments: usize,
}

/// Readiness: should this replica receive traffic?
#[utoipa::path(
    get,
    path = "/health/ready",
    tag = "health",
    responses(
        (status = 200, description = "Ready to serve", body = Readiness),
        (status = 503, description = "The database is unreachable", body = Readiness),
    )
)]
pub async fn ready(State(state): State<AppState>) -> Response {
    let database_ok = flagforge_storage::pool::ping(&state.pool).await;

    let body = Readiness {
        status: if database_ok { "ready" } else { "degraded" },
        database: if database_ok { "up" } else { "down" },
        cached_environments: state.cache.len(),
    };

    let status = if database_ok { StatusCode::OK } else { StatusCode::SERVICE_UNAVAILABLE };

    (status, Json(body)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn liveness_reports_the_crate_version() {
        let Json(health) = live().await;
        assert_eq!(health.status, "ok");
        assert_eq!(health.version, env!("CARGO_PKG_VERSION"));
    }
}
