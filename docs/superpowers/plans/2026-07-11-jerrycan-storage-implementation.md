# jerrycan-storage Implementation Plan

**Goal:** Ship `jerrycan-storage` — design-modeled buckets (`design.json` contract v2), a `BlobStore` trait with local + S3-compatible backends, DB-backed object metadata with owner/tenant/prefix scope enforcement, generated per-bucket endpoints + isolation tests, signed URLs — plus the small jerrycan-auth bcrypt-verify enhancement the migrator needs.

**Architecture:** The runtime lives in a new extension crate `crates/jerrycan-storage/` (same shape as `jerrycan-ratelimit`): a `Storage` service holding an `Arc<dyn BlobStore>` (LocalStore default, S3Store behind `storage-s3` on jerrycan's own hyper+rustls stack) and doing all metadata/scope/signing work against `jerrycan-db`'s `storage_objects` table. The platform side introduces contract v2 (`storage` block in design.json), a new `storagegen.rs` generator that emits an entirely TOOL-owned `crates/storage/` crate (real handlers, not stubs — bucket behavior is fully deterministic from the design) plus generated acceptance/isolation tests, and the mounting/validation/facade plumbing that the realtime plan will later extend.

**Tech Stack:** Rust (edition 2024, workspace 0.2.0), hyper/hyper-util/hyper-rustls (rustls+ring only), hmac + sha2 (SigV4 + app-HMAC), quick-xml (the single new third-party crate for storage), sea-orm raw-SQL via jerrycan-db (same idiom as jerrycan-jobs' PostgresStore), tokio fs, MinIO for the S3 integration test, bcrypt crate for the auth task.

For agentic workers: execute task-by-task; steps use `- [ ]` checkboxes.

---

## Coordination note (storage owns the v2 plumbing)

Storage OWNS the introduction of design.json **contract_version 2** and the reusable reserved-dependency plumbing: `design.rs` `wants_storage()` + the `facade_features()` push, the `mounting.rs` reserved-name filter entry, the v2-gated validation in `questions.rs`, and the facade feature wiring. The **realtime plan will extend exactly these seams** — keep every addition generic (one new `wants_*`, one filter arm, one feature push, one validation block; no storage-specific special cases in shared code paths).

## Spec resolutions (ambiguities resolved here — read before implementing)

1. **JC0415 already exists.** `Error::unsupported_media_type()` (415/JC0415) already ships in jerrycan-core and is registered in `codes.rs` (the Multipart extractor emits it). The spec's "new JC0415" becomes: reuse the constructor for `allowed_mime` violations and extend the registry `cause`/docs text to cover bucket MIME allowlists (Task 16).
2. **`wants_storage()`, not `has_storage()`.** The codebase convention is `wants_db/wants_jobs/wants_oauth` — the new gate follows it (Rule 11).
3. **`storage_objects` DDL is framework-owned** (`STORAGE_MIGRATIONS` in jerrycan-storage, mirroring `JOBS_MIGRATIONS`), with `owner_id TEXT NULL` / `tenant_id TEXT NULL` holding the *stringified* principal/tenant pk. The spec's "`<owner pk type>`" would force per-app DDL and a new aggregation path in mounting; TEXT keys cover i64/string/uuid pks uniformly with exact-equality scoping (Supabase itself stores `owner` as uuid text). Applied via `db.migrate(jerrycan::storage::STORAGE_MIGRATIONS)` exactly like jobs.
4. **"storage requires db/auth" are validation questions, not auto-adds** — mirrors jobs (`questions.rs` rejects jobs-without-db; nothing is silently added). Additionally storage requires an **active auth model** unconditionally: the spec's endpoint table guards every mutation (`POST`/`DELETE`/`sign` = "auth + owner/Tenant"), so even an all-public-bucket design needs auth.
5. **Upload shape:** `POST /<b>?key=<path>` with a raw body (`RawBody`); the request `Content-Type` is the stored mime. Multipart-form upload is NOT in v1 (a multipart filename can't carry `/`-nested keys; Supabase's own upload API is raw-body-per-path).
6. **Object identity:** `storage_objects.id` is a service-generated UUIDv4 string (TEXT pk); download/delete/sign address `/{id}`. `key` stays unique per bucket; a duplicate `(bucket, key)` upload is `409 JC0409` via the existing `db_error` unique-violation mapping.
7. **Default `max_size` = 50 MiB** when the bucket omits it (Supabase's default `file_size_limit` parity). The bucket's `max_size` is enforced twice: as the generated route's `.body_limit(n)` (transport-level `413 JC0413`) and again inside `Storage::put_object` (covers direct service calls).
8. **App-HMAC signs `bucket|object_id|exp`** (the signed URL is `/<b>/<id>?exp=…&sig=…`, so the id — not the key — is what the URL carries). Key material: `JERRYCAN_SECRET` (already required by auth, which storage designs must declare). HMAC-SHA256, hex, constant-time verify via `Mac::verify_slice`.
9. **`JERRYCAN_STORAGE` grammar:** `local:<root>` (default `local:./storage` when unset) or `s3://bucket?region=<r>&endpoint=<url>`. S3 credentials come from the standard `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY` env vars. Always path-style addressing (works on AWS, MinIO, R2). Plaintext `http://` endpoints are refused unless loopback (same TLS-downgrade guard as `jerrycan-auth::oauth`).
10. **Generated apps get the `storage-s3` facade feature** (which implies `storage`). The spec's "config by env, zero-touch" means a packaged binary must switch to S3 *without recompiling*, so the S3 backend must always be compiled into storage apps. The plain `storage` feature remains for library users who want local-only.
11. **`BlobStore` is object-safe with hand-boxed futures** (`BlobFuture`), not `async fn` as sketched in the spec — the crate convention (`RateLimitStore`, `JobFuture`, `TokenFuture`) and required for `Arc<dyn BlobStore>`.
12. **The generated `crates/storage/` crate is 100% TOOL-owned** (rewritten every generate, like jobs' `lib.rs`). Its generated tests pass immediately (handlers are real implementations, not TDD-red stubs), so `gen-tests` does NOT count them toward `expected_failing`.
13. **`Option<T>` extractor added to jerrycan-core** (blanket `FromRequest` impl): a private bucket's `GET /{id}` must accept *either* a scoped session *or* a valid `exp`/`sig` pair, which requires optional auth extraction. Small, generic, and realtime will want it too.
14. **`public` + tenant-scoped owner** is allowed (public read, scoped write) with **no validation question** — the spec's "flagged in the design summary" refers to a summary surface that doesn't exist yet; deferred (left as a comment in `questions.rs`).
15. **bcrypt task adds the `bcrypt` crate** (MIT, builds on RustCrypto's blowfish) — verification only; jerrycan never *mints* bcrypt hashes. `hash_password` stays argon2-only; a `needs_rehash()` helper lets generated login handlers transparently upgrade migrated users to argon2 on the next successful login.

## File Structure

```
Cargo.toml                                      # MODIFY: members + jerrycan-storage/quick-xml/bcrypt workspace deps
crates/jerrycan-storage/
├── Cargo.toml                                  # CREATE: extension-crate manifest (storage-s3 feature)
├── src/
│   ├── lib.rs                                  # CREATE: Storage service, Bucket rules, Scope, from_env, object_response, Extension impl
│   ├── store.rs                                # CREATE: BlobStore trait + BlobFuture + validate_key + LocalStore + MemoryStore
│   ├── sign.rs                                 # CREATE: app-HMAC signed-URL primitives (hex, sign, verify)
│   ├── meta.rs                                 # CREATE: STORAGE_MIGRATIONS + ObjectMeta + scoped metadata SQL
│   ├── sigv4.rs                                # CREATE (storage-s3): SigV4 header signing + query presign
│   ├── xml.rs                                  # CREATE (storage-s3): quick-xml S3 error + InitiateMultipartUploadResult parsing
│   └── s3_store.rs                             # CREATE (storage-s3): S3Store on hyper_util legacy client + hyper-rustls
└── tests/
    └── s3_minio.rs                             # CREATE (storage-s3): env-gated MinIO integration test
crates/jerrycan-core/src/extract.rs             # MODIFY: blanket Option<T> FromRequest impl
crates/jerrycan/Cargo.toml                      # MODIFY: storage + storage-s3 facade features, optional dep
crates/jerrycan/src/lib.rs                      # MODIFY: `pub use jerrycan_storage as storage;` + storage doc page
crates/jerrycan/src/platform/design.rs          # MODIFY: StorageDesign/BucketDesign/Visibility, wants_storage, facade_features, parse_size
crates/jerrycan/src/platform/questions.rs       # MODIFY: contract v2 + storage-block validation
crates/jerrycan/src/platform/codes.rs           # MODIFY: JC0415 cause text covers mime allowlists
crates/jerrycan/src/platform/storagegen.rs      # CREATE: generates crates/storage/ (modules, handlers, tests)
crates/jerrycan/src/platform/mod.rs             # MODIFY: `pub mod storagegen;`
crates/jerrycan/src/platform/mounting.rs        # MODIFY: reserved filter, extension block, migrations line, bucket mounts, members, app deps
docs/contracts/design-schema.json               # MODIFY: contract_version enum [0,1,2] + storage definition
docs/ai/13-error-codes.md                       # MODIFY: JC0415 row mentions bucket allowlists
docs/ai/18-storage.md                           # CREATE: storage docs page (doc-tested)
crates/jerrycan/embedded/ai/13-error-codes.md   # MODIFY: mirror of docs/ai (embedded copy)
crates/jerrycan/embedded/ai/18-storage.md       # CREATE: mirror of docs/ai (embedded copy)
crates/jerrycan/src/platform/docsidx.rs         # MODIFY: PAGES entry for the storage page
crates/jerrycan-auth/Cargo.toml                 # MODIFY: bcrypt dep
crates/jerrycan-auth/src/password.rs            # MODIFY: bcrypt-verify dispatch + needs_rehash
```

Generated OUTPUT (emitted by storagegen into a generated app — not files in this repo):

```
<app>/crates/storage/Cargo.toml                 # tool-owned manifest
<app>/crates/storage/src/lib.rs                 # tool-owned bucket module declarations
<app>/crates/storage/src/<bucket>.rs            # tool-owned per-bucket module: BUCKET const + module() + 5 handlers
<app>/crates/storage/tests/acceptance.rs        # tool-owned acceptance + isolation + negative-control tests
```

---

## Task 1 — Crate skeleton + `BlobStore` trait + `LocalStore` + `MemoryStore`

**Files:**
- Create: `crates/jerrycan-storage/Cargo.toml`, `crates/jerrycan-storage/src/lib.rs`, `crates/jerrycan-storage/src/store.rs`
- Modify: `Cargo.toml` (root: members + workspace dep)
- Test: in-module `#[cfg(test)]` in `store.rs`

- [ ] 1. Add the crate to the workspace. Root `Cargo.toml`: append `"crates/jerrycan-storage",` after `"crates/jerrycan-jobs",` in `members`, and add to `[workspace.dependencies]` after the `jerrycan-jobs` line:

```toml
jerrycan-storage = { path = "crates/jerrycan-storage", version = "0.2.0" }
```

Create `crates/jerrycan-storage/Cargo.toml`:

```toml
[package]
name = "jerrycan-storage"
description = "Object-storage extension for the jerrycan framework: design-modeled buckets, local + S3-compatible blob stores, signed URLs, owner/tenant-scoped metadata. https://jerrycan.cc"
version.workspace = true
edition.workspace = true
license.workspace = true
homepage.workspace = true
repository.workspace = true
authors.workspace = true
rust-version.workspace = true
keywords = ["storage", "s3", "blob", "web"]
categories = ["web-programming"]

[dependencies]
jerrycan-core = { workspace = true }
jerrycan-db = { workspace = true }
bytes.workspace = true
http.workspace = true
hmac.workspace = true
sha2.workspace = true
rand.workspace = true
serde.workspace = true
serde_json.workspace = true
tokio = { workspace = true, features = ["fs"] }

[dev-dependencies]
tokio = { workspace = true, features = ["macros", "rt-multi-thread"] }
tempfile = "3"
```

Create `crates/jerrycan-storage/src/lib.rs` (skeleton only for now):

```rust
//! Object storage as a jerrycan extension: design-modeled buckets, a pluggable
//! blob store (local filesystem default, S3-compatible behind `storage-s3`),
//! DB-backed object metadata, and signed URLs. <https://jerrycan.cc>
#![forbid(unsafe_code)]

pub mod store;

pub use store::{BlobFuture, BlobStore, LocalStore, MemoryStore};
```

- [ ] 2. Write the failing tests in `crates/jerrycan-storage/src/store.rs` (write the whole file with the tests referencing not-yet-written items — TDD-red is a compile failure first):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    #[test]
    fn validate_key_rejects_traversal_and_junk() {
        // WHY: keys become filesystem paths in LocalStore and object paths in
        // S3Store — a traversal key would escape the bucket root.
        for bad in ["", "/abs", "a//b", "../up", "a/../b", "a/./b", "trail/", "spa ce", "quo\"te"] {
            assert!(validate_key(bad).is_err(), "key {bad:?} must be rejected");
        }
        for ok in ["a.png", "folder/file.pdf", "1/deep/path-x_y.bin"] {
            assert!(validate_key(ok).is_ok(), "key {ok:?} must be accepted");
        }
    }

    #[tokio::test]
    async fn memory_store_round_trips_and_deletes() {
        let s = MemoryStore::new();
        s.put("b", "k.txt", Bytes::from_static(b"hello"), "text/plain").await.unwrap();
        assert_eq!(s.get("b", "k.txt").await.unwrap(), Bytes::from_static(b"hello"));
        s.delete("b", "k.txt").await.unwrap();
        let err = s.get("b", "k.txt").await.unwrap_err();
        assert_eq!(err.code(), "JC0404", "missing blob reads as not_found");
        // presign is None: memory/local backends fall back to app-HMAC URLs.
        assert!(s.presign_get("b", "k.txt", std::time::Duration::from_secs(60)).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn local_store_round_trips_under_its_root() {
        let dir = tempfile::tempdir().unwrap();
        let s = LocalStore::new(dir.path());
        s.put("avatars", "u/1.png", Bytes::from_static(b"png"), "image/png").await.unwrap();
        assert!(dir.path().join("avatars/u/1.png").is_file(), "bytes land under root/bucket/key");
        assert_eq!(s.get("avatars", "u/1.png").await.unwrap(), Bytes::from_static(b"png"));
        s.delete("avatars", "u/1.png").await.unwrap();
        assert_eq!(s.get("avatars", "u/1.png").await.unwrap_err().code(), "JC0404");
        // Delete of a missing key is idempotent (the metadata row is the truth).
        s.delete("avatars", "u/1.png").await.unwrap();
    }

    #[tokio::test]
    async fn local_store_refuses_traversal_keys() {
        let dir = tempfile::tempdir().unwrap();
        let s = LocalStore::new(dir.path());
        let err = s
            .put("b", "../escape.txt", Bytes::from_static(b"x"), "text/plain")
            .await
            .unwrap_err();
        assert_eq!(err.code(), "JC0400", "traversal key is a bad request");
    }
}
```

- [ ] 3. Run: `cargo test -p jerrycan-storage` — expected FAIL: `validate_key`, `MemoryStore`, `LocalStore` unresolved (compile error).
- [ ] 4. Implement the rest of `store.rs` above the tests:

```rust
//! The blob-store layer: the object-safe [`BlobStore`] trait, key validation,
//! the zero-config [`LocalStore`] (filesystem), and the test/dev [`MemoryStore`].
//! Bytes only — metadata, scoping, and signing live in the service (lib.rs).

use bytes::Bytes;
use jerrycan_core::{Error, Result};
use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Mutex;
use std::time::Duration;

/// The boxed future blob operations return. Hand-boxed (not `async-trait`) so
/// the trait stays object-safe behind `Arc<dyn BlobStore>` — the same idiom as
/// `RateLimitStore::hit` and `jerrycan_jobs`'s `JobFuture`.
pub type BlobFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// A backend that stores raw object bytes keyed by `bucket/key`. The metadata
/// row in `storage_objects` is the source of truth for access; the store holds
/// only bytes.
pub trait BlobStore: Send + Sync + 'static {
    fn put<'a>(&'a self, bucket: &'a str, key: &'a str, body: Bytes, mime: &'a str) -> BlobFuture<'a, ()>;
    fn get<'a>(&'a self, bucket: &'a str, key: &'a str) -> BlobFuture<'a, Bytes>;
    /// Idempotent: deleting a missing key is Ok (the metadata row is the truth).
    fn delete<'a>(&'a self, bucket: &'a str, key: &'a str) -> BlobFuture<'a, ()>;
    /// A backend-native presigned GET URL, or `None` when the backend has no
    /// native signing (local/memory) — the caller falls back to app-HMAC.
    fn presign_get<'a>(&'a self, bucket: &'a str, key: &'a str, ttl: Duration) -> BlobFuture<'a, Option<String>>;
}

/// Validate an object key: 1..=1024 chars of `[A-Za-z0-9._/-]`, no leading or
/// trailing `/`, and no empty/`.`/`..` segments. Keys become filesystem paths
/// (LocalStore) and S3 object paths, so traversal must be impossible by
/// construction, and the charset keeps keys safe to interpolate into URLs.
pub(crate) fn validate_key(key: &str) -> Result<()> {
    let ok = !key.is_empty()
        && key.len() <= 1024
        && !key.starts_with('/')
        && !key.ends_with('/')
        && !key.split('/').any(|seg| seg.is_empty() || seg == "." || seg == "..")
        && key.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/'));
    if ok {
        Ok(())
    } else {
        Err(Error::bad_request(format!(
            "storage: invalid object key `{key}` — allowed [A-Za-z0-9._/-], no empty/./.. segments, no leading or trailing slash"
        )))
    }
}

/// The zero-config filesystem store: bytes under `root/bucket/key`. Dev and
/// single-node self-host. `presign_get` is `None` (app-HMAC fallback).
pub struct LocalStore {
    root: PathBuf,
}

impl LocalStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn path_for(&self, bucket: &str, key: &str) -> Result<PathBuf> {
        validate_key(key)?;
        Ok(self.root.join(bucket).join(key))
    }
}

impl BlobStore for LocalStore {
    fn put<'a>(&'a self, bucket: &'a str, key: &'a str, body: Bytes, _mime: &'a str) -> BlobFuture<'a, ()> {
        Box::pin(async move {
            let path = self.path_for(bucket, key)?;
            let parent = path.parent().expect("bucket/key path has a parent");
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| Error::internal(format!("storage: create {}: {e}", parent.display())))?;
            tokio::fs::write(&path, &body)
                .await
                .map_err(|e| Error::internal(format!("storage: write {}: {e}", path.display())))
        })
    }

    fn get<'a>(&'a self, bucket: &'a str, key: &'a str) -> BlobFuture<'a, Bytes> {
        Box::pin(async move {
            let path = self.path_for(bucket, key)?;
            match tokio::fs::read(&path).await {
                Ok(bytes) => Ok(Bytes::from(bytes)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Error::not_found()),
                Err(e) => Err(Error::internal(format!("storage: read {}: {e}", path.display()))),
            }
        })
    }

    fn delete<'a>(&'a self, bucket: &'a str, key: &'a str) -> BlobFuture<'a, ()> {
        Box::pin(async move {
            let path = self.path_for(bucket, key)?;
            match tokio::fs::remove_file(&path).await {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(Error::internal(format!("storage: delete {}: {e}", path.display()))),
            }
        })
    }

    fn presign_get<'a>(&'a self, _bucket: &'a str, _key: &'a str, _ttl: Duration) -> BlobFuture<'a, Option<String>> {
        Box::pin(async { Ok(None) })
    }
}

/// An in-process store for tests and ephemeral dev — the generated storage
/// acceptance tests run on it so they need no filesystem or network.
pub struct MemoryStore {
    map: Mutex<HashMap<(String, String), Bytes>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self { map: Mutex::new(HashMap::new()) }
    }
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

impl BlobStore for MemoryStore {
    fn put<'a>(&'a self, bucket: &'a str, key: &'a str, body: Bytes, _mime: &'a str) -> BlobFuture<'a, ()> {
        Box::pin(async move {
            validate_key(key)?;
            self.map
                .lock()
                .expect("storage memory store mutex poisoned")
                .insert((bucket.to_string(), key.to_string()), body);
            Ok(())
        })
    }

    fn get<'a>(&'a self, bucket: &'a str, key: &'a str) -> BlobFuture<'a, Bytes> {
        Box::pin(async move {
            self.map
                .lock()
                .expect("storage memory store mutex poisoned")
                .get(&(bucket.to_string(), key.to_string()))
                .cloned()
                .ok_or_else(Error::not_found)
        })
    }

    fn delete<'a>(&'a self, bucket: &'a str, key: &'a str) -> BlobFuture<'a, ()> {
        Box::pin(async move {
            self.map
                .lock()
                .expect("storage memory store mutex poisoned")
                .remove(&(bucket.to_string(), key.to_string()));
            Ok(())
        })
    }

    fn presign_get<'a>(&'a self, _bucket: &'a str, _key: &'a str, _ttl: Duration) -> BlobFuture<'a, Option<String>> {
        Box::pin(async { Ok(None) })
    }
}
```

- [ ] 5. Run: `cargo test -p jerrycan-storage` — expected PASS (4 tests). Also `cargo clippy -p jerrycan-storage --all-targets -- -D warnings`.
- [ ] 6. Commit: `Add jerrycan-storage crate: BlobStore trait, LocalStore, MemoryStore`

## Task 2 — App-HMAC signed-URL primitives (`sign.rs`)

**Files:**
- Create: `crates/jerrycan-storage/src/sign.rs`
- Modify: `crates/jerrycan-storage/src/lib.rs` (add `mod sign;`)
- Test: in-module

- [ ] 1. Write the failing tests (bottom of the new `sign.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &[u8] = b"a-very-long-development-secret-string!!";

    #[test]
    fn sign_then_verify_round_trips_and_expires() {
        // WHY: the signed URL is the ONLY credential a download carries — it
        // must verify before expiry and hard-fail after (no grace).
        let sig = sign(KEY, "avatars", "obj-1", 1_000);
        assert!(verify(KEY, "avatars", "obj-1", 1_000, &sig, 999), "valid before expiry");
        assert!(!verify(KEY, "avatars", "obj-1", 1_000, &sig, 1_000), "exp is exclusive");
        assert!(!verify(KEY, "avatars", "obj-1", 1_000, &sig, 2_000), "expired");
    }

    #[test]
    fn any_component_change_breaks_the_signature() {
        // WHY: the signature binds bucket + object id + expiry — reusing a sig
        // across buckets/objects or stretching the expiry must fail.
        let sig = sign(KEY, "avatars", "obj-1", 1_000);
        assert!(!verify(KEY, "invoices", "obj-1", 1_000, &sig, 1));
        assert!(!verify(KEY, "avatars", "obj-2", 1_000, &sig, 1));
        assert!(!verify(KEY, "avatars", "obj-1", 9_000, &sig, 1));
        assert!(!verify(b"other-key", "avatars", "obj-1", 1_000, &sig, 1));
        assert!(!verify(KEY, "avatars", "obj-1", 1_000, "zz-not-hex", 1), "junk sig is false, not a panic");
        let truncated = &sig[..sig.len() - 2];
        assert!(!verify(KEY, "avatars", "obj-1", 1_000, truncated, 1));
    }

    #[test]
    fn hex_round_trips() {
        assert_eq!(hex(&[0x00, 0xff, 0x10]), "00ff10");
        assert_eq!(unhex("00ff10").unwrap(), vec![0x00, 0xff, 0x10]);
        assert!(unhex("0g").is_none());
        assert!(unhex("0").is_none(), "odd length");
    }
}
```

- [ ] 2. Run: `cargo test -p jerrycan-storage sign` — expected FAIL: `sign`/`verify`/`hex`/`unhex` unresolved (compile error). (`mod sign;` must be added to lib.rs first or the file won't compile at all.)
- [ ] 3. Implement above the tests:

```rust
//! App-HMAC signed URLs (the universal default): HMAC-SHA256 over
//! `bucket|object_id|exp`, hex-encoded, verified constant-time. Works on every
//! backend (local included) and keeps the in-app guard + access log; the S3
//! native presign (sigv4.rs) is the opt-in alternative.

