//! The durable [`PostgresStore`] (spec §v2.3): a [`JobStore`] over raw SQL via
//! [`jerrycan_db`], plus the framework migration ([`JOBS_MIGRATIONS`]) for the
//! jobs tables and the `pg_advisory_xact_lock` cron leader ([`PostgresStore::cron_tick`]).
//!
//! It is the **production default** store and MUST match the `InMemoryStore`
//! reference semantics in `store.rs` exactly — the two implementations cannot be
//! allowed to diverge. The key invariants it replicates:
//!
//! * **Lease** claims `(Pending AND run_at<=now) OR (Leased AND lease_expires_at<now)`,
//!   sets `Leased`, `lease_expires_at = now + lease`, `attempts += 1`, in
//!   `(run_at, id)` order. Postgres uses `FOR UPDATE SKIP LOCKED` for a race-free
//!   multi-worker claim; sqlite serializes through a single transaction (its
//!   single-writer lock).
//! * **Idempotency is PERMANENT** — a used `idempotency_key` forever no-ops to
//!   `Duplicate(existing_id)` (there is no key TTL).
//! * **`list_dead` orders by `id`** (matching the in-memory reference).
//! * **`requeue_dead`** resets `run_at` to the epoch (immediately due) and
//!   `attempts` to 0.
//!
//! ## Timestamp encoding
//! Times are stored as **BIGINT epoch-millis** (chrono-free and dialect-identical):
//! a `SystemTime` ↔ `i64` round-trip via `to_millis` / `from_millis`, mirroring
//! `jerrycan_ratelimit`'s window math. `status` is TEXT (`'pending'`/`'leased'`/
//! `'done'`/`'dead'`) for operator readability; `payload` is TEXT holding
//! `serde_json::Value::to_string()` (sqlite has no jsonb and Postgres TEXT is
//! dialect-identical, so one DDL shape covers both).

use crate::cron::{CronSchedule, due_fire};
use crate::store::{
    DEFAULT_MAX_ATTEMPTS, EnqueueOutcome, Job, JobFuture, JobStatus, JobStore, NewJob,
};
use jerrycan_core::Result;
use jerrycan_db::sea_orm::{ConnectionTrait, QueryResult, Statement, TransactionTrait, Value};
use jerrycan_db::{Backend, Db, Migration, db_error};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The framework migration for the jobs tables. Namespaced `jerrycan_jobs*` so it
/// never collides with a user module named `jobs`. Both dialects are kept to one
/// shape: BIGINT epoch-millis timestamps, TEXT status/payload, a partial unique
/// index on `idempotency_key` (so NULL keys never conflict on `ON CONFLICT`), and
/// a `(queue, status, run_at)` index for the lease scan.
pub const JOBS_MIGRATIONS: &[Migration] = &[Migration {
    name: "jerrycan_jobs_0001_create",
    // sqlite: a plain partial unique index also works (sqlite ≥3.8 supports
    // `WHERE`), and INTEGER PRIMARY KEY AUTOINCREMENT yields the rowid sequence.
    sqlite: "\
CREATE TABLE jerrycan_jobs (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    name             TEXT NOT NULL,
    queue            TEXT NOT NULL,
    payload          TEXT NOT NULL,
    run_at           BIGINT NOT NULL,
    attempts         INTEGER NOT NULL DEFAULT 0,
    max_attempts     INTEGER NOT NULL,
    status           TEXT NOT NULL DEFAULT 'pending',
    idempotency_key  TEXT,
    lease_expires_at BIGINT,
    created_at       BIGINT NOT NULL
);
CREATE UNIQUE INDEX jerrycan_jobs_idempotency_key
    ON jerrycan_jobs (idempotency_key) WHERE idempotency_key IS NOT NULL;
CREATE INDEX jerrycan_jobs_lease_scan
    ON jerrycan_jobs (queue, status, run_at);
CREATE TABLE jerrycan_jobs_cron (
    job        TEXT PRIMARY KEY,
    last_fired BIGINT NOT NULL
);",
    postgres: "\
CREATE TABLE jerrycan_jobs (
    id               BIGSERIAL PRIMARY KEY,
    name             TEXT NOT NULL,
    queue            TEXT NOT NULL,
    payload          TEXT NOT NULL,
    run_at           BIGINT NOT NULL,
    attempts         INTEGER NOT NULL DEFAULT 0,
    max_attempts     INTEGER NOT NULL,
    status           TEXT NOT NULL DEFAULT 'pending',
    idempotency_key  TEXT,
    lease_expires_at BIGINT,
    created_at       BIGINT NOT NULL
);
CREATE UNIQUE INDEX jerrycan_jobs_idempotency_key
    ON jerrycan_jobs (idempotency_key) WHERE idempotency_key IS NOT NULL;
