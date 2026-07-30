//! Isolation between tenants, scope enforcement and the audit trail.

mod harness;

use axum::http::StatusCode;
use harness::{TestApp, simple_config};
use serde_json::json;
use sqlx::PgPool;

#[sqlx::test(migrations = "../../migrations")]
async fn one_organization_cannot_see_or_touch_another(pool: PgPool) {
    let app = TestApp::new(pool);

    let acme = app.bootstrap("Acme", "ada@acme.test").await;
    app.create_flag(&acme.token, "checkout.v2").await;

    // A second tenant that happens to use the same project and flag keys.
    let rival = app.register("Rival Co", "eve@rival.test").await;

    // Listing shows nothing of Acme's.
    let projects = app.get("/api/v1/projects", Some(&rival)).await.expect(StatusCode::OK);
    assert_eq!(projects.as_array().unwrap().len(), 0);

    // Naming Acme's project directly is a 404, not a 403: confirming that a
    // project exists is itself a leak.
    for (method, uri) in [
        ("GET", "/api/v1/projects/checkout"),
        ("GET", "/api/v1/projects/checkout/flags"),
        ("GET", "/api/v1/projects/checkout/environments"),
        ("GET", "/api/v1/projects/checkout/flags/checkout.v2"),
        ("GET", "/api/v1/projects/checkout/environments/production/flags/checkout.v2"),
    ] {
        let response = match method {
            "GET" => app.get(uri, Some(&rival)).await,
            _ => unreachable!(),
        };
        assert_eq!(
            response.status,
            StatusCode::NOT_FOUND,
            "{method} {uri} leaked to another tenant"
        );
    }

    // Writes are refused too.
    let write = app.configure(&rival, "checkout.v2", simple_config(true, "on")).await;
    assert_eq!(write.status, StatusCode::NOT_FOUND);

    let delete = app.delete("/api/v1/projects/checkout", Some(&rival)).await;
    assert_eq!(delete.status, StatusCode::NOT_FOUND);

    // Acme's configuration is untouched.
    let still_there = app
        .get("/api/v1/projects/checkout/flags/checkout.v2", Some(&acme.token))
        .await
        .expect(StatusCode::OK);
    assert_eq!(still_there["key"], "checkout.v2");
}

