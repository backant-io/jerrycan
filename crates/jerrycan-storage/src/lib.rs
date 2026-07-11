//! Object storage as a jerrycan extension: design-modeled buckets, a pluggable
//! blob store (local filesystem default, S3-compatible behind `storage-s3`),
//! DB-backed object metadata, and signed URLs. <https://jerrycan.cc>
#![forbid(unsafe_code)]

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

/// Re-exported so `jerrycan::storage::bytes::Bytes` is reachable from generated
/// apps and doc-tests without a separate `bytes` dependency.
pub use bytes;
pub use meta::{ObjectMeta, STORAGE_MIGRATIONS};
pub use store::{BlobFuture, BlobStore, LocalStore, MemoryStore};
#[cfg(feature = "storage-s3")]
pub use s3_store::S3Store;

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

// Manual, key-material-safe Debug: `Arc<dyn BlobStore>` can't derive Debug, and
// the sign key must never be printed — only whether one is present.
impl std::fmt::Debug for Storage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Storage")
            .field("has_sign_key", &self.sign_key.is_some())
            .finish_non_exhaustive()
    }
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

fn unix_secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
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

#[cfg(test)]
mod tests {
    use super::*;
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
}
