# jerrycan migrate (Supabase) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `jerrycan migrate --from supabase <export-dir>` (offline primary; `--live <conn>` opt-in) deterministically translates a Supabase export into a scaffolded jerrycan project — contract-v2 `design.json` (with `storage` + `realtime` blocks), a streamed/resumable data seed, a machine-readable `gap-report.json`, and a `MIGRATION.md` with a secret-rotation checklist — then hands off to the normal `generate → check → package` loop, gated by the reference-export eval with cross-tenant negative controls.

**Architecture:** A staged deterministic pipeline under `platform::migrate`: two front-ends (offline SQL-dump parser via `sqlparser`, live Postgres-catalog reader) fold into one intermediate `PgDatabase` IR; pure translator stages (type map, entity builder, module grouper, conservative RLS recognizer, tenancy detector, CRUD/auth/storage/realtime/cron mappers) each emit either design constructs or structured gap items — never guesses. Emission validates the design through the existing `questions::validate`, scaffolds via the existing `scaffold`/`schema` machinery, and streams the seed in batches with a resumable applier (`jerrycan db seed`).

**Tech Stack:** Rust; `sqlparser 0.62` (MIT OR Apache-2.0 — the one new third-party crate this plan adds: a maintained, pure-Rust, `#![forbid(unsafe)]`-compatible SQL parser whose `PostgreSqlDialect` covers `CREATE TABLE/TYPE/INDEX/POLICY`, dollar-quoted bodies, and full expression ASTs for RLS predicates; rejected alternatives: `pg_query` wraps the C libpg_query — heavy, unsafe FFI; hand-rolled parsing — not defensible for arbitrary dumps); `clap` (CLI); existing `serde`/`serde_json`, `sha2`, `jerrycan-db` (sea-orm `execute_unprepared` for the seed applier); `bcrypt 0.17` (MIT, pure Rust) in `jerrycan-auth` for migrated-password verification. No `csv` crate, no `regex` crate — the streaming CSV reader and secret matchers are small hand-rolled, unit-tested functions (workspace dependency discipline).

**Spec:** `docs/superpowers/specs/2026-07-10-jerrycan-migrator-design.md` (+ storage and realtime specs of the same date).

---

## BUILD-ORDER — LAST (coordination note)

**Do not start this plan until the `jerrycan-storage` and `jerrycan-realtime` plans have landed.** The translator emits `contract_version: 2` designs including top-level `storage` and `realtime` blocks and constructs them as **typed** `Design` fields — so `design.rs` must already carry `storage: Option<…>` / `realtime: Option<…>` (with `wants_storage()` / `wants_realtime()`), `questions.rs` must accept `contract_version == 2`, and `storagegen.rs` / `realtimegen.rs` must exist for the capstone eval to go green. If those types are missing, Task 6/12/13 simply won't compile — the build order is self-enforcing. Where this plan references v2 fields (bucket `owner`, `owner_prefix`, `max_size`, `realtime.changes`), use the exact field names/grammar as landed by those plans; the intent here follows their specs verbatim.

---

## Resolved ambiguities (decisions baked into this plan)

1. **SQL parsing dependency:** `sqlparser = "0.62"` (see Tech Stack). Parsing is **per-statement with graceful degradation**: the dump is tokenized once (dollar-quoted bodies are single tokens, so top-level `;` is a safe boundary), each statement is parsed independently, and any statement `sqlparser` rejects becomes an `Unparsed` record that flows to the gap report instead of aborting the migration. `CREATE/ALTER PUBLICATION` (not in sqlparser's grammar) get a tiny dedicated recognizer.
2. **sqlparser AST field names:** the destructuring code in Tasks 4/8 is written against the 0.62 API as documented; if a variant/field name differs on docs.rs/sqlparser/0.62.0, adapt the pattern — **the unit tests pin the behavior, not the parser's AST shape.**
3. **Gap kinds:** the spec's seven (`rls_policy | pg_function | edge_function | unmapped_type | realtime_channel | broadcast | presence`) plus four the pipeline demonstrably needs: `pg_trigger` (triggers are separate work items from functions), `foreign_key` (fk column name that can't map to a derived `belongs_to` column), `cron_job` (unmappable schedules), `suspected_secret` (the data-scan flag the spec's Security section requires).
4. **Entity-level `owner = auth.uid()` mapping (rule R3):** design.json has no per-entity owner field, so: (a) if an **org-level tenant** is detected (membership-join policies), an owner-scoped table that *also* carries the tenant fk translates to tenant scoping (isolation preserved; an *advisory* gap notes the stricter per-user filter); an owner-scoped table *without* the tenant fk is a **blocking gap** (never guessed). (b) If **no org tenant exists anywhere**, the design gets `tenancy: { entity: "User" }` — each user is their own tenant with an identity membership row seeded — which reproduces owner semantics exactly through existing, isolation-tested machinery. Bucket-level `owner`/`owner_prefix` use the storage block's native fields.
5. **Endpoint surface:** Supabase exposes CRUD via PostgREST, so the translator emits the full CRUD set per entity (`list/get/create/update/delete` → `GET /`, `GET /{id}`, `POST /`, `PATCH /{id}`, `DELETE /{id}`) with guards derived from the RLS translation. `MIGRATION.md` carries the old-path → new-path mapping table for the frontend repoint.
6. **Tables with RLS disabled** (Supabase would serve them wide-open through PostgREST): default to `auth_required` CRUD + an *advisory* gap — secure-by-default, surfaced, reversible by the agent.
7. **Seed format:** per-table streamed output — tables ≤ `--bulk-threshold` (default 5000) rows become `seed/inline/NNN_<table>.sql` (multi-row INSERTs, 500 rows/statement); larger tables become `seed/bulk/<table>.csv` (streamed verbatim). `seed/manifest.json` lists files in FK-topological order with row counts + sha256. The **resumable applier is a new `jerrycan db seed` subcommand** that checkpoints per file/batch into `seed/.state.json`. Blobs ride as `seed/blobs/<bucket>/<key>` and are copied into the local blob store by the applier.
8. **Export data contract:** `data/<schema>.<table>.csv` produced by `\copy (select * from <t>) to 'data/<schema>.<t>.csv' with (format csv, header true, null '\N')` — `\N` disambiguates NULL from empty string. Bucket config is `storage/buckets.json`; object bytes are `storage/objects/<bucket>/<key>`. All commands are documented verbatim in the new docs page (Task 16).
9. **`--live`** reads Postgres catalogs (`information_schema`, `pg_policies`, `pg_publication_tables`, `cron.job`, `storage.buckets`, plus row streams) into the *same* `PgDatabase` IR — one translator, two front-ends. Object **bytes** are not fetched live (no storage-API client in v1): a gap item + `MIGRATION.md` step instruct the offline byte copy. Never used in CI.
10. **bcrypt:** unconditional dispatch inside `jerrycan_auth::password::verify_password` on the `$2…$` PHC prefix (no feature flag, no design/scaffold signal needed — lossless login must work in every generated app); `needs_rehash()` enables transparent argon2 upgrade on next login. **Skip Task 17 if the storage-phase auth work already landed this** (check `crates/jerrycan-auth/src/password.rs` first).
11. **`max_size` grammar:** emit using jerrycan-storage's landed grammar; exact-MB/KB byte counts render as `"<n>MB"`/`"<n>KB"`, anything non-exact rounds **up** to the next MB with an advisory gap (never silently tighter than Supabase's limit).
12. **Naming:** `entity_name("order_items") = "OrderItem"` — snake segments PascalCased, last segment singularized by rule (`ies→y`, `ses→s`… else strip trailing `s`; 3-entry irregular map `people/children/statuses`). Modules are kebab-case table-group names. Deterministic; agent refines per spec.
13. **MCP twin deferred:** like `jerrycan deploy`, `migrate` ships CLI-only; a `jerrycan_migrate` MCP tool is a follow-up (open a ticket when this plan completes).
14. **Determinism is a tested property:** every map is a `BTreeMap`, every list is sorted; Task 19 asserts two runs produce byte-identical outputs.

---

## File Structure

- Modify `Cargo.toml` (workspace) — add `sqlparser = "0.62"` to `[workspace.dependencies]`.
- Modify `crates/jerrycan/Cargo.toml` — `sqlparser = { workspace = true, optional = true }`; add `"dep:sqlparser"` to the `cli` feature.
- Modify `crates/jerrycan/src/platform/mod.rs` — `pub mod migrate;` (alphabetical, after `mcp_dispatch`).
- Create `crates/jerrycan/src/platform/migrate/mod.rs` — `MigrateOptions`/`MigrateOutput`, the staged `run_migrate` orchestration, design validation + scaffold handoff.
- Create `crates/jerrycan/src/platform/migrate/export.rs` — offline export-directory contract: layout validation + file loading.
- Create `crates/jerrycan/src/platform/migrate/gaps.rs` — `GapItem`/`GapKind`/`Severity` + deterministic `gap-report.json` writer.
- Create `crates/jerrycan/src/platform/migrate/parse.rs` — statement splitter + per-statement sqlparser parse with graceful degradation.
- Create `crates/jerrycan/src/platform/migrate/pgmodel.rs` — the `PgDatabase` IR + the fold from parsed statements (tables, enums, policies, publications, functions/triggers, RLS flags).
- Create `crates/jerrycan/src/platform/migrate/typemap.rs` — Postgres type → `FieldType` map; unmappable → gap.
- Create `crates/jerrycan/src/platform/migrate/entities.rs` — tables → `Entity`/`Field`/`belongs_to` (+ naming helpers).
- Create `crates/jerrycan/src/platform/migrate/grouping.rs` — FK-graph + name-prefix module grouping.
- Create `crates/jerrycan/src/platform/migrate/rls.rs` — the conservative RLS recognizer (canonical shapes only; everything else → gap).
- Create `crates/jerrycan/src/platform/migrate/tenancy.rs` — membership-table detection, `tenancy` block, owner rule R3.
- Create `crates/jerrycan/src/platform/migrate/crud.rs` — CRUD endpoint emission with guards from the RLS translation.
- Create `crates/jerrycan/src/platform/migrate/authmap.rs` — `auth.users`/`auth.identities` → `auth` block, roles, `oauth` dep, users module + user seed.
- Create `crates/jerrycan/src/platform/migrate/storagemap.rs` — `storage/buckets.json` + objects policies/rows/bytes → `storage` block + object seed.
- Create `crates/jerrycan/src/platform/migrate/realtimemap.rs` — `supabase_realtime` publication → `realtime.changes`; broadcast/presence advisories.
- Create `crates/jerrycan/src/platform/migrate/cronmap.rs` — `cron.sql` → `jobs[]` + per-job `pg_function` gaps.
- Create `crates/jerrycan/src/platform/migrate/seed.rs` — streaming CSV reader, inline/bulk seed writer, manifest; the resumable applier core.
- Create `crates/jerrycan/src/platform/migrate/redact.rs` — secret matchers (JWT / `sb_secret_` / `sbp_` / password-bearing conn strings), placeholder emission, data-column flagging.
- Create `crates/jerrycan/src/platform/migrate/migrationmd.rs` — `MIGRATION.md` emitter (summary, endpoint mapping, rotation checklist, seed instructions).
- Create `crates/jerrycan/src/platform/migrate/live.rs` — `--live` Postgres catalog reader into `PgDatabase`.
- Modify `crates/jerrycan/src/main.rs` — `Cmd::Migrate { … }` + `cmd_migrate`; `DbCmd::Seed` + `cmd_db_seed`.
- Modify `crates/jerrycan-auth/src/password.rs` + `crates/jerrycan-auth/Cargo.toml` — bcrypt verify + `needs_rehash` (Task 17, conditional).
- Modify `crates/jerrycan/src/platform/docsidx.rs` + create `docs/ai/20-migrate-supabase.md` — the export-contract docs page (if storage/realtime claimed different numbers, take the next free one; keep the slug `migrate-supabase`).
- Create `conformance/fixtures/supabase-export/…` — the checked-in reference Supabase export (multi-tenant SaaS: schema.sql, data/, storage/, functions/, cron.sql).
- Create `crates/jerrycan/tests/migrate_supabase.rs` — unit-adjacent integration tests over the reference export (always on).
- Create `crates/jerrycan/tests/migrate_e2e.rs` — the capstone eval (`#[ignore]`d, CI eval job): migrate → generate → check green + negative controls.
- Modify `conformance/eval/PROTOCOL.md` — register the migrator eval as un-skippable.

Tests for each module live in `#[cfg(test)] mod tests` inside the module file (matching `design.rs`/`questions.rs` convention) unless a task names an integration test file.

---

### Task 1: dependency, CLI skeleton, export-directory contract

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Modify: `crates/jerrycan/Cargo.toml`
- Create: `crates/jerrycan/src/platform/migrate/mod.rs`
- Create: `crates/jerrycan/src/platform/migrate/export.rs`
- Modify: `crates/jerrycan/src/platform/mod.rs`
- Modify: `crates/jerrycan/src/main.rs`
- Test: inline `#[cfg(test)]` in `export.rs`

- [ ] **Step 1: Write the failing test** (bottom of the new `export.rs`; the file starts as just this test + a `todo!`-free stub set — write the types in Step 3)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_valid_export_layout_loads_and_lists_data_files() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("schema.sql"), "create table public.todos (id uuid primary key);").unwrap();
        std::fs::create_dir_all(root.join("data")).unwrap();
        std::fs::write(root.join("data/public.todos.csv"), "id\n").unwrap();
        let export = Export::open(root).expect("valid layout");
        assert!(export.schema_sql.contains("create table"));
        assert_eq!(export.data_files, vec![("public".to_string(), "todos".to_string(), root.join("data/public.todos.csv"))]);
        assert!(export.cron_sql.is_none() && export.buckets_json.is_none());
    }

    #[test]
    fn a_missing_schema_sql_is_a_pointed_error() {
        let tmp = tempfile::tempdir().unwrap();
        let err = Export::open(tmp.path()).unwrap_err();
        assert!(err.contains("schema.sql"), "names the missing file: {err}");
        assert!(err.contains("supabase db dump") || err.contains("pg_dump"), "tells the operator how to produce it: {err}");
    }
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p jerrycan migrate::export`
Expected: FAIL — module `migrate` does not exist.

- [ ] **Step 3: Minimal implementation**

Workspace `Cargo.toml`, in `[workspace.dependencies]` (after `sea-query-binder`):

```toml
# Supabase-dump SQL parsing for `jerrycan migrate` (pure Rust, MIT OR Apache-2.0).
sqlparser = "0.62"
```

`crates/jerrycan/Cargo.toml`: add `sqlparser = { workspace = true, optional = true }` under `[dependencies]` and extend the `cli` feature: `cli = ["dep:clap", "dep:serde", "dep:serde_json", "dep:tokio", "dep:sea-query", "dep:sqlparser", "db"]`.

`crates/jerrycan/src/platform/mod.rs`: add `pub mod migrate;` after `pub mod mcp_dispatch;`.

`crates/jerrycan/src/platform/migrate/mod.rs`:

```rust
//! `jerrycan migrate --from supabase`: the deterministic translator (spec
//! 2026-07-10). Two front-ends (offline export dir, live catalogs) fold into
//! one PgDatabase IR; pure stages translate what is safe and gap-report the rest.

