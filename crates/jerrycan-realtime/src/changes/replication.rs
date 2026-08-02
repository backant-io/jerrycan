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
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// The reserved advisory-lock key for the replication leader. Distinct from
/// jerrycan-jobs' cron key (JOBS_CRON_ADVISORY_KEY) and jerrycan-db's migration
/// key — all are documented project-reserved keys ("jcRTLDR1").
pub const REALTIME_LEADER_ADVISORY_KEY: i64 = 0x6A63_5254_4C44_5231;

/// #234 recovery-convergence heartbeat interval. While the leader is CONNECTED
/// and streaming (the source is demonstrably healthy), it re-publishes
/// `ChangesHealth{false}` this often so a follower that MISSED the one-shot
/// recovery `false` — a Redis pump reconnect (pub/sub has no replay) or a fan-in
/// `broadcast` `Lagged` drop — converges to admitting `changes:` joins within one
/// interval instead of answering JC0530 forever. Chosen to match the 30s max
/// backoff so onset (`true` each backoff iteration) and recovery (`false` each
/// heartbeat) converge on the same cadence. Bounded: at most one message per
/// interval per leader (only the leader streams) — no publish storm.
const HEARTBEAT: Duration = Duration::from_secs(30);

/// #242 sustained-outage fail-loud threshold. After the source has attached at
/// least once (`connected_ever`), a run of this many CONSECUTIVE reconnect
/// attempts that never re-attach is treated as a permanent post-connect outage
/// (slot dropped and unrecreatable, DB decommissioned) — not an ordinary blip —
/// and the supervisor re-marks `changes_unavailable` so `changes:` joins answer
/// JC0530 instead of streaming from a dead feed. A single successful attach
/// resets the run to 0. Chosen large enough that ordinary reconnect blips
/// (which reattach within a few attempts, and reset the counter each time) never
/// trip it over the 1s→30s backoff schedule — 5 spans ~30s+ of sustained failure
/// — while a genuinely dead source still fails loud within a couple of minutes.
const MAX_CONSECUTIVE_FAILURES: u32 = 5;

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

