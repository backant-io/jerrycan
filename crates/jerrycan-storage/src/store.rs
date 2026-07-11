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
