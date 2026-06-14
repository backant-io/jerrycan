//! The job worker (spec §v2.3): a pure poll-batch [`Worker::tick`] over a
//! [`JobStore`]. It leases due jobs, runs each typed task fn in a FRESH forked
//! [`TaskContext`] under a per-job timeout, then acks / retries-with-backoff /
//! dead-letters. The worker is **pure w.r.t. `now`**: the serve-time `on_serve`
//! loop passes `clock.now()`, tests pass a hand-advanced instant. Handlers are
//! at-least-once, **not** exactly-once — they MUST be idempotent.

use crate::store::{Job, JobStore};
use jerrycan_core::{Result, TaskContext};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// A typed job task fn, type-erased for the dispatch table. The engine forks a
/// fresh [`TaskContext`] per job and passes the JSON payload; the generated
/// typed stub deserializes it into its `{Name}Payload` (queue jobs) or ignores
/// it (cron).
pub type JobFn = Arc<
    dyn Fn(TaskContext, serde_json::Value) -> Pin<Box<dyn Future<Output = Result<()>> + Send>>
        + Send
        + Sync,
>;

/// Engine defaults (per-enqueue overrides live on `NewJob`; per-job
/// `max_attempts` is on the `Job` row). NOT design-declared — runtime config.
///
/// **Lease invariant:** the effective lease is `max(lease_duration, exec_timeout)`
/// — the lease MUST cover a job's max runtime so a still-running job is never
/// reclaimed and re-run by a concurrent worker; a crashed worker's job is
/// reclaimed after the effective lease (you cannot distinguish a crash from a
/// slow job before `exec_timeout` elapses). [`Worker::tick`] enforces this with
/// the `max(...)` clamp regardless of how `lease_duration` is configured;
/// [`JobConfig::default`] is additionally kept self-consistent
/// (`lease_duration >= exec_timeout`).
#[derive(Clone)]
pub struct JobConfig {
    /// Backoff floor: the first retry waits ~`backoff_base` (e.g. 1s).
    pub backoff_base: Duration,
    /// Backoff ceiling: exponential growth is capped here (e.g. 5min).
    pub backoff_cap: Duration,
    /// How long a lease is held before it is reclaimable. The *effective* lease
    /// is `max(lease_duration, exec_timeout)` (see the type-level invariant), so
    /// a still-running job is never reclaimed mid-flight.
    pub lease_duration: Duration,
    /// Per-job execution timeout (e.g. 5min). Background tasks get NO
    /// `handler_timeout`; this is their only wall-clock budget.
    pub exec_timeout: Duration,
    /// Real-time worker poll cadence (e.g. 1s).
    pub poll_interval: Duration,
    /// Jobs leased per tick (e.g. 10).
    pub batch: u32,
}

impl Default for JobConfig {
    fn default() -> Self {
        Self {
            backoff_base: Duration::from_secs(1),
            backoff_cap: Duration::from_secs(5 * 60),
            // 6min — strictly greater than `exec_timeout` (5min) so the default is
            // self-consistent with the lease invariant (a still-running job, bounded
            // by `exec_timeout`, can never have its lease reclaimed mid-flight).
            lease_duration: Duration::from_secs(6 * 60),
            exec_timeout: Duration::from_secs(5 * 60),
            poll_interval: Duration::from_secs(1),
            batch: 10,
        }
    }
}

/// A worker over one store and one dispatch table. Cloning the `Arc`s is cheap;
/// the per-queue `on_serve` loops each build their own `Worker`.
pub struct Worker {
    store: Arc<dyn JobStore>,
    dispatch: Arc<HashMap<String, JobFn>>,
    config: JobConfig,
}

impl Worker {
    /// Build a worker over `store`, dispatching by job name through `dispatch`,
    /// with engine defaults from `config`.
    pub fn new(
        store: Arc<dyn JobStore>,
        dispatch: Arc<HashMap<String, JobFn>>,
        config: JobConfig,
    ) -> Self {
        Self {
            store,
            dispatch,
            config,
        }
    }

