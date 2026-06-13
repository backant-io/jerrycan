//! Redis-backed fixed-window store (behind `rate-limit-redis`). Each window is a
//! key `ratelimit:{partition}:{window_id}` incremented atomically with a TTL of
//! one window, so eviction is Redis's job (the in-memory sweeper is unused here).

use crate::store::{HitFuture, Outcome, RateLimitStore, window_bounds};
use std::time::{Duration, SystemTime};

/// Atomic fixed-window counter: `INCR` the window key and, on the first hit
/// (`c == 1`), set its TTL to one window so Redis evicts it. Returning the count
/// in one round-trip keeps the limit decision race-free across nodes.
const SCRIPT: &str = r"
local c = redis.call('INCR', KEYS[1])
if c == 1 then redis.call('PEXPIRE', KEYS[1], ARGV[1]) end
return c
";

/// A Redis-backed [`RateLimitStore`]. Construct with [`RedisStore::connect`].
///
/// Holds one auto-reconnecting [`redis::aio::ConnectionManager`] (a cheap-to-clone
/// handle over a single multiplexed connection), so `hit` never opens a fresh
/// connection. Suitable for multi-node deployments where the in-memory store's
/// per-process counters would not coordinate.
pub struct RedisStore {
    conn: redis::aio::ConnectionManager,
}

impl RedisStore {
    /// Connect to `url` (e.g. `redis://127.0.0.1/` or `rediss://host/`). Async
    /// connection setup happens here so `Extension::register` stays sync (mirrors
    /// `Db::connect`). Fails fast if the server is unreachable.
    pub async fn connect(url: &str) -> jerrycan_core::Result<Self> {
        let client = redis::Client::open(url)
            .map_err(|e| jerrycan_core::Error::internal(format!("redis open: {e}")))?;
        let conn = redis::aio::ConnectionManager::new(client)
            .await
            .map_err(|e| jerrycan_core::Error::internal(format!("redis connect: {e}")))?;
        Ok(Self { conn })
    }
}

impl RateLimitStore for RedisStore {
    fn hit<'a>(
        &'a self,
        key: &'a str,
        window: Duration,
        limit: u32,
        now: SystemTime,
    ) -> HitFuture<'a> {
        Box::pin(async move {
            let (id, retry_after) = window_bounds(now, window);
            let redis_key = format!("ratelimit:{key}:{id}");
            let window_ms = window.as_millis().max(1) as u64;
            // Clone is cheap: ConnectionManager shares one multiplexed connection.
            let mut conn = self.conn.clone();
            let count: u32 = redis::Script::new(SCRIPT)
                .key(redis_key)
                .arg(window_ms)
                .invoke_async(&mut conn)
                .await
                .map_err(|e| jerrycan_core::Error::internal(format!("redis: {e}")))?;
            Ok(Outcome {
                allowed: count <= limit,
                remaining: limit.saturating_sub(count),
                retry_after,
            })
        })
    }
    // prune is the trait default no-op; Redis TTLs handle eviction.
}
