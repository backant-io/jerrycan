//! Object storage as a jerrycan extension: design-modeled buckets, a pluggable
//! blob store (local filesystem default, S3-compatible behind `storage-s3`),
//! DB-backed object metadata, and signed URLs. <https://jerrycan.cc>
#![forbid(unsafe_code)]

pub mod store;

pub use store::{BlobFuture, BlobStore, LocalStore, MemoryStore};
