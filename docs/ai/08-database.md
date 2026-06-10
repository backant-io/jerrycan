# Database

## Purpose
`jerrycan::db` is SQL storage for generated backends: one `Db` handle over
SQLite and Postgres (URL-driven), module-owned migrations, repos resolved
through DI. Enable with the design dependency `"db"` (or `jerrycan add db`).

## Signature
```rust
# use jerrycan::prelude::*;
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
use jerrycan::db::{Db, Migration};

let db = Db::connect("sqlite::memory:").await.unwrap();   // or postgres://…; from_env() reads JERRYCAN_DATABASE_URL
db.migrate(&[Migration {
    name: "0001_create_notes",
    sqlite: "CREATE TABLE notes (id INTEGER PRIMARY KEY AUTOINCREMENT, text TEXT NOT NULL)",
    postgres: "CREATE TABLE notes (id BIGSERIAL PRIMARY KEY, text TEXT NOT NULL)",
}]).await.unwrap();

let app = App::new().extend(db);                            // Db is an Extension: registers itself app-wide
# let _ = app.into_test();
# }); }
```

## Minimal example
```rust
# use jerrycan::prelude::*;
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
use jerrycan::db::{db_error, Db};
use jerrycan::db::sqlx::Row;

async fn count(db: Dep<Db>) -> Result<Json<i64>> {
    let row = jerrycan::db::sqlx::query("SELECT COUNT(*) AS n FROM notes")
        .fetch_one(db.pool())
        .await
        .map_err(db_error)?;
    Ok(Json(row.get("n")))
}

let db = Db::connect("sqlite::memory:").await.unwrap();
db.migrate(&[jerrycan::db::Migration {
    name: "0001_create_notes",
    sqlite: "CREATE TABLE notes (id INTEGER PRIMARY KEY AUTOINCREMENT, text TEXT NOT NULL)",
    postgres: "CREATE TABLE notes (id BIGSERIAL PRIMARY KEY, text TEXT NOT NULL)",
}]).await.unwrap();

let t = App::new().extend(db).route("/count", get(count)).into_test();
assert_eq!(t.get("/count").await.json::<i64>(), 0);
# }); }
```

## Variations
- Placeholders: write `?` everywhere and wrap backend-sensitive strings with
  `db.sql("… WHERE id = ?")` — it becomes `$1` on Postgres, stays `?` on SQLite.
- Generated repos take `Dep<Db>` through a factory: `.provide_dep(repo::todo_repo)`
  (the tool-owned lib.rs wires this; your handlers just declare `repo: Dep<TodoRepo>`).
- `jerrycan db migrate --url postgres://…` applies module migrations from the CLI;
  generated apps also migrate automatically at startup.

## Errors you'll hit
- Any database failure surfaces as `500 JC0510` with NO internals in the body —
  the real sqlx error goes to stderr for the operator.
- A failing migration stops the run and is NOT recorded — fix it and rerun.

## Anti-patterns
- Don't build SQL strings from request input — bind with `?` always (the
  jerrycan lint walks repos for SQL outside repo.rs, and string-built SQL is
  the one injection door this framework refuses to open).
- Don't share one Postgres database across parallel tests — generated
  acceptance tests use `sqlite::memory:` per test for hermetic isolation.
- `db.sql()` translates EVERY `?` — never put a literal `?` inside a string you
  pass through it; values always travel as binds.
- Boolean fields are stored as `BIGINT` 0/1 on both backends (sqlx Any cannot
  round-trip native booleans on SQLite). In hand-written SQL compare with
  `= 0`/`= 1` — never `TRUE`/`FALSE` literals (that breaks on Postgres).
