//! Background job engine for jerrycan (spec §v2.3): at-least-once queues with
//! retries + dead-letter, cron with skip-missed semantics, run_at delayed jobs,
//! over a Postgres (default), in-memory, or Redis Streams store (the last behind
//! the `jobs-redis` feature, spec §v2.3b). <https://jerrycan.cc>
#![forbid(unsafe_code)]

pub mod cron;
pub mod postgres_store;
#[cfg(feature = "jobs-redis")]
pub mod redis_store;
pub mod store;
pub mod worker;

pub use cron::{CronError, CronSchedule, due_fire};
pub use postgres_store::{JOBS_CRON_ADVISORY_KEY, JOBS_MIGRATIONS, PostgresStore};
#[cfg(feature = "jobs-redis")]
pub use redis_store::RedisStore;
pub use store::{
    DEFAULT_MAX_ATTEMPTS, EnqueueOutcome, InMemoryStore, Job, JobFuture, JobStatus, JobStore,
    NewJob,
};
pub use worker::{JobConfig, JobFn, Worker};

use cron::CronSchedule as Schedule;
use jerrycan_core::{App, Extension};
use jerrycan_db::Db;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

/// The background job extension (spec §v2.3). Install with
/// `app.extend(Jobs::in_memory().queue("default", 4).register("send_email", f))`.
///
/// It provides a [`JobsHandle`] (so app handlers resolve `Dep<JobsHandle>` to
/// enqueue), spawns `concurrency` per-queue worker `on_serve` loops, and — if
/// any cron jobs are registered — a single cron-poller `on_serve` loop. The
/// store defaults to the in-memory reference; swap in Postgres/Redis via
/// [`Jobs::store`]. The cron poller is an in-memory single-process leader here;
/// the Postgres advisory-lock leader lands in a later task.
#[derive(Clone)]
pub struct Jobs {
    store: Arc<dyn JobStore>,
    config: JobConfig,
    /// job name → typed task fn, built via [`Jobs::register`].
    dispatch: Arc<HashMap<String, JobFn>>,
    /// (queue, concurrency): each spawns `concurrency` worker loops.
    queues: Vec<(String, u32)>,
    /// (job name, schedule, queue): the leader enqueues each due tick.
    crons: Vec<(String, Schedule, String)>,
    /// In-memory last-fired-per-cron (used by the in-memory leader only).
    cron_state: Arc<Mutex<HashMap<String, SystemTime>>>,
    /// When set, the engine is Postgres-backed: the cron poller uses the
    /// advisory-lock leader ([`PostgresStore::cron_tick`]) over the
    /// `jerrycan_jobs_cron` table instead of the in-memory single-process poller.
    pg_leader: Option<PostgresStore>,
}

impl Jobs {
    /// A jobs engine over the in-memory reference store with engine defaults and
    /// an empty dispatch/queue/cron set.
    pub fn in_memory() -> Self {
        Self {
            store: Arc::new(InMemoryStore::new()),
            config: JobConfig::default(),
            dispatch: Arc::new(HashMap::new()),
            queues: Vec::new(),
            crons: Vec::new(),
            cron_state: Arc::new(Mutex::new(HashMap::new())),
            pg_leader: None,
        }
    }

    /// A jobs engine over the durable [`PostgresStore`] (the production default).
    /// The store is shared as both the `JobStore` (workers + enqueue) and — when
    /// the `Db` is Postgres-backed — the cron leader, so the cron poller uses the
    /// `pg_advisory_xact_lock` leader instead of the in-memory single-process one.
    ///
    /// Call [`PostgresStore::migrate`] (or apply [`JOBS_MIGRATIONS`]) once before
    /// serving so the `jerrycan_jobs*` tables exist.
    pub fn postgres(db: Db) -> Self {
        let pg = PostgresStore::new(db);
        Self {
            store: Arc::new(pg.clone()),
            config: JobConfig::default(),
            dispatch: Arc::new(HashMap::new()),
            queues: Vec::new(),
            crons: Vec::new(),
            cron_state: Arc::new(Mutex::new(HashMap::new())),
            pg_leader: Some(pg),
        }
    }

    /// Use a custom store (e.g. the Redis store from a later task). This also
    /// clears any Postgres cron leader: a custom store falls back to the
    /// in-memory single-process cron poller.
    pub fn store(mut self, store: Arc<dyn JobStore>) -> Self {
        self.store = store;
        self.pg_leader = None;
        self
    }

    /// A jobs engine over the durable, multi-node [`RedisStore`] (spec §v2.3b).
    /// Like [`store`](Jobs::store) but typed: it clears any Postgres cron leader,
    /// so the cron poller uses the in-memory single-process leader whose
    /// duplicate cross-node ticks are collapsed by the store's atomic (Lua,
    /// `SET NX`) idempotency.
    #[cfg(feature = "jobs-redis")]
    pub fn redis(self, store: crate::redis_store::RedisStore) -> Self {
        self.store(Arc::new(store))
    }

