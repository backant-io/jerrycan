//! Spec §Data seed: a streaming CSV reader (no `csv` crate), the inline/bulk
//! seed writer with a manifest, and the resumable applier core. No table is
//! ever held fully in memory (rows are consumed lazily).

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::iter::Peekable;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SeedType {
    Integer,
    Float,
    Boolean,
    Text,
    Uuid,
    Datetime,
    Json,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeedColumn {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: SeedType,
}

// ---------------------------------------------------------------------------
// Streaming CSV reader (RFC-4180 + Postgres `\N` NULL sentinel).
// ---------------------------------------------------------------------------

pub struct CsvReader<R: Read> {
    bytes: Peekable<std::io::Bytes<BufReader<R>>>,
    headers: Vec<String>,
}

impl<R: Read> CsvReader<R> {
    pub fn new(reader: R) -> Self {
        let mut me = CsvReader {
            bytes: BufReader::new(reader).bytes().peekable(),
            headers: Vec::new(),
        };
        if let Some(Ok(row)) = me.read_record() {
            me.headers = row.into_iter().map(|c| c.unwrap_or_default()).collect();
        }
        me
    }

    pub fn headers(&self) -> &[String] {
        &self.headers
    }

    fn read_record(&mut self) -> Option<Result<Vec<Option<String>>, String>> {
        let mut fields: Vec<Option<String>> = Vec::new();
        let mut field = String::new();
        let mut in_quotes = false;
        let mut had_quote = false;
        let mut any = false;

        let finish_field = |field: String, had_quote: bool| -> Option<String> {
            if !had_quote && field == "\\N" {
                None
            } else {
                Some(field)
            }
        };

        loop {
            let next = self.bytes.next();
            match next {
                None => {
                    if !any {
                        return None;
                    }
                    fields.push(finish_field(field, had_quote));
                    return Some(Ok(fields));
                }
                Some(Err(e)) => return Some(Err(e.to_string())),
                Some(Ok(b)) => {
                    any = true;
                    let c = b as char;
                    if in_quotes {
                        if c == '"' {
                            // Doubled quote → literal; otherwise the field closes.
                            if matches!(self.bytes.peek(), Some(Ok(b'"'))) {
                                self.bytes.next();
                                field.push('"');
                            } else {
                                in_quotes = false;
                            }
                        } else {
                            field.push(c);
                        }
                    } else {
                        match c {
                            '"' => {
                                in_quotes = true;
                                had_quote = true;
                            }
                            ',' => {
                                fields.push(finish_field(std::mem::take(&mut field), had_quote));
                                had_quote = false;
                            }
                            '\n' => {
                                fields.push(finish_field(field, had_quote));
                                return Some(Ok(fields));
                            }
                            '\r' => {}
                            _ => field.push(c),
                        }
                    }
                }
            }
        }
    }
}

impl<R: Read> Iterator for CsvReader<R> {
    type Item = Result<Vec<Option<String>>, String>;
    fn next(&mut self) -> Option<Self::Item> {
        self.read_record()
    }
}

// ---------------------------------------------------------------------------
// SQL literal rendering.
// ---------------------------------------------------------------------------

fn render_value(ty: SeedType, value: &Option<String>) -> Result<String, String> {
    let Some(v) = value else {
        return Ok("NULL".to_string());
    };
    match ty {
        SeedType::Integer => {
            v.trim()
                .parse::<i64>()
                .map_err(|_| format!("`{v}` is not an integer"))?;
            Ok(v.trim().to_string())
        }
        SeedType::Float => {
            v.trim()
                .parse::<f64>()
                .map_err(|_| format!("`{v}` is not a number"))?;
            Ok(v.trim().to_string())
        }
        SeedType::Boolean => match v.trim().to_lowercase().as_str() {
            "t" | "true" | "1" | "yes" | "y" => Ok("TRUE".to_string()),
            "f" | "false" | "0" | "no" | "n" => Ok("FALSE".to_string()),
            _ => Err(format!("`{v}` is not a boolean")),
        },
        SeedType::Text | SeedType::Uuid | SeedType::Datetime | SeedType::Json => {
            Ok(format!("'{}'", v.replace('\'', "''")))
        }
    }
}

fn render_csv_field(value: &Option<String>) -> String {
    match value {
        None => "\\N".to_string(),
        Some(v) => {
            if v.contains([',', '"', '\n', '\r']) {
                format!("\"{}\"", v.replace('"', "\"\""))
            } else {
                v.clone()
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Manifest + seed writer.
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub struct ManifestTable {
    pub table: String,
    pub mode: String, // "inline" | "bulk"
    pub file: String,
    pub rows: usize,
    pub sha256: String,
    #[serde(default)]
    pub columns: Vec<SeedColumn>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ManifestBlob {
    pub bucket: String,
    pub key: String,
    pub file: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub batch_size: usize,
    pub tables: Vec<ManifestTable>,
    #[serde(default)]
    pub blobs: Vec<ManifestBlob>,
}

pub struct SeedWriter {
    root: PathBuf,
    bulk_threshold: usize,
    batch_size: usize,
    seq: usize,
    tables: Vec<ManifestTable>,
    blobs: Vec<ManifestBlob>,
}

impl SeedWriter {
    pub fn new(root: &Path, bulk_threshold: usize, batch_size: usize) -> Self {
        SeedWriter {
            root: root.to_path_buf(),
            bulk_threshold,
            batch_size: batch_size.max(1),
            seq: 0,
            tables: Vec::new(),
            blobs: Vec::new(),
        }
    }

    pub fn add_blob(&mut self, bucket: &str, key: &str, file: &str) {
        self.blobs.push(ManifestBlob {
            bucket: bucket.into(),
            key: key.into(),
            file: file.into(),
        });
    }

    pub fn write_table(
        &mut self,
        table: &str,
        columns: &[SeedColumn],
        rows: impl Iterator<Item = Vec<Option<String>>>,
        row_count: usize,
    ) -> Result<(), String> {
        if row_count <= self.bulk_threshold {
            self.write_inline(table, columns, rows)
        } else {
            self.write_bulk(table, columns, rows, row_count)
        }
    }

    fn write_inline(
        &mut self,
        table: &str,
        columns: &[SeedColumn],
        rows: impl Iterator<Item = Vec<Option<String>>>,
    ) -> Result<(), String> {
        self.seq += 1;
        let rel = format!("seed/inline/{:03}_{table}.sql", self.seq);
        let path = self.root.join(&rel);
        create_parent(&path)?;
        let mut file = BufWriter::new(File::create(&path).map_err(|e| e.to_string())?);
        let mut hasher = Sha256::new();
        let col_list = columns
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let header = format!("INSERT INTO {table} ({col_list}) VALUES ");

        let mut rows_written = 0usize;
        let mut batch: Vec<String> = Vec::new();
        let emit = |file: &mut BufWriter<File>,
                    hasher: &mut Sha256,
                    batch: &[String]|
         -> Result<(), String> {
            if batch.is_empty() {
                return Ok(());
            }
            let stmt = format!("{header}{};\n", batch.join(", "));
            file.write_all(stmt.as_bytes()).map_err(|e| e.to_string())?;
            hasher.update(stmt.as_bytes());
            Ok(())
        };

        for row in rows {
            let mut tuple = Vec::with_capacity(columns.len());
            for (col, value) in columns.iter().zip(row.iter()) {
                tuple.push(render_value(col.ty, value)?);
            }
            batch.push(format!("({})", tuple.join(", ")));
            rows_written += 1;
            if batch.len() >= self.batch_size {
                emit(&mut file, &mut hasher, &batch)?;
                batch.clear();
            }
        }
        emit(&mut file, &mut hasher, &batch)?;
        file.flush().map_err(|e| e.to_string())?;

        self.tables.push(ManifestTable {
            table: table.to_string(),
            mode: "inline".into(),
            file: rel,
            rows: rows_written,
            sha256: hex(&hasher.finalize()),
            columns: columns.to_vec(),
        });
        Ok(())
    }

    fn write_bulk(
        &mut self,
        table: &str,
        columns: &[SeedColumn],
        rows: impl Iterator<Item = Vec<Option<String>>>,
        _row_count: usize,
    ) -> Result<(), String> {
        let rel = format!("seed/bulk/{table}.csv");
        let path = self.root.join(&rel);
        create_parent(&path)?;
        let mut file = BufWriter::new(File::create(&path).map_err(|e| e.to_string())?);
        let mut hasher = Sha256::new();

        let header = columns
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let header_line = format!("{header}\n");
        file.write_all(header_line.as_bytes())
            .map_err(|e| e.to_string())?;
        hasher.update(header_line.as_bytes());

        let mut rows_written = 0usize;
        for row in rows {
            let line = format!(
                "{}\n",
                row.iter().map(render_csv_field).collect::<Vec<_>>().join(",")
            );
            file.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
            hasher.update(line.as_bytes());
            rows_written += 1;
        }
        file.flush().map_err(|e| e.to_string())?;

        self.tables.push(ManifestTable {
            table: table.to_string(),
            mode: "bulk".into(),
            file: rel,
            rows: rows_written,
            sha256: hex(&hasher.finalize()),
            columns: columns.to_vec(),
        });
        Ok(())
    }

    /// Write `seed/manifest.json` and return its bytes (stable order).
    pub fn finish(self) -> Result<String, String> {
        let manifest = Manifest {
            batch_size: self.batch_size,
            tables: self.tables,
            blobs: self.blobs,
        };
        let mut json = serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?;
        json.push('\n');
        let path = self.root.join("seed/manifest.json");
        create_parent(&path)?;
        std::fs::write(&path, &json).map_err(|e| e.to_string())?;
        Ok(json)
    }
}

fn create_parent(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------------------
// Resumable apply plan (pure) + applier.
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SeedState {
    #[serde(default)]
    pub done_files: Vec<String>,
    #[serde(default)]
    pub bulk_progress: BTreeMap<String, usize>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ApplyStep {
    Inline { file: String },
    Bulk { table: String, skip_batches: usize },
}

#[derive(Debug)]
pub struct ApplyPlan {
    pub steps: Vec<ApplyStep>,
    pub batch_size: usize,
}

impl ApplyPlan {
    pub fn from(manifest_json: &str, state_json: Option<&str>) -> Result<ApplyPlan, String> {
        let manifest: Manifest =
            serde_json::from_str(manifest_json).map_err(|e| format!("bad manifest: {e}"))?;
        let state: SeedState = match state_json {
            Some(s) => serde_json::from_str(s).map_err(|e| format!("bad state: {e}"))?,
            None => SeedState::default(),
        };
        let batch_size = manifest.batch_size.max(1);
        let mut steps = Vec::new();
        for t in &manifest.tables {
            match t.mode.as_str() {
                "inline" => {
                    if !state.done_files.contains(&t.file) {
                        steps.push(ApplyStep::Inline {
                            file: t.file.clone(),
                        });
                    }
                }
                "bulk" => {
                    let total = t.rows.div_ceil(batch_size);
                    let applied = state.bulk_progress.get(&t.table).copied().unwrap_or(0);
                    if applied < total {
                        steps.push(ApplyStep::Bulk {
                            table: t.table.clone(),
                            skip_batches: applied,
                        });
                    }
                }
                other => return Err(format!("unknown seed mode `{other}`")),
            }
        }
        Ok(ApplyPlan { steps, batch_size })
    }
}

#[derive(Debug, Serialize)]
pub struct AppliedSummary {
    pub applied_tables: Vec<String>,
    pub resumed: bool,
}

/// Apply the migrated seed against `db`, checkpointing into `seed/.state.json`
/// after every inline file and every bulk batch so an interrupt resumes cleanly.
pub async fn apply(root: &Path, db: &jerrycan_db::Db) -> Result<AppliedSummary, String> {
    use jerrycan_db::sea_orm::ConnectionTrait;

    let manifest_json = std::fs::read_to_string(root.join("seed/manifest.json"))
        .map_err(|e| format!("no seed/manifest.json — run `jerrycan migrate` first: {e}"))?;
    let manifest: Manifest = serde_json::from_str(&manifest_json).map_err(|e| e.to_string())?;
    let state_path = root.join("seed/.state.json");
    let mut state: SeedState = std::fs::read_to_string(&state_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let resumed = !state.done_files.is_empty() || !state.bulk_progress.is_empty();

    let plan = ApplyPlan::from(&manifest_json, Some(&serde_json::to_string(&state).unwrap()))?;
    let by_table: BTreeMap<&str, &ManifestTable> =
        manifest.tables.iter().map(|t| (t.table.as_str(), t)).collect();
    let mut applied_tables = Vec::new();

    for step in &plan.steps {
        match step {
            ApplyStep::Inline { file } => {
                let sql = std::fs::read_to_string(root.join(file)).map_err(|e| e.to_string())?;
                db.conn()
                    .execute_unprepared(&sql)
                    .await
                    .map_err(|e| e.to_string())?;
                state.done_files.push(file.clone());
                write_state(&state_path, &state)?;
                applied_tables.push(file.clone());
            }
            ApplyStep::Bulk {
                table,
                skip_batches,
            } => {
                let meta = by_table.get(table.as_str()).ok_or("bulk table missing")?;
                let csv = File::open(root.join(&meta.file)).map_err(|e| e.to_string())?;
                let reader = CsvReader::new(csv);
                let mut batch: Vec<Vec<Option<String>>> = Vec::new();
                let mut batch_index = 0usize;
                let mut done_batches = *skip_batches;
                for row in reader {
                    batch.push(row?);
                    if batch.len() >= plan.batch_size {
                        if batch_index >= *skip_batches {
                            flush_bulk_batch(db.conn(), table, &meta.columns, &batch).await?;
                            done_batches += 1;
                            state.bulk_progress.insert(table.clone(), done_batches);
                            write_state(&state_path, &state)?;
                        }
                        batch_index += 1;
                        batch.clear();
                    }
                }
                if !batch.is_empty() && batch_index >= *skip_batches {
                    flush_bulk_batch(db.conn(), table, &meta.columns, &batch).await?;
                    done_batches += 1;
                    state.bulk_progress.insert(table.clone(), done_batches);
                    write_state(&state_path, &state)?;
                }
                applied_tables.push(table.clone());
            }
        }
    }
    Ok(AppliedSummary {
        applied_tables,
        resumed,
    })
}

async fn flush_bulk_batch(
    conn: &jerrycan_db::sea_orm::DatabaseConnection,
    table: &str,
    columns: &[SeedColumn],
    batch: &[Vec<Option<String>>],
) -> Result<(), String> {
    use jerrycan_db::sea_orm::ConnectionTrait;
    let col_list = columns
        .iter()
        .map(|c| c.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let mut tuples = Vec::with_capacity(batch.len());
    for row in batch {
        let mut tuple = Vec::with_capacity(columns.len());
        for (col, value) in columns.iter().zip(row.iter()) {
            tuple.push(render_value(col.ty, value)?);
        }
        tuples.push(format!("({})", tuple.join(", ")));
    }
    let sql = format!("INSERT INTO {table} ({col_list}) VALUES {};", tuples.join(", "));
    conn.execute_unprepared(&sql)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn write_state(path: &Path, state: &SeedState) -> Result<(), String> {
    create_parent(path)?;
    let json = serde_json::to_string_pretty(state).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(name: &str, ty: SeedType) -> SeedColumn {
        SeedColumn {
            name: name.into(),
            ty,
        }
    }

    fn rows(data: &[&[&str]]) -> Vec<Vec<Option<String>>> {
        data.iter()
            .map(|r| r.iter().map(|c| Some((*c).to_string())).collect())
            .collect()
    }

    fn manifest_fixture() -> String {
        // todos inline (1 file), events bulk (4 rows, batch 2).
        serde_json::to_string(&Manifest {
            batch_size: 2,
            tables: vec![
                ManifestTable {
                    table: "todos".into(),
                    mode: "inline".into(),
                    file: "seed/inline/001_todos.sql".into(),
                    rows: 2,
                    sha256: "x".into(),
                    columns: vec![],
                },
                ManifestTable {
                    table: "events".into(),
                    mode: "bulk".into(),
                    file: "seed/bulk/events.csv".into(),
                    rows: 4,
                    sha256: "y".into(),
                    columns: vec![],
                },
            ],
            blobs: vec![],
        })
        .unwrap()
    }

    #[test]
    fn the_csv_reader_streams_quotes_embedded_delimiters_and_nulls() {
        let csv = "id,title,note\n1,\"a, \"\"quoted\"\" title\",\\N\n2,plain,ok\n";
        let rows: Vec<Vec<Option<String>>> =
            CsvReader::new(csv.as_bytes()).map(|r| r.unwrap()).collect();
        assert_eq!(
            rows[0],
            vec![Some("1".into()), Some("a, \"quoted\" title".into()), None],
            "\\N is NULL"
        );
        assert_eq!(rows[1][2], Some("ok".into()));
    }

    #[test]
    fn small_tables_write_batched_inserts_large_tables_go_bulk() {
        let tmp = tempfile::tempdir().unwrap();
        let mut writer = SeedWriter::new(tmp.path(), /*bulk_threshold*/ 3, /*batch*/ 2);
        let cols = vec![col("id", SeedType::Integer), col("title", SeedType::Text)];
        // 2 rows ≤ threshold → inline SQL, batched.
        writer
            .write_table(
                "todos",
                &cols,
                rows(&[&["1", "alpha"], &["2", "it's"]]).into_iter(),
                2,
            )
            .unwrap();
        // 4 rows > threshold → verbatim bulk CSV.
        writer
            .write_table(
                "events",
                &cols,
                rows(&[&["1", "a"], &["2", "b"], &["3", "c"], &["4", "d"]]).into_iter(),
                4,
            )
            .unwrap();
        let manifest = writer.finish().unwrap();
        let inline = std::fs::read_to_string(tmp.path().join("seed/inline/001_todos.sql")).unwrap();
        assert!(inline.contains("INSERT INTO todos (id, title) VALUES"));
        assert!(inline.contains("(2, 'it''s')"), "SQL string escaping: {inline}");
        assert!(tmp.path().join("seed/bulk/events.csv").exists());
        let m: serde_json::Value = serde_json::from_str(&manifest).unwrap();
        assert_eq!(m["tables"][1]["mode"], "bulk");
        assert_eq!(m["tables"][1]["rows"], 4);
        assert!(m["tables"][1]["sha256"].as_str().unwrap().len() == 64);
    }

    #[test]
    fn the_applier_checkpoints_and_resumes_at_batch_boundaries() {
        let manifest = manifest_fixture();
        let state = r#"{ "done_files": ["seed/inline/001_todos.sql"], "bulk_progress": { "events": 1 } }"#;
        let plan = ApplyPlan::from(&manifest, Some(state)).unwrap();
        assert_eq!(
            plan.steps,
            vec![ApplyStep::Bulk {
                table: "events".into(),
                skip_batches: 1
            }]
        );
        let done = ApplyPlan::from(
            &manifest,
            Some(r#"{ "done_files": ["seed/inline/001_todos.sql"], "bulk_progress": { "events": 2 } }"#),
        )
        .unwrap();
        assert!(done.steps.is_empty(), "fully applied seed is a no-op");
    }
}