use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Lowercase hex. Shared by the checksum/ETag path (lib.rs) and signatures.
pub(crate) fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(char::from_digit((b >> 4) as u32, 16).expect("nibble < 16"));
        out.push(char::from_digit((b & 0x0f) as u32, 16).expect("nibble < 16"));
    }
    out
}

/// Strict lowercase/uppercase hex decode; `None` on odd length or a non-hex char.
pub(crate) fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(s.len() / 2);
    for pair in b.chunks_exact(2) {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out.push(((hi << 4) | lo) as u8);
    }
    Some(out)
}

fn mac_for(key: &[u8], bucket: &str, object_id: &str, exp_unix: u64) -> HmacSha256 {
    let mut mac = HmacSha256::new_from_slice(key).expect("hmac accepts any key length");
    mac.update(format!("{bucket}|{object_id}|{exp_unix}").as_bytes());
    mac
}

/// The hex signature for `/<bucket>/<object_id>?exp=<exp_unix>&sig=…`.
pub(crate) fn sign(key: &[u8], bucket: &str, object_id: &str, exp_unix: u64) -> String {
    hex(&mac_for(key, bucket, object_id, exp_unix).finalize().into_bytes())
}

/// Verify a presented signature: unexpired (`now < exp`) and a constant-time
/// MAC match (`verify_slice` — never `==` on the hex strings).
pub(crate) fn verify(
    key: &[u8],
    bucket: &str,
    object_id: &str,
    exp_unix: u64,
    sig_hex: &str,
    now_unix: u64,
) -> bool {
    if now_unix >= exp_unix {
        return false;
    }
    let Some(sig) = unhex(sig_hex) else {
        return false;
    };
    mac_for(key, bucket, object_id, exp_unix).verify_slice(&sig).is_ok()
}
```

- [ ] 4. Run: `cargo test -p jerrycan-storage sign` — expected PASS (3 tests).
- [ ] 5. Commit: `Add app-HMAC signed-URL primitives to jerrycan-storage`

## Task 3 — Bucket rules: mime allowlist globs + the `Bucket`/`Scope` types

**Files:**
- Modify: `crates/jerrycan-storage/src/lib.rs`
- Test: in-module

- [ ] 1. Write the failing tests (new `#[cfg(test)] mod tests` in lib.rs):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn bucket(allowed: &'static [&'static str]) -> Bucket {
        Bucket { name: "b", public: false, owner_prefix: false, max_size: 1024, allowed_mime: allowed }
    }

    #[test]
    fn empty_allowlist_allows_everything() {
        assert!(bucket(&[]).allows_mime("application/octet-stream"));
    }

    #[test]
    fn globs_match_prefix_exact_matches_whole_and_params_are_stripped() {
        // WHY: `allowed_mime: ["image/*"]` is the Supabase-parity contract —
        // image/png passes, text/plain 415s, and `; charset=` noise never
        // defeats the check.
        let b = bucket(&["image/*", "application/pdf"]);
        assert!(b.allows_mime("image/png"));
        assert!(b.allows_mime("IMAGE/JPEG"), "case-insensitive");
        assert!(b.allows_mime("application/pdf"));
        assert!(b.allows_mime("application/pdf; charset=binary"), "parameters stripped");
        assert!(!b.allows_mime("text/plain"));
        assert!(!b.allows_mime("imagex/png"), "prefix must be a whole type segment");
        assert!(!b.allows_mime("application/pdfx"), "exact match is exact");
        assert!(bucket(&["*/*"]).allows_mime("anything/at-all"));
    }
}
```

- [ ] 2. Run: `cargo test -p jerrycan-storage --lib allows_mime` — expected FAIL: `Bucket` unresolved.
- [ ] 3. Implement in `lib.rs` (above the tests; `mod meta;` etc. arrive in later tasks):

```rust
/// One bucket's generated, compile-time rules. The generator emits a
/// `const BUCKET: Bucket` per bucket module — everything here is design-derived.
#[derive(Clone, Copy, Debug)]
pub struct Bucket {
    pub name: &'static str,
    /// `visibility: public` — unauthenticated GET list/download.
    pub public: bool,
    /// Keys stored as `{owner_id}/…`; every access asserts the prefix.
    pub owner_prefix: bool,
    /// Per-object byte cap (also the generated route's `.body_limit`).
    pub max_size: usize,
    /// Content-type allowlist (`image/*` globs, exact types). Empty = allow all.
    pub allowed_mime: &'static [&'static str],
}

impl Bucket {
    /// Is `mime` (a request Content-Type; parameters stripped, case-insensitive)
    /// allowed by this bucket? Violation is the caller's `415 JC0415`.
    pub fn allows_mime(&self, mime: &str) -> bool {
        if self.allowed_mime.is_empty() {
            return true;
        }
        let mime = mime.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
        self.allowed_mime.iter().any(|pat| {
            let pat = pat.to_ascii_lowercase();
            if pat == "*/*" {
                return true;
            }
            match pat.strip_suffix("/*") {
                Some(prefix) => mime.split('/').next() == Some(prefix),
                None => mime == pat,
            }
        })
    }
}

/// The caller's resolved scope — stamped on writes, filtered on reads. Both ids
/// are STRINGIFIED pks (see the plan's spec resolution #3): the user id for
/// `owner: User`-style buckets, the Tenant guard's tenant id for tenant-owned
/// buckets. `None` = unscoped (public reads, ownerless buckets).
#[derive(Clone, Debug, Default)]
pub struct Scope {
    pub owner_id: Option<String>,
    pub tenant_id: Option<String>,
}
```

- [ ] 4. Run: `cargo test -p jerrycan-storage --lib` — expected PASS.
- [ ] 5. Commit: `Add bucket rules: mime allowlist globs and the Scope type`

## Task 4 — `storage_objects` metadata: `STORAGE_MIGRATIONS` + scoped SQL (`meta.rs`)

**Files:**
- Create: `crates/jerrycan-storage/src/meta.rs`
- Modify: `crates/jerrycan-storage/src/lib.rs` (`pub mod meta;` + re-exports)
- Test: in-module (sqlite::memory: via jerrycan-db, same idiom as jerrycan-jobs' PostgresStore tests)

- [ ] 1. Write the failing tests (bottom of the new `meta.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::Scope;
    use jerrycan_db::Db;

    async fn db() -> Db {
        let db = Db::connect("sqlite::memory:").await.expect("test db");
        db.migrate(STORAGE_MIGRATIONS).await.expect("storage migrations");
        db
    }

    fn meta(id: &str, key: &str, owner: Option<&str>, tenant: Option<&str>) -> ObjectMeta {
        ObjectMeta {
            id: id.to_string(),
            bucket: "b".to_string(),
            key: key.to_string(),
            owner_id: owner.map(String::from),
            tenant_id: tenant.map(String::from),
            size: 3,
            mime: "text/plain".to_string(),
            checksum: "abc123".to_string(),
            created_at: 1_000,
        }
    }

    fn scope(owner: Option<&str>, tenant: Option<&str>) -> Scope {
        Scope { owner_id: owner.map(String::from), tenant_id: tenant.map(String::from) }
    }

    #[tokio::test]
    async fn insert_get_list_delete_round_trip() {
        let db = db().await;
        insert(&db, &meta("id-1", "a.txt", Some("1"), None)).await.unwrap();
        let got = get_scoped(&db, "b", "id-1", &Scope::default()).await.unwrap().unwrap();
        assert_eq!((got.key.as_str(), got.size, got.checksum.as_str()), ("a.txt", 3, "abc123"));
        assert_eq!(list_scoped(&db, "b", &Scope::default()).await.unwrap().len(), 1);
        delete_row(&db, "b", "id-1").await.unwrap();
        assert!(get_scoped(&db, "b", "id-1", &Scope::default()).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn duplicate_bucket_key_is_409() {
        // WHY: unique(bucket, key) is the Supabase-parity contract — a re-upload
        // to the same key must be a client 409, not a silent overwrite or a 500.
        let db = db().await;
        insert(&db, &meta("id-1", "same.txt", None, None)).await.unwrap();
        let err = insert(&db, &meta("id-2", "same.txt", None, None)).await.unwrap_err();
        assert_eq!(err.code(), "JC0409");
    }

    #[tokio::test]
    async fn owner_and_tenant_filters_scope_reads_and_lists() {
        // WHY (Rule 9): this IS the isolation mechanism — a scoped read of a
        // foreign row must come back None (the handler's 404), and a scoped
        // list must only contain the caller's rows.
        let db = db().await;
        insert(&db, &meta("o1", "a.txt", Some("1"), Some("10"))).await.unwrap();
        insert(&db, &meta("o2", "b.txt", Some("2"), Some("20"))).await.unwrap();
        // Cross-owner get: None. Same-owner get: Some.
        assert!(get_scoped(&db, "b", "o1", &scope(Some("2"), None)).await.unwrap().is_none());
        assert!(get_scoped(&db, "b", "o1", &scope(Some("1"), None)).await.unwrap().is_some());
        // Cross-tenant get: None even with the right owner filter absent.
        assert!(get_scoped(&db, "b", "o1", &scope(None, Some("20"))).await.unwrap().is_none());
        // Scoped list sees only the caller's row.
        let mine = list_scoped(&db, "b", &scope(Some("1"), Some("10"))).await.unwrap();
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].id, "o1");
        // Unscoped (public) list sees both, ordered by key.
        let all = list_scoped(&db, "b", &Scope::default()).await.unwrap();
        assert_eq!(all.iter().map(|m| m.key.as_str()).collect::<Vec<_>>(), vec!["a.txt", "b.txt"]);
    }

    #[test]
    fn object_ids_are_uuid_v4_shaped_and_unique() {
        let a = new_object_id();
        let b = new_object_id();
        assert_ne!(a, b);
        assert_eq!(a.len(), 36);
        assert_eq!(a.as_bytes()[14], b'4', "version nibble is 4");
    }
}
```

- [ ] 2. Run: `cargo test -p jerrycan-storage meta` — expected FAIL: module items unresolved.
- [ ] 3. Implement `meta.rs`:

```rust
//! Object metadata in `jerrycan-db`: the framework-owned `storage_objects`
//! migration and the scoped SQL layer. The metadata row is the SOURCE OF TRUTH
//! for access — listing, ownership, tenant isolation, and owner_prefix checks
//! all run here; the blob store holds only bytes. owner_id/tenant_id are TEXT
//! (stringified pks) so one DDL shape covers i64/string/uuid owners on both
//! dialects (mirrors jerrycan-jobs' one-shape JOBS_MIGRATIONS).

use crate::Scope;
use jerrycan_core::{Error, Result};
use jerrycan_db::sea_orm::{ConnectionTrait, QueryResult, Statement, Value};
use jerrycan_db::{Db, Migration, db_error};
use serde::{Deserialize, Serialize};

/// The framework migration for the object-metadata table. `key` is quoted in
/// every statement (non-reserved but keyword-adjacent on both dialects).
pub const STORAGE_MIGRATIONS: &[Migration] = &[Migration {
    name: "jerrycan_storage_0001_create",
    sqlite: "\
CREATE TABLE storage_objects (
    id         TEXT PRIMARY KEY,
    bucket     TEXT NOT NULL,
    \"key\"      TEXT NOT NULL,
    owner_id   TEXT,
    tenant_id  TEXT,
    size       BIGINT NOT NULL,
    mime       TEXT NOT NULL,
    checksum   TEXT NOT NULL,
    created_at BIGINT NOT NULL
);
CREATE UNIQUE INDEX storage_objects_bucket_key ON storage_objects (bucket, \"key\");
CREATE INDEX storage_objects_scope ON storage_objects (bucket, owner_id);",
    postgres: "\
CREATE TABLE storage_objects (
    id         TEXT PRIMARY KEY,
    bucket     TEXT NOT NULL,
    \"key\"      TEXT NOT NULL,
    owner_id   TEXT,
    tenant_id  TEXT,
    size       BIGINT NOT NULL,
    mime       TEXT NOT NULL,
    checksum   TEXT NOT NULL,
    created_at BIGINT NOT NULL
);
CREATE UNIQUE INDEX storage_objects_bucket_key ON storage_objects (bucket, \"key\");
CREATE INDEX storage_objects_scope ON storage_objects (bucket, owner_id);",
}];

/// One object's metadata row. Serialized as the upload/list/download JSON body.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ObjectMeta {
    pub id: String,
    pub bucket: String,
    pub key: String,
    pub owner_id: Option<String>,
    pub tenant_id: Option<String>,
    pub size: i64,
    pub mime: String,
    /// sha256 hex of the bytes — doubles as the ETag.
    pub checksum: String,
    /// Epoch millis.
    pub created_at: i64,
}

/// A fresh UUIDv4 string from OS randomness (no uuid crate: 16 CSPRNG bytes,
/// version/variant bits set, canonical hyphenation).
pub(crate) fn new_object_id() -> String {
    use rand::RngCore;
    let mut b = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = crate::sign::hex(&b);
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

fn stmt(db: &Db, sql: &str, values: Vec<Value>) -> Statement {
    Statement::from_sql_and_values(db.conn().get_database_backend(), db.sql(sql), values)
}

/// `AND owner_id = ? AND tenant_id = ?` fragments for whichever scope ids are
/// set. An empty scope adds nothing (public/unscoped access).
fn scope_sql(scope: &Scope, sql: &mut String, values: &mut Vec<Value>) {
    if let Some(owner) = &scope.owner_id {
        sql.push_str(" AND owner_id = ?");
        values.push(owner.clone().into());
    }
    if let Some(tenant) = &scope.tenant_id {
        sql.push_str(" AND tenant_id = ?");
        values.push(tenant.clone().into());
    }
}

fn row_to_meta(row: &QueryResult) -> Result<ObjectMeta> {
    let col_err = |c: &str, e: jerrycan_db::sea_orm::DbErr| {
        Error::internal(format!("jerrycan-storage: column `{c}`: {e}"))
    };
    Ok(ObjectMeta {
        id: row.try_get("", "id").map_err(|e| col_err("id", e))?,
        bucket: row.try_get("", "bucket").map_err(|e| col_err("bucket", e))?,
        key: row.try_get("", "key").map_err(|e| col_err("key", e))?,
        owner_id: row.try_get("", "owner_id").map_err(|e| col_err("owner_id", e))?,
        tenant_id: row.try_get("", "tenant_id").map_err(|e| col_err("tenant_id", e))?,
        size: row.try_get("", "size").map_err(|e| col_err("size", e))?,
        mime: row.try_get("", "mime").map_err(|e| col_err("mime", e))?,
        checksum: row.try_get("", "checksum").map_err(|e| col_err("checksum", e))?,
        created_at: row.try_get("", "created_at").map_err(|e| col_err("created_at", e))?,
    })
}

const COLS: &str = "id, bucket, \"key\", owner_id, tenant_id, size, mime, checksum, created_at";

/// Insert one row. A `(bucket, key)` unique violation maps to 409 via db_error.
pub(crate) async fn insert(db: &Db, m: &ObjectMeta) -> Result<()> {
    db.conn()
        .execute(stmt(
            db,
            "INSERT INTO storage_objects (id, bucket, \"key\", owner_id, tenant_id, size, mime, checksum, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            vec![
                m.id.clone().into(),
                m.bucket.clone().into(),
                m.key.clone().into(),
                m.owner_id.clone().into(),
                m.tenant_id.clone().into(),
                m.size.into(),
                m.mime.clone().into(),
                m.checksum.clone().into(),
                m.created_at.into(),
            ],
        ))
        .await
        .map_err(db_error)?;
    Ok(())
}

/// One object by id within a bucket, filtered by whatever the scope sets —
/// a scoped read of a foreign row is None (the caller's 404).
pub(crate) async fn get_scoped(db: &Db, bucket: &str, id: &str, scope: &Scope) -> Result<Option<ObjectMeta>> {
    let mut sql = format!("SELECT {COLS} FROM storage_objects WHERE bucket = ? AND id = ?");
    let mut values: Vec<Value> = vec![bucket.into(), id.into()];
    scope_sql(scope, &mut sql, &mut values);
    let row = db.conn().query_one(stmt(db, &sql, values)).await.map_err(db_error)?;
    row.as_ref().map(row_to_meta).transpose()
}

/// A bucket's objects under the scope, ordered by key (stable listings).
pub(crate) async fn list_scoped(db: &Db, bucket: &str, scope: &Scope) -> Result<Vec<ObjectMeta>> {
    let mut sql = format!("SELECT {COLS} FROM storage_objects WHERE bucket = ?");
    let mut values: Vec<Value> = vec![bucket.into()];
    scope_sql(scope, &mut sql, &mut values);
    sql.push_str(" ORDER BY \"key\"");
    let rows = db.conn().query_all(stmt(db, &sql, values)).await.map_err(db_error)?;
    rows.iter().map(row_to_meta).collect()
}

/// Remove one row (scope already proven by the caller's get_scoped).
pub(crate) async fn delete_row(db: &Db, bucket: &str, id: &str) -> Result<()> {
    db.conn()
        .execute(stmt(
            db,
            "DELETE FROM storage_objects WHERE bucket = ? AND id = ?",
            vec![bucket.into(), id.into()],
        ))
        .await
        .map_err(db_error)?;
    Ok(())
}
```

In `lib.rs` add `pub mod meta;` and `pub use meta::{ObjectMeta, STORAGE_MIGRATIONS};`.

- [ ] 4. Run: `cargo test -p jerrycan-storage meta` — expected PASS (5 tests).
- [ ] 5. Commit: `Add storage_objects metadata store and STORAGE_MIGRATIONS`

## Task 5 — The `Storage` service: env config, object CRUD, scope enforcement, `object_response`

**Files:**
- Modify: `crates/jerrycan-storage/src/lib.rs`
- Test: in-module

- [ ] 1. Write the failing tests (extend lib.rs's `mod tests`):

```rust
    use crate::meta::STORAGE_MIGRATIONS;
    use bytes::Bytes;
    use jerrycan_db::Db;

    const SECRET: &str = "a-very-long-development-secret-string!!";

    async fn db() -> Db {
        let db = Db::connect("sqlite::memory:").await.expect("test db");
        db.migrate(STORAGE_MIGRATIONS).await.expect("migrations");
        db
    }

    const AVATARS: Bucket = Bucket {
        name: "avatars", public: true, owner_prefix: false, max_size: 16, allowed_mime: &["image/*"],
    };
    const INVOICES: Bucket = Bucket {
        name: "invoices", public: false, owner_prefix: true, max_size: 1024, allowed_mime: &[],
    };

    fn owner(id: &str) -> Scope {
        Scope { owner_id: Some(id.to_string()), tenant_id: None }
    }

    #[test]
    fn from_config_parses_local_rejects_junk_and_gates_s3() {
        // WHY: JERRYCAN_STORAGE is the zero-touch backend switch — a typo must
        // fail loud at startup, and s3:// without the compiled backend must
        // point at the missing feature, never silently fall back to local.
        assert!(Storage::from_config("local:/var/data", Some(SECRET)).is_ok());
        let err = Storage::from_config("gcs://nope", Some(SECRET)).unwrap_err();
        assert!(err.message().contains("JERRYCAN_STORAGE"), "{err}");
        #[cfg(not(feature = "storage-s3"))]
        {
            let err = Storage::from_config("s3://bucket?region=us-east-1", Some(SECRET)).unwrap_err();
            assert!(err.message().contains("storage-s3"), "{err}");
        }
    }

    #[tokio::test]
    async fn put_enforces_mime_and_size_and_stamps_metadata() {
        let db = db().await;
        let s = Storage::memory().with_sign_secret(SECRET);
        // Disallowed mime → 415 JC0415.
        let err = s.put_object(&db, &AVATARS, &owner("1"), "a.txt", "text/plain", Bytes::from_static(b"x")).await.unwrap_err();
        assert_eq!(err.code(), "JC0415");
        // Oversize → 413 JC0413 (max_size 16).
        let err = s.put_object(&db, &AVATARS, &owner("1"), "big.png", "image/png", Bytes::from(vec![0u8; 17])).await.unwrap_err();
        assert_eq!(err.code(), "JC0413");
        // Happy path: checksum is the sha256 hex; owner is stamped.
        let meta = s.put_object(&db, &AVATARS, &owner("1"), "a.png", "image/png", Bytes::from_static(b"png-bytes")).await.unwrap();
        assert_eq!(meta.owner_id.as_deref(), Some("1"));
        assert_eq!(meta.size, 9);
        assert_eq!(meta.checksum, sign::hex(&<sha2::Sha256 as sha2::Digest>::digest(b"png-bytes")));
        // Round trip through the store.
        let (got, bytes) = s.get_object(&db, &AVATARS, None, &meta.id).await.unwrap();
        assert_eq!(bytes, Bytes::from_static(b"png-bytes"));
        assert_eq!(got.id, meta.id);
    }

    #[tokio::test]
    async fn owner_prefix_prepends_and_scopes_cross_prefix_reads_to_404() {
        // WHY: owner_prefix is the Supabase folder-per-user pattern made
        // mechanical — B must never read/delete under A's prefix.
        let db = db().await;
        let s = Storage::memory().with_sign_secret(SECRET);
        let meta = s.put_object(&db, &INVOICES, &owner("1"), "inv.pdf", "application/pdf", Bytes::from_static(b"pdf")).await.unwrap();
        assert_eq!(meta.key, "1/inv.pdf", "key is stored under the owner prefix");
        // Same relative key from owner 2: a distinct object, no collision.
        let meta2 = s.put_object(&db, &INVOICES, &owner("2"), "inv.pdf", "application/pdf", Bytes::from_static(b"pdf")).await.unwrap();
        assert_eq!(meta2.key, "2/inv.pdf");
        // Cross-owner read/delete: 404, and the row survives.
        assert_eq!(s.get_object(&db, &INVOICES, Some(&owner("2")), &meta.id).await.unwrap_err().code(), "JC0404");
        assert_eq!(s.delete_object(&db, &INVOICES, &owner("2"), &meta.id).await.unwrap_err().code(), "JC0404");
        assert!(s.get_object(&db, &INVOICES, Some(&owner("1")), &meta.id).await.is_ok(), "owner still reads their object");
        // Scoped list: owner 1 sees only their object.
        let mine = s.list_objects(&db, &INVOICES, Some(&owner("1"))).await.unwrap();
        assert_eq!(mine.iter().map(|m| m.key.as_str()).collect::<Vec<_>>(), vec!["1/inv.pdf"]);
    }

    #[tokio::test]
    async fn delete_removes_row_and_bytes() {
        let db = db().await;
        let s = Storage::memory().with_sign_secret(SECRET);
        let meta = s.put_object(&db, &INVOICES, &owner("1"), "x.bin", "application/octet-stream", Bytes::from_static(b"x")).await.unwrap();
        s.delete_object(&db, &INVOICES, &owner("1"), &meta.id).await.unwrap();
        assert_eq!(s.get_object(&db, &INVOICES, Some(&owner("1")), &meta.id).await.unwrap_err().code(), "JC0404");
    }

    #[test]
    fn object_response_carries_etag_mime_and_cache_control() {
        // WHY: public GETs must be cache-friendly (spec: ETag + Cache-Control);
        // private responses must never be cached by shared caches.
        let meta = ObjectMeta {
            id: "i".into(), bucket: "b".into(), key: "k".into(), owner_id: None, tenant_id: None,
            size: 1, mime: "image/png".into(), checksum: "deadbeef".into(), created_at: 0,
        };
        let public = object_response(&meta, Bytes::from_static(b"x"), true).unwrap();
        assert_eq!(public.headers().get("etag").unwrap(), "\"deadbeef\"");
        assert_eq!(public.headers().get("content-type").unwrap(), "image/png");
        assert_eq!(public.headers().get("cache-control").unwrap(), "public, max-age=3600");
        let private = object_response(&meta, Bytes::from_static(b"x"), false).unwrap();
        assert_eq!(private.headers().get("cache-control").unwrap(), "private, no-store");
    }

    #[tokio::test]
    async fn storage_registers_as_an_extension_dep() {
        // WHY: generated handlers take `Dep<Storage>` — the Extension impl must
        // provide the service app-wide (same shape as Db).
        use jerrycan_core::{App, Dep, Json, get};
        async fn probe(s: Dep<Storage>) -> jerrycan_core::Result<Json<bool>> {
            Ok(Json(s.sign_key.is_some()))
        }
        let t = App::new()
            .extend(Storage::memory().with_sign_secret(SECRET))
            .route("/probe", get(probe))
            .into_test();
        let res = t.get("/probe").await;
        assert_eq!(res.status().as_u16(), 200);
        assert_eq!(res.text(), "true");
    }
