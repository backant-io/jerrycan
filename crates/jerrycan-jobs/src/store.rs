//! The job store layer: the [`JobStore`] trait, its job record [`Job`], the
//! enqueue request builder [`NewJob`], the [`EnqueueOutcome`], and the
//! deterministic reference [`InMemoryStore`] (spec §v2.3).
//!
//! The store is **clock-free**: every time-sensitive method takes an explicit
//! `now: SystemTime`, so the worker (a later task) supplies `clock.now()` and
//! tests supply a fixed time. The trait is object-safe — used behind `dyn` —
//! so each method returns a hand-boxed [`JobFuture`] rather than `async fn`,
//! the same pattern as `jerrycan_ratelimit`'s `RateLimitStore`. The Postgres
//! (default) and Redis stores land in later tasks and reuse this contract.

use jerrycan_core::Result;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, SystemTime};

/// Default attempt budget for a job whose enqueue request leaves it unset:
/// the initial run plus four retries before dead-lettering.
pub const DEFAULT_MAX_ATTEMPTS: u32 = 5;

/// The boxed `Send` future every [`JobStore`] method returns. A hand-boxed
/// future keeps the trait object-safe without pulling in `async-trait`, and
/// naming it (rather than spelling the `Pin<Box<dyn ...>>` inline at every
/// method) keeps the trait clear of the `clippy::type_complexity` lint — the
/// same role `HitFuture` plays in `jerrycan_ratelimit`.
pub type JobFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// Lifecycle state of a persisted job.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobStatus {
    /// Waiting to run once `run_at <= now`.
    Pending,
    /// Claimed by a worker; reclaimable once its lease expires (at-least-once).
    Leased,
    /// Acknowledged complete; terminal.
    Done,
    /// Exhausted its attempts (or hard-failed) and parked in the dead-letter
    /// set; not leased again until an admin requeues it.
    Dead,
}

/// A persisted job row — the storage shape, **not** the frozen `JobDesign`
/// contract (which is fixed at name/schedule/queue). `run_at`, `attempts`,
/// `lease_expires_at`, and `idempotency_key` are runtime bookkeeping the store
/// owns.
#[derive(Clone, Debug)]
pub struct Job {
    pub id: i64,
    pub name: String,
    pub queue: String,
    pub payload: serde_json::Value,
    pub run_at: SystemTime,
    pub attempts: u32,
    pub max_attempts: u32,
    pub status: JobStatus,
    pub idempotency_key: Option<String>,
    pub lease_expires_at: Option<SystemTime>,
    pub created_at: SystemTime,
}

/// An enqueue request (builder). `run_at`, `idempotency_key`, and
/// `max_attempts` are **runtime** params supplied at enqueue time — they are
/// not part of the frozen `JobDesign` contract (name/schedule/queue).
#[derive(Clone, Debug)]
pub struct NewJob {
    pub name: String,
    pub queue: String,
    pub payload: serde_json::Value,
    pub run_at: Option<SystemTime>,
    pub idempotency_key: Option<String>,
    pub max_attempts: Option<u32>,
}

impl NewJob {
    /// A job for `name` on `queue` with a null payload, due immediately, no
    /// idempotency key, and the default attempt budget.
    pub fn new(name: &str, queue: &str) -> Self {
        Self {
            name: name.to_string(),
            queue: queue.to_string(),
            payload: serde_json::Value::Null,
            run_at: None,
            idempotency_key: None,
            max_attempts: None,
        }
    }

    /// Attach a JSON payload.
    pub fn payload(mut self, v: serde_json::Value) -> Self {
        self.payload = v;
        self
    }

    /// Delay the job until `at` (it is not leased before then).
    pub fn run_at(mut self, at: SystemTime) -> Self {
        self.run_at = Some(at);
        self
    }

    /// Deduplicate enqueues: a second enqueue with the same key is a no-op.
    pub fn idempotency_key(mut self, k: &str) -> Self {
        self.idempotency_key = Some(k.into());
        self
    }

    /// Override the attempt budget before dead-lettering.
    pub fn max_attempts(mut self, n: u32) -> Self {
        self.max_attempts = Some(n);
        self
    }
}

