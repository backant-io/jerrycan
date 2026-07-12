//! The PRIMARY change source: logical decoding of the WAL via pgoutput, driven
//! by the dedicated `pgwire-replication` client (it owns the WAL socket, TLS,
//! SCRAM, the outer frames, standby-status feedback, and the Begin/Commit
//! boundary events — sqlx stays the sole data-layer client). Self-maintaining:
//! idempotent publication/slot + REPLICA IDENTITY FULL, continuous LSN
//! confirmation via `update_applied_lsn`, supervised reconnect with backoff,
//! slot auto-recreate + resync on invalidation, advisory-lock leader election.

use crate::ChangeChannelSpec;
use crate::changes::pgoutput::{Logical, RelationCache, decode_logical};
use jerrycan_db::Db;
use jerrycan_db::sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use pgwire_replication::{
    Lsn, ReplicationClient, ReplicationConfig, ReplicationEvent, SslMode, TlsConfig,
};
use std::time::Duration;

/// The reserved advisory-lock key for the replication leader. Distinct from
/// jerrycan-jobs' cron key (JOBS_CRON_ADVISORY_KEY) and jerrycan-db's migration
/// key — all are documented project-reserved keys ("jcRTLDR1").
pub const REALTIME_LEADER_ADVISORY_KEY: i64 = 0x6A63_5254_4C44_5231;

/// A parsed Postgres connection string (the fields pgwire-replication needs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PgConn {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub dbname: String,
    pub sslmode: SslMode,
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl PgConn {
    /// Parse a `postgres://` / `postgresql://` URL. Defaults: port 5432,
    /// dbname = user, sslmode = prefer (try TLS, allow plaintext — so a local
    /// no-TLS container connects). `?sslmode=` overrides (libpq names).
    pub(crate) fn parse(url: &str) -> Option<Self> {
        let rest = url
            .strip_prefix("postgres://")
            .or_else(|| url.strip_prefix("postgresql://"))?;
        // Split trailing ?query.
        let (authority_path, query) = match rest.split_once('?') {
            Some((a, q)) => (a, Some(q)),
            None => (rest, None),
        };
        // userinfo@host / path
        let (userinfo, hostpath) = match authority_path.split_once('@') {
            Some((u, h)) => (Some(u), h),
            None => (None, authority_path),
        };
        let (user, password) = match userinfo {
            Some(ui) => match ui.split_once(':') {
                Some((u, p)) => (percent_decode(u), percent_decode(p)),
                None => (percent_decode(ui), String::new()),
            },
            None => (String::new(), String::new()),
        };
        if user.is_empty() {
            return None;
        }
        let (hostport, path) = match hostpath.split_once('/') {
            Some((hp, p)) => (hp, Some(p)),
            None => (hostpath, None),
        };
        if hostport.is_empty() {
            return None;
        }
        let (host, port) = match hostport.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), p.parse().ok()?),
            None => (hostport.to_string(), 5432),
        };
        let dbname = path
            .filter(|p| !p.is_empty())
            .map(|p| p.to_string())
            .unwrap_or_else(|| user.clone());
        let sslmode = query
            .and_then(|q| {
                q.split('&').find_map(|kv| {
                    kv.strip_prefix("sslmode=").map(|v| match v {
                        "disable" => SslMode::Disable,
                        "require" => SslMode::Require,
                        "verify-ca" => SslMode::VerifyCa,
                        "verify-full" => SslMode::VerifyFull,
                        _ => SslMode::Prefer,
                    })
                })
            })
            .unwrap_or(SslMode::Prefer);
        Some(PgConn {
            host,
            port,
            user,
            password,
            dbname,
            sslmode,
        })
    }

    fn replication_config(&self, publication: &str) -> ReplicationConfig {
        ReplicationConfig {
            host: self.host.clone(),
            port: self.port,
            user: self.user.clone(),
            password: self.password.clone(),
            database: self.dbname.clone(),
            tls: TlsConfig {
                mode: self.sslmode,
                ..Default::default()
            },
            slot: crate::changes::SLOT.to_string(),
            publication: publication.to_string(),
            start_lsn: Lsn::ZERO, // resume from the slot's confirmed_flush_lsn
            ..Default::default()
        }
    }
}

