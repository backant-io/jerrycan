# jerrycan Phase 3 — Production Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Production readiness — `jerrycan-auth` (password hashing, encrypted sessions, JWT, guards), `jerrycan-observe` (structured logs, request IDs, `/healthz`, `/metrics`), and `jerrycan package` (musl binary, hardened Docker/k8s/systemd + SBOM) — exiting at the spec §11 criterion: *the golden app deploys to Docker + k8s + bare server from one command*.

**Architecture:** Two new extension crates attach through the existing §6 `Extension` trait and reach apps via facade features (`jerrycan = { features = ["auth", "observe"] }`), exactly like db/validate from Phase 2. Auth uses **vetted RustCrypto primitives** (`argon2`, `chacha20poly1305`, `hmac`+`sha2`, `base64`) but **hand-rolls the JWT envelope and the cookie codec** — no monolithic auth framework, every byte path reviewable, `forbid(unsafe_code)` intact. Observe uses `tracing` + `tracing-subscriber` for JSON logs but hand-rolls the Prometheus text format and the counter registry (no `prometheus`/`metrics` crate). `jerrycan package` is a platform command that runs the §4.4 check gate first, then emits artifacts; the CycloneDX SBOM is built from `cargo metadata` (already available) — no `cargo-cyclonedx` dependency.

**Tech Stack:** New workspace deps (auth crate only): `argon2 0.5`, `chacha20poly1305 0.10`, `hmac 0.12`, `sha2 0.10`, `base64 0.22`, `rand 0.8` (CSPRNG for nonces/salts). Observe: `tracing 0.1`, `tracing-subscriber 0.3` (json feature). All RustCrypto crates are `#![forbid(unsafe_code)]`-clean pure Rust. No new platform tools — `docker`, `kubectl`/`kubeconform` are invoked if present and gated otherwise.

