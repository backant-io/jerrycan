//! Database extension: one URL-driven `Db` over SQLite and Postgres
//! (sea-orm's `DatabaseConnection`), module-owned dual-dialect migrations, and a
//! deterministic `?`→`$n` translator (placeholders are library-owned; ours is
//! quote-blind and safe because generated SQL never embeds string literals).
#![forbid(unsafe_code)]

use jerrycan_core::{App, Error, Extension, Result};
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};

// Connections are driven by sea-orm; generated repos build ALL SQL through
// sea-query (dialect rendering is library-owned: placeholders, RETURNING,
// quoting). Re-exported so generated crates depend on `jerrycan` alone.
pub use sea_orm;
pub use sea_query;
pub use sea_query_binder;

/// Which engine the connection speaks. Generated code branches on this for the
/// few statements that genuinely differ (insert-id strategies, DDL).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Sqlite,
    Postgres,
}

/// The database dependency: a cloneable connection handle. Register app-wide
/// with `App::new().extend(db)` (or `.provide(db)` — `extend` is the §6 seam).
#[derive(Clone)]
pub struct Db {
    conn: DatabaseConnection,
    backend: Backend,
}

impl Db {
    /// Connect by URL: `sqlite::memory:`, `sqlite://path.db`, `postgres://…`.
    pub async fn connect(url: &str) -> Result<Self> {
        let backend = if url.starts_with("postgres") {
            Backend::Postgres
        } else if url.starts_with("sqlite") {
            Backend::Sqlite
        } else {
            return Err(Error::internal(format!(
                "unsupported database url scheme: `{url}` (sqlite:// or postgres:// in v0)"
            )));
        };
        // Decision #4: one connection for sqlite (memory correctness + writer lock),
        // small default pool for postgres.
        let max = match backend {
            Backend::Sqlite => 1,
            Backend::Postgres => 5,
        };
        let mut opts = sea_orm::ConnectOptions::new(url.to_string());
        opts.max_connections(max);
        let conn = Database::connect(opts).await.map_err(db_error)?;
        Ok(Self { conn, backend })
    }

    /// `JERRYCAN_DATABASE_URL`, defaulting to `sqlite::memory:` for dev.
    pub async fn from_env() -> Result<Self> {
        let url = std::env::var("JERRYCAN_DATABASE_URL")
            .unwrap_or_else(|_| "sqlite::memory:".to_string());
        Self::connect(&url).await
    }

    /// The underlying sea-orm connection. Generated repos and migrations execute
    /// through this handle (`execute_unprepared`, `query_one`, …).
    pub fn conn(&self) -> &DatabaseConnection {
        &self.conn
    }

    pub fn backend(&self) -> Backend {
        self.backend
    }

    /// Backend-correct placeholders for a `?`-style query string.
    pub fn sql(&self, query: &str) -> String {
        translate_placeholders(query, self.backend)
    }

    /// The sea-query builder matching this connection's dialect. Generated repos
    /// pass it to `build_any` so one builder call renders correct SQL
    /// (placeholders, RETURNING, quoting) for whichever engine is connected.
    pub fn query_builder(&self) -> &'static dyn sea_query::QueryBuilder {
        match self.backend {
            Backend::Sqlite => &sea_query::SqliteQueryBuilder,
            Backend::Postgres => &sea_query::PostgresQueryBuilder,
        }
    }

    /// The sea-orm backend tag for this connection — selects the dialect when
    /// constructing a [`Statement`] from raw SQL and bound values.
    fn backend_db(&self) -> sea_orm::DatabaseBackend {
        match self.backend {
            Backend::Sqlite => sea_orm::DatabaseBackend::Sqlite,
            Backend::Postgres => sea_orm::DatabaseBackend::Postgres,
        }
    }
}

/// One migration, both dialects. Generated apps embed these via the tool-owned
/// `app/src/migrations.rs`; modules own the .sql files (spec §5 anatomy).
#[derive(Debug, Clone, Copy)]
pub struct Migration {
    pub name: &'static str,
    pub sqlite: &'static str,
    pub postgres: &'static str,
}

