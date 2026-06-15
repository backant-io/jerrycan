//! Scoped API keys (spec §v2.4 Task 2): mint a high-entropy key, store only its
//! SHA-256 hash, authenticate requests, and scope-check them.
//!
//! ## Why SHA-256, not argon2
//! API keys are machine-generated with 256 bits of entropy ([`mint`] draws 32
//! bytes from the OS CSPRNG), so brute-forcing the preimage is infeasible
//! regardless of hash speed. argon2 buys nothing here and would make the lookup
//! column slow and variable-length. A plain hex SHA-256 gives a fast, fixed-width
//! (64-char) lookup key — exactly what a DB index wants.
//!
//! ## Why the verify is constant-time
//! [`verify`] hashes the presented plaintext and compares the two **32-byte
//! digests** in constant time via `hmac`'s `verify_slice` (the same primitive the
//! webhook code uses). It deliberately does NOT compare the hex `String`s with
//! `==`: `String`/`&str` equality short-circuits on the first differing byte,
//! which leaks, through timing, how many leading hex chars of the stored hash a
//! guess matched. Comparing the raw digests in fixed time closes that channel.
//! (The stored hash is not itself a secret the way an HMAC key is, but verifying
//! in constant time is cheap and removes the channel by construction.)
//!
//! ## DI: how the store reaches the extractor
//! [`ApiKey`] resolves the store as the concrete newtype [`ApiKeys`] (which wraps
//! `Arc<dyn ApiKeyStore>`), NOT a bare `Arc<dyn ApiKeyStore>`. The DI layer keys
//! providers on `TypeId` and round-trips a bare trait-object `Arc` just fine, but
//! the newtype is the documented, unambiguous contract: the app calls
//! `app.provide(ApiKeys::new(store))` and the extractor resolves `ApiKeys`. See
//! the `bare_arc_dyn_also_round_trips_through_di` test for evidence the bare form
//! works too; we still standardize on the newtype.

use crate::webhook::hex_encode;
use base64::Engine;
use chacha20poly1305::aead::OsRng;
use jerrycan_core::{Error, FromRequest, Headers, RequestCtx, Result};
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

/// A freshly minted key. The `plaintext` is shown to the operator exactly once
/// and is NEVER stored — only its `hash` (and `prefix`) are persisted.
pub struct MintedApiKey {
    /// The full secret to hand to the caller once: `{prefix}_{base64url-random}`.
    pub plaintext: String,
    /// The human-readable label prefix (e.g. `"sk_live"`), stored for display.
    pub prefix: String,
    /// Hex SHA-256 of the full plaintext — the value to persist and index on.
    pub hash: String,
}

/// Mint a new API key: 32 bytes from the OS CSPRNG, formatted as
/// `{prefix}_{base64url_nopad(random)}`. The returned [`MintedApiKey::hash`] is
/// what you store; [`MintedApiKey::plaintext`] is shown once and discarded.
pub fn mint(prefix: &str) -> MintedApiKey {
    let mut random = [0u8; 32];
    OsRng.fill_bytes(&mut random);
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random);
    let plaintext = format!("{prefix}_{encoded}");
    let hash = hash_key(&plaintext);
    MintedApiKey {
        plaintext,
        prefix: prefix.to_string(),
        hash,
    }
}

/// Hex SHA-256 of the full key plaintext — the value stored in the lookup column.
pub fn hash_key(plaintext: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(plaintext.as_bytes());
    hex_encode(&hasher.finalize())
}

