//! Engine integration tests (spec §v2.3): the worker's headline retry →
//! dead-letter cycle and ack path driven through the pure `tick(now)` with NO
//! running worker, plus the cron leader's enqueue-once-per-due-tick through the
//! real `cron_tick_once` path.

use jerrycan_core::prelude::*;
use jerrycan_db::Db;
use jerrycan_jobs::worker::{JobConfig, JobFn, Worker};
use jerrycan_jobs::{InMemoryStore, JOBS_MIGRATIONS, JobStore, Jobs, NewJob, PostgresStore};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime};

fn t0() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000)
}

#[tokio::test]
async fn worker_runs_acks_retries_with_backoff_then_dead_letters() {
    let store: Arc<dyn JobStore> = Arc::new(InMemoryStore::new());
    let runs = Arc::new(AtomicU32::new(0));
    // A task that always fails — every run counts and returns an error.
    let r = runs.clone();
    let failing: JobFn = Arc::new(move |_ctx, _p| {
        let r = r.clone();
        Box::pin(async move {
            r.fetch_add(1, Ordering::SeqCst);
            Err(jerrycan_core::Error::internal("nope"))
        })
    });
    let mut dispatch = HashMap::new();
    dispatch.insert("flaky".to_string(), failing);

    // A TaskContext to fork from — a plain TestApp provides nothing special; the
    // task uses no deps.
    let t = App::new().into_test();
    let base = t.task_context();

    let cfg = JobConfig {
        backoff_base: Duration::from_secs(1),
        backoff_cap: Duration::from_secs(60),
        lease_duration: Duration::from_secs(30),
        exec_timeout: Duration::from_secs(5),
        poll_interval: Duration::from_secs(1),
        batch: 10,
    };
    let worker = Worker::new(store.clone(), Arc::new(dispatch), cfg);

    store
        .enqueue(NewJob::new("flaky", "default").max_attempts(3), t0())
        .await
        .unwrap();

    // tick 1: runs (attempt 1), fails → retry with backoff. `tick` takes an
    // owned (Send) context; fork a fresh one per call from the base.
    assert_eq!(worker.tick("default", t0(), base.fork()).await.unwrap(), 1);
    assert_eq!(runs.load(Ordering::SeqCst), 1);

    // Advance well past each backoff window and keep ticking. With
    // max_attempts=3, after 3 executions the job is dead-lettered
    // (attempts >= max), not retried forever. Each 300s advance clears the
    // backoff (base*2^(n-1) ≤ 4s ≪ 300s).
    let mut now = t0();
    for _ in 0..6 {
        now += Duration::from_secs(300);
        worker.tick("default", now, base.fork()).await.unwrap();
    }
    assert_eq!(
        store.list_dead("default", 10).await.unwrap().len(),
        1,
        "exhausted retries → dead-letter"
    );
    assert_eq!(
        runs.load(Ordering::SeqCst),
        3,
        "exactly max_attempts executions, then dead-letter"
    );
}

#[tokio::test]
async fn a_still_running_job_is_not_reclaimed_before_exec_timeout() {
    // THE lease invariant (the worker clamps the lease to >= exec_timeout): a job
    // whose handler is still running well past `lease_duration` but before
    // `exec_timeout` must NOT become re-leasable, or a concurrent worker loop
    // would reclaim it and run it a second time (silent duplicated work). The
    // worker leases with the EFFECTIVE lease `max(lease_duration, exec_timeout)`,
    // so a 5s `lease_duration` under a 60s `exec_timeout` actually holds the lease
    // for 60s. We drive `tick` to lease, then probe the store directly at a time
    // past `lease_duration` (5s) but before `exec_timeout` (60s) and assert the
    // job is still claimed (not re-leasable).
    let store: Arc<dyn JobStore> = Arc::new(InMemoryStore::new());
    // A handler that "runs forever" relative to this test: it parks so the job is
    // still in-flight when we probe. The probe drives the store directly (no
    // second worker loop), so we don't actually await this future to completion.
    let blocking: JobFn = Arc::new(|_ctx, _p| Box::pin(std::future::pending()));
    let mut dispatch = HashMap::new();
    dispatch.insert("slow".to_string(), blocking);

    let cfg = JobConfig {
        backoff_base: Duration::from_secs(1),
        backoff_cap: Duration::from_secs(60),
        // lease_duration deliberately SHORTER than exec_timeout: without the
        // clamp this job would be reclaimable after 5s, mid-run.
        lease_duration: Duration::from_secs(5),
        exec_timeout: Duration::from_secs(60),
        poll_interval: Duration::from_secs(1),
        batch: 10,
    };
    let worker = Worker::new(store.clone(), Arc::new(dispatch), cfg);
    let t = App::new().into_test();
    let base = t.task_context();

    store
        .enqueue(NewJob::new("slow", "default"), t0())
        .await
        .unwrap();

    // Lease the job by ticking ONCE under a bounded timeout: the handler parks
    // forever, so `tick` itself would hang waiting on the 60s exec_timeout. We
    // only need it to have CLAIMED the job (the lease is written before the
    // handler runs), so abandon the tick after a beat — the lease persists.
    let _ = tokio::time::timeout(
        std::time::Duration::from_millis(50),
        worker.tick("default", t0(), base.fork()),
    )
    .await;

    // Past `lease_duration` (5s) but BEFORE `exec_timeout` (60s): the job is still
    // running, so its EFFECTIVE 60s lease must still hold — NOT re-leasable.
    let probe = t0() + Duration::from_secs(6);
    assert!(
        store
            .lease("default", probe, Duration::from_secs(5), 10)
            .await
            .unwrap()
            .is_empty(),
        "a still-running job must NOT be reclaimed before exec_timeout: the worker \
         leases with the effective lease max(lease_duration, exec_timeout) = 60s, \
         not the 5s lease_duration"
    );

    // Sanity floor: past the effective lease (60s) a crashed worker's job IS
    // reclaimable (you can't distinguish a crash from a slow job before then).
    let after_effective = t0() + Duration::from_secs(61);
    assert_eq!(
        store
            .lease("default", after_effective, Duration::from_secs(5), 10)
            .await
            .unwrap()
            .len(),
        1,
        "past the effective lease the job is reclaimable (crash recovery)"
    );
}