**Pinned design decisions (the architect's calls — do not relitigate):**
1. **RustCrypto primitives, hand-rolled envelopes.** We do NOT implement argon2/chacha/hmac ourselves (that would be the security anti-pattern §2 warns against). We DO hand-roll: the JWT `header.payload.signature` assembly + HS256 sign/verify (over `hmac`), and the session cookie `base64(nonce ‖ ciphertext)` codec (over `chacha20poly1305`). Reviewable, dependency-light, standard.
2. **Sessions are encrypted+signed (AEAD), JWT is signed-only.** ChaCha20-Poly1305 gives confidentiality+integrity for cookies (server-private state). JWTs are HS256 (signed, readable) — the bearer-token interop format; never put secrets in a JWT.
3. **One secret, derived.** `JERRYCAN_SECRET` (>=32 bytes) seeds both: the session key = `HKDF-less` simple `SHA256(secret ‖ "session")[..32]`, the JWT key = `SHA256(secret ‖ "jwt")`. Missing/short secret in production = loud startup error; dev gets a fixed dev-only key with a stderr warning.
4. **Guards are dependencies (spec §4.3).** `CurrentUser`/`Session`/`JwtClaims` are `FromRequest` extractors returning `401 JC0401`; role checks are a generated guard dependency returning `403 JC0403`. No new middleware for auth.
5. **observe is opt-in but recommended.** Request-id + JSON access logging is a `Middleware`; `/healthz` and `/metrics` are routes added by the `Observe` extension. Metrics are a tiny global atomic registry (requests_total, requests_in_flight, request_duration buckets) rendered as Prometheus text — no histogram crate.
6. **`jerrycan package` never deploys.** It emits artifacts (binary, Docker image OR Dockerfile, k8s YAML, systemd unit, SBOM) after a green check. Execution (push/apply/ssh) stays the agent's job (spec non-goal).
7. **musl with graceful fallback.** `--binary` targets `x86_64-unknown-linux-musl` for a static binary; if the target/linker is absent, it falls back to the host target with a clear note (gnu binary still works in the distroless image via a glibc base when musl is unavailable — the Dockerfile picks the base accordingly).
8. New stable codes: `JC0401` (401, "authentication required"), `JC0403` (403, "forbidden"). New lint `JL0004` (mutating route in an auth design without a guard dependency).

---

## File Structure

```
Cargo.toml                                   # MODIFY: [workspace.dependencies] argon2/chacha/hmac/sha2/base64/rand/tracing(+subscriber)
crates/jerrycan-core/src/
├── error.rs                                 # MODIFY: unauthorized() JC0401, forbidden() JC0403 + tests
└── test_client.rs                           # MODIFY: header-carrying requests (get_with/header builder) for auth tests
crates/jerrycan-auth/                        # REPLACE placeholder
├── Cargo.toml
└── src/
    ├── lib.rs                               # Auth extension, secret derivation, re-exports
    ├── password.rs                          # hash_password / verify_password (argon2)
    ├── session.rs                           # SessionStore (AEAD codec), Session extractor, login/logout
    ├── jwt.rs                               # HS256 encode/decode, JwtClaims extractor
    └── guard.rs                             # CurrentUser, require_role helper (→ 403)
crates/jerrycan-observe/                     # REPLACE placeholder
├── Cargo.toml
└── src/
    ├── lib.rs                               # Observe extension, init_logging
    ├── metrics.rs                           # atomic registry + Prometheus text render
    └── access_log.rs                        # request-id + JSON access-log middleware
crates/jerrycan/
├── Cargo.toml                               # MODIFY: optional auth/observe deps + features; package deps (flate2/tar? NO — see Task)
├── src/lib.rs                               # MODIFY: cfg re-exports jerrycan::auth / ::observe + doc mounts
└── src/platform/
    ├── design.rs                            # MODIFY: wants_auth()/wants_observe()
    ├── genroute.rs                          # MODIFY: auth-mode guard params in stubs; observe untouched-per-route
    ├── mounting.rs                          # MODIFY: auth/observe extension wiring in main.rs
    ├── scaffold.rs                          # MODIFY: feature list += auth/observe
    ├── templates.rs                         # MODIFY: (features only — reuses inject_features)
    ├── questions.rs                         # MODIFY: auth design coherence (roles non-empty if guards used)
    ├── lints.rs                             # MODIFY: JL0004 auth-guard lint
    ├── testgen.rs                           # MODIFY: 401 cases for auth_required endpoints; credentialed flow preamble
    ├── sbom.rs                              # CREATE: cargo metadata → CycloneDX 1.5 JSON
    └── package.rs                           # CREATE: artifact emitters (binary/docker/k8s/systemd) + check gate
crates/jerrycan/src/main.rs                  # MODIFY: `package` command
crates/jerrycan/tests/
├── authgen.rs                               # CREATE: auth-mode generation assertions (fast)
├── package.rs                               # CREATE: artifact-emission assertions (fast, no real builds)
└── conformance.rs                           # MODIFY: deploy-anywhere heavy tests (binary/docker/k8s)
conformance/fixtures/auth/                   # CREATE: auth-aware handler fixtures (guarded handlers)
docs/ai/10-auth.md                           # CREATE: doc-tested
docs/ai/11-observability.md                  # CREATE: doc-tested
docs/ai/12-packaging.md                      # CREATE: prose (package command reference)
docs/ai/05-errors.md                         # MODIFY: JC0401/JC0403 rows
docs/contracts/cli-ux.md                     # MODIFY: package row detail; add row already present
docs/contracts/design-schema.json            # MODIFY: auth.roles guidance; reserved names auth/observe
.github/workflows/ci.yml                     # MODIFY: musl target + musl-tools; docker available on ubuntu runners
README.md / docs/phase1-backlog.md           # MODIFY: roadmap flip; backlog
```

**Conventions (as Phase 2):** gates are `cargo fmt --all` && `cargo clippy --workspace --all-targets --all-features -- -D warnings` && `cargo test --workspace --all-features` before EVERY commit. Plain commit messages; `#![forbid(unsafe_code)]` in every crate; heavy tests `#[ignore]`; plan-code compile failures fixed minimally + recorded; design-level walls → BLOCKED. **Crypto rule:** never hand-implement a primitive — use the RustCrypto crate; only hand-roll envelopes/codecs as decision #1 specifies.

---

### Task 1: Core — `JC0401`/`JC0403` + header-carrying test requests

**Files:**
- Modify: `crates/jerrycan-core/src/error.rs`, `crates/jerrycan-core/src/test_client.rs`

- [ ] **Step 1: Write the failing tests**

In `error.rs` `mod tests`, extend `errors_carry_status_and_stable_code`:

```rust
        assert_eq!(Error::unauthorized().code(), "JC0401");
        assert_eq!(Error::unauthorized().status(), StatusCode::UNAUTHORIZED);
        assert_eq!(Error::forbidden().code(), "JC0403");
        assert_eq!(Error::forbidden().status(), StatusCode::FORBIDDEN);
```

In `test_client.rs` `mod tests` (create the module if absent; otherwise append) — but simplest is an integration test. Add to `crates/jerrycan-core/tests/hardening.rs`:

```rust
#[tokio::test]
async fn test_requests_can_carry_headers() {
    use jerrycan_core::{get, App, Headers};
    async fn echo_auth(headers: Headers) -> String {
        headers.get("authorization").unwrap_or("none").to_string()
    }
    let t = App::new().route("/h", get(echo_auth)).into_test();
    assert_eq!(t.get("/h").await.text(), "none");
    assert_eq!(t.get_with("/h", &[("authorization", "Bearer xyz")]).await.text(), "Bearer xyz");
}
```

NOTE: this test also assumes a `Headers` extractor exists in core. If it does not yet (Phase 0 shipped Path/Query/Json/Dep but the spec §4.1 lists `Headers`), this task ADDS it — see Step 3b.

- [ ] **Step 2: Run to verify failure** — `cargo test -p jerrycan-core` → compile FAIL.

- [ ] **Step 3a: Implement the error constructors** (`error.rs`, after `unprocessable`):

```rust
    /// Authentication is required or failed (spec §4.4 auth).
    pub fn unauthorized() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "JC0401", "authentication required")
    }
    /// Authenticated but not permitted.
    pub fn forbidden() -> Self {
        Self::new(StatusCode::FORBIDDEN, "JC0403", "forbidden")
    }
```

- [ ] **Step 3b: Add the `Headers` extractor if missing** (`extract.rs`)

Check whether `Headers` exists. If not, add:

```rust
/// Read-only access to request headers in a handler signature.
pub struct Headers(pub(crate) http::HeaderMap);

impl Headers {
    /// Header value as a &str, or None if absent or non-ASCII.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).and_then(|v| v.to_str().ok())
    }
}

impl FromRequest for Headers {
    async fn from_request(ctx: &mut RequestCtx) -> Result<Self> {
        Ok(Headers(ctx.headers().clone()))
    }
}
```

Re-export `Headers` from `lib.rs` and add to prelude.

- [ ] **Step 3c: Header-carrying test requests** (`test_client.rs`)

Add a header-aware request path. `TestApp` request methods currently build header-less requests; add:

```rust
impl TestApp {
    /// GET with explicit request headers (auth tests, content negotiation).
    pub async fn get_with(&self, path: &str, headers: &[(&str, &str)]) -> TestResponse {
        self.request_with(Method::GET, path, None, headers).await
    }

    /// POST JSON with explicit request headers.
    pub async fn post_json_with<B: Serialize>(&self, path: &str, body: &B, headers: &[(&str, &str)]) -> TestResponse {
        self.request_with(Method::POST, path, Some(serde_json::to_vec(body).expect("serialize")), headers).await
    }

    async fn request_with(
        &self,
        method: Method,
        path: &str,
        json: Option<Vec<u8>>,
        headers: &[(&str, &str)],
    ) -> TestResponse {
        let mut builder = http::Request::builder().method(method).uri(path);
        if json.is_some() {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
        }
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let req = builder.body(()).expect("test request build");
        let (parts, ()) = req.into_parts();
        let body = Bytes::from(json.unwrap_or_default());
        TestResponse::collect(self.built.dispatch(parts, body).await).await
    }
}
```

REFACTOR the existing `request` to delegate: `self.request_with(method, path, json, &[]).await` (avoids duplicating the builder). Confirm the field is named `built` (match the existing struct; if it's different, use that name — record it).

- [ ] **Step 4: Run to verify pass** — `cargo test -p jerrycan-core` green; the header echo + error code tests pass. Full `--all-features` gate green.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan-core/src/error.rs crates/jerrycan-core/src/extract.rs crates/jerrycan-core/src/test_client.rs crates/jerrycan-core/src/lib.rs crates/jerrycan-core/tests/hardening.rs
git commit -m "Add JC0401/JC0403 codes, Headers extractor, and header-carrying test requests"
```

---

### Task 2: `jerrycan-auth` — password hashing + secret derivation

**Files:**
- Modify: `Cargo.toml` (workspace deps)
- Replace: `crates/jerrycan-auth/Cargo.toml`, `crates/jerrycan-auth/src/lib.rs`
- Create: `crates/jerrycan-auth/src/password.rs`

- [ ] **Step 1: Workspace deps**

Root `Cargo.toml` `[workspace.dependencies]`:

```toml
argon2 = "0.5"
chacha20poly1305 = "0.10"
hmac = "0.12"
sha2 = "0.10"
base64 = "0.22"
rand = "0.8"
jerrycan-auth = { path = "crates/jerrycan-auth", version = "0.0.0" }
jerrycan-observe = { path = "crates/jerrycan-observe", version = "0.0.0" }
```

`crates/jerrycan-auth/Cargo.toml`:

```toml
[package]
name = "jerrycan-auth"
description = "Authentication extension for the jerrycan framework: argon2 password hashing, encrypted sessions, JWT, role guards. Real releases begin at 0.1.0. https://jerrycan.cc"
version.workspace = true
edition.workspace = true
license.workspace = true
homepage.workspace = true

[dependencies]
jerrycan-core = { path = "../jerrycan-core", version = "0.0.0" }
serde.workspace = true
serde_json.workspace = true
argon2.workspace = true
chacha20poly1305.workspace = true
hmac.workspace = true
sha2.workspace = true
base64.workspace = true
rand.workspace = true

[dev-dependencies]
tokio.workspace = true
```

- [ ] **Step 2: Write the failing tests** (`crates/jerrycan-auth/src/password.rs`)

```rust
//! Password hashing via argon2 (RustCrypto). We never invent crypto — argon2
//! does the KDF; we expose a thin, misuse-resistant pair.

use jerrycan_core::{Error, Result};

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
```

- [ ] **Step 3: Run to verify failure** — `cargo test -p jerrycan-auth` → compile FAIL.

- [ ] **Step 4: Implement** (above the tests)

```rust
use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;

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
    let parsed = PasswordHash::new(phc).map_err(|e| Error::internal(format!("stored hash is malformed: {e}")))?;
    Ok(Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok())
}
```

`crates/jerrycan-auth/src/lib.rs` (skeleton — secret derivation + module wiring; extended in later tasks):

```rust
//! Authentication for jerrycan: argon2 password hashing, AEAD session cookies,
//! HS256 JWTs, role guards. Vetted RustCrypto primitives; hand-rolled envelopes
//! (see module docs). #![forbid(unsafe_code)].
#![forbid(unsafe_code)]

use sha2::{Digest, Sha256};

pub mod password;

pub use password::{hash_password, verify_password};

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
```

- [ ] **Step 5: Run to verify pass** — `cargo test -p jerrycan-auth` → 4 tests PASS (3 password + 1 secret). Full gate green.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock crates/jerrycan-auth
git commit -m "Add jerrycan-auth with argon2 password hashing and key derivation"
```

---
### Task 3: `jerrycan-auth` — encrypted session cookies (`Session`)

**Files:**
- Create: `crates/jerrycan-auth/src/session.rs`
- Modify: `crates/jerrycan-auth/src/lib.rs`

- [ ] **Step 1: Write the failing tests** (`session.rs`)

```rust
//! Session cookies: server-private state, ChaCha20-Poly1305 AEAD (confidential
//! + tamper-evident). Wire format: base64url(nonce[12] ‖ ciphertext+tag).
//! The cookie is Secure/HttpOnly/SameSite=Lax by default (spec §4.4).

use jerrycan_core::{Error, Result};
use serde::{de::DeserializeOwned, Serialize};

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct Sess { user_id: i64, role: String }

    fn store() -> SessionStore {
        SessionStore::new(&crate::derive_key(b"a-very-long-development-secret-string!!", "session"))
    }

    #[test]
    fn encrypt_then_decrypt_round_trips() {
        let s = store();
        let token = s.encode(&Sess { user_id: 7, role: "admin".into() }).unwrap();
        let back: Sess = s.decode(&token).unwrap();
        assert_eq!(back, Sess { user_id: 7, role: "admin".into() });
    }

    #[test]
    fn tokens_are_opaque_and_nonce_randomized() {
        let s = store();
        let a = s.encode(&Sess { user_id: 1, role: "u".into() }).unwrap();
        let b = s.encode(&Sess { user_id: 1, role: "u".into() }).unwrap();
        assert_ne!(a, b, "fresh nonce per encode");
        assert!(!a.contains("user_id"), "ciphertext is opaque: {a}");
    }

    #[test]
    fn tampering_is_rejected() {
        let s = store();
        let mut token = s.encode(&Sess { user_id: 1, role: "u".into() }).unwrap();
        // Flip a character in the middle of the base64 payload.
        let mid = token.len() / 2;
        let bytes = flip_one_char(&token, mid);
        token = bytes;
        assert!(s.decode::<Sess>(&token).is_err(), "AEAD must reject tampering");
    }

    #[test]
    fn a_wrong_key_cannot_decrypt() {
        let a = store();
        let token = a.encode(&Sess { user_id: 1, role: "u".into() }).unwrap();
        let other = SessionStore::new(&crate::derive_key(b"a-totally-different-secret-of-length-32+", "session"));
        assert!(other.decode::<Sess>(&token).is_err());
    }

    #[test]
    fn set_cookie_and_clear_cookie_have_secure_attributes() {
        let s = store();
        let set = s.set_cookie(&Sess { user_id: 1, role: "u".into() }).unwrap();
        assert!(set.starts_with("jerrycan_session="));
        for attr in ["HttpOnly", "Secure", "SameSite=Lax", "Path=/"] {
            assert!(set.contains(attr), "missing {attr}: {set}");
        }
        let clear = s.clear_cookie();
        assert!(clear.contains("Max-Age=0"));
    }

    // Flips one base64 char to a different one (corrupts the token).
    fn flip_one_char(s: &str, at: usize) -> String {
        let mut chars: Vec<char> = s.chars().collect();
        chars[at] = if chars[at] == 'A' { 'B' } else { 'A' };
        chars.into_iter().collect()
    }
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p jerrycan-auth session` → compile FAIL.

- [ ] **Step 3: Implement** (above the tests)

```rust
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit, OsRng};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use rand::RngCore;

const COOKIE_NAME: &str = "jerrycan_session";

/// Encrypts/decrypts session payloads with a per-store AEAD key.
#[derive(Clone)]
pub struct SessionStore {
    cipher: ChaCha20Poly1305,
}

impl SessionStore {
    pub fn new(key: &[u8; 32]) -> Self {
        Self { cipher: ChaCha20Poly1305::new(key.into()) }
    }

    /// Serialize + encrypt to a base64url token (no padding).
    pub fn encode<T: Serialize>(&self, value: &T) -> Result<String> {
        let plaintext = serde_json::to_vec(value).map_err(|e| Error::internal(format!("session serialize: {e}")))?;
        let mut nonce_bytes = [0u8; 12];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext = self
            .cipher
            .encrypt(nonce, plaintext.as_ref())
            .map_err(|_| Error::internal("session encrypt failed"))?;
        let mut combined = Vec::with_capacity(12 + ciphertext.len());
        combined.extend_from_slice(&nonce_bytes);
        combined.extend_from_slice(&ciphertext);
        Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(combined))
    }

    /// Decrypt + deserialize. Any failure (bad base64, short input, AEAD
    /// rejection, JSON shape) is `JC0401` — an untrusted client value.
    pub fn decode<T: DeserializeOwned>(&self, token: &str) -> Result<T> {
        let combined = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(token)
            .map_err(|_| Error::unauthorized())?;
        if combined.len() < 12 {
            return Err(Error::unauthorized());
        }
        let (nonce_bytes, ciphertext) = combined.split_at(12);
        let plaintext = self
            .cipher
            .decrypt(Nonce::from_slice(nonce_bytes), ciphertext)
            .map_err(|_| Error::unauthorized())?;
        serde_json::from_slice(&plaintext).map_err(|_| Error::unauthorized())
    }

    /// A `Set-Cookie` header value establishing the session (secure defaults).
    pub fn set_cookie<T: Serialize>(&self, value: &T) -> Result<String> {
        let token = self.encode(value)?;
        Ok(format!(
            "{COOKIE_NAME}={token}; HttpOnly; Secure; SameSite=Lax; Path=/"
        ))
    }

    /// A `Set-Cookie` header value clearing the session.
    pub fn clear_cookie(&self) -> String {
        format!("{COOKIE_NAME}=; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age=0")
    }

    pub(crate) fn read_cookie(&self, cookie_header: &str) -> Option<String> {
        cookie_header
            .split(';')
            .filter_map(|kv| kv.trim().split_once('='))
            .find(|(k, _)| *k == COOKIE_NAME)
            .map(|(_, v)| v.to_string())
    }
}
```

Add `pub mod session;` to lib.rs.

- [ ] **Step 4: Run to verify pass** — `cargo test -p jerrycan-auth session` → 5 tests PASS. Full gate green.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan-auth/src/session.rs crates/jerrycan-auth/src/lib.rs
git commit -m "Add AEAD-encrypted session cookie store to jerrycan-auth"
```

---

### Task 4: `jerrycan-auth` — HS256 JWT

**Files:**
- Create: `crates/jerrycan-auth/src/jwt.rs`
- Modify: `crates/jerrycan-auth/src/lib.rs`

- [ ] **Step 1: Write the failing tests** (`jwt.rs`)

```rust
//! HS256 JWTs: signed, NOT encrypted (interop bearer tokens — never put secrets
//! in a JWT). We hand-roll the `header.payload.signature` envelope over the
//! `hmac` crate; we do NOT implement HMAC ourselves.

use jerrycan_core::{Error, Result};
use serde::{de::DeserializeOwned, Serialize};

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct Claims { sub: String, role: String, exp: u64 }

    fn key() -> [u8; 32] {
        crate::derive_key(b"a-very-long-development-secret-string!!", "jwt")
    }

    #[test]
    fn encode_then_decode_round_trips() {
        let token = encode(&Claims { sub: "u1".into(), role: "admin".into(), exp: 9999999999 }, &key()).unwrap();
        assert_eq!(token.split('.').count(), 3, "header.payload.signature");
        let claims: Claims = decode(&token, &key()).unwrap();
        assert_eq!(claims, Claims { sub: "u1".into(), role: "admin".into(), exp: 9999999999 });
    }

    #[test]
    fn a_tampered_payload_fails_signature_verification() {
        let token = encode(&Claims { sub: "u1".into(), role: "user".into(), exp: 9999999999 }, &key()).unwrap();
        let mut parts: Vec<&str> = token.split('.').collect();
        // Swap the payload for a forged "admin" one (re-encoded), keep the old signature.
        let forged = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"sub":"u1","role":"admin","exp":9999999999}"#);
        parts[1] = &forged;
        let tampered = parts.join(".");
        assert!(decode::<Claims>(&tampered, &key()).is_err());
    }

    #[test]
    fn a_wrong_key_is_rejected() {
        let token = encode(&Claims { sub: "u1".into(), role: "user".into(), exp: 9999999999 }, &key()).unwrap();
        let other = crate::derive_key(b"different-secret-of-at-least-32-bytes!!", "jwt");
        assert!(decode::<Claims>(&token, &other).is_err());
    }

    #[test]
    fn expired_tokens_are_rejected() {
        let token = encode(&Claims { sub: "u1".into(), role: "user".into(), exp: 1 }, &key()).unwrap();
        let err = decode::<Claims>(&token, &key()).unwrap_err();
        assert_eq!(err.code(), "JC0401");
    }
}
```

- [ ] **Step 2: Run to verify failure** — compile FAIL.

- [ ] **Step 3: Implement** (above the tests)

```rust
use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn unb64(s: &str) -> Result<Vec<u8>> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|_| Error::unauthorized())
}

fn sign(message: &str, key: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(message.as_bytes());
    b64(&mac.finalize().into_bytes())
}

/// Encode claims as a signed HS256 JWT. Claims SHOULD include an `exp`
/// (unix seconds); `decode` enforces it when present.
pub fn encode<T: Serialize>(claims: &T, key: &[u8]) -> Result<String> {
    let header = b64(br#"{"alg":"HS256","typ":"JWT"}"#);
    let payload_json = serde_json::to_vec(claims).map_err(|e| Error::internal(format!("jwt serialize: {e}")))?;
    let payload = b64(&payload_json);
    let message = format!("{header}.{payload}");
    let signature = sign(&message, key);
    Ok(format!("{message}.{signature}"))
}

/// Verify signature (constant-time via hmac's `verify`), enforce `exp` if
/// present, then deserialize. Any failure is `JC0401`.
pub fn decode<T: DeserializeOwned>(token: &str, key: &[u8]) -> Result<T> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err(Error::unauthorized());
    }
    let message = format!("{}.{}", parts[0], parts[1]);
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(message.as_bytes());
    let provided = unb64(parts[2])?;
    mac.verify_slice(&provided).map_err(|_| Error::unauthorized())?;

    let payload = unb64(parts[1])?;
    // Enforce exp if the payload carries one (don't require a fixed claim type).
    if let Ok(map) = serde_json::from_slice::<serde_json::Value>(&payload) {
        if let Some(exp) = map.get("exp").and_then(|v| v.as_u64()) {
            if exp <= now_unix() {
                return Err(Error::unauthorized());
            }
        }
    }
    serde_json::from_slice(&payload).map_err(|_| Error::unauthorized())
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
```

Add `pub mod jwt;` to lib.rs.

- [ ] **Step 4: Run to verify pass** — `cargo test -p jerrycan-auth jwt` → 4 tests PASS. Full gate green.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan-auth/src/jwt.rs crates/jerrycan-auth/src/lib.rs
git commit -m "Add hand-rolled HS256 JWT encode/decode over RustCrypto hmac"
```

