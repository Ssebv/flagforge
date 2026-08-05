//! Route table and the middleware stack around it.

pub mod audit;
pub mod auth;
pub mod evaluate;
pub mod flags;
pub mod health;
pub mod keys;
pub mod projects;
pub mod segments;

use std::sync::Arc;
use std::time::Instant;

use axum::extract::MatchedPath;
use axum::http::{HeaderName, HeaderValue, Request, StatusCode};
use axum::routing::{delete, get, post};
use axum::{Router, middleware};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use metrics_exporter_prometheus::PrometheusHandle;
use rand::Rng;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::compression::CompressionLayer;
use tower_http::cors::{Any, CorsLayer};
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::request_id::{
    MakeRequestId, PropagateRequestIdLayer, RequestId, SetRequestIdLayer,
};
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;
use uuid::Uuid;

use crate::error::ApiError;
use crate::rate_limit::{self, RateLimiter};
use crate::state::AppState;

const REQUEST_ID_HEADER: &str = "x-request-id";

/// Builds the whole application.
pub fn router(state: AppState, metrics: PrometheusHandle) -> Router {
    let limiter = Arc::new(RateLimiter::new(&state.config.rate_limit));

    // Operational endpoints sit outside the rate limiter: a probe that gets
    // throttled reports the service as down, which is the opposite of useful.
    let operational = Router::new()
        .route("/health", get(health::live))
        .route("/health/ready", get(health::ready))
        .route(
            "/metrics",
            get({
                let handle = Arc::new(metrics);
                move || {
                    let handle = Arc::clone(&handle);
                    async move { handle.render() }
                }
            }),
        );

    let public = Router::new()
        .route("/api/v1/auth/register", post(auth::register))
        .route("/api/v1/auth/login", post(auth::login));

    let management = Router::new()
        .route("/api/v1/auth/me", get(auth::me))
        .route("/api/v1/projects", get(projects::list).post(projects::create))
        .route("/api/v1/projects/{project_key}", get(projects::get).delete(projects::delete))
        .route(
            "/api/v1/projects/{project_key}/environments",
            get(projects::list_environments).post(projects::create_environment),
        )
        .route(
            "/api/v1/projects/{project_key}/environments/{environment_key}",
            delete(projects::delete_environment),
        )
        .route("/api/v1/projects/{project_key}/flags", get(flags::list).post(flags::create))
        .route(
            "/api/v1/projects/{project_key}/flags/{flag_key}",
            get(flags::get).patch(flags::update).delete(flags::delete),
        )
        .route(
            "/api/v1/projects/{project_key}/environments/{environment_key}/flags",
            get(flags::list_configured),
        )
        .route(
            "/api/v1/projects/{project_key}/environments/{environment_key}/flags/{flag_key}",
            get(flags::get_config).put(flags::update_config),
        )
        .route(
            "/api/v1/projects/{project_key}/environments/{environment_key}/segments",
            get(segments::list).post(segments::create),
        )
        .route(
            "/api/v1/projects/{project_key}/environments/{environment_key}/segments/{segment_key}",
            get(segments::get).put(segments::update).delete(segments::delete),
        )
        .route(
            "/api/v1/projects/{project_key}/environments/{environment_key}/keys",
            get(keys::list).post(keys::create),
        )
        .route(
            "/api/v1/projects/{project_key}/environments/{environment_key}/keys/{key_id}",
            delete(keys::revoke),
        )
        .route("/api/v1/audit", get(audit::list));

    // SDK routes are called from browsers with client-scoped keys, so they
    // need permissive CORS. Management routes deliberately do not: a token in
    // a browser should only ever be used by our own tooling.
    let sdk = Router::new()
        .route("/api/v1/evaluate", post(evaluate::evaluate_all))
        .route("/api/v1/evaluate/{flag_key}", post(evaluate::evaluate_one))
        .route("/api/v1/snapshot", get(evaluate::snapshot))
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any)
                // No credentials: these routes authenticate by header, never
                // by cookie, so allowing credentials would only widen the
                // surface for nothing.
                .max_age(std::time::Duration::from_secs(600)),
        );

    let limited = public
        .merge(management)
        .merge(sdk)
        .layer(middleware::from_fn_with_state(Arc::clone(&limiter), rate_limit::enforce));

    let mut app = operational
        .merge(limited)
        // Anything the router does not know is either a mistyped API path (a
        // problem document) or a client-side route (the SPA shell).
        .fallback(crate::dashboard::serve)
        .layer(middleware::from_fn(observe))
        .layer(TraceLayer::new_for_http())
        .layer(PropagateRequestIdLayer::new(HeaderName::from_static(REQUEST_ID_HEADER)))
        .layer(SetRequestIdLayer::new(HeaderName::from_static(REQUEST_ID_HEADER), MakeRequestUuid))
        .layer(CompressionLayer::new())
        .layer(TimeoutLayer::with_status_code(
            StatusCode::GATEWAY_TIMEOUT,
            state.config.server.request_timeout,
        ))
        .layer(RequestBodyLimitLayer::new(state.config.server.body_limit_bytes))
        // Outermost, so a panic anywhere inside still becomes a 500 for this
        // one request instead of taking the process with it.
        .layer(CatchPanicLayer::new())
        .with_state(state.clone());

    if !state.config.environment.is_production() {
        app = app.merge(crate::openapi::swagger_ui());
    }

    app
}

