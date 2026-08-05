//! The seeded demo, and the read-only account a public deployment publishes.
//!
//! These are the guarantees a live demo rests on: that the seed can run on
//! every deploy without failing one, and that the credentials printed in the
//! README cannot be used to dismantle the thing they are advertising.

mod harness;

use axum::http::StatusCode;
use flagforge_api::seed::{self, Credentials};
use harness::{TestApp, simple_config};
use serde_json::json;
use sqlx::PgPool;

const VIEWER: (&str, &str) = ("viewer@acme.test", "read-only-demo-account");

async fn sign_in(app: &TestApp, email: &str, password: &str) -> String {
    let body = app
        .post("/api/v1/auth/login", None, json!({"email": email, "password": password}))
        .await
        .expect(StatusCode::OK);
    body["token"].as_str().unwrap().to_owned()
}

/// The release command runs on every deploy, so the second one has to be a
/// no-op rather than a failure — otherwise the first redeploy breaks.
#[sqlx::test(migrations = "../../migrations")]
async fn seeding_twice_with_if_empty_is_a_no_op(pool: PgPool) {
    let again = || Credentials { if_empty: true, ..Credentials::default() };

    seed::run(&pool, again()).await.expect("the first seed should populate");
    seed::run(&pool, again()).await.expect("the second seed should be a quiet no-op");

    let organizations: i64 =
        sqlx::query_scalar("SELECT count(*) FROM organizations").fetch_one(&pool).await.unwrap();
    assert_eq!(organizations, 1, "the second run duplicated the organization");
}

/// Without the flag it still refuses, because seeding a database that already
/// has data by hand is almost always a mistake.
#[sqlx::test(migrations = "../../migrations")]
async fn seeding_twice_without_the_flag_still_refuses(pool: PgPool) {
    seed::run(&pool, Credentials::default()).await.unwrap();

    let error = seed::run(&pool, Credentials::default()).await.unwrap_err();
    assert!(error.to_string().contains("already exists"), "{error}");
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_demo_viewer_can_read_production(pool: PgPool) {
    seed::run(&pool, Credentials::default()).await.unwrap();
    let app = TestApp::new(pool);
    let token = sign_in(&app, VIEWER.0, VIEWER.1).await;

    let flags = app
        .get("/api/v1/projects/checkout/environments/production/flags", Some(&token))
        .await
        .expect(StatusCode::OK);
    assert!(
        flags.as_array().is_some_and(|list| !list.is_empty()),
        "the demo account must actually see the demo: {flags}"
    );

    // Segments too — the page the newest feature lives on.
    let segments = app
        .get("/api/v1/projects/checkout/environments/production/segments", Some(&token))
        .await
        .expect(StatusCode::OK);
    assert!(segments.as_array().is_some_and(|list| !list.is_empty()));
}

/// The whole reason the published account is a viewer: a visitor must not be
/// able to turn a flag off, edit an audience, or delete the project.
#[sqlx::test(migrations = "../../migrations")]
async fn the_demo_viewer_cannot_change_anything(pool: PgPool) {
    seed::run(&pool, Credentials::default()).await.unwrap();
    let app = TestApp::new(pool);
    let token = sign_in(&app, VIEWER.0, VIEWER.1).await;

    let refused = app.configure(&token, "checkout.v2", simple_config(false, "off")).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "body was {}", refused.body);

    let segment = app
        .put(
            "/api/v1/projects/checkout/environments/production/segments/beta-testers",
            Some(&token),
            json!({"included": ["anyone"]}),
        )
        .await;
    assert_eq!(segment.status, StatusCode::FORBIDDEN, "body was {}", segment.body);

    let project = app.delete("/api/v1/projects/checkout", Some(&token)).await;
    assert_eq!(project.status, StatusCode::FORBIDDEN, "body was {}", project.body);

    let flag = app
        .post(
            "/api/v1/projects/checkout/flags",
            Some(&token),
            json!({"key": "sneaky", "name": "Sneaky"}),
        )
        .await;
    assert_eq!(flag.status, StatusCode::FORBIDDEN, "body was {}", flag.body);
}

/// Minting an SDK key is an administrative act, and a key is a credential that
/// outlives the session that made it.
#[sqlx::test(migrations = "../../migrations")]
async fn the_demo_viewer_cannot_mint_an_sdk_key(pool: PgPool) {
    seed::run(&pool, Credentials::default()).await.unwrap();
    let app = TestApp::new(pool);
    let token = sign_in(&app, VIEWER.0, VIEWER.1).await;

    let refused = app
        .post(
            "/api/v1/projects/checkout/environments/production/keys",
            Some(&token),
            json!({"name": "mine", "scope": "server"}),
        )
        .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "body was {}", refused.body);
}

/// The owner still works — the viewer is an addition, not a replacement.
#[sqlx::test(migrations = "../../migrations")]
async fn the_seeded_owner_can_still_write(pool: PgPool) {
    let credentials = Credentials::default();
    let (email, password) = (credentials.email.clone(), credentials.password.clone());
    seed::run(&pool, credentials).await.unwrap();

    let app = TestApp::new(pool);
    let token = sign_in(&app, &email, &password).await;

    app.configure(&token, "checkout.v2", simple_config(false, "off")).await.expect(StatusCode::OK);
}
