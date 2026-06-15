//! Authentication for jerrycan: argon2 password hashing, AEAD session cookies,
//! HS256 JWTs, role guards. Vetted RustCrypto primitives; hand-rolled envelopes
//! (see module docs). #![forbid(unsafe_code)].
#![forbid(unsafe_code)]

use jerrycan_core::{App, Extension};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

pub mod api_key;
pub mod guard;
pub mod jwt;
pub mod password;
pub mod session;
pub mod webhook;

pub use api_key::{
    ApiKey, ApiKeyFuture, ApiKeyRecord, ApiKeyStore, ApiKeys, InMemoryApiKeyStore, MintedApiKey,
    hash_key, mint, require_scope, verify,
};
pub use guard::{Bearer, Session, require_role};
pub use password::{hash_password, verify_password};
pub use session::SessionStore;

/// Minimum entropy for `JERRYCAN_SECRET`. Shorter secrets are rejected in prod.
pub(crate) const MIN_SECRET_LEN: usize = 32;

/// Derive a 32-byte subkey from the master secret and a domain label, so the
/// session key, the JWT key, and the token-at-rest key are independent even
/// though one secret seeds all of them.
///
/// Returns `Zeroizing<[u8; 32]>` so the derived bytes are wiped from memory when
/// the value drops. It derefs to `[u8; 32]`, so `&derive_key(..)` coerces to the
/// `&[u8; 32]` / `&[u8]` that callers (`SessionStore::new`, `jwt::*`) expect.
pub(crate) fn derive_key(secret: &[u8], label: &str) -> Zeroizing<[u8; 32]> {
    let mut hasher = Sha256::new();
    hasher.update(secret);
    hasher.update(label.as_bytes());
    Zeroizing::new(hasher.finalize().into())
}

/// The auth extension: holds the derived session, token-at-rest, and JWT keys,
/// registered as a dependency so `Session`/`Bearer` extractors can resolve it.
///
/// Each store is rotation-aware (multi-key decrypt): the keys derived from the
/// *primary* secret encrypt new data, while keys derived from any *retired*
/// secrets only decrypt pre-rotation data (see [`Auth::with_secrets`]).
#[derive(Clone)]
pub struct Auth {
    sessions: SessionStore,
    tokens: SessionStore,
    jwt_key: [u8; 32],
}

impl Auth {
    /// Build from an explicit secret (>= 32 bytes recommended), with no retired
    /// secrets. Equivalent to `with_secrets(secret, &[])`.
    pub fn with_secret(secret: &str) -> Self {
        Self::with_secrets(secret, &[])
    }

    /// Build with key rotation: `primary` encrypts new sessions/tokens; each of
    /// `retired` can still *decrypt* sessions/tokens minted before rotation but
    /// is never used to encrypt. Move the previous `JERRYCAN_SECRET` into
    /// `retired` to rotate without logging users out, then drop it once you want
    /// its sessions/tokens fully invalidated.
    pub fn with_secrets(primary: &str, retired: &[&str]) -> Self {
        // Session and token-at-rest keys: distinct labels keep their ciphertexts
        // non-cross-decryptable even though one secret seeds both.
        let session_primary = derive_key(primary.as_bytes(), "session");
        let token_primary = derive_key(primary.as_bytes(), "oauth-token");

        // Derive fallback key sets from the retired secrets. The `Zeroizing`
        // wrappers wipe the bytes when these vecs drop at the end of the fn.
        let session_fallbacks: Vec<Zeroizing<[u8; 32]>> = retired
            .iter()
            .map(|s| derive_key(s.as_bytes(), "session"))
            .collect();
        let token_fallbacks: Vec<Zeroizing<[u8; 32]>> = retired
            .iter()
            .map(|s| derive_key(s.as_bytes(), "oauth-token"))
            .collect();
        // `SessionStore::with_keys` wants `&[[u8; 32]]`; map through the deref.
        let session_fallback_keys: Vec<[u8; 32]> = session_fallbacks.iter().map(|k| **k).collect();
        let token_fallback_keys: Vec<[u8; 32]> = token_fallbacks.iter().map(|k| **k).collect();

        Self {
            sessions: SessionStore::with_keys(&session_primary, &session_fallback_keys),
            tokens: SessionStore::with_keys(&token_primary, &token_fallback_keys),
            jwt_key: *derive_key(primary.as_bytes(), "jwt"),
        }
    }