    /// Replace the engine config wholesale (backoff/lease/exec/poll/batch).
    pub fn config(mut self, config: JobConfig) -> Self {
        self.config = config;
        self
    }

    /// Register a typed task fn under `name`. A later registration for the same
    /// name wins (last-write).
    pub fn register(mut self, name: &str, f: JobFn) -> Self {
        Arc::make_mut(&mut self.dispatch).insert(name.into(), f);
        self
    }

    /// Run `concurrency` worker loops on `queue` (floored at 1). The store's
    /// `lease` serializes claims, so the loops never double-run a job.
    pub fn queue(mut self, name: &str, concurrency: u32) -> Self {
        self.queues.push((name.into(), concurrency.max(1)));
        self
    }

    /// Register a cron-triggered job: the leader enqueues `job` on `queue` each
    /// due tick. The cron expression is parsed HERE — a bad expression is a
    /// build-time configuration error, so this **panics loudly** rather than
    /// silently dropping a schedule.
    pub fn cron(mut self, job: &str, expr: &str, queue: &str) -> Self {
        let schedule = Schedule::parse(expr)
            .unwrap_or_else(|e| panic!("jerrycan-jobs: invalid cron for job '{job}': {e}"));
        self.crons.push((job.into(), schedule, queue.into()));
        self
    }

    /// An enqueue handle to provide for app handlers.
    pub fn handle(&self) -> JobsHandle {
        JobsHandle {
            store: self.store.clone(),
        }
    }

    /// Evaluate every registered cron once against `now` (in-memory leader): for
    /// each cron whose most-recent tick is newly due, enqueue it on its queue
    /// (idempotency-keyed per `(job, fire)` so a double-tick can't double-enqueue)
    /// and advance its `last_fired`. Returns how many jobs were enqueued.
    ///
    /// This is the exact logic the cron-poller `on_serve` loop runs each
    /// interval, lifted to a testable async method. It NEVER holds the
    /// `cron_state` mutex across an `.await`: it collects the due tuples under
    /// the lock, drops it, enqueues each, then re-locks to record `last_fired`.
    pub async fn cron_tick_once(&self, now: SystemTime) -> usize {
        // Phase 1: under the lock, decide what is due. No await held here.
        struct Due {
            job: String,
            queue: String,
            fire: SystemTime,
            key: String,
        }
        let due: Vec<Due> = {
            let state = self.cron_state.lock().expect("cron state mutex poisoned");
            self.crons
                .iter()
                .filter_map(|(job, sched, queue)| {
                    let last = state.get(job).copied();
                    cron::due_fire(sched, last, now).map(|fire| Due {
                        job: job.clone(),
                        queue: queue.clone(),
                        fire,
                        key: cron_idempotency_key(job, fire),
                    })
                })
                .collect()
        };

        // Phase 2: enqueue each due job (await, lock NOT held). The idempotency
        // key makes a duplicate enqueue a store no-op, so a double-tick is safe.
        // Matches the Postgres leader exactly: `run_at = fire` (the job is due at
        // the tick instant, not `now`), only an `Inserted` counts toward
        // `enqueued` (a `Duplicate` means the tick already fired), and we record
        // which jobs to advance — only those whose enqueue SUCCEEDED, so a
        // transient store error (a fallible store, e.g. v2.3b Redis) leaves
        // `last_fired` unadvanced and the tick retries on the next poll.
        let mut enqueued = 0;
        let mut advanced: Vec<&Due> = Vec::with_capacity(due.len());
        for d in &due {
            let job = NewJob::new(&d.job, &d.queue)
                .idempotency_key(&d.key)
                .run_at(d.fire);
            match self.store.enqueue(job, now).await {
                Ok(EnqueueOutcome::Inserted(_)) => {
                    enqueued += 1;
                    advanced.push(d);
                }
                // A Duplicate means this tick already fired (idempotent no-op);
                // advancing is still correct, but it does NOT count as a new
                // enqueue (matching the Postgres rows-returned semantics).
                Ok(EnqueueOutcome::Duplicate(_)) => advanced.push(d),
                // An Err must NOT advance last_fired: a transient failure retries
                // next poll.
                Err(_) => {}
            }
        }

        // Phase 3: re-lock and advance last_fired only for the jobs whose enqueue
        // succeeded (Inserted or Duplicate).
        if !advanced.is_empty() {
            let mut state = self.cron_state.lock().expect("cron state mutex poisoned");
            for d in &advanced {
                state.insert(d.job.clone(), d.fire);
            }
        }
        enqueued
    }
}

/// The per-`(job, fire)` cron idempotency key. The store no-ops a second
/// enqueue with the same key, so two leader ticks landing on the same fire
/// instant enqueue the job exactly once.
fn cron_idempotency_key(job: &str, fire: SystemTime) -> String {
    // Keyed on epoch MILLIS to match the Postgres leader's encoding
    // (postgres_store.rs), so the in-memory and durable leaders mint identical
    // keys for the same fire instant — no divergence if a deployment ever reuses
    // a DB across backends.
    let millis = fire
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("cron:{job}:{millis}")
}