/// The default config is self-consistent with the lease invariant: its lease
/// covers a job's whole `exec_timeout`, so even a never-clamped store could not
/// reclaim a still-running job under the defaults.
#[test]
fn default_lease_covers_exec_timeout() {
    let c = JobConfig::default();
    assert!(
        c.lease_duration >= c.exec_timeout,
        "JobConfig::default lease_duration ({:?}) must be >= exec_timeout ({:?}) \
         so the effective lease covers a job's max runtime",
        c.lease_duration,
        c.exec_timeout
    );
}

#[tokio::test]
async fn worker_acks_a_successful_job() {
    let store: Arc<dyn JobStore> = Arc::new(InMemoryStore::new());
    let ok: JobFn = Arc::new(|_ctx, _p| Box::pin(async { Ok(()) }));
    let mut dispatch = HashMap::new();
    dispatch.insert("good".to_string(), ok);
    let t = App::new().into_test();
    let base = t.task_context();
    let worker = Worker::new(store.clone(), Arc::new(dispatch), JobConfig::default());

    store
        .enqueue(NewJob::new("good", "default"), t0())
        .await
        .unwrap();
    assert_eq!(worker.tick("default", t0(), base.fork()).await.unwrap(), 1);

    // Acked → not re-leasable, not dead.
    assert!(
        store
            .lease(
                "default",
                t0() + Duration::from_secs(3600),
                Duration::from_secs(30),
                10
            )
            .await
            .unwrap()
            .is_empty(),
        "acked job is terminal"
    );
    assert!(store.list_dead("default", 10).await.unwrap().is_empty());
}

#[tokio::test]
async fn worker_dead_letters_a_job_with_no_registered_handler() {
    // A leased job whose name is not in the dispatch table cannot run; the
    // worker dead-letters it (operator visibility via eprintln) rather than
    // looping on it forever.
    let store: Arc<dyn JobStore> = Arc::new(InMemoryStore::new());
    let worker = Worker::new(
        store.clone(),
        Arc::new(HashMap::new()),
        JobConfig::default(),
    );
    let t = App::new().into_test();
    let base = t.task_context();

    store
        .enqueue(NewJob::new("unknown", "default"), t0())
        .await
        .unwrap();
    assert_eq!(worker.tick("default", t0(), base.fork()).await.unwrap(), 1);
    assert_eq!(
        store.list_dead("default", 10).await.unwrap().len(),
        1,
        "an unhandled job name is dead-lettered, not retried"
    );
}

