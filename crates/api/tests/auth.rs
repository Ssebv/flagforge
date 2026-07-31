//! Registration, login and credential separation.

mod harness;

use axum::http::StatusCode;
use harness::TestApp;
use serde_json::json;
use sqlx::PgPool;

const MIGRATIONS: &str = "../../migrations";

#[sqlx::test(migrations = "../../migrations")]
async fn registering_creates_an_organization_and_returns_a_usable_token(pool: PgPool) {
    let app = TestApp::new(pool);

    let body = app
        .post(
            "/api/v1/auth/register",
            None,
            json!({
                "organization_name": "Acme Inc",
                "email": "Ada@Acme.test",
                "password": "correct-horse-battery-staple",
            }),
        )
        .await
        .expect(StatusCode::CREATED);

    assert_eq!(body["organization"]["slug"], "acme-inc");
    assert_eq!(body["user"]["role"], "owner");
    // Addresses are normalised, so `Ada@` and `ada@` are one account.
    assert_eq!(body["user"]["email"], "ada@acme.test");

    let token = body["token"].as_str().unwrap();
    let me = app.get("/api/v1/auth/me", Some(token)).await.expect(StatusCode::OK);
    assert_eq!(me["user"]["email"], "ada@acme.test");
    assert_eq!(me["organization"]["name"], "Acme Inc");
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_registration_response_never_contains_the_password_or_its_hash(pool: PgPool) {
    let app = TestApp::new(pool);

    let body = app
        .post(
            "/api/v1/auth/register",
            None,
            json!({
                "organization_name": "Acme",
                "email": "ada@acme.test",
                "password": "correct-horse-battery-staple",
            }),
        )
        .await
        .expect(StatusCode::CREATED);

    let rendered = body.to_string();
    assert!(!rendered.contains("correct-horse"), "{rendered}");
    assert!(!rendered.contains("argon2"), "{rendered}");
    assert!(!rendered.contains("password"), "{rendered}");
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_same_email_cannot_register_twice(pool: PgPool) {
    let app = TestApp::new(pool);
    let payload = json!({
        "organization_name": "Acme",
        "email": "ada@acme.test",
        "password": "correct-horse-battery-staple",
    });

    app.post("/api/v1/auth/register", None, payload.clone()).await.expect(StatusCode::CREATED);

    let second = app.post("/api/v1/auth/register", None, payload).await;
    assert_eq!(second.status, StatusCode::CONFLICT);
}

#[sqlx::test(migrations = "../../migrations")]
async fn two_unrelated_companies_may_share_a_name(pool: PgPool) {
    let app = TestApp::new(pool);

    let first = app
        .post(
            "/api/v1/auth/register",
            None,
            json!({
                "organization_name": "Acme Inc",
                "email": "ada@acme.test",
                "password": "correct-horse-battery-staple",
            }),
        )
        .await
        .expect(StatusCode::CREATED);

    // Plenty of unrelated companies are called the same thing, and there is no
    // flow for joining someone else's organization — so a taken slug must not
    // be a dead end at signup.
    let second = app
        .post(
            "/api/v1/auth/register",
            None,
            json!({
                "organization_name": "Acme Inc",
                "email": "eve@other.test",
                "password": "correct-horse-battery-staple",
            }),
        )
        .await
        .expect(StatusCode::CREATED);

    assert_eq!(first["organization"]["slug"], "acme-inc");
    assert_eq!(second["organization"]["slug"], "acme-inc-2");
    assert_ne!(first["organization"]["id"], second["organization"]["id"]);
}

#[sqlx::test(migrations = "../../migrations")]
async fn login_accepts_the_right_password_and_refuses_the_wrong_one(pool: PgPool) {
    let app = TestApp::new(pool);
    app.register("Acme", "ada@acme.test").await;

    let ok = app
        .post(
            "/api/v1/auth/login",
            None,
            json!({"email": "ada@acme.test", "password": "correct-horse-battery-staple"}),
        )
        .await
        .expect(StatusCode::OK);
    assert!(ok["token"].as_str().is_some_and(|t| !t.is_empty()));

    let wrong = app
        .post(
            "/api/v1/auth/login",
            None,
            json!({"email": "ada@acme.test", "password": "not-the-right-password"}),
        )
        .await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "../../migrations")]
async fn login_does_not_reveal_whether_an_account_exists(pool: PgPool) {
    let app = TestApp::new(pool);
    app.register("Acme", "ada@acme.test").await;

    let wrong_password = app
        .post(
            "/api/v1/auth/login",
            None,
            json!({"email": "ada@acme.test", "password": "wrong-password-entirely"}),
        )
        .await;

    let no_such_user = app
        .post(
            "/api/v1/auth/login",
            None,
            json!({"email": "nobody@acme.test", "password": "wrong-password-entirely"}),
        )
        .await;

    // Identical status *and* identical body: anything else is an account
    // enumeration oracle.
    assert_eq!(wrong_password.status, no_such_user.status);
    assert_eq!(wrong_password.body, no_such_user.body);
}

#[sqlx::test(migrations = "../../migrations")]
async fn short_passwords_are_rejected_at_registration(pool: PgPool) {
    let app = TestApp::new(pool);

    let response = app
        .post(
            "/api/v1/auth/register",
            None,
            json!({"organization_name": "Acme", "email": "ada@acme.test", "password": "short"}),
        )
        .await;

    assert_eq!(response.status, StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_management_api_requires_a_token(pool: PgPool) {
    let app = TestApp::new(pool);

    assert_eq!(app.get("/api/v1/projects", None).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        app.get("/api/v1/projects", Some("obviously-not-a-jwt")).await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_two_credential_types_are_not_interchangeable(pool: PgPool) {
    let app = TestApp::new(pool);
    let tenant = app.bootstrap("Acme", "ada@acme.test").await;

    // An SDK key must not reach the management API...
    let management = app.get("/api/v1/projects", Some(&tenant.sdk_key)).await;
    assert_eq!(management.status, StatusCode::UNAUTHORIZED);

    // ...and a user token must not be accepted as an SDK key.
    let evaluation =
        app.post("/api/v1/evaluate", Some(&tenant.token), json!({"context": {"key": "u"}})).await;
    assert_eq!(evaluation.status, StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_revoked_sdk_key_stops_working_immediately(pool: PgPool) {
    let app = TestApp::new(pool);
    let tenant = app.bootstrap("Acme", "ada@acme.test").await;
    app.create_flag(&tenant.token, "checkout.v2").await;

    app.post("/api/v1/evaluate", Some(&tenant.sdk_key), json!({"context": {"key": "u"}}))
        .await
        .expect(StatusCode::OK);

    app.delete(
        &format!("/api/v1/projects/checkout/environments/production/keys/{}", tenant.sdk_key_id),
        Some(&tenant.token),
    )
    .await
    .expect(StatusCode::NO_CONTENT);

    let after =
        app.post("/api/v1/evaluate", Some(&tenant.sdk_key), json!({"context": {"key": "u"}})).await;
    assert_eq!(after.status, StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_sdk_key_secret_is_returned_exactly_once(pool: PgPool) {
    let app = TestApp::new(pool);
    let tenant = app.bootstrap("Acme", "ada@acme.test").await;

    let listed = app
        .get("/api/v1/projects/checkout/environments/production/keys", Some(&tenant.token))
        .await
        .expect(StatusCode::OK);

    let rendered = listed.to_string();
    assert!(!rendered.contains(&tenant.sdk_key), "listing a key must not disclose its secret");
    // The prefix is enough to identify it in a UI.
    assert!(rendered.contains(&tenant.sdk_key[..14]));
}

#[sqlx::test(migrations = "../../migrations")]
async fn migrations_are_embedded_in_the_binary(pool: PgPool) {
    // `MIGRATOR` is what a container uses to bring up its own schema; if the
    // embedded path ever stopped matching the directory, this would be the
    // only place it showed up before deploy time.
    assert!(!flagforge_storage::MIGRATOR.migrations.is_empty());
    assert!(std::path::Path::new(MIGRATIONS).exists());
    assert!(flagforge_storage::pool::ping(&pool).await);
}
