//! Authentication for jerrycan: argon2 password hashing, AEAD session cookies,
//! HS256 JWTs, role guards. Vetted RustCrypto primitives; hand-rolled envelopes
//! (see module docs). #![forbid(unsafe_code)].
#![forbid(unsafe_code)]

use sha2::{Digest, Sha256};

pub mod password;

pub use password::{hash_password, verify_password};

/// Minimum entropy for `JERRYCAN_SECRET`. Shorter secrets are rejected in prod.
// Consumed by `Auth::from_env` in a later task; reserved here with the key derivation.
#[allow(dead_code)]
pub(crate) const MIN_SECRET_LEN: usize = 32;

/// Derive a 32-byte subkey from the master secret and a domain label, so the
/// session key and the JWT key are independent even though one secret seeds both.
// Used by `secret_tests` now; by the session/jwt stores in later tasks.
#[allow(dead_code)]
pub(crate) fn derive_key(secret: &[u8], label: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(secret);
    hasher.update(label.as_bytes());
    hasher.finalize().into()
}

#[cfg(test)]
mod secret_tests {
    use super::*;

    #[test]
    fn derived_keys_are_label_separated() {
        let s = b"a-very-long-development-secret-string!!";
        assert_ne!(derive_key(s, "session"), derive_key(s, "jwt"));
        assert_eq!(derive_key(s, "session"), derive_key(s, "session"));
    }
}
