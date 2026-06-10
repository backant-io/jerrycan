//! Password hashing via argon2 (RustCrypto). We never invent crypto — argon2
//! does the KDF; we expose a thin, misuse-resistant pair.

use jerrycan_core::{Error, Result};

use argon2::Argon2;
use argon2::password_hash::{
    PasswordHash, PasswordHasher, PasswordVerifier, SaltString, rand_core::OsRng,
};

/// Hash a password into a PHC string (`$argon2id$...`), random salt per call.
pub fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| Error::internal(format!("password hash failed: {e}")))
}

/// Verify a password against a stored PHC string. `Ok(false)` = mismatch;
/// `Err` = the stored hash is malformed (operator/data problem, not a guess).
pub fn verify_password(password: &str, phc: &str) -> Result<bool> {
    let parsed = PasswordHash::new(phc)
        .map_err(|e| Error::internal(format!("stored hash is malformed: {e}")))?;
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_then_verify_round_trips() {
        let hash = hash_password("correct horse").unwrap();
        assert!(hash.starts_with("$argon2"), "PHC string: {hash}");
        assert!(verify_password("correct horse", &hash).unwrap());
        assert!(!verify_password("wrong", &hash).unwrap());
    }

    #[test]
    fn hashes_are_salted_and_unique() {
        let a = hash_password("same").unwrap();
        let b = hash_password("same").unwrap();
        assert_ne!(a, b, "random salt per hash");
        assert!(verify_password("same", &a).unwrap());
        assert!(verify_password("same", &b).unwrap());
    }

    #[test]
    fn a_malformed_hash_is_an_error_not_a_panic() {
        assert!(verify_password("x", "not-a-phc-string").is_err());
    }
}
