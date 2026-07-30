//! Access tokens for the management API.

use std::time::Duration;

use chrono::Utc;
use flagforge_storage::models::Role;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    #[error("token is expired")]
    Expired,
    #[error("token is not valid")]
    Invalid,
    #[error("failed to issue token")]
    Issuance,
}

/// JWT payload.
///
/// Carries the organization and role so authorization decisions need no
/// database round trip; anything longer-lived than the token TTL (like a
/// revoked user) is deliberately *not* represented here — that is what the
/// short TTL is for.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    /// User id.
    pub sub: Uuid,
    pub org: Uuid,
    pub email: String,
    pub role: Role,
    /// Issued-at, seconds since the epoch.
    pub iat: i64,
    /// Expiry, seconds since the epoch.
    pub exp: i64,
    /// Unique token id, so a specific token can be traced through logs.
    pub jti: Uuid,
}

/// Issues and verifies tokens with a single symmetric key.
#[derive(Clone)]
pub struct TokenIssuer {
    encoding: EncodingKey,
    decoding: DecodingKey,
    validation: Validation,
    ttl: Duration,
}

// The keys hold secret material; keep them out of any accidental `{:?}`.
impl std::fmt::Debug for TokenIssuer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenIssuer").field("ttl", &self.ttl).finish_non_exhaustive()
    }
}

impl TokenIssuer {
    pub fn new(secret: &str, ttl: Duration) -> Self {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.set_required_spec_claims(&["exp", "sub"]);
        // No clock skew allowance: every node runs NTP, and a generous leeway
        // is just a longer window for a stolen token.
        validation.leeway = 0;

        Self {
            encoding: EncodingKey::from_secret(secret.as_bytes()),
            decoding: DecodingKey::from_secret(secret.as_bytes()),
            validation,
            ttl,
        }
    }

    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    pub fn issue(
        &self,
        user_id: Uuid,
        organization_id: Uuid,
        email: &str,
        role: Role,
    ) -> Result<String, TokenError> {
        let now = Utc::now().timestamp();
        let claims = Claims {
            sub: user_id,
            org: organization_id,
            email: email.to_owned(),
            role,
            iat: now,
            exp: now + self.ttl.as_secs() as i64,
            jti: Uuid::new_v4(),
        };

        jsonwebtoken::encode(&Header::new(Algorithm::HS256), &claims, &self.encoding)
            .map_err(|_| TokenError::Issuance)
    }

    pub fn verify(&self, token: &str) -> Result<Claims, TokenError> {
        use jsonwebtoken::errors::ErrorKind;

        jsonwebtoken::decode::<Claims>(token, &self.decoding, &self.validation)
            .map(|data| data.claims)
            .map_err(|err| match err.kind() {
                ErrorKind::ExpiredSignature => TokenError::Expired,
                _ => TokenError::Invalid,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issuer(ttl_secs: u64) -> TokenIssuer {
        TokenIssuer::new("a-test-secret-that-is-long-enough-000000", Duration::from_secs(ttl_secs))
    }

    #[test]
    fn a_token_round_trips_its_claims() {
        let issuer = issuer(60);
        let user = Uuid::new_v4();
        let org = Uuid::new_v4();

        let token = issuer.issue(user, org, "ada@example.com", Role::Admin).unwrap();
        let claims = issuer.verify(&token).unwrap();

        assert_eq!(claims.sub, user);
        assert_eq!(claims.org, org);
        assert_eq!(claims.role, Role::Admin);
        assert_eq!(claims.email, "ada@example.com");
    }

    #[test]
    fn a_token_signed_with_another_secret_is_rejected() {
        let token = issuer(60).issue(Uuid::new_v4(), Uuid::new_v4(), "a@b.c", Role::Owner).unwrap();
        let other =
            TokenIssuer::new("a-completely-different-secret-0000000000", Duration::from_secs(60));

        assert!(matches!(other.verify(&token), Err(TokenError::Invalid)));
    }

    #[test]
    fn tampering_with_the_payload_invalidates_the_signature() {
        let issuer = issuer(60);
        let token = issuer.issue(Uuid::new_v4(), Uuid::new_v4(), "a@b.c", Role::Viewer).unwrap();

        // Flip a character in the payload segment.
        let mut parts: Vec<&str> = token.split('.').collect();
        let payload = parts[1].to_owned();
        let tampered = format!("{}X", &payload[..payload.len() - 1]);
        parts[1] = &tampered;

        assert!(issuer.verify(&parts.join(".")).is_err());
    }

    #[test]
    fn an_expired_token_is_reported_as_expired() {
        // A zero-second TTL is already in the past by the time we verify.
        let issuer = issuer(0);
        let token = issuer.issue(Uuid::new_v4(), Uuid::new_v4(), "a@b.c", Role::Member).unwrap();
        std::thread::sleep(Duration::from_millis(1100));

        assert!(matches!(issuer.verify(&token), Err(TokenError::Expired)));
    }

    #[test]
    fn garbage_is_not_a_token() {
        assert!(issuer(60).verify("not.a.token").is_err());
        assert!(issuer(60).verify("").is_err());
    }

    #[test]
    fn debug_output_does_not_contain_the_signing_key() {
        let rendered = format!("{:?}", issuer(60));
        assert!(!rendered.contains("secret"), "{rendered}");
    }
}