```

(If `App::route` does not exist at the app level, mount a one-route `Module` instead — mirror whatever `jerrycan-core`'s own extension tests do.)

- [ ] 2. Run: `cargo test -p jerrycan-storage --lib` — expected FAIL: `Storage`, `object_response` unresolved.
- [ ] 3. Implement in `lib.rs`:

```rust
use bytes::Bytes;
use jerrycan_core::{App, Error, Extension, Result};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub mod meta;
mod sign;
pub mod store;
#[cfg(feature = "storage-s3")]
mod s3_store;
#[cfg(feature = "storage-s3")]
mod sigv4;
#[cfg(feature = "storage-s3")]
mod xml;

pub use meta::{ObjectMeta, STORAGE_MIGRATIONS};
pub use store::{BlobFuture, BlobStore, LocalStore, MemoryStore};
#[cfg(feature = "storage-s3")]
pub use s3_store::S3Store;

/// A time-limited signed URL: native S3 presign when the backend supports it,
/// else the app-HMAC path `/<bucket>/<id>?exp=…&sig=…`.
#[derive(Clone, Debug, serde::Serialize)]
pub struct SignedUrl {
    pub url: String,
    /// Unix seconds.
    pub expires_at: u64,
}

/// The storage service: one blob backend + the app-HMAC signing key. Registered
/// app-wide via `.extend(...)` (it provides itself, like `Db`); generated
/// handlers take `Dep<Storage>`.
#[derive(Clone)]
pub struct Storage {
    store: Arc<dyn BlobStore>,
    sign_key: Option<Arc<Vec<u8>>>,
}

impl Storage {
    /// Filesystem-backed (the zero-config default).
    pub fn local(root: impl Into<std::path::PathBuf>) -> Self {
        Self::with_store(Arc::new(LocalStore::new(root)))
    }

    /// In-process store — generated acceptance tests and ephemeral dev.
    pub fn memory() -> Self {
        Self::with_store(Arc::new(MemoryStore::new()))
    }

    /// Any custom backend.
    pub fn with_store(store: Arc<dyn BlobStore>) -> Self {
        Self { store, sign_key: None }
    }

    /// Key the app-HMAC signed URLs (normally `JERRYCAN_SECRET` via from_env).
    pub fn with_sign_secret(mut self, secret: &str) -> Self {
        self.sign_key = Some(Arc::new(secret.as_bytes().to_vec()));
        self
    }

    /// `JERRYCAN_STORAGE` (default `local:./storage`) + `JERRYCAN_SECRET`.
    /// Grammar: `local:<root>` | `s3://bucket?region=<r>&endpoint=<url>`
    /// (s3 credentials from AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY).
    pub fn from_env() -> Result<Self> {
        let url = std::env::var("JERRYCAN_STORAGE").unwrap_or_else(|_| "local:./storage".to_string());
        Self::from_config(&url, std::env::var("JERRYCAN_SECRET").ok().as_deref())
    }

    /// The pure core of [`from_env`] (testable without touching the process env).
    pub fn from_config(url: &str, secret: Option<&str>) -> Result<Self> {
        let storage = if let Some(root) = url.strip_prefix("local:") {
            Self::local(root)
        } else if url.starts_with("s3://") {
            #[cfg(feature = "storage-s3")]
            {
                Self::with_store(Arc::new(s3_store::S3Store::from_url(url)?))
            }
            #[cfg(not(feature = "storage-s3"))]
            {
                return Err(Error::internal(
                    "JERRYCAN_STORAGE is s3://… but the S3 backend is not compiled in — enable the `storage-s3` facade feature",
                ));
            }
        } else {
            return Err(Error::internal(format!(
                "JERRYCAN_STORAGE: unrecognized value `{url}` — use local:<root> or s3://bucket?region=…&endpoint=…"
            )));
        };
        Ok(match secret {
            Some(secret) => storage.with_sign_secret(secret),
            None => storage,
        })
    }

    fn sign_key(&self) -> Result<&[u8]> {
        self.sign_key
            .as_deref()
            .map(|v| v.as_slice())
            .ok_or_else(|| Error::internal("storage: JERRYCAN_SECRET is required for signed URLs"))
    }

    /// Upload: validate key/size/mime, prepend the owner prefix, reserve the
    /// metadata row (unique(bucket,key) → 409), then write bytes. A failed blob
    /// write compensates by removing the reserved row — no orphan metadata.
    pub async fn put_object(
        &self,
        db: &jerrycan_db::Db,
        bucket: &Bucket,
        scope: &Scope,
        key: &str,
        mime: &str,
        body: Bytes,
    ) -> Result<ObjectMeta> {
        store::validate_key(key)?;
        if body.len() > bucket.max_size {
            return Err(Error::payload_too_large());
        }
        let mime = mime.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
        let mime = if mime.is_empty() { "application/octet-stream".to_string() } else { mime };
        if !bucket.allows_mime(&mime) {
            return Err(Error::unsupported_media_type());
        }
        let key = if bucket.owner_prefix {
            let owner = scope.owner_id.as_deref().ok_or_else(|| {
                Error::internal("storage: owner_prefix bucket used without an owner scope")
            })?;
            format!("{owner}/{key}")
        } else {
            key.to_string()
        };
        use sha2::Digest;
        let m = ObjectMeta {
            id: meta::new_object_id(),
            bucket: bucket.name.to_string(),
            key,
            owner_id: scope.owner_id.clone(),
            tenant_id: scope.tenant_id.clone(),
            size: body.len() as i64,
            mime: mime.clone(),
            checksum: sign::hex(&sha2::Sha256::digest(&body)),
            created_at: now_millis(),
        };
        meta::insert(db, &m).await?;
        if let Err(e) = self.store.put(bucket.name, &m.key, body, &m.mime).await {
            // Compensate: the reservation must not outlive a failed byte write.
            let _ = meta::delete_row(db, &m.bucket, &m.id).await;
            return Err(e);
        }
        Ok(m)
    }

    /// Download: scoped metadata lookup (foreign row = 404), owner_prefix
    /// belt-and-braces assertion, then bytes. `scope: None` = public read.
    pub async fn get_object(
        &self,
        db: &jerrycan_db::Db,
        bucket: &Bucket,
        scope: Option<&Scope>,
        id: &str,
    ) -> Result<(ObjectMeta, Bytes)> {
        let unscoped = Scope::default();
        let m = meta::get_scoped(db, bucket.name, id, scope.unwrap_or(&unscoped))
            .await?
            .ok_or_else(Error::not_found)?;
        assert_prefix(bucket, scope, &m)?;
        let bytes = self.store.get(bucket.name, &m.key).await?;
        Ok((m, bytes))
    }

    /// List: scoped (owner/tenant) or open (public bucket), ordered by key.
    pub async fn list_objects(
        &self,
        db: &jerrycan_db::Db,
        bucket: &Bucket,
        scope: Option<&Scope>,
    ) -> Result<Vec<ObjectMeta>> {
        let unscoped = Scope::default();
        meta::list_scoped(db, bucket.name, scope.unwrap_or(&unscoped)).await
    }

    /// Delete: scoped lookup proves ownership (foreign row = 404), then row +
    /// bytes go together. Blob delete is idempotent.
    pub async fn delete_object(
        &self,
        db: &jerrycan_db::Db,
        bucket: &Bucket,
        scope: &Scope,
        id: &str,
    ) -> Result<()> {
        let m = meta::get_scoped(db, bucket.name, id, scope)
            .await?
            .ok_or_else(Error::not_found)?;
        assert_prefix(bucket, Some(scope), &m)?;
        meta::delete_row(db, bucket.name, id).await?;
        self.store.delete(bucket.name, &m.key).await
    }
}

/// The owner_prefix path assertion (spec: "adds a path-prefix assertion to
/// every access check"): the first key segment must equal the caller's owner
/// id. The DB owner filter already isolates rows; this keeps a mis-stamped row
/// from ever crossing owners.
fn assert_prefix(bucket: &Bucket, scope: Option<&Scope>, m: &ObjectMeta) -> Result<()> {
    if !bucket.owner_prefix {
        return Ok(());
    }
    let Some(owner) = scope.and_then(|s| s.owner_id.as_deref()) else {
        return Ok(()); // public/unscoped read of a public prefix bucket
    };
    if m.key.split('/').next() == Some(owner) {
        Ok(())
    } else {
        Err(Error::not_found())
    }
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Build the download response: Content-Type from the metadata, `ETag` = the
/// sha256 checksum (quoted), and a Cache-Control that is cache-friendly for
/// public buckets and cache-hostile for private ones.
pub fn object_response(meta: &ObjectMeta, bytes: Bytes, public: bool) -> Result<jerrycan_core::Response> {
    let cache = if public { "public, max-age=3600" } else { "private, no-store" };
    http::Response::builder()
        .status(200)
        .header("content-type", &meta.mime)
        .header("etag", format!("\"{}\"", meta.checksum))
        .header("cache-control", cache)
        .body(jerrycan_core::JcBody::full(bytes))
        .map_err(|e| Error::internal(format!("storage: building object response: {e}")))
}

impl Extension for Storage {
    fn register(self, app: App) -> App {
        app.provide(self)
    }
}
```

- [ ] 4. Run: `cargo test -p jerrycan-storage` — expected PASS. Also `cargo clippy -p jerrycan-storage --all-targets -- -D warnings`.
- [ ] 5. Commit: `Add Storage service: env-config backends, object CRUD, scope enforcement`

## Task 6 — Signed-URL ops on the service (`sign_object` / `get_signed`)

**Files:**
- Modify: `crates/jerrycan-storage/src/lib.rs`
- Test: in-module

- [ ] 1. Write the failing tests (extend lib.rs `mod tests`):

```rust
    #[tokio::test]
    async fn sign_object_falls_back_to_app_hmac_and_get_signed_verifies() {
        // WHY: local/memory backends have no native presign — sign must fall
        // back to the app-HMAC URL, and get_signed must honor it WITHOUT any
        // session (that is the whole point of a signed URL).
        let db = db().await;
        let s = Storage::memory().with_sign_secret(SECRET);
        let meta = s.put_object(&db, &INVOICES, &owner("1"), "inv.pdf", "application/pdf", Bytes::from_static(b"pdf")).await.unwrap();
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let signed = s.sign_object(&db, &INVOICES, &owner("1"), &meta.id, 300, now).await.unwrap();
        assert_eq!(signed.expires_at, 1_300);
        assert!(signed.url.starts_with("/invoices/"), "app-HMAC fallback URL: {}", signed.url);
        // Parse exp/sig back out of the URL and redeem it.
        let query = signed.url.split_once('?').unwrap().1;
        let mut exp = 0u64;
        let mut sig = String::new();
        for pair in query.split('&') {
            match pair.split_once('=').unwrap() {
                ("exp", v) => exp = v.parse().unwrap(),
                ("sig", v) => sig = v.to_string(),
                _ => {}
            }
        }
        let (got, bytes) = s.get_signed(&db, &INVOICES, &meta.id, exp, &sig, now).await.unwrap();
        assert_eq!(got.id, meta.id);
        assert_eq!(bytes, Bytes::from_static(b"pdf"));
        // Tampered sig and expired URL are 401 — an invalid credential.
        assert_eq!(s.get_signed(&db, &INVOICES, &meta.id, exp, "00aa", now).await.unwrap_err().code(), "JC0401");
        let later = now + Duration::from_secs(9_999);
        assert_eq!(s.get_signed(&db, &INVOICES, &meta.id, exp, &sig, later).await.unwrap_err().code(), "JC0401");
    }

    #[tokio::test]
    async fn sign_object_is_scoped_and_clamps_ttl() {
        // WHY: signing is an access grant — a caller must not be able to mint
        // a URL for a foreign object, nor stretch the TTL beyond the cap.
        let db = db().await;
        let s = Storage::memory().with_sign_secret(SECRET);
        let meta = s.put_object(&db, &INVOICES, &owner("1"), "inv.pdf", "application/pdf", Bytes::from_static(b"p")).await.unwrap();
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let err = s.sign_object(&db, &INVOICES, &owner("2"), &meta.id, 300, now).await.unwrap_err();
        assert_eq!(err.code(), "JC0404", "cross-owner sign is a 404");
        let capped = s.sign_object(&db, &INVOICES, &owner("1"), &meta.id, 999_999, now).await.unwrap();
        assert_eq!(capped.expires_at, 1_000 + 86_400, "TTL clamps to 24h");
    }

    #[tokio::test]
    async fn sign_without_secret_fails_loud() {
        let db = db().await;
        let s = Storage::memory(); // no sign secret
        let meta = s.put_object(&db, &INVOICES, &owner("1"), "a.bin", "application/octet-stream", Bytes::from_static(b"x")).await.unwrap();
        let now = SystemTime::now();
        let err = s.sign_object(&db, &INVOICES, &owner("1"), &meta.id, 300, now).await.unwrap_err();
        assert!(err.message().contains("JERRYCAN_SECRET"), "{err}");
    }
```

- [ ] 2. Run: `cargo test -p jerrycan-storage sign_object` — expected FAIL: methods unresolved.
- [ ] 3. Implement (inside `impl Storage`):

```rust
    /// Issue a time-limited download URL for an object the caller can reach
    /// (scoped lookup — a foreign object is a 404, so signing can't leak).
    /// Native backend presign when available (S3), else app-HMAC. TTL clamps
    /// to [1s, 24h].
    pub async fn sign_object(
        &self,
        db: &jerrycan_db::Db,
        bucket: &Bucket,
        scope: &Scope,
        id: &str,
        ttl_secs: u64,
        now: SystemTime,
    ) -> Result<SignedUrl> {
        let ttl = ttl_secs.clamp(1, 86_400);
        let m = meta::get_scoped(db, bucket.name, id, scope)
            .await?
            .ok_or_else(Error::not_found)?;
        assert_prefix(bucket, Some(scope), &m)?;
        let expires_at = unix_secs(now) + ttl;
        if let Some(url) = self
            .store
            .presign_get(bucket.name, &m.key, Duration::from_secs(ttl))
            .await?
        {
            return Ok(SignedUrl { url, expires_at });
        }
        let sig = sign::sign(self.sign_key()?, bucket.name, id, expires_at);
        Ok(SignedUrl {
            url: format!("/{}/{}?exp={}&sig={}", bucket.name, id, expires_at, sig),
            expires_at,
        })
    }

    /// Redeem an app-HMAC signed URL: constant-time verify + expiry, then an
    /// UNSCOPED fetch (the signature IS the credential). Invalid/expired = 401.
    pub async fn get_signed(
        &self,
        db: &jerrycan_db::Db,
        bucket: &Bucket,
        id: &str,
        exp: u64,
        sig: &str,
        now: SystemTime,
    ) -> Result<(ObjectMeta, Bytes)> {
        if !sign::verify(self.sign_key()?, bucket.name, id, exp, sig, unix_secs(now)) {
            return Err(Error::unauthorized());
        }
        let m = meta::get_scoped(db, bucket.name, id, &Scope::default())
            .await?
            .ok_or_else(Error::not_found)?;
        let bytes = self.store.get(bucket.name, &m.key).await?;
        Ok((m, bytes))
    }
```

Plus the free helper next to `now_millis`:

```rust
fn unix_secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}
```

- [ ] 4. Run: `cargo test -p jerrycan-storage` — expected PASS.
- [ ] 5. Commit: `Add signed-URL issue and redeem ops to the Storage service`

## Task 7 — jerrycan-core: blanket `Option<T>` extractor

**Files:**
- Modify: `crates/jerrycan-core/src/extract.rs`
- Test: in-module (extract.rs's existing `#[cfg(test)]` module)

