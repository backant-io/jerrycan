//! #138 — the last-admin guard is ATOMIC under concurrency.
//!
//! The #107 member surface first blocked removing/demoting the sole admin with a
//! `count_admins` READ followed by a SEPARATE DELETE/UPDATE (check-then-act). Two
//! concurrent admin-gated writes on one tenant could both pass the read and leave the
//! tenant with ZERO admins — member management locked out forever. A single
//! conditional statement (`DELETE … AND NOT (role='admin' AND (SELECT COUNT(*) …)<=1)`)
//! is NOT enough either: the two writes target DIFFERENT admin rows and the COUNT
//! subquery takes no lock, so under Postgres READ COMMITTED both subqueries read the
//! pre-image count and both write → still zero admins (write-skew, proven below).
//!
//! The fix (genroute `remove_member`/`set_member_role`): ONE transaction that FIRST
//! locks the tenant's admin rows (`SELECT … FOR UPDATE`, Postgres only — SQLite
//! serializes on its single writer and can't parse FOR UPDATE), THEN runs the
//! conditional write. The loser blocks on the lock, re-reads count = 1, and its
//! conditional statement affects 0 rows → 409. The tenant keeps ≥ 1 admin, always.
//!
//! The SQL constants below are the EXACT text the generator emits for a tenancy whose
//! members table is `org_members`, tenant fk `org_id`, admin role `admin`
//! (`test_a_generated_member_writes_emit_the_txn_lock_guard` scaffolds an app and
//! asserts the generated repo contains them — binding this behavioral proof to the
//! shipped generated code, exactly as `tests/migrate_membership_lossless.rs` does).

/// The Postgres-only admin-set lock the generated write takes before the conditional
/// statement (`if backend == Postgres { … }` in the generated method).
const LOCK_SQL: &str = "SELECT id FROM org_members WHERE org_id = ? AND role = 'admin' FOR UPDATE";

/// The conditional DELETE: removes the (user, tenant) row UNLESS it is the last admin.
const DELETE_SQL: &str = "DELETE FROM org_members WHERE user_id = ? AND org_id = ? AND NOT (role = 'admin' AND (SELECT COUNT(*) FROM org_members WHERE org_id = ? AND role = 'admin') <= 1)";

/// The conditional UPDATE: re-roles the (user, tenant) row UNLESS it is a genuine
/// demote of the last admin (`? <> 'admin'` is the NEW role, so re-affirming admin
/// proceeds as a no-op update).
const UPDATE_SQL: &str = "UPDATE org_members SET role = ? WHERE user_id = ? AND org_id = ? AND NOT (role = 'admin' AND ? <> 'admin' AND (SELECT COUNT(*) FROM org_members WHERE org_id = ? AND role = 'admin') <= 1)";

/// A minimal integer-pk tenancy design whose tenant module (`orgs`) declares the
/// tenant entity `Org` — so its generated repo carries the member surface.
const ORG_DESIGN: &str = r#"{ "name": "orgs-api", "contract_version": 1,
    "auth": { "model": "session", "roles": ["admin", "member"] },
    "dependencies": ["db", "auth"],
    "tenancy": { "entity": "Org", "member_roles": ["admin", "member"] },
    "modules": [
        { "name": "orgs",
          "entities": [{ "name": "Org", "fields": [
              { "name": "id", "type": "integer" },
              { "name": "name", "type": "string" } ]}],
          "endpoints": [
              { "operation_id": "list_orgs", "method": "GET", "path": "/", "auth_required": true,
                "success": { "status": 200, "entity": "Org", "list": true } },
              { "operation_id": "create_org", "method": "POST", "path": "/", "auth_required": true,
                "request_body": { "entity": "Org" },
                "success": { "status": 201, "entity": "Org" } } ] }
    ] }"#;

