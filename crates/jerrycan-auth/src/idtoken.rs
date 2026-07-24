//! Third-party **ID-token** verification (Sign in with Apple, Google Sign-In, and
//! custom OIDC issuers), gated `#[cfg(feature = "idtoken")]`.
//!
//! ## The pattern this solves
//! The dominant native-mobile sign-in flow: the app obtains a provider ID token
//! client-side (Apple/Google SDK), POSTs it to the backend, and the backend
//! verifies the RS256 JWS against the provider's JWKS — checking `iss`, `aud`,
//! `exp`/`nbf` (+ an optional `nonce`) — then issues its OWN session (e.g. a
//! jerrycan session cookie or an [`crate::jwt`] token). Without this, every app
//! hand-rolls JWKS fetch/cache + signature validation, which is exactly where
//! subtle auth bugs (alg-confusion, skipped `aud`, no rotation) live. This module
//! is that verifier.
//!
//! ## Shape
//! [`Verifier`] is config — the acceptable `iss` value(s), the `jwks_uri`, and the
//! expected `aud` — plus a rotation-safe JWKS cache. Adding a provider is a data
//! change (a new preset like [`Verifier::google`]/[`Verifier::apple`]), not a code
//! change: the verify logic is provider-agnostic. [`Verifier::verify`] selects the
//! signing key by the token header's `kid`, verifies the **RS256** signature via
//! `jsonwebtoken` over a custom `ring`-backed `CryptoProvider` (the private
//! `ring_crypto` module below — the SAME crypto backend as the rest of the
//! crate's TLS, so no `rsa`-crate advisory enters the artifact), and validates
//! the registered claims.
//!
//! ## JWKS source seam (hermetic testing)
//! The JWKS is fetched through a [`JwksSource`] — object-safe, hand-boxed `Send`
//! future, the SAME idiom as [`crate::oauth::TokenTransport`]. Production uses
//! [`HttpJwksSource`] (hyper + hyper-rustls, HTTPS only). Tests inject an
//! in-process source that serves a known RSA public key, so a token signed in-test
//! verifies with no network — the ID-token analogue of [`crate::mock_idp`].
//!
//! ## Security posture
//! - **Algorithm is pinned to RS256, three times over.** A token whose header
//!   `alg` is anything else (`none`, `HS256`, …) is rejected *before* any key
//!   lookup; the `Validation` algorithm allowlist is exactly `[RS256]`; and the
//!   process-wide jsonwebtoken `CryptoProvider` this module installs implements
//!   ONLY RS256, so no other JWS algorithm can even be evaluated. This closes the
//!   alg-confusion and unsigned-token holes that a naive verifier leaves open.
//! - `iss` must match a configured value; `aud` must contain the expected
//!   audience; `exp`/`nbf` are enforced with a small clock-skew leeway.
//! - On an unknown `kid` the cache refetches **once** before failing, so key
//!   rotation is transparent — but a refetch is rate-limited by a cooldown so a
//!   stream of tokens bearing bogus `kid`s can't turn this into a JWKS-endpoint
//!   DoS amplifier.

use jerrycan_core::http::StatusCode;
use jerrycan_core::{Error, Result};
use serde::Deserialize;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};

/// How long a fetched JWKS is trusted before the next verify refetches it.
const DEFAULT_JWKS_TTL: Duration = Duration::from_secs(3600);
/// Clock-skew leeway applied to `exp`/`nbf`/`iat` (seconds).
const DEFAULT_LEEWAY_SECS: u64 = 60;
/// Minimum spacing between rotation-triggered (unknown-`kid`) refetches. Bounds a
/// bogus-`kid` flood to at most one JWKS fetch per this window.
const DEFAULT_MIN_REFETCH: Duration = Duration::from_secs(60);

/// Build a 401 (`JC0401`) with a specific reason. Every *token* verification
/// failure is unauthorized — the reason is for logs, not for the client to trust.
fn unauth(message: impl Into<String>) -> Error {
    Error::new(StatusCode::UNAUTHORIZED, "JC0401", message)
}

/// One RSA verifying key from a JWK Set (RFC 7517). Only the members the RS256
/// check needs are modeled; any others (`use`, `x5c`, …) are ignored. Non-RSA
/// keys are filtered out at lookup time.
#[derive(Clone, Debug, Deserialize)]
pub struct Jwk {
    /// Key type. Only `"RSA"` is usable here.
    pub kty: String,
    /// Key id, matched against the token header's `kid`.
    #[serde(default)]
    pub kid: Option<String>,
    /// Intended algorithm (advisory; providers may omit it).
    #[serde(default)]
    pub alg: Option<String>,
    /// RSA modulus, base64url (no pad).
    #[serde(default)]
    pub n: Option<String>,
    /// RSA public exponent, base64url (no pad).
    #[serde(default)]
    pub e: Option<String>,
}

/// A JSON Web Key Set, as served by a provider's `jwks_uri`.
#[derive(Clone, Debug, Deserialize)]
pub struct Jwks {
    /// The keys. A provider publishes several during a rotation overlap.
    pub keys: Vec<Jwk>,
}

impl Jwks {
    /// The first usable RSA key whose `kid` matches, if any.
    fn find(&self, kid: &str) -> Option<Jwk> {
        self.keys
            .iter()
            .find(|k| k.kty.eq_ignore_ascii_case("RSA") && k.kid.as_deref() == Some(kid))
            .cloned()
    }
}

/// The boxed `Send` future a [`JwksSource`] returns. Hand-boxed (not `async-trait`)
/// so the trait stays object-safe behind `dyn` — the same idiom as
/// [`crate::oauth::TokenFuture`].
pub type JwksFuture<'a> = Pin<Box<dyn Future<Output = Result<Jwks>> + Send + 'a>>;

/// Where a [`Verifier`] gets a provider's JWKS. Object-safe so the production
/// [`HttpJwksSource`] and an in-process test source are interchangeable — inject a
/// test source to verify tokens with no network.
pub trait JwksSource: Send + Sync {
    /// Fetch and parse the key set at `jwks_uri`.
    fn fetch<'a>(&'a self, jwks_uri: &'a str) -> JwksFuture<'a>;
}

/// The verified claims of an ID token. The registered claims most apps need are
/// typed; everything else the provider sent is preserved verbatim in [`Self::extra`]
/// (e.g. `email_verified`, `name`, Apple's `is_private_email`).
#[derive(Clone, Debug, Deserialize)]
pub struct IdTokenClaims {
    /// Subject — the provider's stable, unique user id. This is your join key for
    /// the external identity; do NOT key on `email`, which can change or be absent.
    pub sub: String,
    /// Issuer as asserted by the token (already checked against the configured
    /// issuer set before you receive this value).
    pub iss: String,
    /// Expiry (unix seconds).
    pub exp: u64,
    /// Issued-at (unix seconds), when present.
    #[serde(default)]
    pub iat: Option<u64>,
    /// The user's email, when the provider included it (Apple may omit it after
    /// the first sign-in).
    #[serde(default)]
    pub email: Option<String>,
    /// The `nonce` the token carries, when present — checked against the expected
    /// value when [`Verifier::require_nonce`] is set.
    #[serde(default)]
    pub nonce: Option<String>,
    /// Every remaining claim, verbatim. Read provider-specific fields from here.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl IdTokenClaims {
    /// Whether the provider asserted the email is verified. Handles BOTH spellings
    /// of `email_verified`: the JSON bool Google sends and the string `"true"`
    /// Apple sends. Absent/other ⇒ `false`.
    pub fn email_verified(&self) -> bool {
        match self.extra.get("email_verified") {
            Some(serde_json::Value::Bool(b)) => *b,
            Some(serde_json::Value::String(s)) => s.eq_ignore_ascii_case("true"),
            _ => false,
        }
    }
}