- [ ] 1. Write the failing test (in extract.rs's tests, following its existing TestApp-based style):

```rust
    #[tokio::test]
    async fn option_extractor_yields_none_on_failure_and_some_on_success() {
        // WHY: a private bucket's GET must accept EITHER a session OR a signed
        // URL — the handler needs optional extraction instead of a hard 401
        // from the extractor. Option<T> is None on ANY extraction failure.
        #[derive(serde::Deserialize)]
        struct P { n: i64 }
        async fn probe(q: Option<Query<P>>) -> Result<Json<Option<i64>>> {
            Ok(Json(q.map(|Query(p)| p.n)))
        }
        let t = crate::App::new()
            .route("/probe", crate::get(probe))
            .into_test();
        assert_eq!(t.get("/probe?n=7").await.text(), "7");
        // Missing/malformed query → None, not a 400.
        assert_eq!(t.get("/probe").await.text(), "null");
        assert_eq!(t.get("/probe?n=not-a-number").await.text(), "null");
    }
```

(Match the surrounding tests' exact `App`/route construction idiom — if routes are only registered through `Module`, mount a module as the neighboring tests do.)

- [ ] 2. Run: `cargo test -p jerrycan-core option_extractor` — expected FAIL: `Option<Query<P>>: FromRequest` not satisfied.
- [ ] 3. Implement in `extract.rs` (next to the other `FromRequest` impls):

```rust
/// Optional extraction: `Some` when the inner extractor succeeds, `None` on
/// ANY extraction failure. For genuinely optional inputs — the canonical use
/// is optional auth (`Option<CurrentUser>` on a route that also accepts a
/// signed URL). Do NOT use it to paper over malformed required input: the
/// failure reason is discarded by design.
impl<T: FromRequest> FromRequest for Option<T> {
    async fn from_request(ctx: &mut RequestCtx) -> Result<Self> {
        Ok(T::from_request(ctx).await.ok())
    }
}
```

- [ ] 4. Run: `cargo test -p jerrycan-core` — expected PASS (full crate stays green).
- [ ] 5. Commit: `Support Option<T> extractors in jerrycan-core`

## Task 8 — Facade `storage` feature + re-export

**Files:**
- Modify: `crates/jerrycan/Cargo.toml`, `crates/jerrycan/src/lib.rs`

- [ ] 1. Failing check first: `cargo check -p jerrycan --features storage` — expected FAIL: `the package 'jerrycan' does not contain this feature: storage`.
- [ ] 2. Implement. `crates/jerrycan/Cargo.toml` — after the `rate-limit-redis` feature line:

```toml
# Object storage: design-modeled buckets + blob backends. Metadata lives in a
# jerrycan-db table, so `storage` implies `db` (like `jobs`).
storage = ["dep:jerrycan-storage", "db"]
```

And in `[dependencies]` after `jerrycan-ratelimit`:

```toml
jerrycan-storage = { workspace = true, optional = true }
```

`crates/jerrycan/src/lib.rs` — after the `jobs` re-export:

```rust
#[cfg(feature = "storage")]
pub use jerrycan_storage as storage;
```

- [ ] 3. Run: `cargo check -p jerrycan --features storage` and `cargo test -p jerrycan` — expected PASS (feature resolves; default build untouched).
- [ ] 4. Commit: `Add facade storage feature re-exporting jerrycan-storage`

## Task 9 — SigV4 signer (`sigv4.rs`, behind `storage-s3`)

**Files:**
- Modify: `crates/jerrycan-storage/Cargo.toml` (feature + optional deps), `crates/jerrycan-storage/src/lib.rs` (cfg-gated `mod sigv4;` already added in Task 5's mod block)
- Create: `crates/jerrycan-storage/src/sigv4.rs`
- Test: in-module, run with `--features storage-s3`

- [ ] 1. Add the feature + optional deps to `crates/jerrycan-storage/Cargo.toml`:

```toml
# --- storage-s3 only: outbound HTTPS client on jerrycan's own stack (mirrors
# jerrycan-auth's `oauth` feature: hyper + hyper-rustls, rustls/ring only) and
# quick-xml for S3 error + multipart-initiate bodies. No reqwest, no aws-sdk.
hyper = { workspace = true, optional = true }
hyper-util = { workspace = true, features = ["client", "client-legacy"], optional = true }
hyper-rustls = { workspace = true, optional = true }
http-body-util = { workspace = true, optional = true }
rustls = { workspace = true, optional = true }
webpki-roots = { workspace = true, optional = true }
quick-xml = { workspace = true, optional = true }
```

```toml
[features]
storage-s3 = [
    "dep:hyper",
    "dep:hyper-util",
    "dep:hyper-rustls",
    "dep:http-body-util",
    "dep:rustls",
    "dep:webpki-roots",
    "dep:quick-xml",
]
```

Root `Cargo.toml` `[workspace.dependencies]` — the ONE new third-party crate for storage:

```toml
# S3 XML parsing (error + multipart-initiate bodies only; listing is DB-backed
# so object keys never round-trip through XML). Pure safe Rust, MIT.
quick-xml = "0.37"
```

- [ ] 2. Write the failing tests (bottom of new `sigv4.rs`) — pinned to AWS's published SigV4 example vectors (secret `wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY`, 2015-08-30, us-east-1, iam):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";

    #[test]
    fn signing_key_matches_the_aws_published_vector() {
        let k = signing_key(SECRET, "20150830", "us-east-1", "iam");
        assert_eq!(
            crate::sign::hex(&k),
            "c4afb1cc5771d871763a393e44b703571b55cc28424d1a5e86da6ed3c154a4b9"
        );
    }

    #[test]
    fn full_request_signature_matches_the_aws_published_vector() {
        // GET https://iam.amazonaws.com/?Action=ListUsers&Version=2010-05-08
        // (the canonical example from the AWS SigV4 documentation).
        let creds = Credentials {
            access_key: "AKIDEXAMPLE".into(),
            secret_key: SECRET.into(),
            region: "us-east-1".into(),
        };
        let empty_body_sha = sha256_hex(b"");
        let (auth, signature) = authorization(
            &creds,
            "iam",
            "GET",
            "/",
            &[("Action".into(), "ListUsers".into()), ("Version".into(), "2010-05-08".into())],
            &[
                ("content-type".into(), "application/x-www-form-urlencoded; charset=utf-8".into()),
                ("host".into(), "iam.amazonaws.com".into()),
                ("x-amz-date".into(), "20150830T123600Z".into()),
            ],
            &empty_body_sha,
            "20150830T123600Z",
        );
        assert_eq!(signature, "5d672d79c15b13162d9279b0855cfba6789a8edb4c82c400e06b5924a6f2b5d7");
        assert!(auth.starts_with("AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/iam/aws4_request, SignedHeaders=content-type;host;x-amz-date, Signature="), "{auth}");
    }

    #[test]
    fn uri_encode_is_rfc3986_with_optional_slash_passthrough() {
        assert_eq!(uri_encode("a b/c~d", false), "a%20b%2Fc~d");
        assert_eq!(uri_encode("a b/c~d", true), "a%20b/c~d");
    }

    #[test]
    fn presign_query_carries_the_v4_parameters_and_is_deterministic() {
        let creds = Credentials {
            access_key: "AKIDEXAMPLE".into(),
            secret_key: SECRET.into(),
            region: "us-east-1".into(),
        };
        let a = presign_url(&creds, "https://s3.us-east-1.amazonaws.com", "/bkt/app/k.png", 300, "20150830T123600Z");
        let b = presign_url(&creds, "https://s3.us-east-1.amazonaws.com", "/bkt/app/k.png", 300, "20150830T123600Z");
        assert_eq!(a, b, "presigning is deterministic for a fixed instant");
        for needle in [
            "X-Amz-Algorithm=AWS4-HMAC-SHA256",
            "X-Amz-Credential=AKIDEXAMPLE%2F20150830%2Fus-east-1%2Fs3%2Faws4_request",
            "X-Amz-Date=20150830T123600Z",
            "X-Amz-Expires=300",
            "X-Amz-SignedHeaders=host",
            "X-Amz-Signature=",
        ] {
            assert!(a.contains(needle), "missing {needle} in {a}");
        }
    }
}
```

- [ ] 3. Run: `cargo test -p jerrycan-storage --features storage-s3 sigv4` — expected FAIL: module items unresolved.
- [ ] 4. Implement `sigv4.rs`:

```rust
//! AWS Signature Version 4 over `hmac` + `sha2` (no new crypto crate): header
//! signing for S3 requests and query presigning for native signed GET URLs.
//! Reference: the AWS SigV4 specification; unit tests pin the AWS-published
//! example vectors so a canonicalization regression cannot slip through.

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

pub(crate) struct Credentials {
    pub access_key: String,
    pub secret_key: String,
    pub region: String,
}

pub(crate) fn sha256_hex(data: &[u8]) -> String {
    crate::sign::hex(&Sha256::digest(data))
}

fn hmac_raw(key: &[u8], data: &str) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(key).expect("hmac accepts any key length");
    mac.update(data.as_bytes());
    mac.finalize().into_bytes().into()
}

/// The AWS4 signing-key chain: HMAC("AWS4"+secret, date) → region → service →
/// "aws4_request".
pub(crate) fn signing_key(secret: &str, date: &str, region: &str, service: &str) -> [u8; 32] {
    let k_date = hmac_raw(format!("AWS4{secret}").as_bytes(), date);
    let k_region = hmac_raw(&k_date, region);
    let k_service = hmac_raw(&k_region, service);
    hmac_raw(&k_service, "aws4_request")
}

/// RFC 3986 percent-encoding over the unreserved set; `keep_slash` leaves `/`
/// intact (S3 canonical URIs encode per path segment).
pub(crate) fn uri_encode(s: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b'/' if keep_slash => out.push('/'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Canonical query string: keys+values uri-encoded, sorted by encoded key.
fn canonical_query(query: &[(String, String)]) -> String {
    let mut pairs: Vec<(String, String)> = query
        .iter()
        .map(|(k, v)| (uri_encode(k, false), uri_encode(v, false)))
        .collect();
    pairs.sort();
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

/// The Authorization header + raw signature for a header-signed request.
/// `headers` must already contain `host` (and, for S3, `x-amz-date` +
/// `x-amz-content-sha256`); names are lowercased and sorted here.
pub(crate) fn authorization(
    creds: &Credentials,
    service: &str,
    method: &str,
    canonical_path: &str,
    query: &[(String, String)],
    headers: &[(String, String)],
    payload_sha256_hex: &str,
    datetime: &str, // YYYYMMDDTHHMMSSZ
) -> (String, String) {
    let mut hdrs: Vec<(String, String)> = headers
        .iter()
        .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    hdrs.sort();
    let signed_headers = hdrs.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(";");
    let canonical_headers: String = hdrs.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let canonical_request = format!(
        "{method}\n{canonical_path}\n{}\n{canonical_headers}\n{signed_headers}\n{payload_sha256_hex}",
        canonical_query(query)
    );
    let date = &datetime[..8];
    let scope = format!("{date}/{}/{service}/aws4_request", creds.region);
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{datetime}\n{scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );
    let key = signing_key(&creds.secret_key, date, &creds.region, service);
    let signature = crate::sign::hex(&hmac_raw(&key, &string_to_sign));
    let auth = format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
        creds.access_key
    );
    (auth, signature)
}

/// A query-presigned GET URL (service = s3, UNSIGNED-PAYLOAD, host-only
/// signed headers) — the native signed-URL path (Supabase createSignedUrl parity).
pub(crate) fn presign_url(
    creds: &Credentials,
    endpoint: &str, // scheme://host[:port], no trailing slash
    canonical_path: &str,
    ttl_secs: u64,
    datetime: &str,
) -> String {
    let date = &datetime[..8];
    let scope = format!("{date}/{}/s3/aws4_request", creds.region);
    let host = endpoint.split_once("://").map(|(_, rest)| rest).unwrap_or(endpoint);
    let query: Vec<(String, String)> = vec![
        ("X-Amz-Algorithm".into(), "AWS4-HMAC-SHA256".into()),
        ("X-Amz-Credential".into(), format!("{}/{scope}", creds.access_key)),
        ("X-Amz-Date".into(), datetime.into()),
        ("X-Amz-Expires".into(), ttl_secs.to_string()),
        ("X-Amz-SignedHeaders".into(), "host".into()),
    ];
    let canonical_request = format!(
        "GET\n{canonical_path}\n{}\nhost:{host}\n\nhost\nUNSIGNED-PAYLOAD",
        canonical_query(&query)
    );
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{datetime}\n{scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );
    let key = signing_key(&creds.secret_key, date, &creds.region, "s3");
    let signature = crate::sign::hex(&hmac_raw(&key, &string_to_sign));
    format!(
        "{endpoint}{canonical_path}?{}&X-Amz-Signature={signature}",
        canonical_query(&query)
    )
}
```

- [ ] 5. Run: `cargo test -p jerrycan-storage --features storage-s3 sigv4` — expected PASS (4 tests; the two AWS vectors are the proof). Also run the default-features suite: `cargo test -p jerrycan-storage` stays green.
- [ ] 6. Commit: `Add SigV4 signer (headers + query presign) behind storage-s3`

## Task 10 — S3 XML parsing (`xml.rs`, quick-xml)

**Files:**
- Create: `crates/jerrycan-storage/src/xml.rs`
- Test: in-module, `--features storage-s3`

- [ ] 1. Write the failing tests with REAL provider bodies (AWS, MinIO, R2 shapes differ in attributes/fields — the parser must tolerate all):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_aws_minio_and_r2_error_bodies() {
        // AWS S3 (extra elements after Message).
        let aws = br#"<?xml version="1.0" encoding="UTF-8"?>
<Error><Code>NoSuchKey</Code><Message>The specified key does not exist.</Message><Key>a.png</Key><RequestId>ABC123</RequestId><HostId>host==</HostId></Error>"#;
        assert_eq!(
            parse_error(aws),
            Some(("NoSuchKey".into(), "The specified key does not exist.".into()))
        );
        // MinIO (adds BucketName/Resource/Region).
        let minio = br#"<?xml version="1.0" encoding="UTF-8"?>
<Error><Code>AccessDenied</Code><Message>Access Denied.</Message><BucketName>jc</BucketName><Resource>/jc/x</Resource><RequestId>17</RequestId><HostId>h</HostId></Error>"#;
        assert_eq!(parse_error(minio), Some(("AccessDenied".into(), "Access Denied.".into())));
        // R2 (minimal body, no xml declaration).
        let r2 = br#"<Error><Code>InternalError</Code><Message>We encountered an internal error.</Message></Error>"#;
        assert_eq!(parse_error(r2), Some(("InternalError".into(), "We encountered an internal error.".into())));
        // Not an error document at all → None (caller falls back to status text).
        assert_eq!(parse_error(b"not xml"), None);
        assert_eq!(parse_error(b"<Ok/>"), None);
    }

    #[test]
    fn parses_initiate_multipart_upload_id_with_and_without_xmlns() {
        // AWS emits xmlns; MinIO does too; the parser must not care.
        let aws = br#"<?xml version="1.0" encoding="UTF-8"?>
<InitiateMultipartUploadResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/"><Bucket>jc</Bucket><Key>app/k.bin</Key><UploadId>VXBsb2FkIElE</UploadId></InitiateMultipartUploadResult>"#;
        assert_eq!(parse_upload_id(aws).unwrap(), "VXBsb2FkIElE");
        let bare = br#"<InitiateMultipartUploadResult><Bucket>b</Bucket><Key>k</Key><UploadId>u-1</UploadId></InitiateMultipartUploadResult>"#;
        assert_eq!(parse_upload_id(bare).unwrap(), "u-1");
        // A body with no UploadId is a loud error, never an empty string.
        assert!(parse_upload_id(b"<InitiateMultipartUploadResult></InitiateMultipartUploadResult>").is_err());
    }
}
```

- [ ] 2. Run: `cargo test -p jerrycan-storage --features storage-s3 xml` — expected FAIL.
- [ ] 3. Implement `xml.rs`:

```rust
//! quick-xml parsing for the ONLY two S3 XML shapes jerrycan reads: error
//! bodies (`<Error><Code>/<Message>`) and `InitiateMultipartUploadResult`
//! (`<UploadId>`). Listing is DB-backed, so object keys never round-trip
//! through XML. Tested against AWS, MinIO, and R2 body variants.

use jerrycan_core::{Error, Result};
use quick_xml::Reader;
use quick_xml::events::Event;

/// Pull the text content of every element named in `wanted`, in document
/// order, tolerating unknown siblings and namespace attributes.
fn texts_of(body: &[u8], wanted: &[&str]) -> Vec<(String, String)> {
    let mut reader = Reader::from_reader(body);
    reader.config_mut().trim_text(true);
    let mut out = Vec::new();
    let mut current: Option<String> = None;
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).to_string();
                current = wanted.contains(&name.as_str()).then_some(name);
            }
            Ok(Event::Text(t)) => {
                if let (Some(name), Ok(text)) = (current.take(), t.decode()) {
                    out.push((name, text.into_owned()));
                }
            }
            Ok(Event::End(_)) => current = None,
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    out
}

/// `(Code, Message)` from an S3 error body, or `None` when the body is not an
/// S3 error document (the caller reports the raw HTTP status instead).
pub(crate) fn parse_error(body: &[u8]) -> Option<(String, String)> {
    let found = texts_of(body, &["Code", "Message"]);
    let code = found.iter().find(|(k, _)| k == "Code")?.1.clone();
    let message = found
        .iter()
        .find(|(k, _)| k == "Message")
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    Some((code, message))
}

/// The `UploadId` from an InitiateMultipartUploadResult body — loud when absent.
pub(crate) fn parse_upload_id(body: &[u8]) -> Result<String> {
    texts_of(body, &["UploadId"])
        .into_iter()
        .next()
        .map(|(_, v)| v)
        .ok_or_else(|| Error::internal("s3: InitiateMultipartUpload response carried no UploadId"))
}
```

- [ ] 4. Run: `cargo test -p jerrycan-storage --features storage-s3 xml` — expected PASS.
- [ ] 5. Commit: `Add quick-xml S3 error and multipart-initiate parsing`

## Task 11 — `S3Store` core: config parse, hyper client, single-shot put/get/delete

**Files:**
- Create: `crates/jerrycan-storage/src/s3_store.rs`
- Test: in-module (config/URL/guard logic only — network paths are Task 13's MinIO test)

- [ ] 1. Write the failing tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(url: &str) -> Result<S3Config, jerrycan_core::Error> {
        S3Config::from_url(url, Some("ak".into()), Some("sk".into()))
    }

    #[test]
    fn config_parses_bucket_region_and_endpoint() {
        let c = cfg("s3://my-bucket?region=eu-central-1&endpoint=https://minio.example.com:9000").unwrap();
        assert_eq!(c.bucket, "my-bucket");
        assert_eq!(c.region, "eu-central-1");
        assert_eq!(c.endpoint, "https://minio.example.com:9000");
        // Defaults: us-east-1 + the AWS regional endpoint, derived from region.
        let d = cfg("s3://my-bucket").unwrap();
        assert_eq!(d.region, "us-east-1");
        assert_eq!(d.endpoint, "https://s3.us-east-1.amazonaws.com");
    }

    #[test]
    fn config_requires_credentials_and_a_bucket() {
        let err = S3Config::from_url("s3://b", None, Some("sk".into())).unwrap_err();
        assert!(err.message().contains("AWS_ACCESS_KEY_ID"), "{err}");
        let err = cfg("s3://?region=x").unwrap_err();
        assert!(err.message().contains("bucket"), "{err}");
    }

    #[test]
    fn plaintext_endpoints_are_loopback_only() {
        // WHY: an http:// endpoint ships the SigV4-authorized payload in
        // cleartext — allowed only for the local MinIO harness (same
        // TLS-downgrade guard as jerrycan-auth's OAuth transport).
        assert!(cfg("s3://b?endpoint=http://127.0.0.1:9000").is_ok());
        assert!(cfg("s3://b?endpoint=http://localhost:9000").is_ok());
        let err = cfg("s3://b?endpoint=http://minio.internal:9000").unwrap_err();
        assert!(err.message().contains("plaintext"), "{err}");
        assert!(cfg("s3://b?endpoint=https://minio.internal:9000").is_ok());
    }

    #[test]
    fn object_paths_are_path_style_and_segment_encoded() {
        let c = cfg("s3://my-bucket?endpoint=https://x.example.com").unwrap();
        assert_eq!(c.object_path("avatars", "u 1/pic.png"), "/my-bucket/avatars/u%201/pic.png");
    }
}
```

- [ ] 2. Run: `cargo test -p jerrycan-storage --features storage-s3 s3_store` — expected FAIL.
- [ ] 3. Implement `s3_store.rs` (client shape mirrors `jerrycan-auth::oauth::HttpTransport` byte-for-byte where applicable):

```rust
//! The S3-compatible blob store (AWS S3, Cloudflare R2, MinIO, Supabase's S3
//! endpoint), built on jerrycan's own outbound stack: hyper_util's legacy
//! client + hyper-rustls (rustls/ring, bundled webpki roots) — the identical
//! shape to jerrycan-auth's OAuth transport. Always path-style addressing:
//! `/{s3_bucket}/{app_bucket}/{key}`. Plaintext http:// endpoints are refused
//! unless loopback (the MinIO harness), mirroring the OAuth TLS-downgrade guard.

use crate::sigv4::{self, Credentials};
use crate::store::{BlobFuture, BlobStore};
use crate::xml;
use bytes::Bytes;
use http_body_util::BodyExt;
use jerrycan_core::{Error, Result};
use std::time::Duration;

/// Multipart threshold AND part size: bodies above this upload in 8 MiB parts.
const PART_SIZE: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug)]
pub(crate) struct S3Config {
    pub bucket: String,
    pub region: String,
    pub endpoint: String, // scheme://host[:port], no trailing slash
    pub access_key: String,
    pub secret_key: String,
}

impl S3Config {
    /// Parse `s3://bucket?region=…&endpoint=…`; credentials are passed in
    /// (from_env reads AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY).
    pub(crate) fn from_url(url: &str, access_key: Option<String>, secret_key: Option<String>) -> Result<Self> {
        let rest = url.strip_prefix("s3://").ok_or_else(|| {
            Error::internal(format!("s3 config: `{url}` does not start with s3://"))
        })?;
        let (bucket, query) = rest.split_once('?').unwrap_or((rest, ""));
        if bucket.is_empty() {
            return Err(Error::internal("s3 config: missing bucket — use s3://<bucket>?region=…"));
        }
        let mut region = "us-east-1".to_string();
        let mut endpoint = None;
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            match pair.split_once('=') {
                Some(("region", v)) => region = v.to_string(),
                Some(("endpoint", v)) => endpoint = Some(v.trim_end_matches('/').to_string()),
                _ => {
                    return Err(Error::internal(format!(
                        "s3 config: unknown parameter `{pair}` — supported: region, endpoint"
                    )));
                }
            }
        }
        let endpoint = endpoint.unwrap_or_else(|| format!("https://s3.{region}.amazonaws.com"));
        if !plaintext_endpoint_ok(&endpoint) {
            return Err(Error::internal(
                "s3 config: refusing a plaintext http:// endpoint to a non-loopback host — use https:// (http is allowed only for a local MinIO)",
            ));
        }
        let access_key = access_key.ok_or_else(|| {
            Error::internal("s3 config: AWS_ACCESS_KEY_ID is not set")
        })?;
        let secret_key = secret_key.ok_or_else(|| {
            Error::internal("s3 config: AWS_SECRET_ACCESS_KEY is not set")
        })?;
        Ok(Self { bucket, region, endpoint, access_key, secret_key })
    }

    /// Path-style object path, key encoded per segment (slashes kept).
    pub(crate) fn object_path(&self, app_bucket: &str, key: &str) -> String {
        format!("/{}/{}/{}", self.bucket, app_bucket, sigv4::uri_encode(key, true))
    }

    fn host(&self) -> &str {
        self.endpoint.split_once("://").map(|(_, rest)| rest).unwrap_or(&self.endpoint)
    }

    fn credentials(&self) -> Credentials {
        Credentials {
            access_key: self.access_key.clone(),
            secret_key: self.secret_key.clone(),
            region: self.region.clone(),
        }
    }
}

/// `https://` always; `http://` only to 127.0.0.1 / ::1 / localhost (the local
/// MinIO harness). Same policy as `jerrycan-auth::oauth::is_loopback_http_ok`.
fn plaintext_endpoint_ok(endpoint: &str) -> bool {
    let Some((scheme, rest)) = endpoint.split_once("://") else {
        return false;
    };
    if scheme.eq_ignore_ascii_case("https") {
        return true;
    }
    if !scheme.eq_ignore_ascii_case("http") {
        return false;
    }
    let authority = rest.split(['/', '?', '#']).next().expect("split yields one element");
    if authority.contains('@') {
        return false;
    }
    let host = if let Some(after) = authority.strip_prefix('[') {
        match after.split_once(']') {
            Some((inner, _)) => inner,
            None => return false,
        }
    } else {
        authority.rsplit_once(':').map_or(authority, |(h, _)| h)
    };
    host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1"
}

type Client = hyper_util::client::legacy::Client<
    hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
    http_body_util::Full<Bytes>,
>;

/// The S3-compatible [`BlobStore`]. Construct via [`S3Store::from_url`].
pub struct S3Store {
    config: S3Config,
    client: Client,
}

impl S3Store {
    /// `s3://bucket?region=…&endpoint=…`; credentials from the standard
    /// AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY env vars.
    pub fn from_url(url: &str) -> Result<Self> {
        let config = S3Config::from_url(
            url,
            std::env::var("AWS_ACCESS_KEY_ID").ok(),
            std::env::var("AWS_SECRET_ACCESS_KEY").ok(),
        )?;
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_provider_and_webpki_roots(rustls::crypto::ring::default_provider())
            .expect("ring provider supports rustls' safe default protocol versions")
            .https_or_http()
            .enable_http1()
            .build();
        let client = hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
            .build(connector);
        Ok(Self { config, client })
    }

    /// `YYYYMMDDTHHMMSSZ` for now — SystemTime-derived, no chrono (mirrors the
    /// epoch-millis philosophy in jerrycan-jobs).
    fn amz_datetime() -> String {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        // Civil-from-days (Howard Hinnant's algorithm) — correct for all dates
        // the process will ever see; leap seconds are not S3's concern.
        let days = (secs / 86_400) as i64;
        let (h, m, s) = ((secs % 86_400) / 3_600, (secs % 3_600) / 60, secs % 60);
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let mo = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = if mo <= 2 { y + 1 } else { y };
        format!("{y:04}{mo:02}{d:02}T{h:02}{m:02}{s:02}Z")
    }

    /// One signed request. Non-2xx maps NoSuchKey/404 → not_found, everything
    /// else → an internal error carrying the parsed `<Code>: <Message>` (never
    /// the signature or credentials).
    async fn request(
        &self,
        method: &str,
        path: &str,
        query: &[(String, String)],
        body: Bytes,
        content_type: Option<&str>,
    ) -> Result<(http::StatusCode, http::HeaderMap, Bytes)> {
        let datetime = Self::amz_datetime();
        let payload_hash = sigv4::sha256_hex(&body);
        let mut headers: Vec<(String, String)> = vec![
            ("host".into(), self.config.host().to_string()),
            ("x-amz-content-sha256".into(), payload_hash.clone()),
            ("x-amz-date".into(), datetime.clone()),
        ];
        if let Some(ct) = content_type {
            headers.push(("content-type".into(), ct.to_string()));
        }
        let (auth, _sig) = sigv4::authorization(
            &self.config.credentials(),
            "s3",
            method,
            path,
            query,
            &headers,
            &payload_hash,
            &datetime,
        );
        let qs = if query.is_empty() {
            String::new()
        } else {
            let pairs: Vec<String> = query
                .iter()
                .map(|(k, v)| {
                    if v.is_empty() {
                        sigv4::uri_encode(k, false)
                    } else {
                        format!("{}={}", sigv4::uri_encode(k, false), sigv4::uri_encode(v, false))
                    }
                })
                .collect();
            format!("?{}", pairs.join("&"))
        };
        let uri = format!("{}{}{}", self.config.endpoint, path, qs);
        let mut builder = hyper::Request::builder()
            .method(method)
            .uri(&uri)
            .header("authorization", &auth);
        for (k, v) in &headers {
            if k != "host" {
                builder = builder.header(k, v);
            }
        }
        let request = builder
            .body(http_body_util::Full::new(body))
            .map_err(|e| Error::internal(format!("s3: building request failed: {e}")))?;
        let response = self
            .client
            .request(request)
            .await
            .map_err(|_| Error::internal("s3: request to the storage endpoint failed"))?;
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response
            .into_body()
            .collect()
            .await
            .map_err(|_| Error::internal("s3: reading the response body failed"))?
            .to_bytes();
        Ok((status, headers, bytes))
    }

    /// Map a non-2xx S3 response to a jerrycan error.
    fn s3_error(status: http::StatusCode, body: &[u8]) -> Error {
        match xml::parse_error(body) {
            Some((code, _)) if code == "NoSuchKey" || status == http::StatusCode::NOT_FOUND => Error::not_found(),
            Some((code, message)) => Error::internal(format!("s3: {code}: {message}")),
            None if status == http::StatusCode::NOT_FOUND => Error::not_found(),
            None => Error::internal(format!("s3: unexpected status {status}")),
        }
    }
}