/// Constant-time check that `plaintext` hashes to `stored_hash`.
///
/// Hashes `plaintext` and compares the two raw 32-byte digests in fixed time
/// (via `hmac`'s `verify_slice`), so the comparison can't leak a partial match
/// through timing. A malformed (non-hex / wrong-length) `stored_hash` simply
/// fails — never panics. See the module docs for why this is not a `String ==`.
pub fn verify(plaintext: &str, stored_hash: &str) -> bool {
    use hmac::{Hmac, Mac};
    use sha2::Sha256 as Sha256Mac;

    let Some(stored) = decode_hex_digest(stored_hash) else {
        return false;
    };
    let mut digest = [0u8; 32];
    let mut hasher = Sha256::new();
    hasher.update(plaintext.as_bytes());
    digest.copy_from_slice(&hasher.finalize());

    // `verify_slice` is a constant-time tag comparison. We MAC the candidate
    // digest under a fixed zero key and compare against the stored digest MAC'd
    // the same way — equivalently, just compare the digests in fixed time. Using
    // the HMAC machinery keeps us on the vetted constant-time path rather than a
    // hand-rolled loop. The key is constant, so this adds no secret material.
    let mut mac =
        Hmac::<Sha256Mac>::new_from_slice(&[0u8; 32]).expect("hmac accepts any key length");
    mac.update(&stored);
    let mut expected =
        Hmac::<Sha256Mac>::new_from_slice(&[0u8; 32]).expect("hmac accepts any key length");
    expected.update(&digest);
    mac.verify_slice(&expected.finalize().into_bytes()).is_ok()
}

/// Decode a 64-char lowercase/uppercase hex string into a 32-byte digest.
/// Returns `None` for the wrong length or any non-hex char — never panics.
fn decode_hex_digest(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let bytes = s.as_bytes();
    let mut out = [0u8; 32];
    for (i, pair) in bytes.chunks_exact(2).enumerate() {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out[i] = (hi * 16 + lo) as u8;
    }
    Some(out)
}

/// A stored API-key record: its DB id, display prefix, hex hash, and scopes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiKeyRecord {
    pub id: i64,
    pub prefix: String,
    pub hash: String,
    pub scopes: Vec<String>,
}

impl ApiKeyRecord {
    /// `Ok(())` if this key carries `needed` (or the `"*"` wildcard); else 403.
    pub fn require_scope(&self, needed: &str) -> Result<()> {
        require_scope(&self.scopes, needed)
    }
}

/// Scope check (mirrors [`crate::require_role`]): `Ok(())` when `scopes` contains
/// `needed` or the wildcard `"*"`, otherwise `Error::forbidden()` (403). The
/// wildcard is an admin/root grant — a `"*"` key passes every check.
pub fn require_scope(scopes: &[String], needed: &str) -> Result<()> {
    if scopes.iter().any(|s| s == "*" || s == needed) {
        Ok(())
    } else {
        Err(Error::forbidden())
    }
}

/// The boxed `Send` future every [`ApiKeyStore`] method returns. Hand-boxed (not
/// `async-trait`) so the trait stays object-safe behind `dyn` — the same idiom
/// as `jerrycan_jobs`'s `JobFuture` and `jerrycan_ratelimit`'s `HitFuture`.
pub type ApiKeyFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// Looks up a stored key by its hex hash. Object-safe so apps can back it with a
/// DB table (returning `None` for an unknown hash, NOT an error).
pub trait ApiKeyStore: Send + Sync {
    fn lookup<'a>(&'a self, hash: &'a str) -> ApiKeyFuture<'a, Option<ApiKeyRecord>>;
}

/// In-memory store keyed by hex hash — for tests, the mock, and small deploys.
/// Production apps implement [`ApiKeyStore`] over their database instead.
#[derive(Default)]
pub struct InMemoryApiKeyStore {
    keys: Mutex<HashMap<String, ApiKeyRecord>>,
}

impl InMemoryApiKeyStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a record, indexed by its `hash`. Replaces any record with the same
    /// hash (a hash collision would mean an identical key — practically never).
    pub fn insert(&self, record: ApiKeyRecord) {
        self.keys
            .lock()
            .expect("api-key store mutex poisoned")
            .insert(record.hash.clone(), record);
    }
}

impl ApiKeyStore for InMemoryApiKeyStore {
    fn lookup<'a>(&'a self, hash: &'a str) -> ApiKeyFuture<'a, Option<ApiKeyRecord>> {
        Box::pin(async move {
            Ok(self
                .keys
                .lock()
                .expect("api-key store mutex poisoned")
                .get(hash)
                .cloned())
        })
    }
}

