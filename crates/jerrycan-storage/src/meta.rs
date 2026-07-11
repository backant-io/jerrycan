//! Object metadata in `jerrycan-db`: the framework-owned `storage_objects`
//! migration and the scoped SQL layer. The metadata row is the SOURCE OF TRUTH
//! for access — listing, ownership, tenant isolation, and owner_prefix checks
//! all run here; the blob store holds only bytes. owner_id/tenant_id are TEXT
//! (stringified pks) so one DDL shape covers i64/string/uuid owners on both
//! dialects (mirrors jerrycan-jobs' one-shape JOBS_MIGRATIONS).

use crate::Scope;
use jerrycan_core::{Error, Result};
use jerrycan_db::sea_orm::{ConnectionTrait, QueryResult, Statement, Value};
use jerrycan_db::{Db, Migration, db_error};
use serde::{Deserialize, Serialize};

/// The framework migration for the object-metadata table. `key` is quoted in
/// every statement (non-reserved but keyword-adjacent on both dialects).
pub const STORAGE_MIGRATIONS: &[Migration] = &[Migration {
    name: "jerrycan_storage_0001_create",
    sqlite: "\
CREATE TABLE storage_objects (
    id         TEXT PRIMARY KEY,
    bucket     TEXT NOT NULL,
    \"key\"      TEXT NOT NULL,
    owner_id   TEXT,
    tenant_id  TEXT,
    size       BIGINT NOT NULL,
    mime       TEXT NOT NULL,
    checksum   TEXT NOT NULL,
    created_at BIGINT NOT NULL
);
CREATE UNIQUE INDEX storage_objects_bucket_key ON storage_objects (bucket, \"key\");
CREATE INDEX storage_objects_scope ON storage_objects (bucket, owner_id);",
    postgres: "\
CREATE TABLE storage_objects (
    id         TEXT PRIMARY KEY,
    bucket     TEXT NOT NULL,
    \"key\"      TEXT NOT NULL,
    owner_id   TEXT,
    tenant_id  TEXT,
    size       BIGINT NOT NULL,
    mime       TEXT NOT NULL,
    checksum   TEXT NOT NULL,
    created_at BIGINT NOT NULL
);
CREATE UNIQUE INDEX storage_objects_bucket_key ON storage_objects (bucket, \"key\");
CREATE INDEX storage_objects_scope ON storage_objects (bucket, owner_id);",
}];

/// One object's metadata row. Serialized as the upload/list/download JSON body.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ObjectMeta {
    pub id: String,
    pub bucket: String,
    pub key: String,
    pub owner_id: Option<String>,
    pub tenant_id: Option<String>,
    pub size: i64,
    pub mime: String,
    /// sha256 hex of the bytes — doubles as the ETag.
    pub checksum: String,
    /// Epoch millis.
    pub created_at: i64,
}

/// A fresh UUIDv4 string from OS randomness (no uuid crate: 16 CSPRNG bytes,
/// version/variant bits set, canonical hyphenation).
pub(crate) fn new_object_id() -> String {
    use rand::RngCore;
    let mut b = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = crate::sign::hex(&b);
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

fn stmt(db: &Db, sql: &str, values: Vec<Value>) -> Statement {
    Statement::from_sql_and_values(db.conn().get_database_backend(), db.sql(sql), values)
}

/// `AND owner_id = ? AND tenant_id = ?` fragments for whichever scope ids are
/// set. An empty scope adds nothing (public/unscoped access).
fn scope_sql(scope: &Scope, sql: &mut String, values: &mut Vec<Value>) {
    if let Some(owner) = &scope.owner_id {
        sql.push_str(" AND owner_id = ?");
        values.push(owner.clone().into());
    }
    if let Some(tenant) = &scope.tenant_id {
        sql.push_str(" AND tenant_id = ?");
        values.push(tenant.clone().into());
    }
}

fn row_to_meta(row: &QueryResult) -> Result<ObjectMeta> {
    let col_err = |c: &str, e: jerrycan_db::sea_orm::DbErr| {
        Error::internal(format!("jerrycan-storage: column `{c}`: {e}"))
    };
    Ok(ObjectMeta {
        id: row.try_get("", "id").map_err(|e| col_err("id", e))?,
        bucket: row
            .try_get("", "bucket")
            .map_err(|e| col_err("bucket", e))?,
        key: row.try_get("", "key").map_err(|e| col_err("key", e))?,
        owner_id: row
            .try_get("", "owner_id")
            .map_err(|e| col_err("owner_id", e))?,
        tenant_id: row
            .try_get("", "tenant_id")
            .map_err(|e| col_err("tenant_id", e))?,
        size: row.try_get("", "size").map_err(|e| col_err("size", e))?,
        mime: row.try_get("", "mime").map_err(|e| col_err("mime", e))?,
        checksum: row
            .try_get("", "checksum")
            .map_err(|e| col_err("checksum", e))?,
        created_at: row
            .try_get("", "created_at")
            .map_err(|e| col_err("created_at", e))?,
    })
}

const COLS: &str = "id, bucket, \"key\", owner_id, tenant_id, size, mime, checksum, created_at";

/// Insert one row. A `(bucket, key)` unique violation maps to 409 via db_error.
pub(crate) async fn insert(db: &Db, m: &ObjectMeta) -> Result<()> {
    db.conn()
        .execute(stmt(
            db,
            "INSERT INTO storage_objects (id, bucket, \"key\", owner_id, tenant_id, size, mime, checksum, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            vec![
                m.id.clone().into(),
                m.bucket.clone().into(),
                m.key.clone().into(),
                m.owner_id.clone().into(),
                m.tenant_id.clone().into(),
                m.size.into(),
                m.mime.clone().into(),
                m.checksum.clone().into(),
                m.created_at.into(),
            ],
        ))
        .await
        .map_err(db_error)?;
    Ok(())
}

