//! Live-Postgres integration test for the durable [`PostgresStore`] (spec
//! §v2.3): the real `FOR UPDATE SKIP LOCKED` lease, the partial-unique
//! idempotency dedup, retry-backoff, dead-letter/requeue, and the
//! `pg_advisory_xact_lock` cron leader — none of which the sqlite-backed unit
//! tests can reach (sqlite has no SKIP LOCKED and no advisory locks).
//!
//! Ignored by default (CI has no Postgres unless it provisions a service). Run
//! with a live server:
//! ```text
//! JERRYCAN_TEST_PG_URL=postgres://user:pass@localhost/jerrycan_test \
//!   cargo test -p jerrycan-jobs --test postgres_store -- --ignored
//! ```
//! Each run uses a unique queue/idempotency namespace so reruns against the same
//! database never collide. The migration is idempotent (the tracking table skips
//! applied names), so it is safe to run repeatedly.

use jerrycan_jobs::{CronSchedule, EnqueueOutcome, JobStatus, JobStore, NewJob, PostgresStore};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn t0() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_000_000)
}

/// A run-unique suffix so reruns against the same Postgres never collide.
fn unique() -> String {
    format!(
        "{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
}

#[tokio::test]
#[ignore = "needs a local postgres (set JERRYCAN_TEST_PG_URL)"]
async fn postgres_store_matches_the_reference_semantics_end_to_end() {
    let Ok(url) = std::env::var("JERRYCAN_TEST_PG_URL") else {
        eprintln!("SKIP: JERRYCAN_TEST_PG_URL not set (no live Postgres)");
        return;
    };
    let store = PostgresStore::connect(&url).await.unwrap();
    store.migrate().await.unwrap();

    let q = format!("test_{}", unique());
    let lease_dur = Duration::from_secs(30);

    // enqueue → lease (SKIP LOCKED claims it, attempts=1).
    store.enqueue(NewJob::new("a", &q), t0()).await.unwrap();
    let leased = store.lease(&q, t0(), lease_dur, 10).await.unwrap();
    assert_eq!(leased.len(), 1, "the due job is claimed via SKIP LOCKED");
    assert_eq!(leased[0].attempts, 1, "lease is the first attempt");
    assert_eq!(leased[0].status, JobStatus::Leased);
    let id = leased[0].id;

    // Don't ack: not re-leasable before the lease expires.
    assert!(
        store
            .lease(&q, t0() + Duration::from_secs(5), lease_dur, 10)
            .await
            .unwrap()
            .is_empty(),
        "held lease is not reclaimable before expiry"
    );

    // Re-lease after the lease expires (at-least-once: attempts=2).
    let again = store
        .lease(&q, t0() + lease_dur + Duration::from_secs(1), lease_dur, 10)
        .await
        .unwrap();
    assert_eq!(again.len(), 1, "an expired lease is reclaimed");
    assert_eq!(again[0].id, id);
    assert_eq!(again[0].attempts, 2, "the reclaim is the second attempt");

    // ack → terminal, never leased again.
    store.ack(id).await.unwrap();
    assert!(
        store
            .lease(&q, t0() + Duration::from_secs(3600), lease_dur, 10)
            .await
            .unwrap()
            .is_empty(),
        "an acked job is done"
    );

    // Idempotency: a duplicate key is a no-op reporting the existing id; only one
    // job is leasable.
    let key = format!("idem_{}", unique());
    let qi = format!("idem_{}", unique());
    let first = store
        .enqueue(NewJob::new("b", &qi).idempotency_key(&key), t0())
        .await
        .unwrap();
    let second = store
        .enqueue(NewJob::new("b", &qi).idempotency_key(&key), t0())
        .await
        .unwrap();
    let EnqueueOutcome::Inserted(iid) = first else {
        panic!("first enqueue inserts");
    };
    assert!(
        matches!(second, EnqueueOutcome::Duplicate(d) if d == iid),
        "same key no-ops to the existing id"
    );
    assert_eq!(
        store.lease(&qi, t0(), lease_dur, 10).await.unwrap().len(),
        1,
        "only one job exists for the deduped key"
    );

    // retry-backoff: not leasable until run_at.
    let qr = format!("retry_{}", unique());
    store.enqueue(NewJob::new("c", &qr), t0()).await.unwrap();
    let job = store
        .lease(&qr, t0(), lease_dur, 10)
        .await
        .unwrap()
        .pop()
        .unwrap();
    let backoff = t0() + Duration::from_secs(60);
    store.retry(job.id, backoff).await.unwrap();
    assert!(
        store
            .lease(&qr, t0() + Duration::from_secs(30), lease_dur, 10)
            .await
            .unwrap()
            .is_empty(),
        "not due until the backoff window elapses"
    );
    assert_eq!(
        store
            .lease(&qr, backoff + Duration::from_secs(1), lease_dur, 10)
            .await
            .unwrap()
            .len(),
        1,
        "due after backoff"
    );

    // dead-letter + list_dead + requeue.
    let qd = format!("dead_{}", unique());
    store.enqueue(NewJob::new("d", &qd), t0()).await.unwrap();
    let dj = store
        .lease(&qd, t0(), lease_dur, 10)
        .await
        .unwrap()
        .pop()
        .unwrap();
    store.dead_letter(dj.id).await.unwrap();
    let dead = store.list_dead(&qd, 10).await.unwrap();
    assert_eq!(dead.len(), 1, "the dead job is listed");
    assert_eq!(dead[0].id, dj.id);
    assert_eq!(dead[0].status, JobStatus::Dead);
    store.requeue_dead(dj.id).await.unwrap();
    let requeued = store
        .lease(&qd, t0() + Duration::from_secs(3600), lease_dur, 10)
        .await
        .unwrap();
    assert_eq!(requeued.len(), 1, "a requeued dead job is leasable again");
    assert_eq!(requeued[0].attempts, 1, "attempts reset, this lease is #1");
}

#[tokio::test]
#[ignore = "needs a local postgres (set JERRYCAN_TEST_PG_URL)"]
async fn cron_leader_enqueues_once_per_due_tick_under_the_advisory_lock() {
    let Ok(url) = std::env::var("JERRYCAN_TEST_PG_URL") else {
        eprintln!("SKIP: JERRYCAN_TEST_PG_URL not set (no live Postgres)");
        return;
    };
    let store = PostgresStore::connect(&url).await.unwrap();
    store.migrate().await.unwrap();

    // A run-unique cron job name so its jerrycan_jobs_cron row and idempotency
    // keys are isolated from other runs.
    let job = format!("hourly_{}", unique());
    let queue = format!("cron_{}", unique());
    let sched = CronSchedule::parse("0 * * * *").unwrap();
    let crons = vec![(job.clone(), sched, queue.clone())];

    // First tick (last_fired NULL → first-run policy fires the most-recent tick).
    let now = t0() + Duration::from_secs(5 * 3600 + 30 * 60); // 05:30-ish past t0
    let n1 = store.cron_tick(&crons, now).await.unwrap();
    assert_eq!(n1, 1, "the most-recent due tick is enqueued once");

    // A second tick at the same instant is a no-op: last_fired advanced and the
    // idempotency key blocks any re-enqueue (atomic enqueue + last_fired).
    let n2 = store.cron_tick(&crons, now).await.unwrap();
    assert_eq!(n2, 0, "nothing new is due; no double-fire");

    // The next hour's tick is due and fires once more.
    let n3 = store
        .cron_tick(&crons, now + Duration::from_secs(3600))
        .await
        .unwrap();
    assert_eq!(n3, 1, "the next hour's tick fires exactly once");

    // Exactly one job per fired tick is leasable on the cron queue.
    let leased = store
        .lease(
            &queue,
            now + Duration::from_secs(7200),
            Duration::from_secs(30),
            100,
        )
        .await
        .unwrap();
    assert_eq!(leased.len(), 2, "two distinct ticks enqueued two jobs");
    assert!(leased.iter().all(|j| j.name == job));
}
