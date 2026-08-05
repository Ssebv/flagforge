//! The seeded demo, and the read-only account a public deployment publishes.
//!
//! These are the guarantees a live demo rests on: that the seed can run on
//! every deploy without failing one, and that the credentials printed in the
//! README cannot be used to dismantle the thing they are advertising.

mod harness;

use axum::http::StatusCode;
use flagforge_api::config::RuntimeEnvironment;
use flagforge_api::seed::{self, Credentials, DEV_OWNER_PASSWORD};
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

    seed::run(&pool, again(), RuntimeEnvironment::Development)
        .await
        .expect("the first seed should populate");
    seed::run(&pool, again(), RuntimeEnvironment::Development)
        .await
        .expect("the second seed should be a quiet no-op");

    let organizations: i64 =
        sqlx::query_scalar("SELECT count(*) FROM organizations").fetch_one(&pool).await.unwrap();
    assert_eq!(organizations, 1, "the second run duplicated the organization");
}

/// Without the flag it still refuses, because seeding a database that already
/// has data by hand is almost always a mistake.
#[sqlx::test(migrations = "../../migrations")]
async fn seeding_twice_without_the_flag_still_refuses(pool: PgPool) {
    seed::run(&pool, Credentials::default(), RuntimeEnvironment::Development).await.unwrap();

    let error = seed::run(&pool, Credentials::default(), RuntimeEnvironment::Development)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("already exists"), "{error}");
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_demo_viewer_can_read_production(pool: PgPool) {
    seed::run(&pool, Credentials::default(), RuntimeEnvironment::Development).await.unwrap();
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
    seed::run(&pool, Credentials::default(), RuntimeEnvironment::Development).await.unwrap();
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
    seed::run(&pool, Credentials::default(), RuntimeEnvironment::Development).await.unwrap();
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
    seed::run(&pool, Credentials::default(), RuntimeEnvironment::Development).await.unwrap();

    let app = TestApp::new(pool);
    let token = sign_in(&app, "ada@acme.test", DEV_OWNER_PASSWORD).await;

    app.configure(&token, "checkout.v2", simple_config(false, "off")).await.expect(StatusCode::OK);
}

/// The trap this closes: the development owner password is printed in the
/// README, so a deployment seeded with it hands its administrator account to
/// anyone who reads the repository — defeating the read-only viewer entirely.
#[sqlx::test(migrations = "../../migrations")]
async fn a_production_seed_does_not_use_the_documented_password(pool: PgPool) {
    seed::run(&pool, Credentials::default(), RuntimeEnvironment::Production).await.unwrap();

    let app = TestApp::new(pool);
    let refused = app
        .post(
            "/api/v1/auth/login",
            None,
            json!({"email": "ada@acme.test", "password": DEV_OWNER_PASSWORD}),
        )
        .await;
    assert_eq!(
        refused.status,
        StatusCode::UNAUTHORIZED,
        "the README's password opened a production deployment: {}",
        refused.body
    );

    // The viewer is still the published, working login.
    sign_in(&app, VIEWER.0, VIEWER.1).await;
}

/// An explicit password is still honoured everywhere, so an operator who wants
/// to pin one can.
#[sqlx::test(migrations = "../../migrations")]
async fn an_explicit_password_is_used_even_in_production(pool: PgPool) {
    let chosen = "a-password-the-operator-picked";
    let credentials = Credentials { password: Some(chosen.to_owned()), ..Credentials::default() };
    seed::run(&pool, credentials, RuntimeEnvironment::Production).await.unwrap();

    let app = TestApp::new(pool);
    sign_in(&app, "ada@acme.test", chosen).await;
}

// ------------------------------------------------------------- /metrics --

/// `/metrics` is the one route with no authentication, which is right on a
/// laptop and wrong on the public internet.
#[sqlx::test(migrations = "../../migrations")]
async fn metrics_stay_open_when_no_token_is_configured(pool: PgPool) {
    let app = TestApp::new(pool);
    app.get("/metrics", None).await.expect(StatusCode::OK);
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_configured_metrics_token_closes_the_endpoint(pool: PgPool) {
    let app = TestApp::with_metrics_token(pool, Some("scrape-me"));

    let anonymous = app.get("/metrics", None).await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED, "body was {}", anonymous.body);

    let wrong = app.get("/metrics", Some("nope")).await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED, "body was {}", wrong.body);

    app.get("/metrics", Some("scrape-me")).await.expect(StatusCode::OK);
}

/// A user's JWT must not double as a scrape credential, and vice versa — they
/// are separate secrets with separate lifetimes.
#[sqlx::test(migrations = "../../migrations")]
async fn a_user_token_does_not_open_metrics(pool: PgPool) {
    seed::run(&pool, Credentials::default(), RuntimeEnvironment::Development).await.unwrap();
    let app = TestApp::with_metrics_token(pool, Some("scrape-me"));
    let token = sign_in(&app, VIEWER.0, VIEWER.1).await;

    let refused = app.get("/metrics", Some(&token)).await;
    assert_eq!(refused.status, StatusCode::UNAUTHORIZED, "body was {}", refused.body);
}