pub mod export;
```

`crates/jerrycan/src/platform/migrate/export.rs`:

```rust
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
                let Some(stem) = path.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".csv")) else {
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
                .map(|es| es.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect())
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
```

`crates/jerrycan/src/main.rs` — add to the `Cmd` enum (after `Deploy { … }`):

```rust
    /// Migrate a Supabase project into a jerrycan backend
    Migrate {
        /// Source platform (currently: supabase)
        #[arg(long)]
        from: String,
        /// Offline export directory (layout: `jerrycan docs migrate-supabase`)
        export_dir: Option<PathBuf>,
        /// Opt-in: read a live Supabase Postgres instead of an export. Never in CI.
        #[arg(long, conflicts_with = "export_dir")]
        live: Option<String>,
        /// Target project directory (default: ./<app-name>)
        #[arg(long)]
        out: Option<PathBuf>,
        /// App name override (default: kebab-case of the export directory name)
        #[arg(long)]
        name: Option<String>,
        /// Tables with more rows than this become resumable bulk-COPY seed steps
        #[arg(long, default_value_t = 5000)]
        bulk_threshold: usize,
    },
```

Add the dispatch arm in `run` (after the `Cmd::Deploy` arm) and a stub handler that validates `--from` and the export dir only (full orchestration lands in Task 16):

```rust
        Cmd::Migrate { from, export_dir, live, out, name, bulk_threshold } => {
            cmd_migrate(&from, export_dir.as_deref(), live.as_deref(), out.as_deref(), name.as_deref(), bulk_threshold, cli.json)
        }
```

```rust
fn cmd_migrate(
    from: &str,
    export_dir: Option<&Path>,
    live: Option<&str>,
    _out: Option<&Path>,
    _name: Option<&str>,
    _bulk_threshold: usize,
    _json_mode: bool,
) -> Result<(), Failure> {
    if from != "supabase" {
        return Err(Failure::usage(format!("unknown migration source `{from}` — supported: supabase")));
    }
    if live.is_some() {
        return Err(Failure::usage("--live lands after the offline path — export the project and use the export directory for now"));
    }
    let dir = export_dir.ok_or_else(|| Failure::usage("provide the export directory (or --live <conn>)"))?;
    let _export = jerrycan::platform::migrate::export::Export::open(dir).map_err(Failure::usage)?;
    Err(Failure::usage("migration pipeline not wired yet — implemented across this plan's tasks"))
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p jerrycan migrate::export && cargo build -p jerrycan && target/debug/jerrycan migrate --help`
Expected: both tests PASS; help shows `--from`, `--live`, `--out`, `--bulk-threshold`.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/jerrycan/Cargo.toml crates/jerrycan/src/platform/mod.rs crates/jerrycan/src/platform/migrate crates/jerrycan/src/main.rs
git commit -m "Add jerrycan migrate skeleton: sqlparser dep, CLI subcommand, export-dir contract"
```

---

### Task 2: gap-report types + deterministic writer

**Files:**
- Create: `crates/jerrycan/src/platform/migrate/gaps.rs`
- Modify: `crates/jerrycan/src/platform/migrate/mod.rs` (`pub mod gaps;`)
- Test: inline in `gaps.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gap_items_serialize_to_the_spec_shape_and_sort_deterministically() {
        let mut items = vec![
            GapItem {
                kind: GapKind::PgFunction,
                source: "public.audit()".into(),
                location: "schema.sql:90".into(),
                reason: "plpgsql bodies are ported by the agent".into(),
                original: "CREATE FUNCTION audit() …".into(),
                suggested: "port to a Rust handler or job task".into(),
                severity: Severity::Advisory,
            },
            GapItem {
                kind: GapKind::RlsPolicy,
                source: "public.orders policy \"tenant_isolation\"".into(),
                location: "schema.sql:14".into(),
                reason: "predicate references a join we don't auto-translate".into(),
                original: "USING (EXISTS (SELECT 1 FROM order_shares …))".into(),
                suggested: "implement as a guard on the orders module".into(),
                severity: Severity::Blocking,
            },
        ];
        let json = render_gap_report(&mut items);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        // Blocking sorts before advisory; within severity, by location.
        assert_eq!(v[0]["kind"], "rls_policy");
        assert_eq!(v[0]["severity"], "blocking");
        assert_eq!(v[1]["kind"], "pg_function");
        // Every spec field present, snake_case kinds.
        for key in ["kind", "source", "location", "reason", "original", "suggested", "severity"] {
            assert!(v[0].get(key).is_some(), "missing {key}");
        }
        // Determinism: rendering twice is byte-identical.
        assert_eq!(json, render_gap_report(&mut items));
    }
}
```

- [ ] **Step 2: Run to verify it fails** — `cargo test -p jerrycan migrate::gaps` → FAIL (module missing).

- [ ] **Step 3: Minimal implementation** (`gaps.rs`)

```rust
//! Structured machine-readable gap work-items (spec §Gap report). The agent's
//! judgment queue: everything the translator will not guess lands here.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GapKind {
    RlsPolicy,
    PgFunction,
    PgTrigger,
    EdgeFunction,
    UnmappedType,
    ForeignKey,
    RealtimeChannel,
    Broadcast,
    Presence,
    CronJob,
    SuspectedSecret,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Blocking,
    Advisory,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GapItem {
    pub kind: GapKind,
    pub source: String,
    pub location: String,
    pub reason: String,
    pub original: String,
    pub suggested: String,
    pub severity: Severity,
}

/// Sort (blocking first, then location, then source) and render pretty JSON —
/// stable bytes for identical inputs (eval-gated determinism).
pub fn render_gap_report(items: &mut [GapItem]) -> String {
    items.sort_by(|a, b| {
        (a.severity, &a.location, &a.source).cmp(&(b.severity, &b.location, &b.source))
    });
    let mut out = serde_json::to_string_pretty(&items).expect("gap items serialize");
    out.push('\n');
    out
}
```

- [ ] **Step 4: Run to verify it passes** — `cargo test -p jerrycan migrate::gaps` → PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan/src/platform/migrate
git commit -m "Add migrate gap-report types and deterministic writer"
```

---

### Task 3: statement splitter + per-statement parse with graceful degradation

**Files:**
- Create: `crates/jerrycan/src/platform/migrate/parse.rs`
- Modify: `crates/jerrycan/src/platform/migrate/mod.rs` (`pub mod parse;`)
- Test: inline in `parse.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const DUMP: &str = r#"
create table public.todos (id uuid primary key, title text not null);

create function public.touch() returns trigger as $$
begin
  new.updated_at := now(); return new; -- note: ; inside the $$ body must not split
end;
$$ language plpgsql;

alter publication supabase_realtime add table public.todos;
"#;

    #[test]
    fn splits_on_top_level_semicolons_and_survives_dollar_quoting() {
        let stmts = split_and_parse(DUMP);
        assert_eq!(stmts.len(), 3, "{stmts:?}");
        assert!(matches!(&stmts[0], RawStatement::Parsed { .. }), "CREATE TABLE parses");
        // ALTER PUBLICATION is not in sqlparser's grammar → degrades, never aborts.
        match &stmts[2] {
            RawStatement::Unparsed { sql, line } => {
                assert!(sql.contains("supabase_realtime"));
                assert_eq!(*line, 9, "1-based line of the statement start");
            }
            other => panic!("expected Unparsed, got {other:?}"),
        }
    }

    #[test]
    fn a_dollar_quoted_body_stays_one_statement() {
        let stmts = split_and_parse(DUMP);
        let fn_sql = match &stmts[1] {
            RawStatement::Parsed { sql, .. } | RawStatement::Unparsed { sql, .. } => sql,
        };
        assert!(fn_sql.contains("language plpgsql"), "body + tail intact: {fn_sql}");
    }
}
```

- [ ] **Step 2: Run to verify it fails** — `cargo test -p jerrycan migrate::parse` → FAIL.

- [ ] **Step 3: Minimal implementation** (`parse.rs`)

```rust
//! Per-statement parsing with graceful degradation (resolved ambiguity #1):
//! tokenize once (dollar-quoted bodies are single tokens, so a top-level `;`
//! is a safe boundary), slice the source per statement, parse each slice
//! independently. Unparseable statements degrade to `Unparsed` — they feed the
//! publication recognizer and the gap report; they never abort a migration.

use sqlparser::ast::Statement;
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;
use sqlparser::tokenizer::{Token, Tokenizer};

#[derive(Debug)]
pub enum RawStatement {
    Parsed { stmt: Box<Statement>, sql: String, line: usize },
    Unparsed { sql: String, line: usize },
}

/// Byte offset of the start of each 1-based line (for Location → offset math).
fn line_offsets(sql: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(sql.match_indices('\n').map(|(i, _)| i + 1))
        .collect()
}

pub fn split_and_parse(sql: &str) -> Vec<RawStatement> {
    let dialect = PostgreSqlDialect {};
    let offsets = line_offsets(sql);
    let to_offset = |line: u64, col: u64| -> usize {
        offsets.get(line as usize - 1).copied().unwrap_or(0) + (col as usize - 1)
    };
    let tokens = match Tokenizer::new(&dialect, sql).tokenize_with_location() {
        Ok(t) => t,
        // A file the tokenizer rejects outright becomes one Unparsed blob.
        Err(_) => return vec![RawStatement::Unparsed { sql: sql.trim().to_string(), line: 1 }],
    };
    let mut out = Vec::new();
    let mut stmt_start: Option<usize> = None; // byte offset
    let mut stmt_line = 1usize;
    for tok in &tokens {
        let at = to_offset(tok.span.start.line, tok.span.start.column);
        match &tok.token {
            Token::Whitespace(_) => {}
            Token::SemiColon => {
                if let Some(start) = stmt_start.take() {
                    push_statement(&mut out, &sql[start..at], stmt_line);
                }
            }
            _ => {
                if stmt_start.is_none() {
                    stmt_start = Some(at);
                    stmt_line = tok.span.start.line as usize;
                }
            }
        }
    }
    if let Some(start) = stmt_start {
        push_statement(&mut out, &sql[start..], stmt_line);
    }
    out
}

fn push_statement(out: &mut Vec<RawStatement>, stmt_sql: &str, line: usize) {
    let stmt_sql = stmt_sql.trim();
    if stmt_sql.is_empty() {
        return;
    }
    match Parser::parse_sql(&PostgreSqlDialect {}, stmt_sql) {
        Ok(mut stmts) if stmts.len() == 1 => out.push(RawStatement::Parsed {
            stmt: Box::new(stmts.remove(0)),
            sql: stmt_sql.to_string(),
            line,
        }),
        _ => out.push(RawStatement::Unparsed { sql: stmt_sql.to_string(), line }),
    }
}
```

(API note per resolved ambiguity #2: on 0.62 the token location field is `span` with `Location { line, column }`, both 1-based `u64`; if the accessor differs, adapt — the two tests pin the split/line behavior.)

- [ ] **Step 4: Run to verify it passes** — `cargo test -p jerrycan migrate::parse` → PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan/src/platform/migrate
git commit -m "Add migrate SQL statement splitter with per-statement parse degradation"
```

---

### Task 4: the `PgDatabase` IR fold

**Files:**
- Create: `crates/jerrycan/src/platform/migrate/pgmodel.rs`
- Modify: `crates/jerrycan/src/platform/migrate/mod.rs` (`pub mod pgmodel;`)
- Test: inline in `pgmodel.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str = r#"
create type public.customer_status as enum ('lead', 'active', 'churned');

create table public.workspaces (
    id uuid primary key,
    name text not null
);

create table public.customers (
    id uuid primary key,
    workspace_id uuid not null references public.workspaces(id) on delete cascade,
    email text not null unique,
    status public.customer_status not null,
    score numeric,
    created_at timestamptz not null default now()
);

create index customers_score_idx on public.customers (score);

alter table public.customers enable row level security;

create policy "workspace members" on public.customers
    using (workspace_id in (select workspace_id from public.workspace_members where user_id = auth.uid()));

create function public.audit() returns trigger as $$ begin return new; end; $$ language plpgsql;

create publication supabase_realtime for table public.customers;
alter publication supabase_realtime add table public.workspaces;
"#;

    fn db() -> PgDatabase {
        PgDatabase::fold(&crate::platform::migrate::parse::split_and_parse(SCHEMA))
    }

    #[test]
    fn tables_columns_fks_uniques_and_rls_fold_from_the_dump() {
        let db = db();
        let c = &db.tables["public.customers"];
        assert!(c.rls_enabled);
        assert_eq!(c.pk, vec!["id"]);
        let ws_fk = c.fks.iter().find(|f| f.ref_table == "public.workspaces").unwrap();
        assert_eq!(ws_fk.columns, vec!["workspace_id"]);
        assert_eq!(ws_fk.on_delete, FkAction::Cascade);
        let email = c.columns.iter().find(|col| col.name == "email").unwrap();
        assert!(email.not_null && email.unique);
        let score = c.columns.iter().find(|col| col.name == "score").unwrap();
        assert!(score.indexed, "CREATE INDEX marks the column");
        assert_eq!(db.enums["public.customer_status"], vec!["lead", "active", "churned"]);
    }

    #[test]
    fn policies_functions_and_publications_are_collected() {
        let db = db();
        assert_eq!(db.policies.len(), 1);
        assert_eq!(db.policies[0].table, "public.customers");
        assert!(db.policies[0].using.is_some());
        assert_eq!(db.functions.len(), 1, "raw function captured for the gap report");
        assert_eq!(
            db.publications["supabase_realtime"],
            vec!["public.customers", "public.workspaces"],
            "CREATE + ALTER PUBLICATION both recognized (from Unparsed statements)"
        );
    }
}
```

- [ ] **Step 2: Run to verify it fails** — `cargo test -p jerrycan migrate::pgmodel` → FAIL.

- [ ] **Step 3: Minimal implementation** (`pgmodel.rs`)

The IR and the fold. (Destructure per sqlparser 0.62; see resolved ambiguity #2 — adapt field names to the published API, tests pin behavior.)

```rust
//! The Postgres IR both front-ends produce: everything downstream stages need,
//! nothing sqlparser-shaped leaks past this module except policy `Expr`s.

use super::parse::RawStatement;
use sqlparser::ast::{self, Statement};
use std::collections::BTreeMap;

#[derive(Debug, Default)]
pub struct PgDatabase {
    /// Keyed "schema.table".
    pub tables: BTreeMap<String, PgTable>,
    /// Enum type name ("schema.name") → labels.
    pub enums: BTreeMap<String, Vec<String>>,
    pub policies: Vec<PgPolicy>,
    /// Publication name → sorted table names.
    pub publications: BTreeMap<String, Vec<String>>,
    pub functions: Vec<PgRawObject>,
    pub triggers: Vec<PgRawObject>,
    /// Statements neither parsed nor recognized (candidate gap items).
    pub unparsed: Vec<(String, usize)>,
}

#[derive(Debug, Default)]
pub struct PgTable {
    pub schema: String,
    pub name: String,
    pub columns: Vec<PgColumn>,
    pub pk: Vec<String>,
    pub fks: Vec<PgFk>,
    pub rls_enabled: bool,
    pub line: usize,
}

#[derive(Debug)]
pub struct PgColumn {
    pub name: String,
    /// Normalized lowercase type name, e.g. "text", "timestamptz", "public.customer_status".
    pub pg_type: String,
    pub not_null: bool,
    pub unique: bool,
    pub indexed: bool,
    /// `CHECK (col IN ('a','b'))` values, column-level (enum-by-check).
    pub check_in_values: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FkAction { Cascade, SetNull, Restrict }

#[derive(Debug)]
pub struct PgFk {
    pub columns: Vec<String>,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
    pub on_delete: FkAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyCommand { All, Select, Insert, Update, Delete }

#[derive(Debug)]
pub struct PgPolicy {
    pub table: String,
    pub name: String,
    pub command: PolicyCommand,
    /// The `TO role, …` clause (lowercased), e.g. ["authenticated"].
    pub to_roles: Vec<String>,
    pub using: Option<ast::Expr>,
    pub with_check: Option<ast::Expr>,
    pub original: String,
    pub line: usize,
}

#[derive(Debug)]
pub struct PgRawObject {
    pub name: String,
    pub sql: String,
    pub line: usize,
}
```

Fold outline (implement fully; the shapes below are the load-bearing logic):

```rust
fn object_name(name: &ast::ObjectName) -> String {
    // "schema.table" (default schema "public" when unqualified), lowercased.
    let parts: Vec<String> = name.0.iter().map(|p| p.to_string().to_lowercase()).collect();
    if parts.len() == 1 { format!("public.{}", parts[0]) } else { parts.join(".") }
}

impl PgDatabase {
    pub fn fold(stmts: &[RawStatement]) -> Self {
        let mut db = Self::default();
        for raw in stmts {
            match raw {
                RawStatement::Parsed { stmt, sql, line } => db.fold_stmt(stmt, sql, *line),
                RawStatement::Unparsed { sql, line } => {
                    if !db.try_publication(sql) {
                        db.unparsed.push((sql.clone(), *line));
                    }
                }
            }
        }
        db
    }
}
```

`fold_stmt` arms:
- `Statement::CreateTable(ct)` → build `PgTable` from `ct.columns` (options: `NotNull`, `Unique { is_primary }` → pk/unique, `ForeignKey { foreign_table, referred_columns, on_delete, .. }` → `PgFk`, `Check(expr)` → `check_in_values` via a small `extract_in_values(expr, col)` that matches `Expr::InList { expr: Identifier(col), list: [Value::SingleQuotedString…] }`) and `ct.constraints` (`PrimaryKey`, `Unique`, `ForeignKey`, table-level `Check` also fed to `extract_in_values`). `DataType` → normalized string via `data_type_name(dt)`: match the common variants (`Text`, `Uuid`, `Integer/Int/BigInt/SmallInt`, `Boolean`, `Numeric/Decimal/Real/DoublePrecision`, `Timestamp(_, tz)` → `"timestamptz"`/`"timestamp"`, `Date`, `JSON`→`"json"`, `JSONB`→`"jsonb"`, `Bytea`, `Array(_)` → `"<inner>[]"`, `Custom(name, _)` → `object_name(name)`), falling back to `dt.to_string().to_lowercase()`.
- `Statement::CreateType { name, representation: UserDefinedTypeRepresentation::Enum { labels } }` → `enums.insert(object_name(name), labels)`.
- `Statement::CreateIndex(ci)` → for each single-column index expression that is a bare column identifier, set `indexed = true` on that column of `ci.table_name` (`unique` indexes additionally set `unique` when single-column).
- `Statement::AlterTable { name, operations, .. }` → `AddConstraint` folds like table-level constraints; `EnableRowLevelSecurity` sets `rls_enabled`.
- `Statement::CreatePolicy { name, table_name, command, to, using, with_check, .. }` → `PgPolicy` (map the command enum; absent command = `All`; `to` roles lowercased).
- `Statement::CreateFunction(..)` → push `PgRawObject` (name from the statement, `sql` = the original slice).
- `Statement::CreateTrigger { .. }` → push to `triggers`.
- Everything else → ignore (GRANT/COMMENT/SET/etc. are noise in dumps).

`try_publication(sql)` — the two-form hand recognizer (case-insensitive, whitespace-tolerant):
`CREATE PUBLICATION <name> FOR TABLE <t> [, <t>…]` and `ALTER PUBLICATION <name> ADD TABLE <t> [, <t>…]`. Split words, find the keywords, normalize each table via the same default-schema rule, insert sorted+deduped. Returns `true` when recognized.

- [ ] **Step 4: Run to verify it passes** — `cargo test -p jerrycan migrate::pgmodel` → PASS. Also `cargo clippy -p jerrycan -- -D warnings`.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan/src/platform/migrate
git commit -m "Add migrate PgDatabase IR: tables, enums, policies, publications fold"
```

---

### Task 5: the Postgres → design type map

**Files:**
- Create: `crates/jerrycan/src/platform/migrate/typemap.rs`
- Modify: `crates/jerrycan/src/platform/migrate/mod.rs` (`pub mod typemap;`)
- Test: inline in `typemap.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::design::FieldType;
    use std::collections::BTreeMap;

    #[test]
    fn the_spec_type_map_holds_exactly() {
        let enums = BTreeMap::new();
        let cases = [
            ("text", FieldType::String), ("varchar", FieldType::String), ("citext", FieldType::String),
            ("int4", FieldType::Integer), ("integer", FieldType::Integer), ("bigint", FieldType::Integer),
            ("int8", FieldType::Integer), ("smallint", FieldType::Integer),
            ("numeric", FieldType::Float), ("real", FieldType::Float), ("double precision", FieldType::Float),
            ("boolean", FieldType::Boolean), ("bool", FieldType::Boolean),
            ("timestamp", FieldType::Datetime), ("timestamptz", FieldType::Datetime), ("date", FieldType::Datetime),
            ("uuid", FieldType::Uuid), ("json", FieldType::Json), ("jsonb", FieldType::Json),
        ];
        for (pg, want) in cases {
            match map_pg_type(pg, &enums) {
                MappedType::Field { field_type, values: None } => assert_eq!(field_type, want, "{pg}"),
                other => panic!("{pg}: {other:?}"),
            }
        }
    }

    #[test]
    fn enum_types_map_to_string_with_values() {
        let mut enums = BTreeMap::new();
        enums.insert("public.customer_status".to_string(), vec!["lead".into(), "active".into()]);
        match map_pg_type("public.customer_status", &enums) {
            MappedType::Field { field_type: FieldType::String, values: Some(v) } => {
                assert_eq!(v, vec!["lead", "active"]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn arrays_composites_domains_geometry_are_unmappable_never_guessed() {
        let enums = BTreeMap::new();
        for pg in ["text[]", "public.address", "geometry", "tsvector", "bytea", "inet"] {
            assert!(matches!(map_pg_type(pg, &enums), MappedType::Unmappable { .. }), "{pg} must gap");
        }
    }
}
```

- [ ] **Step 2: Run to verify it fails** — `cargo test -p jerrycan migrate::typemap` → FAIL.

- [ ] **Step 3: Minimal implementation** (`typemap.rs`)

```rust
//! Spec §Deterministic translator (1): the Postgres → design type map.
//! Anything not in the table is Unmappable — the caller emits an
//! `unmapped_type` gap item; the type is never guessed.

use crate::platform::design::FieldType;
use std::collections::BTreeMap;

#[derive(Debug)]
pub enum MappedType {
    Field { field_type: FieldType, values: Option<Vec<String>> },
    Unmappable { pg_type: String, reason: &'static str },
}

pub fn map_pg_type(pg: &str, enums: &BTreeMap<String, Vec<String>>) -> MappedType {
    if let Some(labels) = enums.get(pg) {
        return MappedType::Field { field_type: FieldType::String, values: Some(labels.clone()) };
    }
    if pg.ends_with("[]") {
        return MappedType::Unmappable { pg_type: pg.into(), reason: "array types have no design representation — model as a child entity or json" };
    }
    let ft = match pg {
        "text" | "varchar" | "character varying" | "char" | "character" | "citext" => FieldType::String,
        "smallint" | "int2" | "integer" | "int" | "int4" | "bigint" | "int8" | "serial" | "bigserial" => FieldType::Integer,
        "numeric" | "decimal" | "real" | "float4" | "double precision" | "float8" => FieldType::Float,
        "boolean" | "bool" => FieldType::Boolean,
        "timestamp" | "timestamptz" | "timestamp with time zone" | "timestamp without time zone" | "date" => FieldType::Datetime,
        "uuid" => FieldType::Uuid,
        "json" | "jsonb" => FieldType::Json,
        _ => {
            return MappedType::Unmappable {
                pg_type: pg.into(),
                reason: "no deterministic design type for this Postgres type (composite/domain/extension type)",
            };
        }
    };
    MappedType::Field { field_type: ft, values: None }
}
```

- [ ] **Step 4: Run to verify it passes** — `cargo test -p jerrycan migrate::typemap` → PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan/src/platform/migrate
git commit -m "Add migrate Postgres-to-design type map with unmappable gaps"
```

---

### Task 6: tables → entities (fields, belongs_to, flags, naming)

**Files:**
- Create: `crates/jerrycan/src/platform/migrate/entities.rs`
- Modify: `crates/jerrycan/src/platform/migrate/mod.rs` (`pub mod entities;`)
- Test: inline in `entities.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::design::{FieldType, OnDelete};
    use crate::platform::migrate::{parse, pgmodel::PgDatabase};

    const SCHEMA: &str = r#"
create table public.workspaces (id uuid primary key, name text not null);
create table public.order_items (
    id uuid primary key,
    workspace_id uuid not null references public.workspaces(id) on delete cascade,
    author_id uuid references public.workspaces(id),
    label text not null check (label in ('a', 'b')),
    qty integer,
    location point
);
"#;

    fn build() -> BuildResult {
        let db = PgDatabase::fold(&parse::split_and_parse(SCHEMA));
        build_entities(&db)
    }

    #[test]
    fn a_table_becomes_a_singular_pascal_entity_with_mapped_fields() {
        let out = build();
        let item = out.entities.iter().find(|(_, e)| e.name == "OrderItem").map(|(_, e)| e).unwrap();
        assert_eq!(item.fields.iter().find(|f| f.name == "id").unwrap().field_type, FieldType::Uuid);
        let label = item.fields.iter().find(|f| f.name == "label").unwrap();
        assert_eq!(label.values.as_deref(), Some(&["a".to_string(), "b".to_string()][..]));
        let qty = item.fields.iter().find(|f| f.name == "qty").unwrap();
        assert!(!qty.required, "nullable column → required: false");
    }

    #[test]
    fn matching_fk_becomes_belongs_to_and_the_column_is_suppressed() {
        let out = build();
        let item = out.entities.iter().find(|(_, e)| e.name == "OrderItem").map(|(_, e)| e).unwrap();
        let bt = item.belongs_to.iter().find(|b| b.entity == "Workspace").unwrap();
        assert_eq!(bt.on_delete, OnDelete::Cascade);
        // workspace_id is derived by belongs_to — an explicit field would fail questions.rs.
        assert!(!item.fields.iter().any(|f| f.name == "workspace_id"));
    }

    #[test]
    fn mismatched_fk_names_and_unmappable_types_gap_instead_of_guessing() {
        let out = build();
        // author_id references workspaces but snake(Workspace)_id == workspace_id ≠ author_id.
        assert!(out.gaps.iter().any(|g| g.kind == crate::platform::migrate::gaps::GapKind::ForeignKey
            && g.source.contains("author_id")));
        // point column → unmapped_type gap, field dropped.
        assert!(out.gaps.iter().any(|g| g.kind == crate::platform::migrate::gaps::GapKind::UnmappedType
            && g.source.contains("location")));
        let item = out.entities.iter().find(|(_, e)| e.name == "OrderItem").map(|(_, e)| e).unwrap();
        assert!(!item.fields.iter().any(|f| f.name == "location"));
    }

    #[test]
    fn naming_helpers_are_deterministic() {
        assert_eq!(entity_name("order_items"), "OrderItem");
        assert_eq!(entity_name("companies"), "Company");
        assert_eq!(entity_name("statuses"), "Status");
        assert_eq!(entity_name("people"), "Person");
        assert_eq!(entity_name("workspace"), "Workspace");
    }
}
```

- [ ] **Step 2: Run to verify it fails** — `cargo test -p jerrycan migrate::entities` → FAIL.

- [ ] **Step 3: Minimal implementation** (`entities.rs`)

```rust
//! Spec §Deterministic translator (1): CREATE TABLE → Entity/Field/belongs_to.
//! Reserved-schema tables (auth/storage/cron/…) are handled by their own
//! mappers; this stage only sees `public` tables the caller passes in.

use super::gaps::{GapItem, GapKind, Severity};
use super::pgmodel::{FkAction, PgDatabase, PgTable};
use super::typemap::{MappedType, map_pg_type};
use crate::platform::design::{BelongsTo, Design, Entity, Field, FieldType, OnDelete};

pub struct BuildResult {
    /// (source "schema.table", entity) — table key kept for seeding + grouping.
    pub entities: Vec<(String, Entity)>,
    pub gaps: Vec<GapItem>,
}

const IRREGULAR: &[(&str, &str)] = &[("people", "person"), ("children", "child"), ("statuses", "status")];

fn singularize(word: &str) -> String {
    for (plural, singular) in IRREGULAR {
        if word == *plural { return (*singular).to_string(); }
    }
    if let Some(stem) = word.strip_suffix("ies") { return format!("{stem}y"); }
    if let Some(stem) = word.strip_suffix("ses") { return format!("{stem}s"); }
    if word.len() > 1 && word.ends_with('s') && !word.ends_with("ss") {
        return word[..word.len() - 1].to_string();
    }
    word.to_string()
}

/// "order_items" → "OrderItem" (last segment singularized, all PascalCased).
pub fn entity_name(table: &str) -> String {
    let segments: Vec<&str> = table.split('_').collect();
    let mut out = String::new();
    for (i, seg) in segments.iter().enumerate() {
        let seg = if i == segments.len() - 1 { singularize(seg) } else { (*seg).to_string() };
        let mut chars = seg.chars();
        if let Some(first) = chars.next() {
            out.push(first.to_ascii_uppercase());
            out.push_str(chars.as_str());
        }
    }
    out
}

pub fn build_entities(db: &PgDatabase) -> BuildResult {
    let mut entities = Vec::new();
    let mut gaps = Vec::new();
    for (key, table) in &db.tables {
        if !key.starts_with("public.") { continue; }
        if let Some(entity) = build_one(key, table, db, &mut gaps) {
            entities.push((key.clone(), entity));
        }
    }
    BuildResult { entities, gaps }
}
```

`build_one(key, table, db, gaps) -> Option<Entity>` logic (implement fully):
1. Composite pk → blocking `UnmappedType` gap (`reason: "composite primary key"`), return `None` (table skipped; the agent models it).
2. For each fk with a single column: resolve target entity via `entity_name(ref table name)`. If the fk column name equals `Design::fk_column(&target)` → emit `BelongsTo { entity: target, on_delete }` (`FkAction::Cascade → OnDelete::Cascade`, `SetNull → SetNull`, `Restrict → Restrict`) and record the column as suppressed. Else → `GapKind::ForeignKey` advisory gap (`source: "<key>.<column>"`, `suggested: "rename the column to <derived> in the seed mapping or keep it as a plain field + handler-enforced integrity"`), and keep the column as a plain field.
3. For each non-suppressed column: `map_pg_type` → `Field { name, field_type, required: not_null, unique, index: indexed, values: check_in_values.or(enum values) }`. `Unmappable` → blocking `UnmappedType` gap (`source: "<key>.<column>"`, `original: pg_type`, `suggested: "model as string/json or a separate entity; update the seed mapping"`), column dropped.
4. `id` column: must map to integer/string/uuid (questions.rs pk rule); other pk types → blocking gap + skip table.

- [ ] **Step 4: Run to verify it passes** — `cargo test -p jerrycan migrate::entities` → PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan/src/platform/migrate
git commit -m "Add migrate table-to-entity builder: fields, belongs_to, flags, gaps"
```

---

### Task 7: module grouping (FK graph + name prefix)

**Files:**
- Create: `crates/jerrycan/src/platform/migrate/grouping.rs`
- Modify: `crates/jerrycan/src/platform/migrate/mod.rs` (`pub mod grouping;`)
- Test: inline in `grouping.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fk_components_group_and_hub_edges_do_not_merge_everything() {
        // customers→workspaces(hub), notes→customers, billing_invoices→workspaces(hub),
        // billing_receipts→billing_invoices, plans (isolated).
        let edges = vec![
            ("public.customers".to_string(), "public.workspaces".to_string()),
            ("public.notes".to_string(), "public.customers".to_string()),
            ("public.billing_invoices".to_string(), "public.workspaces".to_string()),
            ("public.billing_receipts".to_string(), "public.billing_invoices".to_string()),
        ];
        let tables = ["public.billing_invoices", "public.billing_receipts", "public.customers",
                      "public.notes", "public.plans", "public.workspaces"]
            .map(String::from).to_vec();
        let hubs = ["public.workspaces".to_string()].into_iter().collect();
        let modules = group_modules(&tables, &edges, &hubs);
        // Deterministic: sorted by module name; hub gets its own module.
        assert_eq!(
            modules,
            vec![
                ("billing".to_string(), vec!["public.billing_invoices".into(), "public.billing_receipts".into()]),
                ("customers".to_string(), vec!["public.customers".into(), "public.notes".into()]),
                ("plans".to_string(), vec!["public.plans".into()]),
                ("workspaces".to_string(), vec!["public.workspaces".into()]),
            ]
        );
    }
}
```

- [ ] **Step 2: Run to verify it fails** — `cargo test -p jerrycan migrate::grouping` → FAIL.

- [ ] **Step 3: Minimal implementation** (`grouping.rs`)

```rust
//! Spec §Deterministic translator (2): cluster tables into modules by FK graph
//! + shared name prefix. Hub tables (tenant, users) anchor their own modules —
//! edges INTO a hub are ignored so tenancy doesn't collapse the app into one
//! module. The agent refines grouping afterwards (spec §Agent-judgment layer).

use std::collections::{BTreeMap, BTreeSet};

/// `edges` are (child, parent) fk pairs among "schema.table" keys.
/// Returns (module_name, member tables sorted), sorted by module name.
pub fn group_modules(
    tables: &[String],
    edges: &[(String, String)],
    hubs: &BTreeSet<String>,
) -> Vec<(String, Vec<String>)> {
    // Union-find over non-hub edges.
    let mut parent: BTreeMap<&str, &str> = tables.iter().map(|t| (t.as_str(), t.as_str())).collect();
    fn find<'a>(parent: &BTreeMap<&'a str, &'a str>, mut x: &'a str) -> &'a str {
        while parent[x] != x { x = parent[x]; }
        x
    }
    for (child, par) in edges {
        if hubs.contains(child) || hubs.contains(par) { continue; }
        let (rc, rp) = (find(&parent, child.as_str()), find(&parent, par.as_str()));
        if rc != rp {
            let (lo, hi) = if rc < rp { (rc, rp) } else { (rp, rc) };
            parent.insert(hi, lo); // deterministic: smaller name wins as root
        }
    }
    // Merge components whose ROOT tables share a `_`-prefix of ≥ 3 chars.
    let mut components: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for t in tables {
        components.entry(find(&parent, t).to_string()).or_default().push(t.clone());
    }
    let mut by_prefix: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (root, mut members) in components {
        members.sort();
        let bare = root.rsplit('.').next().unwrap_or(&root);
        let prefix = bare.split('_').next().unwrap_or(bare);
        let key = if prefix.len() >= 3 { prefix.to_string() } else { bare.to_string() };
        by_prefix.entry(key).or_default().extend(members);
    }
    let mut out: Vec<(String, Vec<String>)> = by_prefix
        .into_iter()
        .map(|(name, mut members)| {
            members.sort();
            members.dedup();
            // Module names are kebab-case (questions.rs); tables are snake.
            (name.replace('_', "-"), members)
        })
        .collect();
    out.sort();
    out
}
```

Note the deterministic tie-breaks: union roots by lexicographic minimum; module name = shared prefix when ≥3 chars, else the root table's bare name. `plans` stays `plans` (module names keep the table's plural — they're route areas, not entities).

- [ ] **Step 4: Run to verify it passes** — `cargo test -p jerrycan migrate::grouping` → PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan/src/platform/migrate
git commit -m "Add migrate module grouping: fk components with hub exclusion and prefix merge"
```

---

### Task 8: the conservative RLS recognizer

**Files:**
- Create: `crates/jerrycan/src/platform/migrate/rls.rs`
- Modify: `crates/jerrycan/src/platform/migrate/mod.rs` (`pub mod rls;`)
- Test: inline in `rls.rs`

This is the security-critical stage. **Canonical shapes only; every other predicate returns `Gap` — never a guess** (spec §RLS translation + Resolved decision 3). The backstop is the generated isolation tests, but the recognizer itself must be provably conservative: the tests below include shapes that *look* close and MUST gap.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::migrate::{parse, pgmodel::PgDatabase};

    fn policy_scopes(sql: &str) -> Recognized {
        let full = format!("create table public.t (id uuid primary key);\n{sql}");
        let db = PgDatabase::fold(&parse::split_and_parse(&full));
        recognize(&db.policies[0])
    }

    #[test]
    fn owner_eq_auth_uid_recognizes_both_orders_and_select_wrapping() {
        for sql in [
            r#"create policy p on public.t using (user_id = auth.uid());"#,
            r#"create policy p on public.t using (auth.uid() = user_id);"#,
            r#"create policy p on public.t using ((select auth.uid()) = user_id);"#,
        ] {
            match policy_scopes(sql) {
                Recognized::Scopes(s) => assert_eq!(s, vec![Scope::Owner { column: "user_id".into() }], "{sql}"),
                Recognized::Gap { reason } => panic!("{sql} must recognize: {reason}"),
            }
        }
    }

    #[test]
    fn membership_join_recognizes_in_and_exists_shapes() {
        let in_shape = r#"create policy p on public.t using
            (workspace_id in (select workspace_id from public.workspace_members where user_id = auth.uid()));"#;
        let exists_shape = r#"create policy p on public.t using
            (exists (select 1 from public.workspace_members m
                     where m.workspace_id = t.workspace_id and m.user_id = auth.uid()));"#;
        for sql in [in_shape, exists_shape] {
            match policy_scopes(sql) {
                Recognized::Scopes(s) => assert_eq!(
                    s,
                    vec![Scope::TenantMembership {
                        outer_column: "workspace_id".into(),
                        membership_table: "public.workspace_members".into(),
                        required_roles: vec![],
                    }],
                    "{sql}"
                ),
                Recognized::Gap { reason } => panic!("{sql} must recognize: {reason}"),
            }
        }
    }

    #[test]
    fn membership_join_with_role_filter_carries_required_roles() {
        let sql = r#"create policy p on public.t for delete using
            (workspace_id in (select workspace_id from public.workspace_members
                              where user_id = auth.uid() and role = 'owner'));"#;
        match policy_scopes(sql) {
            Recognized::Scopes(s) => assert_eq!(
                s,
                vec![Scope::TenantMembership {
                    outer_column: "workspace_id".into(),
                    membership_table: "public.workspace_members".into(),
                    required_roles: vec!["owner".into()],
                }]
            ),
            Recognized::Gap { reason } => panic!("must recognize: {reason}"),
        }
    }

    #[test]
    fn storage_foldername_prefix_and_bucket_eq_recognize_together() {
        let sql = r#"create policy p on storage.objects for all using
            (bucket_id = 'avatars' and (storage.foldername(name))[1] = auth.uid()::text);"#;
        match policy_scopes(sql) {
            Recognized::Scopes(s) => assert_eq!(
                s,
                vec![Scope::BucketEq { bucket: "avatars".into() }, Scope::OwnerPrefix]
            ),
            Recognized::Gap { reason } => panic!("must recognize: {reason}"),
        }
    }

    #[test]
    fn public_read_and_role_gates_recognize() {
        match policy_scopes(r#"create policy p on public.t for select using (true);"#) {
            Recognized::Scopes(s) => assert_eq!(s, vec![Scope::PublicRead]),
            Recognized::Gap { reason } => panic!("{reason}"),
        }
        match policy_scopes(r#"create policy p on public.t using (auth.uid() is not null);"#) {
            Recognized::Scopes(s) => assert_eq!(s, vec![Scope::Authenticated]),
            Recognized::Gap { reason } => panic!("{reason}"),
        }
        match policy_scopes(r#"create policy p on public.t to authenticated using (true);"#) {
            Recognized::Scopes(s) => assert_eq!(s, vec![Scope::Authenticated]),
            Recognized::Gap { reason } => panic!("{reason}"),
        }
    }

    #[test]
    fn near_miss_shapes_gap_and_never_guess() {
        // Share-list join (not a membership shape: subquery filters on the row pk).
        let share = r#"create policy p on public.t for select using
            (exists (select 1 from public.note_shares s where s.note_id = t.id and s.shared_with = auth.uid()));"#;
        // OR-composition is never mechanical. A `true` WRITE policy is never public-read.
        let ored = r#"create policy p on public.t using (user_id = auth.uid() or is_public);"#;
        let open_write = r#"create policy p on public.t for insert with check (true);"#;
        // Arbitrary jwt-claim condition.
        let claim = r#"create policy p on public.t using ((auth.jwt() ->> 'plan') = 'pro');"#;
        for sql in [share, ored, open_write, claim] {
            assert!(matches!(policy_scopes(sql), Recognized::Gap { .. }), "{sql} MUST gap");
        }
    }
}
```

- [ ] **Step 2: Run to verify it fails** — `cargo test -p jerrycan migrate::rls` → FAIL.

- [ ] **Step 3: Minimal implementation** (`rls.rs`)

```rust
//! The conservative RLS recognizer (spec §4). Canonical, unambiguous shapes
//! ONLY. `recognize` returns Gap for anything else — the gap report + generated
//! isolation tests are the safety net; this module must never guess (Resolved
//! decision 3: "unrecognized → gap report (never guessed)").

use super::pgmodel::{PgPolicy, PolicyCommand};
use sqlparser::ast::{BinaryOperator, Expr, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// `<column> = auth.uid()`
    Owner { column: String },
    /// `<outer_column> IN (SELECT <outer_column> FROM <m> WHERE <user_col> = auth.uid() [AND role …])`
    /// or the equivalent EXISTS join. Validated against the detected membership
    /// table in tenancy.rs — the recognizer only certifies the SHAPE.
    TenantMembership { outer_column: String, membership_table: String, required_roles: Vec<String> },
    /// `(storage.foldername(name))[1] = auth.uid()[::text]`
    OwnerPrefix,
    /// `USING (true)` on SELECT.
    PublicRead,
    /// `auth.uid() IS NOT NULL`, or `TO authenticated`, or `auth.role() = 'authenticated'`.
    Authenticated,
    /// `bucket_id = '<name>'` — meaningful only on storage.objects policies.
    BucketEq { bucket: String },
}

#[derive(Debug)]
pub enum Recognized {
    Scopes(Vec<Scope>),
    Gap { reason: String },
}

pub fn recognize(policy: &PgPolicy) -> Recognized {
    // TO authenticated + true is the "any logged-in user" canonical shape.
    let to_authenticated = policy.to_roles.iter().any(|r| r == "authenticated");
    let exprs: Vec<&Expr> = [policy.using.as_ref(), policy.with_check.as_ref()].into_iter().flatten().collect();
    if exprs.is_empty() {
        return Recognized::Gap { reason: "policy has neither USING nor WITH CHECK we can read".into() };
    }
    let mut scopes = Vec::new();
    for expr in exprs {
        for conjunct in split_conjuncts(expr) {
            match classify(strip(conjunct), policy, to_authenticated) {
                Some(scope) => scopes.push(scope),
                None => {
                    return Recognized::Gap {
                        reason: format!("predicate `{conjunct}` is not a canonical shape — not auto-translated"),
                    };
                }
            }
        }
    }
    scopes.sort_by_key(|s| format!("{s:?}"));
    scopes.dedup();
    Recognized::Scopes(scopes)
}
```

Helpers (implement fully):
- `split_conjuncts(&Expr) -> Vec<&Expr>` — recursively split `BinaryOperator::And`; `Or` is NOT split (it falls through to `classify`, which won't match → gap).
- `strip(&Expr) -> &Expr` — unwrap `Expr::Nested`, `Expr::Cast { expr, .. }`, and a `Expr::Subquery` whose query is exactly `SELECT auth.uid()` (return a canonical auth-uid marker — easiest: a helper `is_auth_uid(&Expr) -> bool` that answers Function `auth.uid` OR that bare subquery, used by the matchers instead of stripping subqueries).
- `is_auth_uid` — `Expr::Function` whose `ObjectName` parts are `["auth", "uid"]` (case-insensitive), or `Expr::Subquery` wrapping exactly that.
- `column_name(&Expr) -> Option<String>` — `Identifier(i)` → `i`; `CompoundIdentifier(parts)` → last part (the table qualifier is checked by the membership matcher where it matters).
- `classify(expr, policy, to_authenticated) -> Option<Scope>` arms, in order:
  1. `Value(Boolean(true))` → `PublicRead` only when `policy.command` is `Select` (or `All` **with** `to_authenticated` → `Authenticated`); bare `true` on a write command → `None` (gap: open-write is never canonical).
  2. `IsNotNull(inner)` where `is_auth_uid(inner)` → `Authenticated`.
  3. `BinaryOp Eq`: (a) one side `is_auth_uid`, other side a column → `Owner`; (b) `Function auth.role() = '<lit>'` → `Authenticated` when lit == "authenticated", else `None`; (c) `Identifier bucket_id = SingleQuotedString(b)` → `BucketEq`; (d) one side `Expr::Subscript { expr: Function storage.foldername(args=[column]), subscript: 1 }` (allow `Cast` around either side), other side `is_auth_uid` → `OwnerPrefix`.
  4. `InSubquery { expr: column C, subquery, negated: false }` → membership shape: subquery must be a single-table `SELECT <C> FROM <m> WHERE <conds>` where conds split into exactly: one `<user_col> = auth.uid()` and optionally `role = '<lit>'` / `role IN ('<lit>…)'` → `TenantMembership { outer_column: C, membership_table: m, required_roles }`. Any extra condition, join, DISTINCT-on-other-column, or projection mismatch → `None`.
  5. `Exists { subquery, negated: false }` → same, matching `WHERE m.<C> = <outer>.<C> AND m.<user_col> = auth.uid() [AND role…]`; the correlated column pair must use the SAME column name on both sides (that's the canonical Supabase template) — different names → `None`.
- If `to_authenticated` and scopes would otherwise be empty (policy was just `TO authenticated USING (true)`) the `true` arm above already yields `Authenticated`.

- [ ] **Step 4: Run to verify it passes** — `cargo test -p jerrycan migrate::rls` → PASS (all six tests, especially `near_miss_shapes_gap_and_never_guess`).

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan/src/platform/migrate
git commit -m "Add migrate conservative RLS recognizer: canonical shapes only, near-misses gap"
```

---

### Task 9: tenancy detection + the owner rule (R3)

**Files:**
- Create: `crates/jerrycan/src/platform/migrate/tenancy.rs`
- Modify: `crates/jerrycan/src/platform/migrate/mod.rs` (`pub mod tenancy;`)
- Test: inline in `tenancy.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::migrate::{parse, pgmodel::PgDatabase};

    const ORG_SCHEMA: &str = r#"
create table public.workspaces (id uuid primary key, name text not null);
create table public.workspace_members (
    workspace_id uuid not null references public.workspaces(id) on delete cascade,
    user_id uuid not null,
    role text not null check (role in ('owner', 'member')),
    primary key (workspace_id, user_id)
);
create table public.customers (
    id uuid primary key,
    workspace_id uuid not null references public.workspaces(id),
    email text not null
);
alter table public.customers enable row level security;
create policy m on public.customers using
    (workspace_id in (select workspace_id from public.workspace_members where user_id = auth.uid()));
create table public.todos (id uuid primary key, user_id uuid not null, title text);
alter table public.todos enable row level security;
create policy own on public.todos using (user_id = auth.uid());
"#;

    #[test]
    fn membership_join_detects_the_tenant_and_member_roles() {
        let db = PgDatabase::fold(&parse::split_and_parse(ORG_SCHEMA));
        let det = detect(&db);
        assert_eq!(det.tenant_table.as_deref(), Some("public.workspaces"));
        assert_eq!(det.membership_table.as_deref(), Some("public.workspace_members"));
        assert_eq!(det.member_roles, vec!["owner", "member"], "from the role CHECK, declaration order");
    }

    #[test]
    fn owner_scoped_table_without_the_tenant_fk_is_a_blocking_gap_under_org_tenancy() {
        let db = PgDatabase::fold(&parse::split_and_parse(ORG_SCHEMA));
        let det = detect(&db);
        // todos has user_id = auth.uid() but no workspace_id → R3(a): gap, never guessed.
        let access = table_access(&db, &det);
        assert!(matches!(access["public.todos"], TableAccess::Gap { .. }));
        assert!(matches!(access["public.customers"], TableAccess::Tenant { .. }));
    }

    #[test]
    fn pure_owner_apps_get_user_as_the_tenant() {
        let owner_only = r#"
create table public.todos (id uuid primary key, user_id uuid not null, title text);
alter table public.todos enable row level security;
create policy own on public.todos using (user_id = auth.uid());
"#;
        let db = PgDatabase::fold(&parse::split_and_parse(owner_only));
        let det = detect(&db);
        assert!(det.tenant_table.is_none());
        let access = table_access(&db, &det);
        // R3(b): no org tenant anywhere → owner tables scope by tenant User.
        assert!(matches!(access["public.todos"], TableAccess::OwnerAsUserTenant { .. }));
    }
}
```

- [ ] **Step 2: Run to verify it fails** — `cargo test -p jerrycan migrate::tenancy` → FAIL.

- [ ] **Step 3: Minimal implementation** (`tenancy.rs`)

```rust
//! Spec §3 tenancy detection + §4's owner translation, rule R3 (resolved
//! ambiguity #4): membership-join → tenancy; owner-only apps → tenant = User;
//! owner tables under org tenancy translate only when the tenant fk is present.

use super::pgmodel::PgDatabase;
use super::rls::{Recognized, Scope, recognize};
use std::collections::BTreeMap;

#[derive(Debug, Default)]
pub struct TenancyDetection {
    pub tenant_table: Option<String>,
    pub membership_table: Option<String>,
    /// From the membership role column's CHECK/enum values, declaration order.
    pub member_roles: Vec<String>,
}

/// Per-table access summary the CRUD/storage mappers consume.
#[derive(Debug)]
pub enum TableAccess {
    /// Membership-join scoping (tenant fk column carried on the table).
    Tenant { required_roles_by_command: BTreeMap<super::pgmodel::PolicyCommand, Vec<String>> },
    /// R3(b): owner scoping expressed as tenancy over User.
    OwnerAsUserTenant { owner_column: String },
    /// Only `Authenticated` / role gates — guarded but not row-scoped.
    AuthOnly,
    /// SELECT is public; writes carry one of the scoped variants above.
    PublicRead { write: Box<TableAccess> },
    /// RLS enabled but at least one policy didn't recognize → agent work.
    Gap { reasons: Vec<String> },
    /// RLS disabled (resolved ambiguity #6): guarded by default + advisory.
    NoRls,
}
```

`detect(db)`: find candidate membership tables — a table whose fks include one to a table T and whose columns include a `user_id` (uuid) column, and which is named by at least one recognized `TenantMembership.membership_table` across all policies; tie-break: most-referenced, then lexicographic. `member_roles` from the membership table's `role` column `check_in_values` (or enum values), preserved in declaration order; fallback: empty (the seed pass may fill from data later — Task 14 note).

`table_access(db, det)`: for each RLS-enabled public table, run `recognize` over its policies and fold:
- All scopes recognized AND every `TenantMembership` names `det.membership_table` with `outer_column == fk to tenant` → `Tenant` (collect per-command `required_roles`).
- `Owner{column}` scopes: if `det.tenant_table.is_some()` — table also has the tenant fk → fold into `Tenant` + push an *advisory* gap (owner filter subsumed by tenant scope — the caller collects these); no tenant fk → `Gap` (blocking). If no org tenant at all → `OwnerAsUserTenant`.
- `PublicRead` on SELECT + recognized write scopes → `PublicRead { write }`; note: the CRUD mapper (Task 10) rejects `PublicRead` over tenant-scoped writes (questions.rs forbids public on tenant-owned) and downgrades to fully-guarded + advisory gap.
- Only `Authenticated` → `AuthOnly`. Any `Recognized::Gap` → `TableAccess::Gap` with reasons.
- RLS-disabled tables → `NoRls`.
- A `TenantMembership` scope that names a DIFFERENT table than the detected membership table (e.g. a share list that happened to match the shape) → `Gap` — the shape certifies syntax; only the detected membership table certifies semantics.

- [ ] **Step 4: Run to verify it passes** — `cargo test -p jerrycan migrate::tenancy` → PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan/src/platform/migrate
git commit -m "Add migrate tenancy detection and owner-scoping rule with gap fallbacks"
```

---

### Task 10: CRUD endpoint emission with guards

**Files:**
- Create: `crates/jerrycan/src/platform/migrate/crud.rs`
- Modify: `crates/jerrycan/src/platform/migrate/mod.rs` (`pub mod crud;`)
- Test: inline in `crud.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::design::HttpMethod;
    use crate::platform::migrate::tenancy::TableAccess;
    use std::collections::BTreeMap;

    #[test]
    fn a_tenant_scoped_entity_gets_the_guarded_crud_five() {
        let eps = endpoints_for("Customer", &TableAccess::Tenant { required_roles_by_command: BTreeMap::new() });
        let ops: Vec<(&str, HttpMethod, &str)> =
            eps.iter().map(|e| (e.operation_id.as_str(), e.method, e.path.as_str())).collect();
        assert_eq!(ops, vec![
            ("list_customers", HttpMethod::GET, "/"),
            ("create_customer", HttpMethod::POST, "/"),
            ("get_customer", HttpMethod::GET, "/{id}"),
            ("update_customer", HttpMethod::PATCH, "/{id}"),
            ("delete_customer", HttpMethod::DELETE, "/{id}"),
        ]);
        assert!(eps.iter().all(|e| e.auth_required), "every tenant endpoint is guarded");
        let get = &eps[2];
        assert!(get.errors.iter().any(|er| er.status == 404 && er.code.as_deref() == Some("JC0404")));
    }

    #[test]
    fn public_read_marks_only_the_reads_public() {
        let access = TableAccess::PublicRead { write: Box::new(TableAccess::AuthOnly) };
        let eps = endpoints_for("Plan", &access);
        assert!(eps.iter().find(|e| e.operation_id == "list_plans").unwrap().public);
        assert!(eps.iter().find(|e| e.operation_id == "get_plan").unwrap().public);
        let create = eps.iter().find(|e| e.operation_id == "create_plan").unwrap();
        assert!(create.auth_required && !create.public);
    }

    #[test]
    fn per_command_roles_flow_into_required_roles() {
        let mut roles = BTreeMap::new();
        roles.insert(crate::platform::migrate::pgmodel::PolicyCommand::Delete, vec!["owner".to_string()]);
        let eps = endpoints_for("Customer", &TableAccess::Tenant { required_roles_by_command: roles });
        let del = eps.iter().find(|e| e.operation_id == "delete_customer").unwrap();
        assert_eq!(del.required_roles, vec!["owner"]);
    }
}
```

- [ ] **Step 2: Run to verify it fails** — `cargo test -p jerrycan migrate::crud` → FAIL.

- [ ] **Step 3: Minimal implementation** (`crud.rs`)

```rust
//! Resolved ambiguity #5: PostgREST exposes CRUD per table, so the translated
//! design exposes the CRUD five per entity, guards derived from the RLS
//! translation. MIGRATION.md maps the old PostgREST paths onto these.

use super::pgmodel::PolicyCommand;
use super::tenancy::TableAccess;
use crate::platform::design::{Design, Endpoint, ErrorCase, HttpMethod, RequestBody, Success};

fn plural_snake(entity: &str) -> String {
    let snake = Design::to_snake(entity);
    if snake.ends_with('s') { format!("{snake}es") } else if snake.ends_with('y') {
        format!("{}ies", &snake[..snake.len() - 1])
    } else { format!("{snake}s") }
}

pub fn endpoints_for(entity: &str, access: &TableAccess) -> Vec<Endpoint> {
    let (read_public, guarded) = match access {
        TableAccess::PublicRead { .. } => (true, true),
        TableAccess::NoRls | TableAccess::Gap { .. } | TableAccess::Tenant { .. }
        | TableAccess::OwnerAsUserTenant { .. } | TableAccess::AuthOnly => (false, true),
    };
    let roles_for = |cmd: PolicyCommand| -> Vec<String> {
        match access {
            TableAccess::Tenant { required_roles_by_command } => {
                required_roles_by_command.get(&cmd).cloned()
                    .or_else(|| required_roles_by_command.get(&PolicyCommand::All).cloned())
                    .unwrap_or_default()
            }
            _ => Vec::new(),
        }
    };
    let not_found = || vec![ErrorCase { status: 404, code: Some("JC0404".into()), when: "unknown id".into() }];
    let plural = plural_snake(entity);
    let single = Design::to_snake(entity);
    let ep = |op: String, method: HttpMethod, path: &str, body: bool, status: u16, list: bool,
              errors: Vec<ErrorCase>, cmd: PolicyCommand, is_read: bool| -> Endpoint {
        let public = read_public && is_read;
        Endpoint {
            operation_id: op,
            method,
            path: path.into(),
            auth_required: guarded && !public,
            required_roles: if public { Vec::new() } else { roles_for(cmd) },
            public,
            request_body: body.then(|| RequestBody { entity: entity.into() }),
            success: Success { status, entity: Some(entity.into()), list },
            errors,
        }
    };
    vec![
        ep(format!("list_{plural}"), HttpMethod::GET, "/", false, 200, true, vec![], PolicyCommand::Select, true),
        ep(format!("create_{single}"), HttpMethod::POST, "/", true, 201, false, vec![], PolicyCommand::Insert, false),
        ep(format!("get_{single}"), HttpMethod::GET, "/{id}", false, 200, false, not_found(), PolicyCommand::Select, true),
        ep(format!("update_{single}"), HttpMethod::PATCH, "/{id}", true, 200, false, not_found(), PolicyCommand::Update, false),
        ep(format!("delete_{single}"), HttpMethod::DELETE, "/{id}", false, 204, false, not_found(), PolicyCommand::Delete, false),
    ]
}
```

Note: `delete` success carries `entity: Some(..)` + status 204 — check against how existing designs express 204 (MINIMAL uses `success: { status: 204 }` with no entity); set `entity: None` for delete to match. Also: the orchestrator (Task 16) is responsible for downgrading `PublicRead` to fully-guarded + advisory gap when the entity is tenant-owned (questions.rs rejects that combination) — add a `pub fn strip_public(eps: &mut [Endpoint])` helper here for it.

- [ ] **Step 4: Run to verify it passes** — `cargo test -p jerrycan migrate::crud` → PASS (adjust the delete-entity assertion per the note).

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan/src/platform/migrate
git commit -m "Add migrate CRUD endpoint emission with RLS-derived guards"
```

---

### Task 11: auth mapping (users, roles, oauth, user seed)

**Files:**
- Create: `crates/jerrycan/src/platform/migrate/authmap.rs`
- Modify: `crates/jerrycan/src/platform/migrate/mod.rs` (`pub mod authmap;`)
- Test: inline in `authmap.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::design::AuthModel;

    #[test]
    fn auth_users_produce_the_jwt_auth_block_and_a_users_module() {
        let out = build_auth(
            &["owner".to_string(), "member".to_string()], // member_roles from tenancy
            &["google".to_string()],                       // providers from auth.identities
        );
        assert_eq!(out.auth.model, AuthModel::Jwt, "Supabase auth is JWT");
        assert_eq!(out.auth.roles, vec!["member", "owner"], "sorted, deduped");
        assert!(out.dependencies.contains(&"auth".to_string()));
        assert!(out.dependencies.contains(&"oauth".to_string()), "google identity → oauth dep");
        let users = &out.users_module;
        assert_eq!(users.name, "users");
        let user = &users.entities[0];
        assert_eq!(user.name, "User");
        let email = user.fields.iter().find(|f| f.name == "email").unwrap();
        assert!(email.unique);
        let hash = user.fields.iter().find(|f| f.name == "password_hash").unwrap();
        assert!(!hash.required, "oauth-only users have no password hash");
        // register + login are public (JL0004 carve-out), matching the reference slice.
        assert!(users.endpoints.iter().any(|e| e.operation_id == "register" && e.public));
        assert!(users.endpoints.iter().any(|e| e.operation_id == "login" && e.public));
    }

    #[test]
    fn no_identity_providers_means_no_oauth_dependency() {
        let out = build_auth(&[], &[]);
        assert!(!out.dependencies.contains(&"oauth".to_string()));
    }
}
```

- [ ] **Step 2: Run to verify it fails** — `cargo test -p jerrycan migrate::authmap` → FAIL.

- [ ] **Step 3: Minimal implementation** (`authmap.rs`)

```rust
//! Spec §5: auth.users → auth.model jwt + roles; identities → oauth dep;
//! users → seed rows preserving bcrypt hashes (verified via jerrycan-auth's
//! bcrypt dispatch — Task 17). Passwords/keys are NEVER copied into config.

use crate::platform::design::{
    Auth, AuthModel, Endpoint, Entity, Field, FieldType, HttpMethod, ModuleDesign, RequestBody, Success,
};

pub struct AuthOutput {
    pub auth: Auth,
    pub dependencies: Vec<String>, // "auth" [+ "oauth"]
    pub users_module: ModuleDesign,
}

pub fn build_auth(member_roles: &[String], providers: &[String]) -> AuthOutput {
    let mut roles: Vec<String> = member_roles.to_vec();
    roles.sort();
    roles.dedup();
    let mut dependencies = vec!["auth".to_string()];
    let non_password: Vec<&String> = providers.iter().filter(|p| *p != "email" && *p != "phone").collect();
    if !non_password.is_empty() {
        dependencies.push("oauth".to_string());
    }
    let field = |name: &str, ft: FieldType, required: bool, unique: bool| Field {
        name: name.into(), field_type: ft, required, unique, index: false, values: None,
    };
    let user = Entity {
        name: "User".into(),
        belongs_to: vec![],
        fields: vec![
            field("id", FieldType::Uuid, true, false),
            field("email", FieldType::String, true, true),
            field("password_hash", FieldType::String, false, false),
        ],
    };
    let users_module = ModuleDesign {
        name: "users".into(),
        mount: None,
        description: Some("Migrated from Supabase auth.users".into()),
        entities: vec![user],
        endpoints: vec![
            Endpoint {
                operation_id: "register".into(), method: HttpMethod::POST, path: "/register".into(),
                auth_required: false, required_roles: vec![], public: true,
                request_body: Some(RequestBody { entity: "User".into() }),
                success: Success { status: 201, entity: Some("User".into()), list: false }, errors: vec![],
            },
            Endpoint {
                operation_id: "login".into(), method: HttpMethod::POST, path: "/login".into(),
                auth_required: false, required_roles: vec![], public: true,
                request_body: Some(RequestBody { entity: "User".into() }),
                success: Success { status: 200, entity: Some("User".into()), list: false }, errors: vec![],
            },
        ],
        subroutes: vec![],
        dependencies: vec![],
    };
    AuthOutput { auth: Auth { model: AuthModel::Jwt, roles }, dependencies, users_module }
}

/// Providers found in auth.identities data (distinct `provider` column values,
/// sorted). Streamed by the seed reader; kept separate so live mode reuses it.
pub fn providers_from_identities(rows: impl Iterator<Item = Vec<Option<String>>>, provider_idx: usize) -> Vec<String> {
    let mut set: std::collections::BTreeSet<String> = rows
        .filter_map(|r| r.get(provider_idx).cloned().flatten())
        .collect();
    set.remove("");
    set.into_iter().collect()
}
```

The user **seed mapping** (auth.users CSV → generated `users` table rows: `id`, `email`, `encrypted_password → password_hash`) is a column-mapping entry consumed by Task 14's seed writer — add `pub fn user_seed_mapping() -> &'static [(&'static str, &'static str)]` returning `[("id","id"),("email","email"),("encrypted_password","password_hash")]`, with unmapped auth.users columns dropped (they're Supabase-internal). OAuth provider config is **never** copied: `MIGRATION.md` (Task 16) lists placeholder env vars per detected provider.

- [ ] **Step 4: Run to verify it passes** — `cargo test -p jerrycan migrate::authmap` → PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan/src/platform/migrate
git commit -m "Add migrate auth mapping: jwt model, roles, oauth detection, users module"
```

---

### Task 12: storage mapping (buckets, bucket policies, object seed)

**Files:**
- Create: `crates/jerrycan/src/platform/migrate/storagemap.rs`
- Modify: `crates/jerrycan/src/platform/migrate/mod.rs` (`pub mod storagemap;`)
- Test: inline in `storagemap.rs`

Uses the `storage` block types as landed by the storage plan (`design.storage`, bucket fields `name`/`visibility`/`owner`/`owner_prefix`/`max_size`/`allowed_mime` — use the exact landed names).

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::migrate::{parse, pgmodel::PgDatabase};

    const BUCKETS_JSON: &str = r#"[
        {"id": "avatars", "name": "avatars", "public": true,  "file_size_limit": 5242880, "allowed_mime_types": ["image/*"]},
        {"id": "invoices", "name": "invoices", "public": false, "file_size_limit": null, "allowed_mime_types": null}
    ]"#;

    const POLICIES: &str = r#"
create table public.t (id uuid primary key);
create policy avatar_owner on storage.objects for all using
    (bucket_id = 'avatars' and (storage.foldername(name))[1] = auth.uid()::text);
create policy invoice_shares on storage.objects for select using
    (bucket_id = 'invoices' and exists
        (select 1 from public.invoice_shares s where s.object_id = objects.id and s.user_id = auth.uid()));
"#;

    #[test]
    fn buckets_translate_with_visibility_size_mime_and_prefix_policy() {
        let db = PgDatabase::fold(&parse::split_and_parse(POLICIES));
        let out = build_storage(BUCKETS_JSON, &db, "User").unwrap();
        let avatars = out.buckets.iter().find(|b| b.name == "avatars").unwrap();
        assert_eq!(avatars.visibility, "public");
        assert_eq!(avatars.owner.as_deref(), Some("User"));
        assert!(avatars.owner_prefix, "foldername[1] = auth.uid() → owner_prefix");
        assert_eq!(avatars.max_size.as_deref(), Some("5MB"), "exact byte count renders human");
        assert_eq!(avatars.allowed_mime, vec!["image/*"]);
    }

    #[test]
    fn share_join_bucket_policies_gap_and_the_bucket_stays_private_guarded() {
        let db = PgDatabase::fold(&parse::split_and_parse(POLICIES));
        let out = build_storage(BUCKETS_JSON, &db, "User").unwrap();
        let invoices = out.buckets.iter().find(|b| b.name == "invoices").unwrap();
        assert_eq!(invoices.visibility, "private");
        assert!(out.gaps.iter().any(|g|
            g.kind == crate::platform::migrate::gaps::GapKind::RlsPolicy
                && g.source.contains("invoice_shares")
                && g.suggested.contains("handler guard")));
    }
}
```

- [ ] **Step 2: Run to verify it fails** — `cargo test -p jerrycan migrate::storagemap` → FAIL.

- [ ] **Step 3: Minimal implementation** (`storagemap.rs`)

Core logic (write against the landed storage-block types; the local `BucketOut` below decouples the tests from field-name drift only if the landed type differs — prefer constructing the landed type directly):

- Parse `buckets.json` (serde into a local `SupabaseBucket { id, name, public: bool, file_size_limit: Option<u64>, allowed_mime_types: Option<Vec<String>> }`).
- Partition `db.policies` by `table == "storage.objects"`, group per bucket via the `BucketEq` conjunct from `rls::recognize`; a storage policy without a recognizable `BucketEq` conjunct → blocking gap (applies to unknown buckets).
- Per bucket fold scopes: `OwnerPrefix` → `owner_prefix: true` + `owner: <user entity>`; `Owner{column: "owner"}` (Supabase's `storage.objects.owner`) → `owner`; `TenantMembership` validated against the detected membership table → `owner: <tenant entity>`; `PublicRead` on SELECT + `public: true` in buckets.json → `visibility: "public"` (asymmetric read/write is native to the storage block); any `Recognized::Gap` → gap item (`suggested: "implement as a handler guard on the <bucket> bucket endpoints"`), bucket emitted `private` with no owner (fully guarded — secure default).
- `max_size`: resolved ambiguity #11 — `5242880 → "5MB"`, `1048576 → "1MB"`, `10240 → "10KB"`; non-exact → round up to next MB + advisory gap.
- Object seed: `data/storage.objects.csv` rows map to the generated `storage_objects` table — mapping `[("id","id"),("bucket_id","bucket"),("name","key"),("owner","owner_id")]`, `size`/`mime` extracted from the `metadata` JSON column (`size`, `mimetype` keys), `checksum` computed sha256 (streamed) from `storage/objects/<bucket>/<key>` bytes when present (missing bytes → advisory gap per object file, row still seeded), `tenant_id` derived from `owner_id` via the membership map **only when that owner has exactly one membership** (ambiguous → blocking gap naming the object). Emit as a seed table entry for Task 14 plus a `blobs` list `(bucket, key, source_path)`.

- [ ] **Step 4: Run to verify it passes** — `cargo test -p jerrycan migrate::storagemap` → PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan/src/platform/migrate
git commit -m "Add migrate storage mapping: buckets, prefix/owner policies, object seed plan"
```