/// Idempotent DDL reconcile through the data-layer Db: publication
/// (create-or-SET TABLE), REPLICA IDENTITY FULL per table, and the logical slot
/// (create when absent, pgoutput plugin).
pub(crate) async fn ensure_replication(
    db: &Db,
    specs: &[ChangeChannelSpec],
) -> jerrycan_core::Result<()> {
    let conn = db.conn();
    let exists = conn
        .query_one(Statement::from_string(
            DatabaseBackend::Postgres,
            crate::changes::publication_exists_sql(),
        ))
        .await
        .map_err(jerrycan_db::db_error)?;
    let pub_sql = if exists.is_some() {
        crate::changes::reconcile_publication_sql(specs)
    } else {
        crate::changes::create_publication_sql(specs)
    };
    conn.execute_unprepared(&pub_sql)
        .await
        .map_err(jerrycan_db::db_error)?;
    for spec in specs {
        conn.execute_unprepared(&crate::changes::replica_identity_sql(spec))
            .await
            .map_err(jerrycan_db::db_error)?;
    }
    ensure_slot(db).await
}

/// Create the logical slot if it is absent (idempotent).
pub(crate) async fn ensure_slot(db: &Db) -> jerrycan_core::Result<()> {
    let conn = db.conn();
    let slot = crate::changes::SLOT;
    let present = conn
        .query_one(Statement::from_string(
            DatabaseBackend::Postgres,
            format!("SELECT 1 FROM pg_replication_slots WHERE slot_name = '{slot}'"),
        ))
        .await
        .map_err(jerrycan_db::db_error)?;
    if present.is_none() {
        conn.execute_unprepared(&format!(
            "SELECT pg_create_logical_replication_slot('{slot}', 'pgoutput')"
        ))
        .await
        .map_err(jerrycan_db::db_error)?;
    }
    Ok(())
}

/// Drop the slot if present (used before recreating after invalidation).
pub(crate) async fn drop_slot(db: &Db) -> jerrycan_core::Result<()> {
    let slot = crate::changes::SLOT;
    db.conn()
        .execute_unprepared(&format!(
            "SELECT pg_drop_replication_slot('{slot}') \
             WHERE EXISTS (SELECT 1 FROM pg_replication_slots WHERE slot_name = '{slot}')"
        ))
        .await
        .map_err(jerrycan_db::db_error)?;
    Ok(())
}

/// Held by the elected leader: a dedicated (non-pooled) connection owning
/// `pg_advisory_lock`. Dropping it releases the lock server-side.
pub(crate) struct LeaderGuard {
    _conn: sqlx::PgConnection,
}

pub(crate) struct LeaderGate;

impl LeaderGate {
    /// Poll `pg_try_advisory_lock` every 5s on a dedicated connection until
    /// acquired or shutdown. A dropped/killed connection = automatic release,
    /// so failover needs no coordination. Returns None on shutdown.
    pub(crate) async fn acquire(
        url: &str,
        shutdown: &mut tokio::sync::watch::Receiver<bool>,
    ) -> Option<LeaderGuard> {
        use sqlx::Connection;
        loop {
            if *shutdown.borrow() {
                return None;
            }
            match sqlx::PgConnection::connect(url).await {
                Ok(mut conn) => {
                    let got: Result<bool, _> =
                        sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
                            .bind(REALTIME_LEADER_ADVISORY_KEY)
                            .fetch_one(&mut conn)
                            .await;
                    match got {
                        Ok(true) => return Some(LeaderGuard { _conn: conn }),
                        Ok(false) => {}
                        Err(e) => eprintln!("jerrycan-realtime: advisory-lock probe failed: {e}"),
                    }
                }
                Err(e) => eprintln!("jerrycan-realtime: leader connect failed: {e}"),
            }
            tokio::select! {
                _ = shutdown.changed() => return None,
                _ = tokio::time::sleep(Duration::from_secs(5)) => {}
            }
        }
    }
}

