//! #187 — the generated `reserve_against` method is ATOMIC under concurrency (no
//! oversell).
//!
//! A field declaring `reserve_against` wires `{Entity}Repo::reserve(id, n)` — the
//! #108-proven conditional UPDATE `SET used = used + ? WHERE id = ? AND used + ? <=
//! capacity`. The whole point of #187 is to make the reservation correct BY
//! CONSTRUCTION: a hand-written read-then-write (read remaining capacity, check it,
//! then UPDATE) passes every SQLite test (its single writer serializes) and silently
//! OVERSELLS on Postgres, where two requests both read the same remaining capacity,
//! both pass the check, and both write. The single conditional UPDATE cannot oversell:
//! every caller contends on the SAME pk row, so the row lock + WHERE guard serialize
//! them — the loser re-reads the committed `used` and its guard fails (0 rows).
//!
//! `RESERVE_SQL` below is the EXACT text the generator emits for a `Room` whose counter
//! is `used` and capacity is `capacity` (table `rooms`). `test_a` scaffolds an app and
//! asserts the generated repo contains it — binding this behavioral proof to the
//! shipped generated code (exactly as `tests/last_admin_concurrency.rs` does for #138),
//! so the proof can never certify stale text.

/// The atomic reserve the generator emits (module `venue`, entity `Room`, counter
/// `used`, capacity `capacity`). The concurrency legs fire this IDENTICAL statement.
/// Every identifier is double-quoted (ANSI `"ident"`, honored by SQLite AND Postgres)
/// so a keyword-named counter/capacity can never emit a syntax error (review FIX 1) —
/// this is RESERVE_SQL's runtime VALUE (real quotes); the generated repo SOURCE carries
/// the same string with the quotes ESCAPED, which `test_a` matches separately.
const RESERVE_SQL: &str = "UPDATE \"rooms\" SET \"used\" = \"used\" + ? WHERE \"id\" = ? AND \"used\" + ? <= \"capacity\"";

/// A minimal integer-pk db design whose `Room` entity declares `reserve_against` on
/// its `used` counter against the `capacity` ceiling — so its generated repo carries
/// the atomic `reserve` method.
const ROOM_DESIGN: &str = r#"{ "name": "rooms-api", "contract_version": 1,
    "dependencies": ["db"],
    "modules": [
        { "name": "venue",
          "entities": [{ "name": "Room", "fields": [
              { "name": "id", "type": "integer" },
              { "name": "capacity", "type": "integer" },
              { "name": "used", "type": "integer", "reserve_against": "capacity" } ]}],
          "endpoints": [
              { "operation_id": "list_rooms", "method": "GET", "path": "/",
                "success": { "status": 200, "entity": "Room", "list": true } },
              { "operation_id": "create_room", "method": "POST", "path": "/",
                "request_body": { "entity": "Room" },
                "success": { "status": 201, "entity": "Room" } } ] }
    ] }"#;

/// GENERATION BINDING: the scaffolded repo emits the atomic reserve verbatim — the
/// exact signature, the exact conditional UPDATE, the `[n, id, n]` binding, and success
/// keyed on one affected row. If codegen drifts from the SQL the concurrency legs fire,
/// this fails first, so the proof can never certify stale text.
#[test]
fn test_a_generated_reserve_emits_the_atomic_capacity_guard() {
    use jerrycan::platform::design::Design;
    use jerrycan::platform::scaffold;

    let design: Design = serde_json::from_str(ROOM_DESIGN).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("app");
    scaffold::scaffold(&root, &design).unwrap();
    let repo = std::fs::read_to_string(root.join("crates/routes/venue/src/repo.rs")).unwrap();

    assert!(
        repo.contains("pub async fn reserve(&self, id: i64, n: i64) -> Result<bool>"),
        "generated repo must emit `reserve(id, n) -> Result<bool>`:\n{repo}"
    );
    // The generated repo SOURCE carries the atomic UPDATE as a Rust string literal, so
    // its double-quoted identifiers appear ESCAPED (`\"rooms\"`) in the source text —
    // distinct from RESERVE_SQL's runtime value (real quotes) the legs actually fire.
    assert!(
        repo.contains(
            r#"UPDATE \"rooms\" SET \"used\" = \"used\" + ? WHERE \"id\" = ? AND \"used\" + ? <= \"capacity\""#
        ),
        "generated reserve must carry the exact atomic capacity guard the legs fire:\n{repo}"
    );
    assert!(
        repo.contains("[n.into(), id.into(), n.into()]"),
        "generated reserve must bind [n, id, n]:\n{repo}"
    );
    assert!(
        repo.contains("rows_affected() == 1"),
        "generated reserve keys success on exactly one affected row:\n{repo}"
    );
}

#[cfg(feature = "db")]
use jerrycan::db::Db;
#[cfg(feature = "db")]
use jerrycan::db::sea_orm::{ConnectionTrait, Statement};