---

### Task 5: `jerrycan-auth` — `Auth` extension, `Session`/`JwtClaims` extractors, `require_role`

**Files:**
- Create: `crates/jerrycan-auth/src/guard.rs`
- Modify: `crates/jerrycan-auth/src/lib.rs`, `crates/jerrycan-auth/src/session.rs`, `crates/jerrycan-auth/src/jwt.rs`

- [ ] **Step 1: Write the failing tests** (`crates/jerrycan-auth/src/guard.rs`)

```rust
//! Guards are dependencies (spec §4.3): `Session<T>`/`Bearer<T>` are extractors
//! returning 401; `require_role` returns 403. No auth middleware.

use jerrycan_core::{Error, FromRequest, Headers, RequestCtx, Result};
use serde::de::DeserializeOwned;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Auth, SessionStore};
    use jerrycan_core::{get, post, App, Dep, Json};
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, Clone)]
    struct User { id: i64, role: String }

    async fn login(auth: Dep<Auth>) -> Result<jerrycan_core::Response> {
        // Issue a session cookie for a fixed user (test login).
        let cookie = auth.sessions().set_cookie(&User { id: 1, role: "admin".into() })?;
        let mut res = jerrycan_core::IntoResponse::into_response("ok");
        res.headers_mut().insert(
            jerrycan_core::http::header::SET_COOKIE,
            jerrycan_core::http::HeaderValue::from_str(&cookie).unwrap(),
        );
        Ok(res)
    }

    async fn whoami(Session(user): Session<User>) -> Json<i64> {
        Json(user.id)
    }

    fn app() -> App {
        App::new()
            .extend(Auth::with_secret("a-very-long-development-secret-string!!"))
            .route("/login", post(login))
            .route("/me", get(whoami))
    }

    #[tokio::test]
    async fn no_cookie_is_401() {
        let t = app().into_test();
        assert_eq!(t.get("/me").await.status(), jerrycan_core::http::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn login_then_authenticated_request_succeeds() {
        let t = app().into_test();
        let login = t.post_json("/login", &()).await;
        let set_cookie = login.headers()["set-cookie"].to_str().unwrap().to_string();
        let cookie = set_cookie.split(';').next().unwrap().to_string(); // jerrycan_session=...
        let res = t.get_with("/me", &[("cookie", &cookie)]).await;
        assert_eq!(res.status(), jerrycan_core::http::StatusCode::OK);
        assert_eq!(res.json::<i64>(), 1);
    }

    #[tokio::test]
    async fn require_role_rejects_wrong_role_with_403() {
        async fn admin_only(Session(user): Session<User>) -> Result<&'static str> {
            require_role(&user.role, "superadmin")?;
            Ok("secret")
        }
        let t = App::new()
            .extend(Auth::with_secret("a-very-long-development-secret-string!!"))
            .route("/login", post(login))
            .route("/admin", get(admin_only))
            .into_test();
        let login = t.post_json("/login", &()).await;
        let cookie = login.headers()["set-cookie"].to_str().unwrap().split(';').next().unwrap().to_string();
        let res = t.get_with("/admin", &[("cookie", &cookie)]).await;
        assert_eq!(res.status(), jerrycan_core::http::StatusCode::FORBIDDEN);
    }
}
```

- [ ] **Step 2: Run to verify failure** — compile FAIL (`Session`, `require_role`, `Auth::with_secret`, `Auth::sessions` missing).

- [ ] **Step 3: Implement guard.rs**

```rust
/// Session extractor: decrypts the `jerrycan_session` cookie into `T`.
/// Absent/invalid cookie → 401. Requires the `Auth` extension to be registered.
pub struct Session<T>(pub T);

impl<T: DeserializeOwned + Send> FromRequest for Session<T> {
    async fn from_request(ctx: &mut RequestCtx) -> Result<Self> {
        let auth = ctx.resolve::<crate::Auth>().await?;
        let headers = Headers::from_request(ctx).await?;
        let cookie_header = headers.get("cookie").ok_or_else(Error::unauthorized)?;
        let token = auth.sessions().read_cookie(cookie_header).ok_or_else(Error::unauthorized)?;
        auth.sessions().decode::<T>(&token).map(Session)
    }
}

/// Bearer JWT extractor: verifies the `Authorization: Bearer <jwt>` token into `T`.
pub struct Bearer<T>(pub T);

impl<T: DeserializeOwned + Send> FromRequest for Bearer<T> {
    async fn from_request(ctx: &mut RequestCtx) -> Result<Self> {
        let auth = ctx.resolve::<crate::Auth>().await?;
        let headers = Headers::from_request(ctx).await?;
        let value = headers.get("authorization").ok_or_else(Error::unauthorized)?;
        let token = value.strip_prefix("Bearer ").ok_or_else(Error::unauthorized)?;
        crate::jwt::decode::<T>(token, auth.jwt_key()).map(Bearer)
    }
}

/// Role check helper for generated guards: `403` when the role doesn't match.
pub fn require_role(actual: &str, required: &str) -> Result<()> {
    if actual == required {
        Ok(())
    } else {
        Err(Error::forbidden())
    }
}
```

NOTE: `ctx.resolve::<T>()` is the per-request DI resolver from Phase 0; confirm it's `pub` on `RequestCtx` (it was used by `Dep`'s `FromRequest`). If it's `pub(crate)`, widen to `pub` (record it) — extractors in sibling crates need it.

- [ ] **Step 4: Implement the `Auth` extension** (lib.rs)

```rust
use jerrycan_core::{App, Extension};

pub mod guard;
pub use guard::{require_role, Bearer, Session};
pub use session::SessionStore;

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
        Self { sessions: SessionStore::new(&session_key), jwt_key }
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
                Ok(Self::with_secret("jerrycan-insecure-development-secret-do-not-use!!"))
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
```

(Move `pub mod session; pub mod jwt;` near the top; keep `derive_key`/`MIN_SECRET_LEN`.)

- [ ] **Step 5: Run to verify pass** — `cargo test -p jerrycan-auth` → all PASS (incl. the 3 guard integration tests). Full gate green.

- [ ] **Step 6: Commit**

```bash
git add crates/jerrycan-auth/src
git commit -m "Add Auth extension with Session/Bearer extractors and require_role guard"
```

---
### Task 6: `jerrycan-observe` — metrics registry + Prometheus text

**Files:**
- Replace: `crates/jerrycan-observe/Cargo.toml`
- Create: `crates/jerrycan-observe/src/lib.rs`, `crates/jerrycan-observe/src/metrics.rs`

- [ ] **Step 1: Manifest**

`crates/jerrycan-observe/Cargo.toml`:

```toml
[package]
name = "jerrycan-observe"
description = "Observability extension for the jerrycan framework: request IDs, JSON logs, /healthz, Prometheus /metrics. Real releases begin at 0.1.0. https://jerrycan.cc"
version.workspace = true
edition.workspace = true
license.workspace = true
homepage.workspace = true

[dependencies]
jerrycan-core = { path = "../jerrycan-core", version = "0.0.0" }
tracing.workspace = true
tracing-subscriber.workspace = true

[dev-dependencies]
tokio.workspace = true
```