/// One streaming session: connect, decode each XLogData into ChangeEvents, and
/// confirm the applied LSN continuously. Returns Ok(()) on clean shutdown, Err
/// (with the message) to trigger the supervisor's backoff.
pub(crate) async fn stream_once(
    conn: &PgConn,
    specs: &[ChangeChannelSpec],
    events: &tokio::sync::mpsc::Sender<crate::changes::ChangeEvent>,
    shutdown: &mut tokio::sync::watch::Receiver<bool>,
) -> jerrycan_core::Result<()> {
    let cfg = conn.replication_config(crate::changes::PUBLICATION);
    let mut client = ReplicationClient::connect(cfg)
        .await
        .map_err(|e| replication_error(&e.to_string()))?;
    let mut cache = RelationCache::default();
    loop {
        let ev = tokio::select! {
            _ = shutdown.changed() => return Ok(()),
            r = client.recv() => r,
        };
        match ev {
            Ok(Some(ReplicationEvent::XLogData { wal_end, data, .. })) => {
                match decode_logical(&data, &mut cache) {
                    Ok(Logical::Row(row)) => {
                        for spec in specs.iter().filter(|s| s.table == row.table) {
                            if let Some(event) = row.clone().into_event(spec)
                                && events.send(event).await.is_err()
                            {
                                return Ok(()); // hub gone
                            }
                        }
                    }
                    Ok(Logical::Meta) => {}
                    Err(e) => eprintln!("jerrycan-realtime: pgoutput decode error: {e}"),
                }
                client.update_applied_lsn(wal_end);
            }
            Ok(Some(ReplicationEvent::Commit { end_lsn, .. })) => {
                client.update_applied_lsn(end_lsn);
            }
            Ok(Some(_)) => {} // KeepAlive/Begin/Message/StoppedAt handled by the client
            Ok(None) => return Ok(()), // stream closed cleanly
            Err(e) => return Err(replication_error(&e.to_string())),
        }
        if *shutdown.borrow() {
            return Ok(());
        }
    }
}

/// The supervised leader loop the source selector spawns: acquire leadership →
/// ensure DDL → stream; on error, backoff 1s→30s, re-ensure, reconnect. A slot
/// invalidation/absence recreates the slot and emits a resync so subscribers
/// refetch.
pub(crate) async fn run_supervised(
    db: Db,
    specs: Vec<ChangeChannelSpec>,
    events: tokio::sync::mpsc::Sender<crate::changes::ChangeEvent>,
    resync: tokio::sync::mpsc::Sender<()>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let url = db.url().to_string();
    let Some(conn) = PgConn::parse(&url) else {
        eprintln!("jerrycan-realtime: replication disabled — cannot parse database url");
        return;
    };
    // Elect the single replication leader (multi-node: exactly one owns the slot).
    let Some(_guard) = LeaderGate::acquire(&url, &mut shutdown).await else {
        return; // shutdown before we became leader
    };
    let mut backoff = Duration::from_secs(1);
    loop {
        if *shutdown.borrow() {
            return;
        }
        if let Err(e) = ensure_replication(&db, &specs).await {
            eprintln!("jerrycan-realtime: replication DDL reconcile failed: {e}");
        }
        match stream_once(&conn, &specs, &events, &mut shutdown).await {
            Ok(()) => {
                if *shutdown.borrow() {
                    return;
                }
                backoff = Duration::from_secs(1);
            }
            Err(e) => {
                let msg = e.message().to_string();
                if is_slot_invalid(&msg) {
                    eprintln!(
                        "jerrycan-realtime: replication slot invalidated — recreating + resync"
                    );
                    let _ = drop_slot(&db).await;
                    let _ = ensure_slot(&db).await;
                    let _ = resync.send(()).await;
                    backoff = Duration::from_secs(1);
                } else {
                    eprintln!("jerrycan-realtime: replication stream error: {msg}");
                    backoff = (backoff * 2).min(Duration::from_secs(30));
                }
            }
        }
        tokio::select! {
            _ = shutdown.changed() => return,
            _ = tokio::time::sleep(backoff) => {}
        }
    }
}

/// SQLSTATE-ish heuristic: an invalidated/absent slot mentions these. On a
/// match the supervisor recreates the slot and resyncs.
fn is_slot_invalid(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("does not exist")
        || m.contains("has been invalidated")
        || m.contains("55000")
        || m.contains("42704")
}

