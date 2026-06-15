#![cfg(feature = "jobs-redis")]
//! Live-Redis integration tests for the durable, multi-node [`RedisStore`] (spec
//! §v2.3b): the Streams + consumer-group lease, the atomic (Lua, `SET NX`)
//! idempotency dedup, `run_at`/backoff scheduling, `XAUTOCLAIM` crashed-worker
//! reclaim, and dead-letter / requeue — none of which the in-memory contract
//! tests exercise against a real server.
//!
//! Ignored by default (CI has no Redis). Run with a local server:
//! ```text
//! cargo test -p jerrycan-jobs --features jobs-redis --test redis_store -- --ignored
//! ```
//! Each test derives a UNIQUE queue/key prefix from its name + a nanosecond
//! suffix, so concurrent or repeat runs on a shared Redis never collide, and
//! best-effort flushes its keys at the end. (Wall-clock-dependent reclaim uses a
//! small real lease and a real sleep — the one place the store relies on the
//! Redis-server clock rather than the injected `now`.)

use jerrycan_jobs::{EnqueueOutcome, JobStatus, JobStore, NewJob, RedisStore};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const URL: &str = "redis://127.0.0.1/";

/// A deterministic injected `now` for the scheduling assertions (the same base
/// the in-memory/Postgres contract tests use).
fn t0() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_000_000)
}