impl BlobStore for S3Store {
    fn put<'a>(&'a self, bucket: &'a str, key: &'a str, body: Bytes, mime: &'a str) -> BlobFuture<'a, ()> {
        Box::pin(async move {
            crate::store::validate_key(key)?;
            if body.len() > PART_SIZE {
                return self.put_multipart(bucket, key, body, mime).await; // Task 12
            }
            let path = self.config.object_path(bucket, key);
            let (status, _h, resp) = self.request("PUT", &path, &[], body, Some(mime)).await?;
            if status.is_success() { Ok(()) } else { Err(Self::s3_error(status, &resp)) }
        })
    }

    fn get<'a>(&'a self, bucket: &'a str, key: &'a str) -> BlobFuture<'a, Bytes> {
        Box::pin(async move {
            let path = self.config.object_path(bucket, key);
            let (status, _h, resp) = self.request("GET", &path, &[], Bytes::new(), None).await?;
            if status.is_success() { Ok(resp) } else { Err(Self::s3_error(status, &resp)) }
        })
    }

    fn delete<'a>(&'a self, bucket: &'a str, key: &'a str) -> BlobFuture<'a, ()> {
        Box::pin(async move {
            let path = self.config.object_path(bucket, key);
            let (status, _h, resp) = self.request("DELETE", &path, &[], Bytes::new(), None).await?;
            // 204 success; 404 is idempotent-ok (the metadata row is the truth).
            if status.is_success() || status == http::StatusCode::NOT_FOUND {
                Ok(())
            } else {
                Err(Self::s3_error(status, &resp))
            }
        })
    }

    fn presign_get<'a>(&'a self, bucket: &'a str, key: &'a str, ttl: Duration) -> BlobFuture<'a, Option<String>> {
        Box::pin(async move {
            let path = self.config.object_path(bucket, key);
            Ok(Some(sigv4::presign_url(
                &self.config.credentials(),
                &self.config.endpoint,
                &path,
                ttl.as_secs().max(1),
                &Self::amz_datetime(),
            )))
        })
    }
}
```

For this task, stub `put_multipart` so the crate compiles (Task 12 replaces it — this is the ONE intentionally-red seam, and it is loud, not silent):

```rust
impl S3Store {
    async fn put_multipart(&self, _bucket: &str, _key: &str, _body: Bytes, _mime: &str) -> Result<()> {
        Err(Error::internal("s3: multipart upload lands in the next task"))
    }
}
```

- [ ] 4. Run: `cargo test -p jerrycan-storage --features storage-s3` — expected PASS (config/path/guard tests; no network).
- [ ] 5. Commit: `Add S3Store: path-style S3 backend on hyper and rustls`

## Task 12 — S3 multipart upload

**Files:**
- Modify: `crates/jerrycan-storage/src/s3_store.rs`
- Test: in-module (part math + XML body) — the live path is Task 13

- [ ] 1. Write the failing tests:

```rust
    #[test]
    fn part_split_covers_the_body_exactly() {
        // 20 MiB → 8 + 8 + 4.
        let chunks = part_ranges(20 * 1024 * 1024);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0], (0, 8 * 1024 * 1024));
        assert_eq!(chunks[1], (8 * 1024 * 1024, 16 * 1024 * 1024));
        assert_eq!(chunks[2], (16 * 1024 * 1024, 20 * 1024 * 1024));
        // Exactly one part size → a single range (multipart not even entered).
        assert_eq!(part_ranges(8 * 1024 * 1024), vec![(0, 8 * 1024 * 1024)]);
    }

    #[test]
    fn complete_body_lists_parts_in_order_with_their_etags() {
        let body = complete_multipart_body(&[(1, "\"etag-a\"".into()), (2, "\"etag-b\"".into())]);
        assert_eq!(
            body,
            "<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>\"etag-a\"</ETag></Part><Part><PartNumber>2</PartNumber><ETag>\"etag-b\"</ETag></Part></CompleteMultipartUpload>"
        );
    }
```

- [ ] 2. Run: `cargo test -p jerrycan-storage --features storage-s3 part_` — expected FAIL.
- [ ] 3. Implement (replace the Task 11 stub):

```rust
/// `(start, end)` byte ranges of PART_SIZE chunks covering `len`.
fn part_ranges(len: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0;
    while start < len {
        out.push((start, (start + PART_SIZE).min(len)));
        start += PART_SIZE;
    }
    out
}

/// The CompleteMultipartUpload request body. ETags pass through verbatim
/// (S3 returns them quoted). Building XML is trivial string work — quick-xml
/// is for PARSING only.
fn complete_multipart_body(parts: &[(usize, String)]) -> String {
    let mut body = String::from("<CompleteMultipartUpload>");
    for (n, etag) in parts {
        body.push_str(&format!("<Part><PartNumber>{n}</PartNumber><ETag>{etag}</ETag></Part>"));
    }
    body.push_str("</CompleteMultipartUpload>");
    body
}

impl S3Store {
    /// Multipart upload: initiate (XML UploadId) → PUT each 8 MiB part
    /// (collecting ETag headers) → complete (XML part manifest). An error at
    /// any stage aborts the upload so the bucket carries no dangling parts.
    async fn put_multipart(&self, bucket: &str, key: &str, body: Bytes, mime: &str) -> Result<()> {
        let path = self.config.object_path(bucket, key);
        let (status, _h, resp) = self
            .request("POST", &path, &[("uploads".into(), String::new())], Bytes::new(), Some(mime))
            .await?;
        if !status.is_success() {
            return Err(Self::s3_error(status, &resp));
        }
        let upload_id = xml::parse_upload_id(&resp)?;

        let mut parts: Vec<(usize, String)> = Vec::new();
        for (i, (start, end)) in part_ranges(body.len()).into_iter().enumerate() {
            let n = i + 1;
            let query = vec![
                ("partNumber".into(), n.to_string()),
                ("uploadId".into(), upload_id.clone()),
            ];
            let (status, headers, resp) = self
                .request("PUT", &path, &query, body.slice(start..end), None)
                .await?;
            if !status.is_success() {
                self.abort_multipart(&path, &upload_id).await;
                return Err(Self::s3_error(status, &resp));
            }
            let etag = headers
                .get("etag")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
                .ok_or_else(|| Error::internal("s3: UploadPart response carried no ETag"))?;
            parts.push((n, etag));
        }

        let manifest = complete_multipart_body(&parts);
        let (status, _h, resp) = self
            .request(
                "POST",
                &path,
                &[("uploadId".into(), upload_id.clone())],
                Bytes::from(manifest),
                Some("application/xml"),
            )
            .await?;
        // CompleteMultipartUpload can return 200 with an <Error> body.
        if !status.is_success() || xml::parse_error(&resp).is_some() {
            self.abort_multipart(&path, &upload_id).await;
            return Err(Self::s3_error(status, &resp));
        }
        Ok(())
    }

    /// Best-effort abort — failure here is logged, not surfaced (the original
    /// error is what the caller needs).
    async fn abort_multipart(&self, path: &str, upload_id: &str) {
        let query = vec![("uploadId".into(), upload_id.to_string())];
        if let Err(e) = self.request("DELETE", path, &query, Bytes::new(), None).await {
            eprintln!("jerrycan-storage: abort multipart upload failed: {e}");
        }
    }
}
```

- [ ] 4. Run: `cargo test -p jerrycan-storage --features storage-s3` — expected PASS. `cargo clippy -p jerrycan-storage --all-targets --features storage-s3 -- -D warnings`.
- [ ] 5. Commit: `Add S3 multipart upload with abort-on-failure`

## Task 13 — Facade `storage-s3` feature + MinIO integration test

**Files:**
- Modify: `crates/jerrycan/Cargo.toml`
- Create: `crates/jerrycan-storage/tests/s3_minio.rs`

- [ ] 1. Failing check: `cargo check -p jerrycan --features storage-s3` — expected FAIL: unknown feature.
- [ ] 2. Implement. `crates/jerrycan/Cargo.toml` after the `storage` feature:

```toml
# The S3-compatible backend (AWS/R2/MinIO/Supabase-S3) as a facade feature.
# Generated storage apps enable THIS one: JERRYCAN_STORAGE switches backends
# by env at runtime, so the S3 code must be compiled into the packaged binary.
storage-s3 = ["storage", "jerrycan-storage/storage-s3"]
```

- [ ] 3. Write the MinIO integration test `crates/jerrycan-storage/tests/s3_minio.rs` (env-gated: skips loudly when no endpoint is configured; CI runs a MinIO container):

```rust
//! Live integration against a MinIO container. Gated on JERRYCAN_TEST_S3 so
//! `cargo test` stays hermetic by default. To run locally:
//!   docker run --rm -d -p 9000:9000 --name jc-minio minio/minio server /data
//!   AWS_ACCESS_KEY_ID=minioadmin AWS_SECRET_ACCESS_KEY=minioadmin \
//!   JERRYCAN_TEST_S3='s3://jerrycan-test?region=us-east-1&endpoint=http://127.0.0.1:9000' \
//!   cargo test -p jerrycan-storage --features storage-s3 --test s3_minio
#![cfg(feature = "storage-s3")]

use bytes::Bytes;
use jerrycan_storage::{BlobStore, S3Store};
use std::time::Duration;

fn store() -> Option<S3Store> {
    let Ok(url) = std::env::var("JERRYCAN_TEST_S3") else {
        eprintln!("SKIP s3_minio: JERRYCAN_TEST_S3 not set (see file header to run against MinIO)");
        return None;
    };
    Some(S3Store::from_url(&url).expect("JERRYCAN_TEST_S3 must parse"))
}

#[tokio::test]
async fn single_shot_round_trip_and_idempotent_delete() {
    let Some(s) = store() else { return };
    s.ensure_bucket().await.expect("bucket exists");
    s.put("it", "small/a.txt", Bytes::from_static(b"hello minio"), "text/plain").await.unwrap();
    assert_eq!(s.get("it", "small/a.txt").await.unwrap(), Bytes::from_static(b"hello minio"));
    s.delete("it", "small/a.txt").await.unwrap();
    assert_eq!(s.get("it", "small/a.txt").await.unwrap_err().code(), "JC0404");
    s.delete("it", "small/a.txt").await.unwrap(); // idempotent
}

#[tokio::test]
async fn multipart_round_trips_a_20mib_body() {
    let Some(s) = store() else { return };
    s.ensure_bucket().await.expect("bucket exists");
    // > PART_SIZE forces initiate/parts/complete; a byte pattern (not zeros)
    // catches part reordering.
    let body: Vec<u8> = (0..20 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
    s.put("it", "big/blob.bin", Bytes::from(body.clone()), "application/octet-stream").await.unwrap();
    let got = s.get("it", "big/blob.bin").await.unwrap();
    assert_eq!(got.len(), body.len());
    assert_eq!(&got[..], &body[..], "multipart reassembly is byte-exact");
    s.delete("it", "big/blob.bin").await.unwrap();
}

#[tokio::test]
async fn presigned_get_url_is_fetchable_without_credentials() {
    let Some(s) = store() else { return };
    s.ensure_bucket().await.expect("bucket exists");
    s.put("it", "signed/x.txt", Bytes::from_static(b"presigned"), "text/plain").await.unwrap();
    let url = s
        .presign_get("it", "signed/x.txt", Duration::from_secs(120))
        .await
        .unwrap()
        .expect("s3 backend presigns natively");
    let bytes = s.fetch_unauthenticated(&url).await.expect("presigned fetch");
    assert_eq!(bytes, Bytes::from_static(b"presigned"));
    // A tampered signature is refused by the SERVER (403), proving the
    // signature is load-bearing, not decorative.
    assert!(s.fetch_unauthenticated(&format!("{url}0")).await.is_err());
    s.delete("it", "signed/x.txt").await.unwrap();
}
```

- [ ] 4. Add the two test-support methods to `s3_store.rs` (public, useful for ops too):

```rust
impl S3Store {
    /// Create the configured bucket if it does not exist (idempotent:
    /// BucketAlreadyOwnedByYou / BucketAlreadyExists are success). Used by the
    /// MinIO harness and first-boot provisioning.
    pub async fn ensure_bucket(&self) -> Result<()> {
        let path = format!("/{}", self.config.bucket);
        let (status, _h, resp) = self.request("PUT", &path, &[], Bytes::new(), None).await?;
        if status.is_success() {
            return Ok(());
        }
        match xml::parse_error(&resp) {
            Some((code, _)) if code == "BucketAlreadyOwnedByYou" || code == "BucketAlreadyExists" => Ok(()),
            _ => Err(Self::s3_error(status, &resp)),
        }
    }

    /// GET an absolute URL with NO SigV4 headers — proves a presigned URL is
    /// self-authorizing. Errors on any non-2xx.
    pub async fn fetch_unauthenticated(&self, url: &str) -> Result<Bytes> {
        let request = hyper::Request::builder()
            .method("GET")
            .uri(url)
            .body(http_body_util::Full::new(Bytes::new()))
            .map_err(|e| Error::internal(format!("s3: building request failed: {e}")))?;
        let response = self
            .client
            .request(request)
            .await
            .map_err(|_| Error::internal("s3: presigned fetch failed"))?;
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .map_err(|_| Error::internal("s3: reading the response body failed"))?
            .to_bytes();
        if status.is_success() { Ok(bytes) } else { Err(Self::s3_error(status, &bytes)) }
    }
}
```

- [ ] 5. Run: `cargo check -p jerrycan --features storage-s3` (PASS), `cargo test -p jerrycan-storage --features storage-s3` (PASS — MinIO tests skip without env), and with a local MinIO per the file header (all 3 live tests PASS).
- [ ] 6. Commit: `Add storage-s3 facade feature and MinIO integration test`

## Task 14 — Contract v2 in `design.rs` + published schema

**Files:**
- Modify: `crates/jerrycan/src/platform/design.rs`, `docs/contracts/design-schema.json`
- Test: in-module (design.rs tests)

- [ ] 1. Write the failing tests (design.rs `mod tests` — also add the shared v2 fixture other platform tasks reuse):

```rust
    pub(crate) const V2_STORAGE: &str = r#"{
        "name": "files-app", "contract_version": 2,
        "auth": { "model": "session", "roles": ["owner", "member"] },
        "dependencies": ["db", "auth"],
        "tenancy": { "entity": "Org", "member_roles": ["owner", "member"] },
        "storage": { "buckets": [
            { "name": "avatars", "visibility": "public", "owner": "User",
              "max_size": "5MB", "allowed_mime": ["image/*"] },
            { "name": "invoices", "visibility": "private", "owner": "Org",
              "owner_prefix": true, "max_size": "20MB" }
        ]},
        "modules": [
            { "name": "orgs",
              "entities": [
                  { "name": "Org", "fields": [
                      { "name": "id", "type": "integer" },
                      { "name": "plan", "type": "string" } ] },
                  { "name": "User", "fields": [
                      { "name": "id", "type": "integer" },
                      { "name": "email", "type": "string" } ] }
              ],
              "endpoints": [{ "operation_id": "list_orgs", "method": "GET", "path": "/",
                  "success": { "status": 200, "entity": "Org", "list": true } }] }
        ]
    }"#;

    #[test]
    fn v2_storage_block_round_trips_and_gates_wants_storage() {
        let d: Design = serde_json::from_str(V2_STORAGE).unwrap();
        assert_eq!(d.contract_version, 2);
        let s = d.storage.as_ref().unwrap();
        assert_eq!(s.buckets.len(), 2);
        assert_eq!(s.buckets[0].name, "avatars");
        assert_eq!(s.buckets[0].visibility, Visibility::Public);
        assert_eq!(s.buckets[0].owner.as_deref(), Some("User"));
        assert!(!s.buckets[0].owner_prefix, "owner_prefix defaults false");
        assert!(s.buckets[1].owner_prefix);
        assert!(d.wants_storage());
        let back = serde_json::to_string(&d).unwrap();
        let re: Design = serde_json::from_str(&back).unwrap();
        assert!(re.wants_storage(), "storage survives a round trip");
        // v0/v1 designs stay valid and storage-free.
        let v0: Design = serde_json::from_str(MINIMAL).unwrap();
        assert!(v0.storage.is_none() && !v0.wants_storage());
    }

    #[test]
    fn wants_storage_appends_the_storage_s3_facade_feature_last() {
        // Generated apps get storage-s3 (S3 compiled in) so JERRYCAN_STORAGE
        // can switch backends by env WITHOUT recompiling (zero-touch config).
        let d: Design = serde_json::from_str(V2_STORAGE).unwrap();
        let feats = d.facade_features();
        assert_eq!(feats.last(), Some(&"storage-s3"), "storage-s3 appended last: {feats:?}");
        assert!(feats.contains(&"db") && feats.contains(&"auth"));
        // No storage block → no storage feature (order of the rest unchanged).
        let no: Design = serde_json::from_str(V1_FULL).unwrap();
        assert!(!no.facade_features().contains(&"storage-s3"));
    }

    #[test]
    fn parse_size_handles_the_documented_suffixes() {
        assert_eq!(Design::parse_size("5MB"), Some(5 * 1024 * 1024));
        assert_eq!(Design::parse_size("20MB"), Some(20 * 1024 * 1024));
        assert_eq!(Design::parse_size("512KB"), Some(512 * 1024));
        assert_eq!(Design::parse_size("1GB"), Some(1024 * 1024 * 1024));
        assert_eq!(Design::parse_size("123B"), Some(123));
        assert_eq!(Design::parse_size("123"), Some(123), "bare number = bytes");
        assert_eq!(Design::parse_size("5mb"), None, "suffixes are uppercase (schema-validated)");
        assert_eq!(Design::parse_size("lots"), None);
    }
```

Also UPDATE the existing `published_schema_accepts_v1_constructs` test — the contract legitimately grew:

```rust
        assert_eq!(
            v["properties"]["contract_version"]["enum"],
            serde_json::json!([0, 1, 2])
        );
        assert!(s.contains("\"storage\"") && s.contains("\"buckets\"") && s.contains("\"owner_prefix\""));
```

- [ ] 2. Run: `cargo test -p jerrycan platform::design` — expected FAIL: `storage` field, `Visibility`, `wants_storage`, `parse_size` unresolved; schema assertion fails.
- [ ] 3. Implement in `design.rs`:

Add to `Design` (after `jobs`):

```rust
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<StorageDesign>,
```

New types (after `JobDesign`):

```rust
/// Contract v2: the top-level `storage` block — design-modeled object buckets.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageDesign {
    pub buckets: Vec<BucketDesign>,
}

/// One bucket: mounts at `/<name>` with generated guarded endpoints.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BucketDesign {
    /// `^[a-z][a-z0-9-]*$` — becomes the mount and the generated module ident.
    pub name: String,
    pub visibility: Visibility,
    /// Owning entity: the tenancy entity (tenant-owned bucket) or any other
    /// declared entity (the authenticated user id stamps owner_id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// Keys stored under `{owner_id}/…` with a prefix assertion on every
    /// access (Supabase folder-per-user parity). Requires `owner`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub owner_prefix: bool,
    /// Per-object cap, e.g. "5MB" (^[0-9]+(B|KB|MB|GB)?$). Default 50MB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_size: Option<String>,
    /// Content-type allowlist (globs like "image/*"). Empty = allow all.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_mime: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    Public,
    Private,
}
```

Add to `impl Design` (after `wants_jobs`):

```rust
    /// A declared storage block (with buckets) switches on the generated
    /// `crates/storage/` crate, the Storage extension + STORAGE_MIGRATIONS
    /// wiring in main.rs, and the `storage-s3` facade feature. Storage requires
    /// `db` (metadata table) and an active auth model (mutations are always
    /// guarded); validation rejects designs missing either.
    pub fn wants_storage(&self) -> bool {
        self.storage.as_ref().is_some_and(|s| !s.buckets.is_empty())
    }

    /// "5MB" → bytes. Uppercase B/KB/MB/GB suffixes (binary multiples); a bare
    /// number is bytes. None = unparseable (a validation question).
    pub fn parse_size(s: &str) -> Option<u64> {
        let (num, mult) = if let Some(n) = s.strip_suffix("GB") {
            (n, 1024 * 1024 * 1024)
        } else if let Some(n) = s.strip_suffix("MB") {
            (n, 1024 * 1024)
        } else if let Some(n) = s.strip_suffix("KB") {
            (n, 1024)
        } else if let Some(n) = s.strip_suffix('B') {
            (n, 1)
        } else {
            (s, 1)
        };
        num.parse::<u64>().ok().map(|n| n * mult)
    }
```

In `facade_features()`, after the `oauth` push (append-last keeps existing feature order byte-stable):

```rust
        // Appended after oauth so existing designs' feature order is unchanged.
        // storage-s3 (implies storage): the S3 backend must be compiled into
        // every storage app so JERRYCAN_STORAGE switches backends by env alone.
        if self.wants_storage() {
            features.push("storage-s3");
        }
```

- [ ] 4. Update `docs/contracts/design-schema.json`: change `"contract_version": { "enum": [0, 1] }` to `[0, 1, 2]`, extend the top-level `description` with "Contract v2 adds the top-level storage block (design-modeled object buckets); v0/v1 documents remain valid.", and add to `properties` after `jobs`:

```json
    "storage": {
      "type": "object",
      "additionalProperties": false,
      "required": ["buckets"],
      "description": "Contract v2: design-modeled object storage. Each bucket mounts guarded endpoints at /<name> (upload, list, download, delete, sign); object metadata lives in the storage_objects table; bytes live in the env-configured blob backend (JERRYCAN_STORAGE). Requires the db dependency and an active auth model.",
      "properties": {
        "buckets": {
          "type": "array",
          "minItems": 1,
          "items": {
            "type": "object",
            "additionalProperties": false,
            "required": ["name", "visibility"],
            "properties": {
              "name": {
                "type": "string",
                "pattern": "^[a-z][a-z0-9-]*$",
                "description": "Mounts at /<name>; must not collide with a module mount."
              },
              "visibility": {
                "enum": ["public", "private"],
                "description": "public = unauthenticated GET list/download (still metadata-tracked, cache-friendly); private = every endpoint guarded."
              },
              "owner": {
                "type": "string",
                "pattern": "^[A-Z][A-Za-z0-9]*$",
                "description": "Owning entity. The tenancy entity makes the bucket tenant-owned (Tenant-guard scoped); any other declared entity stamps the authenticated user id as owner_id. If the owner entity belongs_to the tenancy entity, objects are additionally tenant-isolated."
              },
              "owner_prefix": {
                "type": "boolean",
                "default": false,
                "description": "Store keys as {owner_id}/… and assert the first path segment on every access — the Supabase storage.foldername(name)[1] = auth.uid() pattern, made mechanical. Requires owner."
              },
              "max_size": {
                "type": "string",
                "pattern": "^[0-9]+(B|KB|MB|GB)?$",
                "description": "Per-object cap (over-limit is 413 JC0413). Default 50MB."
              },
              "allowed_mime": {
                "type": "array",
                "minItems": 1,
                "items": { "type": "string" },
                "description": "Content-type allowlist; image/* globs supported. Violation is 415 JC0415."
              }
            }
          }
        }
      }
    }