    /// Build from `JERRYCAN_SECRET` (primary) plus optional `JERRYCAN_SECRET_OLD`
    /// (a comma-separated list of retired secrets for key rotation).
    ///
    /// In production (`JERRYCAN_ENV=prod`) a missing or short primary secret is a
    /// loud error, and each non-empty retired secret must also meet
    /// `MIN_SECRET_LEN` (empty entries are skipped — they let you write
    /// `JERRYCAN_SECRET_OLD=""` or a trailing comma harmlessly). In dev it warns
    /// and uses a fixed dev key (NEVER use in production). When
    /// `JERRYCAN_SECRET_OLD` is unset, behavior is identical to a single secret.
    pub fn from_env() -> jerrycan_core::Result<Self> {
        let is_prod = std::env::var("JERRYCAN_ENV").as_deref() == Ok("prod");
        let secret = std::env::var("JERRYCAN_SECRET").ok();
        let retired_raw = std::env::var("JERRYCAN_SECRET_OLD").unwrap_or_default();
        Self::from_env_parts(is_prod, secret.as_deref(), &retired_raw)
    }

    /// The pure core of [`Auth::from_env`]: all the env-var parsing and prod
    /// validation, parameterized on the raw values so it is testable without
    /// mutating process-global state (which `#![forbid(unsafe_code)]` + edition
    /// 2024's `unsafe set_var` makes awkward, and which races under parallel
    /// tests). `secret` is `JERRYCAN_SECRET`; `retired_raw` is the raw
    /// `JERRYCAN_SECRET_OLD` string (comma-separated, empties skipped).
    fn from_env_parts(
        is_prod: bool,
        secret: Option<&str>,
        retired_raw: &str,
    ) -> jerrycan_core::Result<Self> {
        let retired: Vec<&str> = retired_raw
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        if is_prod && let Some(short) = retired.iter().find(|s| s.len() < MIN_SECRET_LEN) {
            return Err(jerrycan_core::Error::internal(format!(
                "JERRYCAN_SECRET_OLD entries must each be at least {MIN_SECRET_LEN} bytes in production (got one of length {})",
                short.len()
            )));
        }

        match secret {
            Some(s) if s.len() >= MIN_SECRET_LEN => Ok(Self::with_secrets(s, &retired)),
            Some(_) if is_prod => Err(jerrycan_core::Error::internal(format!(
                "JERRYCAN_SECRET must be at least {MIN_SECRET_LEN} bytes in production"
            ))),
            None if is_prod => Err(jerrycan_core::Error::internal(
                "JERRYCAN_SECRET is required in production (JERRYCAN_ENV=prod)",
            )),
            _ => {
                eprintln!(
                    "jerrycan-auth: WARNING using an insecure development secret; set JERRYCAN_SECRET (>= {MIN_SECRET_LEN} bytes) for production"
                );
                Ok(Self::with_secrets(
                    "jerrycan-insecure-development-secret-do-not-use!!",
                    &retired,
                ))
            }
        }
    }

    pub fn sessions(&self) -> &SessionStore {
        &self.sessions
    }