/// One object by id within a bucket, filtered by whatever the scope sets —
/// a scoped read of a foreign row is None (the caller's 404).
pub(crate) async fn get_scoped(
    db: &Db,
    bucket: &str,
    id: &str,
    scope: &Scope,
) -> Result<Option<ObjectMeta>> {
    let mut sql = format!("SELECT {COLS} FROM storage_objects WHERE bucket = ? AND id = ?");
    let mut values: Vec<Value> = vec![bucket.into(), id.into()];
    scope_sql(scope, &mut sql, &mut values);
    let row = db
        .conn()
        .query_one(stmt(db, &sql, values))
        .await
        .map_err(db_error)?;
    row.as_ref().map(row_to_meta).transpose()
}

/// A bucket's objects under the scope, ordered by key (stable listings).
pub(crate) async fn list_scoped(db: &Db, bucket: &str, scope: &Scope) -> Result<Vec<ObjectMeta>> {
    let mut sql = format!("SELECT {COLS} FROM storage_objects WHERE bucket = ?");
    let mut values: Vec<Value> = vec![bucket.into()];
    scope_sql(scope, &mut sql, &mut values);
    sql.push_str(" ORDER BY \"key\"");
    let rows = db
        .conn()
        .query_all(stmt(db, &sql, values))
        .await
        .map_err(db_error)?;
    rows.iter().map(row_to_meta).collect()
}

/// Remove one row (scope already proven by the caller's get_scoped).
pub(crate) async fn delete_row(db: &Db, bucket: &str, id: &str) -> Result<()> {
    db.conn()
        .execute(stmt(
            db,
            "DELETE FROM storage_objects WHERE bucket = ? AND id = ?",
            vec![bucket.into(), id.into()],
        ))
        .await
        .map_err(db_error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Scope;
    use jerrycan_db::Db;

    async fn db() -> Db {
        let db = Db::connect("sqlite::memory:").await.expect("test db");
        db.migrate(STORAGE_MIGRATIONS)
            .await
            .expect("storage migrations");
        db
    }

    fn meta(id: &str, key: &str, owner: Option<&str>, tenant: Option<&str>) -> ObjectMeta {
        ObjectMeta {
            id: id.to_string(),
            bucket: "b".to_string(),
            key: key.to_string(),
            owner_id: owner.map(String::from),
            tenant_id: tenant.map(String::from),
            size: 3,
            mime: "text/plain".to_string(),
            checksum: "abc123".to_string(),
            created_at: 1_000,
        }
    }

    fn scope(owner: Option<&str>, tenant: Option<&str>) -> Scope {
        Scope {
            owner_id: owner.map(String::from),
            tenant_id: tenant.map(String::from),
        }
    }

    #[tokio::test]
    async fn insert_get_list_delete_round_trip() {
        let db = db().await;
        insert(&db, &meta("id-1", "a.txt", Some("1"), None))
            .await
            .unwrap();
        let got = get_scoped(&db, "b", "id-1", &Scope::default())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (got.key.as_str(), got.size, got.checksum.as_str()),
            ("a.txt", 3, "abc123")
        );
        assert_eq!(
            list_scoped(&db, "b", &Scope::default())
                .await
                .unwrap()
                .len(),
            1
        );
        delete_row(&db, "b", "id-1").await.unwrap();
        assert!(
            get_scoped(&db, "b", "id-1", &Scope::default())
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn duplicate_bucket_key_is_409() {
        // WHY: unique(bucket, key) is the Supabase-parity contract — a re-upload
        // to the same key must be a client 409, not a silent overwrite or a 500.
        let db = db().await;
        insert(&db, &meta("id-1", "same.txt", None, None))
            .await
            .unwrap();
        let err = insert(&db, &meta("id-2", "same.txt", None, None))
            .await
            .unwrap_err();
        assert_eq!(err.code(), "JC0409");
    }

    #[tokio::test]
    async fn owner_and_tenant_filters_scope_reads_and_lists() {
        // WHY (Rule 9): this IS the isolation mechanism — a scoped read of a
        // foreign row must come back None (the handler's 404), and a scoped
        // list must only contain the caller's rows.
        let db = db().await;
        insert(&db, &meta("o1", "a.txt", Some("1"), Some("10")))
            .await
            .unwrap();
        insert(&db, &meta("o2", "b.txt", Some("2"), Some("20")))
            .await
            .unwrap();
        // Cross-owner get: None. Same-owner get: Some.
        assert!(
            get_scoped(&db, "b", "o1", &scope(Some("2"), None))
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            get_scoped(&db, "b", "o1", &scope(Some("1"), None))
                .await
                .unwrap()
                .is_some()
        );
        // Cross-tenant get: None even with the right owner filter absent.
        assert!(
            get_scoped(&db, "b", "o1", &scope(None, Some("20")))
                .await
                .unwrap()
                .is_none()
        );
        // Scoped list sees only the caller's row.
        let mine = list_scoped(&db, "b", &scope(Some("1"), Some("10")))
            .await
            .unwrap();
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].id, "o1");
        // Unscoped (public) list sees both, ordered by key.
        let all = list_scoped(&db, "b", &Scope::default()).await.unwrap();
        assert_eq!(
            all.iter().map(|m| m.key.as_str()).collect::<Vec<_>>(),
            vec!["a.txt", "b.txt"]
        );
    }

    #[test]
    fn object_ids_are_uuid_v4_shaped_and_unique() {
        let a = new_object_id();
        let b = new_object_id();
        assert_ne!(a, b);
        assert_eq!(a.len(), 36);
        assert_eq!(a.as_bytes()[14], b'4', "version nibble is 4");
    }
}