```

- [ ] 5. Run: `cargo test -p jerrycan platform::design` — expected PASS (new + updated tests). Full `cargo test -p jerrycan` still green (questions.rs untouched: a v2 doc now PARSES; validation gating lands next task — the only v2 fixture lives in design.rs tests, which don't call validate).
- [ ] 6. Commit: `Add design contract v2: storage block, wants_storage, storage-s3 facade feature`

## Task 15 — v2-gated storage validation in `questions.rs`

**Files:**
- Modify: `crates/jerrycan/src/platform/questions.rs`
- Test: in-module

- [ ] 1. Write the failing tests (questions.rs `mod tests`; import the fixture: `use crate::platform::design::tests::{MINIMAL, V1_FULL, V2_STORAGE};`):

```rust
    #[test]
    fn contract_version_2_is_now_valid_and_3_is_not() {
        let ok: Design = serde_json::from_str(V2_STORAGE).unwrap();
        assert!(
            !validate(&ok).iter().any(|q| q.id == "/contract_version"),
            "{:?}", validate(&ok)
        );
        let mut bad: Design = serde_json::from_str(V2_STORAGE).unwrap();
        bad.contract_version = 3;
        assert!(validate(&bad).iter().any(|q| q.id == "/contract_version"));
    }

    #[test]
    fn v2_storage_fixture_is_question_free() {
        let d: Design = serde_json::from_str(V2_STORAGE).unwrap();
        assert!(validate(&d).is_empty(), "{:?}", validate(&d));
    }

    #[test]
    fn storage_requires_contract_v2_db_and_an_active_auth_model() {
        // v1 + storage: rejected (v2 owns the block).
        let mut d: Design = serde_json::from_str(V2_STORAGE).unwrap();
        d.contract_version = 1;
        assert!(validate(&d).iter().any(|q| q.id == "/storage" && q.question.contains("contract_version 2")));
        // storage without db: rejected (metadata table).
        let mut d: Design = serde_json::from_str(V2_STORAGE).unwrap();
        d.dependencies.retain(|dep| dep != "db");
        assert!(validate(&d).iter().any(|q| q.id == "/storage" && q.question.contains("db")));
        // storage without an active auth model: rejected (mutations are always guarded).
        let mut d: Design = serde_json::from_str(V2_STORAGE).unwrap();
        d.auth = None;
        assert!(validate(&d).iter().any(|q| q.id == "/storage" && q.question.contains("auth")));
    }

    #[test]
    fn bucket_names_owners_and_rules_are_validated() {
        // Bad kebab name.
        let mut d: Design = serde_json::from_str(V2_STORAGE).unwrap();
        d.storage.as_mut().unwrap().buckets[0].name = "Avatars".into();
        assert!(validate(&d).iter().any(|q| q.id == "/storage/buckets/0/name"));
        // A name whose snake ident is a Rust keyword breaks the generated crate.
        let mut d: Design = serde_json::from_str(V2_STORAGE).unwrap();
        d.storage.as_mut().unwrap().buckets[0].name = "match".into();
        assert!(validate(&d).iter().any(|q| q.id == "/storage/buckets/0/name" && q.question.contains("keyword")));
        // Duplicate bucket names.
        let mut d: Design = serde_json::from_str(V2_STORAGE).unwrap();
        let dup = d.storage.as_ref().unwrap().buckets[0].clone();
        d.storage.as_mut().unwrap().buckets.push(dup);
        assert!(validate(&d).iter().any(|q| q.id == "/storage/buckets/2/name" && q.question.contains("unique")));
        // Unknown owner entity.
        let mut d: Design = serde_json::from_str(V2_STORAGE).unwrap();
        d.storage.as_mut().unwrap().buckets[0].owner = Some("Ghost".into());
        assert!(validate(&d).iter().any(|q| q.id == "/storage/buckets/0/owner" && q.question.contains("Ghost")));
        // owner_prefix without owner.
        let mut d: Design = serde_json::from_str(V2_STORAGE).unwrap();
        d.storage.as_mut().unwrap().buckets[1].owner = None;
        assert!(validate(&d).iter().any(|q| q.id == "/storage/buckets/1/owner_prefix"));
        // Unparseable max_size.
        let mut d: Design = serde_json::from_str(V2_STORAGE).unwrap();
        d.storage.as_mut().unwrap().buckets[0].max_size = Some("lots".into());
        assert!(validate(&d).iter().any(|q| q.id == "/storage/buckets/0/max_size"));
        // A mime entry that could break generated string literals.
        let mut d: Design = serde_json::from_str(V2_STORAGE).unwrap();
        d.storage.as_mut().unwrap().buckets[0].allowed_mime = vec!["image/\"png".into()];
        assert!(validate(&d).iter().any(|q| q.id == "/storage/buckets/0/allowed_mime/0"));
    }

    #[test]
    fn bucket_mounts_must_not_collide_with_module_mounts() {
        // WHY: buckets mount at /<name> beside the modules — a collision would
        // shadow routes silently at serve time.
        let mut d: Design = serde_json::from_str(V2_STORAGE).unwrap();
        d.storage.as_mut().unwrap().buckets[0].name = "orgs".into();
        assert!(validate(&d).iter().any(|q| q.id == "/storage/buckets/0/name" && q.question.contains("mount")));
    }
```

- [ ] 2. Run: `cargo test -p jerrycan platform::questions` — expected FAIL: `/contract_version` still rejects 2 (fixture not question-free); storage checks missing.
- [ ] 3. Implement in `validate()`. Change the version gate:

```rust
    if d.contract_version > 2 {
        qs.push(q(
            "/contract_version",
            "contract_version must be 0, 1, or 2 for this platform version.",
        ));
    }
```

Add the storage block AFTER the `entity_names` set is built (it references declared entities). Realtime will add its own sibling block here — keep this one self-contained:

```rust
    // Storage (contract v2). Bucket names/mime patterns are interpolated into
    // generated Rust literals and mounts, so everything is validated up front
    // (the job-queue precedent: reject at design time, not at generated-crate
    // build time). NOTE: `visibility: public` + a tenant-scoped owner is
    // deliberately allowed (public read, scoped write) — no question.
    if let Some(ref storage) = d.storage {
        if d.contract_version < 2 {
            qs.push(q(
                "/storage",
                "The storage block requires contract_version 2 — bump contract_version (v0/v1 designs stay valid without storage).",
            ));
        }
        if !d.wants_db() {
            qs.push(q(
                "/storage",
                "Storage requires a database dependency — add `db` to `dependencies` (object metadata lives in the storage_objects table).",
            ));
        }
        let active_auth_model = d
            .auth
            .as_ref()
            .map(|a| a.model != AuthModel::None)
            .unwrap_or(false);
        if !active_auth_model {
            qs.push(q(
                "/storage",
                "Storage requires an active auth model — bucket mutations (upload/delete/sign) are always guarded; set auth.model to `session` or `jwt`.",
            ));
        }
        let module_mounts: std::collections::HashSet<String> =
            d.modules.iter().map(|m| m.effective_mount()).collect();
        let mut seen_buckets = std::collections::HashSet::new();
        for (i, b) in storage.buckets.iter().enumerate() {
            let bptr = format!("/storage/buckets/{i}");
            if !is_kebab(&b.name) {
                qs.push(q(
                    format!("{bptr}/name"),
                    format!("Bucket `{}` is not kebab-case (^[a-z][a-z0-9-]*$).", b.name),
                ));
            }
            let ident = b.name.replace('-', "_");
            if RUST_KEYWORDS.contains(&ident.as_str()) {
                qs.push(q(
                    format!("{bptr}/name"),
                    format!("Bucket `{}` becomes the Rust module `{ident}`, which is a keyword — rename it.", b.name),
                ));
            }
            if !seen_buckets.insert(b.name.as_str()) {
                qs.push(q(
                    format!("{bptr}/name"),
                    format!("Bucket name `{}` is already used — bucket names must be unique.", b.name),
                ));
            }
            if module_mounts.contains(&format!("/{}", b.name)) {
                qs.push(q(
                    format!("{bptr}/name"),
                    format!("Bucket `{}` mounts at /{} which collides with a module mount — rename the bucket or remount the module.", b.name, b.name),
                ));
            }
            if let Some(ref owner) = b.owner
                && !entity_names.contains(owner.as_str())
            {
                qs.push(q(
                    format!("{bptr}/owner"),
                    format!("Bucket owner `{owner}` is not a declared entity anywhere in the design — define it or fix the reference."),
                ));
            }
            if b.owner_prefix && b.owner.is_none() {
                qs.push(q(
                    format!("{bptr}/owner_prefix"),
                    format!("Bucket `{}` sets owner_prefix without an owner — owner_prefix stores keys under {{owner_id}}/… and needs `owner`.", b.name),
                ));
            }
            if let Some(ref max) = b.max_size
                && Design::parse_size(max).is_none()
            {
                qs.push(q(
                    format!("{bptr}/max_size"),
                    format!("max_size `{max}` is not a size — use ^[0-9]+(B|KB|MB|GB)?$ (e.g. \"5MB\")."),
                ));
            }
            for (j, m) in b.allowed_mime.iter().enumerate() {
                let well_formed = m.split_once('/').is_some_and(|(t, sub)| {
                    let seg_ok = |s: &str| {
                        !s.is_empty()
                            && s.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'.' | b'+' | b'-'))
                    };
                    (seg_ok(t) || t == "*") && (seg_ok(sub) || sub == "*")
                });
                if !well_formed {
                    qs.push(q(
                        format!("{bptr}/allowed_mime/{j}"),
                        format!("`{m}` is not a mime pattern — use type/subtype or type/* (lowercase)."),
                    ));
                }
            }
        }
    }
```

- [ ] 4. Run: `cargo test -p jerrycan platform::questions` — expected PASS (existing tests untouched and green).
- [ ] 5. Commit: `Validate storage buckets in questions (contract v2 gated)`

## Task 16 — JC0415 registry text + error-code docs

**Files:**
- Modify: `crates/jerrycan/src/platform/codes.rs`, `docs/ai/13-error-codes.md`, `crates/jerrycan/embedded/ai/13-error-codes.md`
- Test: existing codes.rs tests stay green (the "no orphan codes" walk already covers jerrycan-storage's `unsupported_media_type` emission because JC0415 is registered)

- [ ] 1. Failing check first — a doc-shaped test in codes.rs:

```rust
    #[test]
    fn jc0415_covers_bucket_mime_allowlists() {
        // WHY: `jerrycan explain JC0415` is the agent's first stop when a
        // generated bucket rejects an upload — the registry must name the
        // allowlist cause, not just the Multipart boundary case.
        let info = lookup("JC0415").unwrap();
        assert!(info.cause.contains("allowed_mime"), "cause: {}", info.cause);
    }
```

- [ ] 2. Run: `cargo test -p jerrycan jc0415` — expected FAIL (cause text doesn't mention allowlists).
- [ ] 3. Implement — extend the existing JC0415 `CodeInfo` in `codes.rs`:

```rust
    CodeInfo {
        code: "JC0415",
        title: "unsupported media type",
        cause: "the request's content type is not what the endpoint consumes: Multipart requires multipart/form-data with a boundary, and a storage bucket upload must match the bucket's allowed_mime allowlist",
        fix: "send the content type the endpoint declares; for uploads, multipart/form-data with a valid boundary parameter, or a Content-Type inside the bucket's allowed_mime list",
        doc: "jerrycan docs extractors",
    },
