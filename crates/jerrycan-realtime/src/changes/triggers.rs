//! The FALLBACK change source: generated AFTER-row triggers → `pg_notify` →
//! sqlx `PgListener`. The payload carries table/op/pk/scope keys only (8KB
//! NOTIFY cap); the adapter refetches the row body for insert/update. Multi-node
//! is free — every node LISTENs; Postgres is the bus (delivery here goes
//! hub-local, never onto the realtime bus).

use crate::ChangeChannelSpec;
use crate::changes::{ChangeEvent, ChangeOp, NotifyPayload};
use jerrycan_db::Db;
use jerrycan_db::sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

/// Idempotent DDL: per spec table, the notify function + trigger from the
/// changes templates, executed through the data-layer Db. The function is one
/// statement (semicolons live inside its `$$` body); the trigger template is
/// two statements (DROP then CREATE) split on the `;\n` it emits.
pub(crate) async fn ensure_triggers(
    db: &Db,
    specs: &[ChangeChannelSpec],
) -> jerrycan_core::Result<()> {
    let conn = db.conn();
    for spec in specs {
        conn.execute_unprepared(&crate::changes::notify_function_sql(spec))
            .await
            .map_err(jerrycan_db::db_error)?;
        for stmt in crate::changes::trigger_sql(spec).split(";\n") {
            let stmt = stmt.trim();
            if stmt.is_empty() {
                continue;
            }
            conn.execute_unprepared(stmt)
                .await
                .map_err(jerrycan_db::db_error)?;
        }
    }
    Ok(())
}

pub(crate) struct TriggerAdapter {
    pub(crate) db: Db,
    pub(crate) url: String,
    pub(crate) specs: Vec<ChangeChannelSpec>,
}

impl TriggerAdapter {
    /// LISTEN loop: connect PgListener → listen(jc_changes) → recv → parse the
    /// payload → find the spec by table → refetch the row body for
    /// insert/update (missing row ⇒ keys-only event) → emit a ChangeEvent.
    /// Reconnects with 1s→30s backoff on listener error.
    pub(crate) async fn run(
        self,
        events: tokio::sync::mpsc::Sender<ChangeEvent>,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) {
        let mut backoff = std::time::Duration::from_secs(1);
        loop {
            if *shutdown.borrow() {
                return;
            }
            match self.listen_once(&events, &mut shutdown).await {
                Ok(()) => return, // clean shutdown
                Err(e) => {
                    eprintln!("jerrycan-realtime: trigger listener error: {e}");
                    tokio::select! {
                        _ = shutdown.changed() => return,
                        _ = tokio::time::sleep(backoff) => {}
                    }
                    backoff = (backoff * 2).min(std::time::Duration::from_secs(30));
                }
            }
        }
    }

    async fn listen_once(
        &self,
        events: &tokio::sync::mpsc::Sender<ChangeEvent>,
        shutdown: &mut tokio::sync::watch::Receiver<bool>,
    ) -> jerrycan_core::Result<()> {
        let mut listener = sqlx::postgres::PgListener::connect(&self.url)
            .await
            .map_err(sqlx_error)?;
        listener
            .listen(crate::changes::NOTIFY_CHANNEL)
            .await
            .map_err(sqlx_error)?;
        loop {
            let note = tokio::select! {
                _ = shutdown.changed() => return Ok(()),
                r = listener.recv() => r.map_err(sqlx_error)?,
            };
            let Ok(payload) = serde_json::from_str::<NotifyPayload>(note.payload()) else {
                eprintln!("jerrycan-realtime: undecodable NOTIFY payload skipped");
                continue;
            };
            let Some(spec) = self.specs.iter().find(|s| s.table == payload.table) else {
                continue; // a table we don't publish
            };
            let mut event = payload.clone().into_event(&spec.entity);
            if matches!(event.op, ChangeOp::Insert | ChangeOp::Update) {
                event.row = self.refetch(spec, &payload.pk).await;
            }
            if events.send(event).await.is_err() {
                return Ok(()); // hub gone
            }
        }
    }

    /// Refetch the row body as JSON; None if the row is already gone (fail-open
    /// on the body — the scope keys came from the NOTIFY payload, so delivery
    /// stays scope-correct even without the body).
    async fn refetch(&self, spec: &ChangeChannelSpec, pk: &str) -> Option<serde_json::Value> {
        let sql = format!(
            "SELECT row_to_json(t)::text AS j FROM \
             (SELECT * FROM \"{}\" WHERE \"{}\"::text = $1) t",
            spec.table, spec.pk_column
        );
        let row = self
            .db
            .conn()
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                sql,
                [pk.into()],
            ))
            .await
            .ok()??;
        let json: String = row.try_get("", "j").ok()?;
        serde_json::from_str(&json).ok()
    }
}

