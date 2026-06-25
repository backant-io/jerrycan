//! Agent-owned: module-scoped dependencies and middleware.
//!
//! The api-keys module owns the API-key store: a single in-memory store shared
//! between `create_api_key` (which inserts the minted hash) and `usage` (which
//! authenticates against it via the documented `ApiKeys` DI contract). Both the
//! concrete `SharedKeyStore` handle and the `ApiKeys` wrapper point at the SAME
//! `Arc`, so a key minted by one request authenticates on the next.
use jerrycan::auth::{ApiKeys, InMemoryApiKeyStore};
use jerrycan::prelude::*;
use std::sync::Arc;

/// A concrete handle on the shared key store so `create_api_key` can `insert`
/// (the `ApiKeyStore` trait is read-only — `lookup` only — by design).
#[derive(Clone)]
pub(crate) struct SharedKeyStore(pub Arc<InMemoryApiKeyStore>);

/// Called by the tool-owned lib.rs — register module deps/middleware here;
/// regeneration never touches this file.
pub(crate) fn configure(module: Module) -> Module {
    let store = Arc::new(InMemoryApiKeyStore::new());
    module
        .provide(SharedKeyStore(store.clone()))
        .provide(ApiKeys::from_arc(store))
}