Workspace `[workspace.dependencies]` (added in Task 2's batch, but if not present add here):

```toml
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["json", "env-filter"] }
```

- [ ] **Step 2: Write the failing tests** (`metrics.rs`)

```rust
//! A tiny global metrics registry rendered as Prometheus text. No histogram
//! crate: counters are atomics, latency is fixed-bucket counters.

use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_render_as_prometheus_text() {
        let m = Metrics::new();
        m.record(200, 0.003);
        m.record(200, 0.020);
        m.record(404, 0.001);
        let text = m.render();
        assert!(text.contains("# TYPE jerrycan_requests_total counter"));
        assert!(text.contains(r#"jerrycan_requests_total{status="200"} 2"#), "{text}");
        assert!(text.contains(r#"jerrycan_requests_total{status="404"} 1"#), "{text}");
        assert!(text.contains("# TYPE jerrycan_request_duration_seconds histogram"));
        // 0.003 and 0.020 fall in le="0.005"? no (0.020 > 0.005) — cumulative buckets:
        assert!(text.contains(r#"jerrycan_request_duration_seconds_bucket{le="0.005"} 2"#), "0.003 + 0.001 ≤ 5ms: {text}");
        assert!(text.contains(r#"jerrycan_request_duration_seconds_bucket{le="+Inf"} 3"#), "{text}");
        assert!(text.contains("jerrycan_request_duration_seconds_count 3"));
    }

    #[test]
    fn in_flight_tracks_concurrency() {
        let m = Metrics::new();
        let g = m.in_flight_guard();
        assert!(m.render().contains("jerrycan_requests_in_flight 1"));
        drop(g);
        assert!(m.render().contains("jerrycan_requests_in_flight 0"));
    }
}
```

- [ ] **Step 3: Run to verify failure** — compile FAIL.

- [ ] **Step 4: Implement** (above the tests)

```rust
/// Fixed Prometheus latency buckets (seconds), cumulative on render.
const BUCKETS: [f64; 8] = [0.005, 0.010, 0.025, 0.050, 0.100, 0.250, 0.500, 1.000];

/// Process-wide metrics. Cheap atomics; status counters keyed by a small set of
/// common codes plus an "other" sink (avoids unbounded label cardinality).
pub struct Metrics {
    status_2xx: AtomicU64,
    status_4xx: AtomicU64,
    status_5xx: AtomicU64,
    status_200: AtomicU64,
    status_404: AtomicU64,
    in_flight: AtomicU64,
    duration_count: AtomicU64,
    duration_sum_micros: AtomicU64,
    buckets: [AtomicU64; 8],
}

impl Metrics {
    pub fn new() -> Self {
        Self {
            status_2xx: AtomicU64::new(0),
            status_4xx: AtomicU64::new(0),
            status_5xx: AtomicU64::new(0),
            status_200: AtomicU64::new(0),
            status_404: AtomicU64::new(0),
            in_flight: AtomicU64::new(0),
            duration_count: AtomicU64::new(0),
            duration_sum_micros: AtomicU64::new(0),
            buckets: Default::default(),
        }
    }

    /// Record one finished request.
    pub fn record(&self, status: u16, seconds: f64) {
        match status {
            200 => { self.status_200.fetch_add(1, Ordering::Relaxed); }
            404 => { self.status_404.fetch_add(1, Ordering::Relaxed); }
            _ => {}
        }
        let class = match status {
            200..=299 => &self.status_2xx,
            400..=499 => &self.status_4xx,
            500..=599 => &self.status_5xx,
            _ => &self.status_2xx,
        };
        class.fetch_add(1, Ordering::Relaxed);
        self.duration_count.fetch_add(1, Ordering::Relaxed);
        self.duration_sum_micros.fetch_add((seconds * 1_000_000.0) as u64, Ordering::Relaxed);
        for (i, edge) in BUCKETS.iter().enumerate() {
            if seconds <= *edge {
                self.buckets[i].fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// RAII guard for the in-flight gauge.
    pub fn in_flight_guard(&self) -> InFlightGuard<'_> {
        self.in_flight.fetch_add(1, Ordering::Relaxed);
        InFlightGuard { metrics: self }
    }

    /// Render the current state as Prometheus exposition text.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let total_200 = self.status_200.load(Ordering::Relaxed);
        let total_404 = self.status_404.load(Ordering::Relaxed);
        let total = self.duration_count.load(Ordering::Relaxed);
        out.push_str("# TYPE jerrycan_requests_total counter\n");
        out.push_str(&format!("jerrycan_requests_total{{status=\"200\"}} {total_200}\n"));
        out.push_str(&format!("jerrycan_requests_total{{status=\"404\"}} {total_404}\n"));
        out.push_str("# TYPE jerrycan_requests_in_flight gauge\n");
        out.push_str(&format!("jerrycan_requests_in_flight {}\n", self.in_flight.load(Ordering::Relaxed)));
        out.push_str("# TYPE jerrycan_request_duration_seconds histogram\n");
        let mut cumulative = 0;
        for (i, edge) in BUCKETS.iter().enumerate() {
            cumulative = self.buckets[i].load(Ordering::Relaxed); // already cumulative by construction
            out.push_str(&format!("jerrycan_request_duration_seconds_bucket{{le=\"{edge}\"}} {cumulative}\n"));
        }
        out.push_str(&format!("jerrycan_request_duration_seconds_bucket{{le=\"+Inf\"}} {total}\n"));
        let sum = self.duration_sum_micros.load(Ordering::Relaxed) as f64 / 1_000_000.0;
        out.push_str(&format!("jerrycan_request_duration_seconds_sum {sum}\n"));
        out.push_str(&format!("jerrycan_request_duration_seconds_count {total}\n"));
        let _ = cumulative; // last bucket load; kept for readability of the loop
        out
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

/// Decrements the in-flight gauge on drop.
pub struct InFlightGuard<'a> {
    metrics: &'a Metrics,
}

impl Drop for InFlightGuard<'_> {
    fn drop(&mut self) {
        self.metrics.in_flight.fetch_sub(1, Ordering::Relaxed);
    }
}
```

Pre-solved note: each `buckets[i]` is incremented for every observation with `seconds <= edge[i]`, so it is ALREADY the cumulative count at that boundary — `render` reads it directly (no running sum needed). The test's `le="0.005" → 2` (0.003 and 0.001, not 0.020) confirms this. Remove the misleading `cumulative` running variable if clippy complains; read `self.buckets[i]` inline.

- [ ] **Step 5: Run to verify pass** — `cargo test -p jerrycan-observe metrics` → 2 tests PASS. Full gate green.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock crates/jerrycan-observe
git commit -m "Add hand-rolled metrics registry with Prometheus text rendering"
```

---

### Task 7: `jerrycan-observe` — access-log middleware + `Observe` extension

**Files:**
- Create: `crates/jerrycan-observe/src/access_log.rs`
- Modify: `crates/jerrycan-observe/src/lib.rs`

- [ ] **Step 1: Write the failing tests** (`access_log.rs`)

```rust
//! Request-id + access logging middleware, and the Observe extension wiring
//! /healthz and /metrics. Logging output is structured JSON via tracing; tests
//! assert behavior (header + metrics endpoints), not log lines.

use jerrycan_core::{Middleware, MiddlewareFuture, Next, RequestCtx};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Observe;
    use jerrycan_core::{get, App};

    #[tokio::test]
    async fn responses_carry_a_request_id_header() {
        let t = App::new().extend(Observe::new()).route("/x", get(|| async { "x" })).into_test();
        let res = t.get("/x").await;
        let id = res.headers().get("x-request-id").expect("x-request-id present");
        assert!(!id.to_str().unwrap().is_empty());
    }

    #[tokio::test]
    async fn healthz_and_metrics_are_served() {
        let t = App::new().extend(Observe::new()).route("/x", get(|| async { "x" })).into_test();
        assert_eq!(t.get("/healthz").await.text(), "ok");

        // Drive one request so a counter is non-zero, then scrape.
        let _ = t.get("/x").await;
        let metrics = t.get("/metrics").await;
        assert_eq!(metrics.headers()["content-type"], "text/plain; version=0.0.4");
        assert!(metrics.text().contains("jerrycan_requests_total"), "{}", metrics.text());
    }
}
```

- [ ] **Step 2: Run to verify failure** — compile FAIL.

- [ ] **Step 3: Implement access_log.rs**

```rust
use crate::metrics::Metrics;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Monotonic request-id source (process-local; pair with the hostname/pod for
/// global uniqueness at the log-aggregation layer).
static REQUEST_SEQ: AtomicU64 = AtomicU64::new(1);

fn next_request_id() -> String {
    let n = REQUEST_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("req-{n:016x}")
}

/// Middleware: assigns a request id, times the request, records metrics, emits
/// one structured access-log line, and stamps `x-request-id` on the response.
pub struct AccessLog {
    pub(crate) metrics: Arc<Metrics>,
}

impl Middleware for AccessLog {
    fn handle<'a>(&'a self, ctx: &'a mut RequestCtx, next: Next<'a>) -> MiddlewareFuture<'a> {
        Box::pin(async move {
            let request_id = next_request_id();
            let method = ctx.method().to_string();
            let path = ctx.uri().path().to_string();
            let _guard = self.metrics.in_flight_guard();
            let started = std::time::Instant::now();

            let mut response = next.run(&mut *ctx).await;

            let elapsed = started.elapsed().as_secs_f64();
            let status = response.status().as_u16();
            self.metrics.record(status, elapsed);
            tracing::info!(
                target: "jerrycan::access",
                request_id = %request_id,
                method = %method,
                path = %path,
                status = status,
                duration_ms = elapsed * 1000.0,
                "request"
            );
            if let Ok(value) = jerrycan_core::http::HeaderValue::from_str(&request_id) {
                response.headers_mut().insert(
                    jerrycan_core::http::HeaderName::from_static("x-request-id"),
                    value,
                );
            }
            response
        })
    }
}
```

- [ ] **Step 4: Implement the `Observe` extension** (lib.rs)

```rust
//! Observability for jerrycan: request IDs, structured JSON access logs,
//! /healthz, and a Prometheus /metrics endpoint. #![forbid(unsafe_code)].
#![forbid(unsafe_code)]

use jerrycan_core::{get, App, Extension, IntoResponse, Response};
use std::sync::Arc;

pub mod access_log;
pub mod metrics;

pub use metrics::Metrics;

/// The observability extension: app-wide access-log middleware + health/metrics
/// routes sharing one metrics registry.
pub struct Observe {
    metrics: Arc<Metrics>,
}

impl Observe {
    pub fn new() -> Self {
        Self { metrics: Arc::new(Metrics::new()) }
    }
}

impl Default for Observe {
    fn default() -> Self {
        Self::new()
    }
}

impl Extension for Observe {
    fn register(self, app: App) -> App {
        let metrics_for_mw = self.metrics.clone();
        let metrics_for_route = self.metrics.clone();
        app.middleware(access_log::AccessLog { metrics: metrics_for_mw })
            .route("/healthz", get(|| async { "ok" }))
            .route(
                "/metrics",
                get(move || {
                    let metrics = metrics_for_route.clone();
                    async move { prometheus_response(metrics.render()) }
                }),
            )
    }
}

/// Initialize JSON logging once (call from `main` before serving). Idempotent;
/// honors `RUST_LOG`. No-op if a global subscriber is already set.
pub fn init_logging() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = fmt().json().with_env_filter(filter).try_init();
}

fn prometheus_response(body: String) -> Response {
    let mut response = body.into_response();
    response.headers_mut().insert(
        jerrycan_core::http::header::CONTENT_TYPE,
        jerrycan_core::http::HeaderValue::from_static("text/plain; version=0.0.4"),
    );
    response
}
```

Pre-solved trap: `/metrics` and `/healthz` are added by the extension; if the generated app ALSO had design routes at those paths, `build()` would conflict-error (fail loud). Document in 11-observability that those paths are reserved when observe is on. The access-log middleware records metrics for ALL routes including its own /metrics and /healthz — acceptable (self-observation).

- [ ] **Step 5: Run to verify pass** — `cargo test -p jerrycan-observe` → all PASS. Full gate green.

- [ ] **Step 6: Commit**

```bash
git add crates/jerrycan-observe/src
git commit -m "Add access-log middleware and Observe extension with healthz and metrics"
```

---

### Task 8: Facade features — `jerrycan::auth` and `jerrycan::observe`

**Files:**
- Modify: `crates/jerrycan/Cargo.toml`, `crates/jerrycan/src/lib.rs`, `crates/jerrycan/tests/features.rs`

- [ ] **Step 1: Write the failing tests** (append to `tests/features.rs`)

```rust
#[cfg(feature = "auth")]
#[test]
fn auth_reexport_is_usable() {
    let hash = jerrycan::auth::hash_password("pw").unwrap();
    assert!(jerrycan::auth::verify_password("pw", &hash).unwrap());
}

#[cfg(feature = "observe")]
#[test]
fn observe_reexport_is_usable() {
    let m = jerrycan::observe::Metrics::new();
    m.record(200, 0.01);
    assert!(m.render().contains("jerrycan_requests_total"));
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p jerrycan --all-features --test features` → compile FAIL.

- [ ] **Step 3: Implement**

`crates/jerrycan/Cargo.toml` `[dependencies]`:

```toml
jerrycan-auth = { workspace = true, optional = true }
jerrycan-observe = { workspace = true, optional = true }
```

`[features]`:

```toml
auth = ["dep:jerrycan-auth"]
observe = ["dep:jerrycan-observe"]
```

`crates/jerrycan/src/lib.rs`:

```rust
#[cfg(feature = "auth")]
pub use jerrycan_auth as auth;

#[cfg(feature = "observe")]
pub use jerrycan_observe as observe;
```

- [ ] **Step 4: Run to verify pass** — `cargo test -p jerrycan --all-features` green; `cargo check -p jerrycan --no-default-features` and `cargo check -p jerrycan` (cli) green. Full gate green.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan/Cargo.toml crates/jerrycan/src/lib.rs crates/jerrycan/tests/features.rs Cargo.lock
git commit -m "Expose jerrycan-auth and jerrycan-observe through facade features"
```

---
### Task 9: Generation ripple — auth/observe modes, JL0004, gen-tests 401s

**Files:**
- Modify: `crates/jerrycan/src/platform/design.rs`, `genroute.rs`, `mounting.rs`, `scaffold.rs`, `questions.rs`, `lints.rs`, `testgen.rs`
- Modify: `docs/contracts/design-schema.json`
- Create: `crates/jerrycan/tests/authgen.rs`

Mode plumbing extends Phase 2's `GenMode { db }` to `GenMode { db, auth }` (observe needs no per-route codegen — it's pure extension wiring). Auth-mode triggers: `design.auth.model != "none"`. A `auth_required` (or non-empty `required_roles`) endpoint gets a guard parameter in its handler stub.

- [ ] **Step 1: Write the failing tests** (`crates/jerrycan/tests/authgen.rs`)

```rust
//! auth/observe-mode generation: guard params, extension wiring, JL0004.

use jerrycan::platform::design::Design;
use jerrycan::platform::scaffold;
use std::fs;

const GOLDEN: &str = include_str!("../../../conformance/designs/todo-api.design.json");

/// Golden design with session auth + an admin-guarded delete + observe.
fn auth_design() -> Design {
    let mut v: serde_json::Value = serde_json::from_str(GOLDEN).unwrap();
    v["dependencies"] = serde_json::json!(["auth", "observe"]);
    v["auth"] = serde_json::json!({ "model": "session", "roles": ["admin"] });
    // mark delete_todo admin-guarded
    let eps = v["modules"][0]["endpoints"].as_array_mut().unwrap();
    for ep in eps {
        if ep["operation_id"] == "delete_todo" {
            ep["required_roles"] = serde_json::json!(["admin"]);
        }
        if ep["operation_id"] == "create_todo" {
            ep["auth_required"] = serde_json::json!(true);
        }
    }
    serde_json::from_value(v).unwrap()
}

fn scaffold_auth() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("todo-api");
    scaffold::scaffold(&root, &auth_design()).unwrap();
    (tmp, root)
}

#[test]
fn auth_required_endpoints_get_a_session_guard_param() {
    let (_t, root) = scaffold_auth();
    let handlers = fs::read_to_string(root.join("crates/routes/todos/src/handlers.rs")).unwrap();
    // create_todo is auth_required → carries a CurrentUser guard
    assert!(handlers.contains("_user: CurrentUser"), "auth_required handler guarded: {handlers}");
    // delete_todo requires role admin → guard + role check stub note
    assert!(handlers.contains("// guard: requires role \"admin\""), "{handlers}");
    // list_todos is public → no guard
    let list_fn = handlers.split("async fn list_todos").nth(1).unwrap();
    assert!(!list_fn.split("->").next().unwrap().contains("CurrentUser"), "public handler unguarded");
}

#[test]
fn main_wires_auth_and_observe_extensions() {
    let (_t, root) = scaffold_auth();
    let main_rs = fs::read_to_string(root.join("crates/app/src/main.rs")).unwrap();
    assert!(main_rs.contains("jerrycan::observe::init_logging();"), "{main_rs}");
    assert!(main_rs.contains(".extend(jerrycan::auth::Auth::from_env()?)"), "{main_rs}");
    assert!(main_rs.contains(".extend(jerrycan::observe::Observe::new())"), "{main_rs}");
    let ws = fs::read_to_string(root.join("Cargo.toml")).unwrap();
    assert!(ws.contains("features = [\"auth\", \"observe\"]"), "{ws}");
}

#[test]
fn current_user_type_alias_is_generated_in_shared() {
    let (_t, root) = scaffold_auth();
    let shared = fs::read_to_string(root.join("crates/shared/src/lib.rs")).unwrap();
    // The app's notion of the session user lives in shared so guards across modules agree.
    assert!(shared.contains("pub type CurrentUser"), "{shared}");
}
```