fn sqlx_error(e: sqlx::Error) -> jerrycan_core::Error {
    eprintln!("jerrycan-realtime: {e}");
    jerrycan_core::Error::new(
        jerrycan_core::http::StatusCode::INTERNAL_SERVER_ERROR,
        "JC0510",
        "trigger listener database error",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lead() -> ChangeChannelSpec {
        ChangeChannelSpec {
            entity: "Lead".into(),
            table: "lead".into(),
            pk_column: "id".into(),
            tenant_column: Some("workspace_id".into()),
        }
    }

    #[test]
    fn trigger_sql_splits_into_exactly_drop_then_create() {
        let sql = crate::changes::trigger_sql(&lead());
        let stmts: Vec<&str> = sql
            .split(";\n")
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        assert_eq!(stmts.len(), 2, "{stmts:?}");
        assert!(stmts[0].starts_with("DROP TRIGGER IF EXISTS"));
        assert!(stmts[1].starts_with("CREATE TRIGGER"));
    }

    // -------------------------------------------------------------------
    // Live trigger-fallback test against a STOCK Postgres (wal_level=replica):
    //
    //   docker run --rm -d --name jc-rt-pg-stock -p 5434:5432 \
    //     -e POSTGRES_PASSWORD=postgres postgres:16
    //   JERRYCAN_TEST_PG=postgres://postgres:postgres@127.0.0.1:5434/postgres \
    //     cargo test -p jerrycan-realtime triggers -- --ignored --test-threads=1
    // -------------------------------------------------------------------
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs a stock Postgres (set JERRYCAN_TEST_PG)"]
    async fn triggers_install_idempotently_and_stream_insert_update_delete() {
        let Ok(url) = std::env::var("JERRYCAN_TEST_PG") else {
            eprintln!("SKIP: JERRYCAN_TEST_PG not set");
            return;
        };
        let db = Db::connect(&url).await.unwrap();
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let table = format!("tr_lead_{suffix}");
        db.conn()
            .execute_unprepared(&format!(
                "CREATE TABLE \"{table}\" (id BIGINT PRIMARY KEY, workspace_id BIGINT)"
            ))
            .await
            .unwrap();
        let spec = ChangeChannelSpec {
            entity: "Lead".into(),
            table: table.clone(),
            pk_column: "id".into(),
            tenant_column: Some("workspace_id".into()),
        };
        let specs = vec![spec];
        // Idempotent: twice.
        ensure_triggers(&db, &specs).await.unwrap();
        ensure_triggers(&db, &specs).await.unwrap();

        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let (stx, srx) = tokio::sync::watch::channel(false);
        let adapter = TriggerAdapter {
            db: db.clone(),
            url: url.clone(),
            specs,
        };
        let handle = tokio::spawn(adapter.run(tx, srx));
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        db.conn()
            .execute_unprepared(&format!(
                "INSERT INTO \"{table}\" (id, workspace_id) VALUES (1, 7)"
            ))
            .await
            .unwrap();
        let ins = tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv())
            .await
            .expect("insert event")
            .unwrap();
        assert_eq!(ins.op, ChangeOp::Insert);
        assert_eq!(ins.pk, "1");
        assert_eq!(ins.tenant_id.as_deref(), Some("7"));
        assert!(ins.row.is_some(), "insert refetches the body");

        db.conn()
            .execute_unprepared(&format!(
                "UPDATE \"{table}\" SET workspace_id = 9 WHERE id = 1"
            ))
            .await
            .unwrap();
        let upd = tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv())
            .await
            .expect("update event")
            .unwrap();
        assert_eq!(upd.op, ChangeOp::Update);
        assert_eq!(upd.tenant_id.as_deref(), Some("9"));
        assert_eq!(upd.old_tenant_id.as_deref(), Some("7"));

        db.conn()
            .execute_unprepared(&format!("DELETE FROM \"{table}\" WHERE id = 1"))
            .await
            .unwrap();
        let del = tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv())
            .await
            .expect("delete event")
            .unwrap();
        assert_eq!(del.op, ChangeOp::Delete);
        assert!(del.row.is_none());
        assert_eq!(del.old_tenant_id.as_deref(), Some("9"));

        let _ = stx.send(true);
        let _ = handle.await;
        db.conn()
            .execute_unprepared(&format!("DROP TABLE \"{table}\""))
            .await
            .unwrap();
    }
}