    /// The token-at-rest codec (rotation-aware, keyed independently of sessions).
    /// Encrypt an OAuth `TokenResponse` with `auth.tokens().encode(&t)?` before
    /// persisting the ciphertext; `decode` on read. Key rotation applies
    /// automatically, exactly as for sessions.
    pub fn tokens(&self) -> &SessionStore {
        &self.tokens
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
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct Tok {
        access: String,
        refresh: String,
    }

    fn sample_token() -> Tok {
        Tok {
            access: "at-123".into(),
            refresh: "rt-456".into(),
        }
    }

    // Two real 32+ byte secrets for rotation tests.
    const SECRET_OLD: &str = "old-secret-of-at-least-thirty-two-bytes!!";
    const SECRET_NEW: &str = "new-secret-of-at-least-thirty-two-bytes!!";
    const SECRET_STRANGER: &str = "stranger-secret-at-least-thirty-two-byte";

    #[test]
    fn derived_keys_are_label_separated() {
        let s = b"a-very-long-development-secret-string!!";
        assert_ne!(*derive_key(s, "session"), *derive_key(s, "jwt"));
        assert_ne!(*derive_key(s, "session"), *derive_key(s, "oauth-token"));
        assert_ne!(*derive_key(s, "jwt"), *derive_key(s, "oauth-token"));
        assert_eq!(*derive_key(s, "session"), *derive_key(s, "session"));
    }

    #[test]
    fn rotated_token_at_rest_still_decodes_so_rotation_does_not_log_everyone_out() {
        // App encrypts an OAuth token under the OLD secret, persists ciphertext.
        let before = Auth::with_secret(SECRET_OLD);
        let ciphertext = before.tokens().encode(&sample_token()).unwrap();

        // Operator rotates JERRYCAN_SECRET to NEW, lists OLD as retired.
        let after = Auth::with_secrets(SECRET_NEW, &[SECRET_OLD]);
        let back: Tok = after
            .tokens()
            .decode(&ciphertext)
            .expect("token encrypted before rotation must decode via the retired key");
        assert_eq!(back, sample_token());
    }

    #[test]
    fn a_secret_in_neither_primary_nor_retired_fails_401_real_retirement_invalidates() {
        // Token from a secret that is never the primary and never retired.
        let stranger = Auth::with_secret(SECRET_STRANGER);
        let ciphertext = stranger.tokens().encode(&sample_token()).unwrap();

        let auth = Auth::with_secrets(SECRET_NEW, &[SECRET_OLD]);
        let err = auth.tokens().decode::<Tok>(&ciphertext).unwrap_err();
        assert_eq!(
            err.code(),
            "JC0401",
            "fully-retired/unknown secrets must eventually invalidate their tokens"
        );
    }

    #[test]
    fn tokens_and_sessions_ciphertexts_are_not_cross_decryptable_label_separation() {
        let auth = Auth::with_secret(SECRET_NEW);

        // A token ciphertext must NOT decode through the session store...
        let token_ct = auth.tokens().encode(&sample_token()).unwrap();
        assert!(
            auth.sessions().decode::<Tok>(&token_ct).is_err(),
            "a leaked session key must not read tokens-at-rest"
        );

        // ...and a session ciphertext must NOT decode through the token store.
        let session_ct = auth.sessions().encode(&sample_token()).unwrap();
        assert!(
            auth.tokens().decode::<Tok>(&session_ct).is_err(),
            "a leaked token key must not read sessions"
        );
    }

    // --- from_env parsing/validation ---
    //
    // We test `from_env_parts` (the pure core) directly rather than mutating
    // process-global env vars. Edition 2024 makes `std::env::set_var` `unsafe`,
    // which `#![forbid(unsafe_code)]` rejects; and env mutation races under
    // cargo's parallel test threads. Passing the raw values in keeps every
    // assertion deterministic and exercises the exact logic `from_env` runs.
    //
    // `Auth` intentionally does not derive `Debug` (it holds key material), so
    // `Result<Auth>` can't use `unwrap`/`unwrap_err`. These helpers extract the
    // success/error sides without requiring `Auth: Debug`.
    fn ok_auth(r: jerrycan_core::Result<Auth>) -> Auth {
        match r {
            Ok(a) => a,
            Err(e) => panic!("expected Ok(Auth), got error: {e}"),
        }
    }
    fn err_of(r: jerrycan_core::Result<Auth>) -> jerrycan_core::Error {
        match r {
            Ok(_) => panic!("expected an error, got Ok(Auth)"),
            Err(e) => e,
        }
    }

    #[test]
    fn from_env_with_two_retired_secrets_decodes_tokens_from_either_old_key() {
        // JERRYCAN_SECRET_OLD="SECRET_OLD,SECRET_STRANGER" ⇒ two fallbacks.
        let token_a = Auth::with_secret(SECRET_OLD)
            .tokens()
            .encode(&sample_token())
            .unwrap();
        let token_b = Auth::with_secret(SECRET_STRANGER)
            .tokens()
            .encode(&sample_token())
            .unwrap();

        let old = format!("{SECRET_OLD},{SECRET_STRANGER}");
        let auth = ok_auth(Auth::from_env_parts(false, Some(SECRET_NEW), &old));

        // Both retired secrets became fallbacks: tokens from each still decode.
        assert_eq!(
            auth.tokens().decode::<Tok>(&token_a).unwrap(),
            sample_token()
        );
        assert_eq!(
            auth.tokens().decode::<Tok>(&token_b).unwrap(),
            sample_token()
        );
        // A token from the (current) primary obviously also decodes.
        let token_new = auth.tokens().encode(&sample_token()).unwrap();
        assert_eq!(
            auth.tokens().decode::<Tok>(&token_new).unwrap(),
            sample_token()
        );
    }

    #[test]
    fn from_env_prod_rejects_a_too_short_retired_secret() {
        let err = err_of(Auth::from_env_parts(true, Some(SECRET_NEW), "too-short"));
        assert!(
            err.to_string().contains("JERRYCAN_SECRET_OLD"),
            "prod must reject a short retired secret, got: {err}"
        );
    }

    #[test]
    fn from_env_dev_tolerates_a_short_retired_secret() {
        // Outside prod, length is not enforced (dev convenience), so this builds.
        Auth::from_env_parts(false, Some(SECRET_NEW), "too-short")
            .expect("dev must not enforce retired-secret length");
    }

    #[test]
    fn from_env_empty_retired_entries_are_skipped_even_in_prod() {
        // Trailing comma / blank entry must not become a (short) fallback key.
        let auth = Auth::from_env_parts(true, Some(SECRET_NEW), ",  ,")
            .expect("blank-only retired list is valid in prod");
        // No fallbacks ⇒ behaves like a single-secret store: an OLD-key token
        // does NOT decode.
        let ct = Auth::with_secret(SECRET_OLD)
            .tokens()
            .encode(&sample_token())
            .unwrap();
        assert!(auth.tokens().decode::<Tok>(&ct).is_err());
    }

    #[test]
    fn from_env_unset_retired_is_identical_to_single_secret() {
        // Empty JERRYCAN_SECRET_OLD ⇒ no fallbacks, same as with_secret.
        let from_parts = ok_auth(Auth::from_env_parts(true, Some(SECRET_NEW), ""));
        let single = Auth::with_secret(SECRET_NEW);
        let ct = single.tokens().encode(&sample_token()).unwrap();
        assert_eq!(
            from_parts.tokens().decode::<Tok>(&ct).unwrap(),
            sample_token()
        );
    }

    #[test]
    fn from_env_prod_requires_a_secret() {
        let err = err_of(Auth::from_env_parts(true, None, ""));
        assert!(err.to_string().contains("JERRYCAN_SECRET is required"));
    }

    #[test]
    fn from_env_prod_rejects_short_primary() {
        let err = err_of(Auth::from_env_parts(true, Some("short"), ""));
        assert!(err.to_string().contains("at least"));
    }
}