/// A cached JWKS plus when it was fetched (for TTL + refetch-cooldown decisions).
struct CacheEntry {
    jwks: Jwks,
    fetched_at: Instant,
}

/// A verifier for one provider + one expected audience.
///
/// Cheap to clone (the cache and source are shared behind `Arc`), so a single
/// configured `Verifier` can be stored in app state and used across requests.
#[derive(Clone)]
pub struct Verifier {
    /// Acceptable `iss` values. More than one because a provider may emit several
    /// spellings (Google: both `https://accounts.google.com` and the bare host).
    issuers: Vec<String>,
    /// The provider's JWKS endpoint.
    jwks_uri: String,
    /// Acceptable `aud` values (your client / service IDs).
    audiences: Vec<String>,
    leeway_secs: u64,
    expected_nonce: Option<String>,
    ttl: Duration,
    min_refetch: Duration,
    source: Arc<dyn JwksSource>,
    cache: Arc<Mutex<Option<CacheEntry>>>,
}

impl Verifier {
    /// **Google Sign-In.** Accepts BOTH issuer spellings Google emits
    /// (`https://accounts.google.com` and the bare `accounts.google.com`).
    /// `audience` is your OAuth client id (the ID token's `aud`).
    pub fn google(audience: impl Into<String>) -> Self {
        Self::custom(
            [
                "https://accounts.google.com".to_string(),
                "accounts.google.com".to_string(),
            ],
            "https://www.googleapis.com/oauth2/v3/certs",
            [audience.into()],
        )
    }

    /// **Sign in with Apple.** `audience` is your app's Service ID (web) or bundle
    /// ID (native) — whatever Apple was configured to put in `aud`.
    pub fn apple(audience: impl Into<String>) -> Self {
        Self::custom(
            ["https://appleid.apple.com".to_string()],
            "https://appleid.apple.com/auth/keys",
            [audience.into()],
        )
    }

    /// A **custom OIDC issuer**: the acceptable `iss` value(s), the `jwks_uri`, and
    /// the expected `aud` value(s). Use this for any provider without a preset.
    pub fn custom(
        issuers: impl IntoIterator<Item = String>,
        jwks_uri: impl Into<String>,
        audiences: impl IntoIterator<Item = String>,
    ) -> Self {
        // Make the ring-backed RS256 provider the process default before any
        // decode can run (jsonwebtoken 10 ships no crypto of its own here).
        ring_crypto::install();
        Self {
            issuers: issuers.into_iter().collect(),
            jwks_uri: jwks_uri.into(),
            audiences: audiences.into_iter().collect(),
            leeway_secs: DEFAULT_LEEWAY_SECS,
            expected_nonce: None,
            ttl: DEFAULT_JWKS_TTL,
            min_refetch: DEFAULT_MIN_REFETCH,
            source: Arc::new(HttpJwksSource::new()),
            cache: Arc::new(Mutex::new(None)),
        }
    }

    /// Replace the JWKS source — inject an in-process source for hermetic tests.
    #[must_use]
    pub fn with_source(mut self, source: Arc<dyn JwksSource>) -> Self {
        self.source = source;
        self
    }

    /// Set the JWKS cache TTL (default 1 hour). After it elapses, the next verify
    /// refetches before selecting a key.
    #[must_use]
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    /// Set the clock-skew leeway for `exp`/`nbf`/`iat` (default 60s).
    #[must_use]
    pub fn with_leeway(mut self, secs: u64) -> Self {
        self.leeway_secs = secs;
        self
    }

    /// Set the minimum spacing between unknown-`kid` (rotation) refetches
    /// (default 60s). Lower it only if you understand the JWKS-DoS trade-off.
    #[must_use]
    pub fn with_min_refetch_interval(mut self, interval: Duration) -> Self {
        self.min_refetch = interval;
        self
    }

    /// Require the token's `nonce` to equal `nonce` — bind the token to a value
    /// your app generated per sign-in attempt (replay / CSRF defense).
    #[must_use]
    pub fn require_nonce(mut self, nonce: impl Into<String>) -> Self {
        self.expected_nonce = Some(nonce.into());
        self
    }

    /// Add another acceptable `aud` (e.g. an app that ships both a web Service ID
    /// and a native bundle ID under one Apple team).
    #[must_use]
    pub fn add_audience(mut self, audience: impl Into<String>) -> Self {
        self.audiences.push(audience.into());
        self
    }

    /// Verify `token` and return its claims, or a typed error.
    ///
    /// Steps, in order (each rejects before the next runs):
    /// 1. Parse the JWS header; **require `alg = RS256`** and a `kid`.
    /// 2. Select the signing key for that `kid`, refetching the JWKS once on a
    ///    miss (rotation-safe).
    /// 3. Verify the RS256 signature and the registered claims (`iss`, `aud`,
    ///    `exp`, `nbf`) via `jsonwebtoken`.
    /// 4. If a nonce was required, check it.
    pub async fn verify(&self, token: &str) -> Result<IdTokenClaims> {
        // Idempotent belt-and-braces: `custom()` already installed the provider,
        // but every decode below must be preceded by an install on this thread.
        ring_crypto::install();

        // 1. Header first: pin RS256 and read the kid BEFORE any key lookup or
        //    network. A token whose alg is "none"/HS256/etc. never reaches step 2,
        //    so a forged unsigned/HMAC token can't be validated against an RSA key.
        let header = decode_header(token).map_err(|_| unauth("id token: malformed JWS header"))?;
        if header.alg != Algorithm::RS256 {
            return Err(unauth("id token: unsupported alg (only RS256 is accepted)"));
        }
        let kid = header
            .kid
            .ok_or_else(|| unauth("id token: JWS header has no kid"))?;

        // 2. Resolve the key by kid (rotation-safe cache).
        let jwk = self.key_for_kid(&kid).await?;
        let decoding_key = decoding_key(&jwk)?;

        // 3. Signature + registered-claim validation. jsonwebtoken verifies the
        //    RS256 signature first, then enforces iss/aud/exp/nbf. Restricting the
        //    allowed algorithms to RS256 here is a second, independent guard behind
        //    the header check above.
        let mut validation = Validation::new(Algorithm::RS256);
        // `new(RS256)` already sets the allowlist; re-pin it explicitly so no
        // jsonwebtoken default change can ever widen the accepted algorithms.
        validation.algorithms = vec![Algorithm::RS256];
        validation.set_issuer(&self.issuers);
        validation.set_audience(&self.audiences);
        // `set_audience` only checks the aud VALUE when the claim is present;
        // require the claim to EXIST too, or a validly-signed token that omits
        // `aud` entirely would pass. OIDC id tokens always carry `aud` (the
        // client_id). Default required set is {"exp"}; this makes it {"exp","aud"}.
        validation.required_spec_claims.insert("aud".to_string());
        validation.leeway = self.leeway_secs;
        validation.validate_exp = true;
        validation.validate_nbf = true;
        let data =
            decode::<IdTokenClaims>(token, &decoding_key, &validation).map_err(map_jwt_error)?;
        let claims = data.claims;

        // 4. Optional nonce binding: an absent OR mismatched nonce both fail.
        if let Some(expected) = &self.expected_nonce
            && claims.nonce.as_deref() != Some(expected.as_str())
        {
            return Err(unauth("id token: nonce mismatch"));
        }

        Ok(claims)
    }

