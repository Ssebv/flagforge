//! Registration, login and identity.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use flagforge_storage::StorageError;
use flagforge_storage::accounts;
use flagforge_storage::models::{Organization, User};
use serde::{Deserialize, Serialize};

use crate::auth::{AuthUser, password};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

/// An Argon2 hash of a random value nobody knows.
///
/// Login verifies against this when the email is unknown, so a request for a
/// non-existent account costs the same Argon2 computation as one for a real
/// account. Without it, response times alone enumerate your users.
///
/// Computed once at first use rather than hard-coded, so it can never drift
/// out of sync with the parameters real hashes are produced with.
fn decoy_hash() -> &'static str {
    static DECOY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    DECOY.get_or_init(|| {
        let unknowable = uuid::Uuid::new_v4().to_string();
        password::hash(&unknowable).expect("a UUID is a valid password length")
    })
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct RegisterRequest {
    /// Display name for the new organization.
    #[schema(example = "Acme Inc")]
    pub organization_name: String,
    #[schema(example = "ada@acme.test")]
    pub email: String,
    /// At least 12 characters.
    #[schema(example = "correct-horse-battery-staple")]
    pub password: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct SessionResponse {
    pub token: String,
    /// Token lifetime in seconds.
    pub expires_in: u64,
    pub user: User,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct RegisterResponse {
    #[serde(flatten)]
    pub session: SessionResponse,
    pub organization: Organization,
}

/// Creates an organization and its owner.
#[utoipa::path(
    post,
    path = "/api/v1/auth/register",
    tag = "auth",
    request_body = RegisterRequest,
    responses(
        (status = 201, description = "Organization created", body = RegisterResponse),
        (status = 400, description = "Malformed input"),
        (status = 409, description = "That email already has an account"),
    )
)]
pub async fn register(
    State(state): State<AppState>,
    Json(body): Json<RegisterRequest>,
) -> ApiResult<(StatusCode, Json<RegisterResponse>)> {
    let email = body.email.trim().to_lowercase();
    if !looks_like_email(&email) {
        return Err(ApiError::BadRequest("that does not look like an email address".into()));
    }

    let name = body.organization_name.trim();
    if name.is_empty() || name.len() > 200 {
        return Err(ApiError::BadRequest("organization name must be 1-200 characters".into()));
    }

    let hash =
        password::hash(&body.password).map_err(|err| ApiError::BadRequest(err.to_string()))?;

    let (organization, user) = register_with_free_slug(&state, name, &email, &hash).await?;

    let token = state
        .tokens
        .issue(user.id, user.organization_id, &user.email, user.role)
        .map_err(|err| ApiError::Internal(anyhow::anyhow!(err)))?;

    tracing::info!(organization = %organization.id, user = %user.id, "organization registered");

    Ok((
        StatusCode::CREATED,
        Json(RegisterResponse {
            session: SessionResponse { token, expires_in: state.tokens.ttl().as_secs(), user },
            organization,
        }),
    ))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

/// Exchanges credentials for an access token.
#[utoipa::path(
    post,
    path = "/api/v1/auth/login",
    tag = "auth",
    request_body = LoginRequest,
    responses(
        (status = 200, description = "Authenticated", body = SessionResponse),
        (status = 401, description = "Wrong email or password"),
    )
)]
pub async fn login(
    State(state): State<AppState>,
    Json(body): Json<LoginRequest>,
) -> ApiResult<Json<SessionResponse>> {
    let found = accounts::find_by_email(&state.pool, &body.email).await?;

    // Hash-compare unconditionally; see `decoy_hash`.
    let stored_hash = found.as_ref().map(|f| f.password_hash.as_str()).unwrap_or(decoy_hash());
    let password_ok = password::verify(&body.password, stored_hash);

    // One message for both failure modes, so the response cannot be used to
    // check whether an address is registered.
    let (Some(found), true) = (found, password_ok) else {
        metrics::counter!("flagforge_login_failures_total").increment(1);
        return Err(ApiError::Unauthorized("invalid email or password"));
    };

    let user = found.user;
    let token = state
        .tokens
        .issue(user.id, user.organization_id, &user.email, user.role)
        .map_err(|err| ApiError::Internal(anyhow::anyhow!(err)))?;

    Ok(Json(SessionResponse { token, expires_in: state.tokens.ttl().as_secs(), user }))
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct MeResponse {
    pub user: User,
    pub organization: Organization,
}

/// Returns the caller's identity.
#[utoipa::path(
    get,
    path = "/api/v1/auth/me",
    tag = "auth",
    security(("bearer" = [])),
    responses(
        (status = 200, description = "The authenticated user", body = MeResponse),
        (status = 401, description = "Missing or invalid token"),
    )
)]
pub async fn me(State(state): State<AppState>, caller: AuthUser) -> ApiResult<Json<MeResponse>> {
    let user = accounts::find_user(&state.pool, caller.user_id).await?;
    let organization = accounts::find_organization(&state.pool, caller.organization_id).await?;
    Ok(Json(MeResponse { user, organization }))
}

