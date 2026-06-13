//! Fixed-window, identity-aware rate limiting as a jerrycan extension. The
//! store layer (this + redis); the RateLimit extension + middleware land in the
//! sibling modules. <https://jerrycan.cc>
#![forbid(unsafe_code)]

mod middleware;
#[cfg(feature = "rate-limit-redis")]
mod redis_store;
pub mod store;

pub use store::{InMemoryStore, Outcome, RateLimitStore};

#[cfg(feature = "rate-limit-redis")]
pub use redis_store::RedisStore;

use jerrycan_core::{App, Extension, RequestCtx};
use std::sync::Arc;
use std::time::Duration;

/// A closure mapping a request to a stable user partition key (e.g. a JWT sub).
type UserKeyFn = Arc<dyn Fn(&RequestCtx) -> Option<String> + Send + Sync>;

/// Fixed-window, identity-aware rate limiting (spec §v2.2). Install with
/// `app.extend(RateLimit::per_window(limit, window))`. Partition key is
/// api-key header → user-key closure → client IP; OPTIONS is exempt; over-limit
/// requests get 429 JC0429 + Retry-After. Time comes from the injected Clock so
/// windows are deterministic under TestApp::clock().
#[derive(Clone)]
pub struct RateLimit {
    limit: u32,
    window: Duration,
    store: Arc<dyn RateLimitStore>,
    api_key_header: http::HeaderName,
    user_key: Option<UserKeyFn>,
    trust_forwarded_for: bool,
}

impl RateLimit {
    /// Allow `limit` requests per `window` per partition key, using the default
    /// in-memory store. The api-key tier reads `x-api-key`.
    pub fn per_window(limit: u32, window: Duration) -> Self {
        Self {
            limit,
            window,
            store: Arc::new(InMemoryStore::new()),
            api_key_header: http::HeaderName::from_static("x-api-key"),
            user_key: None,
            trust_forwarded_for: false,
        }
    }

    /// Use a custom store (e.g. the Redis store behind `rate-limit-redis`).
    pub fn store(mut self, store: Arc<dyn RateLimitStore>) -> Self {
        self.store = store;
        self
    }

    /// Change the header consulted for the api-key partition tier.
    pub fn api_key_header(mut self, name: &'static str) -> Self {
        self.api_key_header = http::HeaderName::from_static(name);
        self
    }

    /// Supply a closure mapping a request to a stable user key (e.g. a JWT sub).
    pub fn user_key<F>(mut self, f: F) -> Self
    where
        F: Fn(&RequestCtx) -> Option<String> + Send + Sync + 'static,
    {
        self.user_key = Some(Arc::new(f));
        self
    }

    /// Honor X-Forwarded-For for the IP tier (ONLY behind a trusted proxy — the
    /// header is client-spoofable). Off by default; the raw socket peer is used.
    pub fn trust_forwarded_for(mut self, yes: bool) -> Self {
        self.trust_forwarded_for = yes;
        self
    }

    // pub(crate) accessors for the sibling middleware module.
    pub(crate) fn limit(&self) -> u32 {
        self.limit
    }
    pub(crate) fn window(&self) -> Duration {
        self.window
    }
    pub(crate) fn store_ref(&self) -> &Arc<dyn RateLimitStore> {
        &self.store
    }
    pub(crate) fn api_key_header_ref(&self) -> &http::HeaderName {
        &self.api_key_header
    }
    pub(crate) fn user_key_ref(&self) -> Option<&UserKeyFn> {
        self.user_key.as_ref()
    }
    pub(crate) fn trusts_forwarded_for(&self) -> bool {
        self.trust_forwarded_for
    }
}

impl Extension for RateLimit {
    fn register(self, app: App) -> App {
        let mw = middleware::RateLimitMw::new(self.clone());
        let store = self.store.clone();
        // The sweeper interval tracks the window (floored at 1s so a sub-second
        // window does not busy-loop the prune task).
        let interval = self.window.max(Duration::from_secs(1));
        app.middleware(mw)
            .on_serve("rate-limit-sweeper", move |mut ctx, mut shutdown| {
                let store = store.clone();
                async move {
                    // Resolve the Clock from the SAME task context (honors test
                    // overrides); if absent, skip pruning (the limiter still
                    // works, memory just isn't swept).
                    let clock = match ctx.resolve::<jerrycan_core::Clock>().await {
                        Ok(c) => c,
                        Err(_) => return,
                    };
                    loop {
                        // REAL tokio time for the interval; the injected Clock
                        // only supplies the window-boundary `now` passed to prune.
                        match tokio::time::timeout(interval, shutdown.changed()).await {
                            Ok(_) => break,                     // shutdown fired
                            Err(_) => store.prune(clock.now()), // interval elapsed
                        }
                    }
                }
            })
    }
}