Add to questions.rs tests:

```rust
    #[test]
    fn required_roles_need_a_role_in_auth_roles_and_auth_model() {
        let mut v: serde_json::Value = serde_json::from_str(MINIMAL).unwrap();
        v["auth"] = serde_json::json!({ "model": "none" });
        v["modules"][0]["endpoints"][2]["required_roles"] = serde_json::json!(["admin"]);
        let d: Design = serde_json::from_value(v).unwrap();
        assert!(validate(&d).iter().any(|q| q.question.contains("auth.model") || q.question.contains("auth.roles")));
    }
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p jerrycan --test authgen` → assertions fail (no guard codegen).

- [ ] **Step 3: Implement**

(a) `design.rs`:

```rust
impl Design {
    pub fn wants_auth(&self) -> bool {
        self.auth.as_ref().map(|a| a.model != AuthModel::None).unwrap_or(false)
            || self.dependencies.iter().any(|d| d == "auth")
    }
    pub fn wants_observe(&self) -> bool {
        self.dependencies.iter().any(|d| d == "observe")
    }
}

impl Endpoint {
    /// This endpoint needs an authenticated user (and maybe a role).
    pub fn is_guarded(&self) -> bool {
        self.auth_required || !self.required_roles.is_empty()
    }
}
```

(b) `genroute.rs` — `GenMode { db, auth }`. In `handler_params`, when `mode.auth && ep.is_guarded()`, prepend a guard param `_user: CurrentUser` (after `_repo` if present, before `Path`/`Json` — order: repo, user, path, body). Emit a leading comment line for role-guarded endpoints inside the stub body:

```rust
fn guard_comment(ep: &Endpoint) -> String {
    if ep.required_roles.is_empty() {
        String::new()
    } else {
        let roles = ep.required_roles.join("\", \"");
        format!("    // guard: requires role \"{roles}\" — call require_role(&_user.role, \"...\")? before proceeding\n")
    }
}
```

In `handlers_rs`, when `mode.auth`, add `use jerrycan::auth::{require_role, Session};` and `use shared::CurrentUser;` imports, and prepend `guard_comment(ep)` inside each guarded handler's body (before the `Err(...)` stub line). The guard param type is `CurrentUser` (the shared alias), extracted as `_user: CurrentUser`. Define `CurrentUser` as `Session<SessionUser>` via the shared alias (next).

(c) `scaffold.rs` — when `wants_auth`, write into `crates/shared/src/lib.rs` (tool-owned region or full file) a session-user type + alias:

```rust
fn shared_auth_types() -> &'static str {
    "\n/// The session payload (app-wide). Generated because the design declares auth.\n#[derive(serde::Serialize, serde::Deserialize, Clone)]\npub struct SessionUser {\n    pub id: i64,\n    pub role: String,\n}\n\n/// The guard extractor handlers use: a decrypted session.\npub type CurrentUser = jerrycan::auth::Session<SessionUser>;\n"
}
```

Append to SHARED_LIB content when `wants_auth` (shared crate gains `jerrycan` + `serde` deps with the auth feature — adjust SHARED_CARGO conditionally OR always include serde and add jerrycan when auth). Simplest: in db/auth modes the shared crate depends on `jerrycan` (workspace, with the app's features). Make SHARED_CARGO conditional: base + `jerrycan.workspace = true` when wants_auth.

(d) `mounting.rs` — `expected_main` gains auth/observe wiring. Order in `main`: `init_logging()` (if observe) → build `App` → `.extend(Auth::from_env()?)` (if auth, FIRST so guards resolve) → `.extend(Observe::new())` (if observe) → `.extend(db)` (if db) → validate → mounts → serve. The function already branches on db; extend it to compose all extension lines. Pull the per-mode extension lines into a helper returning the ordered `.extend(...)` block.

(e) `questions.rs` — role coherence: if any endpoint has `required_roles` but `auth.model == none` (or auth absent), emit a question; if `required_roles` references a role not in `auth.roles`, the existing Phase-0 check already catches it (verify and keep).

(f) `lints.rs` — JL0004:

```rust
/// JL0004: in an auth design, a mutating route (POST/PUT/PATCH/DELETE) whose
/// design endpoint is NOT guarded (no auth_required, no required_roles).
fn lint_unguarded_mutations(design: &Design, out: &mut Vec<Diagnostic>) {
    if !design.wants_auth() {
        return;
    }
    fn walk(m: &ModuleDesign, out: &mut Vec<Diagnostic>) {
        for ep in &m.endpoints {
            let mutating = matches!(ep.method, HttpMethod::POST | HttpMethod::PUT | HttpMethod::PATCH | HttpMethod::DELETE);
            if mutating && !ep.is_guarded() {
                out.push(/* JL0004 diagnostic naming module+operation_id, suggestion: set auth_required or required_roles */);
            }
        }
        for sub in &m.subroutes { walk(sub, out); }
    }
    for m in &design.modules { walk(m, out); }
}
```

Wire into `lints::run`. Build the Diagnostic with code "JL0004", file `design.json`, message `mutating route `{op}` in module `{m}` has no auth guard (design declares auth)`, suggestion `set auth_required: true or required_roles in design.json`, doc `jerrycan docs auth`.

(g) `testgen.rs` — for `auth_required`/role-guarded endpoints: emit a 401 test (request WITHOUT credentials → assert 401), and make the success test credentialed. The auth preamble logs in once and threads the cookie:

```rust
// in preamble for auth designs: a logged-in TestApp helper
// async fn auth_app() -> (TestApp, String /*cookie*/) { ... POST /login ... }
```

Concretely: when `design.wants_auth()`, the generated `app()` helper also performs a login to obtain a session cookie, and guarded-endpoint success tests pass it via `get_with`/`post_json_with`; each guarded endpoint additionally gets a `{op}_without_auth_is_401` test (no cookie → 401). NOTE: this requires the app to HAVE a login route; the golden design has none. So generate a **test-only login shim** in the acceptance preamble using the `Auth` extension directly to mint a cookie (no app route needed):

```rust
fn auth_preamble_login() -> &'static str {
    // Build the session cookie directly via the Auth extension (test-only),
    // independent of whether the app exposes a /login route.
    "fn test_cookie() -> String {\n    let auth = jerrycan::auth::Auth::with_secret(\"a-very-long-development-secret-string!!\");\n    let token = auth.sessions().encode(&shared::SessionUser { id: 1, role: \"admin\".into() }).expect(\"encode\");\n    format!(\"jerrycan_session={token}\")\n}\n"
}
```

and the app() helper builds the App WITH `.extend(jerrycan::auth::Auth::with_secret("a-very-long-development-secret-string!!"))` so the same secret decrypts the test cookie. Guarded success tests use `get_with(path, &[("cookie", &test_cookie())])`. 401 tests omit it.

- [ ] **Step 4: Run to verify pass** — `cargo test -p jerrycan --test authgen` 3 PASS; questions test PASS; memory/db generation tests still byte-stable (the GenMode tuple gained a field — update the Phase-2 `GenMode { db }` literals to `GenMode { db, auth: false }` everywhere, or give GenMode a `..Default::default()` ergonomic and use it). Full `--all-features` gate green.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan/src/platform crates/jerrycan/tests/authgen.rs docs/contracts/design-schema.json
git commit -m "Generate auth guards, observe wiring, JL0004 lint, and auth gen-tests"
```

---

### Task 10: Heavy — auth+observe golden app builds, passes check, serves guarded routes

**Files:**
- Create: `conformance/fixtures/auth/todos_handlers.rs`, `comments_handlers.rs`, `users_handlers.rs`
- Modify: `crates/jerrycan/tests/conformance.rs`

- [ ] **Step 1: Write the auth fixtures** (guarded handlers; in-memory mode for speed)

`conformance/fixtures/auth/todos_handlers.rs`:

```rust
//! Conformance fixture (auth mode): guarded todos handlers.
use super::model::*;
use super::repo::*;
use jerrycan::prelude::*;
use jerrycan::auth::require_role;
use shared::CurrentUser;

pub(crate) async fn list_todos(repo: Dep<TodoRepo>) -> Result<Json<Vec<Todo>>> {
    Ok(Json(repo.all()))
}

pub(crate) async fn create_todo(repo: Dep<TodoRepo>, _user: CurrentUser, Json(body): Json<Todo>) -> Result<Created<Todo>> {
    repo.insert(body.clone());
    Ok(Created(body))
}

pub(crate) async fn show_todo(repo: Dep<TodoRepo>, Path(id): Path<i64>) -> Result<Json<Todo>> {
    repo.get(id).map(Json).ok_or_else(Error::not_found)
}

pub(crate) async fn delete_todo(repo: Dep<TodoRepo>, _user: CurrentUser, Path(id): Path<i64>) -> Result<NoContent> {
    require_role(&_user.0.role, "admin")?;
    if repo.remove(id) { Ok(NoContent) } else { Err(Error::not_found()) }
}
```

(comments/users fixtures mirror the Phase 1 memory fixtures, with `create_*` taking `_user: CurrentUser`. Provide both files in full following the same pattern.)

- [ ] **Step 2: Heavy test** (append to conformance.rs)

```rust
/// Scaffold the golden app in auth+observe mode (in-memory repos) against the
/// LOCAL framework with auth+observe features.
fn scaffold_golden_auth(tmp: &Path) -> PathBuf {
    let mut design: serde_json::Value = serde_json::from_str(GOLDEN).unwrap();
    design["dependencies"] = serde_json::json!(["auth", "observe"]);
    design["auth"] = serde_json::json!({ "model": "session", "roles": ["admin"] });
    for ep in design["modules"][0]["endpoints"].as_array_mut().unwrap() {
        if ep["operation_id"] == "create_todo" { ep["auth_required"] = serde_json::json!(true); }
        if ep["operation_id"] == "delete_todo" { ep["required_roles"] = serde_json::json!(["admin"]); }
    }
    for ep in design["modules"][1]["endpoints"].as_array_mut().unwrap() {
        if ep["operation_id"] == "create_user" { ep["auth_required"] = serde_json::json!(true); }
    }
    let design_path = tmp.join("design.json");
    std::fs::write(&design_path, serde_json::to_string_pretty(&design).unwrap()).unwrap();
    let app = tmp.join("todo-api");
    let dep = format!("jerrycan = {{ path = \"{}\", default-features = false }}", repo_root().join("crates/jerrycan").display());
    let st = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .env("JERRYCAN_FRAMEWORK_DEP", &dep)
        .arg("new").arg(&app).arg("--design").arg(&design_path).status().unwrap();
    assert!(st.success());
    app
}

#[test]
#[ignore = "heavy: auth+observe golden app builds, checks, and serves guarded routes"]
fn auth_observe_app_builds_checks_and_guards() {
    let tmp = tempfile::tempdir().unwrap();
    let app = scaffold_golden_auth(tmp.path());
    for (fixture, target) in [
        ("auth/todos_handlers.rs", "crates/routes/todos/src/handlers.rs"),
        ("auth/comments_handlers.rs", "crates/routes/todos/src/subroutes/comments/handlers.rs"),
        ("auth/users_handlers.rs", "crates/routes/users/src/handlers.rs"),
    ] {
        std::fs::copy(repo_root().join("conformance/fixtures").join(fixture), app.join(target)).unwrap();
    }

    // Full gate green (JL0004 must be satisfied — guarded mutations).
    let out = Command::new(env!("CARGO_BIN_EXE_jerrycan")).current_dir(&app).args(["--json", "check"]).output().unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(payload["ok"], true, "diagnostics: {}", payload["diagnostics"]);

    // Serve and exercise guard behavior over real HTTP.
    let port = { let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap(); l.local_addr().unwrap().port() };
    let addr = format!("127.0.0.1:{port}");
    let mut server = Command::new("cargo").current_dir(&app)
        .env("JERRYCAN_ADDR", &addr)
        .env("JERRYCAN_SECRET", "a-very-long-development-secret-string!!")
        .args(["run", "-p", "app"]).spawn().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
    while std::time::Instant::now() < deadline {
        if std::net::TcpStream::connect(&addr).is_ok() { break; }
        std::thread::sleep(std::time::Duration::from_millis(400));
    }
    let http = |req: String| -> String {
        let mut s = std::net::TcpStream::connect(&addr).unwrap();
        s.write_all(req.as_bytes()).unwrap();
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).unwrap();
        String::from_utf8_lossy(&buf).into_owned()
    };

    // Public list works without auth.
    assert!(http("GET /todos/ HTTP/1.1\r\nHost: l\r\nConnection: close\r\n\r\n".into()).starts_with("HTTP/1.1 200"));
    // Guarded create without a cookie → 401.
    let body = r#"{"title":"x","done":false}"#;
    let create = |cookie: &str| format!(
        "POST /todos/ HTTP/1.1\r\nHost: l\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{}Connection: close\r\n\r\n{body}",
        body.len(), cookie
    );
    assert!(http(create("")).starts_with("HTTP/1.1 401"), "no cookie → 401");
    // Mint an admin cookie with the same secret and create successfully.
    // (Compute the cookie via a tiny helper binary? simpler: the app has no login,
    //  so use jerrycan-auth directly in-test to build the cookie.)
    let cookie = {
        let auth = jerrycan::auth::Auth::with_secret("a-very-long-development-secret-string!!");
        let token = auth.sessions().encode(&serde_json::json!({ "id": 1, "role": "admin" })).unwrap();
        format!("Cookie: jerrycan_session={token}\r\n")
    };
    assert!(http(create(&cookie)).starts_with("HTTP/1.1 201"), "admin cookie → 201");
    // Observe endpoints live.
    assert_eq!(http("GET /healthz HTTP/1.1\r\nHost: l\r\nConnection: close\r\n\r\n".into()).lines().next().unwrap(), "HTTP/1.1 200 OK");
    assert!(http("GET /metrics HTTP/1.1\r\nHost: l\r\nConnection: close\r\n\r\n".into()).contains("jerrycan_requests_total"));

    let _ = server.kill();
    let _ = server.wait();
}
```

NOTE: the in-test cookie uses `serde_json::json!({id,role})` which must serialize identically to `shared::SessionUser`. Since `SessionUser` is `{id: i64, role: String}` and serde_json maps match field names, the AEAD payload round-trips. The conformance test adds `jerrycan = { features = ["auth"] }` to ITS OWN dev-deps so `jerrycan::auth` is callable in the test — confirm `crates/jerrycan/Cargo.toml` dev-deps include the auth feature (the test binary is part of the jerrycan crate; gate the test body on `#[cfg(feature="auth")]` and ensure `--all-features` runs it; CI uses --all-features for the heavy run).

