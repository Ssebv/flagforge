//! SDK key generation and lookup hashing.
//!
//! A password KDF would be the wrong tool here. These secrets are 256 bits of
//! CSPRNG output, so there is no low-entropy guess to slow down — and the
//! evaluation endpoint verifies one on every request, where an Argon2 hash per
//! call would dominate the latency budget. SHA-256 over full-entropy input is
//! both safe and fast.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use flagforge_storage::models::KeyScope;
use rand::Rng;
use sha2::{Digest, Sha256};

/// Every key starts with this, so a leaked secret is greppable in logs and
/// recognisable in a repository scan.
pub const KEY_PREFIX: &str = "ff";
/// Characters of the full key kept for display.
const DISPLAY_PREFIX_LEN: usize = 14;
/// 256 bits of entropy.
const SECRET_BYTES: usize = 32;

/// A freshly minted key. The secret exists only here — after this it is hashed
/// and the plaintext is unrecoverable, which is why creation responses are the
/// one and only chance to copy it.
#[derive(Debug, Clone)]
pub struct GeneratedKey {
    pub secret: String,
    pub hash: String,
    pub prefix: String,
}

pub fn generate(scope: KeyScope) -> GeneratedKey {
    let mut bytes = [0u8; SECRET_BYTES];
    rand::rng().fill_bytes(&mut bytes);

    let label = match scope {
        KeyScope::Server => "srv",
        KeyScope::Client => "cli",
    };
    let secret = format!("{KEY_PREFIX}_{label}_{}", URL_SAFE_NO_PAD.encode(bytes));

    GeneratedKey {
        hash: hash(&secret),
        prefix: secret.chars().take(DISPLAY_PREFIX_LEN).collect(),
        secret,
    }
}

/// Lookup hash for a presented secret.
pub fn hash(secret: &str) -> String {
    let digest = Sha256::digest(secret.as_bytes());
    // Hex rather than base64 so the value is safe to paste into psql.
    digest.iter().fold(String::with_capacity(64), |mut acc, byte| {
        use std::fmt::Write;
        let _ = write!(acc, "{byte:02x}");
        acc
    })
}

/// Extracts the scope a key claims, used to reject a client key on a
/// server-only route before touching the database.
pub fn claimed_scope(secret: &str) -> Option<KeyScope> {
    match secret.split('_').nth(1) {
        Some("srv") => Some(KeyScope::Server),
        Some("cli") => Some(KeyScope::Client),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn keys_are_scoped_and_recognisable() {
        let server = generate(KeyScope::Server);
        assert!(server.secret.starts_with("ff_srv_"));
        assert_eq!(claimed_scope(&server.secret), Some(KeyScope::Server));

        let client = generate(KeyScope::Client);
        assert!(client.secret.starts_with("ff_cli_"));
        assert_eq!(claimed_scope(&client.secret), Some(KeyScope::Client));
    }

    #[test]
    fn the_stored_prefix_reveals_nothing_useful() {
        let key = generate(KeyScope::Server);
        assert_eq!(key.prefix.len(), DISPLAY_PREFIX_LEN);
        assert!(key.secret.starts_with(&key.prefix));
        // The remaining entropy must not be derivable from what we store.
        assert!(key.secret.len() > key.prefix.len() + 20);
    }

    #[test]
    fn hashing_is_deterministic_and_one_way() {
        let key = generate(KeyScope::Server);
        assert_eq!(hash(&key.secret), key.hash);
        assert_eq!(key.hash.len(), 64);
        assert!(!key.hash.contains(&key.secret[7..]));
    }

    #[test]
    fn generated_keys_do_not_repeat() {
        let seen: HashSet<String> = (0..1_000).map(|_| generate(KeyScope::Server).secret).collect();
        assert_eq!(seen.len(), 1_000);
    }

    #[test]
    fn an_unrecognised_key_has_no_scope() {
        assert_eq!(claimed_scope("bearer-token"), None);
        assert_eq!(claimed_scope("ff_bogus_abc"), None);
        assert_eq!(claimed_scope(""), None);
    }
}
