//! Password hashing via argon2 (RustCrypto). We never invent crypto — argon2
//! does the KDF; we expose a thin, misuse-resistant pair.
//! bcrypt is verify-only, for Supabase-migrated users; argon2 is the only hash
//! we mint.

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

/// True for a bcrypt PHC string (`$2a$` / `$2b$` / `$2y$`) — the hash format
/// Supabase (GoTrue) stores. jerrycan verifies these for migrated users but
/// never mints them.
fn is_bcrypt(phc: &str) -> bool {
    phc.starts_with("$2a$") || phc.starts_with("$2b$") || phc.starts_with("$2y$")
}

/// Verify a password against a stored hash: argon2 (`$argon2*`, the native
/// format) or bcrypt (`$2a$/$2b$/$2y$`, Supabase-migrated users). `Ok(false)`
/// = mismatch; `Err` = the stored hash is malformed (operator/data problem,
/// not a guess).
pub fn verify_password(password: &str, phc: &str) -> Result<bool> {
    // SECURITY: the error message is a client-visible 500 body on the login
    // path, and bcrypt's InvalidHash Display embeds the FULL stored hash —
    // detail goes to stderr for the operator, the client gets a generic
    // message (the jerrycan-db db_error convention).
    if is_bcrypt(phc) {
        return bcrypt::verify(password, phc).map_err(|e| {
            eprintln!("jerrycan-auth: stored bcrypt hash is malformed: {e}");
            Error::internal("stored password hash is malformed")
        });
    }
    let parsed = PasswordHash::new(phc).map_err(|e| {
        eprintln!("jerrycan-auth: stored hash is malformed: {e}");
        Error::internal("stored password hash is malformed")
    })?;
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

/// Should this stored hash be transparently upgraded on the next successful
/// login? True for bcrypt (migrated users): after `verify_password` returns
/// `Ok(true)`, call [`hash_password`] and persist the argon2 result — the user
/// never notices, and the bcrypt hash retires itself.
pub fn needs_rehash(phc: &str) -> bool {
    is_bcrypt(phc)
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

    #[test]
    fn bcrypt_hashes_verify_for_migrated_users() {
        // WHY: lossless Supabase migration — users must log in with their
        // EXISTING passwords, and Supabase stores bcrypt. Round-trip through a
        // real bcrypt hash (cost 4 keeps the test fast).
        let phc = bcrypt::hash("hunter2", 4).unwrap();
        assert!(phc.starts_with("$2"), "bcrypt PHC: {phc}");
        assert!(verify_password("hunter2", &phc).unwrap());
        assert!(!verify_password("wrong", &phc).unwrap());
    }

    #[test]
    fn a_known_bcrypt_vector_verifies_across_prefix_variants() {
        // The widely-published bcrypt hash of "password" (cost 10). Supabase
        // emits $2a$/$2b$; PHP-era exports use $2y$ — all three must verify.
        let base = "$2a$10$XA0qRPpPalpuJbU.To.aIubemyIlzNCQ4Yq9badxgru8fu7JNaxqW";
        assert!(verify_password("password", base).unwrap());
        assert!(!verify_password("not-password", base).unwrap());
        let two_y = base.replacen("$2a$", "$2y$", 1);
        assert!(verify_password("password", &two_y).unwrap());
    }

    #[test]
    fn malformed_bcrypt_is_an_error_not_a_panic_or_a_false() {
        // A $2-prefixed non-hash is an operator/data problem — surfaced, not
        // silently treated as a wrong password.
        assert!(verify_password("x", "$2b$not-a-real-hash").is_err());
    }

    #[test]
    fn malformed_hash_errors_never_leak_the_stored_hash() {
        // WHY (security): bcrypt's InvalidHash error Display embeds the FULL
        // stored hash string, and verify_password's error message is a
        // client-visible 500 body on the login path. Hash material (even
        // malformed) must go to stderr only — the client gets a generic
        // message (the jerrycan-db db_error convention).
        let bcrypt_ish = "$2b$10$SECRET-HASH-MATERIAL-THAT-MUST-NOT-LEAK";
        let err = verify_password("x", bcrypt_ish).unwrap_err();
        assert!(
            !err.message().contains("SECRET-HASH-MATERIAL"),
            "stored hash leaked to the client: {}",
            err.message()
        );
        let err = verify_password("x", "$argon2id$corrupt$SECRETSALTMATERIAL").unwrap_err();
        assert!(
            !err.message().contains("SECRETSALTMATERIAL"),
            "stored hash leaked to the client: {}",
            err.message()
        );
    }

    #[test]
    fn needs_rehash_flags_bcrypt_but_never_argon2() {
        // WHY: the transparent-upgrade path — a login handler re-hashes to
        // argon2 after a successful bcrypt verify, exactly once.
        let bcrypt_phc = bcrypt::hash("pw", 4).unwrap();
        assert!(needs_rehash(&bcrypt_phc));
        let argon = hash_password("pw").unwrap();
        assert!(!needs_rehash(&argon));
    }

    #[test]
    fn argon2_path_is_unchanged() {
        let hash = hash_password("correct horse").unwrap();
        assert!(hash.starts_with("$argon2"), "we never MINT bcrypt: {hash}");
        assert!(verify_password("correct horse", &hash).unwrap());
    }
}
