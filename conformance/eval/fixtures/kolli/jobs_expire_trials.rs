//! Background job `expire_trials` (cron, queue `billing`). Agent-owned.
//! Regeneration never clobbers this file.
//!
//! Idempotent real work: UPSERTs a heartbeat into a self-owned `job_audit` table
//! (created if absent). Jobs run AT-LEAST-ONCE, so the effect is an overwrite of
//! the same row — never a bare insert — and a re-run is harmless.

use jerrycan::TaskContext;
use jerrycan::db::Db;
use jerrycan::db::sea_orm::{ConnectionTrait, Statement};

/// The `expire_trials` cron task. The leader enqueues it each due tick.
pub async fn expire_trials(mut ctx: TaskContext) -> jerrycan::Result<()> {
    let db = ctx.resolve::<Db>().await?;
    heartbeat(&db, "expire_trials").await
}

/// Ensure the audit table exists and UPSERT this job's marker. Portable across
/// sqlite and postgres (both honor `ON CONFLICT ... DO UPDATE`); idempotent.
async fn heartbeat(db: &Db, name: &str) -> jerrycan::Result<()> {
    let backend = db.conn().get_database_backend();
    db.conn()
        .execute(Statement::from_string(
            backend,
            "CREATE TABLE IF NOT EXISTS job_audit (name TEXT PRIMARY KEY, last_run TEXT NOT NULL)"
                .to_string(),
        ))
        .await
        .map_err(jerrycan::db::db_error)?;
    db.conn()
        .execute(Statement::from_sql_and_values(
            backend,
            db.sql(
                "INSERT INTO job_audit (name, last_run) VALUES (?, 'ok') \
                 ON CONFLICT (name) DO UPDATE SET last_run = excluded.last_run",
            ),
            [name.into()],
        ))
        .await
        .map_err(jerrycan::db::db_error)?;
    Ok(())
}