/// Runtime-loaded migration (CLI `jerrycan db migrate` reads module files from
/// disk). The owned twin of [`Migration`]; both delegate to the same runner.
#[derive(Debug, Clone)]
pub struct OwnedMigration {
    pub name: String,
    pub sqlite: String,
    pub postgres: String,
}

impl Db {
    /// Apply pending migrations in slice order; returns the names applied.
    /// Tracking table `_jerrycan_migrations` remembers what ran. A failure
    /// stops the run and records nothing for the failed entry.
    pub async fn migrate(&self, migrations: &[Migration]) -> Result<Vec<String>> {
        self.migrate_iter(migrations.iter().map(|m| (m.name, m.sqlite, m.postgres)))
            .await
    }

    /// Owned-migration twin of [`migrate`](Self::migrate) — same runner.
    pub async fn migrate_owned(&self, migrations: &[OwnedMigration]) -> Result<Vec<String>> {
        self.migrate_iter(
            migrations
                .iter()
                .map(|m| (m.name.as_str(), m.sqlite.as_str(), m.postgres.as_str())),
        )
        .await
    }

    /// The shared core: apply each `(name, sqlite, postgres)` in order, skipping
    /// already-recorded names. A failure stops the run, recording nothing for it.
    async fn migrate_iter<'a>(
        &self,
        items: impl Iterator<Item = (&'a str, &'a str, &'a str)>,
    ) -> Result<Vec<String>> {
        self.conn
            .execute_unprepared(
                "CREATE TABLE IF NOT EXISTS _jerrycan_migrations (name TEXT PRIMARY KEY, applied_at TEXT NOT NULL)",
            )
            .await
            .map_err(db_error)?;

        let mut applied = Vec::new();
        for (name, sqlite, postgres) in items {
            let seen = self
                .conn
                .query_one(Statement::from_sql_and_values(
                    self.backend_db(),
                    self.sql("SELECT name FROM _jerrycan_migrations WHERE name = ?"),
                    [name.into()],
                ))
                .await
                .map_err(db_error)?;
            if seen.is_some() {
                continue;
            }
            let statement = match self.backend {
                Backend::Sqlite => sqlite,
                Backend::Postgres => postgres,
            };
            self.conn.execute_unprepared(statement).await.map_err(|e| {
                eprintln!("jerrycan-db: migration `{name}` failed");
                db_error(e)
            })?;
            self.conn
                .execute(Statement::from_sql_and_values(
                    self.backend_db(),
                    self.sql("INSERT INTO _jerrycan_migrations (name, applied_at) VALUES (?, ?)"),
                    [name.into(), chrono_free_timestamp().into()],
                ))
                .await
                .map_err(db_error)?;
            applied.push(name.to_string());
        }
        Ok(applied)
    }
}

/// RFC3339-ish UTC timestamp without a chrono dependency (seconds precision).
fn chrono_free_timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("unix:{secs}")
}

/// `?` → `$1, $2, …` for Postgres; identity for SQLite. Quote-blind by design:
/// generated SQL never embeds string literals (binds carry all values).
pub fn translate_placeholders(query: &str, backend: Backend) -> String {
    match backend {
        Backend::Sqlite => query.to_string(),
        Backend::Postgres => {
            let mut out = String::with_capacity(query.len() + 8);
            let mut n = 0;
            for ch in query.chars() {
                if ch == '?' {
                    n += 1;
                    out.push('$');
                    out.push_str(&n.to_string());
                } else {
                    out.push(ch);
                }
            }
            out
        }
    }
}