/// The DI handle for an [`ApiKeyStore`]. Apps register the store with
/// `app.provide(ApiKeys::new(store))`; the [`ApiKey`] extractor resolves this
/// concrete type. A newtype (rather than a bare `Arc<dyn ApiKeyStore>`) is the
/// documented contract — see the module docs.
#[derive(Clone)]
pub struct ApiKeys(pub Arc<dyn ApiKeyStore>);

impl ApiKeys {
    pub fn new(store: impl ApiKeyStore + 'static) -> Self {
        ApiKeys(Arc::new(store))
    }

    /// Wrap an already-`Arc`'d store (e.g. one shared with other components).
    pub fn from_arc(store: Arc<dyn ApiKeyStore>) -> Self {
        ApiKeys(store)
    }
}

/// API-key extractor. Reads the key from `Authorization: Bearer <key>` (checked
/// first) or `X-API-Key: <key>`, hashes it, looks it up in the provided
/// [`ApiKeys`] store, and yields the matching [`ApiKeyRecord`]. A missing,
/// malformed, or unknown key is a 401. Scope checks are a separate, explicit
/// step ([`ApiKeyRecord::require_scope`]) so a handler decides what it needs.
pub struct ApiKey(pub ApiKeyRecord);

impl FromRequest for ApiKey {
    async fn from_request(ctx: &mut RequestCtx) -> Result<Self> {
        let store = ctx.resolve::<ApiKeys>().await?;
        let headers = Headers::from_request(ctx).await?;
        let presented = extract_key(&headers).ok_or_else(Error::unauthorized)?;
        let hash = hash_key(&presented);
        match store.0.lookup(&hash).await? {
            Some(record) => Ok(ApiKey(record)),
            None => Err(Error::unauthorized()),
        }
    }
}

