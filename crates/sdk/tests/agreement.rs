//! The SDK evaluates locally; the server evaluates remotely. They must agree.
//!
//! This is the property the whole design rests on. If local and remote answers
//! can differ, then every "it works on my service" report becomes unfalsifiable
//! — so the test binds a real server, drives a real client over a real socket,
//! and compares the two decisions user by user.

use std::net::SocketAddr;
use std::time::Duration;

use flagforge_api::config::{
    AuthConfig, CacheConfig, Config, DatabaseConfig, RateLimitConfig, RuntimeEnvironment,
    ServerConfig,
};
use flagforge_api::state::AppState;
use flagforge_api::{routes, telemetry};
use flagforge_sdk::{Client, EvaluationContext};
use serde_json::{Value, json};
use sqlx::PgPool;

/// Boots the real application on an ephemeral port.
async fn serve(pool: PgPool) -> SocketAddr {
    let config = Config {
        server: ServerConfig {
            address: SocketAddr::from(([127, 0, 0, 1], 0)),
            request_timeout: Duration::from_secs(10),
            body_limit_bytes: 256 * 1024,
            shutdown_grace: Duration::from_secs(1),
        },
        database: DatabaseConfig {
            url: String::new(),
            max_connections: 5,
            auto_migrate: false,
            startup_timeout: Duration::from_secs(5),
        },
        auth: AuthConfig {
            jwt_secret: "test-secret-long-enough-to-pass-validation".into(),
            token_ttl: Duration::from_secs(3600),
            metrics_token: None,
        },
        cache: CacheConfig { refresh_interval: Duration::from_secs(60) },
        rate_limit: RateLimitConfig { burst: 100_000, per_second: 100_000 },
        environment: RuntimeEnvironment::Development,
    };

    let app = routes::router(AppState::new(pool, config), telemetry::detached_metrics());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    address
}

struct Fixture {
    base: String,
    http: reqwest::Client,
    token: String,
    sdk_key: String,
}

impl Fixture {
    /// Registers an organization and sets up a flag with a rule and a rollout.
    async fn create(address: SocketAddr) -> Self {
        let base = format!("http://{address}");
        let http = reqwest::Client::new();

        let registered: Value = http
            .post(format!("{base}/api/v1/auth/register"))
            .json(&json!({
                "organization_name": "Acme Inc",
                "email": "ada@acme.test",
                "password": "correct-horse-battery-staple",
            }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();

        let mut fixture = Self {
            token: registered["token"].as_str().unwrap().to_owned(),
            sdk_key: String::new(),
            base,
            http,
        };

        fixture.post("/api/v1/projects", json!({"key": "checkout", "name": "Checkout"})).await;
        fixture
            .post(
                "/api/v1/projects/checkout/environments",
                json!({"key": "production", "name": "Production", "is_production": true}),
            )
            .await;
        fixture
            .post(
                "/api/v1/projects/checkout/flags",
                json!({"key": "checkout.v2", "name": "New checkout"}),
            )
            .await;

        // A rule that only some contexts match, plus a rollout for the rest —
        // so the comparison exercises rule matching *and* salt-dependent
        // bucketing, which is the part that can silently diverge.
        fixture
            .put(
                "/api/v1/projects/checkout/environments/production/flags/checkout.v2",
                json!({
                    "enabled": true,
                    "off_variant": "off",
                    "fallthrough": {
                        "kind": "rollout",
                        "weights": [
                            {"variant": "on", "weight": 30_000},
                            {"variant": "off", "weight": 70_000},
                        ],
                    },
                    "rules": [{
                        "id": "11111111-1111-1111-1111-111111111111",
                        "conditions": [
                            {"attribute": "plan", "operator": "in", "values": ["enterprise"]},
                        ],
                        "distribution": {"kind": "fixed", "variant": "on"},
                    }],
                }),
            )
            .await;

        let key: Value = fixture
            .post(
                "/api/v1/projects/checkout/environments/production/keys",
                json!({"name": "sdk-test", "scope": "server"}),
            )
            .await;
        fixture.sdk_key = key["secret"].as_str().unwrap().to_owned();

        fixture
    }

    async fn post(&self, path: &str, body: Value) -> Value {
        let response = self
            .http
            .post(format!("{}{path}", self.base))
            .header("authorization", format!("Bearer {}", self.token))
            .json(&body)
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success(), "POST {path} failed: {}", response.status());
        response.json().await.unwrap()
    }

    async fn put(&self, path: &str, body: Value) {
        let response = self
            .http
            .put(format!("{}{path}", self.base))
            .header("authorization", format!("Bearer {}", self.token))
            .json(&body)
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success(), "PUT {path} failed: {}", response.status());
    }