    /// Resolve the RSA key for `kid`, refetching the JWKS when needed. Encodes the
    /// cache policy:
    /// - fresh cache that has the `kid` ⇒ use it (no fetch);
    /// - empty / stale cache, or a present-but-stale key ⇒ refetch;
    /// - `kid` absent from a still-fresh cache ⇒ refetch ONCE (rotation), but only
    ///   if the cooldown since the last fetch has elapsed — otherwise fail.
    ///
    /// The `Mutex` is only ever held to snapshot / store; it is NEVER held across
    /// the `.await`, so the async fetch can't deadlock other verifies.
    async fn key_for_kid(&self, kid: &str) -> Result<Jwk> {
        enum Plan {
            Use(Jwk),
            Refetch,
            Fail,
        }

        let plan = {
            let guard = self.cache.lock().expect("jwks cache mutex poisoned");
            match guard.as_ref() {
                None => Plan::Refetch,
                Some(entry) => {
                    let age = entry.fetched_at.elapsed();
                    match entry.jwks.find(kid) {
                        Some(jwk) if age < self.ttl => Plan::Use(jwk),
                        // Present but the cache has expired: refetch to pick up
                        // rotations / revocations (honors the TTL contract).
                        Some(_) => Plan::Refetch,
                        // Unknown kid: refetch to discover a freshly rotated key,
                        // but rate-limit so bogus kids can't drive a fetch storm.
                        None if age >= self.min_refetch => Plan::Refetch,
                        None => Plan::Fail,
                    }
                }
            }
        };

        match plan {
            Plan::Use(jwk) => Ok(jwk),
            Plan::Fail => Err(unauth("id token: no JWKS key matches the token kid")),
            Plan::Refetch => {
                let jwks = self.source.fetch(&self.jwks_uri).await?;
                let found = jwks.find(kid);
                *self.cache.lock().expect("jwks cache mutex poisoned") = Some(CacheEntry {
                    jwks,
                    fetched_at: Instant::now(),
                });
                found.ok_or_else(|| unauth("id token: no JWKS key matches the token kid"))
            }
        }
    }
}

/// Build a `jsonwebtoken` decoding key from a JWK's RSA components. The `n`/`e`
/// are base64url strings straight from the JWKS; `from_rsa_components` handles the
/// ASN.1 wrapping and hands the key to `ring`.
fn decoding_key(jwk: &Jwk) -> Result<DecodingKey> {
    if !jwk.kty.eq_ignore_ascii_case("RSA") {
        return Err(unauth("id token: matched JWKS key is not RSA"));
    }
    let (Some(n), Some(e)) = (&jwk.n, &jwk.e) else {
        return Err(unauth("id token: RSA JWK is missing modulus/exponent"));
    };
    DecodingKey::from_rsa_components(n, e)
        .map_err(|_| unauth("id token: RSA JWK has invalid modulus/exponent"))
}

/// Map a `jsonwebtoken` verification failure to a jerrycan 401 with a specific
/// reason. All map to `JC0401` (the token is not acceptable); the reason aids logs
/// without being something the client should act on.
fn map_jwt_error(e: jsonwebtoken::errors::Error) -> Error {
    use jsonwebtoken::errors::ErrorKind;
    let reason = match e.kind() {
        ErrorKind::InvalidToken => "malformed token",
        ErrorKind::InvalidSignature => "bad signature",
        ErrorKind::InvalidIssuer => "wrong issuer",
        ErrorKind::InvalidAudience => "wrong audience",
        ErrorKind::ExpiredSignature => "expired",
        ErrorKind::ImmatureSignature => "not yet valid (nbf)",
        ErrorKind::InvalidAlgorithm => "wrong algorithm",
        ErrorKind::MissingRequiredClaim(_) => "missing a required claim",
        _ => "verification failed",
    };
    unauth(format!("id token: {reason}"))
}

/// The `ring`-backed jsonwebtoken `CryptoProvider`, implementing **RS256 only**.
///
/// jsonwebtoken 10 dropped its built-in `ring` backend for pluggable providers,
/// and its two built-ins are both unacceptable here: `rust_crypto` pulls the
/// pure-Rust `rsa` crate (RUSTSEC-2023-0071, the Marvin timing side-channel —
/// exactly what this module's docs promise never enters the artifact) and
/// `aws_lc_rs` links a SECOND native crypto stack beside the `ring` the crate's
/// TLS already uses. So this module takes the third option jsonwebtoken
/// supports: a custom provider over `ring`'s RSA primitives — the same calls
/// jsonwebtoken 9 itself made.
///
/// Supporting only RS256 is deliberate defense-in-depth: even a caller that
/// somehow bypassed the header pin AND the `Validation` allowlist could not get
/// any other JWS algorithm evaluated by this process's provider. The trade-off
/// of jsonwebtoken's process-global provider model: if the embedding app uses
/// jsonwebtoken elsewhere with a non-RS256 algorithm, that use fails LOUDLY
/// with `InvalidAlgorithm` (never verifies against the wrong primitive).
mod ring_crypto {
    use jsonwebtoken::crypto::{CryptoProvider, JwkUtils, JwtSigner, JwtVerifier};
    use jsonwebtoken::errors::{Error as JwtError, ErrorKind, new_error};
    // jsonwebtoken re-exports the `signature` crate its provider traits build
    // on, so the trait versions can never skew from the ones `decode` expects.
    use jsonwebtoken::signature::{Error as SigError, Signer, Verifier};
    use jsonwebtoken::{Algorithm, AlgorithmFamily, DecodingKey, DecodingKeyKind, EncodingKey};

    /// RS256 signer over a PKCS#1-DER RSA private key (what
    /// `EncodingKey::from_rsa_pem` stores). Production code in this module only
    /// verifies; the signer exists for the in-crate test token mint.
    struct Rs256Signer(EncodingKey);

    impl Signer<Vec<u8>> for Rs256Signer {
        fn try_sign(&self, msg: &[u8]) -> std::result::Result<Vec<u8>, SigError> {
            let key_pair = ring::signature::RsaKeyPair::from_der(self.0.inner())
                .map_err(SigError::from_source)?;
            let mut sig = vec![0u8; key_pair.public().modulus_len()];
            key_pair
                .sign(
                    &ring::signature::RSA_PKCS1_SHA256,
                    &ring::rand::SystemRandom::new(),
                    msg,
                    &mut sig,
                )
                .map_err(SigError::from_source)?;
            Ok(sig)
        }
    }

    impl JwtSigner for Rs256Signer {
        fn algorithm(&self) -> Algorithm {
            Algorithm::RS256
        }
    }

    /// RS256 verifier over either `DecodingKey` shape: PKCS#1 DER (from PEM) or
    /// raw JWKS `(n, e)` components (the path [`super::decoding_key`] builds).
    struct Rs256Verifier(DecodingKey);

    impl Verifier<Vec<u8>> for Rs256Verifier {
        fn verify(&self, msg: &[u8], signature: &Vec<u8>) -> std::result::Result<(), SigError> {
            let params = &ring::signature::RSA_PKCS1_2048_8192_SHA256;
            match self.0.kind() {
                DecodingKeyKind::SecretOrDer(der) => {
                    ring::signature::UnparsedPublicKey::new(params, der).verify(msg, signature)
                }
                DecodingKeyKind::RsaModulusExponent { n, e } => {
                    ring::signature::RsaPublicKeyComponents { n, e }.verify(params, msg, signature)
                }
            }
            .map_err(SigError::from_source)
        }
    }

    impl JwtVerifier for Rs256Verifier {
        fn algorithm(&self) -> Algorithm {
            Algorithm::RS256
        }
    }

    /// The signer factory: RS256 with an RSA key, or a loud error.
    /// `pub(super)` so tests can pin the refusal directly.
    pub(super) fn signer_factory(
        algorithm: &Algorithm,
        key: &EncodingKey,
    ) -> std::result::Result<Box<dyn JwtSigner>, JwtError> {
        if *algorithm != Algorithm::RS256 {
            return Err(new_error(ErrorKind::InvalidAlgorithm));
        }
        if key.family() != AlgorithmFamily::Rsa {
            return Err(new_error(ErrorKind::InvalidKeyFormat));
        }
        Ok(Box::new(Rs256Signer(key.clone())))
    }