/// An enqueue handle for app handlers: resolve `Dep<JobsHandle>` and call
/// [`enqueue`](JobsHandle::enqueue). `now` is the caller's `clock.now()`.
pub struct JobsHandle {
    store: Arc<dyn JobStore>,
}

impl JobsHandle {
    /// Enqueue a job. A duplicate idempotency key is a no-op reporting the
    /// existing id (see [`EnqueueOutcome`]).
    pub async fn enqueue(
        &self,
        job: NewJob,
        now: SystemTime,
    ) -> jerrycan_core::Result<EnqueueOutcome> {
        self.store.enqueue(job, now).await
    }
}

impl Extension for Jobs {
    fn register(self, app: App) -> App {
        // App handlers resolve `Dep<JobsHandle>` to enqueue jobs.
        let mut app = app.provide(self.handle());

        // Per queue: `concurrency` worker loops. Each loop gets its own
        // TaskContext from core and forks a fresh one per job; the store's lease
        // serializes claims so concurrent loops never double-run a job.
        for (queue, concurrency) in &self.queues {
            for i in 0..*concurrency {
                let store = self.store.clone();
                let dispatch = self.dispatch.clone();
                let config = self.config.clone();
                let q = queue.clone();
                // on_serve wants a &'static str; the queue/index are build-time
                // config, so leaking one name per loop is bounded and fine.
                let name: &'static str = Box::leak(format!("jobs-worker-{q}-{i}").into_boxed_str());
                app = app.on_serve(name, move |mut ctx, mut shutdown| {
                    let store = store.clone();
                    let dispatch = dispatch.clone();
                    let poll = config.poll_interval;
                    let q = q.clone();
                    async move {
                        // Resolve the Clock from the SAME task context (honors
                        // test overrides); without it, skip the loop.
                        let clock = match ctx.resolve::<jerrycan_core::Clock>().await {
                            Ok(c) => c,
                            Err(_) => return,
                        };
                        let worker = Worker::new(store, dispatch, config);
                        loop {
                            // REAL tokio time for the poll cadence; the injected
                            // Clock supplies the `now` for due decisions.
                            match tokio::time::timeout(poll, shutdown.changed()).await {
                                Ok(_) => break, // shutdown fired
                                Err(_) => {
                                    // Pass an owned, forked (Send) context per
                                    // poll: `ctx.fork()` is a transient `&self`
                                    // borrow, so no `&TaskContext` crosses the
                                    // await and the loop future stays `Send`.
                                    // Log a tick failure (e.g. a DB outage in
                                    // `store.lease`) for operator visibility, but
                                    // keep looping — a transient blip must not kill
                                    // the worker.
                                    if let Err(e) = worker.tick(&q, clock.now(), ctx.fork()).await {
                                        eprintln!(
                                            "jerrycan-jobs: worker tick on queue '{q}' failed: {e}"
                                        );
                                    }
                                }
                            }
                        }
                    }
                });
            }
        }

        // The cron poller (one loop): each interval, evaluate every cron and
        // enqueue the newly-due ones. Postgres-backed engines use the
        // advisory-lock leader (one node polls at a time, enqueue + last_fired
        // atomic); every other backend uses the in-memory single-process leader.
        if !self.crons.is_empty() {
            let jobs = self.clone();
            let interval = self.config.poll_interval;
            app = app.on_serve("jobs-cron", move |mut ctx, mut shutdown| {
                let jobs = jobs.clone();
                async move {
                    let clock = match ctx.resolve::<jerrycan_core::Clock>().await {
                        Ok(c) => c,
                        Err(_) => return,
                    };
                    loop {
                        match tokio::time::timeout(interval, shutdown.changed()).await {
                            Ok(_) => break, // shutdown fired
                            Err(_) => {
                                // Log a cron tick failure (e.g. a DB outage in the
                                // advisory-lock leader) for operator visibility, but
                                // keep polling — a transient blip must not kill the
                                // cron loop. The in-memory leader is infallible
                                // (returns a plain count), so only the Postgres
                                // leader can surface an error here.
                                if let Some(pg) = &jobs.pg_leader {
                                    // Advisory-lock leader: the lock + enqueue +
                                    // last_fired all live in one transaction.
                                    if let Err(e) = pg.cron_tick(&jobs.crons, clock.now()).await {
                                        eprintln!("jerrycan-jobs: cron tick failed: {e}");
                                    }
                                } else {
                                    // The mutex is never held across this await:
                                    // cron_tick_once locks → drops → enqueues → re-locks.
                                    jobs.cron_tick_once(clock.now()).await;
                                }
                            }
                        }
                    }
                }
            });
        }

        app
    }
}