/// GENERATION BINDING: the scaffolded tenant repo emits the txn + admin-set lock +
/// conditional-write guard verbatim. If codegen drifts from the SQL exercised in the
/// concurrency legs, this fails first — so the proof can never certify stale text.
#[test]
fn test_a_generated_member_writes_emit_the_txn_lock_guard() {
    use jerrycan::platform::design::Design;
    use jerrycan::platform::scaffold;

    let design: Design = serde_json::from_str(ORG_DESIGN).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("app");
    scaffold::scaffold(&root, &design).unwrap();
    let repo = std::fs::read_to_string(root.join("crates/routes/orgs/src/repo.rs")).unwrap();

    assert!(
        repo.contains(LOCK_SQL),
        "generated writes lock the tenant admin set (FOR UPDATE) before the conditional write:\n{repo}"
    );
    assert!(
        repo.contains("== sea_orm::DatabaseBackend::Postgres"),
        "the FOR UPDATE lock is Postgres-only (SQLite serializes on its single writer):\n{repo}"
    );
    assert!(
        repo.contains(DELETE_SQL),
        "remove_member runs the conditional DELETE this test fires concurrently:\n{repo}"
    );
    assert!(
        repo.contains(UPDATE_SQL),
        "set_member_role runs the conditional UPDATE this test fires concurrently:\n{repo}"
    );
    // The guard is inside a transaction (begin/commit) — not two autocommit statements.
    assert!(
        repo.contains("self.db.conn().begin().await") && repo.contains("txn.commit().await"),
        "the lock + conditional write run in ONE transaction:\n{repo}"
    );
}

#[cfg(feature = "db")]
use jerrycan::db::Db;
#[cfg(feature = "db")]
use jerrycan::db::sea_orm::{ConnectionTrait, DatabaseBackend, Statement, TransactionTrait};

/// What a guarded write did — mirrors the generated method's three return shapes:
/// `Applied` = Ok(true) (write landed), `Conflict` = 409 (last-admin, row still
/// exists), `NotFound` = Ok(false)/404 (no such member).
#[cfg(feature = "db")]
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Applied,
    Conflict,
    NotFound,
}

/// Reproduces the generated `remove_member`: txn → (Postgres) admin-set lock →
/// conditional DELETE → 0-path existence read → commit.
#[cfg(feature = "db")]
async fn remove_atomic(db: Db, org: i64, user: String) -> Outcome {
    let backend = db.conn().get_database_backend();
    let txn = db.conn().begin().await.unwrap();
    if backend == DatabaseBackend::Postgres {
        txn.execute(Statement::from_sql_and_values(
            backend,
            db.sql(LOCK_SQL),
            [org.into()],
        ))
        .await
        .unwrap();
    }
    let r = txn
        .execute(Statement::from_sql_and_values(
            backend,
            db.sql(DELETE_SQL),
            [user.clone().into(), org.into(), org.into()],
        ))
        .await
        .unwrap();
    if r.rows_affected() == 1 {
        txn.commit().await.unwrap();
        return Outcome::Applied;
    }
    let still = txn
        .query_one(Statement::from_sql_and_values(
            backend,
            db.sql("SELECT id FROM org_members WHERE user_id = ? AND org_id = ?"),
            [user.into(), org.into()],
        ))
        .await
        .unwrap();
    txn.commit().await.unwrap();
    if still.is_some() {
        Outcome::Conflict
    } else {
        Outcome::NotFound
    }
}

/// Reproduces the generated `set_member_role`: same txn + lock, conditional UPDATE
/// (bind order `[new_role, user, org, new_role, org]`).
#[cfg(feature = "db")]
async fn demote_atomic(db: Db, org: i64, user: String, new_role: String) -> Outcome {
    let backend = db.conn().get_database_backend();
    let txn = db.conn().begin().await.unwrap();
    if backend == DatabaseBackend::Postgres {
        txn.execute(Statement::from_sql_and_values(
            backend,
            db.sql(LOCK_SQL),
            [org.into()],
        ))
        .await
        .unwrap();
    }
    let r = txn
        .execute(Statement::from_sql_and_values(
            backend,
            db.sql(UPDATE_SQL),
            [
                new_role.clone().into(),
                user.clone().into(),
                org.into(),
                new_role.into(),
                org.into(),
            ],
        ))
        .await
        .unwrap();
    if r.rows_affected() == 1 {
        txn.commit().await.unwrap();
        return Outcome::Applied;
    }
    let still = txn
        .query_one(Statement::from_sql_and_values(
            backend,
            db.sql("SELECT id FROM org_members WHERE user_id = ? AND org_id = ?"),
            [user.into(), org.into()],
        ))
        .await
        .unwrap();
    txn.commit().await.unwrap();
    if still.is_some() {
        Outcome::Conflict
    } else {
        Outcome::NotFound
    }
}