/// The outcome of an [`JobStore::enqueue`]. A duplicate idempotency key is a
/// **no-op** that reports the existing job's id, not an error.
#[derive(Clone, Copy, Debug)]
pub enum EnqueueOutcome {
    /// A fresh job was inserted; carries its id.
    Inserted(i64),
    /// An idempotency key already mapped to a job; carries that existing id.
    Duplicate(i64),
}

/// An at-least-once job store (spec §v2.3).
///
/// Object-safe (used behind `dyn`), so methods return a hand-boxed
/// [`JobFuture`] rather than `async fn`. The store is clock-free: every
/// time-sensitive method takes `now`, so the worker and tests supply the time.
pub trait JobStore: Send + Sync + 'static {
    /// Insert a job, or — if its idempotency key already maps to one — report
    /// the existing job's id without inserting.
    fn enqueue<'a>(&'a self, job: NewJob, now: SystemTime) -> JobFuture<'a, EnqueueOutcome>;

    /// Atomically claim up to `max` due jobs on `queue`. A job is due when it
    /// is `Pending` and `run_at <= now`, or `Leased` with an expired lease
    /// (`lease_expires_at < now`, i.e. a crashed worker — reclaimed, the
    /// at-least-once guarantee). Each claimed job's status becomes `Leased`,
    /// its `lease_expires_at` becomes `now + lease`, and its `attempts`
    /// increments.
    fn lease<'a>(
        &'a self,
        queue: &'a str,
        now: SystemTime,
        lease: Duration,
        max: u32,
    ) -> JobFuture<'a, Vec<Job>>;

    /// Acknowledge a job complete (status → `Done`).
    fn ack<'a>(&'a self, id: i64) -> JobFuture<'a, ()>;

    /// Reschedule a failed job for a backoff retry: status → `Pending`,
    /// `run_at` → `backoff_until` (not leasable until that time), lease cleared.
    fn retry<'a>(&'a self, id: i64, backoff_until: SystemTime) -> JobFuture<'a, ()>;

    /// Park a job in the dead-letter set (status → `Dead`); it is not leased
    /// again until [`JobStore::requeue_dead`].
    fn dead_letter<'a>(&'a self, id: i64) -> JobFuture<'a, ()>;

    /// List up to `limit` dead jobs on `queue`, in deterministic order.
    fn list_dead<'a>(&'a self, queue: &'a str, limit: u32) -> JobFuture<'a, Vec<Job>>;

    /// Requeue a dead job (an admin action): status → `Pending`, due
    /// immediately, attempts reset, lease cleared.
    fn requeue_dead<'a>(&'a self, id: i64) -> JobFuture<'a, ()>;
}

/// The mutable interior of [`InMemoryStore`], guarded by a single `Mutex` so a
/// `lease` claim is atomic (all-sync under the lock, no await held across it).
struct Inner {
    jobs: HashMap<i64, Job>,
    /// idempotency key → the job id it first inserted.
    keys: HashMap<String, i64>,
}

/// A std-only, process-local job store — the deterministic reference
/// implementation. Suitable for tests and single-node deployments; the
/// Postgres and Redis stores (later tasks) cover durability and multi-node.
pub struct InMemoryStore {
    inner: Mutex<Inner>,
    next_id: AtomicI64,
}

impl InMemoryStore {
    /// An empty store. Ids start at 1.
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                jobs: HashMap::new(),
                keys: HashMap::new(),
            }),
            next_id: AtomicI64::new(1),
        }
    }
}

impl Default for InMemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

