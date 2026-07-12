//! The offline export-directory contract (spec §Input contract). Layout:
//! schema.sql (required), data/<schema>.<table>.csv, storage/{buckets.json,objects/},
//! functions/<name>/, cron.sql — every reader documents the command that produces it.

use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct Export {
    pub root: PathBuf,
    pub schema_sql: String,
    /// (schema, table, path), sorted by (schema, table) — deterministic order.
    pub data_files: Vec<(String, String, PathBuf)>,
    pub buckets_json: Option<String>,
    /// Bucket-name directories under storage/objects/.
    pub object_dirs: Vec<PathBuf>,
    /// Edge-function directories under functions/.
    pub function_dirs: Vec<PathBuf>,
    pub cron_sql: Option<String>,
}

impl Export {
    pub fn open(root: &Path) -> Result<Self, String> {
        let schema_path = root.join("schema.sql");
        let schema_sql = std::fs::read_to_string(&schema_path).map_err(|_| {
            format!(
                "{} not found — produce it with `supabase db dump --schema public,auth,storage -f schema.sql` \
                 (or `pg_dump --schema-only --schema=public --schema=auth --schema=storage`); \
                 see `jerrycan docs migrate-supabase` for the full export layout",
                schema_path.display()
            )
        })?;
        let mut data_files = Vec::new();
        if let Ok(entries) = std::fs::read_dir(root.join("data")) {
            for entry in entries.flatten() {
                let path = entry.path();
                let Some(stem) = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .and_then(|n| n.strip_suffix(".csv"))
                else {
                    continue;
                };
                let Some((schema, table)) = stem.split_once('.') else {
                    return Err(format!(
                        "data file `{}` is not named <schema>.<table>.csv — see `jerrycan docs migrate-supabase`",
                        path.display()
                    ));
                };
                data_files.push((schema.to_string(), table.to_string(), path));
            }
        }
        data_files.sort();
        let sorted_dirs = |dir: PathBuf| -> Vec<PathBuf> {
            let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
                .map(|es| {
                    es.flatten()
                        .map(|e| e.path())
                        .filter(|p| p.is_dir())
                        .collect()
                })
                .unwrap_or_default();
            v.sort();
            v
        };
        Ok(Self {
            root: root.to_path_buf(),
            schema_sql,
            data_files,
            buckets_json: std::fs::read_to_string(root.join("storage/buckets.json")).ok(),
            object_dirs: sorted_dirs(root.join("storage/objects")),
            function_dirs: sorted_dirs(root.join("functions")),
            cron_sql: std::fs::read_to_string(root.join("cron.sql")).ok(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_valid_export_layout_loads_and_lists_data_files() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(
            root.join("schema.sql"),
            "create table public.todos (id uuid primary key);",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("data")).unwrap();
        std::fs::write(root.join("data/public.todos.csv"), "id\n").unwrap();
        let export = Export::open(root).expect("valid layout");
        assert!(export.schema_sql.contains("create table"));
        assert_eq!(
            export.data_files,
            vec![(
                "public".to_string(),
                "todos".to_string(),
                root.join("data/public.todos.csv")
            )]
        );
        assert!(export.cron_sql.is_none() && export.buckets_json.is_none());
    }

    #[test]
    fn a_missing_schema_sql_is_a_pointed_error() {
        let tmp = tempfile::tempdir().unwrap();
        let err = Export::open(tmp.path()).unwrap_err();
        assert!(err.contains("schema.sql"), "names the missing file: {err}");
        assert!(
            err.contains("supabase db dump") || err.contains("pg_dump"),
            "tells the operator how to produce it: {err}"
        );
    }
}