    async fn get(&self, path: &str) -> Value {
        let response = self
            .http
            .get(format!("{}{path}", self.base))
            .header("authorization", format!("Bearer {}", self.token))
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success(), "GET {path} failed: {}", response.status());
        response.json().await.unwrap()
    }

    /// Asks the server for a decision, the way a thin client would.
    async fn evaluate_remotely(&self, flag: &str, context: &EvaluationContext) -> bool {
        let response: Value = self
            .http
            .post(format!("{}/api/v1/evaluate/{flag}", self.base))
            .header("authorization", format!("Bearer {}", self.sdk_key))
            .json(&json!({"context": context}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();

        response["value"] == Value::Bool(true)
    }
}

fn context(index: u32) -> EvaluationContext {
    EvaluationContext::new(format!("user-{index}"))
        .with("plan", if index % 10 == 0 { "enterprise" } else { "free" })
}

#[sqlx::test(migrations = "../../migrations")]
async fn local_and_remote_evaluation_agree_on_every_user(pool: PgPool) {
    let address = serve(pool).await;
    let fixture = Fixture::create(address).await;

    let client = Client::builder(&fixture.base, &fixture.sdk_key)
        .poll_interval(Duration::ZERO)
        .connect()
        .await
        .expect("the SDK should load a snapshot from a server-scoped key");

    assert!(client.is_ready());
    assert_eq!(client.environment().as_deref(), Some("production"));

    let mut enabled = 0;
    for i in 0..300 {
        let context = context(i);
        let local = client.is_enabled("checkout.v2", &context, false);
        let remote = fixture.evaluate_remotely("checkout.v2", &context).await;

        assert_eq!(
            local, remote,
            "user-{i} got {local} locally and {remote} from the server — the SDK is bucketing \
             against a different salt, or matching rules differently"
        );
        enabled += u32::from(local);
    }

    // Sanity: the comparison would be vacuous if everything landed the same
    // way. 10% match the rule outright, and ~30% of the remaining 90% roll in.
    assert!(
        (60..=130).contains(&enabled),
        "expected a mix of on and off across 300 users, got {enabled} enabled"
    );
}

/// Segments are resolved by the client, from data the client has to have been
/// given. A snapshot shipping the rules but not the segments would make every
/// segment reference match nobody — and report it as an ordinary fallthrough,
/// so nothing would look broken locally while the server said otherwise.
#[sqlx::test(migrations = "../../migrations")]
async fn local_and_remote_agree_on_flags_gated_by_a_segment(pool: PgPool) {
    let address = serve(pool).await;
    let fixture = Fixture::create(address).await;

    fixture
        .post(
            "/api/v1/projects/checkout/environments/production/segments",
            json!({"key": "enterprise-canary", "name": "Enterprise canary"}),
        )
        .await;

    // Membership needs a condition *and* a rollout, so a client that got the
    // segment but bucketed it differently fails just as loudly as one that
    // never got it.
    fixture
        .put(
            "/api/v1/projects/checkout/environments/production/segments/enterprise-canary",
            json!({
                "rules": [{
                    "id": "33333333-3333-3333-3333-333333333333",
                    "conditions": [
                        {"attribute": "plan", "operator": "in", "values": ["enterprise", "free"]},
                    ],
                    "rollout": {"percentage": 40_000},
                }],
            }),
        )
        .await;

    fixture
        .put(
            "/api/v1/projects/checkout/environments/production/flags/checkout.v2",
            json!({
                "enabled": true,
                "off_variant": "off",
                "fallthrough": {"kind": "fixed", "variant": "off"},
                "rules": [{
                    "id": "11111111-1111-1111-1111-111111111111",
                    "conditions": [],
                    "segments": {"any_of": ["enterprise-canary"]},
                    "distribution": {"kind": "fixed", "variant": "on"},
                }],
            }),
        )
        .await;

    let client = Client::builder(&fixture.base, &fixture.sdk_key)
        .poll_interval(Duration::ZERO)
        .connect()
        .await
        .expect("the SDK should load a snapshot from a server-scoped key");

    let mut members = 0;
    for i in 0..200 {
        let context = context(i);
        let local = client.is_enabled("checkout.v2", &context, false);
        let remote = fixture.evaluate_remotely("checkout.v2", &context).await;

        assert_eq!(
            local, remote,
            "user-{i} got {local} locally and {remote} from the server — the snapshot is missing \
             the segment, or the client buckets its cohort differently"
        );
        members += u32::from(local);
    }

    // Roughly the 40 % cohort. Wide bounds, but an empty or universal cohort —
    // the two ways this silently breaks — still fails.
    assert!(
        (50..=130).contains(&members),
        "expected roughly 40% of 200 users in the cohort, got {members}"
    );
}

/// Exposures and conversions are attributed client-side, by the same
/// deterministic evaluation the flag check used. If that attribution drifted
/// from local assignment, an experiment would compare cohorts that do not
/// exist — so the counters the server ends up holding must equal a tally the
/// test keeps by hand.
#[sqlx::test(migrations = "../../migrations")]
async fn recorded_counters_agree_with_local_assignment(pool: PgPool) {
    use std::collections::HashMap;

    let address = serve(pool).await;
    let fixture = Fixture::create(address).await;
    let experiments = "/api/v1/projects/checkout/environments/production/experiments";

    fixture
        .post(
            experiments,
            json!({
                "key": "cta", "name": "Checkout CTA", "flag_key": "checkout.v2",
                "metric_key": "order.completed", "control_variant": "off",
            }),
        )
        .await;
    fixture.post(&format!("{experiments}/cta/start"), json!({})).await;

    // Both background tasks off: the test drives the flush itself.
    let client = Client::builder(&fixture.base, &fixture.sdk_key)
        .poll_interval(Duration::ZERO)
        .event_flush_interval(Duration::ZERO)
        .connect()
        .await
        .unwrap();

    let mut exposures: HashMap<String, u64> = HashMap::new();
    let mut conversions: HashMap<String, u64> = HashMap::new();
    for i in 0..200 {
        let context = context(i);
        let decision =
            client.evaluate("checkout.v2", &context, flagforge_sdk::VariantValue::null());
        let variant = decision.variant.clone().expect("a configured flag resolves a variant");
        *exposures.entry(variant.clone()).or_default() += 1;

        // Every third user converts.
        if i % 3 == 0 {
            assert_eq!(client.track("order.completed", &context), 1);
            *conversions.entry(variant).or_default() += 1;
        }
    }

    // A metric nobody measures is not an error — it counts toward nothing.
    assert_eq!(client.track("unmeasured.metric", &context(0)), 0);

    client.flush_events().await.expect("the flush must reach the server");

    let body = fixture.get(&format!("{experiments}/cta/results")).await;
    let results = body["results"].as_array().expect("results array");
    assert_eq!(results.len(), 2, "{body}");
    for arm in results {
        let variant = arm["variant"].as_str().unwrap();
        assert_eq!(
            arm["exposures"].as_u64().unwrap(),
            exposures.get(variant).copied().unwrap_or(0),
            "exposures for `{variant}` disagree with local assignment: {body}"
        );
        assert_eq!(
            arm["conversions"].as_u64().unwrap(),
            conversions.get(variant).copied().unwrap_or(0),
            "conversions for `{variant}` disagree with local attribution: {body}"
        );
    }

    // Flushing with nothing accumulated is a no-op, not a request.
    client.flush_events().await.unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_sdk_sees_a_change_after_refreshing(pool: PgPool) {
    let address = serve(pool).await;
    let fixture = Fixture::create(address).await;

    let client = Client::builder(&fixture.base, &fixture.sdk_key)
        .poll_interval(Duration::ZERO)
        .connect()
        .await
        .unwrap();

    let ada = EvaluationContext::new("user-0").with("plan", "enterprise");
    assert!(client.is_enabled("checkout.v2", &ada, false));
    let before = client.version().unwrap();

    // Someone turns the flag off in the dashboard.
    fixture
        .put(
            "/api/v1/projects/checkout/environments/production/flags/checkout.v2",
            json!({
                "enabled": false,
                "off_variant": "off",
                "fallthrough": {"kind": "fixed", "variant": "on"},
                "rules": [],
            }),
        )
        .await;

    // Polling is disabled, so the client is still serving what it loaded —
    // deliberately, and this is what "eventually consistent" means here.
    assert!(client.is_enabled("checkout.v2", &ada, false));

    client.refresh().await.unwrap();

    assert!(!client.is_enabled("checkout.v2", &ada, false));
    assert!(client.version().unwrap() > before);
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_client_scoped_key_cannot_drive_local_evaluation(pool: PgPool) {
    let address = serve(pool).await;
    let fixture = Fixture::create(address).await;

    let key: Value = fixture
        .post(
            "/api/v1/projects/checkout/environments/production/keys",
            json!({"name": "browser", "scope": "client"}),
        )
        .await;
    let client_key = key["secret"].as_str().unwrap();

    // Client keys get decisions, never rules — so an SDK that evaluates
    // locally must fail loudly at start-up rather than silently serve
    // fallbacks for the lifetime of the process.
    let error = Client::builder(&fixture.base, client_key)
        .poll_interval(Duration::ZERO)
        .connect()
        .await
        .expect_err("a client-scoped key must not be usable for local evaluation");

    assert!(matches!(error, flagforge_sdk::Error::Unauthorized(_)), "{error:?}");
    assert!(!error.is_transient(), "retrying a scope mistake will never help");
}

#[sqlx::test(migrations = "../../migrations")]
async fn an_unknown_flag_returns_the_call_sites_fallback(pool: PgPool) {
    let address = serve(pool).await;
    let fixture = Fixture::create(address).await;

    let client = Client::builder(&fixture.base, &fixture.sdk_key)
        .poll_interval(Duration::ZERO)
        .connect()
        .await
        .unwrap();

    let context = EvaluationContext::new("user-1");
    assert!(client.is_enabled("never.created", &context, true));
    assert!(!client.is_enabled("never.created", &context, false));
    assert_eq!(client.string_value("never.created", &context, "blue"), "blue");

    let decision =
        client.evaluate("never.created", &context, flagforge_sdk::VariantValue::Bool(true));
    assert_eq!(decision.reason, flagforge_sdk::Reason::FlagNotFound);
}
