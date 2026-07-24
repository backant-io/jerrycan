# Security: jsonwebtoken 9.3.1 -> 10.3.0 (0.6.6) — #162

**Date:** 2026-07-24
**Status:** Approved, pre-implementation
**Issue:** #162 — GHSA-h395-gr6q-cpjc: jsonwebtoken type confusion → potential authorization bypass; vulnerable `< 10.3.0`, patched `10.3.0`. `cargo audit` is clean (GHSA-only, not in RustSec), so the CI gate did not catch it.
**Ships as:** 0.6.6 — a security patch. Internal dependency upgrade + a jerrycan-auth migration; jerrycan-auth's own public API stays stable (`cargo semver-checks` clean).

## Scope
`jsonwebtoken` is used in exactly ONE place: `crates/jerrycan-auth/src/idtoken.rs` — the OAuth **provider id-token / JWKS** validation path (`use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header}`). The `session.rs`/`lib.rs` `.decode()` calls are the session store's own encrypted-cookie decode (NOT jsonwebtoken) — untouched. So the migration surface is one file.

## The change
1. Raise the workspace `jsonwebtoken` requirement (root Cargo.toml) `"9"` → `"10.3"`, keep `default-features = false, features = ["use_pem"]` (ring-backed, no `rsa` crate).
2. Migrate `idtoken.rs` to the jsonwebtoken 10.x API (`DecodingKey`, `Validation`, `decode`, `decode_header`, `Algorithm`, and the error type in `map_jwt_error`). Fix the compile errors the bump surfaces; keep the behavior identical.
3. **Security invariant — MUST be preserved (this is the whole point):** the RS256 pin stays explicit in BOTH places — the `header.alg != Algorithm::RS256` reject (idtoken.rs:291) AND `Validation::new(Algorithm::RS256)` / its `algorithms` allowlist (idtoken.rs:306). The migration must NOT loosen the algorithm allowlist, must NOT accept `alg: none`, and must NOT allow an HMAC alg with an RSA key (the confusion class). If jsonwebtoken 10 changed how the algorithm allowlist is set on `Validation`, set it explicitly to `[RS256]`.

## Verification
- `crates/jerrycan-auth` unit tests + the `mock-idp` harness (the deterministic mock OAuth2 IdP) pass: `cargo test -p jerrycan-auth --features mock-idp` (or the workspace equivalent). A valid RS256 id token verifies; a wrong-kid / wrong-issuer / expired / tampered token is rejected; a non-RS256 (`HS256`, `none`) token is rejected.
- `cargo audit` + the Dependabot alert clear (10.3.0 resolves the advisory).
- `cargo semver-checks -p jerrycan-auth` clean (no public-API change from the migration).
- Full workspace green; heavy gate green.

## Non-goals
No behavior change to token validation semantics beyond the version bump; no change to the session-store crypto (different code path); no new features.
