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

// `from_env()` reads JERRYCAN_DATABASE_URL, defaulting to `sqlite::memory:`
// when it's unset — so dev/test "just works" with no database to provision.
let db = Db::connect("sqlite::memory:").await.unwrap();   // or postgres://…
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
- Startup work (an idempotent dev seed, cache warm-up) goes in the AGENT-owned
  `crates/app/src/boot.rs` — `on_boot(db: &Db)` runs after migrations and before
  the app serves. It is created once and preserved across `jerrycan generate`
  (the tool-owned `main.rs` calls it). Keep it idempotent: it runs on every boot.
  (`jerrycan db seed` is separate — it applies a Supabase migration's streamed seed.)
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

## Concurrency & atomic reservations
The two backends have **different write concurrency**, and it changes what is
safe. `Db::connect` caps the pool per backend (`jerrycan-db` `lib.rs`):

- **SQLite — pool max = 1.** One pooled connection, so every write **serializes**
  (a single writer). This is deliberate: `sqlite::memory:` is per-connection (a
  second connection is its own empty database), and a single writer is SQLite's
  correctness story. A read-then-write sequence can never interleave here — it is
  *accidentally* race-free.
- **Postgres — pool max = 5.** A real pool with **concurrent writers**. A
  read-then-write sequence issued by two requests **can interleave**.

That gap is a trap for "reserve N of a limited resource" (seats, stock, credits).
The tempting shape — *read the remaining capacity, check it, then insert/update* —
passes every SQLite test (the single writer serializes it) and **silently
oversells on Postgres**, where two requests both read the same remaining capacity,
both pass the check, and both write.

> **WARNING — a read-capacity-then-insert reservation passes every SQLite test and
> silently oversells on Postgres.** Do not gate a write on a prior, separate read.
> Use the atomic conditional UPDATE below — it is correct on both backends.

### The safe pattern — one atomic conditional UPDATE
Reserve and check in a **single statement**. The `WHERE` clause carries the
capacity guard, so the row is updated only while the reservation still fits; the
database locks the row for the write, so no two callers can both pass the check:

```sql
UPDATE resource SET used = used + :n
WHERE id = :id AND used + :n <= capacity
```

Then read the affected-row count: **1 ⇒ reserved**, **0 ⇒ at capacity, reject
(409)**. A single UPDATE is atomic on **both** backends. Using the raw-SQL escape
hatch (`db.sql()` + `Statement`, shown above):

```rust
# use jerrycan::prelude::*;
# use jerrycan::db::sea_orm::{ConnectionTrait, Statement};
# use jerrycan::db::{db_error, Db};
// Reserve `n` units of a capacity-limited row in ONE atomic statement. The
// UPDATE matches only while the reservation fits, so two concurrent callers can
// never both pass the capacity check — no oversell, on SQLite or Postgres.
async fn reserve(db: &Db, id: i64, n: i64) -> Result<()> {
    let stmt = Statement::from_sql_and_values(
        db.conn().get_database_backend(),
        db.sql("UPDATE rooms SET used = used + ? WHERE id = ? AND used + ? <= capacity"),
        [n.into(), id.into(), n.into()],
    );
    let reserved = db.conn().execute(stmt).await.map_err(db_error)?.rows_affected();
    if reserved == 1 {
        Ok(())                                   // 1 row ⇒ reserved
    } else {
        Err(Error::conflict("at capacity"))      // 0 rows ⇒ full → 409
    }
}

# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
# let db = Db::connect("sqlite::memory:").await.unwrap();
# db.conn().execute_unprepared("CREATE TABLE rooms (id INTEGER PRIMARY KEY, used INTEGER NOT NULL DEFAULT 0, capacity INTEGER NOT NULL)").await.unwrap();
# db.conn().execute_unprepared("INSERT INTO rooms (id, used, capacity) VALUES (1, 0, 2)").await.unwrap();
reserve(&db, 1, 1).await.unwrap();                       // 0 → 1, fits
reserve(&db, 1, 1).await.unwrap();                       // 1 → 2, fits exactly
assert!(reserve(&db, 1, 1).await.is_err());              // 2 → 3 > capacity: rejected — no oversell
# }); }
```

### Generated `reserve` method — `reserve_against`
You do not have to hand-write that UPDATE. Declaring `reserve_against` on an
integer *counter* field, naming the sibling integer *capacity* it is bounded by,
wires the atomic reserve for you:

```json
{
  "name": "bookings",
  "fields": [
    { "name": "capacity", "type": "integer" },
    { "name": "used", "type": "integer", "default": 0, "reserve_against": "capacity" }
  ]
}
```

The generator emits `BookingRepo::reserve(&self, id, n) -> Result<bool>` on the
SQL-backed repo — the exact conditional UPDATE shown above, returning `Ok(true)`
when the reservation fit (reserved) and `Ok(false)` at capacity (or no such row).
Both stay ordinary integer columns; only the method is wired. **Prefer the
generated `reserve` over hand-writing the pattern** — a hand-written
read-then-write silently oversells on Postgres (see the WARNING above), and the
generated method is the #108-proven UPDATE by construction. The counter and its
capacity must be DISTINCT integer non-`id` fields, at most one `reserve_against`
per entity, on a DB-backed design — otherwise `jerrycan check` refuses with
JC0564.

### Multi-row capacity — `SELECT … FOR UPDATE` in a transaction
When the capacity is derived across several rows and a single UPDATE can't
express it, lock the capacity row(s) first inside a `transaction()`:
`SELECT … FOR UPDATE` takes a Postgres row lock, so a concurrent reserver blocks
until you commit — then it reads your write, not stale capacity. It is a Postgres
row lock; on SQLite it is a harmless no-op (the single writer already serializes).
Prefer the single conditional UPDATE above; reach for `FOR UPDATE` only for the
genuinely multi-row case.

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

## Cross-module data access
A route crate exports only `module()` (the JL0001 lint enforces it), so you
CANNOT import a sibling module's `model`/`repo`. There are two supported
channels — pick by *what* you need to share:

- **A shared TYPE** (a DTO/enum both modules serialize) → put it in the app's
  `shared` crate (`crates/shared/src/lib.rs`). Every route crate already depends
  on `shared`; keep it deliberately tiny (a lint guards its growth).
- **Another module's TABLE** (an admin sweep, a cross-module read) → declare a
  **narrow second SeaORM entity** on that table in YOUR module's agent-owned
  `model.rs`. A SeaORM entity is just a typed description of a table, and the
  running app has every module's tables migrated — so a second entity pointing at
  the same `table_name` resolves at runtime and queries through `db.conn()` like
  any of your own. Declare only the columns you actually touch.

```rust
# use jerrycan::prelude::*;
# use jerrycan::db::sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
# use jerrycan::db::{db_error, Db};
// A NARROW second entity for another module's `subscribers` table, declared in
// YOUR module's agent-owned `model.rs` — only the columns the sweep touches.
mod subscriber {
    use jerrycan::db::sea_orm;
    use jerrycan::db::sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "subscribers")]   // must match the OWNING module's migration
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub status: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

// An admin sweep — flip every `pending` subscriber to `expired`. A cross-module
// write with no access to the subscribers crate, only to its table.
async fn expire_pending(db: Dep<Db>) -> Result<Json<u64>> {
    let stale = subscriber::Entity::find()
        .filter(subscriber::Column::Status.eq("pending"))
        .all(db.conn())
        .await
        .map_err(db_error)?;
    let mut expired = 0u64;
    for row in stale {
        // Update by pk: build the ActiveModel with `id` Set, change `status`.
        let m = subscriber::ActiveModel {
            id: Set(row.id),
            status: Set("expired".to_string()),
        };
        m.update(db.conn()).await.map_err(db_error)?;
        expired += 1;
    }
    Ok(Json(expired))
}
# fn main() { let _ = expire_pending; }
```

Do NOT instead hand-edit the owning module's `lib.rs` to re-export its entity, or
add any `pub` item beyond `module()` to a route crate's `lib.rs` — JL0001 flags
it, and the next `jerrycan generate` clobbers the edit (`lib.rs` is tool-owned).
`model.rs` is agent-owned; the second entity belongs there.

### Where it lives in an ENTITY-LESS module (an admin sweep with no table)
An admin/webhook module that declares **no entity of its own** has **no
`model.rs`** — the generator only writes `model.rs`/`repo.rs` for modules that
declare an entity, and such a module's `lib.rs` only carries `mod deps;` and
`mod handlers;`. So there is no natural `model.rs` home for the narrow second
entity. Put it **inline in that module's agent-owned `handlers.rs`** — the
`mod subscriber { … }` block above the sweep handler, exactly as shown above,
just in `handlers.rs` instead of `model.rs`. `handlers.rs` is agent-owned and
always present, so the inline entity survives regeneration.

Do **not** work around the missing `model.rs` by creating one and hand-adding
`mod model;` to `lib.rs`: `lib.rs` is tool-owned, so the next `jerrycan add` or
`jerrycan generate route <module>` rewrites it and your `mod` line is dropped
(the command now WARNS by name when this happens — but the line is still gone).
Keep cross-module entities in `handlers.rs` and the regeneration never touches them.

**The tradeoff — you now keep two entity definitions of one table in sync.** The
OWNING module's migration is the single source of truth for that table's schema;
your second entity is a hand-maintained view of it. If the owner adds or renames
a column your entity reads, nothing checks the two still agree — YOU update your
copy. Keeping the second entity narrow (only the columns the sweep needs) shrinks
that surface.

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
- `datetime` and `uuid` design fields are `String` at the Rust layer (no native
  time/uuid type yet) — the column is TEXT and the model field is `String`. Parse
  and format them yourself in handlers. For a server-set create timestamp, prefer
  the design sentinel `"default": "now"` on the `datetime` field (see 00-designing.md):
  it drops the field from both request DTOs and the generated create stub sets it via
  `now_rfc3339()` — the prelude helper returning the current UTC time as RFC3339
  (`YYYY-MM-DDTHH:MM:SSZ`). Call `now_rfc3339()` directly in a handler for any other
  timestamp (e.g. an `updated_at` you set on write).
- In memory mode, an absent optional field reads back as its type default (`0`/`""`),
  which may sit outside a `min`/`max` bound; db mode stores NULL (true absence). Set a
  `default` to control the absent value, or use db mode for NULL semantics.