    /// One poll-batch on `queue`: lease up to `batch` due jobs, run each typed
    /// task fn (in a FRESH forked [`TaskContext`]) under `exec_timeout`, then
    /// ack (Ok) / retry-with-backoff (Err, attempts < max) / dead-letter (Err,
    /// attempts >= max). PURE w.r.t. `now` (the `on_serve` loop passes
    /// `clock.now()`; tests pass a hand-advanced instant). Returns the number of
    /// jobs leased and processed this batch. Task handlers MUST be idempotent —
    /// at-least-once, not exactly-once.
    ///
    /// `base_ctx` is taken **by value**: an owned [`TaskContext`] is `Send` (its
    /// body lane is `Send` but `!Sync`), so this future stays `Send` and the
    /// `on_serve` loop can hold it across polls. Forking per job is a transient
    /// `&self` borrow that yields an owned, `Send` context moved into each task
    /// future — no `&TaskContext` ever lives across an `.await`.
    pub async fn tick(&self, queue: &str, now: SystemTime, base_ctx: TaskContext) -> Result<usize> {
        // The EFFECTIVE lease covers a job's whole max runtime: a still-running
        // job (bounded by `exec_timeout`) can never have its lease expire
        // mid-flight and be reclaimed + re-run by a concurrent worker loop. A
        // crashed worker's job still becomes reclaimable after this lease — you
        // cannot tell a crash from a slow job before `exec_timeout`. The
        // `max(...)` makes this hold regardless of how `lease_duration` is set.
        let effective_lease = self.config.lease_duration.max(self.config.exec_timeout);
        let jobs = self
            .store
            .lease(queue, now, effective_lease, self.config.batch)
            .await?;
        let n = jobs.len();
        for job in jobs {
            let Some(task) = self.dispatch.get(&job.name).cloned() else {
                eprintln!(
                    "jerrycan-jobs: JC0521 no handler for job '{}' (id {}) — dead-lettering",
                    job.name, job.id
                );
                self.store.dead_letter(job.id).await?;
                continue;
            };
            // A fresh dependency-resolution cache per job: cached deps never
            // leak between jobs (DI isolation).
            let ctx = base_ctx.fork();
            let fut = task(ctx, job.payload.clone());
            match tokio::time::timeout(self.config.exec_timeout, fut).await {
                Ok(Ok(())) => self.store.ack(job.id).await?,
                Ok(Err(_)) | Err(_ /* timeout */) => self.fail(&job, now).await?,
            }
        }
        Ok(n)
    }

    /// Handle a failed (or timed-out) job: dead-letter once its attempts are
    /// exhausted, otherwise reschedule for an exponential-backoff retry. The
    /// lease already incremented `attempts`, so `attempts >= max_attempts` means
    /// this was the final allowed run.
    async fn fail(&self, job: &Job, now: SystemTime) -> Result<()> {
        if job.attempts >= job.max_attempts {
            eprintln!(
                "jerrycan-jobs: JC0521 job '{}' (id {}) dead-lettered after {} attempts",
                job.name, job.id, job.attempts
            );
            self.store.dead_letter(job.id).await
        } else {
            let backoff = backoff_for(
                job.attempts,
                self.config.backoff_base,
                self.config.backoff_cap,
            );
            self.store.retry(job.id, now + backoff).await
        }
    }
}

/// Exponential backoff: `base * 2^(attempts-1)`, capped at `cap`. `attempts` is
/// 1-based (the lease already incremented it), so the 1st failure waits ~`base`.
/// The shift is clamped at 20 and the multiply saturates, so an absurd attempt
/// count cannot overflow — it just pins to `cap`.
fn backoff_for(attempts: u32, base: Duration, cap: Duration) -> Duration {
    let shift = attempts.saturating_sub(1).min(20);
    base.saturating_mul(1u32.checked_shl(shift).unwrap_or(u32::MAX))
        .min(cap)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_from_base_and_caps() {
        let base = Duration::from_secs(1);
        let cap = Duration::from_secs(60);
        // attempts is 1-based: 1 → base, 2 → 2*base, 3 → 4*base, ...
        assert_eq!(backoff_for(1, base, cap), Duration::from_secs(1));
        assert_eq!(backoff_for(2, base, cap), Duration::from_secs(2));
        assert_eq!(backoff_for(3, base, cap), Duration::from_secs(4));
        assert_eq!(backoff_for(4, base, cap), Duration::from_secs(8));
        // Growth is capped, and an absurd attempt count pins to the cap rather
        // than overflowing.
        assert_eq!(backoff_for(20, base, cap), cap, "capped");
        assert_eq!(backoff_for(u32::MAX, base, cap), cap, "saturates, no panic");
    }
}