/// Reproduces the generated `reserve` EXACTLY: one conditional UPDATE, `Ok(true)` iff
/// exactly one row was affected (the reservation fit).
#[cfg(feature = "db")]
async fn reserve_atomic(db: Db, id: i64, n: i64) -> bool {
    let backend = db.conn().get_database_backend();
    let r = db
        .conn()
        .execute(Statement::from_sql_and_values(
            backend,
            db.sql(RESERVE_SQL),
            [n.into(), id.into(), n.into()],
        ))
        .await
        .unwrap();
    r.rows_affected() == 1
}

#[cfg(feature = "db")]
async fn used_now(db: &Db) -> i64 {
    let row = db
        .conn()
        .query_one(Statement::from_sql_and_values(
            db.conn().get_database_backend(),
            db.sql("SELECT used AS u FROM rooms WHERE id = ?"),
            [1i64.into()],
        ))
        .await
        .unwrap()
        .expect("room row");
    row.try_get::<i64>("", "u")
        .or_else(|_| row.try_get::<i32>("", "u").map(i64::from))
        .unwrap()
}

/// Fire `k` concurrent `reserve(1, 1)` on a room of `capacity`, and assert the
/// no-oversell invariant: exactly `capacity` reservations win, the rest are refused,
/// and the committed `used` lands on `capacity` — never above.
#[cfg(feature = "db")]
async fn assert_no_oversell(db: &Db, capacity: i64, k: i64) {
    db.conn()
        .execute(Statement::from_sql_and_values(
            db.conn().get_database_backend(),
            db.sql("UPDATE rooms SET used = 0, capacity = ? WHERE id = ?"),
            [capacity.into(), 1i64.into()],
        ))
        .await
        .unwrap();

    let handles: Vec<_> = (0..k)
        .map(|_| tokio::spawn(reserve_atomic(db.clone(), 1, 1)))
        .collect();
    let mut wins = 0i64;
    for h in handles {
        if h.await.unwrap() {
            wins += 1;
        }
    }
    assert_eq!(
        wins, capacity,
        "exactly `capacity` ({capacity}) of {k} concurrent reserves must win, got {wins}"
    );
    assert_eq!(
        used_now(db).await,
        capacity,
        "committed `used` must land on capacity ({capacity}) — never oversell"
    );
}

/// SQLite: the single writer (pool max = 1) serializes the reserves, so the counter
/// climbs one at a time and the guard refuses every reserve past `capacity`. This is
/// the backend where a naive read-then-write would *accidentally* be safe; the PG leg
/// proves the SAME statement is correct under real concurrent writers.
#[cfg(feature = "db")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_b_sqlite_concurrent_reserves_never_oversell() {
    let db = Db::connect("sqlite::memory:").await.unwrap();
    db.conn()
        .execute_unprepared(
            "CREATE TABLE rooms (id INTEGER PRIMARY KEY, capacity INTEGER NOT NULL, \
             used INTEGER NOT NULL DEFAULT 0)",
        )
        .await
        .unwrap();
    db.conn()
        .execute_unprepared("INSERT INTO rooms (id, capacity, used) VALUES (1, 0, 0)")
        .await
        .unwrap();

    assert_no_oversell(&db, 5, 20).await;
    assert_no_oversell(&db, 1, 16).await;
    assert_no_oversell(&db, 10, 40).await;
}

/// Review FIX 1 (reserved-word identifiers): the generator double-quotes every
/// interpolated identifier, so a counter/capacity named after a SQL keyword still
/// EXECUTES. This is the exact shape the generator emits for a `Slot` whose counter is
/// `order` and capacity is `limit` (table `slots`) — both reserved words. Fired
/// UNQUOTED it would be `UPDATE slots SET order = order + ? WHERE id = ? AND order + ?
/// <= limit`, a runtime `syntax error near "order"` behind a green `check`. Quoted, it
/// runs and enforces the guard. SQLite is always available (no external dependency).
#[cfg(feature = "db")]
#[tokio::test]
async fn test_b_reserved_word_identifiers_execute_without_syntax_error() {
    // The generator's emission for reserved-word columns (runtime value: real quotes).
    const RESERVE_KEYWORDS_SQL: &str = "UPDATE \"slots\" SET \"order\" = \"order\" + ? WHERE \"id\" = ? AND \"order\" + ? <= \"limit\"";
    let db = Db::connect("sqlite::memory:").await.unwrap();
    // The reserved words MUST be quoted in DDL too, or the CREATE itself is a syntax error.
    db.conn()
        .execute_unprepared(
            "CREATE TABLE slots (id INTEGER PRIMARY KEY, \"limit\" INTEGER NOT NULL, \
             \"order\" INTEGER NOT NULL DEFAULT 0)",
        )
        .await
        .unwrap();
    db.conn()
        .execute_unprepared("INSERT INTO slots (id, \"limit\", \"order\") VALUES (1, 2, 0)")
        .await
        .unwrap();

    let reserve = |n: i64| {
        let db = db.clone();
        async move {
            db.conn()
                .execute(Statement::from_sql_and_values(
                    db.conn().get_database_backend(),
                    db.sql(RESERVE_KEYWORDS_SQL),
                    [n.into(), 1i64.into(), n.into()],
                ))
                .await
                // An `.unwrap()` here would surface the `syntax error` the UNQUOTED SQL
                // throws — the whole point of the regression.
                .expect("quoted reserved-word reserve must EXECUTE (no syntax error)")
                .rows_affected()
                == 1
        }
    };

    assert!(reserve(1).await, "first reserve fits (0 -> 1 of limit 2)");
    assert!(
        reserve(1).await,
        "second reserve fits exactly (1 -> 2 of limit 2)"
    );
    assert!(
        !reserve(1).await,
        "third reserve is refused (2 -> 3 > limit 2) — the quoted guard still enforces capacity"
    );
}