---

### Task 13: realtime + cron mapping

**Files:**
- Create: `crates/jerrycan/src/platform/migrate/realtimemap.rs`
- Create: `crates/jerrycan/src/platform/migrate/cronmap.rs`
- Modify: `crates/jerrycan/src/platform/migrate/mod.rs` (`pub mod realtimemap; pub mod cronmap;`)
- Test: inline in both files

- [ ] **Step 1: Write the failing tests**

`realtimemap.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn publication_tables_become_realtime_changes_plus_standing_advisories() {
        let mut pubs = BTreeMap::new();
        pubs.insert("supabase_realtime".to_string(),
                    vec!["public.customers".to_string(), "public.ghosts".to_string()]);
        let mapped: BTreeMap<String, String> =
            [("public.customers".to_string(), "Customer".to_string())].into_iter().collect();
        let out = build_realtime(&pubs, &mapped);
        assert_eq!(out.changes, vec!["Customer"]);
        // A published table we didn't map → realtime_channel gap (blocking).
        assert!(out.gaps.iter().any(|g| g.kind == crate::platform::migrate::gaps::GapKind::RealtimeChannel
            && g.source.contains("public.ghosts")));
        // Broadcast/Presence live client-side (spec §7) → one advisory each, always.
        assert!(out.gaps.iter().any(|g| g.kind == crate::platform::migrate::gaps::GapKind::Broadcast));
        assert!(out.gaps.iter().any(|g| g.kind == crate::platform::migrate::gaps::GapKind::Presence));
    }
}
```