fn replication_error(msg: &str) -> jerrycan_core::Error {
    jerrycan_core::Error::new(
        jerrycan_core::http::StatusCode::INTERNAL_SERVER_ERROR,
        "JC0531",
        format!("replication stream: {msg}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn postgres_urls_parse_host_port_db_user_password() {
        let c = PgConn::parse("postgres://app:s3cr%40t@db.example.com:5433/prod").unwrap();
        assert_eq!(c.host, "db.example.com");
        assert_eq!(c.port, 5433);
        assert_eq!(c.user, "app");
        assert_eq!(c.password, "s3cr@t"); // percent-decoded
        assert_eq!(c.dbname, "prod");
        assert_eq!(c.sslmode, SslMode::Prefer);
        // Defaults: port 5432, dbname = user.
        let d = PgConn::parse("postgres://alice@localhost").unwrap();
        assert_eq!(d.port, 5432);
        assert_eq!(d.dbname, "alice");
        // sslmode override.
        let e = PgConn::parse("postgres://u:p@h/db?sslmode=verify-full").unwrap();
        assert_eq!(e.sslmode, SslMode::VerifyFull);
        assert!(PgConn::parse("sqlite::memory:").is_none());
        assert!(PgConn::parse("postgres://").is_none());
    }

    #[test]
    fn slot_invalidation_heuristic_matches_the_expected_messages() {
        assert!(is_slot_invalid(
            "replication slot \"jc_realtime\" does not exist"
        ));
        assert!(is_slot_invalid("slot has been invalidated"));
        assert!(!is_slot_invalid("connection reset by peer"));
    }

    // -------------------------------------------------------------------
    // Live logical-replication test — needs a wal_level=logical Postgres:
    //
    //   docker run --rm -d --name jc-rt-pg -p 5433:5432 \
    //     -e POSTGRES_PASSWORD=postgres postgres:16 \
    //     -c wal_level=logical -c max_replication_slots=4 -c max_wal_senders=4
    //   JERRYCAN_TEST_PG_LOGICAL=postgres://postgres:postgres@127.0.0.1:5433/postgres \
    //     cargo test -p jerrycan-realtime replication -- --ignored --test-threads=1
    //
    // Ignored by default (CI's eval job provides the container).
    // -------------------------------------------------------------------
    fn live_url() -> Option<String> {
        std::env::var("JERRYCAN_TEST_PG_LOGICAL").ok()
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs a wal_level=logical Postgres (set JERRYCAN_TEST_PG_LOGICAL)"]
    async fn replication_streams_insert_and_reconcile_is_idempotent() {
        let Some(url) = live_url() else {
            eprintln!("SKIP: JERRYCAN_TEST_PG_LOGICAL not set");
            return;
        };
        let db = Db::connect(&url).await.unwrap();
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let table = format!("rt_lead_{suffix}");
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
        // Idempotent: calling twice must succeed.
        ensure_replication(&db, &specs).await.unwrap();
        ensure_replication(&db, &specs).await.unwrap();

        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let (rtx, _rrx) = tokio::sync::mpsc::channel(4);
        let (stx, srx) = tokio::sync::watch::channel(false);
        let handle = tokio::spawn(run_supervised(db.clone(), specs, tx, rtx, srx));

        // Give the leader/stream a moment to attach, then insert.
        tokio::time::sleep(Duration::from_millis(500)).await;
        db.conn()
            .execute_unprepared(&format!(
                "INSERT INTO \"{table}\" (id, workspace_id) VALUES (1, 7)"
            ))
            .await
            .unwrap();

        let ev = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("event within 10s")
            .expect("channel open");
        assert_eq!(ev.pk, "1");
        assert_eq!(ev.tenant_id.as_deref(), Some("7"));
        assert_eq!(ev.op, crate::changes::ChangeOp::Insert);

        let _ = stx.send(true);
        let _ = handle.await;
        let _ = drop_slot(&db).await;
        db.conn()
            .execute_unprepared(&format!("DROP TABLE \"{table}\""))
            .await
            .unwrap();
    }
}
