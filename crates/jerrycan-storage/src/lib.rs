//! Object storage as a jerrycan extension: design-modeled buckets, a pluggable
//! blob store (local filesystem default, S3-compatible behind `storage-s3`),
//! DB-backed object metadata, and signed URLs. <https://jerrycan.cc>
#![forbid(unsafe_code)]

mod sign;
pub mod store;

pub use store::{BlobFuture, BlobStore, LocalStore, MemoryStore};

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