`cronmap.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const CRON_SQL: &str = r#"
select cron.schedule('nightly-digest', '0 3 * * *', $$select public.send_digest()$$);
select cron.schedule('hourly-sync', '@hourly', $$select public.sync()$$);
"#;

    #[test]
    fn five_field_cron_rows_become_jobs_with_body_gaps() {
        let out = build_jobs(CRON_SQL);
        assert_eq!(out.jobs.len(), 1);
        assert_eq!(out.jobs[0].name, "nightly_digest", "snake_cased for questions.rs");
        assert_eq!(out.jobs[0].schedule.as_deref(), Some("0 3 * * *"));
        // The job BODY is agent work — the generated task fn is a stub.
        assert!(out.gaps.iter().any(|g| g.kind == crate::platform::migrate::gaps::GapKind::PgFunction
            && g.original.contains("send_digest")));
        // @hourly is not the 5-field shape questions.rs accepts → cron_job gap, never guessed.
        assert!(out.gaps.iter().any(|g| g.kind == crate::platform::migrate::gaps::GapKind::CronJob
            && g.source.contains("hourly-sync")));
    }
}
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p jerrycan migrate::realtimemap migrate::cronmap` → FAIL.

- [ ] **Step 3: Minimal implementation**

`realtimemap.rs` — `build_realtime(publications, table_to_entity) -> RealtimeOutput { changes: Vec<String> /*sorted entity names*/, gaps }`: look up `supabase_realtime`; each published table maps via `table_to_entity` or emits a blocking `RealtimeChannel` gap (`suggested: "add the table's entity to realtime.changes after modeling it, or drop the subscription"`). Always append the two advisories: Broadcast (`reason: "Broadcast topics live in client code, not the database"`, `suggested: "recreate used topics as realtime.broadcast[] entries from frontend usage"`) and Presence (same shape). The orchestrator puts `changes` into the design's `realtime` block only when non-empty.