/// Pull the raw key from the headers: `Authorization: Bearer <key>` takes
/// precedence over `X-API-Key: <key>`. Returns `None` if neither is present in a
/// usable form.
fn extract_key(headers: &Headers) -> Option<String> {
    if let Some(auth) = headers.get("authorization")
        && let Some(token) = auth.strip_prefix("Bearer ")
        && !token.is_empty()
    {
        return Some(token.to_string());
    }
    headers
        .get("x-api-key")
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jerrycan_core::{App, Json, get, http::StatusCode};

    // ---- crypto core ----

    #[test]
    fn mint_then_verify_roundtrips_and_tamper_fails() {
        let minted = mint("sk_live");
        // The minted plaintext carries the prefix and a base64url random tail.
        assert!(minted.plaintext.starts_with("sk_live_"));
        assert_eq!(minted.prefix, "sk_live");
        // A valid plaintext verifies against its stored hash...
        assert!(verify(&minted.plaintext, &minted.hash));
        // ...and any tamper (one char flipped) does not.
        let mut tampered = minted.plaintext.clone();
        tampered.push('x');
        assert!(!verify(&tampered, &minted.hash));
        assert!(!verify("sk_live_totally-different", &minted.hash));
    }

    #[test]
    fn stored_hash_never_contains_the_plaintext_secret() {
        // Storing the hash must not leak the key: the random tail (the actual
        // secret) must not appear anywhere in the stored hash, and the hash is a
        // fixed 64-char hex string regardless of the key.
        let minted = mint("sk_live");
        let random_tail = minted
            .plaintext
            .strip_prefix("sk_live_")
            .expect("prefix present");
        assert!(!random_tail.is_empty());
        assert!(
            !minted.hash.contains(random_tail),
            "the stored hash must not embed the plaintext secret"
        );
        assert!(!minted.hash.contains(&minted.plaintext));
        assert_eq!(minted.hash.len(), 64, "hex sha256 is fixed-width");
        assert!(minted.hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn hash_key_matches_a_known_sha256_vector() {
        // Pins the algorithm: SHA-256("abc") is a well-known vector. Proves
        // hash_key computes real SHA-256, not just something roundtrip-consistent.
        assert_eq!(
            hash_key("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn verify_compares_digests_in_constant_time_not_the_hex_string() {
        // Documents the constant-time contract: `verify` works on the raw 32-byte
        // digests, never `==` on the hex String (which short-circuits and leaks a
        // partial-match prefix length via timing). A real timing test is out of
        // scope; this asserts the BEHAVIOR that the implementation guarantees —
        // a stored hash differing only in its LAST hex char must still reject,
        // exactly as one differing in the first char does.
        let minted = mint("sk_test");
        let mut last_flipped = minted.hash.clone();
        let last = last_flipped.pop().unwrap();
        last_flipped.push(if last == '0' { '1' } else { '0' });
        assert!(!verify(&minted.plaintext, &last_flipped));

        let mut first_flipped = minted.hash.clone();
        let first = first_flipped.remove(0);
        first_flipped.insert(0, if first == '0' { '1' } else { '0' });
        assert!(!verify(&minted.plaintext, &first_flipped));

        // Malformed stored hashes never panic and never verify.
        assert!(!verify(&minted.plaintext, "not-hex"));
        assert!(!verify(&minted.plaintext, ""));
        assert!(!verify(&minted.plaintext, &"a".repeat(64))); // valid hex, wrong digest
    }

    // ---- scope checks ----

    #[test]
    fn require_scope_allows_exact_and_wildcard_rejects_others() {
        let scoped = vec!["read".to_string(), "write".to_string()];
        assert!(require_scope(&scoped, "read").is_ok());
        assert!(require_scope(&scoped, "write").is_ok());
        // A scope the key lacks is 403.
        let err = require_scope(&scoped, "admin").unwrap_err();
        assert_eq!(err.status(), StatusCode::FORBIDDEN);

        // The wildcard passes any check.
        let wild = vec!["*".to_string()];
        assert!(require_scope(&wild, "anything").is_ok());
        assert!(require_scope(&wild, "admin").is_ok());

        // Empty scopes grant nothing.
        assert_eq!(
            require_scope(&[], "read").unwrap_err().status(),
            StatusCode::FORBIDDEN
        );

        // The method mirrors the free fn.
        let rec = ApiKeyRecord {
            id: 1,
            prefix: "sk".into(),
            hash: "h".into(),
            scopes: scoped,
        };
        assert!(rec.require_scope("read").is_ok());
        assert_eq!(
            rec.require_scope("admin").unwrap_err().status(),
            StatusCode::FORBIDDEN
        );
    }

    // ---- full extractor path through a real App ----

    /// A scope-gated handler: yields the record's prefix only if the key carries
    /// the `reports:read` scope, so the test asserts the resolved record AND the
    /// scope enforcement in one path.
    async fn reports(ApiKey(key): ApiKey) -> Result<Json<String>> {
        key.require_scope("reports:read")?;
        Ok(Json(key.prefix))
    }

    fn seed_store() -> (InMemoryApiKeyStore, MintedApiKey, MintedApiKey) {
        let store = InMemoryApiKeyStore::new();
        // A scoped key with the required scope.
        let scoped = mint("sk_live");
        store.insert(ApiKeyRecord {
            id: 1,
            prefix: scoped.prefix.clone(),
            hash: scoped.hash.clone(),
            scopes: vec!["reports:read".into()],
        });
        // A key WITHOUT the required scope (only "other").
        let unscoped = mint("sk_other");
        store.insert(ApiKeyRecord {
            id: 2,
            prefix: unscoped.prefix.clone(),
            hash: unscoped.hash.clone(),
            scopes: vec!["other".into()],
        });
        (store, scoped, unscoped)
    }

    #[tokio::test]
    async fn valid_x_api_key_resolves_record_and_passes_scope() {
        let (store, scoped, _unscoped) = seed_store();
        let app = App::new()
            .provide(ApiKeys::new(store))
            .route("/reports", get(reports));
        let t = app.into_test();

        // X-API-Key header form.
        let res = t
            .get_with("/reports", &[("x-api-key", &scoped.plaintext)])
            .await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.json::<String>(), "sk_live");
    }

    #[tokio::test]
    async fn valid_authorization_bearer_resolves_record() {
        let (store, scoped, _unscoped) = seed_store();
        let app = App::new()
            .provide(ApiKeys::new(store))
            .route("/reports", get(reports));
        let t = app.into_test();

        // Authorization: Bearer form — both header schemes must work.
        let bearer = format!("Bearer {}", scoped.plaintext);
        let res = t.get_with("/reports", &[("authorization", &bearer)]).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.json::<String>(), "sk_live");
    }

    #[tokio::test]
    async fn missing_or_garbage_key_is_401() {
        let (store, _scoped, _unscoped) = seed_store();
        let app = App::new()
            .provide(ApiKeys::new(store))
            .route("/reports", get(reports));
        let t = app.into_test();

        // No key at all.
        assert_eq!(t.get("/reports").await.status(), StatusCode::UNAUTHORIZED);
        // A key that isn't in the store.
        let res = t
            .get_with("/reports", &[("x-api-key", "sk_live_not-a-real-key")])
            .await;
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        // An Authorization header that isn't a Bearer token.
        let res = t
            .get_with("/reports", &[("authorization", "Basic abc")])
            .await;
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn valid_key_lacking_scope_is_403() {
        let (store, _scoped, unscoped) = seed_store();
        let app = App::new()
            .provide(ApiKeys::new(store))
            .route("/reports", get(reports));
        let t = app.into_test();

        // The key authenticates (200-eligible) but lacks `reports:read` → 403.
        let res = t
            .get_with("/reports", &[("x-api-key", &unscoped.plaintext)])
            .await;
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn wildcard_key_passes_any_scope_check() {
        let store = InMemoryApiKeyStore::new();
        let admin = mint("sk_admin");
        store.insert(ApiKeyRecord {
            id: 9,
            prefix: admin.prefix.clone(),
            hash: admin.hash.clone(),
            scopes: vec!["*".into()],
        });
        let app = App::new()
            .provide(ApiKeys::new(store))
            .route("/reports", get(reports));
        let t = app.into_test();

        let res = t
            .get_with("/reports", &[("x-api-key", &admin.plaintext)])
            .await;
        assert_eq!(res.status(), StatusCode::OK, "a `*` key passes any scope");
    }

    #[tokio::test]
    async fn bearer_takes_precedence_over_x_api_key() {
        // When both headers are present, Bearer wins. Put the VALID key in
        // Authorization and a garbage value in X-API-Key: success proves Bearer
        // was read first.
        let (store, scoped, _unscoped) = seed_store();
        let app = App::new()
            .provide(ApiKeys::new(store))
            .route("/reports", get(reports));
        let t = app.into_test();

        let bearer = format!("Bearer {}", scoped.plaintext);
        let res = t
            .get_with(
                "/reports",
                &[("authorization", &bearer), ("x-api-key", "garbage")],
            )
            .await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.json::<String>(), "sk_live");
    }

    /// Evidence for the DI decision (documented in the module docs): a bare
    /// `Arc<dyn ApiKeyStore>` DOES round-trip through `provide`/`resolve` (the DI
    /// keys on `TypeId`, and `TypeId::of::<Arc<dyn ApiKeyStore>>()` is stable).
    /// We still standardize the extractor on the `ApiKeys` newtype; this test
    /// just records that the bare form works, so the choice is by convention, not
    /// necessity.
    #[tokio::test]
    async fn bare_arc_dyn_also_round_trips_through_di() {
        use jerrycan_core::Dep;

        async fn count_via_bare(dep: Dep<Arc<dyn ApiKeyStore>>) -> Result<Json<bool>> {
            // Resolving and using it proves the round-trip; the lookup of an
            // absent hash returns Ok(None).
            let found = dep.lookup("deadbeef").await?;
            Ok(Json(found.is_none()))
        }

        let store: Arc<dyn ApiKeyStore> = Arc::new(InMemoryApiKeyStore::new());
        let app = App::new()
            .provide(store)
            .route("/probe", get(count_via_bare));
        let res = app.into_test().get("/probe").await;
        assert_eq!(res.status(), StatusCode::OK);
        assert!(res.json::<bool>(), "bare Arc<dyn ApiKeyStore> resolved");
    }
}
