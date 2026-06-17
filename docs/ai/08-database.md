# Database

## Purpose
`jerrycan::db` is SQL storage for generated backends: one `Db` handle over
SQLite and Postgres (URL-driven), module-owned dual-dialect migrations, and
SeaORM entities resolved through DI. SeaORM owns SQL rendering — placeholders,
quoting, `RETURNING`, booleans — for whichever engine is connected, so the same
entity code runs on both. Enable with the design dependency `"db"`
(or `jerrycan add db`).

## Signature
```rust
# use jerrycan::prelude::*;
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
use jerrycan::db::{Db, Migration};

let db = Db::connect("sqlite::memory:").await.unwrap();   // or postgres://…; from_env() reads JERRYCAN_DATABASE_URL
db.migrate(&[Migration {                                   // dual-dialect: the connected backend picks its column
    name: "0001_create_notes",
    sqlite: "CREATE TABLE notes (id INTEGER PRIMARY KEY AUTOINCREMENT, text TEXT NOT NULL)",
    postgres: "CREATE TABLE notes (id BIGSERIAL PRIMARY KEY, text TEXT NOT NULL)",
}]).await.unwrap();

let app = App::new().extend(db);                            // Db is an Extension: registers itself app-wide
# let _ = app.into_test();
# }); }
```

## Minimal example
A SeaORM entity, a migration that creates its table on either backend, and a
handler that lists and inserts rows through `db.conn()`:
```rust
# use jerrycan::prelude::*;
# use jerrycan::db::sea_orm::{ActiveModelTrait, EntityTrait, Set};
# use jerrycan::db::{db_error, Db, Migration};
// One entity = one table. `DeriveEntityModel` generates Entity/ActiveModel/Column.
mod note {
    use jerrycan::db::sea_orm;                  // the derive macros emit `sea_orm::…` paths
    use jerrycan::db::sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "notes")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub text: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

async fn list(db: Dep<Db>) -> Result<Json<Vec<String>>> {
    let rows = note::Entity::find().all(db.conn()).await.map_err(db_error)?;
    Ok(Json(rows.into_iter().map(|n| n.text).collect()))
}

async fn create(db: Dep<Db>, Json(text): Json<String>) -> Result<Created<i32>> {
    // ActiveModel + `Set` carries values as binds; `id` is left default (DB-assigned).
    let note = note::ActiveModel { text: Set(text), ..Default::default() };
    let saved = note.insert(db.conn()).await.map_err(db_error)?;
    Ok(Created(saved.id))
}

# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
let db = Db::connect("sqlite::memory:").await.unwrap();
db.migrate(&[Migration {
    name: "0001_create_notes",
    sqlite: "CREATE TABLE notes (id INTEGER PRIMARY KEY AUTOINCREMENT, text TEXT NOT NULL)",
    postgres: "CREATE TABLE notes (id BIGSERIAL PRIMARY KEY, text TEXT NOT NULL)",
}]).await.unwrap();

let t = App::new()
    .extend(db)
    .route("/notes", get(list).post(create))
    .into_test();

assert_eq!(t.get("/notes").await.json::<Vec<String>>(), Vec::<String>::new());
t.post_json("/notes", &"hello").await;
assert_eq!(t.get("/notes").await.json::<Vec<String>>(), vec!["hello".to_string()]);
# }); }
```

## Variations
Wrap multiple writes in a transaction — the closure returning `Err` rolls back
EVERY statement it issued, so a handler never leaves a partial write:
```rust
# use jerrycan::prelude::*;
# use jerrycan::db::sea_orm::{self, ConnectionTrait, TransactionError, TransactionTrait};
# use jerrycan::db::{db_error, Db};
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
# let db = Db::connect("sqlite::memory:").await.unwrap();
# db.conn().execute_unprepared("CREATE TABLE notes (id INTEGER PRIMARY KEY, text TEXT)").await.unwrap();
db.conn()
    .transaction::<_, (), sea_orm::DbErr>(|txn| {
        Box::pin(async move {
            txn.execute_unprepared("INSERT INTO notes VALUES (1, 'a')").await?;
            txn.execute_unprepared("INSERT INTO notes VALUES (2, 'b')").await?;
            Ok(()) // returning Err here rolls BOTH inserts back
        })
    })
    .await
    // `transaction` wraps your error as `TransactionError`; both arms hold a DbErr.
    .map_err(|e| match e {
        TransactionError::Connection(e) | TransactionError::Transaction(e) => db_error(e),
    })?;
# Result::<()>::Ok(())
# }).unwrap(); }
```

