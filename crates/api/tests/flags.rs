//! Flag lifecycle, targeting and evaluation, end to end.

mod harness;

use axum::http::StatusCode;
use harness::{TestApp, simple_config};
use serde_json::{Value, json};
use sqlx::PgPool;

#[sqlx::test(migrations = "../../migrations")]
async fn a_new_flag_is_off_in_every_environment(pool: PgPool) {
    let app = TestApp::new(pool);
    let tenant = app.bootstrap("Acme", "ada@acme.test").await;

    // A second environment, created *before* the flag.
    app.post(
        "/api/v1/projects/checkout/environments",
        Some(&tenant.token),
        json!({"key": "staging", "name": "Staging"}),
    )
    .await
    .expect(StatusCode::CREATED);

    app.create_flag(&tenant.token, "checkout.v2").await;

    for environment in ["production", "staging"] {
        let config = app
            .get(
                &format!("/api/v1/projects/checkout/environments/{environment}/flags/checkout.v2"),
                Some(&tenant.token),
            )
            .await
            .expect(StatusCode::OK);

        assert_eq!(config["enabled"], false, "a new flag must be off in {environment}");
    }

    let decision = app.evaluate(&tenant.sdk_key, "checkout.v2", json!({"key": "u"})).await;
    assert_eq!(decision["value"], false);
    assert_eq!(decision["reason"]["kind"], "off");
}

/// The other direction: the environment arrives *after* the flags.
///
/// Adding `staging` to a project that already has flags is the ordinary
/// lifecycle, and it used to leave those flags with no configuration there —
/// missing from the dashboard and from the snapshot, so an SDK fell back to
/// its own default with nothing to distinguish that from "off".
#[sqlx::test(migrations = "../../migrations")]
async fn a_flag_defined_before_an_environment_is_still_configured_in_it(pool: PgPool) {
    let app = TestApp::new(pool);
    let tenant = app.bootstrap("Acme", "ada@acme.test").await;

    app.create_flag(&tenant.token, "checkout.v2").await;
    app.configure(&tenant.token, "checkout.v2", simple_config(true, "on"))
        .await
        .expect(StatusCode::OK);

    // Only now does the second environment exist.
    app.post(
        "/api/v1/projects/checkout/environments",
        Some(&tenant.token),
        json!({"key": "staging", "name": "Staging"}),
    )
    .await
    .expect(StatusCode::CREATED);

    let listed = app
        .get("/api/v1/projects/checkout/environments/staging/flags", Some(&tenant.token))
        .await
        .expect(StatusCode::OK);
    assert_eq!(
        listed.as_array().map(Vec::len),
        Some(1),
        "the flag must be listed in the new environment"
    );

    let config = app
        .get(
            "/api/v1/projects/checkout/environments/staging/flags/checkout.v2",
            Some(&tenant.token),
        )
        .await
        .expect(StatusCode::OK);

    // Seeded, not inherited: a new environment starts inert, and production's
    // targeting does not follow it in.
    assert_eq!(config["enabled"], false, "a seeded flag must start off");
    assert_eq!(config["rules"].as_array().map(Vec::len), Some(0));
    assert_eq!(config["off_variant"], "off");

    // And an SDK pointed at the new environment resolves it rather than
    // reporting a flag that does not exist.
    let key = app
        .post(
            "/api/v1/projects/checkout/environments/staging/keys",
            Some(&tenant.token),
            json!({"name": "staging-backend", "scope": "server"}),
        )
        .await
        .expect(StatusCode::CREATED);
    let staging_key = key["secret"].as_str().unwrap();

    let decision = app.evaluate(staging_key, "checkout.v2", json!({"key": "u"})).await;
    assert_eq!(decision["reason"]["kind"], "off", "not `flag_not_found`");
    assert_eq!(decision["value"], false);
}

/// A flag can also be defined while the project has no environments at all,
/// leaving no configuration anywhere to copy from.
#[sqlx::test(migrations = "../../migrations")]
async fn a_flag_defined_before_any_environment_exists_is_seeded_from_its_variants(pool: PgPool) {
    let app = TestApp::new(pool);
    let token = app.register("Acme", "ada@acme.test").await;

    app.post("/api/v1/projects", Some(&token), json!({"key": "checkout", "name": "Checkout"}))
        .await
        .expect(StatusCode::CREATED);

    app.post(
        "/api/v1/projects/checkout/flags",
        Some(&token),
        json!({"key": "checkout.v2", "name": "Checkout v2"}),
    )
    .await
    .expect(StatusCode::CREATED);

    app.post(
        "/api/v1/projects/checkout/environments",
        Some(&token),
        json!({"key": "production", "name": "Production", "is_production": true}),
    )
    .await
    .expect(StatusCode::CREATED);

    let config = app
        .get("/api/v1/projects/checkout/environments/production/flags/checkout.v2", Some(&token))
        .await
        .expect(StatusCode::OK);

    assert_eq!(config["enabled"], false);
    assert_eq!(config["off_variant"], "off");
    assert_eq!(config["fallthrough"]["kind"], "fixed");
    assert_eq!(config["fallthrough"]["variant"], "off");
}

