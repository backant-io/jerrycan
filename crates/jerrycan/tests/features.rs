//! The facade's feature-gated extension re-exports: generated apps depend on
//! `jerrycan = { features = ["db", "validate"] }` and import `jerrycan::db::…`.
//! This test file only compiles its bodies when the features are on — the
//! workspace gate runs with --all-features, so CI always checks them.

#[cfg(feature = "db")]
#[tokio::test]
async fn db_reexport_is_usable() {
    let db = jerrycan::db::Db::connect("sqlite::memory:").await.unwrap();
    assert_eq!(db.backend(), jerrycan::db::Backend::Sqlite);
    // The sqlx re-export rides along for generated repos:
    jerrycan::db::sqlx::query("CREATE TABLE t (x BIGINT)")
        .execute(db.pool())
        .await
        .unwrap();
}

#[cfg(feature = "validate")]
#[test]
fn validate_reexport_is_usable() {
    let v = jerrycan::validate::Violation::new("f", "m");
    assert_eq!(v.field, "f");
}
