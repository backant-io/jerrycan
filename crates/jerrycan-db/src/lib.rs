//! Database extension: one URL-driven `Db` over SQLite and Postgres (sqlx Any),
//! module-owned dual-dialect migrations, and a deterministic `?`→`$n` translator
//! (sqlx's Any driver does NOT translate placeholders; ours is quote-blind and
//! safe because generated SQL never embeds string literals).
#![forbid(unsafe_code)]

use jerrycan_core::{App, Error, Extension, Result};

/// Which engine the pool speaks. Generated code branches on this for the few
/// statements that genuinely differ (insert-id strategies, DDL).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Sqlite,
    Postgres,
}

/// The database dependency: a cloneable pool handle. Register app-wide with
/// `App::new().extend(db)` (or `.provide(db)` — `extend` is the §6 seam).
#[derive(Clone)]
pub struct Db {
    pool: sqlx::AnyPool,
    backend: Backend,
}

impl Db {
    /// Connect by URL: `sqlite::memory:`, `sqlite://path.db`, `postgres://…`.
    pub async fn connect(url: &str) -> Result<Self> {
        sqlx::any::install_default_drivers(); // idempotent
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
        let pool = sqlx::any::AnyPoolOptions::new()
            .max_connections(max)
            .connect(url)
            .await
            .map_err(db_error)?;
        Ok(Self { pool, backend })
    }

    /// `JERRYCAN_DATABASE_URL`, defaulting to `sqlite::memory:` for dev.
    pub async fn from_env() -> Result<Self> {
        let url = std::env::var("JERRYCAN_DATABASE_URL")
            .unwrap_or_else(|_| "sqlite::memory:".to_string());
        Self::connect(&url).await
    }

    pub fn pool(&self) -> &sqlx::AnyPool {
        &self.pool
    }

    pub fn backend(&self) -> Backend {
        self.backend
    }

    /// Backend-correct placeholders for a `?`-style query string.
    pub fn sql(&self, query: &str) -> String {
        translate_placeholders(query, self.backend)
    }
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

/// Map any sqlx error to the stable JC0510 without leaking internals; the
/// underlying detail goes to stderr for the operator.
pub fn db_error(e: sqlx::Error) -> Error {
    eprintln!("jerrycan-db: {e}");
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

/// Re-exported for generated code: `jerrycan::db::sqlx::{query, Row}` — route
/// crates never declare sqlx themselves.
pub use sqlx;

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::Row;

    #[tokio::test]
    async fn sqlite_memory_is_one_database_across_queries() {
        // Decision #4: sqlite pools are single-connection — otherwise every
        // pooled connection of sqlite::memory: is its OWN empty database.
        let db = Db::connect("sqlite::memory:").await.unwrap();
        assert_eq!(db.backend(), Backend::Sqlite);
        sqlx::query("CREATE TABLE t (x BIGINT)")
            .execute(db.pool())
            .await
            .unwrap();
        sqlx::query("INSERT INTO t (x) VALUES (?)")
            .bind(7i64)
            .execute(db.pool())
            .await
            .unwrap();
        let row = sqlx::query("SELECT x FROM t")
            .fetch_one(db.pool())
            .await
            .unwrap();
        let x: i64 = row.get("x");
        assert_eq!(x, 7, "second pooled query must see the first one's table");
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
        let e = db_error(sqlx::Error::RowNotFound);
        assert_eq!(e.code(), "JC0510");
        assert_eq!(e.message(), "database error");
    }
}
