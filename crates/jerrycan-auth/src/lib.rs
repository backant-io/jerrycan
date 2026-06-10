//! Authentication for jerrycan: argon2 password hashing, AEAD session cookies,
//! HS256 JWTs, role guards. Vetted RustCrypto primitives; hand-rolled envelopes
//! (see module docs). #![forbid(unsafe_code)].
#![forbid(unsafe_code)]

use jerrycan_core::{App, Extension};
use sha2::{Digest, Sha256};

pub mod guard;
pub mod jwt;
pub mod password;
pub mod session;

pub use guard::{Bearer, Session, require_role};
pub use password::{hash_password, verify_password};
pub use session::SessionStore;

/// Minimum entropy for `JERRYCAN_SECRET`. Shorter secrets are rejected in prod.
pub(crate) const MIN_SECRET_LEN: usize = 32;

/// Derive a 32-byte subkey from the master secret and a domain label, so the
/// session key and the JWT key are independent even though one secret seeds both.
pub(crate) fn derive_key(secret: &[u8], label: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(secret);
    hasher.update(label.as_bytes());
    hasher.finalize().into()
}

/// The auth extension: holds the derived session + JWT keys, registered as a
/// dependency so `Session`/`Bearer` extractors can resolve it.
#[derive(Clone)]
pub struct Auth {
    sessions: SessionStore,
    jwt_key: [u8; 32],
}

impl Auth {
    /// Build from an explicit secret (>= 32 bytes recommended).
    pub fn with_secret(secret: &str) -> Self {
        let session_key = derive_key(secret.as_bytes(), "session");
        let jwt_key = derive_key(secret.as_bytes(), "jwt");
        Self {
            sessions: SessionStore::new(&session_key),
            jwt_key,
        }
    }

    /// Build from `JERRYCAN_SECRET`. In production (`JERRYCAN_ENV=prod`) a
    /// missing or short secret is a loud error; in dev it warns and uses a
    /// fixed dev key (NEVER use in production).
    pub fn from_env() -> jerrycan_core::Result<Self> {
        let is_prod = std::env::var("JERRYCAN_ENV").as_deref() == Ok("prod");
        match std::env::var("JERRYCAN_SECRET") {
            Ok(s) if s.len() >= MIN_SECRET_LEN => Ok(Self::with_secret(&s)),
            Ok(_) if is_prod => Err(jerrycan_core::Error::internal(format!(
                "JERRYCAN_SECRET must be at least {MIN_SECRET_LEN} bytes in production"
            ))),
            Err(_) if is_prod => Err(jerrycan_core::Error::internal(
                "JERRYCAN_SECRET is required in production (JERRYCAN_ENV=prod)",
            )),
            _ => {
                eprintln!(
                    "jerrycan-auth: WARNING using an insecure development secret; set JERRYCAN_SECRET (>= {MIN_SECRET_LEN} bytes) for production"
                );
                Ok(Self::with_secret(
                    "jerrycan-insecure-development-secret-do-not-use!!",
                ))
            }
        }
    }

    pub fn sessions(&self) -> &SessionStore {
        &self.sessions
    }

    pub fn jwt_key(&self) -> &[u8; 32] {
        &self.jwt_key
    }
}

impl Extension for Auth {
    fn register(self, app: App) -> App {
        app.provide(self)
    }
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
