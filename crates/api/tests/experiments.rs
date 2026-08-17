//! Experiments end to end: lifecycle, event ingest, judged results, guardrails.

mod harness;

use axum::http::StatusCode;
use harness::TestApp;
use serde_json::{Value, json};
use sqlx::PgPool;

const EXPERIMENTS: &str = "/api/v1/projects/checkout/environments/production/experiments";

fn checkout_experiment() -> Value {
    json!({
        "key": "checkout-cta",
        "name": "Checkout CTA",
        "flag_key": "checkout.v2",
        "metric_key": "order.completed",
        "control_variant": "off",
    })
}

/// A batch of `count`-weighted events for one variant.
fn events(variant: &str, exposures: u64, conversions: u64) -> Value {
    json!({
        "events": [
            {"experiment_key": "checkout-cta", "variant": variant,
             "kind": "exposure", "count": exposures},
            {"experiment_key": "checkout-cta", "variant": variant,
             "kind": "conversion", "count": conversions},
        ]
    })
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_full_lifecycle_measures_and_stops_measuring(pool: PgPool) {
    let app = TestApp::new(pool);
    let tenant = app.bootstrap("Acme", "ada@acme.test").await;
    app.create_flag(&tenant.token, "checkout.v2").await;

    app.post(EXPERIMENTS, Some(&tenant.token), checkout_experiment())
        .await
        .expect(StatusCode::CREATED);

    // A draft is not yet measuring, so the snapshot must not announce it.
    let snapshot = app.get("/api/v1/snapshot", Some(&tenant.sdk_key)).await.expect(StatusCode::OK);
    assert_eq!(snapshot["experiments"], json!([]), "drafts must not reach SDKs");

    // Events for a draft are dropped, not errors: an SDK cannot know yet.
    let dropped = app
        .post("/api/v1/events", Some(&tenant.sdk_key), events("on", 10, 1))
        .await
        .expect(StatusCode::ACCEPTED);
    assert_eq!(dropped["accepted"], 0);
    assert_eq!(dropped["received"], 2);

    app.post(&format!("{EXPERIMENTS}/checkout-cta/start"), Some(&tenant.token), json!({}))
        .await
        .expect(StatusCode::OK);

    let snapshot = app.get("/api/v1/snapshot", Some(&tenant.sdk_key)).await.expect(StatusCode::OK);
    assert_eq!(snapshot["experiments"][0]["key"], "checkout-cta");
    assert_eq!(snapshot["experiments"][0]["flag_key"], "checkout.v2");
    assert_eq!(snapshot["experiments"][0]["metric_key"], "order.completed");

    // A clear difference: 15% converting against 10% over a thousand each.
    for (variant, exposures, conversions) in [("on", 1000, 150), ("off", 1000, 100)] {
        app.post("/api/v1/events", Some(&tenant.sdk_key), events(variant, exposures, conversions))
            .await
            .expect(StatusCode::ACCEPTED);
    }

    let body = app
        .get(&format!("{EXPERIMENTS}/checkout-cta/results"), Some(&tenant.token))
        .await
        .expect(StatusCode::OK);

    let results = body["results"].as_array().expect("results array");
    assert_eq!(results.len(), 2, "{body}");
    let on = &results[0];
    assert_eq!(on["variant"], "on");
    assert_eq!(on["exposures"], 1000);
    assert_eq!(on["conversions"], 150);
    assert_eq!(on["rate"], 0.15);
    assert_eq!(on["vs_control"]["significant"], true, "{on}");
    assert!(on["vs_control"]["p_value"].as_f64().unwrap() < 0.001);
    // The control carries numbers but no verdict about itself.
    assert_eq!(results[1]["variant"], "off");
    assert!(results[1]["vs_control"].is_null());

    app.post(&format!("{EXPERIMENTS}/checkout-cta/stop"), Some(&tenant.token), json!({}))
        .await
        .expect(StatusCode::OK);

    // Stopped: gone from the snapshot, and late events no longer count.
    let snapshot = app.get("/api/v1/snapshot", Some(&tenant.sdk_key)).await.expect(StatusCode::OK);
    assert_eq!(snapshot["experiments"], json!([]));
    let late = app
        .post("/api/v1/events", Some(&tenant.sdk_key), events("on", 5, 1))
        .await
        .expect(StatusCode::ACCEPTED);
    assert_eq!(late["accepted"], 0, "a stopped experiment must stop counting");

    // And stopped is terminal.
    let refused = app
        .post(&format!("{EXPERIMENTS}/checkout-cta/start"), Some(&tenant.token), json!({}))
        .await;
    refused.expect(StatusCode::CONFLICT);
}

#[sqlx::test(migrations = "../../migrations")]
async fn every_arm_renders_before_any_event_arrives(pool: PgPool) {
    let app = TestApp::new(pool);
    let tenant = app.bootstrap("Acme", "ada@acme.test").await;
    app.create_flag(&tenant.token, "checkout.v2").await;

    app.post(EXPERIMENTS, Some(&tenant.token), checkout_experiment())
        .await
        .expect(StatusCode::CREATED);

    let body = app
        .get(&format!("{EXPERIMENTS}/checkout-cta/results"), Some(&tenant.token))
        .await
        .expect(StatusCode::OK);

    let results = body["results"].as_array().expect("results array");
    assert_eq!(results.len(), 2, "both arms must render zero-filled: {body}");
    for arm in results {
        assert_eq!(arm["exposures"], 0);
        assert!(arm["rate"].is_null(), "no exposures means no rate, not 0%");
        assert!(arm["vs_control"].is_null());
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_control_that_is_not_a_variant_is_refused(pool: PgPool) {
    let app = TestApp::new(pool);
    let tenant = app.bootstrap("Acme", "ada@acme.test").await;
    app.create_flag(&tenant.token, "checkout.v2").await;

    let mut body = checkout_experiment();
    body["control_variant"] = "ghost".into();
    let refused = app.post(EXPERIMENTS, Some(&tenant.token), body).await;
    let problem = refused.expect(StatusCode::BAD_REQUEST);
    // The message must name the variants that would have been accepted.
    assert!(problem["title"].as_str().unwrap().contains("on, off"), "{problem}");
}

#[sqlx::test(migrations = "../../migrations")]
async fn an_experiment_pins_its_flag_against_deletion(pool: PgPool) {
    let app = TestApp::new(pool);
    let tenant = app.bootstrap("Acme", "ada@acme.test").await;
    app.create_flag(&tenant.token, "checkout.v2").await;

    app.post(EXPERIMENTS, Some(&tenant.token), checkout_experiment())
        .await
        .expect(StatusCode::CREATED);

    let refused =
        app.delete("/api/v1/projects/checkout/flags/checkout.v2", Some(&tenant.token)).await;
    let problem = refused.expect(StatusCode::CONFLICT);
    assert!(
        problem["title"].as_str().unwrap().contains("production/checkout-cta"),
        "the refusal must name the experiment: {problem}"
    );

    // Removing the experiment unpins the flag.
    app.delete(&format!("{EXPERIMENTS}/checkout-cta"), Some(&tenant.token))
        .await
        .expect(StatusCode::NO_CONTENT);
    app.delete("/api/v1/projects/checkout/flags/checkout.v2", Some(&tenant.token))
        .await
        .expect(StatusCode::NO_CONTENT);
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_running_experiment_cannot_be_deleted_mid_measurement(pool: PgPool) {
    let app = TestApp::new(pool);
    let tenant = app.bootstrap("Acme", "ada@acme.test").await;
    app.create_flag(&tenant.token, "checkout.v2").await;

    app.post(EXPERIMENTS, Some(&tenant.token), checkout_experiment())
        .await
        .expect(StatusCode::CREATED);
    app.post(&format!("{EXPERIMENTS}/checkout-cta/start"), Some(&tenant.token), json!({}))
        .await
        .expect(StatusCode::OK);

    app.delete(&format!("{EXPERIMENTS}/checkout-cta"), Some(&tenant.token))
        .await
        .expect(StatusCode::CONFLICT);

    app.post(&format!("{EXPERIMENTS}/checkout-cta/stop"), Some(&tenant.token), json!({}))
        .await
        .expect(StatusCode::OK);
    app.delete(&format!("{EXPERIMENTS}/checkout-cta"), Some(&tenant.token))
        .await
        .expect(StatusCode::NO_CONTENT);
}

#[sqlx::test(migrations = "../../migrations")]
async fn duplicate_cells_in_one_batch_are_summed_not_a_server_error(pool: PgPool) {
    let app = TestApp::new(pool);
    let tenant = app.bootstrap("Acme", "ada@acme.test").await;
    app.create_flag(&tenant.token, "checkout.v2").await;

    app.post(EXPERIMENTS, Some(&tenant.token), checkout_experiment())
        .await
        .expect(StatusCode::CREATED);
    app.post(&format!("{EXPERIMENTS}/checkout-cta/start"), Some(&tenant.token), json!({}))
        .await
        .expect(StatusCode::OK);

    // Three deltas landing in the same hourly cell — the shape that makes a
    // naive multi-row upsert fail with "cannot affect row a second time".
    let body = json!({
        "events": [
            {"experiment_key": "checkout-cta", "variant": "on", "kind": "exposure", "count": 1},
            {"experiment_key": "checkout-cta", "variant": "on", "kind": "exposure", "count": 2},
            {"experiment_key": "checkout-cta", "variant": "on", "kind": "exposure", "count": 3},
        ]
    });
    app.post("/api/v1/events", Some(&tenant.sdk_key), body).await.expect(StatusCode::ACCEPTED);

    let results = app
        .get(&format!("{EXPERIMENTS}/checkout-cta/results"), Some(&tenant.token))
        .await
        .expect(StatusCode::OK);
    assert_eq!(results["results"][0]["exposures"], 6, "{results}");
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_results_series_buckets_by_hour_and_windows_a_week(pool: PgPool) {
    let app = TestApp::new(pool);
    let tenant = app.bootstrap("Acme", "ada@acme.test").await;
    app.create_flag(&tenant.token, "checkout.v2").await;

    app.post(EXPERIMENTS, Some(&tenant.token), checkout_experiment())
        .await
        .expect(StatusCode::CREATED);
    app.post(&format!("{EXPERIMENTS}/checkout-cta/start"), Some(&tenant.token), json!({}))
        .await
        .expect(StatusCode::OK);

    // Two events inside one hour, one in the next, and one far outside the
    // window — the first two must merge, the last must not appear at all.
    //
    // Anchored to a *truncated* hour, not to `now` minus two hours: the naive
    // version put `hour_a + 20min` in a different bucket whenever the test ran
    // past minute 40, which is a 33 % failure window that CI eventually hit.
    use chrono::DurationRound;
    let now = chrono::Utc::now();
    let hour_a = (now - chrono::TimeDelta::hours(2))
        .duration_trunc(chrono::TimeDelta::hours(1))
        .expect("hour truncation");
    let hour_a_later = hour_a + chrono::TimeDelta::minutes(20);
    let hour_b = hour_a + chrono::TimeDelta::hours(1);
    let ancient = now - chrono::TimeDelta::days(30);
    let events = json!({
        "events": [
            {"experiment_key": "checkout-cta", "variant": "on", "kind": "exposure",
             "count": 3, "at": hour_a},
            {"experiment_key": "checkout-cta", "variant": "on", "kind": "exposure",
             "count": 2, "at": hour_a_later},
            {"experiment_key": "checkout-cta", "variant": "on", "kind": "conversion",
             "count": 1, "at": hour_b},
            {"experiment_key": "checkout-cta", "variant": "on", "kind": "exposure",
             "count": 99, "at": ancient},
        ]
    });
    app.post("/api/v1/events", Some(&tenant.sdk_key), events).await.expect(StatusCode::ACCEPTED);

    let body = app
        .get(&format!("{EXPERIMENTS}/checkout-cta/results"), Some(&tenant.token))
        .await
        .expect(StatusCode::OK);

    let series = body["series"].as_array().expect("series array");
    assert_eq!(series.len(), 1, "only arms with points in the window appear: {body}");
    assert_eq!(series[0]["variant"], "on");

    let points = series[0]["points"].as_array().expect("points");
    assert_eq!(points.len(), 2, "two hours touched inside the window: {body}");
    assert_eq!(points[0]["exposures"], 5, "same-hour events must merge into one point");
    assert_eq!(points[0]["conversions"], 0);
    assert_eq!(points[1]["conversions"], 1);
    // Oldest first, and truncated to the hour.
    let first = points[0]["hour"].as_str().unwrap();
    let second = points[1]["hour"].as_str().unwrap();
    assert!(first < second, "points must come oldest first: {first} vs {second}");
    assert!(first.contains(":00:00"), "hours must be truncated: {first}");

    // The totals still count everything, window or not.
    assert_eq!(body["results"][0]["exposures"], 104, "{body}");
}

#[sqlx::test(migrations = "../../migrations")]
async fn events_require_an_sdk_key(pool: PgPool) {
    let app = TestApp::new(pool);
    let tenant = app.bootstrap("Acme", "ada@acme.test").await;

    app.post("/api/v1/events", None, events("on", 1, 0)).await.expect(StatusCode::UNAUTHORIZED);

    // A management token is not an SDK key either.
    app.post("/api/v1/events", Some(&tenant.token), events("on", 1, 0))
        .await
        .expect(StatusCode::UNAUTHORIZED);
}
