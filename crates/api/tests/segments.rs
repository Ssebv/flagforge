//! Reusable audiences, end to end: definition, reference, evaluation, removal.

mod harness;

use axum::http::StatusCode;
use harness::TestApp;
use serde_json::{Value, json};
use sqlx::PgPool;

const SEGMENTS: &str = "/api/v1/projects/checkout/environments/production/segments";

/// A flag rule that serves `on` to everyone in `segment`.
fn gated_on(segment: &str) -> Value {
    json!({
        "enabled": true,
        "off_variant": "off",
        "fallthrough": {"kind": "fixed", "variant": "off"},
        "rules": [{
            "id": "11111111-1111-1111-1111-111111111111",
            "conditions": [],
            "segments": {"any_of": [segment]},
            "distribution": {"kind": "fixed", "variant": "on"},
        }],
    })
}

/// Membership rule: `plan` is `pro`.
fn pro_members() -> Value {
    json!({
        "rules": [{
            "id": "22222222-2222-2222-2222-222222222222",
            "conditions": [{"attribute": "plan", "operator": "in", "values": ["pro"]}],
        }],
    })
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_flag_rule_serves_through_its_segment(pool: PgPool) {
    let app = TestApp::new(pool);
    let tenant = app.bootstrap("Acme", "ada@acme.test").await;

    app.post(SEGMENTS, Some(&tenant.token), json!({"key": "beta", "name": "Beta testers"}))
        .await
        .expect(StatusCode::CREATED);

    app.put(&format!("{SEGMENTS}/beta"), Some(&tenant.token), pro_members())
        .await
        .expect(StatusCode::OK);

    app.create_flag(&tenant.token, "checkout.v2").await;
    app.configure(&tenant.token, "checkout.v2", gated_on("beta")).await.expect(StatusCode::OK);

    let member = app
        .evaluate(
            &tenant.sdk_key,
            "checkout.v2",
            json!({"key": "u", "attributes": {"plan": "pro"}}),
        )
        .await;
    assert_eq!(member["value"], true, "a member must get the targeted variant");
    assert_eq!(member["reason"]["kind"], "target_match");

    let outsider = app
        .evaluate(
            &tenant.sdk_key,
            "checkout.v2",
            json!({"key": "u", "attributes": {"plan": "free"}}),
        )
        .await;
    assert_eq!(outsider["value"], false);
    assert_eq!(outsider["reason"]["kind"], "fallthrough");
}

/// The whole point of segments: one edit moves every flag that references it.
#[sqlx::test(migrations = "../../migrations")]
async fn editing_a_segment_moves_every_flag_that_references_it(pool: PgPool) {
    let app = TestApp::new(pool);
    let tenant = app.bootstrap("Acme", "ada@acme.test").await;

    app.post(SEGMENTS, Some(&tenant.token), json!({"key": "beta", "name": "Beta"}))
        .await
        .expect(StatusCode::CREATED);
    app.put(&format!("{SEGMENTS}/beta"), Some(&tenant.token), pro_members())
        .await
        .expect(StatusCode::OK);

    for flag in ["checkout.v2", "new.nav"] {
        app.create_flag(&tenant.token, flag).await;
        app.configure(&tenant.token, flag, gated_on("beta")).await.expect(StatusCode::OK);
    }

    let team = json!({"key": "u", "attributes": {"plan": "team"}});
    for flag in ["checkout.v2", "new.nav"] {
        assert_eq!(app.evaluate(&tenant.sdk_key, flag, team.clone()).await["value"], false);
    }

    // Widen the audience once …
    app.put(
        &format!("{SEGMENTS}/beta"),
        Some(&tenant.token),
        json!({
            "rules": [{
                "id": "22222222-2222-2222-2222-222222222222",
                "conditions": [{
                    "attribute": "plan", "operator": "in", "values": ["pro", "team"],
                }],
            }],
        }),
    )
    .await
    .expect(StatusCode::OK);

    // … and both flags follow, without either being touched.
    for flag in ["checkout.v2", "new.nav"] {
        assert_eq!(
            app.evaluate(&tenant.sdk_key, flag, team.clone()).await["value"],
            true,
            "{flag} did not follow the segment"
        );
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn exclusion_beats_inclusion_end_to_end(pool: PgPool) {
    let app = TestApp::new(pool);
    let tenant = app.bootstrap("Acme", "ada@acme.test").await;

    app.post(SEGMENTS, Some(&tenant.token), json!({"key": "beta", "name": "Beta"}))
        .await
        .expect(StatusCode::CREATED);
    app.put(
        &format!("{SEGMENTS}/beta"),
        Some(&tenant.token),
        json!({"included": ["invited"], "excluded": ["revoked"]}),
    )
    .await
    .expect(StatusCode::OK);

    app.create_flag(&tenant.token, "checkout.v2").await;
    app.configure(&tenant.token, "checkout.v2", gated_on("beta")).await.expect(StatusCode::OK);

    let on = app.evaluate(&tenant.sdk_key, "checkout.v2", json!({"key": "invited"})).await;
    assert_eq!(on["value"], true);

    let off = app.evaluate(&tenant.sdk_key, "checkout.v2", json!({"key": "revoked"})).await;
    assert_eq!(off["value"], false);
}

/// A cohort has to hold still across flags, or it is not a cohort.
#[sqlx::test(migrations = "../../migrations")]
async fn a_segment_rollout_puts_the_same_people_in_every_flag(pool: PgPool) {
    let app = TestApp::new(pool);
    let tenant = app.bootstrap("Acme", "ada@acme.test").await;

    app.post(SEGMENTS, Some(&tenant.token), json!({"key": "canary", "name": "Canary"}))
        .await
        .expect(StatusCode::CREATED);
    app.put(
        &format!("{SEGMENTS}/canary"),
        Some(&tenant.token),
        json!({
            "rules": [{
                "id": "33333333-3333-3333-3333-333333333333",
                "conditions": [],
                "rollout": {"percentage": 50_000},
            }],
        }),
    )
    .await
    .expect(StatusCode::OK);

    for flag in ["checkout.v2", "new.nav"] {
        app.create_flag(&tenant.token, flag).await;
        app.configure(&tenant.token, flag, gated_on("canary")).await.expect(StatusCode::OK);
    }

    let mut inside = 0;
    for i in 0..60 {
        let ctx = json!({"key": format!("user-{i}")});
        let a = app.evaluate(&tenant.sdk_key, "checkout.v2", ctx.clone()).await["value"].clone();
        let b = app.evaluate(&tenant.sdk_key, "new.nav", ctx).await["value"].clone();

        assert_eq!(a, b, "user-{i} is in the cohort for one flag but not the other");
        if a == Value::Bool(true) {
            inside += 1;
        }
    }

    // A 50 % cohort over 60 subjects: loose bounds, but a broken rollout that
    // admits everyone or nobody still fails.
    assert!((15..45).contains(&inside), "expected roughly half the cohort, got {inside}/60");
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_rule_cannot_name_a_segment_this_environment_does_not_define(pool: PgPool) {
    let app = TestApp::new(pool);
    let tenant = app.bootstrap("Acme", "ada@acme.test").await;
    app.create_flag(&tenant.token, "checkout.v2").await;

    let refused = app.configure(&tenant.token, "checkout.v2", gated_on("ghost")).await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY, "body was {}", refused.body);
    assert!(
        refused.body.to_string().contains("ghost"),
        "the error should name the missing segment: {}",
        refused.body
    );
}

/// Segments are per environment, so a rule in production cannot borrow
/// staging's definition — that would make a staging audience a production
/// liability.
#[sqlx::test(migrations = "../../migrations")]
async fn a_segment_does_not_leak_across_environments(pool: PgPool) {
    let app = TestApp::new(pool);
    let tenant = app.bootstrap("Acme", "ada@acme.test").await;

    app.post(
        "/api/v1/projects/checkout/environments",
        Some(&tenant.token),
        json!({"key": "staging", "name": "Staging"}),
    )
    .await
    .expect(StatusCode::CREATED);

    app.post(
        "/api/v1/projects/checkout/environments/staging/segments",
        Some(&tenant.token),
        json!({"key": "beta", "name": "Beta"}),
    )
    .await
    .expect(StatusCode::CREATED);

    // Defined in staging only: production must not see it …
    let listed = app.get(SEGMENTS, Some(&tenant.token)).await.expect(StatusCode::OK);
    assert_eq!(listed.as_array().map(Vec::len), Some(0));

    // … and a production rule must not be able to name it.
    app.create_flag(&tenant.token, "checkout.v2").await;
    let refused = app.configure(&tenant.token, "checkout.v2", gated_on("beta")).await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY, "body was {}", refused.body);
}

#[sqlx::test(migrations = "../../migrations")]
async fn deleting_a_referenced_segment_is_refused_and_names_the_flags(pool: PgPool) {
    let app = TestApp::new(pool);
    let tenant = app.bootstrap("Acme", "ada@acme.test").await;

    app.post(SEGMENTS, Some(&tenant.token), json!({"key": "beta", "name": "Beta"}))
        .await
        .expect(StatusCode::CREATED);

    app.create_flag(&tenant.token, "checkout.v2").await;
    app.configure(&tenant.token, "checkout.v2", gated_on("beta")).await.expect(StatusCode::OK);

    let refused = app.delete(&format!("{SEGMENTS}/beta"), Some(&tenant.token)).await;
    assert_eq!(refused.status, StatusCode::CONFLICT, "body was {}", refused.body);
    assert!(
        refused.body.to_string().contains("checkout.v2"),
        "the error should name the blocking flag: {}",
        refused.body
    );

    // The usage endpoint tells the operator the same thing before they try.
    let usage =
        app.get(&format!("{SEGMENTS}/beta"), Some(&tenant.token)).await.expect(StatusCode::OK);
    assert_eq!(usage["referenced_by"], json!(["checkout.v2"]));

    // Drop the reference and the delete goes through.
    app.configure(
        &tenant.token,
        "checkout.v2",
        json!({
            "enabled": true,
            "off_variant": "off",
            "fallthrough": {"kind": "fixed", "variant": "off"},
            "rules": [],
        }),
    )
    .await
    .expect(StatusCode::OK);

    app.delete(&format!("{SEGMENTS}/beta"), Some(&tenant.token))
        .await
        .expect(StatusCode::NO_CONTENT);
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_stale_expected_version_loses_the_write(pool: PgPool) {
    let app = TestApp::new(pool);
    let tenant = app.bootstrap("Acme", "ada@acme.test").await;

    let created = app
        .post(SEGMENTS, Some(&tenant.token), json!({"key": "beta", "name": "Beta"}))
        .await
        .expect(StatusCode::CREATED);
    let version = created["version"].as_i64().unwrap();

    app.put(
        &format!("{SEGMENTS}/beta"),
        Some(&tenant.token),
        json!({"name": "Beta testers", "expected_version": version}),
    )
    .await
    .expect(StatusCode::OK);

    // The second writer read the same version and is now behind.
    let lost = app
        .put(
            &format!("{SEGMENTS}/beta"),
            Some(&tenant.token),
            json!({"name": "Something else", "expected_version": version}),
        )
        .await;
    assert_eq!(lost.status, StatusCode::CONFLICT, "body was {}", lost.body);
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_segment_edit_moves_the_snapshot_version(pool: PgPool) {
    let app = TestApp::new(pool);
    let tenant = app.bootstrap("Acme", "ada@acme.test").await;

    app.create_flag(&tenant.token, "checkout.v2").await;
    app.post(SEGMENTS, Some(&tenant.token), json!({"key": "beta", "name": "Beta"}))
        .await
        .expect(StatusCode::CREATED);

    let before = app.get("/api/v1/snapshot", Some(&tenant.sdk_key)).await.expect(StatusCode::OK);

    app.put(&format!("{SEGMENTS}/beta"), Some(&tenant.token), pro_members())
        .await
        .expect(StatusCode::OK);

    let after = app.get("/api/v1/snapshot", Some(&tenant.sdk_key)).await.expect(StatusCode::OK);

    assert!(
        after["version"].as_i64() > before["version"].as_i64(),
        "a segment edit must invalidate caches keyed on the snapshot version: {} -> {}",
        before["version"],
        after["version"]
    );
    assert!(after["segments"]["beta"].is_object(), "the snapshot must carry the segment");
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_contradictory_segment_is_refused(pool: PgPool) {
    let app = TestApp::new(pool);
    let tenant = app.bootstrap("Acme", "ada@acme.test").await;

    app.post(SEGMENTS, Some(&tenant.token), json!({"key": "beta", "name": "Beta"}))
        .await
        .expect(StatusCode::CREATED);

    let refused = app
        .put(
            &format!("{SEGMENTS}/beta"),
            Some(&tenant.token),
            json!({"included": ["u"], "excluded": ["u"]}),
        )
        .await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY, "body was {}", refused.body);
}

#[sqlx::test(migrations = "../../migrations")]
async fn segment_changes_are_written_to_the_audit_log(pool: PgPool) {
    let app = TestApp::new(pool);
    let tenant = app.bootstrap("Acme", "ada@acme.test").await;

    app.post(SEGMENTS, Some(&tenant.token), json!({"key": "beta", "name": "Beta"}))
        .await
        .expect(StatusCode::CREATED);
    app.put(&format!("{SEGMENTS}/beta"), Some(&tenant.token), pro_members())
        .await
        .expect(StatusCode::OK);
    app.delete(&format!("{SEGMENTS}/beta"), Some(&tenant.token))
        .await
        .expect(StatusCode::NO_CONTENT);

    let log = app.get("/api/v1/audit", Some(&tenant.token)).await.expect(StatusCode::OK);
    let actions: Vec<&str> =
        log["entries"].as_array().unwrap().iter().filter_map(|e| e["action"].as_str()).collect();

    for expected in ["segment.created", "segment.updated", "segment.deleted"] {
        assert!(actions.contains(&expected), "missing {expected} from {actions:?}");
    }
}