/// Aborts a spawned task when dropped — used to bound the recovery heartbeat to a
/// single streaming session so it never outlives (and never heartbeats "healthy"
/// past) the connection it belongs to, on ANY of `stream_once`'s return paths.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// #234 recovery-convergence heartbeat loop: re-publish `ChangesHealth{false}` on
/// the `health` channel every `interval` until shutdown (or the forwarding task
/// drops the receiver). `stream_once` spawns this ONLY after the replication
/// socket is attached and aborts it (via [`AbortOnDrop`]) when the session ends,
/// so it fires ONLY while genuinely connected — a down source never heartbeats
/// "healthy". Symmetric to onset, which re-publishes `true` each backoff
/// iteration; a follower converges to the leader's true state within
/// `max(backoff, HEARTBEAT)` regardless of which one-shot it missed. Factored out
/// (Rule 9) so the tick cadence is unit-testable without a live Postgres.
async fn run_health_heartbeat(
    health: tokio::sync::mpsc::Sender<bool>,
    interval: Duration,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let mut tick = tokio::time::interval(interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Consume the immediate first tick of a fresh interval so the first heartbeat
    // fires one full interval AFTER connect (the swap-transition publish already
    // covers the moment of recovery — the heartbeat is the additional floor).
    tick.tick().await;
    loop {
        tokio::select! {
            _ = shutdown.changed() => return,
            _ = tick.tick() => {
                if health.send(false).await.is_err() {
                    return; // forwarding task gone
                }
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
    // #234: the recovery heartbeat publishes `false` here while streaming (same
    // channel `run_supervised` uses for the swap-transition publish).
    health: &tokio::sync::mpsc::Sender<bool>,
    connected: &mut bool,
    shutdown: &mut tokio::sync::watch::Receiver<bool>,
) -> jerrycan_core::Result<()> {
    let cfg = conn.replication_config(crate::changes::PUBLICATION);
    let mut client = ReplicationClient::connect(cfg)
        .await
        .map_err(|e| replication_error(&e.to_string()))?;
    // The replication socket is attached: the source is provably provisioned
    // (wal_level, wal_senders, slot, pgoutput all usable). Signal the supervisor
    // so a subsequent mid-stream drop is treated as a transient reconnect (#228),
    // NOT a first-connect failure that must fail loud.
    *connected = true;
    // #234: while THIS session is alive (source demonstrably healthy), heartbeat
    // `ChangesHealth{false}` on a bounded interval so a follower that missed the
    // one-shot recovery converges. Aborted on every return path (AbortOnDrop), so
    // it stops the instant the stream drops — it never heartbeats a dead source.
    let _heartbeat = AbortOnDrop(tokio::spawn(run_health_heartbeat(
        health.clone(),
        HEARTBEAT,
        shutdown.clone(),
    )));
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
    // #232: every mark/lift of `changes_unavailable` is also sent here so the
    // caller republishes it across the bus; followers (which never stream) then
    // set their own flag and answer `changes:` joins with JC0530 / re-admit.
    health: tokio::sync::mpsc::Sender<bool>,
    changes_unavailable: Arc<AtomicBool>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let url = db.url().to_string();
    let Some(conn) = PgConn::parse(&url) else {
        // A url the replication client can never parse is a permanent
        // first-connect failure: fail loud (#228) so a `changes:` join answers
        // JC0530 rather than admitting a feed that will never stream.
        eprintln!(
            "jerrycan-realtime: replication disabled — cannot parse database url — changes channels answer JC0530"
        );
        changes_unavailable.store(true, Ordering::Relaxed);
        let _ = health.send(true).await; // #232: tell followers
        return;
    };
    // Elect the single replication leader (multi-node: exactly one owns the slot).
    let Some(_guard) = LeaderGate::acquire(&url, &mut shutdown).await else {
        return; // shutdown before we became leader
    };
    let mut backoff = Duration::from_secs(1);
    // #228: distinguish NEVER-CONNECTED-ONCE (permanent-ish mis-provisioning:
    // max_wal_senders=0, slot exhaustion, pgoutput unavailable) from a
    // DROP-AFTER-CONNECT (transient). Until the first successful attach, a
    // failing attempt marks the source unavailable; once attached, later drops
    // stay transient (retry silently, as before).
    let mut connected_ever = false;
    // #242: CONSECUTIVE reconnect failures since the last successful attach. A
    // successful attach resets it to 0; once it reaches MAX_CONSECUTIVE_FAILURES
    // a source that once connected has suffered a sustained outage and fails loud
    // (below). Distinct from `connected_ever` (the one-shot #228 first-connect
    // gate): this catches a PERMANENT death AFTER a good first connect, which
    // #228 alone never re-flags.
    let mut consecutive_failures: u32 = 0;
    loop {
        if *shutdown.borrow() {
            return;
        }
        if let Err(e) = ensure_replication(&db, &specs).await {
            eprintln!("jerrycan-realtime: replication DDL reconcile failed: {e}");
        }
        let mut connected_now = false;
        let outcome = stream_once(
            &conn,
            &specs,
            &events,
            &health,
            &mut connected_now,
            &mut shutdown,
        )
        .await;
        if connected_now {
            // A successful attach proves the source is provisioned: lift any
            // earlier fail-loud so joins are admitted again (e.g. a mis-
            // provisioned PG that was fixed), and mark the source connected so
            // subsequent drops are transient.
            connected_ever = true;
            // #242: a successful attach ends any consecutive-failure run, so an
            // ordinary reconnect blip never accumulates toward the fail-loud
            // threshold.
            consecutive_failures = 0;
            // #232: publish the recovery to followers, but ONLY on the real
            // true→false transition — not on every reconnect — so a flapping
            // network cannot storm the bus with `ChangesHealth{false}`.
            if lift_and_was_transition(&changes_unavailable) {
                let _ = health.send(false).await;
            }
        } else {
            // #242: this attempt never re-attached the socket — a consecutive
            // reconnect failure. Saturating so a source down for an extreme
            // duration cannot wrap the counter back below the threshold.
            consecutive_failures = consecutive_failures.saturating_add(1);
        }
        match outcome {
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
        // #228 (fail loud): if we have NEVER attached the replication socket,
        // this is a first-connect failure — mark changes unavailable so a
        // `changes:` join answers JC0530 instead of looping silently into a
        // dead feed while `may_join` admits subscribers. A no-op once attached.
        mark_unavailable_if_never_connected(&changes_unavailable, connected_ever);
        // #242 (fail loud): a source that DID attach once but has now failed to
        // reconnect MAX_CONSECUTIVE_FAILURES times in a row is a sustained dead
        // feed, not an ordinary blip — re-mark unavailable and (mirroring the
        // never-connected #232 convergence below) republish `ChangesHealth{true}`
        // each backoff iteration so followers also answer JC0530. A successful
        // attach resets the run and #234 re-admits, so this never fires on blips.
        if mark_unavailable_if_sustained_outage(
            &changes_unavailable,
            connected_ever,
            consecutive_failures,
        ) {
            let _ = health.send(true).await;
        }
        // #232 late-joiner convergence: while we have NEVER attached, re-publish
        // `ChangesHealth{true}` on EACH backoff iteration so a follower that
        // joined the bus mid-outage (Redis pub/sub is ephemeral — it missed the
        // first mark) converges within one backoff cycle. The backoff sleep
        // below throttles this to the 1s→30s cadence, so there is no unbounded
        // publish storm.
        if !connected_ever {
            let _ = health.send(true).await;
        }
        tokio::select! {
            _ = shutdown.changed() => return,
            _ = tokio::time::sleep(backoff) => {}
        }
    }
}

/// #228 fail-loud decision (factored out for a gated unit test): mark the
/// changes source unavailable when the replication supervisor has NEVER attached
/// the replication socket (`connected_ever == false`). A permanently mis-
/// provisioned Postgres (max_wal_senders=0, slot exhaustion, pgoutput
/// unavailable) would otherwise retry forever while `Hub::join` admits `changes:`
/// subscribers to a feed that never delivers; setting the flag makes a join
/// answer JC0530 instead. Once a connect has succeeded this is a no-op, so a
/// drop after a good first connect stays transient.
pub(crate) fn mark_unavailable_if_never_connected(
    changes_unavailable: &AtomicBool,
    connected_ever: bool,
) {
    if !connected_ever {
        changes_unavailable.store(true, Ordering::Relaxed);
    }
}

/// #242 fail-loud decision for a SUSTAINED post-connect outage (factored out for
/// a gated unit test, mirroring [`mark_unavailable_if_never_connected`]). Once
/// the source has attached at least once (`connected_ever`), a run of
/// `consecutive_failures` reconnect attempts that reaches
/// [`MAX_CONSECUTIVE_FAILURES`] without a single successful re-attach is a silent
/// dead feed (slot dropped and unrecreatable, DB decommissioned), NOT an ordinary
/// blip — mark the source unavailable so a `changes:` join answers JC0530.
/// Returns whether this iteration is in the fail-loud regime so the supervisor
/// republishes `ChangesHealth{true}` to followers. A never-connected source is
/// deliberately excluded (the #228 `mark_unavailable_if_never_connected` path
/// already owns it), and a successful attach resets `consecutive_failures` to 0
/// in the supervisor, so ordinary reconnect blips never trip this.
pub(crate) fn mark_unavailable_if_sustained_outage(
    changes_unavailable: &AtomicBool,
    connected_ever: bool,
    consecutive_failures: u32,
) -> bool {
    let sustained = connected_ever && consecutive_failures >= MAX_CONSECUTIVE_FAILURES;
    if sustained {
        changes_unavailable.store(true, Ordering::Relaxed);
    }
    sustained
}

/// #232 recovery-direction mirror of `mark_unavailable_if_never_connected`: clear
/// the fail-loud flag and report whether this was a real `true→false` transition.
/// The caller publishes `ChangesHealth{false}` to peers ONLY on a transition, so
/// a source that reconnects repeatedly (already available) cannot storm the bus
/// with redundant recovery messages. The end state is always `false` (available),
/// so on the single-node happy path — where the flag was never set — this clears
/// nothing and returns `false`, i.e. no bus publish and behavior is unchanged.
pub(crate) fn lift_and_was_transition(changes_unavailable: &AtomicBool) -> bool {
    changes_unavailable.swap(false, Ordering::Relaxed)
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

    /// #228 (Rule 9) — the fail-loud decision the replication supervisor uses on
    /// a first-connect failure. WHY it matters: a permanently mis-provisioned
    /// replication PG (max_wal_senders=0, slot exhaustion, pgoutput unavailable)
    /// never attaches the socket, so without this a `changes:` join would be
    /// admitted to a feed that never delivers. This test turns RED if the
    /// `store(true)` is removed — the guarantee is that a never-connected source
    /// fails loud (JC0530) rather than looping into a silent dead feed. A source
    /// that connected once stays available across transient drops.
    #[test]
    fn first_connect_failure_marks_changes_unavailable_but_a_connected_source_does_not() {
        // Never attached once ⇒ mark unavailable (join answers JC0530).
        let never = AtomicBool::new(false);
        mark_unavailable_if_never_connected(&never, false);
        assert!(
            never.load(Ordering::Relaxed),
            "a first-connect replication failure must mark changes unavailable so joins answer JC0530"
        );
        // Attached at least once ⇒ a later drop is transient — never mark.
        let connected = AtomicBool::new(false);
        mark_unavailable_if_never_connected(&connected, true);
        assert!(
            !connected.load(Ordering::Relaxed),
            "a source that connected once must stay available across transient drops"
        );
    }

    /// #242 (Rule 9) — the SUSTAINED post-connect outage fail-loud the supervisor
    /// uses. WHY it matters: after a good first connect, a PERMANENT mid-stream
    /// source death (slot dropped and unrecreatable, DB decommissioned) retries
    /// forever; the #228 first-connect gate is a no-op once `connected_ever`, so
    /// without this a `changes:` join would be admitted to a silent dead feed
    /// indefinitely. The guarantee: below the threshold stays transient (ordinary
    /// blips), at the threshold it fails loud (JC0530). RED if the threshold check
    /// is removed (the function would stop marking at MAX).
    #[test]
    fn sustained_post_connect_outage_fails_loud_only_at_the_threshold() {
        let flag = AtomicBool::new(false);
        // A NEVER-connected source is the #228 path, not this one: even a long
        // failure run must not trip the sustained-outage fail-loud here (that
        // would double-own what `mark_unavailable_if_never_connected` covers).
        assert!(
            !mark_unavailable_if_sustained_outage(&flag, false, MAX_CONSECUTIVE_FAILURES + 5),
            "a never-connected source is handled by #228, not the sustained-outage path"
        );
        assert!(!flag.load(Ordering::Relaxed));

        // Post-connect, BELOW the threshold: an ordinary reconnect blip stays
        // transient — no fail-loud, `changes:` joins keep being admitted.
        for failures in 0..MAX_CONSECUTIVE_FAILURES {
            assert!(
                !mark_unavailable_if_sustained_outage(&flag, true, failures),
                "{failures} consecutive reconnect failures is still an ordinary blip"
            );
            assert!(
                !flag.load(Ordering::Relaxed),
                "no false fail-loud below the threshold"
            );
        }

        // AT the threshold: a sustained dead feed fails loud so a join answers
        // JC0530 instead of streaming from a source that will never deliver.
        assert!(
            mark_unavailable_if_sustained_outage(&flag, true, MAX_CONSECUTIVE_FAILURES),
            "a sustained post-connect outage must re-mark changes unavailable"
        );
        assert!(
            flag.load(Ordering::Relaxed),
            "the fail-loud flag must be set so a `changes:` join answers JC0530"
        );
    }

    /// #242 (the no-false-positive guarantee): a successful reconnect resets the
    /// consecutive-failure run, so a source that keeps reconnecting never fails
    /// loud on ordinary blips. Models the supervisor's counter: it fails
    /// MAX-1 times (still transient), a successful attach resets it to 0, and a
    /// single later failure is then nowhere near the threshold. RED if the reset
    /// is dropped (a slowly-flapping source would eventually trip the threshold).
    #[test]
    fn a_reconnect_before_the_threshold_prevents_a_premature_fail_loud() {
        let flag = AtomicBool::new(false);
        let mut failures = MAX_CONSECUTIVE_FAILURES - 1;
        assert!(
            !mark_unavailable_if_sustained_outage(&flag, true, failures),
            "one short of the threshold is still transient"
        );
        // Successful attach: the supervisor resets the run (connected_now ⇒
        // consecutive_failures = 0) and #234 lifts the flag.
        failures = 0;
        let _ = lift_and_was_transition(&flag);
        // One post-reset reconnect failure is far from the threshold.
        failures += 1;
        assert!(
            !mark_unavailable_if_sustained_outage(&flag, true, failures),
            "a reconnect that reset the run must prevent a premature fail-loud"
        );
        assert!(
            !flag.load(Ordering::Relaxed),
            "an ordinary reconnecting source must stay available"
        );
    }

    /// #232 (recovery direction): the lift reports a `ChangesHealth{false}`
    /// publish ONLY on the real `true→false` transition. WHY it matters: without
    /// the transition gate every reconnect of an already-healthy source would
    /// re-broadcast recovery across the bus (a storm on a flapping network); with
    /// it, followers hear the recovery exactly once. A regression that publishes
    /// unconditionally (or never) turns this red.
    #[test]
    fn lift_publishes_only_on_the_true_to_false_transition() {
        // Was unavailable ⇒ lifting is a transition (publish false once).
        let flag = AtomicBool::new(true);
        assert!(
            lift_and_was_transition(&flag),
            "clearing an unavailable source is a transition — publish recovery"
        );
        assert!(!flag.load(Ordering::Relaxed), "the flag must end cleared");
        // Already available ⇒ a further connect is NOT a transition (no publish).
        assert!(
            !lift_and_was_transition(&flag),
            "an already-available source must not re-publish recovery"
        );
        assert!(!flag.load(Ordering::Relaxed));
    }

    /// #232 (the multi-node fix, END TO END without PG): a first-connect failure
    /// in `run_supervised` must BOTH set `changes_unavailable` (the #228 local
    /// fail-loud) AND emit a `ChangesHealth{true}` on the health channel, so the
    /// forwarding task republishes it and followers refuse `changes:` joins with
    /// JC0530 instead of admitting a dead feed. Driven with a `sqlite::memory:`
    /// url — which `PgConn::parse` rejects — so the permanent first-connect
    /// failure path runs with no live Postgres. A regression that marks the flag
    /// but forgets to notify peers (the exact #232 blind spot) turns this red.
    #[tokio::test]
    async fn first_connect_failure_notifies_peers_over_the_health_channel() {
        let db = Db::connect("sqlite::memory:").await.unwrap();
        let spec = ChangeChannelSpec {
            entity: "Lead".into(),
            table: "lead".into(),
            pk_column: "id".into(),
            tenant_column: Some("workspace_id".into()),
            owner_column: None,
            hidden_columns: Vec::new(),
        };
        let (etx, _erx) = tokio::sync::mpsc::channel(16);
        let (rtx, _rrx) = tokio::sync::mpsc::channel(16);
        let (htx, mut hrx) = tokio::sync::mpsc::channel(16);
        let (_stx, srx) = tokio::sync::watch::channel(false);
        let flag = Arc::new(AtomicBool::new(false));

        // sqlite url ⇒ PgConn::parse returns None ⇒ the permanent first-connect
        // failure path runs and returns immediately (no leader election, no PG).
        run_supervised(db, vec![spec], etx, rtx, htx, flag.clone(), srx).await;

        assert!(
            flag.load(Ordering::Relaxed),
            "#228: a first-connect failure must fail loud locally (JC0530)"
        );
        assert_eq!(
            hrx.try_recv().ok(),
            Some(true),
            "#232: the failure must be published so followers also fail loud"
        );
    }

    /// #234 (Rule 9): the recovery heartbeat re-publishes `ChangesHealth{false}`
    /// on the health channel MORE THAN ONCE while connected — closing the #232
    /// asymmetry where recovery was published EXACTLY ONCE (on the true→false
    /// swap) and a follower that missed it stayed stuck on JC0530 forever. WHY it
    /// matters: a follower's admission is driven SOLELY by bus health; without a
    /// repeating healthy heartbeat one dropped `false` (Redis reconnect / fan-in
    /// `Lagged`) strands it though the source is healthy. Driven with a short
    /// interval and the REAL `run_health_heartbeat` loop (no live PG); asserts ≥2
    /// `false` emissions, then that shutdown stops it (a down source must never
    /// heartbeat "healthy"). RED if the heartbeat fires once, emits `true`, or
    /// never fires.
    #[tokio::test]
    async fn health_heartbeat_republishes_false_more_than_once_while_connected() {
        let (htx, mut hrx) = tokio::sync::mpsc::channel::<bool>(16);
        let (stx, srx) = tokio::sync::watch::channel(false);
        let hb = tokio::spawn(run_health_heartbeat(htx, Duration::from_millis(10), srx));

        // Two distinct heartbeat ticks (the immediate first tick is consumed)
        // must EACH publish a recovery `false` — proof recovery is not a one-shot.
        let first = tokio::time::timeout(Duration::from_secs(1), hrx.recv())
            .await
            .expect("first heartbeat within 1s")
            .expect("channel open");
        assert!(
            !first,
            "the heartbeat must publish ChangesHealth{{false}} (recovery)"
        );
        let second = tokio::time::timeout(Duration::from_secs(1), hrx.recv())
            .await
            .expect("second heartbeat within 1s — recovery is NOT a one-shot")
            .expect("channel open");
        assert!(!second, "every heartbeat re-publishes recovery `false`");

        // Shutdown stops the heartbeat: a disconnected source must not heartbeat.
        let _ = stx.send(true);
        tokio::time::timeout(Duration::from_secs(1), hb)
            .await
            .expect("heartbeat stops within 1s of shutdown")
            .expect("heartbeat task joins cleanly");
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
            owner_column: None,
            hidden_columns: Vec::new(),
        };
        let specs = vec![spec];
        // Idempotent: calling twice must succeed.
        ensure_replication(&db, &specs).await.unwrap();
        ensure_replication(&db, &specs).await.unwrap();

        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let (rtx, _rrx) = tokio::sync::mpsc::channel(4);
        let (htx, _hrx) = tokio::sync::mpsc::channel(16);
        let (stx, srx) = tokio::sync::watch::channel(false);
        let unavail = Arc::new(AtomicBool::new(false));
        let handle = tokio::spawn(run_supervised(
            db.clone(),
            specs,
            tx,
            rtx,
            htx,
            unavail,
            srx,
        ));

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