    /// The verifier factory: RS256 with an RSA key, or a loud error. Refusing
    /// every other algorithm HERE is the third, lowest-level RS256 pin.
    pub(super) fn verifier_factory(
        algorithm: &Algorithm,
        key: &DecodingKey,
    ) -> std::result::Result<Box<dyn JwtVerifier>, JwtError> {
        if *algorithm != Algorithm::RS256 {
            return Err(new_error(ErrorKind::InvalidAlgorithm));
        }
        if key.family() != AlgorithmFamily::Rsa {
            return Err(new_error(ErrorKind::InvalidKeyFormat));
        }
        Ok(Box::new(Rs256Verifier(key.clone())))
    }

    static PROVIDER: CryptoProvider = CryptoProvider {
        signer_factory,
        verifier_factory,
        // JWK-utility hooks are only reached via `DecodingKey::from_jwk` /
        // thumbprint APIs, which this crate never calls.
        jwk_utils: JwkUtils::new_unimplemented(),
    };

    /// Install the provider as the process default. Idempotent: after the first
    /// call (ours or anyone's) `install_default` is a no-op `Err`, which is fine
    /// — with neither backend feature enabled, the only way a default already
    /// exists is a prior successful install.
    pub(super) fn install() {
        let _ = PROVIDER.install_default();
    }
}

/// Whether a `jwks_uri` may be fetched. `https://` is always allowed; `http://` is
/// allowed ONLY to a loopback host (the in-repo test-server escape hatch), so a
/// misconfigured plaintext endpoint can't leak a fetch to a real network. Mirrors
/// the policy the OAuth [`crate::oauth::HttpTransport`] applies to token endpoints.
fn fetch_url_allowed(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    if scheme.eq_ignore_ascii_case("https") {
        return true;
    }
    if !scheme.eq_ignore_ascii_case("http") {
        return false;
    }
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .expect("split always yields at least one element");
    // A userinfo authority (`user@host`) hides the real host after the `@`; refuse
    // outright so a loopback-looking credential can't masquerade as the host.
    if authority.contains('@') {
        return false;
    }
    let host = if let Some(after) = authority.strip_prefix('[') {
        match after.split_once(']') {
            Some((inner, _port)) => inner,
            None => return false,
        }
    } else {
        authority.rsplit_once(':').map_or(authority, |(h, _)| h)
    };
    host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1"
}

/// The production [`JwksSource`]: a hyper + hyper-rustls client that GETs the JWKS
/// over HTTPS (rustls/`ring` + bundled `webpki-roots`). Plain `http://` is refused
/// except to a loopback host. Cheap to clone; reuse one across verifiers.
#[derive(Clone)]
pub struct HttpJwksSource {
    client: hyper_util::client::legacy::Client<
        hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
        http_body_util::Full<bytes::Bytes>,
    >,
}