#[sqlx::test(migrations = "../../migrations")]
async fn enabling_a_flag_changes_what_the_sdk_sees(pool: PgPool) {
    let app = TestApp::new(pool);
    let tenant = app.bootstrap("Acme", "ada@acme.test").await;
    app.create_flag(&tenant.token, "checkout.v2").await;

    app.configure(&tenant.token, "checkout.v2", simple_config(true, "on"))
        .await
        .expect(StatusCode::OK);

    let decision = app.evaluate(&tenant.sdk_key, "checkout.v2", json!({"key": "u"})).await;
    assert_eq!(decision["value"], true);
    assert_eq!(decision["reason"]["kind"], "fallthrough");
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_targeting_rule_selects_the_users_it_names(pool: PgPool) {
    let app = TestApp::new(pool);
    let tenant = app.bootstrap("Acme", "ada@acme.test").await;
    app.create_flag(&tenant.token, "checkout.v2").await;

    let rule_id = "11111111-1111-1111-1111-111111111111";
    app.configure(
        &tenant.token,
        "checkout.v2",
        json!({
            "enabled": true,
            "off_variant": "off",
            "fallthrough": {"kind": "fixed", "variant": "off"},
            "rules": [{
                "id": rule_id,
                "description": "Paid plans first",
                "conditions": [
                    {"attribute": "plan", "operator": "in", "values": ["pro", "enterprise"]},
                    {"attribute": "seats", "operator": "greater_than_or_equal", "values": [10]},
                ],
                "distribution": {"kind": "fixed", "variant": "on"},
            }],
        }),
    )
    .await
    .expect(StatusCode::OK);

    let matched = app
        .evaluate(
            &tenant.sdk_key,
            "checkout.v2",
            json!({"key": "u1", "attributes": {"plan": "pro", "seats": 25}}),
        )
        .await;
    assert_eq!(matched["value"], true);
    assert_eq!(matched["reason"]["kind"], "target_match");
    assert_eq!(matched["reason"]["rule_id"], rule_id);

    // Right plan, not enough seats: conditions are ANDed.
    let too_small = app
        .evaluate(
            &tenant.sdk_key,
            "checkout.v2",
            json!({"key": "u2", "attributes": {"plan": "pro", "seats": 3}}),
        )
        .await;
    assert_eq!(too_small["value"], false);
    assert_eq!(too_small["reason"]["kind"], "fallthrough");

    // Missing attributes must not match, rather than matching vacuously.
    let unknown = app.evaluate(&tenant.sdk_key, "checkout.v2", json!({"key": "u3"})).await;
    assert_eq!(unknown["value"], false);
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_percentage_rollout_is_sticky_and_lands_near_its_target(pool: PgPool) {
    let app = TestApp::new(pool);
    let tenant = app.bootstrap("Acme", "ada@acme.test").await;
    app.create_flag(&tenant.token, "checkout.v2").await;

    app.configure(
        &tenant.token,
        "checkout.v2",
        json!({
            "enabled": true,
            "off_variant": "off",
            "fallthrough": {
                "kind": "rollout",
                "weights": [
                    {"variant": "on", "weight": 25_000},
                    {"variant": "off", "weight": 75_000},
                ],
            },
            "rules": [],
        }),
    )
    .await
    .expect(StatusCode::OK);

    let mut enabled = 0;
    for i in 0..1_000 {
        let decision =
            app.evaluate(&tenant.sdk_key, "checkout.v2", json!({"key": format!("user-{i}")})).await;
        enabled += u32::from(decision["value"] == Value::Bool(true));
    }

    assert!(
        (200..=300).contains(&enabled),
        "a 25% rollout hit {enabled}/1000 users, which is outside sampling noise"
    );

    // The same user must not flip between calls; that is what makes a rollout
    // an experiment rather than a coin toss per request.
    let first = app.evaluate(&tenant.sdk_key, "checkout.v2", json!({"key": "user-7"})).await;
    for _ in 0..20 {
        let again = app.evaluate(&tenant.sdk_key, "checkout.v2", json!({"key": "user-7"})).await;
        assert_eq!(again["value"], first["value"]);
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn two_environments_serve_the_same_flag_differently(pool: PgPool) {
    let app = TestApp::new(pool);
    let tenant = app.bootstrap("Acme", "ada@acme.test").await;

    app.post(
        "/api/v1/projects/checkout/environments",
        Some(&tenant.token),
        json!({"key": "staging", "name": "Staging"}),
    )
    .await
    .expect(StatusCode::CREATED);

    let staging_key = app
        .post(
            "/api/v1/projects/checkout/environments/staging/keys",
            Some(&tenant.token),
            json!({"name": "staging-backend", "scope": "server"}),
        )
        .await
        .expect(StatusCode::CREATED)["secret"]
        .as_str()
        .unwrap()
        .to_owned();

    app.create_flag(&tenant.token, "checkout.v2").await;

    // On in staging, still off in production.
    app.put(
        "/api/v1/projects/checkout/environments/staging/flags/checkout.v2",
        Some(&tenant.token),
        simple_config(true, "on"),
    )
    .await
    .expect(StatusCode::OK);

    let staging = app.evaluate(&staging_key, "checkout.v2", json!({"key": "u"})).await;
    let production = app.evaluate(&tenant.sdk_key, "checkout.v2", json!({"key": "u"})).await;

    assert_eq!(staging["value"], true);
    assert_eq!(production["value"], false);
}

#[sqlx::test(migrations = "../../migrations")]
async fn an_unknown_flag_returns_the_callers_own_default(pool: PgPool) {
    let app = TestApp::new(pool);
    let tenant = app.bootstrap("Acme", "ada@acme.test").await;

    let decision = app
        .post(
            "/api/v1/evaluate/never-created",
            Some(&tenant.sdk_key),
            json!({"context": {"key": "u"}, "default": "fallback-value"}),
        )
        .await
        .expect(StatusCode::OK);

    assert_eq!(decision["reason"]["kind"], "flag_not_found");
    assert_eq!(decision["value"], "fallback-value");
    assert_eq!(decision["variant"], Value::Null);
}

#[sqlx::test(migrations = "../../migrations")]
async fn an_archived_flag_disappears_from_evaluation(pool: PgPool) {
    let app = TestApp::new(pool);
    let tenant = app.bootstrap("Acme", "ada@acme.test").await;
    app.create_flag(&tenant.token, "checkout.v2").await;
    app.configure(&tenant.token, "checkout.v2", simple_config(true, "on"))
        .await
        .expect(StatusCode::OK);

    app.patch(
        "/api/v1/projects/checkout/flags/checkout.v2",
        Some(&tenant.token),
        json!({"archived": true}),
    )
    .await
    .expect(StatusCode::OK);

    let all = app
        .post("/api/v1/evaluate", Some(&tenant.sdk_key), json!({"context": {"key": "u"}}))
        .await
        .expect(StatusCode::OK);
    assert_eq!(all["evaluations"].as_array().unwrap().len(), 0);

    // Still visible to operators who ask for it.
    let listed = app
        .get("/api/v1/projects/checkout/flags?include_archived=true", Some(&tenant.token))
        .await
        .expect(StatusCode::OK);
    assert_eq!(listed.as_array().unwrap().len(), 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_configuration_that_cannot_be_evaluated_is_rejected(pool: PgPool) {
    let app = TestApp::new(pool);
    let tenant = app.bootstrap("Acme", "ada@acme.test").await;
    app.create_flag(&tenant.token, "checkout.v2").await;

    let bad_weights = app
        .configure(
            &tenant.token,
            "checkout.v2",
            json!({
                "enabled": true,
                "off_variant": "off",
                "fallthrough": {
                    "kind": "rollout",
                    "weights": [{"variant": "on", "weight": 5}],
                },
            }),
        )
        .await;
    assert_eq!(bad_weights.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(bad_weights.body["errors"][0]["path"], "fallthrough.weights");

    let unknown_variant = app
        .configure(
            &tenant.token,
            "checkout.v2",
            json!({
                "enabled": true,
                "off_variant": "off",
                "fallthrough": {"kind": "fixed", "variant": "ghost"},
            }),
        )
        .await;
    assert_eq!(unknown_variant.status, StatusCode::UNPROCESSABLE_ENTITY);

    let bad_regex = app
        .configure(
            &tenant.token,
            "checkout.v2",
            json!({
                "enabled": true,
                "off_variant": "off",
                "fallthrough": {"kind": "fixed", "variant": "on"},
                "rules": [{
                    "id": "22222222-2222-2222-2222-222222222222",
                    "conditions": [
                        {"attribute": "email", "operator": "matches", "values": ["([a-z"]},
                    ],
                    "distribution": {"kind": "fixed", "variant": "on"},
                }],
            }),
        )
        .await;
    assert_eq!(bad_regex.status, StatusCode::UNPROCESSABLE_ENTITY);

    // Nothing was written by any of the three attempts.
    let decision = app.evaluate(&tenant.sdk_key, "checkout.v2", json!({"key": "u"})).await;
    assert_eq!(decision["reason"]["kind"], "off");
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_stale_write_loses_to_the_one_that_got_there_first(pool: PgPool) {
    let app = TestApp::new(pool);
    let tenant = app.bootstrap("Acme", "ada@acme.test").await;
    app.create_flag(&tenant.token, "checkout.v2").await;

    let current = app
        .get(
            "/api/v1/projects/checkout/environments/production/flags/checkout.v2",
            Some(&tenant.token),
        )
        .await
        .expect(StatusCode::OK);
    let version = current["version"].as_i64().unwrap();

    // Two operators both read version N; the first write wins.
    let mut first = simple_config(true, "on");
    first["expected_version"] = json!(version);
    let applied = app.configure(&tenant.token, "checkout.v2", first).await.expect(StatusCode::OK);
    assert_eq!(applied["version"], version + 1);

    let mut second = simple_config(false, "on");
    second["expected_version"] = json!(version);
    let rejected = app.configure(&tenant.token, "checkout.v2", second).await;

    assert_eq!(rejected.status, StatusCode::CONFLICT);
    assert!(
        rejected.body["title"].as_str().unwrap().contains("modified by someone else"),
        "{}",
        rejected.body
    );

    // The winner's configuration is intact.
    let decision = app.evaluate(&tenant.sdk_key, "checkout.v2", json!({"key": "u"})).await;
    assert_eq!(decision["value"], true);
}

#[sqlx::test(migrations = "../../migrations")]
async fn removing_a_variant_an_environment_still_serves_is_refused(pool: PgPool) {
    let app = TestApp::new(pool);
    let tenant = app.bootstrap("Acme", "ada@acme.test").await;

    app.post(
        "/api/v1/projects/checkout/flags",
        Some(&tenant.token),
        json!({
            "key": "theme",
            "name": "Theme",
            "variants": [
                {"key": "blue", "value": "blue"},
                {"key": "green", "value": "green"},
                {"key": "off", "value": false},
            ],
            "off_variant": "off",
            "fallthrough": {"kind": "fixed", "variant": "blue"},
        }),
    )
    .await
    .expect(StatusCode::CREATED);

    app.configure(
        &tenant.token,
        "theme",
        json!({
            "enabled": true,
            "off_variant": "off",
            "fallthrough": {"kind": "fixed", "variant": "green"},
        }),
    )
    .await
    .expect(StatusCode::OK);

    // `green` is live in production, so dropping it must fail loudly rather
    // than leave production pointing at a variant that no longer exists.
    let response = app
        .patch(
            "/api/v1/projects/checkout/flags/theme",
            Some(&tenant.token),
            json!({
                "variants": [
                    {"key": "blue", "value": "blue"},
                    {"key": "off", "value": false},
                ],
            }),
        )
        .await;

    assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY);
    let path = response.body["errors"][0]["path"].as_str().unwrap();
    assert!(path.starts_with("environments.production."), "{path}");

    // Production still resolves.
    let decision = app.evaluate(&tenant.sdk_key, "theme", json!({"key": "u"})).await;
    assert_eq!(decision["value"], "green");
}

#[sqlx::test(migrations = "../../migrations")]
async fn evaluating_everything_returns_one_decision_per_flag(pool: PgPool) {
    let app = TestApp::new(pool);
    let tenant = app.bootstrap("Acme", "ada@acme.test").await;

    for key in ["alpha", "beta", "gamma"] {
        app.create_flag(&tenant.token, key).await;
    }
    app.configure(&tenant.token, "beta", simple_config(true, "on")).await.expect(StatusCode::OK);

    let all = app
        .post(
            "/api/v1/evaluate",
            Some(&tenant.sdk_key),
            json!({"context": {"key": "u", "attributes": {"plan": "pro"}}}),
        )
        .await
        .expect(StatusCode::OK);

    let evaluations = all["evaluations"].as_array().unwrap();
    assert_eq!(evaluations.len(), 3);
    assert_eq!(all["environment"], "production");

    let keys: Vec<&str> = evaluations.iter().map(|e| e["flag_key"].as_str().unwrap()).collect();
    assert_eq!(keys, ["alpha", "beta", "gamma"], "ordering must be stable");

    let subset = app
        .post(
            "/api/v1/evaluate",
            Some(&tenant.sdk_key),
            json!({"context": {"key": "u"}, "flags": ["beta"]}),
        )
        .await
        .expect(StatusCode::OK);
    assert_eq!(subset["evaluations"].as_array().unwrap().len(), 1);
    assert_eq!(subset["evaluations"][0]["value"], true);
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_context_without_a_key_is_refused(pool: PgPool) {
    let app = TestApp::new(pool);
    let tenant = app.bootstrap("Acme", "ada@acme.test").await;

    let response =
        app.post("/api/v1/evaluate", Some(&tenant.sdk_key), json!({"context": {"key": ""}})).await;

    assert_eq!(response.status, StatusCode::BAD_REQUEST);
}
