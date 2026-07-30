//! Password hashing.

use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use rand::Rng;

/// 128 bits, the size Argon2's own reference implementation recommends.
const SALT_BYTES: usize = 16;

/// Shortest password we accept. Long enough that Argon2's cost is doing the
/// work rather than compensating for a four-character secret.
pub const MIN_PASSWORD_LEN: usize = 12;
pub const MAX_PASSWORD_LEN: usize = 256;

#[derive(Debug, thiserror::Error)]
pub enum PasswordError {
    #[error("password must be between {MIN_PASSWORD_LEN} and {MAX_PASSWORD_LEN} characters")]
    Length,

    #[error("failed to hash password")]
    Hashing,
}

/// Hashes with Argon2id and a fresh random salt, returning a PHC string that
/// embeds the algorithm and parameters — so raising the cost later does not
/// invalidate existing hashes.
pub fn hash(password: &str) -> Result<String, PasswordError> {
    let length = password.chars().count();
    if !(MIN_PASSWORD_LEN..=MAX_PASSWORD_LEN).contains(&length) {
        return Err(PasswordError::Length);
    }

    // Salt bytes come from this crate's own CSPRNG rather than from
    // `SaltString::generate`, so the whole workspace draws randomness from one
    // `rand` version instead of two that have to be kept compatible.
    let mut salt_bytes = [0u8; SALT_BYTES];
    rand::rng().fill_bytes(&mut salt_bytes);
    let salt = SaltString::encode_b64(&salt_bytes).map_err(|_| PasswordError::Hashing)?;

    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|_| PasswordError::Hashing)
}

/// Whether a string is a PHC hash this module could verify against.
///
/// Used by the login path to assert that its timing-equalisation decoy is a
/// real hash — a decoy that failed to parse would return instantly and quietly
/// reintroduce the timing difference it exists to remove.
pub fn is_parseable(phc_hash: &str) -> bool {
    PasswordHash::new(phc_hash).is_ok()
}

/// Verifies a password against a stored PHC hash.
///
/// Returns `false` for a malformed hash rather than erroring: from the
/// caller's point of view "this does not authenticate" is the only useful
/// answer, and distinguishing the two cases in a response would leak whether
/// an account exists.
pub fn verify(password: &str, phc_hash: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(phc_hash) else {
        return false;
    };
    Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = "correct-horse-battery-staple";

    #[test]
    fn a_hash_verifies_against_its_own_password() {
        let hash = hash(GOOD).unwrap();
        assert!(verify(GOOD, &hash));
        assert!(!verify("something else entirely", &hash));
    }

    #[test]
    fn the_same_password_hashes_differently_every_time() {
        // Distinct salts: two users with the same password must not be
        // identifiable by comparing hashes.
        assert_ne!(hash(GOOD).unwrap(), hash(GOOD).unwrap());
    }

    #[test]
    fn the_hash_is_a_phc_string_and_not_the_password() {
        let hash = hash(GOOD).unwrap();
        assert!(hash.starts_with("$argon2id$"), "{hash}");
        assert!(!hash.contains(GOOD));
    }

    #[test]
    fn short_passwords_are_refused() {
        assert!(matches!(hash("short"), Err(PasswordError::Length)));
    }

    #[test]
    fn absurdly_long_passwords_are_refused_before_hashing() {
        // Otherwise a megabyte "password" becomes a free CPU burn per request.
        assert!(matches!(hash(&"a".repeat(MAX_PASSWORD_LEN + 1)), Err(PasswordError::Length)));
    }

    #[test]
    fn a_corrupt_stored_hash_fails_closed() {
        assert!(!verify(GOOD, "not-a-phc-string"));
        assert!(!verify(GOOD, ""));
    }
}