```

Update the JC0415 row in `docs/ai/13-error-codes.md` (line 17) to:

```markdown
| JC0415 | Unsupported media type — content type is not what the endpoint consumes (e.g. `Multipart` needs `multipart/form-data` with a boundary; a storage bucket upload must match the bucket's `allowed_mime` allowlist) |
```

Copy the changed file byte-identically to `crates/jerrycan/embedded/ai/13-error-codes.md` (the embedded mirror served by `jerrycan docs`):

```
cp docs/ai/13-error-codes.md crates/jerrycan/embedded/ai/13-error-codes.md
```

- [ ] 4. Run: `cargo test -p jerrycan platform::codes` — expected PASS (including `every_emitted_code_is_in_the_registry`, which now also sees jerrycan-storage's sources).
- [ ] 5. Commit: `Extend JC0415 registry text and docs for bucket mime allowlists`

## Task 17 — `storagegen.rs`: the generated `crates/storage/` crate (modules + handlers)

**Files:**
- Create: `crates/jerrycan/src/platform/storagegen.rs`
- Modify: `crates/jerrycan/src/platform/mod.rs` (add `pub mod storagegen;` alongside `jobsgen`)
- Test: in-module (generated-string assertions + determinism, mirroring jobsgen's tests)

- [ ] 1. Write the failing tests (bottom of the new `storagegen.rs`; the fixture is design.rs's `V2_STORAGE`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::design::tests::V2_STORAGE;

    fn design() -> Design {
        serde_json::from_str(V2_STORAGE).unwrap()
    }

    fn bucket<'a>(d: &'a Design, name: &str) -> &'a BucketDesign {
        d.storage.as_ref().unwrap().buckets.iter().find(|b| b.name == name).unwrap()
    }

    #[test]
    fn generation_is_deterministic_and_lib_declares_sorted_buckets() {
        let d = design();
        assert_eq!(lib_rs(&d), lib_rs(&d), "byte-identical across runs (JL0003)");
        assert_eq!(bucket_rs(&d, bucket(&d, "avatars")), bucket_rs(&d, bucket(&d, "avatars")));
        let lib = lib_rs(&d);
        let a = lib.find("pub mod avatars;").unwrap();
        let i = lib.find("pub mod invoices;").unwrap();
        assert!(a < i, "bucket modules sorted by name: {lib}");
        assert!(lib.contains("GENERATED by jerrycan"), "tool-owned banner: {lib}");
    }

    /// avatars: public, owner: User (plain user scope), 5MB, image/*.
    #[test]
    fn user_owned_public_bucket_emits_the_right_guards_and_limits() {
        let d = design();
        let m = bucket_rs(&d, bucket(&d, "avatars"));
        // The design-derived const.
        assert!(m.contains("name: \"avatars\"") && m.contains("public: true"), "{m}");
        assert!(m.contains("owner_prefix: false") && m.contains("max_size: 5242880"), "{m}");
        assert!(m.contains("allowed_mime: &[\"image/*\"]"), "{m}");
        // Route table: body_limit = max_size; all five endpoints.
        assert!(m.contains(".route(\"/\", get(list).post(upload).body_limit(5242880))"), "{m}");
        assert!(m.contains(".route(\"/{id}\", get(download).delete(remove))"), "{m}");
        assert!(m.contains(".route(\"/{id}/sign\", post(sign))"), "{m}");
        // Mutations guarded by the session user; owner_id = user id.
        assert!(m.contains("async fn upload(storage: Dep<Storage>, db: Dep<Db>, user: CurrentUser,"), "{m}");
        assert!(m.contains("Scope { owner_id: Some(user.0.id.to_string()), tenant_id: None }"), "{m}");
        // Public reads: download/list take NO guard, and download is cacheable.
        assert!(m.contains("async fn download(storage: Dep<Storage>, db: Dep<Db>, Path(id): Path<String>)"), "{m}");
        assert!(m.contains("async fn list(storage: Dep<Storage>, db: Dep<Db>)"), "{m}");
        assert!(m.contains("object_response(&meta, bytes, true)"), "{m}");
        // No tenant machinery on a plain user bucket.
        assert!(!m.contains("Tenant"), "{m}");
    }

    /// invoices: private, owner: Org == tenancy.entity (tenant scope), prefix.
    #[test]
    fn tenant_owned_private_prefix_bucket_takes_the_tenant_guard() {
        let d = design();
        let m = bucket_rs(&d, bucket(&d, "invoices"));
        assert!(m.contains("public: false") && m.contains("owner_prefix: true"), "{m}");
        assert!(m.contains("use shared::Tenant;"), "{m}");
        assert!(m.contains("async fn upload(storage: Dep<Storage>, db: Dep<Db>, tenant: Dep<Tenant>,"), "{m}");
        assert!(
            m.contains("Scope { owner_id: Some(tenant.id().to_string()), tenant_id: Some(tenant.id().to_string()) }"),
            "{m}"
        );
        // Private download: optional guard OR a signed URL, both paths present.
        assert!(m.contains("tenant: Option<Dep<Tenant>>"), "{m}");
        assert!(m.contains("get_signed(&db, &BUCKET, &id, exp, sig,"), "{m}");
        assert!(m.contains("ok_or_else(Error::unauthorized)?"), "{m}");
        // Private list is scoped.
        assert!(m.contains("async fn list(storage: Dep<Storage>, db: Dep<Db>, tenant: Dep<Tenant>,"), "{m}");
        // Default max on the missing allowed_mime: allow-all.
        assert!(m.contains("allowed_mime: &[]"), "{m}");
        assert!(m.contains("max_size: 20971520"), "{m}");
    }

    /// A user-owned bucket whose owner entity belongs_to the tenant takes BOTH
    /// guards: owner_id = user id, tenant_id = tenant id.
    #[test]
    fn user_in_tenant_bucket_takes_both_guards() {
        let mut d = design();
        // Graft: User belongs_to Org, and repoint avatars' semantics.
        d.modules[0].entities[1].belongs_to = vec![serde_json::from_str(r#"{ "entity": "Org" }"#).unwrap()];
        let m = bucket_rs(&d, &d.storage.as_ref().unwrap().buckets[0].clone());
        assert!(m.contains("user: CurrentUser, tenant: Dep<Tenant>,"), "{m}");
        assert!(
            m.contains("Scope { owner_id: Some(user.0.id.to_string()), tenant_id: Some(tenant.id().to_string()) }"),
            "{m}"
        );
    }

    #[test]
    fn cargo_toml_depends_on_the_facade_and_shared() {
        let c = cargo_toml();
        assert!(c.contains("name = \"storage\"") && c.contains("jerrycan.workspace = true"), "{c}");
        assert!(c.contains("shared = { path = \"../shared\" }"), "{c}");
    }
}
```

- [ ] 2. Run: `cargo test -p jerrycan platform::storagegen` — expected FAIL: module doesn't exist. Add `pub mod storagegen;` to `crates/jerrycan/src/platform/mod.rs` (next to `jobsgen`), then FAIL becomes unresolved generator fns.
- [ ] 3. Implement the generator:

```rust
//! Storage-bucket crate generation: `crates/storage/` with one bucket module
//! per design bucket. EVERYTHING here is TOOL-owned and rewritten on every
//! generate — a bucket's surface is fully deterministic from the design
//! (Rule 5: modeling + access control + endpoint generation is code, not
//! agent judgment), so unlike route crates there are no agent-owned stubs.
//! The heavy lifting (metadata SQL, checksums, signing, blob IO) lives in
//! jerrycan-storage; the generated handlers only resolve the principal into a
//! `Scope` and delegate.

use super::design::{BucketDesign, Design, Entity, ModuleDesign, Visibility};
use std::fs;
use std::path::Path;

/// Default per-object cap when the bucket omits max_size: 50 MiB — Supabase's
/// default file_size_limit, so migrated buckets behave the same.
const DEFAULT_MAX_SIZE: u64 = 50 * 1024 * 1024;

/// bucket kebab name → module ident (`user-files` → `user_files`).
fn bucket_ident(name: &str) -> String {
    name.replace('-', "_")
}

/// Buckets sorted by name — every emission site (lib.rs, mounts, tests)
/// iterates in this order so output is byte-stable.
fn sorted_buckets(design: &Design) -> Vec<&BucketDesign> {
    let mut buckets: Vec<&BucketDesign> = design
        .storage
        .as_ref()
        .map(|s| s.buckets.iter().collect())
        .unwrap_or_default();
    buckets.sort_by(|a, b| a.name.cmp(&b.name));
    buckets
}

/// How a bucket resolves its principal into a Scope.
#[derive(Clone, Copy, PartialEq)]
enum BucketScope {
    /// No owner: mutations auth-gated, rows unscoped.
    Unowned,
    /// owner = a non-tenant entity: the session user id stamps owner_id.
    User,
    /// owner = the tenancy entity: the Tenant guard's id stamps owner+tenant.
    Tenant,
    /// owner belongs_to the tenancy entity: user id + tenant id.
    UserInTenant,
}

fn find_entity<'a>(m: &'a ModuleDesign, name: &str) -> Option<&'a Entity> {
    m.entities
        .iter()
        .find(|e| e.name == name)
        .or_else(|| m.subroutes.iter().find_map(|s| find_entity(s, name)))
}

fn owner_belongs_to_tenant(design: &Design, owner: &str, tenant: &str) -> bool {
    design
        .modules
        .iter()
        .find_map(|m| find_entity(m, owner))
        .is_some_and(|e| e.belongs_to.iter().any(|b| b.entity == tenant))
}

fn bucket_scope(design: &Design, b: &BucketDesign) -> BucketScope {
    match (&b.owner, design.tenancy.as_ref()) {
        (None, _) => BucketScope::Unowned,
        (Some(o), Some(t)) if *o == t.entity => BucketScope::Tenant,
        (Some(o), Some(t)) if owner_belongs_to_tenant(design, o, &t.entity) => BucketScope::UserInTenant,
        (Some(_), _) => BucketScope::User,
    }
}

/// The per-scope code fragments. Every string is valid Rust in its slot;
/// guard params end with ", " so they compose before other params (a trailing
/// comma in a fn signature is legal where a fragment lands last).
struct ScopeFragments {
    guard_params: &'static str,
    opt_guard_params: &'static str,
    scope_expr: &'static str,
    opt_guard_bind: &'static str,
    use_user: bool,
    use_tenant: bool,
}

fn fragments(scope: BucketScope) -> ScopeFragments {
    match scope {
        BucketScope::Unowned => ScopeFragments {
            guard_params: "_user: CurrentUser, ",
            opt_guard_params: "user: Option<CurrentUser>, ",
            scope_expr: "Scope::default()",
            opt_guard_bind: "let _user = user.ok_or_else(Error::unauthorized)?;\n    let scope = Scope::default();",
            use_user: true,
            use_tenant: false,
        },
        BucketScope::User => ScopeFragments {
            guard_params: "user: CurrentUser, ",
            opt_guard_params: "user: Option<CurrentUser>, ",
            scope_expr: "Scope { owner_id: Some(user.0.id.to_string()), tenant_id: None }",
            opt_guard_bind: "let user = user.ok_or_else(Error::unauthorized)?;\n    let scope = Scope { owner_id: Some(user.0.id.to_string()), tenant_id: None };",
            use_user: true,
            use_tenant: false,
        },
        BucketScope::Tenant => ScopeFragments {
            guard_params: "tenant: Dep<Tenant>, ",
            opt_guard_params: "tenant: Option<Dep<Tenant>>, ",
            scope_expr: "Scope { owner_id: Some(tenant.id().to_string()), tenant_id: Some(tenant.id().to_string()) }",
            opt_guard_bind: "let tenant = tenant.ok_or_else(Error::unauthorized)?;\n    let scope = Scope { owner_id: Some(tenant.id().to_string()), tenant_id: Some(tenant.id().to_string()) };",
            use_user: false,
            use_tenant: true,
        },
        BucketScope::UserInTenant => ScopeFragments {
            guard_params: "user: CurrentUser, tenant: Dep<Tenant>, ",
            opt_guard_params: "user: Option<CurrentUser>, tenant: Option<Dep<Tenant>>, ",
            scope_expr: "Scope { owner_id: Some(user.0.id.to_string()), tenant_id: Some(tenant.id().to_string()) }",
            opt_guard_bind: "let user = user.ok_or_else(Error::unauthorized)?;\n    let tenant = tenant.ok_or_else(Error::unauthorized)?;\n    let scope = Scope { owner_id: Some(user.0.id.to_string()), tenant_id: Some(tenant.id().to_string()) };",
            use_user: true,
            use_tenant: true,
        },
    }
}

/// The tool-owned `crates/storage/Cargo.toml`. `shared` carries CurrentUser/
/// Tenant (storage designs always have an active auth model — validated).
pub fn cargo_toml() -> String {
    "[package]\nname = \"storage\"\nversion.workspace = true\nedition.workspace = true\npublish = false\n\n[dependencies]\njerrycan.workspace = true\nserde.workspace = true\nserde_json.workspace = true\nshared = { path = \"../shared\" }\n\n[dev-dependencies]\ntokio.workspace = true\n".to_string()
}

/// The tool-owned `src/lib.rs`: one `pub mod` per bucket, sorted.
pub fn lib_rs(design: &Design) -> String {
    let mods: String = sorted_buckets(design)
        .iter()
        .map(|b| format!("pub mod {};\n", bucket_ident(&b.name)))
        .collect();
    format!(
        "//! GENERATED by jerrycan — storage bucket modules from design.json's `storage`\n//! block. TOOL-OWNED: `jerrycan generate` rewrites every file in this crate\n//! (bucket behavior is fully deterministic from the design; there is no agent\n//! judgment in a bucket's surface — custom logic belongs in route modules).\n#![forbid(unsafe_code)]\n\n{mods}"
    )
}

/// One TOOL-OWNED bucket module: the design-derived `BUCKET` const, `module()`,
/// and the five handlers (upload/list/download/remove/sign).
pub(crate) fn bucket_rs(design: &Design, b: &BucketDesign) -> String {
    let name = &b.name;
    let public = matches!(b.visibility, Visibility::Public);
    let max_size = b
        .max_size
        .as_deref()
        .and_then(Design::parse_size)
        .unwrap_or(DEFAULT_MAX_SIZE);
    let mime_list = b
        .allowed_mime
        .iter()
        .map(|m| format!("\"{m}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let f = fragments(bucket_scope(design, b));

    let mut uses = String::from(
        "use jerrycan::Response;\nuse jerrycan::db::Db;\nuse jerrycan::prelude::*;\nuse jerrycan::storage::{Bucket, ObjectMeta, Scope, SignedUrl, Storage};\n",
    );
    if f.use_user {
        uses.push_str("use shared::CurrentUser;\n");
    }
    if f.use_tenant {
        uses.push_str("use shared::Tenant;\n");
    }

    let guard = f.guard_params;
    let scope_expr = f.scope_expr;

    // Public reads: open list + a plain cacheable download. Private reads:
    // scoped list + a download that accepts a session OR a signed URL.
    let read_handlers = if public {
        format!(
            r#"/// GET / — list (open: public bucket), ordered by key.
pub(crate) async fn list(storage: Dep<Storage>, db: Dep<Db>) -> Result<Json<Vec<ObjectMeta>>> {{
    Ok(Json(storage.list_objects(&db, &BUCKET, None).await?))
}}

/// GET /{{id}} — download (open: public bucket). Emits `ETag` (the sha256
/// checksum) + a cache-friendly `Cache-Control`.
pub(crate) async fn download(storage: Dep<Storage>, db: Dep<Db>, Path(id): Path<String>) -> Result<Response> {{
    let (meta, bytes) = storage.get_object(&db, &BUCKET, None, &id).await?;
    jerrycan::storage::object_response(&meta, bytes, true)
}}
"#
        )
    } else {
        format!(
            r#"#[derive(serde::Deserialize)]
struct GetQuery {{
    #[serde(default)]
    exp: Option<u64>,
    #[serde(default)]
    sig: Option<String>,
}}

/// GET / — list, scoped to the caller (a foreign row never appears).
pub(crate) async fn list(storage: Dep<Storage>, db: Dep<Db>, {guard}) -> Result<Json<Vec<ObjectMeta>>> {{
    let scope = {scope_expr};
    Ok(Json(storage.list_objects(&db, &BUCKET, Some(&scope)).await?))
}}

/// GET /{{id}} — download: a scoped session OR a valid `exp`/`sig` pair (the
/// app-HMAC signed URL). A missing/failed guard on the session path reads as
/// 401 (this route's credential can also be the URL itself).
pub(crate) async fn download(storage: Dep<Storage>, db: Dep<Db>, {opt_guard}Path(id): Path<String>, Query(q): Query<GetQuery>) -> Result<Response> {{
    if let (Some(exp), Some(sig)) = (q.exp, q.sig.as_deref()) {{
        let (meta, bytes) = storage.get_signed(&db, &BUCKET, &id, exp, sig, std::time::SystemTime::now()).await?;
        return jerrycan::storage::object_response(&meta, bytes, false);
    }}
    {opt_guard_bind}
    let (meta, bytes) = storage.get_object(&db, &BUCKET, Some(&scope), &id).await?;
    jerrycan::storage::object_response(&meta, bytes, false)
}}
"#,
            opt_guard = f.opt_guard_params,
            opt_guard_bind = f.opt_guard_bind,
        )
    };

    format!(
        r#"//! GENERATED by jerrycan — storage bucket `{name}`. TOOL-OWNED: regenerated
//! by `jerrycan generate`; do not hand-edit (custom logic belongs in route
//! modules, not here).
{uses}
/// Design-derived rules for bucket `{name}` (contract v2 `storage` block).
const BUCKET: Bucket = Bucket {{
    name: "{name}",
    public: {public},
    owner_prefix: {owner_prefix},
    max_size: {max_size},
    allowed_mime: &[{mime_list}],
}};

/// This bucket's routes. `body_limit` = the bucket's max_size, so an
/// over-limit upload is the framework's 413 JC0413 before the handler runs.
pub fn module() -> Module {{
    Module::new("storage-{name}")
        .route("/", get(list).post(upload).body_limit({max_size}))
        .route("/{{id}}", get(download).delete(remove))
        .route("/{{id}}/sign", post(sign))
}}

#[derive(serde::Deserialize)]
struct UploadQuery {{
    key: String,
}}

#[derive(serde::Deserialize)]
struct SignQuery {{
    #[serde(default = "default_ttl")]
    ttl: u64,
}}

fn default_ttl() -> u64 {{
    300
}}

/// POST /?key=<path> — upload a raw body; the request `Content-Type` is the
/// stored mime (415 JC0415 outside `allowed_mime`); duplicate key = 409.
pub(crate) async fn upload(storage: Dep<Storage>, db: Dep<Db>, {guard}headers: Headers, Query(q): Query<UploadQuery>, RawBody(body): RawBody) -> Result<Created<ObjectMeta>> {{
    let mime = headers.get("content-type").unwrap_or("application/octet-stream").to_string();
    let scope = {scope_expr};
    Ok(Created(storage.put_object(&db, &BUCKET, &scope, &q.key, &mime, body).await?))
}}

{read_handlers}
/// DELETE /{{id}} — delete row + bytes, scoped (a foreign object is a 404).
pub(crate) async fn remove(storage: Dep<Storage>, db: Dep<Db>, {guard}Path(id): Path<String>) -> Result<NoContent> {{
    let scope = {scope_expr};
    storage.delete_object(&db, &BUCKET, &scope, &id).await?;
    Ok(NoContent)
}}

/// POST /{{id}}/sign?ttl=<secs> — a time-limited URL: native S3 presign when
/// the backend supports it, else app-HMAC. TTL clamps to 24h in the service.
pub(crate) async fn sign(storage: Dep<Storage>, db: Dep<Db>, {guard}Path(id): Path<String>, Query(q): Query<SignQuery>) -> Result<Json<SignedUrl>> {{
    let scope = {scope_expr};
    Ok(Json(storage.sign_object(&db, &BUCKET, &scope, &id, q.ttl, std::time::SystemTime::now()).await?))
}}
"#,
        owner_prefix = b.owner_prefix,
    )
}
```

- [ ] 4. Run: `cargo test -p jerrycan platform::storagegen` — expected PASS (5 tests).
- [ ] 5. Commit: `Add storagegen: per-bucket storage crate generation`

## Task 18 — Generated acceptance + isolation tests, and `write_storage`

**Files:**
- Modify: `crates/jerrycan/src/platform/storagegen.rs`, `crates/jerrycan/src/platform/testgen.rs` (visibility only: `fn tenant_row_cols_vals` → `pub(crate) fn`, so the tenant-row seed logic is shared, not duplicated)
- Test: in-module

- [ ] 1. Write the failing tests (extend storagegen's `mod tests`):

```rust
    #[test]
    fn acceptance_covers_round_trip_guards_and_negative_controls() {
        let d = design();
        let a = acceptance_rs(&d);
        assert_eq!(a, acceptance_rs(&d), "deterministic");
        // Shared app() plumbing: memory blob store + storage migrations + auth
        // + the tenant guard (invoices is tenant-owned).
        assert!(a.contains("jerrycan::storage::Storage::memory().with_sign_secret(TEST_SECRET)"), "{a}");
        assert!(a.contains("db.migrate(jerrycan::storage::STORAGE_MIGRATIONS)"), "{a}");
        assert!(a.contains(".provide_dep(shared::tenant)"), "{a}");
        assert!(a.contains(".mount(\"/avatars\", storage::avatars::module())"), "{a}");
        assert!(a.contains(".mount(\"/invoices\", storage::invoices::module())"), "{a}");
        // Tenant seeds: two tenants, two memberships (isolation acts as user 2).
        assert!(a.contains("INSERT INTO \\\"org_members\\\" (user_id, org_id, role) VALUES (1, 1, 'owner')"), "{a}");
        assert!(a.contains("INSERT INTO \\\"org_members\\\" (user_id, org_id, role) VALUES (2, 2, 'owner')"), "{a}");
        // Per-bucket surface tests.
        for needle in [
            "async fn avatars_upload_then_download_round_trips()",
            "async fn avatars_upload_without_auth_is_401()",
            "async fn avatars_disallowed_mime_is_415()",
            "async fn avatars_oversize_upload_is_413()",
            "async fn avatars_cross_owner_access_is_denied()",
            "async fn avatars_signed_url_grants_and_rejects()",
            "async fn invoices_download_without_auth_is_401()",
            "async fn invoices_cross_owner_access_is_denied()",
            "async fn invoices_owner_prefix_isolates_keys()",
            "async fn invoices_signed_url_grants_and_rejects()",
        ] {
            assert!(a.contains(needle), "missing {needle} in:\n{a}");
        }
        // The negative controls encode the SECURITY contract: cross-owner GET
        // 404s and the row survives a cross-owner DELETE.
        assert!(a.contains("cross-owner delete must 404"), "{a}");
        assert!(a.contains("\"2/same.bin\""), "prefix control asserts B's prefixed key: {a}");
        // Public bucket: cache headers asserted; no download-401 test.
        assert!(a.contains("\"public, max-age=3600\""), "{a}");
        assert!(!a.contains("avatars_download_without_auth_is_401"), "{a}");
        // Private bucket: tampered signed URL is rejected.
        assert!(a.contains("invoices tampered signed URL"), "{a}");
    }

    #[test]
    fn write_storage_writes_everything_tool_owned() {
        let tmp = tempfile::tempdir().unwrap();
        let d = design();
        let created = write_storage(tmp.path(), &d).unwrap();
        for rel in [
            "crates/storage/Cargo.toml",
            "crates/storage/src/lib.rs",
            "crates/storage/src/avatars.rs",
            "crates/storage/src/invoices.rs",
            "crates/storage/tests/acceptance.rs",
        ] {
            assert!(created.contains(&rel.to_string()), "{created:?}");
            assert!(tmp.path().join(rel).is_file(), "{rel} on disk");
        }
        // Ownership rule: EVERY file is tool-owned — a hand-edit is restored
        // (unlike route crates, no agent-owned files exist here).
        let handler = tmp.path().join("crates/storage/src/avatars.rs");
        fs::write(&handler, "// hand edit\n").unwrap();
        write_storage(tmp.path(), &d).unwrap();
        assert!(
            fs::read_to_string(&handler).unwrap().contains("const BUCKET"),
            "tool-owned bucket module restored"
        );
    }
```

- [ ] 2. Run: `cargo test -p jerrycan platform::storagegen` — expected FAIL: `acceptance_rs`/`write_storage` unresolved.
- [ ] 3. Implement. First flip `testgen.rs`'s `fn tenant_row_cols_vals(...)` to `pub(crate) fn tenant_row_cols_vals(...)` (one-word diff — the seed-column logic must not fork). Then in `storagegen.rs`:

```rust
/// The module owning the design's tenancy entity (its migration carries the
/// `{tenant}_members` table the Tenant guard queries).
fn tenant_module<'a>(design: &'a Design) -> Option<&'a ModuleDesign> {
    let tenancy = design.tenancy.as_ref()?;
    design
        .modules
        .iter()
        .find(|m| m.entities.iter().any(|e| e.name == tenancy.entity))
}

/// A concrete Content-Type the bucket accepts (for generated happy paths):
/// the first allowlist entry with `*` resolved, or octet-stream.
fn concrete_mime(b: &BucketDesign) -> String {
    match b.allowed_mime.first().map(String::as_str) {
        None | Some("*/*") => "application/octet-stream".to_string(),
        Some(pat) => match pat.strip_suffix("/*") {
            Some("image") => "image/png".to_string(),
            Some(prefix) => format!("{prefix}/test"),
            None => pat.to_string(),
        },
    }
}

/// The tool-owned `crates/storage/tests/acceptance.rs`: per-bucket round
/// trips, guard checks, and the isolation NEGATIVE CONTROLS (cross-owner,
/// cross-tenant via memberships, cross-prefix). These PASS on generation
/// (handlers are real, not stubs) and turn RED if any scope is broken —
/// gen-tests does NOT count them toward expected_failing.
pub fn acceptance_rs(design: &Design) -> String {
    let buckets = sorted_buckets(design);
    let needs_tenant = buckets
        .iter()
        .any(|b| matches!(bucket_scope(design, b), BucketScope::Tenant | BucketScope::UserInTenant));

    // Tenant plumbing (migration include + tenant 1/2 + membership seeds),
    // reusing testgen's column/value derivation so the two seeds can't drift.
    let (seed_use, tenant_setup, tenant_dep) = if needs_tenant {
        let tenancy = design.tenancy.as_ref().expect("validated: tenant buckets require tenancy");
        let t = tenant_module(design).expect("validated: tenancy entity is declared");
        let entity = t
            .entities
            .iter()
            .find(|e| e.name == tenancy.entity)
            .expect("validated: tenancy entity in its module");
        let t_snake = t.name.replace('-', "_");
        let table = format!("{}s", tenancy.entity.to_lowercase());
        let members = format!("{}_members", Design::to_snake(&tenancy.entity));
        let fk = Design::fk_column(&tenancy.entity);
        let role = tenancy.member_roles.first().map(String::as_str).unwrap_or("owner");
        let (cols1, vals1) = super::testgen::tenant_row_cols_vals(entity, "1", 1);
        let (cols2, vals2) = super::testgen::tenant_row_cols_vals(entity, "2", 2);
        let setup = format!(
            "    db.migrate(&[\n        jerrycan::db::Migration {{\n            name: \"{t_snake}_0001_create_tables\",\n            sqlite: include_str!(\"../../routes/{t_name}/migrations/sqlite/0001_create_tables.sql\"),\n            postgres: include_str!(\"../../routes/{t_name}/migrations/postgres/0001_create_tables.sql\"),\n        }},\n    ])\n    .await\n    .expect(\"tenant migrations\");\n    db.conn().execute_unprepared(\"INSERT INTO \\\"{table}\\\" ({cols1}) VALUES ({vals1})\").await.expect(\"seed tenant 1\");\n    db.conn().execute_unprepared(\"INSERT INTO \\\"{table}\\\" ({cols2}) VALUES ({vals2})\").await.expect(\"seed tenant 2\");\n    db.conn().execute_unprepared(\"INSERT INTO \\\"{members}\\\" (user_id, {fk}, role) VALUES (1, 1, '{role}')\").await.expect(\"seed membership 1\");\n    db.conn().execute_unprepared(\"INSERT INTO \\\"{members}\\\" (user_id, {fk}, role) VALUES (2, 2, '{role}')\").await.expect(\"seed membership 2\");\n",
            t_name = t.name,
        );
        (
            "use jerrycan::db::sea_orm::ConnectionTrait;\n\n".to_string(),
            setup,
            "        .provide_dep(shared::tenant)\n".to_string(),
        )
    } else {
        (String::new(), String::new(), String::new())
    };

    let mounts: String = buckets
        .iter()
        .map(|b| format!("        .mount(\"/{}\", storage::{}::module())\n", b.name, bucket_ident(&b.name)))
        .collect();

    let mut tests = String::new();
    for b in &buckets {
        bucket_tests(design, b, &mut tests);
    }

    format!(
        "//! GENERATED by jerrycan — TOOL-OWNED storage acceptance + isolation tests\n\
         //! from design.json's `storage` block. These pass on generation (bucket\n\
         //! handlers are real implementations) and turn RED if any owner/tenant/\n\
         //! prefix scope is broken — the negative controls ARE the security gate.\n\
         //! Regenerated on demand; add your own tests in sibling files, not here.\n\
         use jerrycan::prelude::*;\n\n\
         {seed_use}\
         const TEST_SECRET: &str = \"a-very-long-development-secret-string!!\";\n\n\
         fn test_cookie_for(user_id: i64) -> String {{\n\
         \x20   let auth = jerrycan::auth::Auth::with_secret(TEST_SECRET);\n\
         \x20   let token = auth.sessions().encode(&shared::SessionUser {{ id: user_id, role: \"admin\".into() }}).expect(\"encode\");\n\
         \x20   format!(\"jerrycan_session={{token}}\")\n\
         }}\n\n\
         async fn app() -> TestApp {{\n\
         \x20   let db = jerrycan::db::Db::connect(\"sqlite::memory:\").await.expect(\"test db\");\n\
         \x20   db.migrate(jerrycan::storage::STORAGE_MIGRATIONS).await.expect(\"storage migrations\");\n\
         {tenant_setup}\
         \x20   App::new()\n\
         \x20       .extend(jerrycan::auth::Auth::with_secret(TEST_SECRET))\n\
         \x20       .extend(jerrycan::storage::Storage::memory().with_sign_secret(TEST_SECRET))\n\
         \x20       .extend(db)\n\
         {tenant_dep}\
         {mounts}\
         \x20       .into_test()\n\
         }}\n\n\
         {tests}"
    )
}

/// The per-bucket test battery. Emission is conditional on the bucket's shape;
/// every emitted body is complete (no agent TODOs — the surface is generated).
fn bucket_tests(design: &Design, b: &BucketDesign, out: &mut String) {
    let name = &b.name;
    let ident = bucket_ident(name);
    let public = matches!(b.visibility, Visibility::Public);
    let owned = bucket_scope(design, b) != BucketScope::Unowned;
    let mime = concrete_mime(b);
    let cache = if public { "public, max-age=3600" } else { "private, no-store" };

    // 1. Round trip + ETag/Cache-Control (headers checked on every bucket).
    let download_1 = if public {
        format!("t.get(&format!(\"/{name}/{{id}}\")).await")
    } else {
        format!("t.get_with(&format!(\"/{name}/{{id}}\"), &[(\"cookie\", &test_cookie_for(1))]).await")
    };
    out.push_str(&format!(
        "#[tokio::test]\nasync fn {ident}_upload_then_download_round_trips() {{\n    let t = app().await;\n    let created = t.post_bytes_with(\"/{name}?key=probe.bin\", b\"{ident}-bytes\", &[(\"cookie\", &test_cookie_for(1)), (\"content-type\", \"{mime}\")]).await;\n    assert_eq!(created.status().as_u16(), 201, \"upload; body: {{}}\", created.text());\n    let meta: serde_json::Value = serde_json::from_str(&created.text()).expect(\"meta json\");\n    let id = meta[\"id\"].as_str().expect(\"id\").to_string();\n    let checksum = meta[\"checksum\"].as_str().expect(\"checksum\").to_string();\n    let res = {download_1};\n    assert_eq!(res.status().as_u16(), 200, \"download; body: {{}}\", res.text());\n    assert_eq!(res.bytes(), &b\"{ident}-bytes\"[..]);\n    let etag = res.headers().get(\"etag\").and_then(|v| v.to_str().ok()).expect(\"etag header\");\n    assert_eq!(etag, format!(\"\\\"{{checksum}}\\\"\"), \"ETag is the sha256 checksum\");\n    let cc = res.headers().get(\"cache-control\").and_then(|v| v.to_str().ok()).expect(\"cache-control header\");\n    assert_eq!(cc, \"{cache}\");\n}}\n\n"
    ));

    // 2. Mutations are always guarded.
    out.push_str(&format!(
        "#[tokio::test]\nasync fn {ident}_upload_without_auth_is_401() {{\n    let t = app().await;\n    let res = t.post_bytes_with(\"/{name}?key=noauth.bin\", b\"x\", &[(\"content-type\", \"{mime}\")]).await;\n    assert_eq!(res.status().as_u16(), 401, \"mutations are always guarded; body: {{}}\", res.text());\n}}\n\n"
    ));

    // 3. Private reads are guarded.
    if !public {
        out.push_str(&format!(
            "#[tokio::test]\nasync fn {ident}_download_without_auth_is_401() {{\n    let t = app().await;\n    let created = t.post_bytes_with(\"/{name}?key=guard.bin\", b\"x\", &[(\"cookie\", &test_cookie_for(1)), (\"content-type\", \"{mime}\")]).await;\n    let meta: serde_json::Value = serde_json::from_str(&created.text()).expect(\"meta json\");\n    let id = meta[\"id\"].as_str().expect(\"id\");\n    let res = t.get(&format!(\"/{name}/{{id}}\")).await;\n    assert_eq!(res.status().as_u16(), 401, \"private read without a session; body: {{}}\", res.text());\n}}\n\n"
        ));
    }

    // 4. allowed_mime → 415 JC0415.
    if !b.allowed_mime.is_empty() {
        out.push_str(&format!(
            "#[tokio::test]\nasync fn {ident}_disallowed_mime_is_415() {{\n    let t = app().await;\n    let res = t.post_bytes_with(\"/{name}?key=bad-mime.bin\", b\"x\", &[(\"cookie\", &test_cookie_for(1)), (\"content-type\", \"application/x-jerrycan-blocked\")]).await;\n    assert_eq!(res.status().as_u16(), 415, \"design: allowed_mime violation is 415 JC0415; body: {{}}\", res.text());\n    assert!(res.text().contains(\"JC0415\"), \"body: {{}}\", res.text());\n}}\n\n"
        ));
    }

    // 5. max_size → 413 (the route's body_limit fires at the transport).
    if let Some(max) = b.max_size.as_deref().and_then(Design::parse_size) {
        out.push_str(&format!(
            "#[tokio::test]\nasync fn {ident}_oversize_upload_is_413() {{\n    let t = app().await;\n    let body = vec![0u8; {over}];\n    let res = t.post_bytes_with(\"/{name}?key=huge.bin\", &body, &[(\"cookie\", &test_cookie_for(1)), (\"content-type\", \"{mime}\")]).await;\n    assert_eq!(res.status().as_u16(), 413, \"design: max_size violation is 413 JC0413; body: {{}}\", res.text());\n}}\n\n",
            over = max + 1,
        ));
    }

    // 6. Cross-owner/cross-tenant negative control (owned buckets). User 2 is
    // a different owner AND (for tenant buckets, via the seeded memberships) a
    // different tenant, so this single control covers both scopes.
    if owned {
        let read_leg = if public {
            String::new() // public read is open by design — only writes are scoped.
        } else {
            format!(
                "    let foreign = t.get_with(&format!(\"/{name}/{{id}}\"), &[(\"cookie\", &test_cookie_for(2))]).await;\n    assert_eq!(foreign.status().as_u16(), 404, \"cross-owner get must 404; body: {{}}\", foreign.text());\n    let listed = t.get_with(\"/{name}\", &[(\"cookie\", &test_cookie_for(2))]).await;\n    assert_eq!(listed.status().as_u16(), 200, \"user 2 lists their own objects; body: {{}}\", listed.text());\n    assert!(!listed.text().contains(&id), \"cross-owner list must not leak the foreign id; body: {{}}\", listed.text());\n"
            )
        };
        let survive_leg = if public {
            format!(
                "    let survives = t.get(&format!(\"/{name}/{{id}}\")).await;\n    assert_eq!(survives.status().as_u16(), 200, \"the row must survive a cross-owner delete; body: {{}}\", survives.text());\n"
            )
        } else {
            format!(
                "    let survives = t.get_with(&format!(\"/{name}/{{id}}\"), &[(\"cookie\", &test_cookie_for(1))]).await;\n    assert_eq!(survives.status().as_u16(), 200, \"the row must survive a cross-owner delete; body: {{}}\", survives.text());\n"
            )
        };
        out.push_str(&format!(
            "/// SECURITY: user/tenant 2 must not reach owner 1's `{name}` objects —\n/// this is the isolation contract; breaking any scope turns it red.\n#[tokio::test]\nasync fn {ident}_cross_owner_access_is_denied() {{\n    let t = app().await;\n    let created = t.post_bytes_with(\"/{name}?key=mine.bin\", b\"mine\", &[(\"cookie\", &test_cookie_for(1)), (\"content-type\", \"{mime}\")]).await;\n    assert_eq!(created.status().as_u16(), 201, \"setup; body: {{}}\", created.text());\n    let meta: serde_json::Value = serde_json::from_str(&created.text()).expect(\"meta json\");\n    let id = meta[\"id\"].as_str().expect(\"id\").to_string();\n{read_leg}    let del = t.delete_with(&format!(\"/{name}/{{id}}\"), &[(\"cookie\", &test_cookie_for(2))]).await;\n    assert_eq!(del.status().as_u16(), 404, \"cross-owner delete must 404; body: {{}}\", del.text());\n{survive_leg}}}\n\n"
        ));
    }

    // 7. owner_prefix negative control: same relative key, two owners, two
    // distinct prefixed objects; B never reaches A's.
    if b.owner_prefix {
        out.push_str(&format!(
            "/// SECURITY: owner_prefix isolates keys per owner (Supabase\n/// folder-per-user parity): the same relative key lands under each owner's\n/// prefix and never collides or crosses.\n#[tokio::test]\nasync fn {ident}_owner_prefix_isolates_keys() {{\n    let t = app().await;\n    let a = t.post_bytes_with(\"/{name}?key=same.bin\", b\"a\", &[(\"cookie\", &test_cookie_for(1)), (\"content-type\", \"{mime}\")]).await;\n    assert_eq!(a.status().as_u16(), 201, \"owner 1 upload; body: {{}}\", a.text());\n    let a_meta: serde_json::Value = serde_json::from_str(&a.text()).expect(\"meta json\");\n    assert_eq!(a_meta[\"key\"], serde_json::json!(\"1/same.bin\"), \"key is stored under owner 1's prefix\");\n    let b = t.post_bytes_with(\"/{name}?key=same.bin\", b\"b\", &[(\"cookie\", &test_cookie_for(2)), (\"content-type\", \"{mime}\")]).await;\n    assert_eq!(b.status().as_u16(), 201, \"same relative key, different prefix — no collision; body: {{}}\", b.text());\n    let b_meta: serde_json::Value = serde_json::from_str(&b.text()).expect(\"meta json\");\n    assert_eq!(b_meta[\"key\"], serde_json::json!(\"2/same.bin\"));\n    let a_id = a_meta[\"id\"].as_str().expect(\"id\");\n    let cross = t.delete_with(&format!(\"/{name}/{{a_id}}\"), &[(\"cookie\", &test_cookie_for(2))]).await;\n    assert_eq!(cross.status().as_u16(), 404, \"cross-prefix delete must 404; body: {{}}\", cross.text());\n}}\n\n"
        ));
    }

    // 8. Signed URL: grants without a session; (private) a tampered sig fails.
    let tamper_leg = if public {
        String::new()
    } else {
        format!(
            "    let bad = t.get(&format!(\"{{url}}0\")).await;\n    assert_eq!(bad.status().as_u16(), 401, \"{name} tampered signed URL must be rejected; body: {{}}\", bad.text());\n"
        )
    };
    out.push_str(&format!(
        "#[tokio::test]\nasync fn {ident}_signed_url_grants_and_rejects() {{\n    let t = app().await;\n    let created = t.post_bytes_with(\"/{name}?key=to-sign.bin\", b\"signed\", &[(\"cookie\", &test_cookie_for(1)), (\"content-type\", \"{mime}\")]).await;\n    assert_eq!(created.status().as_u16(), 201, \"setup; body: {{}}\", created.text());\n    let meta: serde_json::Value = serde_json::from_str(&created.text()).expect(\"meta json\");\n    let id = meta[\"id\"].as_str().expect(\"id\");\n    let signed = t.post_json_with(&format!(\"/{name}/{{id}}/sign\"), &serde_json::json!({{}}), &[(\"cookie\", &test_cookie_for(1))]).await;\n    assert_eq!(signed.status().as_u16(), 200, \"sign; body: {{}}\", signed.text());\n    let url = serde_json::from_str::<serde_json::Value>(&signed.text()).expect(\"json\")[\"url\"].as_str().expect(\"url\").to_string();\n    let ok = t.get(&url).await;\n    assert_eq!(ok.status().as_u16(), 200, \"a signed URL needs no session; body: {{}}\", ok.text());\n    assert_eq!(ok.bytes(), &b\"signed\"[..]);\n{tamper_leg}}}\n\n"
    ));
}

/// Write (or refresh) the generated `crates/storage/` crate under `target`
/// (the app root). EVERY file is TOOL-owned and rewritten each run. Returns
/// paths relative to `target`. Precondition: the design passed
/// `questions::validate` (v2, db, active auth, well-formed buckets).
pub fn write_storage(target: &Path, design: &Design) -> Result<Vec<String>, String> {
    let crate_dir = target.join("crates/storage");
    fs::create_dir_all(crate_dir.join("src")).map_err(|e| e.to_string())?;
    let mut created = Vec::new();
    let mut write_tool = |rel: &str, content: &str| -> Result<(), String> {
        let path = crate_dir.join(rel);
        fs::create_dir_all(path.parent().expect("parent")).map_err(|e| e.to_string())?;
        fs::write(&path, content).map_err(|e| format!("write {}: {e}", path.display()))?;
        created.push(format!("crates/storage/{rel}"));
        Ok(())
    };
    write_tool("Cargo.toml", &cargo_toml())?;
    write_tool("src/lib.rs", &lib_rs(design))?;
    for b in sorted_buckets(design) {
        write_tool(&format!("src/{}.rs", bucket_ident(&b.name)), &bucket_rs(design, b))?;
    }
    write_tool("tests/acceptance.rs", &acceptance_rs(design))?;
    Ok(created)
}
```

- [ ] 4. Run: `cargo test -p jerrycan platform::storagegen` and `cargo test -p jerrycan platform::testgen` (the visibility flip must not break testgen) — expected PASS.
- [ ] 5. Commit: `Generate storage acceptance and isolation tests`

## Task 19 — Mounting wiring: extension, migrations, mounts, members, reserved filter

**Files:**
- Modify: `crates/jerrycan/src/platform/mounting.rs`
- Test: in-module

- [ ] 1. Write the failing tests (mounting.rs `mod tests`):

```rust
    fn storage_design() -> Design {
        serde_json::from_str(crate::platform::design::tests::V2_STORAGE).unwrap()
    }

    /// A storage design wires the Storage extension, runs STORAGE_MIGRATIONS
    /// after the app migrations, and mounts each bucket (sorted) AFTER the
    /// module mounts. Order is load-bearing: the extension precedes
    /// `.extend(db)`; migrations precede App::new().
    #[test]
    fn expected_main_wires_storage_extension_migrations_and_mounts() {
        let main = expected_main(&storage_design());
        let ext = main.find(".extend(jerrycan::storage::Storage::from_env()?)").unwrap();
        let db_ext = main.find(".extend(db)\n").unwrap();
        assert!(ext < db_ext, "storage extension before the db move: {main}");
        let app_mig = main.find("db.migrate(migrations::MIGRATIONS)").unwrap();
        let st_mig = main.find("db.migrate(jerrycan::storage::STORAGE_MIGRATIONS)").unwrap();
        let app_new = main.find("App::new()").unwrap();
        assert!(app_mig < st_mig && st_mig < app_new, "storage migrations after app migrations, before App::new: {main}");
        let module_mount = main.find(".mount(\"/orgs\", route_orgs::module())").unwrap();
        let avatars = main.find(".mount(\"/avatars\", storage::avatars::module())").unwrap();
        let invoices = main.find(".mount(\"/invoices\", storage::invoices::module())").unwrap();
        assert!(module_mount < avatars && avatars < invoices, "bucket mounts sorted, after modules: {main}");
    }

    /// No storage block → byte-for-byte no storage wiring.
    #[test]
    fn expected_main_without_storage_has_no_storage_wiring() {
        let d: Design = serde_json::from_str(crate::platform::design::tests::V1_FULL).unwrap();
        let main = expected_main(&d);
        assert!(!main.contains("storage"), "no storage wiring: {main}");
    }

    /// `storage` is a RESERVED dependency name: listing it must not emit the
    /// "provide here" stub comment (the block, not the dependency, is the gate).
    #[test]
    fn storage_dependency_name_is_reserved_not_stubbed() {
        let mut d = storage_design();
        d.dependencies.push("storage".into());
        let main = expected_main(&d);
        assert!(!main.contains("app dependency `storage`"), "{main}");
    }
```

- [ ] 2. Run: `cargo test -p jerrycan platform::mounting` — expected FAIL on all three.
- [ ] 3. Implement in `mounting.rs`:

In `extension_block`, between the observe and jobs pushes:

```rust
    // Storage provides itself (like Db) so handlers resolve Dep<Storage>; the
    // backend + signing key come from env (JERRYCAN_STORAGE / JERRYCAN_SECRET).
    if design.wants_storage() {
        block.push_str("        .extend(jerrycan::storage::Storage::from_env()?)\n");
    }
```

In `expected_main`, extend the reserved-name filter (the seam realtime extends next):

```rust
        .filter(|d| !matches!(d.as_str(), "db" | "validate" | "auth" | "observe" | "storage"))
```

After the module-mount loop, append the bucket mounts (sorted — matches storagegen's `sorted_buckets` order):

```rust
    if let Some(ref storage) = design.storage {
        let mut buckets: Vec<_> = storage.buckets.iter().collect();
        buckets.sort_by(|a, b| a.name.cmp(&b.name));
        for b in buckets {
            mounts.push_str(&format!(
                "        .mount(\"/{}\", storage::{}::module())\n",
                b.name,
                b.name.replace('-', "_")
            ));
        }
    }
```

After the `jobs_migrations` binding, mirror it:

```rust
    // Storage needs its metadata table: run STORAGE_MIGRATIONS right after the
    // app (and jobs) migrations, over the same `db`, before the move.
    let storage_migrations = if design.wants_storage() {
        "    db.migrate(jerrycan::storage::STORAGE_MIGRATIONS).await?;\n"
    } else {
        ""
    };
```

…and add `{storage_migrations}` right after `{jobs_migrations}` in the `format!` template.

In `regenerate`, mirror the jobs write/remove block (step 1d) as step 1e:

```rust
    // 1e. The generated storage crate (bucket modules + tests). Written when
    // the design declares buckets; removed when a prior design declared them
    // and no longer does (a stale crates/storage would break the workspace).
    let storage_dir = app_root.join("crates/storage");
    if design.wants_storage() {
        modified.extend(super::storagegen::write_storage(app_root, design)?);
    } else if storage_dir.exists() {
        fs::remove_dir_all(&storage_dir).map_err(|e| e.to_string())?;
        modified.push("crates/storage".to_string());
    }
```

In the members splice (after the jobs member):

```rust
    if design.wants_storage() {
        members.push_str("    \"crates/storage\",\n");
    }
```

In the app route-deps splice (after the jobs dep):

```rust
    if design.wants_storage() {
        // main.rs references `storage::<bucket>::module()`.
        deps.push_str("storage = { path = \"../storage\" }\n");
    }
```

- [ ] 4. Run: `cargo test -p jerrycan platform::mounting` — expected PASS; then the full `cargo test -p jerrycan` (scaffold/checkpipe suites must stay green — no storage design exists in their fixtures, so output is byte-identical for v0/v1).
- [ ] 5. Commit: `Wire storage into mounting: extension, migrations, mounts, members`

## Task 20 — Storage docs page (doc-tested) + embedded mirror

**Files:**
- Create: `docs/ai/18-storage.md`, `crates/jerrycan/embedded/ai/18-storage.md`
- Modify: `crates/jerrycan/src/lib.rs` (doc_page entry), `crates/jerrycan/src/platform/docsidx.rs` (PAGES entry)

- [ ] 1. Failing check: add the gate first — in `crates/jerrycan/src/lib.rs`'s `doc_tests` module, after the `page_17` entry (match the surrounding numbering):

```rust
    // Storage examples resolve jerrycan::storage + db, so the page is gated on
    // the storage feature; run with `cargo test -p jerrycan --features storage --doc`.
    #[cfg(feature = "storage")]
    doc_page!(page_18_storage, "../../../docs/ai/18-storage.md");
```

And in `docsidx.rs` `PAGES`, after the last entry:

```rust
    ("storage", include_str!("../../embedded/ai/18-storage.md")),
```

Run `cargo check -p jerrycan` — expected FAIL: both include files missing.

- [ ] 2. Write `docs/ai/18-storage.md`. Content (rust fences must compile under `--features storage`; use `rust,ignore` only for the design.json/env snippets that aren't Rust):

~~~markdown
# Storage

Buckets are modeled in `design.json` (contract v2); the generator emits guarded
per-bucket endpoints; object metadata lives in the `storage_objects` table;
bytes live in a pluggable blob store.

## Declaring buckets (contract v2)

```json
{
  "contract_version": 2,
  "storage": {
    "buckets": [
      { "name": "avatars", "visibility": "public", "owner": "User",
        "max_size": "5MB", "allowed_mime": ["image/*"] },
      { "name": "invoices", "visibility": "private", "owner": "Org",
        "owner_prefix": true, "max_size": "20MB" }
    ]
  }
}
```

- `visibility: public` — unauthenticated `GET` reads; mutations are ALWAYS guarded.
- `owner` — the tenancy entity makes the bucket tenant-owned (Tenant-guard
  scoped); any other entity stamps the session user id.
- `owner_prefix: true` — keys stored as `{owner_id}/…`, prefix-asserted on every
  access (the Supabase folder-per-user pattern).
- Storage requires the `db` dependency and an active auth model.

## Generated endpoints (per bucket `<b>`)

| Route | Behavior |
|---|---|
| `POST /<b>?key=<path>` | upload a raw body; `Content-Type` is the mime (415 `JC0415` outside `allowed_mime`; over `max_size` is 413 `JC0413`; duplicate key is 409) |
| `GET /<b>` | list (owner/tenant scoped; open when public) |
| `GET /<b>/{id}` | download — emits `ETag` (sha256) + `Cache-Control`; private buckets also accept `?exp=…&sig=…` |
| `DELETE /<b>/{id}` | delete row + bytes (scoped; foreign object = 404) |
| `POST /<b>/{id}/sign?ttl=300` | a time-limited signed URL |

## Configuring the backend (env, zero-touch)

```text
JERRYCAN_STORAGE=local:/var/data                      # filesystem (default: local:./storage)
JERRYCAN_STORAGE=s3://bucket?region=eu-central-1      # AWS S3
JERRYCAN_STORAGE=s3://bucket?endpoint=https://…       # R2 / MinIO / Supabase S3
```

S3 credentials come from `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY`; signed
URLs use `JERRYCAN_SECRET`. The packaged binary carries both backends — the env
var alone switches them.

## Using the service directly

```rust
use jerrycan::prelude::*;
use jerrycan::storage::{Bucket, Scope, Storage};

const REPORTS: Bucket = Bucket {
    name: "reports",
    public: false,
    owner_prefix: false,
    max_size: 1024 * 1024,
    allowed_mime: &["application/pdf"],
};

async fn archive(storage: Dep<Storage>, db: Dep<jerrycan::db::Db>) -> Result<Json<String>> {
    let scope = Scope { owner_id: Some("1".into()), tenant_id: None };
    let meta = storage
        .put_object(&db, &REPORTS, &scope, "q3.pdf", "application/pdf", bytes::Bytes::from_static(b"%PDF-"))
        .await?;
    Ok(Json(meta.id))
}
```
~~~

(Adjust the runnable snippet to the doc-test harness's exact expectations — mirror how `15-jobs.md` wraps its examples, including any hidden `# fn main` scaffolding those pages use. If `bytes` is not reachable from doc-tests, re-export it from `jerrycan_storage` (`pub use bytes;`) and use `jerrycan::storage::bytes::Bytes`.)

Copy byte-identically: `cp docs/ai/18-storage.md crates/jerrycan/embedded/ai/18-storage.md`.

- [ ] 3. Run: `cargo test -p jerrycan --features storage --doc page_18` and `cargo test -p jerrycan` — expected PASS (docsidx tests see the new page; doc examples compile).
- [ ] 4. Commit: `Add storage docs page (doc-tested) and embedded mirror`

## Task 21 — jerrycan-auth bcrypt-verify (migrated Supabase users)

**Files:**
- Modify: `Cargo.toml` (root: bcrypt workspace dep), `crates/jerrycan-auth/Cargo.toml`, `crates/jerrycan-auth/src/password.rs`
- Test: in-module

- [ ] 1. Write the failing tests (extend password.rs `mod tests`):

```rust
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
        let base = "$2a$10$N9qo8uLOickgx2ZMRZoMyeIjZAgcfl7p92ldGxad68LJZdL17lhWy";
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
```

- [ ] 2. Run: `cargo test -p jerrycan-auth password` — expected FAIL: no `bcrypt` dep, no `needs_rehash`.
- [ ] 3. Implement. Root `Cargo.toml` `[workspace.dependencies]` (dep justification: verification of migrated Supabase hashes only — jerrycan never mints bcrypt; MIT, builds on RustCrypto's blowfish, no unsafe crypto hand-rolling):

```toml
bcrypt = "0.17"
```

`crates/jerrycan-auth/Cargo.toml` `[dependencies]`:

```toml
bcrypt.workspace = true
```

`crates/jerrycan-auth/src/password.rs` — replace `verify_password` and add `needs_rehash` (module doc gains one line: "bcrypt is verify-only, for Supabase-migrated users; argon2 is the only hash we mint"):

```rust
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
    if is_bcrypt(phc) {
        return bcrypt::verify(password, phc)
            .map_err(|e| Error::internal(format!("stored bcrypt hash is malformed: {e}")));
    }
    let parsed = PasswordHash::new(phc)
        .map_err(|e| Error::internal(format!("stored hash is malformed: {e}")))?;
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
```

Export `needs_rehash` wherever `verify_password` is re-exported (check `crates/jerrycan-auth/src/lib.rs` — if it lists `password::{hash_password, verify_password}`, add `needs_rehash`).

- [ ] 4. Run: `cargo test -p jerrycan-auth` — expected PASS. `cargo clippy -p jerrycan-auth --all-targets -- -D warnings`. Note: if the pinned `$2a$…N9qo8…` vector fails while the round-trip test passes, the vector was transcribed wrong — regenerate one locally with `bcrypt::hash("password", 10)` and pin THAT (the round-trip test is the load-bearing proof; the pinned vector guards cross-implementation compatibility).
- [ ] 5. Commit: `Verify bcrypt password hashes in jerrycan-auth for migrated users`

## Task 22 — Final verification sweep

**Files:** none new — whole-workspace gates.

- [ ] 1. `cargo fmt --all -- --check` — clean.
- [ ] 2. `cargo clippy --workspace --all-targets -- -D warnings` and `cargo clippy -p jerrycan-storage --all-targets --features storage-s3 -- -D warnings` — clean.
- [ ] 3. `cargo test --workspace` — green (MinIO tests self-skip without `JERRYCAN_TEST_S3`).
- [ ] 4. `cargo test -p jerrycan --features storage --doc` — the storage doc page compiles.
- [ ] 5. With Docker available: run the MinIO battery per the `s3_minio.rs` header — all three live tests green.
- [ ] 6. End-to-end smoke (the generated surface actually runs): scaffold a throwaway app from a v2 storage design and run its generated tests —

```bash
cd "$(mktemp -d)"
cat > storage-smoke.design.json <<'EOF'
{ "name": "storage-smoke", "contract_version": 2,
  "auth": { "model": "session", "roles": ["owner", "member"] },
  "dependencies": ["db", "auth"],
  "tenancy": { "entity": "Org", "member_roles": ["owner", "member"] },
  "storage": { "buckets": [
    { "name": "avatars", "visibility": "public", "owner": "User", "max_size": "5MB", "allowed_mime": ["image/*"] },
    { "name": "invoices", "visibility": "private", "owner": "Org", "owner_prefix": true, "max_size": "20MB" } ] },
  "modules": [ { "name": "orgs",
    "entities": [
      { "name": "Org", "fields": [ { "name": "id", "type": "integer" }, { "name": "plan", "type": "string" } ] },
      { "name": "User", "fields": [ { "name": "id", "type": "integer" }, { "name": "email", "type": "string" } ] } ],
    "endpoints": [ { "operation_id": "list_orgs", "method": "GET", "path": "/",
      "success": { "status": 200, "entity": "Org", "list": true } } ] } ] }
EOF
JERRYCAN_FRAMEWORK_DEP='jerrycan = { path = "/Users/sorcecoder/github/jerrycan/crates/jerrycan", default-features = false }' \
  cargo run --manifest-path /Users/sorcecoder/github/jerrycan/Cargo.toml -p jerrycan -- new storage-smoke --design storage-smoke.design.json
cd storage-smoke && cargo test -p storage
```

(Use the repo's actual `jerrycan new` invocation shape — check `jerrycan new --help` if the flag differs. The generated `crates/storage/tests/acceptance.rs` must be GREEN out of the box: real handlers, isolation controls enforced.)
- [ ] 7. If the smoke test exposed a generated-code compile error, fix the corresponding storagegen template fragment, re-run Tasks 17–18's unit tests plus this smoke, and amend nothing — land the fix as its own plain commit (e.g. `Fix generated storage handler imports`).
- [ ] 8. Commit (if anything changed): `Storage: final verification fixes`

---

## Done means

- `cargo test --workspace` green; clippy/fmt clean; the storage doc page doc-tests green.
- The MinIO battery green against a live container (single-shot, multipart, presigned GET).
- A scaffolded v2 storage design compiles and its generated acceptance + isolation tests (cross-owner, cross-tenant, cross-prefix, 401/413/415, ETag/Cache-Control, signed URLs) pass out of the box.
- `wants_storage` / reserved filter / validation / facade feature seams are single-purpose and generic — ready for the realtime plan to extend.
- Migrated Supabase users can log in against their bcrypt hashes, and `needs_rehash` enables the transparent argon2 upgrade.