#[sqlx::test(migrations = "../../migrations")]
async fn an_sdk_key_only_reaches_its_own_environment(pool: PgPool) {
    let app = TestApp::new(pool);
    let acme = app.bootstrap("Acme", "ada@acme.test").await;
    let rival = app.bootstrap("Rival Co", "eve@rival.test").await;

    app.create_flag(&acme.token, "acme-only").await;
    app.configure(&acme.token, "acme-only", simple_config(true, "on")).await.expect(StatusCode::OK);

    // Rival's key sees its own (empty) environment, never Acme's flag.
    let seen = app
        .post("/api/v1/evaluate", Some(&rival.sdk_key), json!({"context": {"key": "u"}}))
        .await
        .expect(StatusCode::OK);
    assert_eq!(seen["evaluations"].as_array().unwrap().len(), 0);

    let probed = app.evaluate(&rival.sdk_key, "acme-only", json!({"key": "u"})).await;
    assert_eq!(probed["reason"]["kind"], "flag_not_found");
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_client_scoped_key_can_evaluate_but_not_download_the_rules(pool: PgPool) {
    let app = TestApp::new(pool);
    let tenant = app.bootstrap("Acme", "ada@acme.test").await;
    app.create_flag(&tenant.token, "checkout.v2").await;

    let client_key = app
        .post(
            "/api/v1/projects/checkout/environments/production/keys",
            Some(&tenant.token),
            json!({"name": "web", "scope": "client"}),
        )
        .await
        .expect(StatusCode::CREATED)["secret"]
        .as_str()
        .unwrap()
        .to_owned();

    // Evaluating is fine.
    app.post("/api/v1/evaluate", Some(&client_key), json!({"context": {"key": "u"}}))
        .await
        .expect(StatusCode::OK);

    // Downloading the targeting rules is not: they name internal segments.
    let snapshot = app.get("/api/v1/snapshot", Some(&client_key)).await;
    assert_eq!(snapshot.status, StatusCode::FORBIDDEN);

    // A server key may.
    let allowed = app.get("/api/v1/snapshot", Some(&tenant.sdk_key)).await.expect(StatusCode::OK);
    assert!(allowed["flags"].is_object());
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_snapshot_never_carries_the_bucketing_salt(pool: PgPool) {
    let app = TestApp::new(pool);
    let tenant = app.bootstrap("Acme", "ada@acme.test").await;
    app.create_flag(&tenant.token, "checkout.v2").await;

    let snapshot = app.get("/api/v1/snapshot", Some(&tenant.sdk_key)).await.expect(StatusCode::OK);

    // Knowing the salt would let anyone precompute exactly who a rollout hits.
    assert!(snapshot.get("salt").is_none(), "{snapshot}");
    assert!(!snapshot.to_string().contains("salt"), "{snapshot}");

    let stored_salt: String =
        sqlx::query_scalar("SELECT salt FROM environments WHERE key = 'production'")
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert!(!snapshot.to_string().contains(&stored_salt));
}

#[sqlx::test(migrations = "../../migrations")]
async fn every_change_lands_in_the_audit_trail(pool: PgPool) {
    let app = TestApp::new(pool);
    let tenant = app.bootstrap("Acme", "ada@acme.test").await;
    app.create_flag(&tenant.token, "checkout.v2").await;
    app.configure(&tenant.token, "checkout.v2", simple_config(true, "on"))
        .await
        .expect(StatusCode::OK);

    let page = app.get("/api/v1/audit", Some(&tenant.token)).await.expect(StatusCode::OK);
    let entries = page["entries"].as_array().unwrap();

    let actions: Vec<&str> = entries.iter().map(|e| e["action"].as_str().unwrap()).collect();
    for expected in [
        "flag.configured",
        "flag.created",
        "api_key.created",
        "environment.created",
        "project.created",
    ] {
        assert!(actions.contains(&expected), "missing `{expected}` from {actions:?}");
    }

    // Newest first, and the actor is recorded.
    assert_eq!(entries[0]["action"], "flag.configured");
    assert_eq!(entries[0]["actor_email"], "ada@acme.test");

    // The before/after pair is what makes the trail useful during an incident.
    assert_eq!(entries[0]["before"]["enabled"], false);
    assert_eq!(entries[0]["after"]["enabled"], true);
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_audit_trail_is_scoped_to_one_organization(pool: PgPool) {
    let app = TestApp::new(pool);
    let acme = app.bootstrap("Acme", "ada@acme.test").await;
    let rival = app.bootstrap("Rival Co", "eve@rival.test").await;

    app.create_flag(&acme.token, "acme-secret-project").await;

    let page = app.get("/api/v1/audit", Some(&rival.token)).await.expect(StatusCode::OK);
    let rendered = page.to_string();

    assert!(!rendered.contains("acme-secret-project"), "{rendered}");
    assert!(!rendered.contains("ada@acme.test"), "{rendered}");
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_audit_trail_pages_with_a_cursor(pool: PgPool) {
    let app = TestApp::new(pool);
    let tenant = app.bootstrap("Acme", "ada@acme.test").await;

    for i in 0..8 {
        app.create_flag(&tenant.token, &format!("flag-{i}")).await;
    }

    let first = app.get("/api/v1/audit?limit=5", Some(&tenant.token)).await.expect(StatusCode::OK);
    assert_eq!(first["entries"].as_array().unwrap().len(), 5);

    let cursor = first["next_cursor"].as_i64().expect("a full page must offer a cursor");
    let second = app
        .get(&format!("/api/v1/audit?limit=5&before_id={cursor}"), Some(&tenant.token))
        .await
        .expect(StatusCode::OK);

    let first_ids: Vec<i64> =
        first["entries"].as_array().unwrap().iter().map(|e| e["id"].as_i64().unwrap()).collect();
    let second_ids: Vec<i64> =
        second["entries"].as_array().unwrap().iter().map(|e| e["id"].as_i64().unwrap()).collect();

    assert!(first_ids.iter().all(|id| !second_ids.contains(id)), "pages must not overlap");
    assert!(first_ids.iter().min() > second_ids.iter().max(), "pages must be ordered");
}

#[sqlx::test(migrations = "../../migrations")]
async fn deleting_a_project_takes_its_flags_with_it(pool: PgPool) {
    let app = TestApp::new(pool);
    let tenant = app.bootstrap("Acme", "ada@acme.test").await;
    app.create_flag(&tenant.token, "checkout.v2").await;

    app.delete("/api/v1/projects/checkout", Some(&tenant.token))
        .await
        .expect(StatusCode::NO_CONTENT);

    assert_eq!(
        app.get("/api/v1/projects/checkout", Some(&tenant.token)).await.status,
        StatusCode::NOT_FOUND
    );

    let orphans: i64 =
        sqlx::query_scalar("SELECT count(*) FROM flag_configs").fetch_one(&app.pool).await.unwrap();
    assert_eq!(orphans, 0, "cascading delete left configurations behind");
}

#[sqlx::test(migrations = "../../migrations")]
async fn health_reports_the_database_and_unmatched_routes_are_problem_json(pool: PgPool) {
    let app = TestApp::new(pool);

    let health = app.get("/health", None).await.expect(StatusCode::OK);
    assert_eq!(health["status"], "ok");

    let ready = app.get("/health/ready", None).await.expect(StatusCode::OK);
    assert_eq!(ready["database"], "up");

    let missing = app.get("/api/v1/nope", None).await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    assert_eq!(missing.error_kind(), "not_found");
}
