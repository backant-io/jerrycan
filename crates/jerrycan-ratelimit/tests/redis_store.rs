#![cfg(feature = "rate-limit-redis")]
//! Integration test for the Redis fixed-window store. Ignored by default because
//! CI has no Redis; run with a local server via
//! `cargo test -p jerrycan-ratelimit --features rate-limit-redis -- --ignored`.

use jerrycan_ratelimit::{RateLimitStore, RedisStore};
use std::time::{Duration, SystemTime};

#[tokio::test]
#[ignore = "needs a local redis at redis://127.0.0.1/"]
async fn redis_fixed_window_blocks_after_limit() {
    let store = RedisStore::connect("redis://127.0.0.1/").await.unwrap();
    // unique key so reruns don't collide
    let key = format!(
        "test:{}",
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let now = SystemTime::now();
    let w = Duration::from_secs(60);
    assert!(store.hit(&key, w, 2, now).await.unwrap().allowed);
    assert!(store.hit(&key, w, 2, now).await.unwrap().allowed);
    assert!(!store.hit(&key, w, 2, now).await.unwrap().allowed);
}