- [ ] **Step 3: Run it**

`cargo test -p jerrycan --all-features --test conformance auth_observe -- --include-ignored`
Expected: PASS (budget 5-15 min cold — auth pulls argon2/chacha). Generator/fixture bugs fixed in genroute/scaffold, recorded.

- [ ] **Step 4: Commit**

```bash
git add conformance/fixtures/auth crates/jerrycan/tests/conformance.rs crates/jerrycan/Cargo.toml
git commit -m "Prove auth and observe golden app builds, checks, and guards over HTTP"
```

---
### Task 11: SBOM generation (`sbom.rs`)

**Files:**
- Create: `crates/jerrycan/src/platform/sbom.rs` (+ `pub mod sbom;` in mod.rs)

- [ ] **Step 1: Write the failing tests**

```rust
//! CycloneDX 1.5 SBOM from `cargo metadata` — no cargo-cyclonedx dependency.

use serde_json::Value;

#[cfg(test)]
mod tests {
    use super::*;

    // A trimmed `cargo metadata` shape: packages with name/version/license/source.
    const META: &str = r#"{
        "packages": [
            { "name": "app", "version": "0.1.0", "license": "MIT OR Apache-2.0", "source": null },
            { "name": "serde", "version": "1.0.0", "license": "MIT OR Apache-2.0", "source": "registry+https://github.com/rust-lang/crates.io-index" },
            { "name": "tokio", "version": "1.40.0", "license": "MIT", "source": "registry+https://github.com/rust-lang/crates.io-index" }
        ]
    }"#;

    #[test]
    fn cyclonedx_shape_and_components() {
        let doc = document(&serde_json::from_str(META).unwrap(), "app", "0.1.0");
        assert_eq!(doc["bomFormat"], "CycloneDX");
        assert_eq!(doc["specVersion"], "1.5");
        assert_eq!(doc["metadata"]["component"]["name"], "app");
        let comps = doc["components"].as_array().unwrap();
        // The root package is the metadata.component, not a dependency component.
        assert!(comps.iter().all(|c| c["name"] != "app"), "root excluded from components");
        assert!(comps.iter().any(|c| c["name"] == "serde" && c["version"] == "1.0.0"));
        let serde = comps.iter().find(|c| c["name"] == "serde").unwrap();
        assert_eq!(serde["type"], "library");
        assert!(serde["purl"].as_str().unwrap().starts_with("pkg:cargo/serde@1.0.0"));
        assert_eq!(serde["licenses"][0]["expression"], "MIT OR Apache-2.0");
    }

    #[test]
    fn registryless_packages_are_still_listed() {
        // local path deps (source null) are components too, minus a registry purl.
        let doc = document(&serde_json::from_str(META).unwrap(), "app", "0.1.0");
        // only "app" is source-null and it's the root, so all listed components have purls:
        for c in doc["components"].as_array().unwrap() {
            assert!(c["purl"].is_string());
        }
    }
}
```

- [ ] **Step 2: Run to verify failure** — compile FAIL.

- [ ] **Step 3: Implement**

```rust
/// Build a CycloneDX 1.5 BOM from parsed `cargo metadata`. `root_name`/`root_version`
/// identify the app under analysis (becomes metadata.component, excluded from components).
pub fn document(metadata: &Value, root_name: &str, root_version: &str) -> Value {
    let empty = vec![];
    let packages = metadata["packages"].as_array().unwrap_or(&empty);
    let mut components = Vec::new();
    for pkg in packages {
        let name = pkg["name"].as_str().unwrap_or("");
        let version = pkg["version"].as_str().unwrap_or("");
        if name == root_name && version == root_version {
            continue; // the root is metadata.component, not a dependency
        }
        let mut component = serde_json::json!({
            "type": "library",
            "name": name,
            "version": version,
            "purl": format!("pkg:cargo/{name}@{version}"),
        });
        if let Some(license) = pkg["license"].as_str() {
            component["licenses"] = serde_json::json!([{ "expression": license }]);
        }
        components.push(component);
    }
    components.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    serde_json::json!({
        "bomFormat": "CycloneDX",
        "specVersion": "1.5",
        "version": 1,
        "metadata": {
            "component": { "type": "application", "name": root_name, "version": root_version }
        },
        "components": components,
    })
}

/// Run `cargo metadata` for an app and produce the pretty SBOM JSON.
pub fn generate(app_root: &std::path::Path, root_name: &str, root_version: &str) -> Result<String, String> {
    let output = std::process::Command::new("cargo")
        .current_dir(app_root)
        .args(["metadata", "--format-version", "1"])
        .output()
        .map_err(|e| format!("cargo metadata failed to run: {e}"))?;
    if !output.status.success() {
        return Err(format!("cargo metadata failed: {}", String::from_utf8_lossy(&output.stderr)));
    }
    let metadata: Value = serde_json::from_slice(&output.stdout).map_err(|e| format!("cargo metadata parse: {e}"))?;
    let doc = document(&metadata, root_name, root_version);
    let mut s = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
    s.push('\n');
    Ok(s)
}
```

(NOTE: `cargo metadata` includes the full dependency graph by default — no `--no-deps`. With a path-overridden local `jerrycan`, the graph includes the framework crates; the root `app` package is correctly excluded as `metadata.component`.)

- [ ] **Step 4: Run to verify pass** — `cargo test -p jerrycan sbom` → 2 PASS. Full gate green.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan/src/platform/sbom.rs crates/jerrycan/src/platform/mod.rs
git commit -m "Generate CycloneDX 1.5 SBOMs from cargo metadata"
```

---

### Task 12: `jerrycan package` — artifact emitters

**Files:**
- Create: `crates/jerrycan/src/platform/package.rs` (+ `pub mod package;` in mod.rs)
- Modify: `crates/jerrycan/src/main.rs`
- Create: `crates/jerrycan/tests/package.rs`

`package` emits text artifacts (Dockerfile, k8s YAML, systemd unit, SBOM) deterministically, and for `--binary`/`--docker` invokes the toolchain (cargo/docker) gated on availability. The FAST tests assert the text artifacts; the HEAVY conformance proves real builds.

- [ ] **Step 1: Write the failing fast tests** (`crates/jerrycan/tests/package.rs`)

```rust
//! Artifact-emission assertions (fast: text generation, no real cargo/docker builds).

use jerrycan::platform::package;
use jerrycan::platform::design::Design;

const GOLDEN: &str = include_str!("../../../conformance/designs/todo-api.design.json");

fn design() -> Design { serde_json::from_str(GOLDEN).unwrap() }

#[test]
fn dockerfile_is_distroless_nonroot_static() {
    let df = package::dockerfile(&design(), false);
    assert!(df.contains("FROM rust:") && df.contains(" AS build"), "{df}");
    assert!(df.contains("x86_64-unknown-linux-musl"), "static target: {df}");
    assert!(df.contains("FROM gcr.io/distroless/static") || df.contains("FROM scratch"), "{df}");
    assert!(df.contains("USER nonroot") || df.contains("USER 65532"), "non-root: {df}");
    assert!(df.contains("EXPOSE 8000"));
    assert!(df.contains("ENV JERRYCAN_ADDR=0.0.0.0:8000"), "bind all interfaces in container: {df}");
}

#[test]
fn k8s_manifests_are_hardened() {
    let y = package::k8s_manifests(&design());
    assert!(y.contains("kind: Deployment") && y.contains("kind: Service") && y.contains("kind: NetworkPolicy"));
    assert!(y.contains("runAsNonRoot: true"));
    assert!(y.contains("readOnlyRootFilesystem: true"));
    assert!(y.contains("allowPrivilegeEscalation: false"));
    assert!(y.contains("drop:\n                - ALL") || y.contains("- ALL"), "drop all caps: {y}");
    assert!(y.contains("livenessProbe") && y.contains("/healthz"));
    assert!(y.contains("resources:") && y.contains("limits:"));
    assert!(y.contains("name: todo-api"));
}

#[test]
fn systemd_unit_is_hardened() {
    let u = package::systemd_unit(&design());
    assert!(u.contains("[Service]"));
    assert!(u.contains("DynamicUser=yes"));
    assert!(u.contains("ProtectSystem=strict"));
    assert!(u.contains("NoNewPrivileges=yes"));
    assert!(u.contains("PrivateTmp=yes"));
    assert!(u.contains("Restart=on-failure"));
}

#[test]
fn package_writes_text_targets_into_a_deploy_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("todo-api");
    jerrycan::platform::scaffold::scaffold(&root, &design()).unwrap();
    // text-only target: no toolchain needed
    let written = package::emit_text_artifacts(&root, &design(), &["k8s", "systemd", "docker"]).unwrap();
    assert!(root.join("deploy/Dockerfile").exists());
    assert!(root.join("deploy/k8s.yaml").exists());
    assert!(root.join("deploy/todo-api.service").exists());
    assert!(written.iter().any(|p| p.contains("deploy/k8s.yaml")));
}
```

- [ ] **Step 2: Run to verify failure** — compile FAIL.

- [ ] **Step 3: Implement package.rs**

```rust
use super::design::Design;
use std::path::Path;

const PORT: u16 = 8000;

/// A hardened multi-stage Dockerfile. `musl` toggles the static-target build;
/// when false (musl unavailable), a glibc build on a debian-slim runtime.
pub fn dockerfile(design: &Design, _glibc_fallback: bool) -> String {
    let name = &design.name;
    format!(
        r#"# GENERATED by jerrycan package — hardened, multi-stage, non-root.
FROM rust:1-bookworm AS build
WORKDIR /build
RUN rustup target add x86_64-unknown-linux-musl && \
    (apt-get update && apt-get install -y musl-tools || true)
COPY . .
RUN cargo build --release --target x86_64-unknown-linux-musl -p app
RUN cp target/x86_64-unknown-linux-musl/release/app /build/{name}

FROM gcr.io/distroless/static:nonroot
COPY --from=build /build/{name} /usr/local/bin/{name}
USER nonroot
EXPOSE {PORT}
ENV JERRYCAN_ADDR=0.0.0.0:{PORT}
ENTRYPOINT ["/usr/local/bin/{name}"]
"#
    )
}