/// Map any sea-orm error to a stable JC code without leaking internals; the
/// underlying detail goes to stderr for the operator. Unique-key violations
/// are the client's fault (a re-POSTed id), not a server fault — they map to
/// 409 JC0409 so duplicate writes can't pollute 5xx alerting.
pub fn db_error(e: sea_orm::DbErr) -> Error {
    eprintln!("jerrycan-db: {e}");
    if matches!(
        e.sql_err(),
        Some(sea_orm::SqlErr::UniqueConstraintViolation(_))
    ) {
        return Error::conflict("conflict: a row with this key already exists");
    }
    Error::new(
        jerrycan_core::http::StatusCode::INTERNAL_SERVER_ERROR,
        "JC0510",
        "database error",
    )
}

impl Extension for Db {
    fn register(self, app: App) -> App {
        app.provide(self)
    }
}

/// Re-exported for generated code that still reaches for sqlx types directly;
/// route crates never declare sqlx themselves.
pub use sqlx;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn connects_and_executes_via_sea_orm() {
        // Decision #4: sqlite connections are single-connection — otherwise every
        // pooled connection of sqlite::memory: is its OWN empty database.
        let db = Db::connect("sqlite::memory:").await.unwrap();
        assert_eq!(db.backend(), Backend::Sqlite);
        db.conn()
            .execute_unprepared("CREATE TABLE t (id INTEGER PRIMARY KEY)")
            .await
            .unwrap();
    }

    #[test]
    fn placeholder_translation_is_backend_aware() {
        assert_eq!(
            translate_placeholders("INSERT INTO t (a, b) VALUES (?, ?)", Backend::Postgres),
            "INSERT INTO t (a, b) VALUES ($1, $2)"
        );
        assert_eq!(
            translate_placeholders("INSERT INTO t (a, b) VALUES (?, ?)", Backend::Sqlite),
            "INSERT INTO t (a, b) VALUES (?, ?)"
        );
    }

    #[tokio::test]
    async fn from_env_defaults_to_sqlite_memory() {
        // JERRYCAN_DATABASE_URL unset in the test env → default.
        let db = Db::from_env().await.unwrap();
        assert_eq!(db.backend(), Backend::Sqlite);
    }

    #[test]
    fn db_errors_are_jc0510_and_leak_nothing() {
        let e = db_error(sea_orm::DbErr::Custom("boom".into()));
        assert_eq!(e.code(), "JC0510");
        assert_eq!(e.message(), "database error");
    }

    /// The whole generated-repo chain in one place: sea-query renders the SQL
    /// and binds the values; the connection is only the executor. If this
    /// breaks, every generated repo breaks with it.
    #[tokio::test]
    async fn sea_query_builds_and_executes_via_the_connection() {
        use sea_query::{Alias, Expr, Query};

        let db = Db::connect("sqlite::memory:").await.unwrap();
        db.conn()
            .execute_unprepared("CREATE TABLE sq (id INTEGER PRIMARY KEY, title TEXT NOT NULL)")
            .await
            .unwrap();

        let (sql, values) = Query::insert()
            .into_table(Alias::new("sq"))
            .columns([Alias::new("id"), Alias::new("title")])
            .values_panic([7.into(), "hello".into()])
            .returning(Query::returning().columns([Alias::new("id")]))
            .build_any(db.query_builder());
        let row = db
            .conn()
            .query_one(Statement::from_sql_and_values(db.backend_db(), sql, values))
            .await
            .unwrap()
            .expect("RETURNING id row");
        assert_eq!(
            row.try_get::<i64>("", "id").unwrap(),
            7,
            "RETURNING id round-trips"
        );

        let (sql, values) = Query::select()
            .columns([Alias::new("id"), Alias::new("title")])
            .from(Alias::new("sq"))
            .and_where(Expr::col(Alias::new("id")).eq(7))
            .build_any(db.query_builder());
        let row = db
            .conn()
            .query_one(Statement::from_sql_and_values(db.backend_db(), sql, values))
            .await
            .unwrap()
            .expect("select row");
        assert_eq!(row.try_get::<String>("", "title").unwrap(), "hello");
    }

    /// A duplicate key is the CLIENT's fault: it must surface as 409 JC0409,
    /// not a 500 — a re-POSTed id must never trip server-fault alerting.
    #[tokio::test]
    async fn unique_violations_map_to_409_conflict() {
        let db = Db::connect("sqlite::memory:").await.unwrap();
        db.conn()
            .execute_unprepared("CREATE TABLE u (id INTEGER PRIMARY KEY, t TEXT)")
            .await
            .unwrap();
        db.conn()
            .execute_unprepared("INSERT INTO u VALUES (1, 'a')")
            .await
            .unwrap();
        let dup = db
            .conn()
            .execute_unprepared("INSERT INTO u VALUES (1, 'b')")
            .await
            .expect_err("duplicate pk must fail");
        let e = db_error(dup);
        assert_eq!(e.code(), "JC0409");
        assert_eq!(e.status().as_u16(), 409);
        // Still no internals in the message.
        assert!(!e.message().contains("sqlite"), "{}", e.message());
    }

    fn demo_migrations() -> Vec<Migration> {
        vec![
            Migration {
                name: "0001_create_todos",
                sqlite: "CREATE TABLE todos (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL)",
                postgres: "CREATE TABLE todos (id BIGSERIAL PRIMARY KEY, title TEXT NOT NULL)",
            },
            Migration {
                name: "0002_add_done",
                sqlite: "ALTER TABLE todos ADD COLUMN done BOOLEAN NOT NULL DEFAULT 0",
                postgres: "ALTER TABLE todos ADD COLUMN done BOOLEAN NOT NULL DEFAULT FALSE",
            },
        ]
    }

    #[tokio::test]
    async fn migrations_apply_in_order_and_only_once() {
        let db = Db::connect("sqlite::memory:").await.unwrap();
        let applied = db.migrate(&demo_migrations()).await.unwrap();
        assert_eq!(applied, vec!["0001_create_todos", "0002_add_done"]);

        // Re-running applies nothing (tracking table remembers).
        let applied = db.migrate(&demo_migrations()).await.unwrap();
        assert!(applied.is_empty());

        // The schema is genuinely there.
        db.conn()
            .execute_unprepared("INSERT INTO todos (title, done) VALUES ('x', 1)")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn owned_migrations_apply_in_order_and_only_once() {
        let db = Db::connect("sqlite::memory:").await.unwrap();
        let owned = vec![
            OwnedMigration {
                name: "0001_create_todos".into(),
                sqlite:
                    "CREATE TABLE todos (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL)"
                        .into(),
                postgres: "CREATE TABLE todos (id BIGSERIAL PRIMARY KEY, title TEXT NOT NULL)"
                    .into(),
            },
            OwnedMigration {
                name: "0002_add_done".into(),
                sqlite: "ALTER TABLE todos ADD COLUMN done BOOLEAN NOT NULL DEFAULT 0".into(),
                postgres: "ALTER TABLE todos ADD COLUMN done BOOLEAN NOT NULL DEFAULT FALSE".into(),
            },
        ];
        let applied = db.migrate_owned(&owned).await.unwrap();
        assert_eq!(applied, vec!["0001_create_todos", "0002_add_done"]);
        // Re-running applies nothing (shares the tracking table with `migrate`).
        let applied = db.migrate_owned(&owned).await.unwrap();
        assert!(applied.is_empty());
    }

    #[tokio::test]
    async fn a_failing_migration_surfaces_jc0510_and_is_not_recorded() {
        let db = Db::connect("sqlite::memory:").await.unwrap();
        let bad = vec![Migration {
            name: "0001_broken",
            sqlite: "CREATE GARBAGE",
            postgres: "CREATE GARBAGE",
        }];
        let err = db.migrate(&bad).await.unwrap_err();
        assert_eq!(err.code(), "JC0510");

        // Fixing it lets the same name apply afresh — failures are not recorded.
        let good = vec![Migration {
            name: "0001_broken",
            sqlite: "CREATE TABLE ok (x BIGINT)",
            postgres: "CREATE TABLE ok (x BIGINT)",
        }];
        let applied = db.migrate(&good).await.unwrap();
        assert_eq!(applied, vec!["0001_broken"]);
    }
}