/// A run-unique queue name so reruns / parallel runs on a shared Redis never
/// collide on a key. Derived from the test name + a nanosecond suffix.
fn unique_queue(test: &str) -> String {
    format!(
        "test:{test}:{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
}

/// Best-effort cleanup: delete every `jc:jobs:*` key whose queue segment matches
/// this run's queue, plus the per-job hashes it created. Failures are ignored
/// (the unique prefix already prevents collisions; this just keeps Redis tidy).
async fn cleanup(store: &RedisStore, queue: &str, ids: &[i64], idem_keys: &[&str]) {
    let mut conn = match redis::Client::open(URL) {
        Ok(c) => match redis::aio::ConnectionManager::new(c).await {
            Ok(c) => c,
            Err(_) => return,
        },
        Err(_) => return,
    };
    let _ = store; // keep the signature symmetric with the connect helper
    let mut keys = vec![
        format!("jc:jobs:q:{queue}:s"),
        format!("jc:jobs:q:{queue}:z"),
        format!("jc:jobs:q:{queue}:dead"),
    ];
    for id in ids {
        keys.push(format!("jc:jobs:job:{id}"));
    }
    for k in idem_keys {
        keys.push(format!("jc:jobs:idem:{k}"));
    }
    let _: redis::RedisResult<()> = redis::cmd("DEL").arg(&keys).query_async(&mut conn).await;
}

async fn connect() -> RedisStore {
    RedisStore::connect(URL)
        .await
        .expect("a local redis at redis://127.0.0.1/ (run with --ignored)")
}

/// 1) enqueue → lease → ack roundtrip: status transitions and payload survive.
#[tokio::test]
#[ignore = "needs a local redis at redis://127.0.0.1/"]
async fn enqueue_lease_ack_roundtrip() {
    let store = connect().await;
    let q = unique_queue("roundtrip");
    let payload = serde_json::json!({ "to": "a@example.com", "n": 7 });

    let inserted = store
        .enqueue(NewJob::new("send_email", &q).payload(payload.clone()), t0())
        .await
        .unwrap();
    let EnqueueOutcome::Inserted(id) = inserted else {
        panic!("first enqueue must insert");
    };

    let leased = store
        .lease(&q, t0(), Duration::from_secs(30), 10)
        .await
        .unwrap();
    assert_eq!(leased.len(), 1, "the due job is leased");
    assert_eq!(leased[0].id, id);
    assert_eq!(leased[0].status, JobStatus::Leased);
    assert_eq!(leased[0].attempts, 1, "lease is the first attempt");
    assert_eq!(leased[0].payload, payload, "JSON payload preserved");
    assert_eq!(leased[0].name, "send_email");

    store.ack(id).await.unwrap();
    // Acked → terminal: nothing leasable, even far in the future.
    assert!(
        store
            .lease(
                &q,
                t0() + Duration::from_secs(3600),
                Duration::from_secs(30),
                10
            )
            .await
            .unwrap()
            .is_empty(),
        "an acked job is gone"
    );

    cleanup(&store, &q, &[id], &[]).await;
}

/// 2) idempotency: a second enqueue with the same key returns Duplicate(id) and
/// does NOT create a second leasable job.
#[tokio::test]
#[ignore = "needs a local redis at redis://127.0.0.1/"]
async fn idempotency_key_is_a_cross_node_no_op() {
    let store = connect().await;
    let q = unique_queue("idem");
    let key = format!("k:{q}");

    let first = store
        .enqueue(NewJob::new("a", &q).idempotency_key(&key), t0())
        .await
        .unwrap();
    let second = store
        .enqueue(NewJob::new("a", &q).idempotency_key(&key), t0())
        .await
        .unwrap();
    let EnqueueOutcome::Inserted(id) = first else {
        panic!("first must insert");
    };
    assert!(
        matches!(second, EnqueueOutcome::Duplicate(d) if d == id),
        "same key is a no-op reporting the existing id, not an error"
    );

    let leased = store
        .lease(&q, t0(), Duration::from_secs(30), 10)
        .await
        .unwrap();
    assert_eq!(leased.len(), 1, "only one job exists despite two enqueues");

    cleanup(&store, &q, &[id], &[&key]).await;
}

/// 3) run_at delay: a future job is not leased at `now`, is leased at `run_at`.
#[tokio::test]
#[ignore = "needs a local redis at redis://127.0.0.1/"]
async fn run_at_delays_until_due() {
    let store = connect().await;
    let q = unique_queue("runat");
    let due = t0() + Duration::from_secs(3600);

    let EnqueueOutcome::Inserted(id) = store
        .enqueue(NewJob::new("a", &q).run_at(due), t0())
        .await
        .unwrap()
    else {
        panic!("insert");
    };

    assert!(
        store
            .lease(&q, t0(), Duration::from_secs(30), 10)
            .await
            .unwrap()
            .is_empty(),
        "future job not yet due"
    );
    let leased = store
        .lease(
            &q,
            due + Duration::from_secs(1),
            Duration::from_secs(30),
            10,
        )
        .await
        .unwrap();
    assert_eq!(leased.len(), 1, "due after run_at");
    assert_eq!(leased[0].id, id);

    cleanup(&store, &q, &[id], &[]).await;
}

/// 4) retry backoff: after retry(id, now+Δ) the job is not leased before now+Δ,
/// and is leased after.
#[tokio::test]
#[ignore = "needs a local redis at redis://127.0.0.1/"]
async fn retry_backoff_holds_until_the_window_elapses() {
    let store = connect().await;
    let q = unique_queue("retry");

    let EnqueueOutcome::Inserted(id) = store.enqueue(NewJob::new("a", &q), t0()).await.unwrap()
    else {
        panic!("insert");
    };
    let job = store
        .lease(&q, t0(), Duration::from_secs(30), 10)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(job.id, id);

    let backoff_until = t0() + Duration::from_secs(60);
    store.retry(id, backoff_until).await.unwrap();

    assert!(
        store
            .lease(
                &q,
                t0() + Duration::from_secs(30),
                Duration::from_secs(30),
                10
            )
            .await
            .unwrap()
            .is_empty(),
        "not due until backoff"
    );
    let again = store
        .lease(
            &q,
            backoff_until + Duration::from_secs(1),
            Duration::from_secs(30),
            10,
        )
        .await
        .unwrap();
    assert_eq!(again.len(), 1, "due after backoff");
    assert_eq!(again[0].id, id);
    // attempts: first lease (1) + this re-lease (2).
    assert_eq!(again[0].attempts, 2, "the re-lease is the second attempt");

    cleanup(&store, &q, &[id], &[]).await;
}

/// 5) crashed-worker reclaim: lease (no ack), wait past a tiny lease, lease
/// again → the same job is re-leased with attempts == 2. This is the one
/// wall-clock path: XAUTOCLAIM's min-idle is the Redis-server PEL idle, so the
/// test uses a small REAL lease + a real sleep rather than the injected `now`.
#[tokio::test]
#[ignore = "needs a local redis at redis://127.0.0.1/"]
async fn crashed_worker_lease_is_reclaimed() {
    let store = connect().await;
    let q = unique_queue("reclaim");
    let lease = Duration::from_millis(200);

    let EnqueueOutcome::Inserted(id) = store.enqueue(NewJob::new("a", &q), t0()).await.unwrap()
    else {
        panic!("insert");
    };

    let first = store.lease(&q, t0(), lease, 10).await.unwrap();
    assert_eq!(first.len(), 1, "the due job is leased");
    assert_eq!(first[0].attempts, 1);
    // Do NOT ack — simulate a crashed worker holding the lease.

    // Before the lease idle elapses, it is not reclaimable.
    let immediate = store.lease(&q, t0(), lease, 10).await.unwrap();
    assert!(
        immediate.is_empty(),
        "a still-held lease (PEL idle < lease) is not stolen"
    );

    // Wait past the lease so the PEL idle exceeds it, then reclaim.
    tokio::time::sleep(Duration::from_millis(350)).await;
    let reclaimed = store.lease(&q, t0(), lease, 10).await.unwrap();
    assert_eq!(reclaimed.len(), 1, "an expired lease is reclaimed");
    assert_eq!(reclaimed[0].id, id);
    assert_eq!(
        reclaimed[0].attempts, 2,
        "the reclaim counts as a second attempt"
    );

    store.ack(id).await.unwrap();
    cleanup(&store, &q, &[id], &[]).await;
}

/// 6) dead_letter → list_dead (deterministic, id-ordered) → requeue_dead →
/// leasable again with attempts reset.
#[tokio::test]
#[ignore = "needs a local redis at redis://127.0.0.1/"]
async fn dead_letter_lists_in_order_then_requeues() {
    let store = connect().await;
    let q = unique_queue("dead");

    // Two jobs so list_dead order is observable; enqueue a → b (ids ascend).
    let EnqueueOutcome::Inserted(id_a) = store.enqueue(NewJob::new("a", &q), t0()).await.unwrap()
    else {
        panic!("insert a");
    };
    let EnqueueOutcome::Inserted(id_b) = store.enqueue(NewJob::new("b", &q), t0()).await.unwrap()
    else {
        panic!("insert b");
    };

    // Lease both and dead-letter both.
    let leased = store
        .lease(&q, t0(), Duration::from_secs(30), 10)
        .await
        .unwrap();
    assert_eq!(leased.len(), 2);
    for j in &leased {
        store.dead_letter(j.id).await.unwrap();
    }

    // Dead jobs are not leasable.
    assert!(
        store
            .lease(
                &q,
                t0() + Duration::from_secs(3600),
                Duration::from_secs(30),
                10
            )
            .await
            .unwrap()
            .is_empty(),
        "dead jobs are not leased"
    );

    let dead = store.list_dead(&q, 10).await.unwrap();
    assert_eq!(dead.len(), 2, "both dead jobs listed");
    assert_eq!(dead[0].id, id_a, "ordered by id (insertion order)");
    assert_eq!(dead[1].id, id_b);
    assert!(dead.iter().all(|j| j.status == JobStatus::Dead));

    // Requeue the first; it becomes immediately leasable with attempts reset.
    store.requeue_dead(id_a).await.unwrap();
    let requeued = store
        .lease(
            &q,
            t0() + Duration::from_secs(3601),
            Duration::from_secs(30),
            10,
        )
        .await
        .unwrap();
    assert_eq!(requeued.len(), 1, "requeued dead job is leasable again");
    assert_eq!(requeued[0].id, id_a);
    assert_eq!(
        requeued[0].attempts, 1,
        "attempts reset to 0, then this lease is #1"
    );

    // The still-dead job remains in the dead set.
    let still_dead = store.list_dead(&q, 10).await.unwrap();
    assert_eq!(still_dead.len(), 1);
    assert_eq!(still_dead[0].id, id_b);

    cleanup(&store, &q, &[id_a, id_b], &[]).await;
}