/// Deployment + Service + NetworkPolicy, security-hardened.
pub fn k8s_manifests(design: &Design) -> String {
    let name = &design.name;
    format!(
        r#"# GENERATED by jerrycan package — hardened manifests. Edit the image, then `kubectl apply -f k8s.yaml`.
apiVersion: apps/v1
kind: Deployment
metadata:
  name: {name}
  labels:
    app: {name}
spec:
  replicas: 2
  selector:
    matchLabels:
      app: {name}
  template:
    metadata:
      labels:
        app: {name}
    spec:
      securityContext:
        runAsNonRoot: true
        runAsUser: 65532
        seccompProfile:
          type: RuntimeDefault
      containers:
        - name: {name}
          image: {name}:latest
          ports:
            - containerPort: {PORT}
          env:
            - name: JERRYCAN_ADDR
              value: "0.0.0.0:{PORT}"
          securityContext:
            allowPrivilegeEscalation: false
            readOnlyRootFilesystem: true
            capabilities:
              drop:
                - ALL
          livenessProbe:
            httpGet:
              path: /healthz
              port: {PORT}
            initialDelaySeconds: 2
            periodSeconds: 10
          readinessProbe:
            httpGet:
              path: /healthz
              port: {PORT}
            initialDelaySeconds: 1
            periodSeconds: 5
          resources:
            requests:
              cpu: 50m
              memory: 32Mi
            limits:
              cpu: 500m
              memory: 128Mi
---
apiVersion: v1
kind: Service
metadata:
  name: {name}
spec:
  selector:
    app: {name}
  ports:
    - port: 80
      targetPort: {PORT}
---
apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata:
  name: {name}
spec:
  podSelector:
    matchLabels:
      app: {name}
  policyTypes:
    - Ingress
  ingress:
    - ports:
        - protocol: TCP
          port: {PORT}
"#
    )
}

/// A hardened systemd unit (binary at /usr/local/bin/<name>).
pub fn systemd_unit(design: &Design) -> String {
    let name = &design.name;
    format!(
        r#"# GENERATED by jerrycan package. Install: copy the binary to /usr/local/bin/{name},
# this file to /etc/systemd/system/{name}.service, then `systemctl enable --now {name}`.
[Unit]
Description={name} (jerrycan)
After=network.target

[Service]
ExecStart=/usr/local/bin/{name}
Environment=JERRYCAN_ADDR=0.0.0.0:{PORT}
Environment=JERRYCAN_ENV=prod
DynamicUser=yes
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
PrivateDevices=yes
Restart=on-failure
RestartSec=2

[Install]
WantedBy=multi-user.target
"#
    )
}

/// Write the text artifacts for the requested targets into `<app>/deploy/`.
/// Returns the relative paths written. (Binary/image builds are separate.)
pub fn emit_text_artifacts(app_root: &Path, design: &Design, targets: &[&str]) -> Result<Vec<String>, String> {
    let deploy = app_root.join("deploy");
    std::fs::create_dir_all(&deploy).map_err(|e| e.to_string())?;
    let mut written = Vec::new();
    let mut write = |rel: &str, content: &str| -> Result<(), String> {
        let path = deploy.join(rel);
        std::fs::write(&path, content).map_err(|e| format!("write deploy/{rel}: {e}"))?;
        written.push(format!("deploy/{rel}"));
        Ok(())
    };
    if targets.contains(&"docker") {
        write("Dockerfile", &dockerfile(design, false))?;
    }
    if targets.contains(&"k8s") {
        write("k8s.yaml", &k8s_manifests(design))?;
    }
    if targets.contains(&"systemd") {
        write(&format!("{}.service", design.name), &systemd_unit(design))?;
    }
    Ok(written)
}
```

NOTE on the dockerfile test: it asserts `EXPOSE 8000` and the distroless line — the implementer must keep the literal `{PORT}` substitution producing `8000`. The k8s `drop: - ALL` assertion is whitespace-tolerant (the test checks `"- ALL"`).

- [ ] **Step 4: Implement the CLI command** (main.rs)

`Cmd::Package` already exists from Phase 0/1 (clap `--docker|--binary|--k8s|--systemd`). Replace the unimplemented arm. Add the orchestration:

```rust
fn cmd_package(targets: &PackageTargets, json_mode: bool) -> Result<(), Failure> {
    let root = app_root()?;
    let design = load_design(&root.join("design.json"))?;

    // Gate: never package an app that doesn't pass check.
    let report = checkpipe::run_all(&root, &design, None).map_err(Failure::environment)?;
    if !report.ok {
        return Err(Failure::gate(format!("check failed ({} diagnostics) — fix before packaging", report.diagnostics.len())));
    }

    let mut artifacts = Vec::new();
    let mut text_targets = Vec::new();
    if targets.docker { text_targets.push("docker"); }
    if targets.k8s { text_targets.push("k8s"); }
    if targets.systemd { text_targets.push("systemd"); }
    if !text_targets.is_empty() {
        artifacts.extend(package::emit_text_artifacts(&root, &design, &text_targets).map_err(Failure::gate)?);
    }
    if targets.binary {
        artifacts.push(package::build_binary(&root, &design).map_err(Failure::gate)?);
    }

    // SBOM always (it's cheap and the safety pipeline wants it).
    let version = "0.1.0";
    let sbom = sbom::generate(&root, "app", version).map_err(Failure::gate)?;
    std::fs::write(root.join("deploy/sbom.json"), &sbom).map_err(|e| Failure::gate(e.to_string()))?;
    artifacts.push("deploy/sbom.json".to_string());

    let payload = serde_json::json!({
        "artifacts": artifacts,
        "sbom": "deploy/sbom.json",
        "next_step": "deploy with your own tooling (kubectl apply -f deploy/k8s.yaml, docker build, scp the binary + systemd unit)",
    });
    emit(json_mode, &payload, &format!("packaged {} artifact(s)", artifacts.len()));
    Ok(())
}
```

`package::build_binary` (in package.rs): try the musl target, fall back to host, copy to `deploy/<name>`:

```rust
/// Build a release binary, preferring static musl; falls back to the host
/// target with a note. Returns the relative artifact path.
pub fn build_binary(app_root: &Path, design: &Design) -> Result<String, String> {
    let musl = "x86_64-unknown-linux-musl";
    let musl_ok = std::process::Command::new("rustc")
        .args(["--print", "target-list"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().any(|t| t == musl))
        .unwrap_or(false)
        && target_installed(musl);
    let (target_args, built_path) = if musl_ok {
        (vec!["--target", musl], format!("target/{musl}/release/app"))
    } else {
        eprintln!("jerrycan package: musl target unavailable — building a host-target binary (not fully static). Install with: rustup target add {musl}");
        (vec![], "target/release/app".to_string())
    };
    let mut args = vec!["build", "--release", "-p", "app"];
    args.extend(target_args);
    let status = std::process::Command::new("cargo").current_dir(app_root).args(&args).status()
        .map_err(|e| format!("cargo build failed to run: {e}"))?;
    if !status.success() {
        return Err("release build failed".to_string());
    }
    let deploy = app_root.join("deploy");
    std::fs::create_dir_all(&deploy).map_err(|e| e.to_string())?;
    let dest = deploy.join(&design.name);
    std::fs::copy(app_root.join(&built_path), &dest).map_err(|e| format!("copy binary: {e}"))?;
    Ok(format!("deploy/{}", design.name))
}

fn target_installed(target: &str) -> bool {
    std::process::Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().any(|t| t == target))
        .unwrap_or(false)
}
```

Wire `Cmd::Package` → `cmd_package`; the existing clap struct exposes the four bool flags as `PackageTargets` (adjust to the existing shape — if Phase 0 defined them as separate `--docker` etc. bools on the `Package` variant, read them into a local struct). Imports: `use jerrycan::platform::{package, sbom, checkpipe};`.

- [ ] **Step 5: Run to verify pass** — `cargo test -p jerrycan --test package` → 4 PASS. Full `--all-features` gate green.

- [ ] **Step 6: Commit**

```bash
git add crates/jerrycan/src/platform/package.rs crates/jerrycan/src/platform/mod.rs crates/jerrycan/src/main.rs crates/jerrycan/tests/package.rs
git commit -m "Add jerrycan package emitting hardened Docker, k8s, systemd, binary, and SBOM"
```

---
### Task 13: Heavy — the deploy-anywhere exit criterion

**Files:**
- Modify: `crates/jerrycan/tests/conformance.rs`
- Modify: `.github/workflows/ci.yml`

- [ ] **Step 1: CI gains the musl target + tools**

In `.github/workflows/ci.yml`, after the toolchain step, add musl to the target list and install musl-tools:

```yaml
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt, clippy
          targets: x86_64-unknown-linux-musl
      - name: Install musl + container tools
        run: sudo apt-get update && sudo apt-get install -y musl-tools
```

(GitHub ubuntu runners have `docker` preinstalled; no extra install needed. `kubeconform` is optional — the k8s heavy test uses `kubectl --dry-run=client` if present, else a structural YAML check.) Validate the YAML locally afterward.

- [ ] **Step 2: Write the deploy-anywhere heavy test** (append to conformance.rs)

```rust
/// Spec §11 Phase 3 exit: the golden app deploys to Docker + k8s + bare server
/// from one command. Each leg is gated on its tool; missing tools SKIP that leg
/// loudly (CI has cargo+musl+docker). The binary leg is unconditional.
#[test]
#[ignore = "heavy: package the golden app and prove binary/docker/k8s deploy paths"]
fn golden_app_deploys_everywhere() {
    let tmp = tempfile::tempdir().unwrap();
    // Reuse the memory-mode golden app (deploy paths are storage-agnostic).
    let app = scaffold_golden(tmp.path()); // existing Phase-1 helper (memory mode)
    for (fixture, target) in [
        ("todos_handlers.rs", "crates/routes/todos/src/handlers.rs"),
        ("comments_handlers.rs", "crates/routes/todos/src/subroutes/comments/handlers.rs"),
        ("users_handlers.rs", "crates/routes/users/src/handlers.rs"),
    ] {
        std::fs::copy(repo_root().join("conformance/fixtures").join(fixture), app.join(target)).unwrap();
    }

    // ONE command emits every artifact (after a green check gate).
    let out = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .current_dir(&app)
        .args(["--json", "package", "--binary", "--docker", "--k8s", "--systemd"])
        .output()
        .unwrap();
    assert!(out.status.success(), "package failed: {}", String::from_utf8_lossy(&out.stderr));
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let artifacts = payload["artifacts"].as_array().unwrap();
    for expected in ["deploy/Dockerfile", "deploy/k8s.yaml", "deploy/todo-api.service", "deploy/todo-api", "deploy/sbom.json"] {
        assert!(artifacts.iter().any(|a| a == expected) || app.join(expected).exists(), "missing {expected}");
    }
    // SBOM is valid CycloneDX.
    let sbom: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(app.join("deploy/sbom.json")).unwrap()).unwrap();
    assert_eq!(sbom["bomFormat"], "CycloneDX");
    assert!(sbom["components"].as_array().unwrap().iter().any(|c| c["name"] == "tokio"));

    // BARE SERVER leg: run the built binary directly, curl it.
    let port = pick_port();
    let addr = format!("127.0.0.1:{port}");
    let mut bin = Command::new(app.join("deploy/todo-api"))
        .env("JERRYCAN_ADDR", &addr)
        .spawn()
        .expect("packaged binary runs");
    await_listen(&addr, 60);
    assert!(http_get(&addr, "/todos/").starts_with("HTTP/1.1 200"), "bare binary serves");
    let _ = bin.kill();
    let _ = bin.wait();

    // DOCKER leg (gated): build the image, run it, curl it.
    if tool_present("docker") {
        let tag = "jerrycan-conformance:test";
        let build = Command::new("docker").current_dir(&app).args(["build", "-f", "deploy/Dockerfile", "-t", tag, "."]).status().unwrap();
        assert!(build.success(), "docker build");
        let port = pick_port();
        let run = Command::new("docker")
            .args(["run", "-d", "--rm", "-p", &format!("{port}:8000"), "--name", "jerrycan-conformance", tag])
            .output()
            .unwrap();
        assert!(run.status.success(), "docker run: {}", String::from_utf8_lossy(&run.stderr));
        let addr = format!("127.0.0.1:{port}");
        await_listen(&addr, 60);
        let body = http_get(&addr, "/todos/");
        let _ = Command::new("docker").args(["stop", "jerrycan-conformance"]).status();
        let _ = Command::new("docker").args(["rmi", "-f", tag]).status();
        assert!(body.starts_with("HTTP/1.1 200"), "containerized app serves: {body}");
    } else {
        eprintln!("SKIP docker leg: docker not present");
    }

    // K8S leg (gated): validate the manifests parse + are structurally appl-able.
    if tool_present("kubectl") {
        let out = Command::new("kubectl")
            .current_dir(&app)
            .args(["apply", "--dry-run=client", "-f", "deploy/k8s.yaml"])
            .output()
            .unwrap();
        assert!(out.status.success(), "kubectl dry-run: {}", String::from_utf8_lossy(&out.stderr));
    } else {
        // Structural fallback: every YAML doc parses and has kind+apiVersion.
        let y = std::fs::read_to_string(app.join("deploy/k8s.yaml")).unwrap();
        let docs: Vec<&str> = y.split("\n---\n").collect();
        assert_eq!(docs.len(), 3, "Deployment + Service + NetworkPolicy");
        for d in docs {
            assert!(d.contains("apiVersion:") && d.contains("kind:"), "valid manifest doc");
        }
        eprintln!("SKIP kubectl dry-run: kubectl not present — used structural validation");
    }
}

