//! The rate-limit store layer: the [`RateLimitStore`] trait, its [`Outcome`],
//! the shared fixed-window math, and the std-only [`InMemoryStore`].
//!
//! The store records hits against a key for the fixed window
//! `[window_start, window_start + window)` derived from an explicit `now` —
//! the middleware (Task 7) supplies `clock.now()`, so the store stays
//! clock-free and trivially testable. The Redis store (Task 8) reuses
//! `window_bounds` and the default no-op [`RateLimitStore::prune`].

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

/// The result of recording one request against a key in its current window.
#[derive(Clone, Copy, Debug)]
pub struct Outcome {
    /// Whether this request stays within the limit (`count <= limit`).
    pub allowed: bool,
    /// Requests still permitted in this window after counting this one.
    pub remaining: u32,
    /// Time until the current window ends — the `Retry-After` the middleware
    /// reports on a block. Always within `(0, window]`.
    pub retry_after: Duration,
}

/// The boxed future returned by [`RateLimitStore::hit`]. A hand-boxed future
/// keeps the trait object-safe without pulling in `async-trait`.
pub type HitFuture<'a> = Pin<Box<dyn Future<Output = jerrycan_core::Result<Outcome>> + Send + 'a>>;

/// A backend that counts requests per key within a fixed window.
///
/// Object-safe (used behind `dyn`), so `hit` returns a hand-boxed future
/// rather than using `async fn`.
pub trait RateLimitStore: Send + Sync + 'static {
    /// Record one request against `key` and report whether it is allowed
    /// within the fixed window `[window_start, window_start + window)` derived
    /// from `now`.
    fn hit<'a>(
        &'a self,
        key: &'a str,
        window: Duration,
        limit: u32,
        now: SystemTime,
    ) -> HitFuture<'a>;

    /// Evict idle windows. Default is a no-op: the Redis store relies on
    /// per-key TTLs, so only the in-memory store overrides this. The Task 7
    /// sweeper calls it generically.
    fn prune(&self, _now: SystemTime) {}
}

/// Convert `now` into `(window_id, retry_after)` for a fixed window of size
/// `window`, where `window_id = floor(epoch_ms / window_ms)` and
/// `retry_after = window_ms - (epoch_ms % window_ms)` (time left in the window).
///
/// Defensive against a pre-epoch `now` (clamped to 0, never unwrapped) and a
/// zero-length `window` (`window_ms` floored at 1 to avoid division by zero).
pub(crate) fn window_bounds(now: SystemTime, window: Duration) -> (u64, Duration) {
    let epoch_ms = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let window_ms = window.as_millis().max(1);
    let window_id = (epoch_ms / window_ms) as u64;
    let retry_after_ms = window_ms - (epoch_ms % window_ms);
    (window_id, Duration::from_millis(retry_after_ms as u64))
}

/// Per-key window state. `window_ms` is kept so [`InMemoryStore::prune`] can
/// compute each key's window end independently of any single caller's window.
type Entry = (u64, u32, u128);

/// A std-only, process-local fixed-window store. Suitable for single-node
/// deployments and tests; the Redis store (Task 8) covers multi-node.
pub struct InMemoryStore {
    map: Mutex<HashMap<String, Entry>>,
}

impl InMemoryStore {
    /// An empty store.
    pub fn new() -> Self {
        Self {
            map: Mutex::new(HashMap::new()),
        }
    }

    #[cfg(test)]
    pub fn entry_count(&self) -> usize {
        self.map
            .lock()
            .expect("ratelimit store mutex poisoned")
            .len()
    }
}

impl Default for InMemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

impl RateLimitStore for InMemoryStore {
    fn hit<'a>(
        &'a self,
        key: &'a str,
        window: Duration,
        limit: u32,
        now: SystemTime,
    ) -> HitFuture<'a> {
        Box::pin(async move {
            let (id, retry_after) = window_bounds(now, window);
            let window_ms = window.as_millis().max(1);
            let mut map = self.map.lock().expect("ratelimit store mutex poisoned");
            let entry = map.entry(key.to_string()).or_insert((id, 0, window_ms));
            if entry.0 != id {
                *entry = (id, 0, window_ms);
            }
            entry.1 = entry.1.saturating_add(1);
            let count = entry.1;
            Ok(Outcome {
                allowed: count <= limit,
                remaining: limit.saturating_sub(count),
                retry_after,
            })
        })
    }

    fn prune(&self, now: SystemTime) {
        let now_ms = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let mut map = self.map.lock().expect("ratelimit store mutex poisoned");
        map.retain(|_, (window_id, _, window_ms)| {
            // Keep keys whose window has not fully elapsed by `now`.
            (u128::from(*window_id) + 1) * *window_ms > now_ms
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    #[tokio::test]
    async fn fixed_window_allows_then_blocks_then_resets() {
        let store = InMemoryStore::new();
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let window = Duration::from_secs(60);
        for i in 0..3 {
            let out = store
                .hit("k", window, 3, t0 + Duration::from_secs(i))
                .await
                .unwrap();
            assert!(out.allowed, "request {i} within limit");
        }
        let blocked = store
            .hit("k", window, 3, t0 + Duration::from_secs(5))
            .await
            .unwrap();
        assert!(!blocked.allowed);
        assert!(blocked.retry_after <= window && blocked.retry_after > Duration::ZERO);
        let next = store
            .hit("k", window, 3, t0 + window + Duration::from_secs(1))
            .await
            .unwrap();
        assert!(next.allowed, "new window resets the count");
    }

    #[tokio::test]
    async fn keys_are_independent() {
        let store = InMemoryStore::new();
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let w = Duration::from_secs(60);
        assert!(store.hit("a", w, 1, t0).await.unwrap().allowed);
        assert!(!store.hit("a", w, 1, t0).await.unwrap().allowed);
        assert!(
            store.hit("b", w, 1, t0).await.unwrap().allowed,
            "different key, fresh budget"
        );
    }

    #[tokio::test]
    async fn prune_evicts_only_stale_windows() {
        let store = InMemoryStore::new();
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let w = Duration::from_secs(60);
        store.hit("old", w, 5, t0).await.unwrap();
        store.hit("fresh", w, 5, t0 + w * 2).await.unwrap();
        store.prune(t0 + w * 2 + Duration::from_secs(1));
        assert_eq!(
            store.entry_count(),
            1,
            "only the current-window key remains"
        );
    }
}