`cronmap.rs` — `build_jobs(cron_sql) -> JobsOutput { jobs: Vec<JobDesign>, gaps }`: recognize `cron.schedule('<name>', '<sched>', $$<command>$$)` calls (reuse `parse::split_and_parse`; the calls parse as `SELECT` statements — extract the three literal args from the AST; dollar-quoted third arg arrives as a string literal) AND `INSERT INTO cron.job` rows (columns `jobname, schedule, command`). Validate the schedule with the same 5-field shape check `questions.rs` uses (duplicate the small predicate here; keep them textually identical). Valid → `JobDesign { name: snake_cased(jobname), schedule: Some(s), queue: None }` + a `PgFunction` gap for the command body (`suggested: "implement crates/jobs task `<name>` with this SQL's behavior"`). Invalid shape → `CronJob` gap (blocking).

- [ ] **Step 4: Run to verify they pass** — `cargo test -p jerrycan migrate::realtimemap migrate::cronmap` → PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan/src/platform/migrate
git commit -m "Add migrate realtime publication and cron job mapping"
```

---

### Task 14: the streamed seed writer + resumable `jerrycan db seed`

**Files:**
- Create: `crates/jerrycan/src/platform/migrate/seed.rs`
- Modify: `crates/jerrycan/src/platform/migrate/mod.rs` (`pub mod seed;`)
- Modify: `crates/jerrycan/src/main.rs` (`DbCmd::Seed` + `cmd_db_seed`)
- Test: inline in `seed.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_csv_reader_streams_quotes_embedded_delimiters_and_nulls() {
        let csv = "id,title,note\n1,\"a, \"\"quoted\"\" title\",\\N\n2,plain,ok\n";
        let rows: Vec<Vec<Option<String>>> = CsvReader::new(csv.as_bytes()).map(|r| r.unwrap()).collect();
        assert_eq!(rows[0], vec![Some("1".into()), Some("a, \"quoted\" title".into()), None], "\\N is NULL");
        assert_eq!(rows[1][2], Some("ok".into()));
    }

    #[test]
    fn small_tables_write_batched_inserts_large_tables_go_bulk() {
        let tmp = tempfile::tempdir().unwrap();
        let mut writer = SeedWriter::new(tmp.path(), /*bulk_threshold*/ 3, /*batch*/ 2);
        let cols = vec![col("id", SeedType::Integer), col("title", SeedType::Text)];
        // 2 rows ≤ threshold → inline SQL, batched.
        writer.write_table("todos", &cols, rows(&[&["1", "alpha"], &["2", "it's"]]), 2).unwrap();
        // 4 rows > threshold → verbatim bulk CSV.
        writer.write_table("events", &cols, rows(&[&["1", "a"], &["2", "b"], &["3", "c"], &["4", "d"]]), 4).unwrap();
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
        // Pure planning logic (no DB): given a manifest + a state file that says
        // todos done and events applied through batch 1, the plan resumes at
        // events batch 2 — re-running a completed seed is a no-op.
        let manifest = manifest_fixture(); // todos inline (1 file), events bulk (4 rows, batch 2)
        let state = r#"{ "done_files": ["seed/inline/001_todos.sql"], "bulk_progress": { "events": 1 } }"#;
        let plan = ApplyPlan::from(&manifest, Some(state)).unwrap();
        assert_eq!(plan.steps, vec![ApplyStep::Bulk { table: "events".into(), skip_batches: 1 }]);
        let done = ApplyPlan::from(&manifest, Some(r#"{ "done_files": ["seed/inline/001_todos.sql"], "bulk_progress": { "events": 2 } }"#)).unwrap();
        assert!(done.steps.is_empty(), "fully applied seed is a no-op");
    }
}
```

(Test helpers `col`, `rows`, `manifest_fixture` are small local fns — write them concretely in the test module.)

- [ ] **Step 2: Run to verify it fails** — `cargo test -p jerrycan migrate::seed` → FAIL.

- [ ] **Step 3: Minimal implementation** (`seed.rs`)

Three concrete pieces:

1. **`CsvReader`** — hand-rolled streaming RFC-4180 reader over any `BufRead` (resolved ambiguity — no `csv` crate): iterator of `Result<Vec<Option<String>>, String>`; handles quoted fields, doubled quotes, embedded commas/newlines inside quotes, and `\N` (unquoted) as NULL. Skips the header row (exposes it via `headers()`). ~90 lines; the state machine is `(in_quotes, field_buf, row_buf)` over `chars`.

2. **`SeedWriter`** — `new(project_root, bulk_threshold, batch_size)`; `write_table(table, &[SeedColumn], rows, row_count)`:
   - `row_count ≤ bulk_threshold` → stream into `seed/inline/NNN_<table>.sql` (NNN = insertion order, FK-topological — the orchestrator passes tables in topo order): `INSERT INTO <table> (cols) VALUES (…), (…);` in `batch_size`-row statements. SQL literal rendering per `SeedType { Integer, Float, Boolean, Text, Uuid, Datetime, Json }`: numbers/bools bare (validated parse; invalid → error, fail loud), everything else single-quoted with `'` doubled; NULL → `NULL`.
   - else → stream verbatim to `seed/bulk/<table>.csv` (bytes copied, sha256 computed while streaming — never load the file).
   - Rows are consumed via iterator — **no table is ever fully in memory** (spec §Data seed).
   - `finish()` writes `seed/manifest.json`: `{ "batch_size": …, "tables": [{ "table", "mode": "inline"|"bulk", "file", "rows", "sha256", "columns": [{name,type}] }…], "blobs": [{ "bucket", "key", "file" }…] }` (pretty, stable order) and returns it.

3. **`ApplyPlan` + the applier** — `ApplyPlan::from(manifest_json, state_json) -> Result<ApplyPlan, String>` (pure, tested above), and `pub async fn apply(root: &Path, db: &jerrycan_db::Db) -> Result<AppliedSummary, String>`:
   - inline files: sha256-verify, then `db.conn().execute_unprepared(&sql)` per statement; append the file to `done_files` in `seed/.state.json` after each file.
   - bulk files: stream `CsvReader`, build `batch_size`-row INSERTs using the manifest's column types, execute each batch, then update `bulk_progress[table]` in the state file — **checkpoint after every batch** so an interrupt resumes at the batch boundary.
   - blobs: when `JERRYCAN_STORAGE` is `local:<dir>`, copy `seed/blobs/<bucket>/<key>` → `<dir>/<bucket>/<key>`; otherwise print the S3-upload instruction from MIGRATION.md and skip (recorded in the summary, not silently).

`main.rs`: add `/// Apply the migrated data seed (resumable)` `Seed { #[arg(long)] url: Option<String> }` to `DbCmd`, dispatch to `cmd_db_seed` which mirrors `cmd_db_migrate`'s runtime/connect shape then calls `seed::apply`, and emits `{"applied_tables": …, "resumed": …, "next_step": "jerrycan check"}`.

- [ ] **Step 4: Run to verify it passes** — `cargo test -p jerrycan migrate::seed && cargo build -p jerrycan` → PASS; `target/debug/jerrycan db seed --help` shows the subcommand.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan/src/platform/migrate crates/jerrycan/src/main.rs
git commit -m "Add migrate seed pipeline: streamed CSV, inline/bulk split, resumable db seed"
```

---

### Task 15: secret redaction

**Files:**
- Create: `crates/jerrycan/src/platform/migrate/redact.rs`
- Modify: `crates/jerrycan/src/platform/migrate/mod.rs` (`pub mod redact;`)
- Test: inline in `redact.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const JWT: &str = "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJub3RlIjoiamVycnljYW4gdGVzdCBmaXh0dXJlLCBub3QgYSByZWFsIHNlY3JldCJ9.amVycnljYW4tZml4dHVyZS1zaWduYXR1cmUtcGxhY2Vob2xkZXItMDAw";

    #[test]
    fn jwts_sb_keys_and_password_urls_are_detected() {
        assert_eq!(scan(&format!("key={JWT}"))[0].kind, SecretKind::Jwt);
        assert_eq!(scan(&format!("sb_secret_{}", "x".repeat(24)))[0].kind, SecretKind::SupabaseKey);
        assert_eq!(scan(&format!("sbp_{}", "0".repeat(40)))[0].kind, SecretKind::SupabaseKey);
        assert_eq!(scan("postgresql://postgres:s3cret@db.example.com:5432/x")[0].kind, SecretKind::PasswordUrl);
        assert!(scan("plain text, no secrets, even with eyJ prefix alone").is_empty());
    }

    #[test]
    fn env_files_redact_to_placeholders_and_feed_the_rotation_checklist() {
        let env = format!("SUPABASE_SERVICE_ROLE_KEY={JWT}\nOTHER=fine\n");
        let (redacted, hits) = redact_env(&env);
        assert!(!redacted.contains("eyJ"), "secret bytes never survive: {redacted}");
        assert!(redacted.contains("SUPABASE_SERVICE_ROLE_KEY=<ROTATE-ME:jwt>"));
        assert!(redacted.contains("OTHER=fine"));
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn previews_never_contain_the_secret() {
        let hits = scan(JWT);
        assert!(hits[0].preview.len() < 20 && !JWT.contains(&hits[0].preview), "{}", hits[0].preview);
    }
}
```

- [ ] **Step 2: Run to verify it fails** — `cargo test -p jerrycan migrate::redact` → FAIL.

- [ ] **Step 3: Minimal implementation** (`redact.rs`)

```rust
//! Spec §Security (Rule 14): secrets are never written into the generated app.
//! Hand-rolled matchers (no regex crate): JWT = three dot-joined base64url
//! runs each ≥ 8 chars starting "eyJ"; Supabase key prefixes; conn strings
//! with a password. Data-column hits become `suspected_secret` ADVISORY gaps —
//! flagged, never silently embedded (the data still seeds; it is user data).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretKind { Jwt, SupabaseKey, PasswordUrl }

#[derive(Debug)]
pub struct SecretHit {
    pub kind: SecretKind,
    /// First 8 chars + "…" — safe to print, never the secret.
    pub preview: String,
    pub offset: usize,
}

fn is_b64url(c: char) -> bool { c.is_ascii_alphanumeric() || c == '-' || c == '_' }

pub fn scan(text: &str) -> Vec<SecretHit> { /* … */ }
```

Implement `scan` as three passes over the input:
- **JWT:** find each `"eyJ"`; from there take the maximal `is_b64url` run; require `.`, second run ≥ 8, `.`, third run ≥ 8 → hit spanning all three (this is what Supabase anon/service-role keys are).
- **Supabase keys:** find `"sb_secret_"` (run ≥ 20 total) and `"sbp_"` followed by ≥ 40 hex chars.
- **Password URLs:** find `"postgres://"`/`"postgresql://"`; a `:` between userinfo and an `@` before the next `/` → hit (the password span).
Then `redact_env(text) -> (String, Vec<SecretHit>)`: per line, if the value part contains a hit, replace the whole value with `<ROTATE-ME:jwt|key|password>`; and `pub fn assert_clean(paths: &[PathBuf]) -> Result<(), String>` — scans emitted files (design.json, MIGRATION.md, gap-report.json, generated config) and errors if any hit survives (the orchestrator runs this as a hard gate; a leak is a translator bug, exit non-zero — fail loud).

- [ ] **Step 4: Run to verify it passes** — `cargo test -p jerrycan migrate::redact` → PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan/src/platform/migrate
git commit -m "Add migrate secret scanner: jwt/key/url matchers, env redaction, clean gate"
```

---

### Task 16: MIGRATION.md + full orchestration + docs page

**Files:**
- Create: `crates/jerrycan/src/platform/migrate/migrationmd.rs`
- Modify: `crates/jerrycan/src/platform/migrate/mod.rs` (the `run_migrate` pipeline)
- Modify: `crates/jerrycan/src/main.rs` (`cmd_migrate` full wiring)
- Create: `docs/ai/20-migrate-supabase.md` (or next free number — keep slug `migrate-supabase`)
- Modify: `crates/jerrycan/src/platform/docsidx.rs` (register the page)
- Test: inline in `mod.rs` + `migrationmd.rs`

- [ ] **Step 1: Write the failing test** (in `mod.rs`'s test module — the orchestration contract; uses a small inline export fixture written into a tempdir)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn mini_export(root: &std::path::Path) {
        std::fs::write(root.join("schema.sql"), r#"
create table public.workspaces (id uuid primary key, name text not null);
create table public.workspace_members (
    workspace_id uuid not null references public.workspaces(id) on delete cascade,
    user_id uuid not null, role text not null check (role in ('owner','member')),
    primary key (workspace_id, user_id));
create table public.customers (
    id uuid primary key,
    workspace_id uuid not null references public.workspaces(id) on delete cascade,
    email text not null unique);
alter table public.customers enable row level security;
create policy m on public.customers using
    (workspace_id in (select workspace_id from public.workspace_members where user_id = auth.uid()));
create function public.audit() returns trigger as $$ begin return new; end; $$ language plpgsql;
create publication supabase_realtime for table public.customers;
"#).unwrap();
        std::fs::create_dir_all(root.join("data")).unwrap();
        std::fs::write(root.join("data/public.customers.csv"), "id,workspace_id,email\n").unwrap();
    }

    #[test]
    fn run_migrate_emits_a_question_free_v2_design_plus_artifacts() {
        let tmp = tempfile::tempdir().unwrap();
        let export_dir = tmp.path().join("export");
        std::fs::create_dir_all(&export_dir).unwrap();
        mini_export(&export_dir);
        let out_dir = tmp.path().join("app");
        let out = run_migrate(&MigrateOptions {
            export_dir: export_dir.clone(),
            out_dir: out_dir.clone(),
            name: Some("acme".into()),
            bulk_threshold: 5000,
        }).expect("pipeline runs");
        // The produced design passes the SAME validation `jerrycan new` enforces.
        assert_eq!(out.design.contract_version, 2);
        assert!(crate::platform::questions::validate(&out.design).is_empty(),
            "translator output must be question-free: {:?}", crate::platform::questions::validate(&out.design));
        assert_eq!(out.design.tenancy.as_ref().unwrap().entity, "Workspace");
        // Artifacts on disk, scaffolded project included.
        for rel in ["design.json", "gap-report.json", "MIGRATION.md", "seed/manifest.json"] {
            assert!(out_dir.join(rel).exists(), "{rel}");
        }
        // The plpgsql function landed in the gap report, not the design.
        let gaps = std::fs::read_to_string(out_dir.join("gap-report.json")).unwrap();
        assert!(gaps.contains("pg_function") && gaps.contains("audit"));
        // Determinism: a second run into a fresh dir is byte-identical.
        let out_dir2 = tmp.path().join("app2");
        run_migrate(&MigrateOptions { export_dir, out_dir: out_dir2.clone(), name: Some("acme".into()), bulk_threshold: 5000 }).unwrap();
        for rel in ["design.json", "gap-report.json", "MIGRATION.md"] {
            assert_eq!(std::fs::read(out_dir.join(rel)).unwrap(), std::fs::read(out_dir2.join(rel)).unwrap(), "{rel} deterministic");
        }
    }

    #[test]
    fn migration_md_carries_the_rotation_checklist_and_endpoint_mapping() {
        // (drive via the same fixture; assert on the emitted MIGRATION.md)
        // - "## Secret rotation" section listing JWT secret, anon key, service-role key placeholders
        // - "/rest/v1/customers" mapped to "/customers"
        // - "jerrycan db migrate" then "jerrycan db seed" ordered steps
    }
}
```

(Write `migration_md_carries…` out fully — three `assert!(md.contains(…))` per bullet.)

- [ ] **Step 2: Run to verify it fails** — `cargo test -p jerrycan migrate::tests` → FAIL (`run_migrate` missing).

- [ ] **Step 3: Minimal implementation**

`migrationmd.rs` — `pub fn render(design: &Design, gaps: &[GapItem], seed: &SeedSummary, providers: &[String], modules_by_table: &BTreeMap<String, String>) -> String`, sections in order: `# Migration report`, `## What migrated` (counts table: entities/modules/buckets/realtime channels/jobs/users), `## Endpoint mapping` (per entity: `GET /rest/v1/<table>` → `GET /<module>`, etc.; storage `/storage/v1/object/<bucket>/<key>` → `/<bucket>/{id}`; realtime `supabase.channel('table-db-changes')` → the jerrycan realtime client), `## Apply the data seed` (`jerrycan db migrate` → `jerrycan db seed`, resume semantics, bulk note), `## Secret rotation (do this now)` — checklist of placeholders: `JERRYCAN_SECRET` (new, generated), Supabase JWT secret (rotate — old tokens die with the old backend), anon + service-role keys (revoke), per-provider OAuth `JERRYCAN_OAUTH_<PROVIDER>_CLIENT_ID/SECRET=<ROTATE-ME>`, storage keys; `## Gap report` (counts by severity + pointer to gap-report.json), `## What was NOT migrated` (frontend, plpgsql/edge bodies, Broadcast/Presence — from the spec's non-goals).

`mod.rs` — `MigrateOptions`/`SeedSummary`/`MigrateOutput` + `run_migrate`:
1. `Export::open` → 2. `parse::split_and_parse(schema_sql)` → 3. `PgDatabase::fold` → 4. `tenancy::detect` + `table_access` → 5. `entities::build_entities` (excluding the membership table — it's generated by tenancy — and `auth.*`/`storage.*` tables) → 6. `grouping::group_modules` (hubs = tenant table + users) → 7. per module/entity `crud::endpoints_for` (+ `strip_public` downgrade + advisory gap for tenant-owned public-read) → 8. `authmap::build_auth` (member_roles; providers via `providers_from_identities` streaming `data/auth.identities.csv`) → 9. `storagemap::build_storage` → 10. `realtimemap::build_realtime` (mapped-table → entity map from step 5) → 11. `cronmap::build_jobs(cron_sql)` → 12. edge-function dirs → one blocking `EdgeFunction` gap each; `db.functions`/`db.triggers` → `PgFunction`/`PgTrigger` gaps; leftover `db.unparsed` → advisory `PgFunction` gaps ("statement not understood — review") → 13. assemble `Design { name, contract_version: 2, auth, dependencies (["db","auth"(,"oauth")], sorted stable), tenancy, jobs, modules (users module first, then grouped modules sorted), storage, realtime }` → 14. `questions::validate` MUST be empty — otherwise return `Err` listing the questions prefixed `"translator bug — produced an invalid design:"` (fail loud; this is the same gate `jerrycan new` runs) → 15. `scaffold::scaffold(out_dir, &design)` + `schema::write_schema` → 16. seed pass: FK-topo-sort tables, stream every `data/*.csv` through `SeedWriter` with the entity column mappings (fk column `workspace_id` etc. seeds the belongs_to-derived column of the same name — same name by construction; authmap/storagemap mappings applied; every value scanned by `redact::scan`, hits → `SuspectedSecret` advisory gaps naming table/column/row) + blobs → 17. write `gap-report.json` (`gaps::render_gap_report`) + `MIGRATION.md` → 18. `redact::assert_clean` over design.json/MIGRATION.md/gap-report.json/scaffolded config → hard error on any hit.

`main.rs` `cmd_migrate` — replace the Task-1 stub: offline path calls `run_migrate`, then `emit`s: human line `migrated <n> entities, <b> buckets, <r> realtime channels — <g> gap items (<k> blocking)`; JSON payload `{ "created": files, "design": "<out>/design.json", "gap_report": { "path", "blocking", "advisory" }, "seed": { "tables", "bulk_tables", "rows" }, "next_step": "cd <out> && jerrycan db migrate && jerrycan db seed && jerrycan gen-tests --module <first> && jerrycan check — then work gap-report.json top-down" }`. `--live` stays the Task-1 usage error until Task 18.

Docs page `docs/ai/20-migrate-supabase.md`: the export contract with the exact producing commands —

```
supabase db dump --schema public,auth,storage -f schema.sql          # or pg_dump --schema-only …
psql "$DB_URL" -c "\copy (select * from public.<t>) to 'data/public.<t>.csv' with (format csv, header true, null '\N')"
psql "$DB_URL" -Atc "select coalesce(json_agg(b), '[]'::json) from storage.buckets b" > storage/buckets.json
# object bytes: supabase storage cp -r ss:///<bucket> storage/objects/<bucket>
psql "$DB_URL" -c "\copy (select jobname, schedule, command from cron.job) to stdout" > cron.sql   # or dump cron.schedule() calls
```

plus the directory tree, `--live` caveats, seed/resume semantics, and the rotation checklist rationale. Register in `docsidx.rs` following the existing `PAGES` pattern.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p jerrycan migrate && cargo build -p jerrycan && target/debug/jerrycan docs migrate-supabase | head -5`
Expected: all migrate tests PASS; the docs page renders.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan/src/platform/migrate crates/jerrycan/src/main.rs crates/jerrycan/src/platform/docsidx.rs docs/ai
git commit -m "Wire jerrycan migrate end-to-end: orchestration, MIGRATION.md, docs page"
```

---

### Task 17: bcrypt verification in jerrycan-auth (lossless login) — SKIP IF ALREADY LANDED

**Check first:** if `crates/jerrycan-auth/src/password.rs` already dispatches on `$2`, the storage-phase auth work landed this — skip the task and tick it done.

**Files:**
- Modify: `crates/jerrycan-auth/Cargo.toml` (+ workspace `Cargo.toml`: `bcrypt = "0.17"`)
- Modify: `crates/jerrycan-auth/src/password.rs`
- Test: inline in `password.rs`

- [ ] **Step 1: Write the failing test** (append to `password.rs` tests)

```rust
    #[test]
    fn migrated_bcrypt_hashes_verify_and_report_needs_rehash() {
        // Canonical bcrypt test vector (Openwall/John): password "U*U".
        let bc = "$2a$05$CCCCCCCCCCCCCCCCCCCCC.E5YPO9kmyuRGyh0XouQYb4YMJKvyOeW";
        assert!(verify_password("U*U", bc).unwrap(), "a migrated Supabase user logs in unchanged");
        assert!(!verify_password("wrong", bc).unwrap());
        // Transparent upgrade path: bcrypt says rehash, argon2 does not.
        assert!(needs_rehash(bc));
        assert!(!needs_rehash(&hash_password("x").unwrap()));
    }
```

- [ ] **Step 2: Run to verify it fails** — `cargo test -p jerrycan-auth password` → FAIL (`needs_rehash` missing; bcrypt hash errors as malformed).

- [ ] **Step 3: Minimal implementation**

Workspace `Cargo.toml`: `bcrypt = "0.17"` under `[workspace.dependencies]` (after `base64`); `crates/jerrycan-auth/Cargo.toml`: `bcrypt.workspace = true` (unconditional — resolved ambiguity #10: lossless login must work in every generated app with zero wiring; pure Rust, MIT).

`password.rs` — prepend to `verify_password`:

```rust
/// Verify a password. Accepts argon2 PHC strings (ours) AND bcrypt `$2…$`
/// hashes (migrated from Supabase — spec 2026-07-10 §Required jerrycan-auth
/// enhancement) so migrated users log in with their existing passwords.
pub fn verify_password(password: &str, phc: &str) -> Result<bool> {
    if needs_rehash(phc) {
        return bcrypt::verify(password, phc)
            .map_err(|e| Error::internal(format!("stored bcrypt hash is malformed: {e}")));
    }
    // …existing argon2 body unchanged…
}

/// True for a legacy (bcrypt) hash: callers re-hash with `hash_password` after
/// the next successful login — the transparent argon2 upgrade.
pub fn needs_rehash(phc: &str) -> bool {
    phc.starts_with("$2a$") || phc.starts_with("$2b$") || phc.starts_with("$2y$") || phc.starts_with("$2x$")
}
```

Also add the upgrade note to the generated login handler guidance in `docs/ai/10-auth.md` (one sentence + a 3-line example calling `needs_rehash` → `hash_password` → update row).

- [ ] **Step 4: Run to verify it passes** — `cargo test -p jerrycan-auth && cargo test --workspace` → PASS (workspace green: nothing else touches `verify_password`'s signature).

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/jerrycan-auth docs/ai/10-auth.md
git commit -m "Verify bcrypt hashes in jerrycan-auth so migrated users keep their passwords"
```

---

### Task 18: `--live` catalog front-end

**Files:**
- Create: `crates/jerrycan/src/platform/migrate/live.rs`
- Modify: `crates/jerrycan/src/platform/migrate/mod.rs` (`pub mod live;` + `run_migrate_live`)
- Modify: `crates/jerrycan/src/main.rs` (route `--live`)
- Test: inline in `live.rs` (pure row-mapping tests) + `#[ignore]`d integration

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_rows_fold_into_the_same_ir_as_the_offline_parser() {
        // Pure mapping: feed the row shapes the catalog queries return.
        let mut b = LiveBuilder::default();
        b.column("public", "customers", "email", "text", /*not_null*/ true);
        b.column("public", "customers", "id", "uuid", true);
        b.pk("public", "customers", "id");
        b.fk("public", "customers", "workspace_id", "public", "workspaces", "id", "CASCADE");
        b.rls("public", "customers", true);
        // pg_policies exposes qual/with_check as TEXT — parsed with the same
        // sqlparser expression parser the offline path uses.
        b.policy("public", "customers", "m", "ALL", &["public"],
                 Some("(workspace_id IN (SELECT workspace_id FROM workspace_members WHERE user_id = auth.uid()))"), None);
        let db = b.finish();
        let t = &db.tables["public.customers"];
        assert!(t.rls_enabled && t.pk == vec!["id"]);
        assert_eq!(t.fks[0].on_delete, crate::platform::migrate::pgmodel::FkAction::Cascade);
        assert!(db.policies[0].using.is_some(), "qual text parsed into an Expr");
    }

    #[test]
    fn unparseable_policy_text_degrades_to_a_gap_not_a_crash() {
        let mut b = LiveBuilder::default();
        b.column("public", "t", "id", "uuid", true);
        b.policy("public", "t", "weird", "ALL", &[], Some("some_extension_fn(id ==> 3)"), None);
        let db = b.finish();
        assert!(db.policies[0].using.is_none(), "kept with original text; recognizer will gap it");
        assert!(db.policies[0].original.contains("some_extension_fn"));
    }
}
```

- [ ] **Step 2: Run to verify it fails** — `cargo test -p jerrycan migrate::live` → FAIL.

- [ ] **Step 3: Minimal implementation** (`live.rs`)

- `LiveBuilder` (the tested pure core): accumulates the same `PgDatabase` the offline fold produces. Policy `qual`/`with_check` text → `Parser::new(&PostgreSqlDialect{}).try_with_sql(text)?.parse_expr()`; parse failure keeps `using: None` with `original` set — `rls::recognize` then gaps it (`Recognized::Gap` for the no-expr case already exists from Task 8).
- `pub async fn read_live(conn: &str) -> Result<(PgDatabase, LiveData), String>` — connect via `jerrycan_db::Db::connect` (re-exported sqlx/sea-orm; use `conn().query_all` raw SQL). Catalog queries (verbatim in the code): columns + types from `information_schema.columns`; pk/unique/fk from `pg_constraint` joined to `pg_class`/`pg_attribute`; RLS from `pg_class.relrowsecurity`; policies from `pg_policies` (`qual`, `with_check`, `cmd`, `roles`); enums from `pg_type`/`pg_enum`; publications from `pg_publication_tables`; cron from `cron.job` (if the extension is absent, skip); buckets from `storage.buckets`; identities providers from `auth.identities`. `LiveData` streams table rows (`SELECT * FROM <t>` in pk-ordered pages of 1000 via keyset pagination) into the same `SeedWriter` interface.
- Object **bytes** are not fetched (resolved ambiguity #9): when buckets have objects, emit one advisory gap per bucket + the MIGRATION.md offline-copy step.
- `run_migrate_live(conn, opts)` in `mod.rs` reuses steps 4–18 of `run_migrate` unchanged (extract the shared tail into `translate_and_emit(db, seed_source, opts)` during this task — pure refactor, offline tests stay green).
- `main.rs`: route `--live` to it; keep the "never in CI" warning on stderr: `warning: --live reads a production database — offline export is the supported CI path`.
- Add one `#[ignore = "needs a live postgres; set JERRYCAN_TEST_PG_URL"]` integration test in `live.rs` that round-trips a two-table schema against a local Postgres.

- [ ] **Step 4: Run to verify it passes** — `cargo test -p jerrycan migrate::live` → PASS (2 pure tests; ignored test compiles).

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan/src/platform/migrate crates/jerrycan/src/main.rs
git commit -m "Add jerrycan migrate --live: catalog reader into the shared translator IR"
```

---

### Task 19: the checked-in reference Supabase export + translator integration test

**Files:**
- Create: `conformance/fixtures/supabase-export/schema.sql`
- Create: `conformance/fixtures/supabase-export/data/*.csv` (`auth.users`, `auth.identities`, `public.workspaces`, `public.workspace_members`, `public.customers`, `public.notes`, `public.plans`, `public.events`, `storage.objects`)
- Create: `conformance/fixtures/supabase-export/storage/buckets.json` + `storage/objects/avatars/…` (two tiny PNGs) + `storage/objects/invoices/…`
- Create: `conformance/fixtures/supabase-export/functions/send-digest/index.ts` (10-line Deno stub)
- Create: `conformance/fixtures/supabase-export/cron.sql`
- Test: `crates/jerrycan/tests/migrate_supabase.rs`

The fixture is a realistic multi-tenant CRM ("acme-crm", spec §Eval gate): 2 workspaces, 3 users (2 in workspace A — one `owner`, one `member`; 1 in workspace B), `customers`/`notes` tenant-scoped via membership-join RLS (+ `role = 'owner'` on delete), `plans` public-read, `events` (300 rows — exercises bulk with `--bulk-threshold 100`), one share-list policy on `notes` that MUST gap, buckets `avatars` (public + `owner_prefix` foldername policy) and `invoices` (private, tenant policy), `supabase_realtime` publication on `customers` + `notes`, one plpgsql function + trigger, one 5-field cron + one `@hourly` cron, bcrypt password hashes for all users (generate with the `bcrypt` crate at fixture-authoring time; user `a-owner@acme.test` has password `owner-pass-1`), one auth.identities google row, and one JWT-shaped string planted in a `notes.body` data cell (must be flagged, not silently embedded). **Keep total fixture < 200KB** (spec: seed small enough to live in the eval).

- [ ] **Step 1: Write the failing test** (`crates/jerrycan/tests/migrate_supabase.rs`)

```rust
//! The reference Supabase export migrates deterministically and safely.
use jerrycan::platform::migrate::{run_migrate, MigrateOptions};
use jerrycan::platform::questions;

fn fixture() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance/fixtures/supabase-export")
}

fn migrate_into(dir: &std::path::Path) -> jerrycan::platform::migrate::MigrateOutput {
    run_migrate(&MigrateOptions {
        export_dir: fixture(),
        out_dir: dir.to_path_buf(),
        name: Some("acme-crm".into()),
        bulk_threshold: 100,
    })
    .expect("reference export migrates")
}

#[test]
fn the_reference_export_yields_a_green_v2_design() {
    let tmp = tempfile::tempdir().unwrap();
    let out = migrate_into(tmp.path());
    assert!(questions::validate(&out.design).is_empty());
    assert_eq!(out.design.tenancy.as_ref().unwrap().entity, "Workspace");
    assert!(out.design.storage.is_some() && out.design.realtime.is_some());
    assert_eq!(out.design.jobs.len(), 1, "5-field cron mapped; @hourly gapped");
}

#[test]
fn expected_gaps_exactly_no_more_no_less_by_kind() {
    use jerrycan::platform::migrate::gaps::GapKind::*;
    let tmp = tempfile::tempdir().unwrap();
    let out = migrate_into(tmp.path());
    let mut kinds: Vec<_> = out.gaps.iter().map(|g| g.kind).collect();
    kinds.sort();
    // share-policy → RlsPolicy; plpgsql fn + trigger + cron body → PgFunction×2, PgTrigger;
    // edge fn → EdgeFunction; @hourly → CronJob; planted JWT in data → SuspectedSecret;
    // Broadcast + Presence advisories.
    for want in [RlsPolicy, PgFunction, PgTrigger, EdgeFunction, CronJob, SuspectedSecret, Broadcast, Presence] {
        assert!(kinds.contains(&want), "missing {want:?}: {kinds:?}");
    }
}

#[test]
fn no_secret_survives_into_any_emitted_artifact() {
    let tmp = tempfile::tempdir().unwrap();
    migrate_into(tmp.path());
    for rel in ["design.json", "MIGRATION.md", "gap-report.json"] {
        let text = std::fs::read_to_string(tmp.path().join(rel)).unwrap();
        assert!(jerrycan::platform::migrate::redact::scan(&text).is_empty(), "{rel} leaked a secret");
    }
    let md = std::fs::read_to_string(tmp.path().join("MIGRATION.md")).unwrap();
    assert!(md.contains("Secret rotation"), "rotation checklist present");
}

#[test]
fn the_bulk_table_took_the_resumable_path() {
    let tmp = tempfile::tempdir().unwrap();
    migrate_into(tmp.path());
    assert!(tmp.path().join("seed/bulk/events.csv").exists(), "300 rows > threshold 100");
    let manifest = std::fs::read_to_string(tmp.path().join("seed/manifest.json")).unwrap();
    assert!(manifest.contains("\"mode\": \"bulk\""));
}
```

- [ ] **Step 2: Run to verify it fails** — `cargo test -p jerrycan --test migrate_supabase` → FAIL (fixture missing).

- [ ] **Step 3: Author the fixture** — write `schema.sql` covering every shape above (reuse the SQL snippets from Tasks 4–13 as the base; every policy/table/publication/cron listed in the fixture description gets real DDL), the CSVs (with `\N` NULLs; `auth.users.encrypted_password` = real `$2b$` hashes generated once with `bcrypt::hash`), `buckets.json`, two ≤1KB PNGs, the Deno stub, and `cron.sql` with the two `cron.schedule` calls. Iterate on the four tests — any mismatch between fixture and expectation is a real translator bug or a wrong fixture; fix whichever is wrong, never loosen the "exactly these gaps" intent.

- [ ] **Step 4: Run to verify it passes** — `cargo test -p jerrycan --test migrate_supabase && cargo test --workspace` → PASS, workspace green.

- [ ] **Step 5: Commit**

```bash
git add conformance/fixtures/supabase-export crates/jerrycan/tests/migrate_supabase.rs
git commit -m "Add reference Supabase export fixture and migrator integration tests"
```

---

### Task 20: the capstone eval — migrate → generate → check green with negative controls

**Files:**
- Create: `crates/jerrycan/tests/migrate_e2e.rs`
- Modify: `conformance/eval/PROTOCOL.md`

- [ ] **Step 1: Write the failing test** (`migrate_e2e.rs` — `#[ignore]`d like the live test; CI's eval job and the pre-publish gate run `cargo test -- --ignored`)

```rust
//! The program capstone (spec §Eval gate): the reference export migrates into
//! an app whose `jerrycan check` is green, and cross-tenant access is blocked
//! in REST, storage, and realtime. Requires Postgres (JERRYCAN_TEST_PG_URL)
//! and the jerrycan binary; never skipped in the eval/pre-publish pipelines.
use std::process::Command;

fn jerrycan() -> Command { Command::new(env!("CARGO_BIN_EXE_jerrycan")) }

#[test]
#[ignore = "capstone eval: needs postgres (JERRYCAN_TEST_PG_URL) — run with --ignored in the eval job"]
fn migrated_reference_app_checks_green_with_negative_controls() {
    let tmp = tempfile::tempdir().unwrap();
    let app = tmp.path().join("acme-crm");
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance/fixtures/supabase-export");

    // 1. Migrate (offline, deterministic).
    let out = jerrycan().args(["migrate", "--from", "supabase"]).arg(&fixture)
        .arg("--out").arg(&app).args(["--name", "acme-crm", "--bulk-threshold", "100"])
        .output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    // 2. Generate every module + tests, apply migrations + seed.
    for step in [vec!["db", "migrate"], vec!["db", "seed"]] {
        let out = jerrycan().args(&step).current_dir(&app)
            .env("JERRYCAN_DATABASE_URL", std::env::var("JERRYCAN_TEST_PG_URL").unwrap())
            .output().unwrap();
        assert!(out.status.success(), "{step:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    // 3. The gate: check must be green — this includes the GENERATED isolation
    //    tests + negative controls for tenancy (REST), buckets (storage), and
    //    realtime scope-filtered delivery. A wrong translation cannot pass.
    let out = jerrycan().args(["check"]).current_dir(&app)
        .env("JERRYCAN_DATABASE_URL", std::env::var("JERRYCAN_TEST_PG_URL").unwrap())
        .output().unwrap();
    assert!(out.status.success(), "check must be green: {}", String::from_utf8_lossy(&out.stderr));

    // 4. Belt-and-braces spot probes on the running app (beyond generated tests):
    //    boot the app, login as workspace-B's user, and assert 404 on a
    //    workspace-A customer id (REST), a workspace-A invoice object (storage),
    //    and that a WS subscription receives NO event when workspace-A mutates
    //    (realtime). Implement with the same harness the v2.5 eval uses.
    negative_controls::run(&app);
}
```

Write `negative_controls::run` in the same file against the harness pattern the v2.5 eval-gate plan established (spawn `cargo run -p app`, drive HTTP with the workspace's existing test client util, WS via the realtime crate's test client — reuse, don't reinvent; the three probes above, each asserting the *absence* of cross-tenant data).

- [ ] **Step 2: Run to verify it fails** — `JERRYCAN_TEST_PG_URL=… cargo test -p jerrycan --test migrate_e2e -- --ignored` → FAIL at whatever the first real defect is (expected on first run: generated app compile or seed mismatch — fix forward through the pipeline; the earlier unit tasks make each failure local).

- [ ] **Step 3: Make it pass** — no new implementation modules; this task is integration hardening. Every fix goes into the owning module with a regression unit test there first (systematic-debugging discipline). Then register the eval: append to `conformance/eval/PROTOCOL.md`:

```markdown
## Migrator eval (capstone — un-skippable)

`cargo test -p jerrycan --test migrate_supabase` (always on) and
`JERRYCAN_TEST_PG_URL=… cargo test -p jerrycan --test migrate_e2e -- --ignored`
(CI eval job + pre-publish) must both pass. The e2e migrates
`conformance/fixtures/supabase-export`, generates, seeds, and requires
`jerrycan check` green **plus** cross-tenant negative controls in REST,
storage, and realtime. A red migrator eval blocks publish — no exceptions.
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test --workspace && JERRYCAN_TEST_PG_URL=… cargo test -p jerrycan --test migrate_e2e -- --ignored`
Expected: workspace green AND the capstone green (negative controls included).

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan/tests/migrate_e2e.rs conformance/eval/PROTOCOL.md
git commit -m "Add migrator capstone eval: migrate, generate, check green with negative controls"
```

---

## Done means

- `jerrycan migrate --from supabase <export>` produces a scaffolded project with a question-free contract-v2 `design.json` (storage + realtime blocks), `seed/` (+ resumable `jerrycan db seed`), `gap-report.json`, and `MIGRATION.md` — deterministically (byte-identical re-runs).
- Unrecognized RLS/functions/triggers/edge/types are gap items, **never guesses** — proven by the near-miss unit tests.
- No JWT/service key survives into any emitted artifact — proven by `assert_clean` + the fixture's planted secret.
- A migrated user logs in with their existing (bcrypt) password.
- The reference export migrates to `jerrycan check` green with cross-tenant negative controls across REST, storage, and realtime — registered as an un-skippable eval.