CREATE INDEX jerrycan_jobs_lease_scan
    ON jerrycan_jobs (queue, status, run_at);
CREATE TABLE jerrycan_jobs_cron (
    job        TEXT PRIMARY KEY,
    last_fired BIGINT NOT NULL
);",
}];

/// The reserved 64-bit advisory-lock key for the cron leader. A single fixed
/// constant serializes the whole cron poller: only one instance holds
/// `pg_advisory_xact_lock(JOBS_CRON_ADVISORY_KEY)` at a time, so exactly one node
/// evaluates and enqueues due ticks per transaction. The lock auto-releases at
/// COMMIT (drain-safe). Value is an arbitrary jerrycan-cron magic constant; it is
/// the project's reserved advisory key (threat-model entry lands in Task 11).
pub const JOBS_CRON_ADVISORY_KEY: i64 = 0x6A_43_43_72_6F_6E_00_01; // "jCCron" + 0001

/// A `SystemTime` to epoch-millis (`i64`). Mirrors `jerrycan_ratelimit`'s window
/// math: a pre-epoch time (never produced by the engine) floors to 0.
pub(crate) fn to_millis(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Epoch-millis (`i64`) back to a `SystemTime`. The inverse of [`to_millis`];
/// negative values (never stored) clamp to the epoch.
pub(crate) fn from_millis(ms: i64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(ms.max(0) as u64)
}

/// Stored TEXT → [`JobStatus`]. An unrecognized value can only mean schema
/// corruption, so it fails loud rather than silently coercing to a state.
fn status_from(s: &str) -> Result<JobStatus> {
    match s {
        "pending" => Ok(JobStatus::Pending),
        "leased" => Ok(JobStatus::Leased),
        "done" => Ok(JobStatus::Done),
        "dead" => Ok(JobStatus::Dead),
        other => Err(jerrycan_core::Error::internal(format!(
            "jerrycan-jobs: unknown job status {other:?} in jerrycan_jobs"
        ))),
    }
}

/// Read an INTEGER column that sqlite/sqlx may type as `i32` or `i64` (the same
/// defensive read the platform schema introspection uses for PRAGMA ints).
fn col_i64(row: &QueryResult, col: &str) -> Result<i64> {
    row.try_get::<i64>("", col)
        .or_else(|_| row.try_get::<i32>("", col).map(i64::from))
        .map_err(|e| jerrycan_core::Error::internal(format!("jerrycan-jobs: column `{col}`: {e}")))
}

/// Read a nullable BIGINT column (`i32`/`i64` tolerant), mapping NULL to `None`.
fn col_opt_i64(row: &QueryResult, col: &str) -> Result<Option<i64>> {
    row.try_get::<Option<i64>>("", col)
        .or_else(|_| {
            row.try_get::<Option<i32>>("", col)
                .map(|o| o.map(i64::from))
        })
        .map_err(|e| jerrycan_core::Error::internal(format!("jerrycan-jobs: column `{col}`: {e}")))
}

/// Reconstruct a [`Job`] from a result row: BIGINT→`SystemTime`, status TEXT→enum,
/// payload TEXT→`serde_json::Value`. The single shared decoder for every read
/// path, so the column shape lives in exactly one place.
fn row_to_job(row: &QueryResult) -> Result<Job> {
    let payload_str: String = row.try_get("", "payload").map_err(|e| {
        jerrycan_core::Error::internal(format!("jerrycan-jobs: column `payload`: {e}"))
    })?;
    let payload = serde_json::from_str(&payload_str).map_err(|e| {
        jerrycan_core::Error::internal(format!("jerrycan-jobs: payload is not valid JSON: {e}"))
    })?;
    let status_str: String = row.try_get("", "status").map_err(|e| {
        jerrycan_core::Error::internal(format!("jerrycan-jobs: column `status`: {e}"))
    })?;
    let name: String = row.try_get("", "name").map_err(|e| {
        jerrycan_core::Error::internal(format!("jerrycan-jobs: column `name`: {e}"))
    })?;
    let queue: String = row.try_get("", "queue").map_err(|e| {
        jerrycan_core::Error::internal(format!("jerrycan-jobs: column `queue`: {e}"))
    })?;
    let idempotency_key: Option<String> = row.try_get("", "idempotency_key").map_err(|e| {
        jerrycan_core::Error::internal(format!("jerrycan-jobs: column `idempotency_key`: {e}"))
    })?;

    Ok(Job {
        id: col_i64(row, "id")?,
        name,
        queue,
        payload,
        run_at: from_millis(col_i64(row, "run_at")?),
        attempts: col_i64(row, "attempts")? as u32,
        max_attempts: col_i64(row, "max_attempts")? as u32,
        status: status_from(&status_str)?,
        idempotency_key,
        lease_expires_at: col_opt_i64(row, "lease_expires_at")?.map(from_millis),
        created_at: from_millis(col_i64(row, "created_at")?),
    })
}

/// The column list every read path selects, in the order [`row_to_job`] expects.
const JOB_COLUMNS: &str = "id,name,queue,payload,run_at,attempts,max_attempts,status,idempotency_key,lease_expires_at,created_at";

/// A durable [`JobStore`] over Postgres (the production default) or sqlite, via
/// [`jerrycan_db`]. Construct with [`PostgresStore::new`] (an existing [`Db`]) or
/// [`PostgresStore::connect`] (by URL), then [`migrate`](PostgresStore::migrate)
/// to apply [`JOBS_MIGRATIONS`].
///
/// Despite the name it speaks both dialects — the lease is the only method whose
/// SQL branches on the backend (Postgres `SKIP LOCKED` vs. a serialized sqlite
/// transaction). This lets the sqlite backend give the durable store deterministic
/// test coverage without a live Postgres.
#[derive(Clone)]
pub struct PostgresStore {
    db: Db,
}

impl PostgresStore {
    /// Wrap an existing [`Db`] handle (shares its connection pool).
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// Connect by URL (`postgres://…` or `sqlite::memory:`), mirroring
    /// [`Db::connect`].
    pub async fn connect(url: &str) -> Result<Self> {
        Ok(Self {
            db: Db::connect(url).await?,
        })
    }

    /// The underlying [`Db`] handle.
    pub fn db(&self) -> &Db {
        &self.db
    }

    /// Apply [`JOBS_MIGRATIONS`] (idempotent — the tracking table skips applied
    /// names).
    pub async fn migrate(&self) -> Result<()> {
        self.db.migrate(JOBS_MIGRATIONS).await?;
        Ok(())
    }

    /// The sea-orm backend tag for constructing a [`Statement`].
    fn backend_db(&self) -> jerrycan_db::sea_orm::DatabaseBackend {
        match self.db.backend() {
            Backend::Sqlite => jerrycan_db::sea_orm::DatabaseBackend::Sqlite,
            Backend::Postgres => jerrycan_db::sea_orm::DatabaseBackend::Postgres,
        }
    }

    /// Build a backend-correct [`Statement`] from a `?`-placeholder query and its
    /// binds (`?`→`$n` for Postgres via [`Db::sql`]).
    fn stmt(&self, sql: &str, binds: Vec<Value>) -> Statement {
        Statement::from_sql_and_values(self.backend_db(), self.db.sql(sql), binds)
    }

    /// The cron leader (Postgres-only). In ONE transaction: take the advisory lock
    /// [`JOBS_CRON_ADVISORY_KEY`] (so only one node polls at a time), then for each
    /// `(job, schedule, queue)` read its `last_fired`, compute [`due_fire`], and if
    /// a tick is due enqueue the job (idempotency-keyed `cron:{job}:{fire_millis}`,
    /// so a double-fire is impossible) AND upsert `last_fired = fire` — all
    /// atomically. The advisory lock auto-releases at COMMIT (drain-safe), and the
    /// enqueue + `last_fired` write share the transaction so a crash can never
    /// half-apply (no lost or double fire). Returns how many jobs were enqueued.
    ///
    /// **First-run policy:** a NULL `last_fired` row makes `due_fire(None, …)` fire
    /// the most-recent tick once (a deploy fires the most recent missed tick). This
    /// is the intended pure semantics; `last_fired` is NOT seeded to `now` silently.
    ///
    /// On a non-Postgres backend this is a no-op returning 0 — the in-memory
    /// single-process poller (`Jobs::cron_tick_once`) is the leader there.
    pub async fn cron_tick(
        &self,
        crons: &[(String, CronSchedule, String)],
        now: SystemTime,
    ) -> Result<usize> {
        if self.db.backend() != Backend::Postgres {
            return Ok(0);
        }
        let now_ms = to_millis(now);
        let crons = crons.to_vec();
        self.db
            .conn()
            .transaction::<_, usize, jerrycan_db::sea_orm::DbErr>(move |txn| {
                Box::pin(async move {
                    // Serialize the whole poller: only one node holds this lock.
                    txn.execute(Statement::from_sql_and_values(
                        txn.get_database_backend(),
                        "SELECT pg_advisory_xact_lock($1)",
                        [JOBS_CRON_ADVISORY_KEY.into()],
                    ))
                    .await?;

                    let mut enqueued = 0usize;
                    for (job, schedule, queue) in &crons {
                        // last_fired (NULL row → None).
                        let row = txn
                            .query_one(Statement::from_sql_and_values(
                                txn.get_database_backend(),
                                "SELECT last_fired FROM jerrycan_jobs_cron WHERE job = $1",
                                [job.clone().into()],
                            ))
                            .await?;
                        let last_fired = match row {
                            Some(r) => Some(from_millis(r.try_get::<i64>("", "last_fired")?)),
                            None => None,
                        };

                        let Some(fire) = due_fire(schedule, last_fired, now) else {
                            continue;
                        };
                        let fire_ms = to_millis(fire);
                        let key = format!("cron:{job}:{fire_ms}");

                        // Enqueue (idempotency-keyed): a double-fire is a no-op.
                        let inserted = txn
                            .query_all(Statement::from_sql_and_values(
                                txn.get_database_backend(),
                                "INSERT INTO jerrycan_jobs \
                                 (name,queue,payload,run_at,attempts,max_attempts,status,idempotency_key,created_at) \
                                 VALUES ($1,$2,$3,$4,0,$5,'pending',$6,$7) \
                                 ON CONFLICT (idempotency_key) WHERE idempotency_key IS NOT NULL DO NOTHING RETURNING id",
                                [
                                    job.clone().into(),
                                    queue.clone().into(),
                                    "null".into(),
                                    fire_ms.into(),
                                    (DEFAULT_MAX_ATTEMPTS as i32).into(),
                                    key.into(),
                                    now_ms.into(),
                                ],
                            ))
                            .await?;
                        if !inserted.is_empty() {
                            enqueued += 1;
                        }

                        // Advance last_fired in the SAME transaction (atomic with
                        // the enqueue: a crash can't lose or double-fire).
                        txn.execute(Statement::from_sql_and_values(
                            txn.get_database_backend(),
                            "INSERT INTO jerrycan_jobs_cron (job, last_fired) VALUES ($1, $2) \
                             ON CONFLICT (job) DO UPDATE SET last_fired = EXCLUDED.last_fired",
                            [job.clone().into(), fire_ms.into()],
                        ))
                        .await?;
                    }
                    Ok(enqueued)
                })
            })
            .await
            .map_err(|e| match e {
                jerrycan_db::sea_orm::TransactionError::Connection(db) => db_error(db),
                jerrycan_db::sea_orm::TransactionError::Transaction(db) => db_error(db),
            })
    }
}

impl JobStore for PostgresStore {
    fn enqueue<'a>(&'a self, job: NewJob, now: SystemTime) -> JobFuture<'a, EnqueueOutcome> {
        Box::pin(async move {
            let run_at = to_millis(job.run_at.unwrap_or(now));
            let max_attempts = job.max_attempts.unwrap_or(DEFAULT_MAX_ATTEMPTS) as i32;
            let payload = job.payload.to_string();
            let created_at = to_millis(now);

            // Without an idempotency key there is no conflict target: a plain
            // INSERT … RETURNING id always inserts.
            let Some(key) = job.idempotency_key.clone() else {
                let row = self
                    .db
                    .conn()
                    .query_one(self.stmt(
                        "INSERT INTO jerrycan_jobs \
                         (name,queue,payload,run_at,attempts,max_attempts,status,idempotency_key,created_at) \
                         VALUES (?,?,?,?,0,?,'pending',NULL,?) RETURNING id",
                        vec![
                            job.name.clone().into(),
                            job.queue.clone().into(),
                            payload.into(),
                            run_at.into(),
                            max_attempts.into(),
                            created_at.into(),
                        ],
                    ))
                    .await
                    .map_err(db_error)?
                    .ok_or_else(|| {
                        jerrycan_core::Error::internal("jerrycan-jobs: INSERT RETURNING id gave no row")
                    })?;
                return Ok(EnqueueOutcome::Inserted(col_i64(&row, "id")?));
            };

            // With a key: ON CONFLICT DO NOTHING. A row back → fresh insert; no row
            // → the key already maps to a job, so report the existing id (a
            // permanent, no-TTL dedup — matching the in-memory reference).
            let inserted = self
                .db
                .conn()
                .query_all(self.stmt(
                    "INSERT INTO jerrycan_jobs \
                     (name,queue,payload,run_at,attempts,max_attempts,status,idempotency_key,created_at) \
                     VALUES (?,?,?,?,0,?,'pending',?,?) \
                     ON CONFLICT (idempotency_key) WHERE idempotency_key IS NOT NULL DO NOTHING RETURNING id",
                    vec![
                        job.name.clone().into(),
                        job.queue.clone().into(),
                        payload.into(),
                        run_at.into(),
                        max_attempts.into(),
                        key.clone().into(),
                        created_at.into(),
                    ],
                ))
                .await
                .map_err(db_error)?;
            if let Some(row) = inserted.first() {
                return Ok(EnqueueOutcome::Inserted(col_i64(row, "id")?));
            }

            let existing = self
                .db
                .conn()
                .query_one(self.stmt(
                    "SELECT id FROM jerrycan_jobs WHERE idempotency_key = ?",
                    vec![key.into()],
                ))
                .await
                .map_err(db_error)?
                .ok_or_else(|| {
                    jerrycan_core::Error::internal(
                        "jerrycan-jobs: idempotency conflict but no existing row",
                    )
                })?;
            Ok(EnqueueOutcome::Duplicate(col_i64(&existing, "id")?))
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
            let now_ms = to_millis(now);
            let expires = to_millis(now + lease);
            let limit = max as i64;

            match self.db.backend() {
                // Postgres: one atomic SKIP LOCKED claim. The inner SELECT picks
                // due ids in (run_at, id) order and locks them; concurrent workers
                // skip already-locked rows, so no job is double-claimed.
                Backend::Postgres => {
                    let sql = format!(
                        "UPDATE jerrycan_jobs SET status='leased', lease_expires_at=$1, attempts=attempts+1 \
                         WHERE id IN ( \
                           SELECT id FROM jerrycan_jobs \
                           WHERE queue=$2 AND ((status='pending' AND run_at<=$3) OR (status='leased' AND lease_expires_at<$3)) \
                           ORDER BY run_at, id \
                           FOR UPDATE SKIP LOCKED \
                           LIMIT $4 \
                         ) RETURNING {JOB_COLUMNS}"
                    );
                    let rows = self
                        .db
                        .conn()
                        .query_all(Statement::from_sql_and_values(
                            self.backend_db(),
                            sql,
                            [expires.into(), queue.into(), now_ms.into(), limit.into()],
                        ))
                        .await
                        .map_err(db_error)?;
                    rows.iter().map(row_to_job).collect()
                }
                // Sqlite has no SKIP LOCKED, but its single-writer lock serializes
                // writers: a transaction (SELECT due ids → UPDATE them → re-SELECT)
                // is atomic against any other writer, so the claim is still
                // exclusive. This is the deterministic-coverage path.
                Backend::Sqlite => {
                    let queue = queue.to_string();
                    self.db
                        .conn()
                        .transaction::<_, Vec<Job>, jerrycan_db::sea_orm::DbErr>(move |txn| {
                            Box::pin(async move {
                                let due = txn
                                    .query_all(Statement::from_sql_and_values(
                                        jerrycan_db::sea_orm::DatabaseBackend::Sqlite,
                                        "SELECT id FROM jerrycan_jobs \
                                         WHERE queue=? AND ((status='pending' AND run_at<=?) OR (status='leased' AND lease_expires_at<?)) \
                                         ORDER BY run_at, id LIMIT ?",
                                        [
                                            queue.clone().into(),
                                            now_ms.into(),
                                            now_ms.into(),
                                            limit.into(),
                                        ],
                                    ))
                                    .await?;
                                let ids: Vec<i64> = due
                                    .iter()
                                    .map(|r| {
                                        r.try_get::<i64>("", "id")
                                            .or_else(|_| r.try_get::<i32>("", "id").map(i64::from))
                                    })
                                    .collect::<std::result::Result<_, _>>()?;
                                if ids.is_empty() {
                                    return Ok(Vec::new());
                                }
                                let placeholders =
                                    ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
                                let mut binds: Vec<Value> = vec![expires.into()];
                                binds.extend(ids.iter().map(|id| (*id).into()));
                                txn.execute(Statement::from_sql_and_values(
                                    jerrycan_db::sea_orm::DatabaseBackend::Sqlite,
                                    format!(
                                        "UPDATE jerrycan_jobs SET status='leased', lease_expires_at=?, attempts=attempts+1 \
                                         WHERE id IN ({placeholders})"
                                    ),
                                    binds,
                                ))
                                .await?;
                                // Re-SELECT the updated rows, preserving the
                                // (run_at, id) claim order.
                                let mut sel_binds: Vec<Value> = Vec::new();
                                sel_binds.extend(ids.iter().map(|id| (*id).into()));
                                let rows = txn
                                    .query_all(Statement::from_sql_and_values(
                                        jerrycan_db::sea_orm::DatabaseBackend::Sqlite,
                                        format!(
                                            "SELECT {JOB_COLUMNS} FROM jerrycan_jobs \
                                             WHERE id IN ({placeholders}) ORDER BY run_at, id"
                                        ),
                                        sel_binds,
                                    ))
                                    .await?;
                                rows.iter()
                                    .map(|r| {
                                        row_to_job(r).map_err(|e| {
                                            jerrycan_db::sea_orm::DbErr::Custom(e.message().to_string())
                                        })
                                    })
                                    .collect::<std::result::Result<Vec<Job>, _>>()
                            })
                        })
                        .await
                        .map_err(|e| match e {
                            jerrycan_db::sea_orm::TransactionError::Connection(db) => db_error(db),
                            jerrycan_db::sea_orm::TransactionError::Transaction(db) => db_error(db),
                        })
                }
            }
        })
    }

    fn ack<'a>(&'a self, id: i64) -> JobFuture<'a, ()> {
        Box::pin(async move {
            self.db
                .conn()
                .execute(self.stmt(
                    "UPDATE jerrycan_jobs SET status='done' WHERE id=?",
                    vec![id.into()],
                ))
                .await
                .map_err(db_error)?;
            Ok(())
        })
    }

    fn retry<'a>(&'a self, id: i64, backoff_until: SystemTime) -> JobFuture<'a, ()> {
        Box::pin(async move {
            self.db
                .conn()
                .execute(self.stmt(
                    "UPDATE jerrycan_jobs SET status='pending', run_at=?, lease_expires_at=NULL WHERE id=?",
                    vec![to_millis(backoff_until).into(), id.into()],
                ))
                .await
                .map_err(db_error)?;
            Ok(())
        })
    }

    fn dead_letter<'a>(&'a self, id: i64) -> JobFuture<'a, ()> {
        Box::pin(async move {
            self.db
                .conn()
                .execute(self.stmt(
                    "UPDATE jerrycan_jobs SET status='dead' WHERE id=?",
                    vec![id.into()],
                ))
                .await
                .map_err(db_error)?;
            Ok(())
        })
    }

    fn list_dead<'a>(&'a self, queue: &'a str, limit: u32) -> JobFuture<'a, Vec<Job>> {
        Box::pin(async move {
            let rows = self
                .db
                .conn()
                .query_all(self.stmt(
                    &format!(
                        "SELECT {JOB_COLUMNS} FROM jerrycan_jobs \
                         WHERE queue=? AND status='dead' ORDER BY id LIMIT ?"
                    ),
                    vec![queue.into(), (limit as i64).into()],
                ))
                .await
                .map_err(db_error)?;
            rows.iter().map(row_to_job).collect()
        })
    }

    fn requeue_dead<'a>(&'a self, id: i64) -> JobFuture<'a, ()> {
        Box::pin(async move {
            // run_at=0 (epoch) → immediately due; attempts reset to 0 — matching
            // the in-memory reference's admin requeue.
            self.db
                .conn()
                .execute(self.stmt(
                    "UPDATE jerrycan_jobs SET status='pending', run_at=0, attempts=0, lease_expires_at=NULL WHERE id=?",
                    vec![id.into()],
                ))
                .await
                .map_err(db_error)?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    //! The sqlite backend gives the durable [`PostgresStore`] deterministic
    //! coverage WITHOUT a live Postgres: it runs the same enqueue/lease/ack/
    //! idempotency/run_at/dead-letter sequences the in-memory contract tests use,
    //! asserting the sqlite-path SQL matches the `InMemoryStore` reference
    //! semantics. (The Postgres SKIP-LOCKED path is exercised by the ignored
    //! integration test in `tests/postgres_store.rs`.)
    use super::*;
    use crate::store::EnqueueOutcome;

    async fn store() -> PostgresStore {
        let s = PostgresStore::connect("sqlite::memory:").await.unwrap();
        s.migrate().await.unwrap();
        s
    }

    fn t0() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_000_000)
    }
    fn spec(name: &str) -> NewJob {
        NewJob::new(name, "default")
    }

    #[tokio::test]
    async fn millis_round_trip_is_exact_to_the_millisecond() {
        let t = UNIX_EPOCH + Duration::from_millis(1_234_567_890);
        assert_eq!(from_millis(to_millis(t)), t, "epoch-millis round-trips");
    }

    #[tokio::test]
    async fn migrate_is_idempotent() {
        let s = store().await;
        // A second migrate applies nothing and does not error.
        s.migrate().await.unwrap();
    }

    #[tokio::test]
    async fn at_least_once_a_crashed_lease_is_reclaimed() {
        let s = store().await;
        s.enqueue(spec("a"), t0()).await.unwrap();
        let lease = Duration::from_secs(30);
        let leased = s.lease("default", t0(), lease, 10).await.unwrap();
        assert_eq!(leased.len(), 1, "the due job is leased");
        assert_eq!(leased[0].attempts, 1, "lease counts as the first attempt");
        assert_eq!(leased[0].status, JobStatus::Leased);
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
        let s = store().await;
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
        let s = store().await;
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
        assert_eq!(dead[0].status, JobStatus::Dead);
        s.requeue_dead(job.id).await.unwrap();
        let requeued = s
            .lease(
                "default",
                t0() + Duration::from_secs(3601),
                Duration::from_secs(30),
                10,
            )
            .await
            .unwrap();
        assert_eq!(requeued.len(), 1, "requeued dead job is leasable again");
        assert_eq!(
            requeued[0].attempts, 1,
            "attempts reset then this lease is #1"
        );
    }

    #[tokio::test]
    async fn idempotency_key_makes_a_duplicate_enqueue_a_no_op() {
        let s = store().await;
        let first = s
            .enqueue(spec("a").idempotency_key("k1"), t0())
            .await
            .unwrap();
        let second = s
            .enqueue(spec("a").idempotency_key("k1"), t0())
            .await
            .unwrap();
        let EnqueueOutcome::Inserted(id) = first else {
            panic!("first enqueue must insert");
        };
        assert!(
            matches!(second, EnqueueOutcome::Duplicate(d) if d == id),
            "same key is a no-op reporting the existing id, not an error"
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
    async fn idempotency_dedup_is_permanent_even_after_the_job_completes() {
        // The in-memory reference never expires a used key; the durable store must
        // not either. A key stays a no-op even after its job is acked.
        let s = store().await;
        let EnqueueOutcome::Inserted(id) = s
            .enqueue(spec("a").idempotency_key("once"), t0())
            .await
            .unwrap()
        else {
            panic!("first insert");
        };
        let job = s
            .lease("default", t0(), Duration::from_secs(30), 10)
            .await
            .unwrap()
            .pop()
            .unwrap();
        s.ack(job.id).await.unwrap();
        let again = s
            .enqueue(spec("a").idempotency_key("once"), t0())
            .await
            .unwrap();
        assert!(
            matches!(again, EnqueueOutcome::Duplicate(d) if d == id),
            "a used idempotency key forever no-ops (permanent dedup, no TTL)"
        );
    }

    #[tokio::test]
    async fn run_at_delays_until_due() {
        let s = store().await;
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

    #[tokio::test]
    async fn lease_respects_max_and_orders_by_run_at_then_id() {
        let s = store().await;
        // Three jobs, enqueued out of run_at order; lease must claim the two
        // earliest by (run_at, id).
        s.enqueue(spec("c").run_at(t0() + Duration::from_secs(20)), t0())
            .await
            .unwrap();
        s.enqueue(spec("a").run_at(t0()), t0()).await.unwrap();
        s.enqueue(spec("b").run_at(t0() + Duration::from_secs(10)), t0())
            .await
            .unwrap();
        let leased = s
            .lease(
                "default",
                t0() + Duration::from_secs(30),
                Duration::from_secs(30),
                2,
            )
            .await
            .unwrap();
        assert_eq!(leased.len(), 2, "max=2 caps the batch");
        assert_eq!(leased[0].name, "a", "earliest run_at first");
        assert_eq!(leased[1].name, "b", "then the next-earliest");
    }

    #[tokio::test]
    async fn payload_round_trips_through_text_json() {
        let s = store().await;
        let payload = serde_json::json!({"to": "x@example.com", "n": 7});
        s.enqueue(spec("a").payload(payload.clone()), t0())
            .await
            .unwrap();
        let leased = s
            .lease("default", t0(), Duration::from_secs(30), 10)
            .await
            .unwrap();
        assert_eq!(
            leased[0].payload, payload,
            "JSON payload survives TEXT storage"
        );
    }

    #[tokio::test]
    async fn list_dead_is_scoped_to_queue_and_ordered_by_id() {
        let s = store().await;
        // Two dead jobs on "default", one on "other"; list_dead must return only
        // the "default" pair, ordered by id.
        for name in ["a", "b"] {
            s.enqueue(spec(name), t0()).await.unwrap();
        }
        s.enqueue(NewJob::new("c", "other"), t0()).await.unwrap();
        // Lease and dead-letter everything.
        for q in ["default", "other"] {
            let leased = s.lease(q, t0(), Duration::from_secs(30), 10).await.unwrap();
            for j in leased {
                s.dead_letter(j.id).await.unwrap();
            }
        }
        let dead = s.list_dead("default", 10).await.unwrap();
        assert_eq!(dead.len(), 2, "only the two default-queue dead jobs");
        assert!(dead[0].id < dead[1].id, "ordered by id");
        assert!(dead.iter().all(|j| j.queue == "default"));
    }

    #[tokio::test]
    async fn cron_tick_is_a_noop_on_non_postgres() {
        // The advisory-lock leader is Postgres-only; on sqlite it must not run (no
        // pg_advisory_xact_lock) and simply returns 0.
        let s = store().await;
        let sched = CronSchedule::parse("0 * * * *").unwrap();
        let crons = vec![("hourly".to_string(), sched, "default".to_string())];
        let n = s.cron_tick(&crons, t0()).await.unwrap();
        assert_eq!(n, 0, "cron_tick is a no-op on non-Postgres backends");
    }
}