impl Default for HttpJwksSource {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpJwksSource {
    /// Build the client with an explicit `ring` provider and the bundled Mozilla
    /// roots (same construction as [`crate::oauth::HttpTransport`], so it depends
    /// on no process-global rustls default being installed).
    pub fn new() -> Self {
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_provider_and_webpki_roots(rustls::crypto::ring::default_provider())
            .expect("ring provider supports rustls' safe default protocol versions")
            .https_or_http()
            .enable_http1()
            .build();
        let client =
            hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                .build(connector);
        Self { client }
    }
}

impl JwksSource for HttpJwksSource {
    fn fetch<'a>(&'a self, jwks_uri: &'a str) -> JwksFuture<'a> {
        Box::pin(async move {
            use http_body_util::BodyExt;

            if !fetch_url_allowed(jwks_uri) {
                return Err(Error::internal(
                    "id token: refusing to fetch a JWKS over plaintext http:// from a non-loopback host",
                ));
            }

            let request = hyper::Request::builder()
                .method(hyper::Method::GET)
                .uri(jwks_uri)
                .header(hyper::header::ACCEPT, "application/json")
                .body(http_body_util::Full::new(bytes::Bytes::new()))
                .map_err(|e| {
                    Error::internal(format!("id token: building JWKS request failed: {e}"))
                })?;

            let response = self
                .client
                .request(request)
                .await
                .map_err(|_| Error::internal("id token: JWKS request failed"))?;

            if !response.status().is_success() {
                return Err(Error::internal(format!(
                    "id token: JWKS endpoint returned status {}",
                    response.status().as_u16()
                )));
            }

            let bytes = response
                .into_body()
                .collect()
                .await
                .map_err(|_| Error::internal("id token: reading JWKS body failed"))?
                .to_bytes();

            serde_json::from_slice::<Jwks>(&bytes)
                .map_err(|_| Error::internal("id token: JWKS body was not a valid JSON key set"))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{EncodingKey, Header, encode};
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    // Two fixed test RSA keys (PKCS#1 PEM) generated with `openssl genrsa
    // -traditional 2048`, with their JWKS `n`/`e` extracted from the same key. The
    // private PEM signs tokens in-test; the (n, e) are what the injected JWKS
    // serves — so the signature we produce is exactly what the served key verifies.
    // No key generation at runtime, so the suite is deterministic and needs no
    // `rsa` crate.
    const K1_PEM: &[u8] = b"-----BEGIN RSA PRIVATE KEY-----
MIIEogIBAAKCAQEAsK9jEh54JKNv1D2tz5Xm6fkwU2vGqQHcCaciHJFF7XupV6BU
2UjYnFFBkoUoal8K6DL20nA6zQfLf2TO39Q+5rn+lLPOLVInaw4suFRcUuHqtP++
sFPauStGC2liskWu2Z+lqb8lllJSBnpyu0PPGE8jclM2OydUlGW7+RwZjEJPSMGX
3c2D8b152uT09Yb1vonv4IJv+a5qDS6+gqcQJsXUIBnEyFHa1ci7hA4vkDsgln0+
V/pb3ot5BdO8me50Z0XoTtcGVTv/ZMVutreMlboxEwbhGvM83sh0jhG8L0gUswcu
wiWfs97pRe3QBaXt4HZaDVV0sViaaV3MYiDo4wIDAQABAoIBAE3qIfm5Bwk9K5EA
XBgZRjmyol1/Px2DjOmS0wefBqPJ7y0Nrq0dIyyX5p866k4yOGiaJN87D6sfv10f
8tygx9ZOehZQTmENBAYBO7ZTuVzxdGO6DfjLGb6jdyGMKTJtaURd0xvOh8BI8BQc
RmEPb5GMQJjnWhhu6S0Bygl6G0gOrrCdVVGOiZFo0Wt/jewucbqGaELTzn/T7E7x
8FcRSV9lar0IKZ+xquzcbroK5oTf1vgA5mQavAtmwavzkRP9bjCmPSVfkXkQ9LIb
1r3QwAA0poKwp4YLuEC6+ZX2FhliMoxQDqam3NnmQW6Nxxnlvl/2YDDRSGoCjyTQ
owq7jaUCgYEA8ddpMIyvrW8LEQk0nHfR89h9erHXSqhg2aeKt7aYKKP5p+SK2fy4
SfLpD3VtRNhE12y5+MKXqDVDJ41W17R9SHm/RkWWTb6Yzxp+s9Dq/2mePZfHBz7l
V3Zj834z7b7pT8dgijUJF22Fl4QCGT4h/bD6kPALe3O15ahTZIr9Bj0CgYEAuwdy
r5BB63gPOrJ9xG6pt/4LMWUAtgCrZBcxOYEps0PT+tqzmwRnS765vvJuuPupEDZP
r1tLN82p+hAN+62wnF/uh62g4DIYeYyqobVocLG9QwcKRqx0Ku4Y6o1dX8Ecyopy
bgPllhUCTxaoo9bmDRwAq/Be0ZysTWVXInvLvZ8CgYAg8/svVFwzw6e8YIa8s072
bQ9cApOVZrAbuEqckdLV5tID4I5S+a6a1PCQ3K1Q7i8jM3t7u/gyQV+vKgElT0Cq
+XvotV6vpULpJXESS2tZ9ihLuDy0bguOCWHBMfcddCAScNZkvqlIefH0HVaz3dV/
3femfC70WWX1ryP91Tp4+QKBgF8At4bqljGP+NxuEmiXdeqaRwE+NxA8YtMi3MRD
EfWXfLQuJ5GUuQvGw/90kj2wx/4OOIfwrdKYy8DUKuYvIkksibOtxMxdZgVIKNyf
k3+7KVJE3zlrHE86RrnOOSIMrB1OGjY8EIEeBuA5uEwROyZplQXBwchj9zoRQiOo
EqQtAoGAENCYQnAdXCvnFB8GewpA0wnhwXJq1yPMVKb/VxraCbnfqHiJT+SUnSaG
x3D27dr7bFAItNBGtGJ/X6+fCxsrYukKrSDrb3I6XSqpFCzo5UHwitRz7dQQOyYt
87SPayioJDSeAytxSLT639U/7N8vCeTxB9SvgZRk40a5kB50/Dw=
-----END RSA PRIVATE KEY-----";
    const K1_N: &str = "sK9jEh54JKNv1D2tz5Xm6fkwU2vGqQHcCaciHJFF7XupV6BU2UjYnFFBkoUoal8K6DL20nA6zQfLf2TO39Q-5rn-lLPOLVInaw4suFRcUuHqtP--sFPauStGC2liskWu2Z-lqb8lllJSBnpyu0PPGE8jclM2OydUlGW7-RwZjEJPSMGX3c2D8b152uT09Yb1vonv4IJv-a5qDS6-gqcQJsXUIBnEyFHa1ci7hA4vkDsgln0-V_pb3ot5BdO8me50Z0XoTtcGVTv_ZMVutreMlboxEwbhGvM83sh0jhG8L0gUswcuwiWfs97pRe3QBaXt4HZaDVV0sViaaV3MYiDo4w";
    const K1_E: &str = "AQAB";

    const K2_PEM: &[u8] = b"-----BEGIN RSA PRIVATE KEY-----
MIIEpAIBAAKCAQEA3T4yqTf7Jwd/kcP9FhPrD4wjwDxFWRG52RW3dxZJeYmGVxMU
2SlxaOFCOBbCY8HYu/t6SrE2M0PdPWBMl6PfaNuEWOz51SzUeQsA6vCWsp1zE95M
YQGsJXF1x1MXcSjUkghMC0zzcBpxMU+09L0gngFsDTZfeZSuSJH7bMdsZZanj2aV
N3YRn6bCd/jeFlMjurV6XnZTkUcKZU/0pIiSP17oCohU5ban3vzpIcVdWGAJUUz8
/PXLMuKxPxa1oiHhBqiYfunGZr6Md3gAWyjYmat0E4oKfpd1FOmYAigXABNvSiJB
orWNxyRM1Phuqjmqjo8gj9nU1OTh/NBT+hkrUwIDAQABAoIBAAtqlxxsR4kEfSWZ
woijVnubyd5C6JaEBd4nK9YxwY1LhMADSdUCAK3syqWF2OhGq4/162keY9xEBY8D
5DcChG0u+G3KuuYFmqEtPtkZnKYBFz+Jb7TeM3KHn9dZsdnA/oG+JaO8FiAwi1XZ
Z4dCYHXVBrs7XF8z8OF3jhQCAtV5SIJiJjUKtYgqaxWb6w4GFBSZX3Br7ZxsgkEt
lnIPh/YUI733J91zbWxN5mQ7FLyeCGFxah+K0QwoEZwKKXvlzlSQKYLL7AKLOXDX
qlgt0E0vB5PTxndNx65nja8vzYuQLQw8LW18CobIpbEeEy6y/xLTAQf3gky5an15
Elt4H6ECgYEA/sLMVomyaOtKuT/Jt2MZUEgym00cLUO93TmPsUPrAz7pBklUG8nD
IuMfF8a3MUhJaio/fr2KsLA3bB/D8oQOcJ/5RcogowYSQlG5/rrlAL8wF01Ar5tT
fPdjHQZaJRbqHqsSirMh5X39xIYpnpSKYXlfhHecsc5rk7cziaDfPd8CgYEA3lGq
pvhiq1i/6mYvKw+6ufpKcmUsrUHnE7WAxWfmt3R+BUIlNrVJ3m5O/shSXQEtFn7D
I2cdaF+6nNvQ+z+jtKeH3pPOgr2wu1sNVYNi3E2YpIYKiR4XQ++OyKmzWBpjK66u
OM/XAzbpGHD5fB326iszSbXSYT/tIq+GQvTP2Q0CgYEAhCXxrrXweKIMeblP3jOm
btF0hsBh7EzmULnKAo6TenSIlX02BtAKy676cu/eGM9BXbOaihixt2NA7HIxxzue
7ebde8kUUtwUXphcHXk+zrtdq8ij1DODBCCjJewkmHahbNUaYh33aD6JgwaA0kSE
33kBBgqxmj3T6aSvNCXhhwsCgYEAp5vIXcOLmAT8A3rwerWMIGRLtj0C1siFrz06
jRmNPqhLzikVJ068Fz7wvXNHbSjS1k/RTKKT8Dmj1lh/ELzk7fEUJUEoAzeBw26c
+ehpIxA5UWhhDwkpnyU/b5dJR9X1CFzUqq4/OwQt7ihWXzW0Ds1tCFhU+M6aOHk+
bsJk5Q0CgYB0efHtGCHKSimoXGgwj0wsImqUYDuuVo00inG+8jBo1CEmVlO0Hk++
j4DUu0ODfOwEJSnCbqHKcuU1atUCMWzZzfsoP8JzJnemQwJV0FpDlIEqrRfR/S3i
f8sK4lep78Mx9ojs+u8a7fU3rOzqRoFcatjdno2JkI1Hd5siRAX1MA==
-----END RSA PRIVATE KEY-----";
    const K2_N: &str = "3T4yqTf7Jwd_kcP9FhPrD4wjwDxFWRG52RW3dxZJeYmGVxMU2SlxaOFCOBbCY8HYu_t6SrE2M0PdPWBMl6PfaNuEWOz51SzUeQsA6vCWsp1zE95MYQGsJXF1x1MXcSjUkghMC0zzcBpxMU-09L0gngFsDTZfeZSuSJH7bMdsZZanj2aVN3YRn6bCd_jeFlMjurV6XnZTkUcKZU_0pIiSP17oCohU5ban3vzpIcVdWGAJUUz8_PXLMuKxPxa1oiHhBqiYfunGZr6Md3gAWyjYmat0E4oKfpd1FOmYAigXABNvSiJBorWNxyRM1Phuqjmqjo8gj9nU1OTh_NBT-hkrUw";
    const K2_E: &str = "AQAB";

    const ISS: &str = "https://accounts.google.com";
    const AUD: &str = "my-client-id.apps.googleusercontent.com";

    fn now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_secs()
    }

    /// Build a `Jwk` (RSA) with the given kid and modulus/exponent.
    fn rsa_jwk(kid: &str, n: &str, e: &str) -> Jwk {
        Jwk {
            kty: "RSA".to_string(),
            kid: Some(kid.to_string()),
            alg: Some("RS256".to_string()),
            n: Some(n.to_string()),
            e: Some(e.to_string()),
        }
    }

    /// Sign `claims` as an RS256 JWS with `pem`, stamping the header `kid`.
    fn sign(pem: &[u8], kid: &str, claims: &serde_json::Value) -> String {
        // `encode` goes through the process CryptoProvider; make sure ours is
        // installed even when a test signs before building its `Verifier`.
        ring_crypto::install();
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(kid.to_string());
        let key = EncodingKey::from_rsa_pem(pem).expect("valid RSA PEM");
        encode(&header, claims, &key).expect("sign RS256")
    }

    /// Base64url (no pad) — the JWS segment encoding.
    fn b64url(bytes: &[u8]) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    }

    /// Hand-roll an HS256 token (header/claims/HMAC-SHA256 over the signing
    /// input) WITHOUT jsonwebtoken — the process provider refuses to sign
    /// anything but RS256, and a real attacker mints outside our process anyway.
    fn sign_hs256(secret: &[u8], kid: &str, claims: &serde_json::Value) -> String {
        use hmac::{Hmac, Mac};
        let header = json!({ "alg": "HS256", "typ": "JWT", "kid": kid });
        let signing_input = format!(
            "{}.{}",
            b64url(header.to_string().as_bytes()),
            b64url(claims.to_string().as_bytes())
        );
        let mut mac = Hmac::<sha2::Sha256>::new_from_slice(secret).expect("any key size works");
        mac.update(signing_input.as_bytes());
        let tag = mac.finalize().into_bytes();
        format!("{signing_input}.{}", b64url(&tag))
    }

    /// A well-formed, currently-valid Google-style claim set.
    fn good_claims() -> serde_json::Value {
        let n = now();
        json!({
            "iss": ISS,
            "aud": AUD,
            "sub": "user-abc-123",
            "email": "person@example.com",
            "email_verified": true,
            "iat": n - 10,
            "exp": n + 3600,
        })
    }

    /// An injectable JWKS source that (a) counts fetches and (b) can be swapped to
    /// model rotation. Holding an `Arc<TestSource>` lets a test both feed it to the
    /// verifier (as `Arc<dyn JwksSource>`) and inspect/mutate it.
    struct TestSource {
        jwks: Mutex<Jwks>,
        fetches: AtomicU64,
    }
    impl TestSource {
        fn new(keys: Vec<Jwk>) -> Arc<Self> {
            Arc::new(Self {
                jwks: Mutex::new(Jwks { keys }),
                fetches: AtomicU64::new(0),
            })
        }
        fn set(&self, keys: Vec<Jwk>) {
            self.jwks.lock().expect("test jwks poisoned").keys = keys;
        }
        fn fetch_count(&self) -> u64 {
            self.fetches.load(Ordering::SeqCst)
        }
    }
    impl JwksSource for TestSource {
        fn fetch<'a>(&'a self, _uri: &'a str) -> JwksFuture<'a> {
            self.fetches.fetch_add(1, Ordering::SeqCst);
            let jwks = self.jwks.lock().expect("test jwks poisoned").clone();
            Box::pin(async move { Ok(jwks) })
        }
    }

    /// A verifier over an injected source, expecting the Google-style iss/aud.
    fn verifier_with(source: Arc<dyn JwksSource>) -> Verifier {
        Verifier::custom([ISS.to_string()], "https://unused/jwks", [AUD.to_string()])
            .with_source(source)
    }

    #[tokio::test]
    async fn happy_path_verifies_signature_and_returns_claims() {
        // WHY: the whole point — a token actually signed by the provider's key,
        // with the right iss/aud and unexpired, must verify and yield its claims.
        let source = TestSource::new(vec![rsa_jwk("kid1", K1_N, K1_E)]);
        let v = verifier_with(source.clone());

        let token = sign(K1_PEM, "kid1", &good_claims());
        let claims = v.verify(&token).await.expect("valid token must verify");

        assert_eq!(claims.sub, "user-abc-123");
        assert_eq!(claims.iss, ISS);
        assert_eq!(claims.email.as_deref(), Some("person@example.com"));
        assert!(
            claims.email_verified(),
            "email_verified:true must be readable"
        );
        // A second verify reuses the cache (no second fetch).
        v.verify(&token).await.expect("second verify");
        assert_eq!(source.fetch_count(), 1, "fresh cache must not refetch");
    }

    #[tokio::test]
    async fn apple_style_string_email_verified_is_read_as_true() {
        // WHY: Apple sends email_verified as the STRING "true", not a bool — a
        // verifier that only understood the bool would silently treat verified
        // Apple emails as unverified.
        let source = TestSource::new(vec![rsa_jwk("kid1", K1_N, K1_E)]);
        let v = verifier_with(source);
        let n = now();
        let token = sign(
            K1_PEM,
            "kid1",
            &json!({
                "iss": ISS, "aud": AUD, "sub": "s", "email_verified": "true",
                "iat": n - 10, "exp": n + 3600,
            }),
        );
        let claims = v.verify(&token).await.expect("verify");
        assert!(
            claims.email_verified(),
            "string \"true\" must count as verified"
        );
    }

    #[tokio::test]
    async fn wrong_audience_is_rejected() {
        // WHY: an ID token minted for a DIFFERENT app (aud) must not authenticate
        // a user here — this is the token-substitution defense.
        let source = TestSource::new(vec![rsa_jwk("kid1", K1_N, K1_E)]);
        let v = verifier_with(source);
        let mut claims = good_claims();
        claims["aud"] = json!("some-other-app");
        let token = sign(K1_PEM, "kid1", &claims);
        let err = v.verify(&token).await.unwrap_err();
        assert_eq!(err.status().as_u16(), 401, "wrong aud must be 401");
    }

    #[tokio::test]
    async fn missing_aud_claim_is_rejected() {
        // WHY: `set_audience` only validates the aud VALUE when the claim is
        // present — without requiring presence, a validly-signed token that
        // simply OMITS `aud` would pass the audience check. OIDC id tokens
        // always carry `aud` (the client_id), so absence is never legitimate.
        let source = TestSource::new(vec![rsa_jwk("kid1", K1_N, K1_E)]);
        let v = verifier_with(source);
        let mut claims = good_claims();
        claims
            .as_object_mut()
            .expect("claims are an object")
            .remove("aud");
        let token = sign(K1_PEM, "kid1", &claims);
        let err = v.verify(&token).await.unwrap_err();
        assert_eq!(err.status().as_u16(), 401, "missing aud must be 401");
    }

    #[tokio::test]
    async fn wrong_issuer_is_rejected() {
        // WHY: a validly-signed token from an issuer we do not trust must fail —
        // otherwise any RSA key we happened to cache could mint identities.
        let source = TestSource::new(vec![rsa_jwk("kid1", K1_N, K1_E)]);
        let v = verifier_with(source);
        let mut claims = good_claims();
        claims["iss"] = json!("https://evil.example.com");
        let token = sign(K1_PEM, "kid1", &claims);
        let err = v.verify(&token).await.unwrap_err();
        assert_eq!(err.status().as_u16(), 401, "wrong iss must be 401");
    }

    #[tokio::test]
    async fn expired_token_is_rejected() {
        // WHY: a stolen-but-expired token must not authenticate; exp is enforced
        // (beyond the leeway).
        let source = TestSource::new(vec![rsa_jwk("kid1", K1_N, K1_E)]);
        let v = verifier_with(source).with_leeway(0);
        let n = now();
        let token = sign(
            K1_PEM,
            "kid1",
            &json!({ "iss": ISS, "aud": AUD, "sub": "s", "iat": n - 7200, "exp": n - 3600 }),
        );
        let err = v.verify(&token).await.unwrap_err();
        assert_eq!(err.status().as_u16(), 401, "expired must be 401");
    }

    #[tokio::test]
    async fn not_yet_valid_nbf_is_rejected() {
        // WHY: a token whose nbf is in the future is not yet usable and must fail.
        let source = TestSource::new(vec![rsa_jwk("kid1", K1_N, K1_E)]);
        let v = verifier_with(source).with_leeway(0);
        let n = now();
        let token = sign(
            K1_PEM,
            "kid1",
            &json!({ "iss": ISS, "aud": AUD, "sub": "s", "nbf": n + 3600, "exp": n + 7200 }),
        );
        let err = v.verify(&token).await.unwrap_err();
        assert_eq!(err.status().as_u16(), 401, "future nbf must be 401");
    }

    #[tokio::test]
    async fn tampered_signature_is_rejected() {
        // WHY: flipping the payload after signing (privilege escalation attempt)
        // must fail the RS256 signature check.
        let source = TestSource::new(vec![rsa_jwk("kid1", K1_N, K1_E)]);
        let v = verifier_with(source);
        let token = sign(K1_PEM, "kid1", &good_claims());
        // Corrupt the last signature char (base64url alphabet swap).
        let mut bytes = token.into_bytes();
        let last = bytes.last_mut().expect("non-empty token");
        *last = if *last == b'A' { b'B' } else { b'A' };
        let tampered = String::from_utf8(bytes).expect("still ascii");
        let err = v.verify(&tampered).await.unwrap_err();
        assert_eq!(err.status().as_u16(), 401, "bad signature must be 401");
    }

    #[tokio::test]
    async fn signature_from_a_key_not_in_the_jwks_is_rejected() {
        // WHY: the signature must be checked against the JWKS-published key for the
        // claimed kid — not merely parsed. Here the token is signed with K2 but the
        // JWKS serves K1's public key under "kid1" (the kid the header claims), so
        // verification MUST fail. This catches a verifier that selects a key by kid
        // but forgets to actually bind the signature to it.
        let source = TestSource::new(vec![rsa_jwk("kid1", K1_N, K1_E)]);
        let v = verifier_with(source);
        let token = sign(K2_PEM, "kid1", &good_claims());
        let err = v.verify(&token).await.unwrap_err();
        assert_eq!(err.status().as_u16(), 401, "mismatched key must be 401");
    }

    #[tokio::test]
    async fn hs256_token_is_rejected_before_key_lookup() {
        // WHY: the classic alg-confusion attack — present an HS256 token so the
        // server treats the RSA public key as an HMAC secret. Pinning RS256 must
        // reject it outright, without ever consulting the JWKS.
        let source = TestSource::new(vec![rsa_jwk("kid1", K1_N, K1_E)]);
        let v = verifier_with(source.clone());
        let token = sign_hs256(b"attacker-chosen", "kid1", &good_claims());
        let err = v.verify(&token).await.unwrap_err();
        assert_eq!(err.status().as_u16(), 401, "HS256 must be 401");
        assert_eq!(
            source.fetch_count(),
            0,
            "a non-RS256 token must be rejected before any JWKS fetch"
        );
    }

    #[tokio::test]
    async fn hs256_keyed_with_the_rsa_jwk_material_is_rejected() {
        // WHY: the exact GHSA-h395-gr6q-cpjc / key-confusion vector — an HMAC
        // token whose secret IS the published RSA public key material, hoping a
        // confused verifier feeds the RSA key to an HMAC primitive. Both
        // plausible secret spellings (the base64url modulus string an attacker
        // reads from the JWKS, and its decoded bytes) must fail, without a fetch.
        use base64::Engine as _;
        let source = TestSource::new(vec![rsa_jwk("kid1", K1_N, K1_E)]);
        let v = verifier_with(source.clone());
        let n_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(K1_N)
            .expect("K1_N is valid base64url");
        for secret in [K1_N.as_bytes(), n_bytes.as_slice()] {
            let token = sign_hs256(secret, "kid1", &good_claims());
            let err = v.verify(&token).await.unwrap_err();
            assert_eq!(err.status().as_u16(), 401, "RSA-keyed HS256 must be 401");
        }
        assert_eq!(source.fetch_count(), 0, "rejected before any JWKS fetch");
    }

    #[tokio::test]
    async fn alg_none_token_is_rejected() {
        // WHY: the unsigned-token hole — `alg: none` asserts "no signature to
        // check". Accepting it means anyone can mint identities. It must be
        // rejected at the header (jsonwebtoken's Algorithm has no `none`, so the
        // header does not even parse), with or without a trailing signature part,
        // and without ever touching the JWKS.
        let source = TestSource::new(vec![rsa_jwk("kid1", K1_N, K1_E)]);
        let v = verifier_with(source.clone());
        let header = b64url(br#"{"alg":"none","typ":"JWT","kid":"kid1"}"#);
        let claims = b64url(good_claims().to_string().as_bytes());
        for token in [
            format!("{header}.{claims}."),
            format!("{header}.{claims}"),
            format!("{header}.{claims}.{}", b64url(b"junk")),
        ] {
            let err = v.verify(&token).await.unwrap_err();
            assert_eq!(err.status().as_u16(), 401, "alg:none must be 401");
        }
        assert_eq!(source.fetch_count(), 0, "rejected before any JWKS fetch");
    }

    #[test]
    fn crypto_provider_refuses_every_algorithm_but_rs256() {
        // WHY: the third, lowest-level pin. Even if BOTH the header check and the
        // Validation allowlist were bypassed, the process CryptoProvider must be
        // unable to produce a verifier (or signer) for any non-RS256 algorithm —
        // notably HS256 over an RSA key, the confusion class this release patches.
        let rsa_key = DecodingKey::from_rsa_components(K1_N, K1_E).expect("valid components");
        let hmac_key = DecodingKey::from_secret(b"attacker-chosen");
        for alg in [
            Algorithm::HS256,
            Algorithm::HS384,
            Algorithm::HS512,
            Algorithm::ES256,
            Algorithm::ES384,
            Algorithm::RS384,
            Algorithm::RS512,
            Algorithm::PS256,
            Algorithm::PS384,
            Algorithm::PS512,
            Algorithm::EdDSA,
        ] {
            assert!(
                ring_crypto::verifier_factory(&alg, &rsa_key).is_err(),
                "provider must refuse to verify {alg:?}"
            );
            assert!(
                ring_crypto::signer_factory(&alg, &EncodingKey::from_secret(b"s")).is_err(),
                "provider must refuse to sign {alg:?}"
            );
        }
        // And RS256 with an HMAC-shaped key is a key-format refusal.
        assert!(
            ring_crypto::verifier_factory(&Algorithm::RS256, &hmac_key).is_err(),
            "RS256 over a non-RSA key must be refused"
        );
        // The one legitimate combination works.
        assert!(
            ring_crypto::verifier_factory(&Algorithm::RS256, &rsa_key).is_ok(),
            "RS256 over the RSA JWK must be accepted"
        );
    }

    #[tokio::test]
    async fn unknown_kid_refetches_once_and_verifies_after_rotation() {
        // WHY: providers rotate signing keys. A token bearing a kid we have not
        // cached must trigger exactly one refetch; if the rotated key is now
        // published, the token must verify. (min_refetch=0 so the rotation is not
        // blocked by the anti-DoS cooldown in this same-instant test.)
        let source = TestSource::new(vec![rsa_jwk("kid1", K1_N, K1_E)]);
        let v = verifier_with(source.clone()).with_min_refetch_interval(Duration::ZERO);

        // Prime the cache with kid1.
        let t1 = sign(K1_PEM, "kid1", &good_claims());
        v.verify(&t1).await.expect("kid1 verifies");
        assert_eq!(source.fetch_count(), 1);

        // Provider rotates: now serves kid1 AND a new kid2 (K2).
        source.set(vec![
            rsa_jwk("kid1", K1_N, K1_E),
            rsa_jwk("kid2", K2_N, K2_E),
        ]);
        let t2 = sign(K2_PEM, "kid2", &good_claims());
        let claims = v.verify(&t2).await.expect("rotated kid2 verifies");
        assert_eq!(claims.sub, "user-abc-123");
        assert_eq!(
            source.fetch_count(),
            2,
            "unknown kid must refetch exactly once"
        );
    }

    #[tokio::test]
    async fn bogus_kid_within_cooldown_does_not_refetch() {
        // WHY: an attacker streaming tokens with random unknown kids must NOT be
        // able to make us hammer the provider's JWKS endpoint. Within the refetch
        // cooldown, an unknown kid fails WITHOUT a second fetch.
        let source = TestSource::new(vec![rsa_jwk("kid1", K1_N, K1_E)]);
        // Default cooldown (60s): the bogus request lands well inside it.
        let v = verifier_with(source.clone());

        v.verify(&sign(K1_PEM, "kid1", &good_claims()))
            .await
            .expect("prime cache");
        assert_eq!(source.fetch_count(), 1);

        let bogus = sign(K1_PEM, "nope-unknown-kid", &good_claims());
        let err = v.verify(&bogus).await.unwrap_err();
        assert_eq!(err.status().as_u16(), 401);
        assert_eq!(
            source.fetch_count(),
            1,
            "unknown kid inside the cooldown must not trigger a refetch"
        );
    }

    #[tokio::test]
    async fn expired_cache_refetches_on_next_verify() {
        // WHY: the TTL contract — once the cached JWKS is stale, the next verify
        // must refetch (so revoked keys stop verifying and new keys are picked up).
        let source = TestSource::new(vec![rsa_jwk("kid1", K1_N, K1_E)]);
        let v = verifier_with(source.clone()).with_ttl(Duration::ZERO);
        let token = sign(K1_PEM, "kid1", &good_claims());

        v.verify(&token).await.expect("first");
        v.verify(&token).await.expect("second");
        assert_eq!(
            source.fetch_count(),
            2,
            "a zero-TTL cache must refetch every verify"
        );
    }

    #[tokio::test]
    async fn nonce_must_match_when_required() {
        // WHY: nonce binds the token to a single sign-in attempt (replay/CSRF).
        // A matching nonce passes; a wrong or absent nonce fails.
        let source = TestSource::new(vec![rsa_jwk("kid1", K1_N, K1_E)]);
        let v = verifier_with(source).require_nonce("expected-nonce-xyz");

        let mut claims = good_claims();
        claims["nonce"] = json!("expected-nonce-xyz");
        v.verify(&sign(K1_PEM, "kid1", &claims))
            .await
            .expect("matching nonce verifies");

        claims["nonce"] = json!("attacker-nonce");
        let err = v.verify(&sign(K1_PEM, "kid1", &claims)).await.unwrap_err();
        assert_eq!(err.status().as_u16(), 401, "wrong nonce must be 401");

        // No nonce at all, but one was required.
        let err = v
            .verify(&sign(K1_PEM, "kid1", &good_claims()))
            .await
            .unwrap_err();
        assert_eq!(err.status().as_u16(), 401, "missing nonce must be 401");
    }

    #[tokio::test]
    async fn google_preset_accepts_both_issuer_spellings() {
        // WHY: Google emits `iss` as BOTH "https://accounts.google.com" and the
        // bare "accounts.google.com". The preset must accept either, or half of
        // real Google tokens would be rejected.
        let source = TestSource::new(vec![rsa_jwk("kid1", K1_N, K1_E)]);
        for iss in ["https://accounts.google.com", "accounts.google.com"] {
            let v = Verifier::google("client-xyz").with_source(source.clone());
            let n = now();
            let token = sign(
                K1_PEM,
                "kid1",
                &json!({ "iss": iss, "aud": "client-xyz", "sub": "s", "iat": n - 10, "exp": n + 3600 }),
            );
            v.verify(&token)
                .await
                .unwrap_or_else(|e| panic!("google preset must accept iss {iss}: {e}"));
        }
    }

    #[test]
    fn jwks_fetch_url_guard_allows_https_and_loopback_only() {
        // WHY: a JWKS must never be fetched over plaintext from a real host (a MITM
        // could swap the signing keys). https is always fine; http only to loopback.
        assert!(fetch_url_allowed("https://appleid.apple.com/auth/keys"));
        assert!(fetch_url_allowed("http://127.0.0.1:8080/jwks"));
        assert!(fetch_url_allowed("http://localhost/jwks"));
        assert!(fetch_url_allowed("http://[::1]:9000/jwks"));
        assert!(!fetch_url_allowed("http://evil.example.com/jwks"));
        assert!(!fetch_url_allowed("http://localhost.evil.com/jwks"));
        assert!(!fetch_url_allowed("http://127.0.0.1@evil.com/jwks"));
        assert!(!fetch_url_allowed("ftp://127.0.0.1/jwks"));
        assert!(!fetch_url_allowed("not-a-url"));
    }

    #[tokio::test]
    async fn http_jwks_source_refuses_plaintext_non_loopback_without_network() {
        // WHY: the production source must refuse a misconfigured plaintext, non-
        // loopback JWKS URL BEFORE opening a socket — the guard, not a connect error.
        let source = HttpJwksSource::new();
        let err = source
            .fetch("http://keys.evil.example.com/jwks")
            .await
            .unwrap_err();
        assert!(
            err.message().contains("refusing"),
            "expected the plaintext guard, got: {err}"
        );
    }

    #[tokio::test]
    async fn http_jwks_source_fetches_and_parses_over_a_real_localhost_socket() {
        // WHY: exercise the REAL production path — the hyper GET + body collect +
        // JSON parse in HttpJwksSource — against a real socket, then verify a token
        // end-to-end through it. The injected-source tests never touch hyper; this
        // proves HttpJwksSource itself works.
        use jerrycan_core::http::header;
        use jerrycan_core::{App, JcBody, Response, get};

        let jwks_json = json!({ "keys": [ {
            "kty": "RSA", "kid": "kid1", "alg": "RS256", "use": "sig",
            "n": K1_N, "e": K1_E,
        } ] })
        .to_string();

        // A tiny jerrycan app that serves the JWKS at /jwks.
        let body_for_route = jwks_json.clone();
        let app = App::new().route(
            "/jwks",
            get(move || {
                let body = body_for_route.clone();
                async move {
                    let mut resp: Response =
                        jerrycan_core::http::Response::new(JcBody::full(body.into_bytes()));
                    resp.headers_mut().insert(
                        header::CONTENT_TYPE,
                        header::HeaderValue::from_static("application/json"),
                    );
                    Ok::<_, Error>(resp)
                }
            }),
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local_addr");
        let server = tokio::spawn(async move {
            let _ = app.serve_with(listener).await;
        });

        let jwks_uri = format!("http://{addr}/jwks");
        let v = Verifier::custom([ISS.to_string()], jwks_uri, [AUD.to_string()]);
        // (default source is HttpJwksSource — exactly what we want to exercise)

        let token = sign(K1_PEM, "kid1", &good_claims());
        let claims = v
            .verify(&token)
            .await
            .expect("verify via real http JWKS fetch");
        assert_eq!(claims.sub, "user-abc-123");

        server.abort();
    }
}