- Generated repos take `Dep<Db>` through a factory: `.provide_dep(repo::note_repo)`
  (the tool-owned `lib.rs` wires this; your handlers just declare `repo: Dep<NoteRepo>`).
- `jerrycan db migrate --url postgres://…` applies module migrations from the CLI;
  generated apps also migrate automatically at startup.
- Escape hatch for hand-written SQL — `db.sql()` translates `?`→`$n` for the
  connected backend, then `Statement::from_sql_and_values` binds the values:
  ```rust
  # use jerrycan::prelude::*;
  # use jerrycan::db::sea_orm::{ConnectionTrait, Statement};
  # use jerrycan::db::{db_error, Db};
  # fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
  # let db = Db::connect("sqlite::memory:").await.unwrap();
  # db.conn().execute_unprepared("CREATE TABLE notes (id INTEGER PRIMARY KEY, text TEXT)").await.unwrap();
  let stmt = Statement::from_sql_and_values(
      db.conn().get_database_backend(),
      db.sql("INSERT INTO notes (id, text) VALUES (?, ?)"), // `?`→`$1, $2` on Postgres
      [1.into(), "a".into()],
  );
  db.conn().execute(stmt).await.map_err(db_error)?;
  # Result::<()>::Ok(())
  # }).unwrap(); }
  ```

## Foreign keys in the schema contract (`enforced`)
`jerrycan schema` (and the committed `schema.json`) emits an `"enforced"` bool on
every foreign key. It tells you **who upholds the relation** — don't read
`on_delete` without it:
- **`enforced: true`** — a same-module `belongs_to` becomes a real database
  `FOREIGN KEY` constraint (introspected from the migration). The `on_delete`
  policy (`cascade`/`set_null`/`restrict`) is enforced by the DB itself.
- **`enforced: false`** — a cross-module `belongs_to` is an *indexed but
  application-enforced* relation: the fk column exists (and is indexed) but there
  is **no** DB constraint, because per-module migrations only create their own
  tables. Here `on_delete` is honored by your handlers, **NOT** the database — so
  `{ "on_delete": "cascade", "enforced": false }` does *not* mean the DB will
  cascade-delete; a child row outlives its parent unless a handler removes it.

So `enforced` is the line between a DB-guaranteed constraint and a contract the
code must keep. The `belongs_to` derivation rules behind this live in
`jerrycan docs modules` (Relations); tenant-scoped relations in
`jerrycan docs tenancy`.

## Errors you'll hit
- A unique-key violation surfaces as `409 JC0409` (a re-POSTed id is the
  client's fault); every other database failure is `500 JC0510`. Neither leaks
  internals in the body — the real SeaORM/sqlx error goes to stderr for the
  operator. Always `.map_err(db_error)?` so both codes happen for free.
- A failing migration stops the run and is NOT recorded — fix it and rerun.

## Anti-patterns
- Don't build SQL strings from request input — request values enter queries only
  as binds (entity `Set`/`build_any` values, or `from_sql_and_values`). The
  jerrycan lint walks repos for SQL outside `repo.rs`, and string-built SQL is
  the one injection door this framework refuses to open.
- Don't share one Postgres database across parallel tests — generated acceptance
  tests use `sqlite::memory:` per test for hermetic isolation.
- Booleans are NATIVE under SeaORM — model them as `bool` and use migrations
  with `BOOLEAN`. Never store them as `0`/`1` integers or compare with `= 0`/`= 1`;
  that old as-`i64` workaround is gone.
- JSON columns are `sea_orm` `Json` (`serde_json::Value`) — store the value
  directly. Never `serde_json::to_string` it first; double-encoding turns a JSON
  object into a quoted string the next reader can't parse.
