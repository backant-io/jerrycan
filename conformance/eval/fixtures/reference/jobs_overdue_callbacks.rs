//! Background job `overdue_callbacks` (cron, queue `default`). Agent-owned.
//! Regeneration never clobbers this file.
//!
//! Idempotent real work: UPSERTs a heartbeat into a self-owned `job_audit` table
//! and, where the app's `leads` table is present, counts the leads still in the
//! `new` status (the "overdue" set) — a read, so re-running is harmless. Jobs run
//! AT-LEAST-ONCE; the write is an overwrite UPSERT, never a bare insert.

use jerrycan::TaskContext;
use jerrycan::db::sea_orm::{ConnectionTrait, Statement};
use jerrycan::db::{Backend, Db};

/// The `overdue_callbacks` cron task. The leader enqueues it each due tick.
pub async fn overdue_callbacks(mut ctx: TaskContext) -> jerrycan::Result<()> {
    let db = ctx.resolve::<Db>().await?;
    heartbeat(&db, "overdue_callbacks").await?;
    // Only touch the app table where it exists (the jobs acceptance test migrates
    // only the jobs tables). Counting overdue leads is an idempotent read.
    if table_exists(&db, "leads").await? {
        let backend = db.conn().get_database_backend();
        let _ = db
            .conn()
            .query_one(Statement::from_string(
                backend,
                "SELECT COUNT(*) AS overdue FROM leads WHERE status = 'new'".to_string(),
            ))
            .await
            .map_err(jerrycan::db::db_error)?;
    }
    Ok(())
}

/// Ensure the audit table exists and UPSERT this job's marker. Idempotent.
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

/// Whether a table exists in the current dialect.
async fn table_exists(db: &Db, table: &str) -> jerrycan::Result<bool> {
    let sql = match db.backend() {
        Backend::Postgres => {
            "SELECT 1 FROM information_schema.tables WHERE table_name = ? LIMIT 1"
        }
        Backend::Sqlite => "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ? LIMIT 1",
    };
    let row = db
        .conn()
        .query_one(Statement::from_sql_and_values(
            db.conn().get_database_backend(),
            db.sql(sql),
            [table.into()],
        ))
        .await
        .map_err(jerrycan::db::db_error)?;
    Ok(row.is_some())
}
