//! Authentication extractors.
//!
//! Two credentials coexist. Humans and CI hold a short-lived JWT and reach the
//! management API; SDKs hold a long-lived key scoped to exactly one
//! environment and can only evaluate. Keeping them in separate extractors
//! means an SDK key can never be accepted where a user token is expected, even
//! by mistake.

use axum::extract::FromRequestParts;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use flagforge_storage::api_keys;
use flagforge_storage::models::{KeyIdentity, KeyScope, Role};
use uuid::Uuid;

use crate::auth::jwt::TokenError;
use crate::auth::keys;
use crate::error::ApiError;
use crate::state::AppState;

/// An authenticated human (or machine acting as one).
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub user_id: Uuid,
    pub organization_id: Uuid,
    pub email: String,
    pub role: Role,
}

impl AuthUser {
    /// The `(id, email)` pair every audit record needs.
    pub fn actor(&self) -> (Option<Uuid>, &str) {
        (Some(self.user_id), self.email.as_str())
    }

    pub fn require_write(&self) -> Result<(), ApiError> {
        if self.role.can_write() {
            Ok(())
        } else {
            Err(ApiError::Forbidden("this role cannot modify configuration"))
        }
    }

    pub fn require_admin(&self) -> Result<(), ApiError> {
        if self.role.can_administer() {
            Ok(())
        } else {
            Err(ApiError::Forbidden("this action requires an owner or admin"))
        }
    }
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = bearer(parts).ok_or(ApiError::Unauthorized("missing bearer token"))?;

        // An SDK key presented here is a category error, and saying so beats a
        // confusing "invalid token".
        if keys::claimed_scope(token).is_some() {
            return Err(ApiError::Unauthorized(
                "this endpoint requires a user token, not an SDK key",
            ));
        }

        let claims = state.tokens.verify(token).map_err(|err| match err {
            TokenError::Expired => ApiError::Unauthorized("token has expired"),
            _ => ApiError::Unauthorized("invalid token"),
        })?;

        Ok(Self {
            user_id: claims.sub,
            organization_id: claims.org,
            email: claims.email,
            role: claims.role,
        })
    }
}

/// An authenticated SDK key, already resolved to its environment.
#[derive(Debug, Clone)]
pub struct SdkIdentity(pub KeyIdentity);

impl SdkIdentity {
    pub fn environment_id(&self) -> Uuid {
        self.0.environment_id
    }

    pub fn scope(&self) -> KeyScope {
        self.0.scope
    }
}

impl FromRequestParts<AppState> for SdkIdentity {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let presented = bearer(parts)
            .or_else(|| parts.headers.get("x-flagforge-key").and_then(|v| v.to_str().ok()))
            .ok_or(ApiError::Unauthorized("missing SDK key"))?;

        // Reject anything not shaped like one of our keys before hashing it,
        // so a stray user token never becomes a database lookup.
        if keys::claimed_scope(presented).is_none() {
            return Err(ApiError::Unauthorized("not a valid SDK key"));
        }

        let identity = api_keys::resolve(&state.pool, &keys::hash(presented))
            .await?
            .ok_or(ApiError::Unauthorized("unknown or revoked SDK key"))?;

        // Usage tracking must never delay or fail the request it describes.
        let pool = state.pool.clone();
        let key_id = identity.api_key_id;
        tokio::spawn(async move {
            if let Err(error) = api_keys::touch(&pool, key_id).await {
                tracing::debug!(%error, "could not record SDK key usage");
            }
        });

        Ok(Self(identity))
    }
}

fn bearer(parts: &Parts) -> Option<&str> {
    let value = parts.headers.get(AUTHORIZATION)?.to_str().ok()?;
    let (scheme, credential) = value.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then(|| credential.trim())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;

    fn parts_with(header: &str, value: &str) -> Parts {
        Request::builder().header(header, value).body(()).unwrap().into_parts().0
    }

    #[test]
    fn bearer_parsing_is_scheme_insensitive_and_trims() {
        assert_eq!(bearer(&parts_with("authorization", "Bearer abc")), Some("abc"));
        assert_eq!(bearer(&parts_with("authorization", "bearer  abc ")), Some("abc"));
        assert_eq!(bearer(&parts_with("authorization", "BEARER abc")), Some("abc"));
    }

    #[test]
    fn other_schemes_are_not_accepted() {
        assert_eq!(bearer(&parts_with("authorization", "Basic abc")), None);
        assert_eq!(bearer(&parts_with("authorization", "abc")), None);
    }

    #[test]
    fn roles_gate_the_right_actions() {
        let viewer = AuthUser {
            user_id: Uuid::nil(),
            organization_id: Uuid::nil(),
            email: String::new(),
            role: Role::Viewer,
        };
        assert!(viewer.require_write().is_err());
        assert!(viewer.require_admin().is_err());

        let member = AuthUser { role: Role::Member, ..viewer.clone() };
        assert!(member.require_write().is_ok());
        assert!(member.require_admin().is_err());

        let owner = AuthUser { role: Role::Owner, ..viewer };
        assert!(owner.require_write().is_ok());
        assert!(owner.require_admin().is_ok());
    }
}