/// Postgres — the executable #187 proof on a pool with REAL concurrent writers.
/// (i) shows the LANDMINE the primitive removes: the tempting read-then-write reserve
/// oversells under overlapping transactions (`used` ends ABOVE capacity). (ii) shows
/// the shipped atomic UPDATE keeps `used == capacity` across many genuinely-concurrent
/// rounds. Needs a live Postgres; reset the schema first
/// (`DROP SCHEMA public CASCADE; CREATE SCHEMA public`) then run with
/// `JERRYCAN_TEST_PG_URL=… cargo test -p jerrycan --all-features \
/// --test reserve_capacity_concurrency -- --include-ignored`.
#[cfg(feature = "db")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a local postgres (set JERRYCAN_TEST_PG_URL)"]
async fn test_c_postgres_atomic_reserve_never_oversells() {
    let Ok(url) = std::env::var("JERRYCAN_TEST_PG_URL") else {
        eprintln!("SKIP: JERRYCAN_TEST_PG_URL not set");
        return;
    };
    let db = Db::connect(&url).await.unwrap();
    db.conn()
        .execute_unprepared("DROP TABLE IF EXISTS rooms")
        .await
        .unwrap();
    db.conn()
        .execute_unprepared(
            "CREATE TABLE rooms (id BIGSERIAL PRIMARY KEY, capacity BIGINT NOT NULL, \
             used BIGINT NOT NULL DEFAULT 0)",
        )
        .await
        .unwrap();
    db.conn()
        .execute_unprepared("INSERT INTO rooms (id, capacity, used) VALUES (1, 0, 0)")
        .await
        .unwrap();
    let backend = db.conn().get_database_backend();

    // (i) LANDMINE (deterministic worst case): two concurrent callers run the tempting
    // read-then-write reserve — read `used`, check `used + 1 <= capacity` IN APP, then
    // `UPDATE SET used = used + 1`. On a capacity-1 room both read used = 0 (the sleep
    // guarantees both reads land before either writes), both pass the stale check, then
    // both increment — the second increment reads the first's committed write, so `used`
    // climbs to 2 > capacity 1: the reservation is granted TWICE. This is exactly why a
    // read-then-write is unsafe and the single conditional UPDATE (below) is required.
    db.conn()
        .execute(Statement::from_sql_and_values(
            backend,
            db.sql("UPDATE rooms SET used = 0, capacity = 1 WHERE id = ?"),
            [1i64.into()],
        ))
        .await
        .unwrap();
    let read_then_write = |db: Db| async move {
        // read-then-write: the footgun the primitive replaces.
        let backend = db.conn().get_database_backend();
        let row = db
            .conn()
            .query_one(Statement::from_sql_and_values(
                backend,
                db.sql("SELECT used AS u, capacity AS c FROM rooms WHERE id = ?"),
                [1i64.into()],
            ))
            .await
            .unwrap()
            .unwrap();
        let used: i64 = row
            .try_get::<i64>("", "u")
            .or_else(|_| row.try_get::<i32>("", "u").map(i64::from))
            .unwrap();
        let cap: i64 = row
            .try_get::<i64>("", "c")
            .or_else(|_| row.try_get::<i32>("", "c").map(i64::from))
            .unwrap();
        // Give the sibling caller time to read the SAME pre-image before either writes.
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        // The stale check reserving one unit: `used + 1 <= cap`, i.e. `used < cap`.
        if used < cap {
            db.conn()
                .execute(Statement::from_sql_and_values(
                    backend,
                    db.sql("UPDATE rooms SET used = used + 1 WHERE id = ?"),
                    [1i64.into()],
                ))
                .await
                .unwrap();
        }
    };
    let n1 = tokio::spawn(read_then_write(db.clone()));
    let n2 = tokio::spawn(read_then_write(db.clone()));
    n1.await.unwrap();
    n2.await.unwrap();
    assert!(
        used_now(&db).await > 1,
        "the read-then-write reserve MUST oversell a capacity-1 room under concurrency \
         (used > 1) — if it does not, the landmine isn't being exercised"
    );

    // (ii) FIX — the shipped atomic UPDATE. Many rounds of genuinely-concurrent reserves
    // (separate connections on a pool of 5) on the SAME pk row: the row lock serializes
    // them, so exactly `capacity` win and `used` lands on `capacity`, never above.
    for (capacity, k) in [(5i64, 40i64), (1, 32), (10, 60), (25, 80)] {
        assert_no_oversell(&db, capacity, k).await;
    }

    db.conn()
        .execute_unprepared("DROP TABLE IF EXISTS rooms")
        .await
        .unwrap();
}