// Small helpers (add near the other conformance helpers if not already present).
fn pick_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}
fn tool_present(tool: &str) -> bool {
    Command::new(tool).arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
}
fn await_listen(addr: &str, secs: u64) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    while std::time::Instant::now() < deadline {
        if std::net::TcpStream::connect(addr).is_ok() { return; }
        std::thread::sleep(std::time::Duration::from_millis(400));
    }
    panic!("nothing listening on {addr} after {secs}s");
}
fn http_get(addr: &str, path: &str) -> String {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    s.write_all(format!("GET {path} HTTP/1.1\r\nHost: l\r\nConnection: close\r\n\r\n").as_bytes()).unwrap();
    let mut buf = Vec::new();
    let _ = s.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}
```

(If `pick_port`/`http_get`/`await_listen` already exist in conformance.rs from earlier phases, reuse them and drop the duplicates — record which.)

- [ ] **Step 3: Run it**

`cargo test -p jerrycan --all-features --test conformance golden_app_deploys -- --include-ignored`
Expected: binary leg PASSES; docker leg runs if Docker present (locally optional — report which legs ran); k8s leg uses dry-run or structural fallback. Budget 10-25 min cold (musl build + docker build). Generator/package bugs fixed in package.rs/templates, recorded. **This green run (with the docker leg) IS the Phase 3 exit criterion** — CI proves the docker leg unconditionally.

- [ ] **Step 4: Commit**

```bash
git add crates/jerrycan/tests/conformance.rs .github/workflows/ci.yml
git commit -m "Prove the deploy-anywhere exit criterion across binary, docker, and k8s"
```

---

### Task 14: Docs, backlog, README + Phase 3 exit gate

**Files:**
- Create: `docs/ai/10-auth.md`, `docs/ai/11-observability.md`, `docs/ai/12-packaging.md`
- Modify: `crates/jerrycan/src/lib.rs` (doc mounts), `docs/ai/05-errors.md`, `docsidx.rs`, `docs/contracts/cli-ux.md`, `docs/phase1-backlog.md`, `README.md`

- [ ] **Step 1: Write `docs/ai/10-auth.md`** (doc-tested under `--all-features`)

````markdown
# Authentication

## Purpose
`jerrycan::auth` provides password hashing (argon2), encrypted session cookies,
HS256 JWTs, and role guards. Enable with the design dependency `"auth"` plus an
`auth` block (`{ "model": "session"|"jwt", "roles": [...] }`), or `jerrycan add auth`.
Guards are dependencies (spec §4.3): a `Session`/`Bearer` extractor in a handler
signature is the gate.

## Signature
```rust
# use jerrycan::prelude::*;
use jerrycan::auth::{require_role, Session};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct User { id: i64, role: String }

async fn dashboard(Session(user): Session<User>) -> Result<String> {
    Ok(format!("welcome user {}", user.id))   // unauthenticated → 401 automatically
}

async fn admin_delete(Session(user): Session<User>) -> Result<NoContent> {
    require_role(&user.role, "admin")?;        // wrong role → 403
    Ok(NoContent)
}
# let _ = (dashboard, admin_delete);
```

## Minimal example
```rust
# use jerrycan::prelude::*;
# use jerrycan::auth::{Auth, Session};
# use serde::{Deserialize, Serialize};
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
#[derive(Serialize, Deserialize)]
struct User { id: i64, role: String }
async fn me(Session(u): Session<User>) -> Json<i64> { Json(u.id) }

let auth = Auth::with_secret("a-very-long-development-secret-string!!");
let cookie = auth.sessions().set_cookie(&User { id: 42, role: "user".into() }).unwrap();
let cookie_pair = cookie.split(';').next().unwrap().to_string();

let t = App::new().extend(auth).route("/me", get(me)).into_test();
assert_eq!(t.get("/me").await.status(), jerrycan::http::StatusCode::UNAUTHORIZED);
assert_eq!(t.get_with("/me", &[("cookie", &cookie_pair)]).await.json::<i64>(), 42);
# }); }
```

## Variations
- Passwords: `jerrycan::auth::hash_password(pw)` → PHC string for storage;
  `verify_password(pw, &stored)` → bool. Always argon2id, random salt.
- JWT: `Bearer<Claims>` extracts+verifies `Authorization: Bearer <token>`; mint
  with `jerrycan::auth::jwt::encode(&claims, auth.jwt_key())`. Include `exp`.
- Secret: `Auth::from_env()` reads `JERRYCAN_SECRET` (>= 32 bytes). In
  production (`JERRYCAN_ENV=prod`) a missing/short secret is a startup error.

## Errors you'll hit
- Missing/invalid session cookie or bearer token → `401 JC0401`.
- Authenticated but wrong role (`require_role`) → `403 JC0403`.
- Tampered cookie/JWT → `401` (AEAD/signature rejects it; never a panic).

## Anti-patterns
- Don't put secrets in a JWT — it's signed, not encrypted (readable by anyone).
  Server-private state goes in the AEAD session cookie.
- Don't check auth inside handler bodies with ad-hoc logic — declare a
  `Session`/`Bearer` guard in the signature so the contract is visible.
````

- [ ] **Step 2: Write `docs/ai/11-observability.md`** (doc-tested)

````markdown
# Observability

## Purpose
`jerrycan::observe` adds request IDs, structured JSON access logs, a `/healthz`
liveness route, and a Prometheus `/metrics` endpoint. Enable with the design
dependency `"observe"` (or `jerrycan add observe`).

## Signature
```rust
# use jerrycan::prelude::*;
use jerrycan::observe::Observe;

# fn build() -> App {
App::new().extend(Observe::new())   // adds the access-log middleware + /healthz + /metrics
# }
# let _ = build();
```

## Minimal example
```rust
# use jerrycan::prelude::*;
# use jerrycan::observe::Observe;
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
let t = App::new().extend(Observe::new()).route("/x", get(|| async { "x" })).into_test();
let res = t.get("/x").await;
assert!(res.headers().get("x-request-id").is_some());           // every response stamped
assert_eq!(t.get("/healthz").await.text(), "ok");
assert!(t.get("/metrics").await.text().contains("jerrycan_requests_total"));
# }); }
```

## Variations
- Call `jerrycan::observe::init_logging()` once in `main` for JSON logs to
  stdout (honors `RUST_LOG`). Generated apps do this automatically.
- Scrape `/metrics` with Prometheus; `/healthz` for k8s liveness/readiness
  (the generated k8s manifests already point probes at it).

## Errors you'll hit
- `/healthz` and `/metrics` are RESERVED when observe is on — defining design
  routes at those paths is a build-time conflict (fail loud).

## Anti-patterns
- Don't roll your own request-id middleware alongside Observe — it owns the
  `x-request-id` header and the access log.
````

- [ ] **Step 3: Write `docs/ai/12-packaging.md`** (prose; no doc-tests — it documents a CLI command)

````markdown
# Packaging & Deployment

`jerrycan package` turns a checked app into deployable artifacts. It runs the
full verification gate first (build + clippy + audit + deny + tests + lints) and
refuses to package a failing app. Nothing is deployed — artifacts land in
`deploy/`, and you push them with your own tooling.

## Targets
- `--binary` — a release binary (static musl when available, host-target
  fallback otherwise), copied to `deploy/<name>`.
- `--docker` — `deploy/Dockerfile`: multi-stage, static musl build, distroless
  non-root runtime, binds `0.0.0.0:8000`.
- `--k8s` — `deploy/k8s.yaml`: Deployment + Service + NetworkPolicy, hardened
  (`runAsNonRoot`, `readOnlyRootFilesystem`, dropped capabilities, resource
  limits, `/healthz` probes).
- `--systemd` — `deploy/<name>.service`: `DynamicUser`, `ProtectSystem=strict`,
  `NoNewPrivileges`, `PrivateTmp`, restart-on-failure.

Every run also emits `deploy/sbom.json` — a CycloneDX 1.5 software bill of
materials from the full dependency graph.

## Example
```
jerrycan package --binary --docker --k8s --systemd
# → deploy/{<name>, Dockerfile, k8s.yaml, <name>.service, sbom.json}
docker build -f deploy/Dockerfile -t myapp .
kubectl apply -f deploy/k8s.yaml
```

## Production checklist
- Set `JERRYCAN_SECRET` (>= 32 bytes) and `JERRYCAN_ENV=prod` (the systemd unit
  sets the latter; provide the secret via your secrets manager).
- Set `JERRYCAN_DATABASE_URL` for db-backed apps.
- The container binds `0.0.0.0:8000`; the Service maps port 80 → 8000.
````

- [ ] **Step 4: Mount + index + tables + cli-ux + backlog + README**

(a) `crates/jerrycan/src/lib.rs` `#[cfg(doctest)]` block:

```rust
    #[cfg(feature = "auth")]
    doc_page!(page_10_auth, "../../../docs/ai/10-auth.md");
    #[cfg(feature = "observe")]
    doc_page!(page_11_observability, "../../../docs/ai/11-observability.md");
    doc_page!(page_12_packaging, "../../../docs/ai/12-packaging.md");
```

(b) `docsidx.rs` PAGES: add `auth`, `observability`, `packaging` (ungated).

(c) `docs/ai/05-errors.md` table: add

```markdown
| JC0401 | 401 | Authentication required or failed (jerrycan::auth) |
| JC0403 | 403 | Authenticated but not permitted (require_role) |
```

(d) `docs/contracts/cli-ux.md`: the `jerrycan package` row's detail → `--binary|--docker|--k8s|--systemd; runs the check gate first, emits deploy/ artifacts + CycloneDX SBOM; never deploys`. Add an `add` row entry for auth/observe if the Phase-2 row enumerated db/validate (extend the list).

(e) `docs/contracts/design-schema.json`: the top-level `dependencies` description gains `'auth'` (sessions/JWT/guards via jerrycan-auth) and `'observe'` (request IDs, /healthz, /metrics via jerrycan-observe) to the reserved-names list. The `auth` block already exists in the schema (model/roles); add a description note that `model: "session"|"jwt"` activates auth-mode generation.

(f) `docs/phase1-backlog.md` "Contract v1 candidates": add

```markdown
- OAuth2/OIDC flows and refresh tokens (v0 ships sessions + HS256 JWT only)
- RS256/asymmetric JWT signing (v0 is HS256 symmetric)
- per-route rate limiting as a first-class extension (today: write a middleware)
- multi-arch container images (v0 Dockerfile is x86_64; add buildx arm64)
```

(g) `README.md`: roadmap Phase 3 → `✅ complete`; Phase 4 → `next`. Architecture block: drop `(Phase 3)` markers on auth/observe; mention `jerrycan package`. Update the "Why jerrycan / Deploy anywhere" row to present tense.

- [ ] **Step 5: The Phase 3 exit gate (run ALL, report truthfully)**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo test -p jerrycan --test conformance -- --include-ignored
cargo test -p jerrycan --test genroute_compile -- --include-ignored
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
python3 -c "import yaml; yaml.safe_load(open('.github/workflows/ci.yml'))"
```

(The conformance run includes the auth+observe and deploy-anywhere heavy tests; the docker leg runs if Docker is present locally, else SKIPs — CI proves it.)

- [ ] **Step 6: Commit**

```bash
git add docs crates/jerrycan/src/lib.rs crates/jerrycan/src/platform/docsidx.rs README.md
git commit -m "Document auth, observability, and packaging and close Phase 3"
```

---

## Execution notes

- **Order:** 1 → 14 strictly. Task 5 is the auth keystone (Session/Bearer/require_role + Auth extension); Task 9 is the generation ripple (touches many platform files — keep memory/db/validate output byte-stable, update GenMode literals); Task 10 and 13 are the heavy reckonings (auth guards over HTTP; deploy-anywhere).
- **Gates carry `--all-features`** everywhere. The crypto rule: RustCrypto primitives only; hand-roll envelopes/codecs (decision #1) — never a primitive.
- **Heavy tests** (`#[ignore]`): Task 10 (auth+observe serve), Task 13 (binary/docker/k8s). CI runs them with musl + docker; locally the docker/k8s legs SKIP gracefully if the tools are absent. The bare-binary leg always runs.
- **Pre-solved traps:** `ctx.resolve::<T>()` must be `pub` (sibling-crate extractors call it); GenMode gained a field (update all Phase-2 literals); `/healthz` `/metrics` are reserved paths under observe; the in-test session cookie must use the SAME secret the served app uses (`JERRYCAN_SECRET`); SessionUser JSON shape must match the in-test `json!` shape; musl falls back gracefully so `--binary` never hard-fails on a gnu-only host.
- **Out of scope (tracked):** OAuth/OIDC, RS256, rate-limiting extension, multi-arch images (all backlogged in Task 14); fuzzing + agent-evals + v0.1.0 release are Phase 4.