/// Times every request and records it under its *matched* route.
///
/// Using the matched path (`/api/v1/projects/{project_key}`) rather than the
/// raw URI is what keeps the metric cardinality bounded — otherwise every
/// project key would create its own time series.
async fn observe(
    request: axum::extract::Request,
    next: middleware::Next,
) -> axum::response::Response {
    let method = request.method().as_str().to_owned();
    let path = request
        .extensions()
        .get::<MatchedPath>()
        .map(|p| p.as_str().to_owned())
        .unwrap_or_else(|| "<unmatched>".to_owned());

    let started = Instant::now();
    let response = next.run(request).await;

    crate::telemetry::record_request(
        &method,
        &path,
        response.status().as_u16(),
        started.elapsed().as_secs_f64(),
    );

    response
}

/// Generates a UUIDv7 request id.
///
/// v7 rather than v4 so ids sort by time, which makes them useful as a
/// secondary ordering key when reading logs.
#[derive(Debug, Clone, Copy)]
struct MakeRequestUuid;

impl MakeRequestId for MakeRequestUuid {
    fn make_request_id<B>(&mut self, _: &Request<B>) -> Option<RequestId> {
        HeaderValue::from_str(&Uuid::now_v7().to_string()).ok().map(RequestId::new)
    }
}

/// Rejects identifiers the database would reject anyway, with a message that
/// names the field instead of a constraint.
pub(crate) fn valid_key(candidate: &str, field: &'static str) -> Result<(), ApiError> {
    let ok = !candidate.is_empty()
        && candidate.len() <= flagforge_core::validate::MAX_KEY_LEN
        && candidate.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));

    if ok {
        Ok(())
    } else {
        Err(ApiError::BadRequest(format!(
            "`{field}` must be 1-{} characters of letters, digits, `.`, `_` or `-`",
            flagforge_core::validate::MAX_KEY_LEN
        )))
    }
}

/// 256 bits of CSPRNG output, used as an environment's bucketing salt.
pub fn new_salt() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn keys_are_validated_the_same_way_the_database_would() {
        assert!(valid_key("checkout.v2", "key").is_ok());
        assert!(valid_key("a-b_c.1", "key").is_ok());

        assert!(valid_key("", "key").is_err());
        assert!(valid_key("has space", "key").is_err());
        assert!(valid_key("slash/es", "key").is_err());
        assert!(valid_key(&"a".repeat(129), "key").is_err());
    }

    #[test]
    fn the_error_names_the_offending_field() {
        let ApiError::BadRequest(message) = valid_key("bad key", "environment_key").unwrap_err()
        else {
            panic!("expected a bad request");
        };
        assert!(message.contains("environment_key"), "{message}");
    }

    #[test]
    fn salts_are_unique_and_url_safe() {
        let salts: HashSet<String> = (0..1_000).map(|_| new_salt()).collect();
        assert_eq!(salts.len(), 1_000);

        let salt = new_salt();
        assert!(salt.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')));
        assert!(salt.len() >= 42, "expected 256 bits of entropy, got `{salt}`");
    }

    #[test]
    fn request_ids_are_time_ordered() {
        let mut maker = MakeRequestUuid;
        let request = Request::builder().body(()).unwrap();

        let first = maker.make_request_id(&request).unwrap();
        let second = maker.make_request_id(&request).unwrap();

        assert!(first.header_value() <= second.header_value());
    }
}