impl JobStore for InMemoryStore {
    fn enqueue<'a>(&'a self, job: NewJob, now: SystemTime) -> JobFuture<'a, EnqueueOutcome> {
        Box::pin(async move {
            let mut inner = self.inner.lock().expect("jobs store mutex poisoned");

            // Idempotency: a known key is a no-op reporting the existing id.
            if let Some(key) = &job.idempotency_key
                && let Some(&existing) = inner.keys.get(key)
            {
                return Ok(EnqueueOutcome::Duplicate(existing));
            }

            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            let row = Job {
                id,
                name: job.name,
                queue: job.queue,
                payload: job.payload,
                run_at: job.run_at.unwrap_or(now),
                attempts: 0,
                max_attempts: job.max_attempts.unwrap_or(DEFAULT_MAX_ATTEMPTS),
                status: JobStatus::Pending,
                idempotency_key: job.idempotency_key.clone(),
                lease_expires_at: None,
                created_at: now,
            };
            if let Some(key) = job.idempotency_key {
                inner.keys.insert(key, id);
            }
            inner.jobs.insert(id, row);
            Ok(EnqueueOutcome::Inserted(id))
        })
    }

    fn lease<'a>(
        &'a self,
        queue: &'a str,
        now: SystemTime,
        lease: Duration,
        max: u32,
    ) -> JobFuture<'a, Vec<Job>> {
        Box::pin(async move {
            let mut inner = self.inner.lock().expect("jobs store mutex poisoned");

            // Collect due ids: Pending-and-due, or Leased-with-an-expired-lease
            // (a crashed worker — reclaimed for the at-least-once guarantee).
            let mut due: Vec<i64> = inner
                .jobs
                .values()
                .filter(|j| {
                    j.queue == queue
                        && match j.status {
                            JobStatus::Pending => j.run_at <= now,
                            JobStatus::Leased => j.lease_expires_at.is_some_and(|e| e < now),
                            JobStatus::Done | JobStatus::Dead => false,
                        }
                })
                .map(|j| j.id)
                .collect();

            // Deterministic claim order: by (run_at, id).
            due.sort_by(|a, b| {
                let ja = &inner.jobs[a];
                let jb = &inner.jobs[b];
                ja.run_at.cmp(&jb.run_at).then(ja.id.cmp(&jb.id))
            });
            due.truncate(max as usize);

            let leased = due
                .into_iter()
                .map(|id| {
                    let j = inner.jobs.get_mut(&id).expect("due id exists");
                    j.status = JobStatus::Leased;
                    j.lease_expires_at = Some(now + lease);
                    j.attempts += 1;
                    j.clone()
                })
                .collect();
            Ok(leased)
        })
    }

    fn ack<'a>(&'a self, id: i64) -> JobFuture<'a, ()> {
        Box::pin(async move {
            let mut inner = self.inner.lock().expect("jobs store mutex poisoned");
            if let Some(j) = inner.jobs.get_mut(&id) {
                j.status = JobStatus::Done;
            }
            Ok(())
        })
    }

    fn retry<'a>(&'a self, id: i64, backoff_until: SystemTime) -> JobFuture<'a, ()> {
        Box::pin(async move {
            let mut inner = self.inner.lock().expect("jobs store mutex poisoned");
            if let Some(j) = inner.jobs.get_mut(&id) {
                j.status = JobStatus::Pending;
                j.run_at = backoff_until;
                j.lease_expires_at = None;
            }
            Ok(())
        })
    }

    fn dead_letter<'a>(&'a self, id: i64) -> JobFuture<'a, ()> {
        Box::pin(async move {
            let mut inner = self.inner.lock().expect("jobs store mutex poisoned");
            if let Some(j) = inner.jobs.get_mut(&id) {
                j.status = JobStatus::Dead;
            }
            Ok(())
        })
    }

    fn list_dead<'a>(&'a self, queue: &'a str, limit: u32) -> JobFuture<'a, Vec<Job>> {
        Box::pin(async move {
            let inner = self.inner.lock().expect("jobs store mutex poisoned");
            let mut dead: Vec<Job> = inner
                .jobs
                .values()
                .filter(|j| j.status == JobStatus::Dead && j.queue == queue)
                .cloned()
                .collect();
            dead.sort_by_key(|j| j.id);
            dead.truncate(limit as usize);
            Ok(dead)
        })
    }

    fn requeue_dead<'a>(&'a self, id: i64) -> JobFuture<'a, ()> {
        Box::pin(async move {
            let mut inner = self.inner.lock().expect("jobs store mutex poisoned");
            if let Some(j) = inner.jobs.get_mut(&id) {
                j.status = JobStatus::Pending;
                // Immediately due regardless of the caller's clock — an admin
                // action, not a scheduled run.
                j.run_at = SystemTime::UNIX_EPOCH;
                j.attempts = 0;
                j.lease_expires_at = None;
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn t0() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000)
    }
    fn spec(name: &str) -> NewJob {
        NewJob::new(name, "default")
    }

    #[tokio::test]
    async fn at_least_once_a_crashed_lease_is_reclaimed() {
        let s = InMemoryStore::new();
        s.enqueue(spec("a"), t0()).await.unwrap();
        let lease = Duration::from_secs(30);
        let leased = s.lease("default", t0(), lease, 10).await.unwrap();
        assert_eq!(leased.len(), 1, "the due job is leased");
        assert!(
            s.lease("default", t0() + Duration::from_secs(5), lease, 10)
                .await
                .unwrap()
                .is_empty(),
            "not re-leasable before expiry"
        );
        let again = s
            .lease("default", t0() + lease + Duration::from_secs(1), lease, 10)
            .await
            .unwrap();
        assert_eq!(again.len(), 1, "an expired lease is reclaimed");
        assert_eq!(
            again[0].attempts, 2,
            "the reclaim counts as a second attempt"
        );
    }

    #[tokio::test]
    async fn retry_with_backoff_is_not_releasable_until_the_window_elapses() {
        let s = InMemoryStore::new();
        s.enqueue(spec("a"), t0()).await.unwrap();
        let job = s
            .lease("default", t0(), Duration::from_secs(30), 10)
            .await
            .unwrap()
            .pop()
            .unwrap();
        let backoff_until = t0() + Duration::from_secs(60);
        s.retry(job.id, backoff_until).await.unwrap();
        assert!(
            s.lease(
                "default",
                t0() + Duration::from_secs(30),
                Duration::from_secs(30),
                10
            )
            .await
            .unwrap()
            .is_empty(),
            "not due until backoff"
        );
        assert_eq!(
            s.lease(
                "default",
                backoff_until + Duration::from_secs(1),
                Duration::from_secs(30),
                10
            )
            .await
            .unwrap()
            .len(),
            1,
            "due after backoff"
        );
    }

    #[tokio::test]
    async fn dead_letter_holds_the_job_and_it_is_requeueable() {
        let s = InMemoryStore::new();
        s.enqueue(spec("a"), t0()).await.unwrap();
        let job = s
            .lease("default", t0(), Duration::from_secs(30), 10)
            .await
            .unwrap()
            .pop()
            .unwrap();
        s.dead_letter(job.id).await.unwrap();
        assert!(
            s.lease(
                "default",
                t0() + Duration::from_secs(3600),
                Duration::from_secs(30),
                10
            )
            .await
            .unwrap()
            .is_empty(),
            "dead jobs are not leased"
        );
        let dead = s.list_dead("default", 10).await.unwrap();
        assert_eq!(dead.len(), 1);
        s.requeue_dead(job.id).await.unwrap();
        assert_eq!(
            s.lease(
                "default",
                t0() + Duration::from_secs(3601),
                Duration::from_secs(30),
                10
            )
            .await
            .unwrap()
            .len(),
            1,
            "requeued dead job is leasable again"
        );
    }

    #[tokio::test]
    async fn idempotency_key_makes_a_duplicate_enqueue_a_no_op() {
        let s = InMemoryStore::new();
        let first = s
            .enqueue(spec("a").idempotency_key("k1"), t0())
            .await
            .unwrap();
        let second = s
            .enqueue(spec("a").idempotency_key("k1"), t0())
            .await
            .unwrap();
        assert!(matches!(first, EnqueueOutcome::Inserted(_)));
        assert!(
            matches!(second, EnqueueOutcome::Duplicate(_)),
            "same key is a no-op, not an error"
        );
        assert_eq!(
            s.lease("default", t0(), Duration::from_secs(30), 10)
                .await
                .unwrap()
                .len(),
            1,
            "only one job exists"
        );
    }

    #[tokio::test]
    async fn run_at_delays_until_due() {
        let s = InMemoryStore::new();
        let due = t0() + Duration::from_secs(3600);
        s.enqueue(spec("a").run_at(due), t0()).await.unwrap();
        assert!(
            s.lease("default", t0(), Duration::from_secs(30), 10)
                .await
                .unwrap()
                .is_empty(),
            "future job not yet due"
        );
        assert_eq!(
            s.lease(
                "default",
                due + Duration::from_secs(1),
                Duration::from_secs(30),
                10
            )
            .await
            .unwrap()
            .len(),
            1,
            "due after run_at"
        );
    }
}