#[cfg(feature = "db")]
async fn seed_two_admins(db: &Db) {
    db.conn()
        .execute_unprepared("DELETE FROM org_members")
        .await
        .unwrap();
    db.conn()
        .execute_unprepared(
            "INSERT INTO org_members (user_id, org_id, role) \
             VALUES ('a1', 1, 'admin'), ('a2', 1, 'admin')",
        )
        .await
        .unwrap();
}

#[cfg(feature = "db")]
async fn admin_count(db: &Db) -> i64 {
    let row = db
        .conn()
        .query_one(Statement::from_sql_and_values(
            db.conn().get_database_backend(),
            db.sql("SELECT COUNT(*) AS n FROM org_members WHERE org_id = ? AND role = 'admin'"),
            [1i64.into()],
        ))
        .await
        .unwrap()
        .expect("count row");
    row.try_get::<i64>("", "n")
        .or_else(|_| row.try_get::<i32>("", "n").map(i64::from))
        .unwrap()
}

/// SQLite: the single writer (pool max = 1) serializes the two transactions, so the
/// loser's conditional write sees count = 1 and is refused — one admin always remains.
/// (This is the backend where a naive read-then-act would *accidentally* be safe; the
/// txn+lock shape is what makes it correct on Postgres too, proven in the PG leg.)
#[cfg(feature = "db")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_b_sqlite_concurrent_last_admin_writes_keep_one_admin() {
    let db = Db::connect("sqlite::memory:").await.unwrap();
    db.conn()
        .execute_unprepared(
            "CREATE TABLE org_members (id INTEGER PRIMARY KEY, user_id TEXT NOT NULL, \
             org_id INTEGER NOT NULL, role TEXT NOT NULL, UNIQUE(user_id, org_id))",
        )
        .await
        .unwrap();

    // Two concurrent removals of the two (and only) admins.
    seed_two_admins(&db).await;
    let h1 = tokio::spawn(remove_atomic(db.clone(), 1, "a1".to_string()));
    let h2 = tokio::spawn(remove_atomic(db.clone(), 1, "a2".to_string()));
    let outcomes = [h1.await.unwrap(), h2.await.unwrap()];
    assert!(
        admin_count(&db).await >= 1,
        "concurrent removes must never leave a tenant admin-less"
    );
    assert_eq!(
        outcomes.iter().filter(|o| **o == Outcome::Applied).count(),
        1,
        "exactly one removal wins: {outcomes:?}"
    );
    assert_eq!(
        outcomes.iter().filter(|o| **o == Outcome::Conflict).count(),
        1,
        "the other removal is refused (409): {outcomes:?}"
    );

    // Two concurrent demotes of the two admins.
    seed_two_admins(&db).await;
    let h1 = tokio::spawn(demote_atomic(
        db.clone(),
        1,
        "a1".to_string(),
        "member".to_string(),
    ));
    let h2 = tokio::spawn(demote_atomic(
        db.clone(),
        1,
        "a2".to_string(),
        "member".to_string(),
    ));
    let outcomes = [h1.await.unwrap(), h2.await.unwrap()];
    assert!(
        admin_count(&db).await >= 1,
        "concurrent demotes must never leave a tenant admin-less"
    );
    assert_eq!(
        outcomes.iter().filter(|o| **o == Outcome::Applied).count(),
        1,
        "exactly one demote wins: {outcomes:?}"
    );
    assert_eq!(
        outcomes.iter().filter(|o| **o == Outcome::Conflict).count(),
        1,
        "the other demote is refused (409): {outcomes:?}"
    );
}