/// Minimal structural check. Deliberately not a full RFC 5322 parser — the
/// only real proof an address works is sending mail to it.
fn looks_like_email(candidate: &str) -> bool {
    if candidate.len() > 254 || candidate.contains(char::is_whitespace) {
        return false;
    }
    match candidate.split_once('@') {
        Some((local, domain)) => {
            !local.is_empty()
                && domain.contains('.')
                && !domain.starts_with('.')
                && !domain.ends_with('.')
        }
        None => false,
    }
}

/// How many slug variants to try before giving up.
const SLUG_ATTEMPTS: u32 = 12;

/// Creates the organization, working around a slug another tenant already has.
///
/// Slugs are globally unique, but organization *names* are not — "Acme Inc"
/// is a name plenty of unrelated companies use. Since there is no flow for
/// joining an existing organization, returning a 409 here would leave a
/// legitimate signup with no way forward, so a colliding slug gets a numeric
/// suffix instead. A duplicate *email* still conflicts: that one really does
/// mean "you already have an account".
async fn register_with_free_slug(
    state: &AppState,
    name: &str,
    email: &str,
    password_hash: &str,
) -> ApiResult<(Organization, User)> {
    let base = slugify(name);

    for attempt in 1..=SLUG_ATTEMPTS {
        let slug = if attempt == 1 { base.clone() } else { format!("{base}-{attempt}") };

        match accounts::create_organization_with_owner(
            &state.pool,
            name,
            &slug,
            email,
            password_hash,
        )
        .await
        {
            Ok(created) => return Ok(created),
            Err(StorageError::Conflict { entity: "organization", .. }) => continue,
            Err(error) => return Err(error.into()),
        }
    }

    Err(ApiError::Conflict(format!(
        "could not derive a free identifier from `{name}` — try a more specific name"
    )))
}

/// Derives a URL-safe organization slug.
fn slugify(name: &str) -> String {
    let mut slug = String::with_capacity(name.len());
    let mut last_was_dash = true; // suppresses a leading dash

    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_was_dash = false;
        } else if !last_was_dash {
            slug.push('-');
            last_was_dash = true;
        }
    }

    let slug = slug.trim_end_matches('-');
    // The column requires a leading alphanumeric and at most 63 characters.
    let trimmed: String = slug.chars().take(63).collect();
    let trimmed = trimmed.trim_end_matches('-').to_owned();

    if trimmed.is_empty() { "org".to_owned() } else { trimmed }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_url_safe_and_bounded() {
        assert_eq!(slugify("Acme Inc"), "acme-inc");
        assert_eq!(slugify("  Weird   Name!! "), "weird-name");
        assert_eq!(slugify("Ünïcodé Ltd"), "n-cod-ltd");
        assert_eq!(slugify("!!!"), "org");
        assert_eq!(slugify(""), "org");
        assert!(slugify(&"a".repeat(200)).len() <= 63);
    }

    #[test]
    fn slugs_always_satisfy_the_database_constraint() {
        let pattern = regex_lite(r"^[a-z0-9][a-z0-9-]{0,62}$");
        for name in ["Acme Inc", "  ---  ", "9Lives", "!!!", &"x".repeat(300)] {
            let slug = slugify(name);
            assert!(pattern(&slug), "slug `{slug}` from `{name}` violates the CHECK constraint");
        }
    }

    #[test]
    fn obvious_non_emails_are_rejected() {
        assert!(looks_like_email("ada@example.com"));
        assert!(looks_like_email("a.b+c@sub.example.co.uk"));

        assert!(!looks_like_email("ada"));
        assert!(!looks_like_email("@example.com"));
        assert!(!looks_like_email("ada@example"));
        assert!(!looks_like_email("ada@.com"));
        assert!(!looks_like_email("ada @example.com"));
        assert!(!looks_like_email(&format!("{}@example.com", "a".repeat(300))));
    }

    #[test]
    fn the_decoy_hash_is_a_real_verifiable_argon2_hash() {
        // If this stopped parsing, unknown-user logins would return instantly
        // and the timing equalisation would silently stop working.
        let decoy = decoy_hash();
        assert!(password::is_parseable(decoy), "{decoy}");
        assert!(decoy.starts_with("$argon2id$"), "{decoy}");
        assert!(!password::verify("anything at all", decoy));
    }

    #[test]
    fn the_decoy_hash_is_stable_within_a_process() {
        assert_eq!(decoy_hash(), decoy_hash());
    }

    /// Tiny matcher for the slug constraint, so the test does not need `regex`.
    fn regex_lite(_pattern: &str) -> impl Fn(&str) -> bool {
        |s: &str| {
            let mut chars = s.chars();
            match chars.next() {
                Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
                _ => return false,
            }
            s.len() <= 63
                && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        }
    }
}