#[tokio::test]
async fn cron_enqueues_once_per_due_tick_via_the_real_leader_path() {
    // Drive the REAL cron leader (`Jobs::cron_tick_once`) — the same logic the
    // on_serve loop runs — so the due_fire → idempotency-keyed enqueue → advance
    // path is covered end-to-end over the in-memory store.
    let store: Arc<dyn JobStore> = Arc::new(InMemoryStore::new());
    let jobs = Jobs::in_memory()
        .store(store.clone())
        .cron("hourly_job", "0 * * * *", "default"); // top of every hour

    // 01:01:30 — the most recent tick is 01:00, which has not fired yet.
    let t = SystemTime::UNIX_EPOCH + Duration::from_secs(3600 + 90);
    assert_eq!(
        jobs.cron_tick_once(t).await,
        1,
        "the 01:00 tick enqueued one job"
    );
    assert_eq!(
        store
            .lease("default", t, Duration::from_secs(30), 10)
            .await
            .unwrap()
            .len(),
        1,
        "exactly one hourly job is queued"
    );

    // A second poll in the same hour does NOT re-enqueue: last_fired advanced to
    // 01:00, so nothing new is due until 02:00.
    let t2 = SystemTime::UNIX_EPOCH + Duration::from_secs(3600 + 120);
    assert_eq!(
        jobs.cron_tick_once(t2).await,
        0,
        "no new tick until the next hour — leader does not double-enqueue"
    );

    // At 02:00 the next tick is due and enqueues again.
    let t3 = SystemTime::UNIX_EPOCH + Duration::from_secs(2 * 3600);
    assert_eq!(jobs.cron_tick_once(t3).await, 1, "the 02:00 tick is due");
}

#[tokio::test]
async fn cron_fires_on_sqlite_via_the_in_memory_leader() {
    // #49 regression: `Jobs::postgres` over sqlite — the dev DEFAULT backend —
    // must FIRE a due cron tick. Because the advisory-lock leader is
    // Postgres-only, sqlite cron is served by the in-memory single-process
    // leader routed through `cron_poll_once` (exactly what the on_serve loop
    // runs). Before the fix, `Jobs::postgres` set a pg leader unconditionally,
    // so on sqlite `cron_poll_once` routed to `PostgresStore::cron_tick`, which
    // short-circuits to Ok(0): a declared cron job compiled, passed `check`, and
    // SILENTLY never fired.
    let db = Db::connect("sqlite::memory:").await.unwrap();
    db.migrate(JOBS_MIGRATIONS).await.unwrap();

    // No `.store(..)` (that would clear the pg leader and hide the routing under
    // test): `Jobs::postgres` keeps its own durable PostgresStore over this Db.
    let jobs = Jobs::postgres(db.clone()).cron("hourly", "0 * * * *", "default");

    // 01:01:30 — the 01:00 tick is the most-recent due tick and has not fired.
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(3600 + 90);
    assert_eq!(
        jobs.cron_poll_once(now).await.unwrap(),
        1,
        "a due cron tick fires on sqlite (not the dead pg-leader Ok(0))"
    );

    // It landed in the DURABLE store and is leasable by a worker. The sqlite pool
    // is size 1, so this `db.clone()`-backed store shares the one in-memory
    // database the engine enqueued into.
    let store = PostgresStore::new(db);
    let leased = store
        .lease("default", now, Duration::from_secs(30), 10)
        .await
        .unwrap();
    assert_eq!(leased.len(), 1, "the fired cron job is durably enqueued");
    assert_eq!(leased[0].name, "hourly", "the enqueued job is the cron job");

    // A second poll in the same hour does NOT re-fire: last_fired advanced to
    // 01:00, so nothing new is due until 02:00 (no double-fire).
    let later = SystemTime::UNIX_EPOCH + Duration::from_secs(3600 + 120);
    assert_eq!(
        jobs.cron_poll_once(later).await.unwrap(),
        0,
        "the single-process leader does not double-fire within a tick window"
    );
}

#[tokio::test]
async fn cron_idempotency_key_blocks_a_double_enqueue_on_the_same_fire() {
    // Even if two leaders evaluate the SAME fire instant (e.g. a brief race), the
    // per-(job, fire) idempotency key makes the store no-op the duplicate, so
    // only one job lands on the queue.
    let store: Arc<dyn JobStore> = Arc::new(InMemoryStore::new());
    let fire_secs = 3600u64; // 01:00
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(fire_secs + 30);
    let fire = SystemTime::UNIX_EPOCH + Duration::from_secs(fire_secs);
    let key = format!("cron:hourly_job:{fire_secs}");

    // Two enqueues with the same fire-derived key.
    store
        .enqueue(
            NewJob::new("hourly_job", "default").idempotency_key(&key),
            now,
        )
        .await
        .unwrap();
    store
        .enqueue(
            NewJob::new("hourly_job", "default").idempotency_key(&key),
            now,
        )
        .await
        .unwrap();
    let _ = fire; // documents the instant the key encodes

    assert_eq!(
        store
            .lease("default", now, Duration::from_secs(30), 10)
            .await
            .unwrap()
            .len(),
        1,
        "the duplicate enqueue is a no-op — one job, not two"
    );
}
