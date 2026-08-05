//! Test harness: builds the real application over a real database.
//!
//! `#[sqlx::test]` hands each test its own freshly migrated database, so tests
//! run in parallel without sharing state and without any cleanup step. The
//! router under test is the one `main` serves — middleware, extractors and all
//! — driven through `tower::ServiceExt::oneshot` rather than over a socket.

#![allow(dead_code)] // Each test file uses a different subset of the helpers.

use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use flagforge_api::config::{
    AuthConfig, CacheConfig, Config, DatabaseConfig, RateLimitConfig, RuntimeEnvironment,
    ServerConfig,
};
use flagforge_api::state::AppState;
use flagforge_api::{routes, telemetry};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;

pub struct TestApp {
    router: Router,
    pub pool: PgPool,
}

/// A response, decomposed into the two things assertions care about.
pub struct Response {
    pub status: StatusCode,
    pub body: Value,
}

impl Response {
    /// Panics with the body attached, so a failing assertion shows *why* the
    /// server disagreed instead of just which number it returned.
    pub fn expect(self, status: StatusCode) -> Value {
        assert_eq!(self.status, status, "unexpected status; body was {}", self.body);
        self.body
    }

    pub fn error_kind(&self) -> &str {
        self.body.get("type").and_then(Value::as_str).unwrap_or("<no type field>")
    }
}

impl TestApp {
    pub fn new(pool: PgPool) -> Self {
        Self::with_metrics_token(pool, None)
    }

    /// An app whose `/metrics` demands a bearer token.
    pub fn with_metrics_token(pool: PgPool, metrics_token: Option<&str>) -> Self {
        let metrics_token = metrics_token.map(str::to_owned);
        let config = Config {
            server: ServerConfig {
                address: SocketAddr::from(([127, 0, 0, 1], 0)),
                request_timeout: Duration::from_secs(10),
                body_limit_bytes: 256 * 1024,
                shutdown_grace: Duration::from_secs(1),
            },
            database: DatabaseConfig {
                // Only the change listener would use this, and tests do not
                // start one — handlers refresh the cache inline.
                url: String::new(),
                max_connections: 5,
                auto_migrate: false,
            },
            auth: AuthConfig {
                jwt_secret: "test-secret-long-enough-to-pass-validation".into(),
                token_ttl: Duration::from_secs(3600),
                metrics_token,
            },
            cache: CacheConfig { refresh_interval: Duration::from_secs(60) },
            // Generous, so a test that makes many calls is not throttled; the
            // limiter has its own unit tests.
            rate_limit: RateLimitConfig { burst: 100_000, per_second: 100_000 },
            environment: RuntimeEnvironment::Development,
        };

        let state = AppState::new(pool.clone(), config);
        Self { router: routes::router(state, telemetry::detached_metrics()), pool }
    }

    pub async fn get(&self, uri: &str, token: Option<&str>) -> Response {
        self.send("GET", uri, token, None).await
    }

    pub async fn post(&self, uri: &str, token: Option<&str>, body: Value) -> Response {
        self.send("POST", uri, token, Some(body)).await
    }

    pub async fn put(&self, uri: &str, token: Option<&str>, body: Value) -> Response {
        self.send("PUT", uri, token, Some(body)).await
    }

    pub async fn patch(&self, uri: &str, token: Option<&str>, body: Value) -> Response {
        self.send("PATCH", uri, token, Some(body)).await
    }

    pub async fn delete(&self, uri: &str, token: Option<&str>) -> Response {
        self.send("DELETE", uri, token, None).await
    }

    async fn send(
        &self,
        method: &str,
        uri: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> Response {
        let mut request = Request::builder().method(method).uri(uri);

        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }

        let request = match &body {
            Some(json) => request
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(json).unwrap())),
            None => request.body(Body::empty()),
        }
        .unwrap();

        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();

        // 204s and other empty responses become `null` rather than an error.
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| json!({ "<non-json body>": String::from_utf8_lossy(&bytes) }))
        };

        Response { status, body }
    }

    // ---------------------------------------------------------- fixtures --

    /// Registers an organization and returns its owner's token.
    pub async fn register(&self, org: &str, email: &str) -> String {
        let body = self
            .post(
                "/api/v1/auth/register",
                None,
                json!({
                    "organization_name": org,
                    "email": email,
                    "password": "correct-horse-battery-staple",
                }),
            )
            .await
            .expect(StatusCode::CREATED);

        body["token"].as_str().unwrap().to_owned()
    }

    /// Registers an org and gives it a project, an environment and a server
    /// SDK key — the state most tests need before they can say anything
    /// interesting.
    pub async fn bootstrap(&self, org: &str, email: &str) -> Tenant {
        let token = self.register(org, email).await;

        self.post("/api/v1/projects", Some(&token), json!({"key": "checkout", "name": "Checkout"}))
            .await
            .expect(StatusCode::CREATED);

        self.post(
            "/api/v1/projects/checkout/environments",
            Some(&token),
            json!({"key": "production", "name": "Production", "is_production": true}),
        )
        .await
        .expect(StatusCode::CREATED);

        let key = self
            .post(
                "/api/v1/projects/checkout/environments/production/keys",
                Some(&token),
                json!({"name": "backend", "scope": "server"}),
            )
            .await
            .expect(StatusCode::CREATED);

        Tenant {
            token,
            sdk_key: key["secret"].as_str().unwrap().to_owned(),
            sdk_key_id: key["id"].as_str().unwrap().to_owned(),
        }
    }

    /// Creates a boolean flag, disabled everywhere.
    pub async fn create_flag(&self, token: &str, key: &str) -> Value {
        self.post("/api/v1/projects/checkout/flags", Some(token), json!({"key": key, "name": key}))
            .await
            .expect(StatusCode::CREATED)
    }

    /// Writes a flag's production configuration.
    pub async fn configure(&self, token: &str, flag: &str, config: Value) -> Response {
        self.put(
            &format!("/api/v1/projects/checkout/environments/production/flags/{flag}"),
            Some(token),
            config,
        )
        .await
    }

    /// Evaluates one flag as an SDK.
    pub async fn evaluate(&self, sdk_key: &str, flag: &str, context: Value) -> Value {
        self.post(&format!("/api/v1/evaluate/{flag}"), Some(sdk_key), json!({"context": context}))
            .await
            .expect(StatusCode::OK)
    }
}

pub struct Tenant {
    pub token: String,
    pub sdk_key: String,
    pub sdk_key_id: String,
}

/// `{"enabled": true, ...}` with a fixed fallthrough — the common shape.
pub fn simple_config(enabled: bool, variant: &str) -> Value {
    json!({
        "enabled": enabled,
        "off_variant": "off",
        "fallthrough": {"kind": "fixed", "variant": variant},
        "rules": [],
    })
}