/// Postgres — the executable #138 proof on a pool with REAL concurrent writers:
/// (i) the REJECTED lock-free single-statement design reaches ZERO admins (write-skew,
/// the landmine), and (ii)/(iii) the shipped txn + FOR UPDATE guard keeps ≥ 1 admin
/// across many genuinely-concurrent rounds of removes and demotes. Needs a live
/// Postgres; reset the schema first (`DROP SCHEMA public CASCADE; CREATE SCHEMA public`)
/// then run with `JERRYCAN_TEST_PG_URL=… cargo test -p jerrycan --all-features \
/// --test last_admin_concurrency -- --include-ignored`.
#[cfg(feature = "db")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a local postgres (set JERRYCAN_TEST_PG_URL)"]
async fn test_c_postgres_atomic_guard_beats_the_last_admin_race() {
    let Ok(url) = std::env::var("JERRYCAN_TEST_PG_URL") else {
        eprintln!("SKIP: JERRYCAN_TEST_PG_URL not set");
        return;
    };
    let db = Db::connect(&url).await.unwrap();
    db.conn()
        .execute_unprepared("DROP TABLE IF EXISTS org_members")
        .await
        .unwrap();
    db.conn()
        .execute_unprepared(
            "CREATE TABLE org_members (id BIGSERIAL PRIMARY KEY, user_id TEXT NOT NULL, \
             org_id BIGINT NOT NULL, role TEXT NOT NULL, UNIQUE(user_id, org_id))",
        )
        .await
        .unwrap();
    let backend = db.conn().get_database_backend();

    // (i) LANDMINE (deterministic worst case): two OVERLAPPING transactions each run
    // the lock-free conditional DELETE. Both snapshots see count = 2 (neither sees the
    // other's uncommitted delete under READ COMMITTED), both pass the guard, both
    // delete DIFFERENT admin rows → ZERO admins. This is exactly why a single
    // conditional statement is insufficient and the FOR UPDATE lock is required.
    seed_two_admins(&db).await;
    let t1 = db.conn().begin().await.unwrap();
    let t2 = db.conn().begin().await.unwrap();
    t1.execute(Statement::from_sql_and_values(
        backend,
        db.sql(DELETE_SQL),
        ["a1".into(), 1i64.into(), 1i64.into()],
    ))
    .await
    .unwrap();
    t2.execute(Statement::from_sql_and_values(
        backend,
        db.sql(DELETE_SQL),
        ["a2".into(), 1i64.into(), 1i64.into()],
    ))
    .await
    .unwrap();
    t1.commit().await.unwrap();
    t2.commit().await.unwrap();
    assert_eq!(
        admin_count(&db).await,
        0,
        "the lock-free single-statement design MUST reach zero admins under overlapping \
         txns (the #138 write-skew) — if this is not 0 the landmine isn't being exercised"
    );

    let rounds = 25;

    // (ii) FIX — removes. Genuinely concurrent (separate connections on a pool of 5).
    // The FOR UPDATE lock serializes the pair: the loser blocks, re-reads count = 1,
    // and its conditional DELETE affects 0 rows → 409. Never zero admins, every round.
    for _ in 0..rounds {
        seed_two_admins(&db).await;
        let h1 = tokio::spawn(remove_atomic(db.clone(), 1, "a1".to_string()));
        let h2 = tokio::spawn(remove_atomic(db.clone(), 1, "a2".to_string()));
        let outcomes = [h1.await.unwrap(), h2.await.unwrap()];
        assert!(
            admin_count(&db).await >= 1,
            "atomic concurrent removes must never leave zero admins: {outcomes:?}"
        );
        assert_eq!(
            outcomes.iter().filter(|o| **o == Outcome::Applied).count(),
            1,
            "exactly one removal wins: {outcomes:?}"
        );
        assert_eq!(
            outcomes.iter().filter(|o| **o == Outcome::Conflict).count(),
            1,
            "the other removal gets 409: {outcomes:?}"
        );
    }

    // (iii) FIX — demotes. Same guard on the conditional UPDATE.
    for _ in 0..rounds {
        seed_two_admins(&db).await;
        let h1 = tokio::spawn(demote_atomic(
            db.clone(),
            1,
            "a1".to_string(),
            "member".to_string(),
        ));
        let h2 = tokio::spawn(demote_atomic(
            db.clone(),
            1,
            "a2".to_string(),
            "member".to_string(),
        ));
        let outcomes = [h1.await.unwrap(), h2.await.unwrap()];
        assert!(
            admin_count(&db).await >= 1,
            "atomic concurrent demotes must never leave zero admins: {outcomes:?}"
        );
        assert_eq!(
            outcomes.iter().filter(|o| **o == Outcome::Applied).count(),
            1,
            "exactly one demote wins: {outcomes:?}"
        );
        assert_eq!(
            outcomes.iter().filter(|o| **o == Outcome::Conflict).count(),
            1,
            "the other demote gets 409: {outcomes:?}"
        );
    }

    db.conn()
        .execute_unprepared("DROP TABLE IF EXISTS org_members")
        .await
        .unwrap();
}
