//! `jerrycan migrate --from supabase`: the deterministic translator (spec
//! 2026-07-10). Two front-ends (offline export dir, live catalogs) fold into
//! one PgDatabase IR; pure stages translate what is safe and gap-report the rest.

pub mod export;
pub mod gaps;
pub mod parse;
pub mod pgmodel;
pub mod authmap;
pub mod crud;
pub mod entities;
pub mod grouping;
pub mod live;
pub mod cronmap;
pub mod realtimemap;
pub mod redact;
pub mod rls;
pub mod seed;
pub mod migrationmd;
pub mod storagemap;
pub mod tenancy;
pub mod typemap;

use gaps::{GapItem, GapKind, Severity};
use migrationmd::SeedSummary;
use seed::{SeedColumn, SeedType, SeedWriter};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use tenancy::TableAccess;
use typemap::{MappedType, map_pg_type};

use crate::platform::design::{
    Design, Entity, FieldType, ModuleDesign, RealtimeDesign, Tenancy,
};

pub struct MigrateOptions {
    pub export_dir: PathBuf,
    pub out_dir: PathBuf,
    pub name: Option<String>,
    pub bulk_threshold: usize,
}

pub struct MigrateOutput {
    pub design: Design,
    pub gaps: Vec<GapItem>,
    pub created: Vec<String>,
    pub seed: SeedSummary,
}

fn kebab(s: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() || !trimmed.starts_with(|c: char| c.is_ascii_lowercase()) {
        format!("app-{trimmed}").trim_end_matches('-').to_string()
    } else {
        trimmed
    }
}

fn field_to_seed(ft: FieldType) -> SeedType {
    match ft {
        FieldType::Integer => SeedType::Integer,
        FieldType::Float => SeedType::Float,
        FieldType::Boolean => SeedType::Boolean,
        FieldType::Datetime => SeedType::Datetime,
        FieldType::Uuid => SeedType::Uuid,
        FieldType::Json => SeedType::Json,
        FieldType::String => SeedType::Text,
    }
}

fn seed_type_of(db: &pgmodel::PgDatabase, table_key: &str, col: &str) -> SeedType {
    let pg = db
        .tables
        .get(table_key)
        .and_then(|t| t.columns.iter().find(|c| c.name == col))
        .map(|c| c.pg_type.as_str())
        .unwrap_or("text");
    match map_pg_type(pg, &db.enums) {
        MappedType::Field { field_type, .. } => field_to_seed(field_type),
        MappedType::Unmappable { .. } => SeedType::Text,
    }
}

/// The deterministic Supabase→jerrycan pipeline (offline front-end).
pub fn run_migrate(opts: &MigrateOptions) -> Result<MigrateOutput, String> {
    let export = export::Export::open(&opts.export_dir)?;
    let stmts = parse::split_and_parse(&export.schema_sql);
    let db = pgmodel::PgDatabase::fold(&stmts);
    let providers = providers_from_export(&export);
    let edge_names = export
        .function_dirs
        .iter()
        .map(|d| {
            d.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("edge")
                .to_string()
        })
        .collect();
    let fe = FrontendInputs {
        providers,
        buckets_json: export.buckets_json.clone(),
        cron_sql: export.cron_sql.clone(),
        edge_names,
        seed: Some(&export),
    };
    emit_from_db(db, opts, fe)
}

/// Front-end-supplied inputs the shared translator/emit tail consumes. The
/// offline path fills these from the export dir; `--live` fills them from
/// catalog queries. `seed` is `None` for live (rows are not streamed in v1).
pub(crate) struct FrontendInputs<'a> {
    pub providers: Vec<String>,
    pub buckets_json: Option<String>,
    pub cron_sql: Option<String>,
    pub edge_names: Vec<String>,
    pub seed: Option<&'a export::Export>,
}

fn emit_from_db(
    db: pgmodel::PgDatabase,
    opts: &MigrateOptions,
    fe: FrontendInputs,
) -> Result<MigrateOutput, String> {
    let providers = fe.providers.clone();
    let det = tenancy::detect(&db);
    let access_map = tenancy::table_access(&db, &det);

    // Entities: every public table except the membership table (tenancy owns it).
    let mut exclude = BTreeSet::new();
    if let Some(mt) = &det.membership_table {
        exclude.insert(mt.clone());
    }
    let build = entities::build_entities_filtered(&db, &exclude);
    let mut gaps: Vec<GapItem> = build.gaps.clone();

    // Any public-table RLS policy the recognizer won't certify is agent work —
    // gap it (never guessed). The table still gets fully-guarded CRUD.
    for policy in db.policies.iter().filter(|p| p.table.starts_with("public.")) {
        if let rls::Recognized::Gap { reason } = rls::recognize(policy) {
            gaps.push(GapItem {
                kind: GapKind::RlsPolicy,
                source: format!("{} policy \"{}\"", policy.table, policy.name),
                location: format!("schema.sql:{}", policy.line),
                reason,
                original: policy.original.clone(),
                suggested: "implement as a handler guard on the owning module".into(),
                severity: Severity::Blocking,
            });
        }
    }

    let entity_by_table: BTreeMap<String, Entity> = build
        .entities
        .iter()
        .map(|(k, e)| (k.clone(), e.clone()))
        .collect();
    let table_to_entity: BTreeMap<String, String> = build
        .entities
        .iter()
        .map(|(k, e)| (k.clone(), e.name.clone()))
        .collect();

    let tenant_entity: Option<String> = det.tenant_table.as_ref().map(|tt| {
        let short = tt.strip_prefix("public.").unwrap_or(tt);
        entities::entity_name(short)
    });

    // Auth.
    let auth_out = authmap::build_auth(&det.member_roles, &providers);

    // Modules: users first, then FK-graph groups (hub = tenant table).
    let mut modules: Vec<ModuleDesign> = vec![auth_out.users_module.clone()];
    let entity_tables: Vec<String> = build.entities.iter().map(|(k, _)| k.clone()).collect();
    let mut edges = Vec::new();
    for (k, _) in &build.entities {
        if let Some(t) = db.tables.get(k) {
            for fk in &t.fks {
                if fk.ref_table != *k && entity_by_table.contains_key(&fk.ref_table) {
                    edges.push((k.clone(), fk.ref_table.clone()));
                }
            }
        }
    }
    let mut hubs = BTreeSet::new();
    if let Some(tt) = &det.tenant_table {
        hubs.insert(tt.clone());
    }
    let groups = grouping::group_modules(&entity_tables, &edges, &hubs);

    let mut modules_by_table: BTreeMap<String, String> = BTreeMap::new();
    let mut endpoint_map: Vec<(String, String)> = Vec::new();
    for (mod_name, tables) in groups {
        let mut m_entities = Vec::new();
        let mut m_endpoints = Vec::new();
        for (i, tk) in tables.iter().enumerate() {
            let Some(entity) = entity_by_table.get(tk) else {
                continue;
            };
            m_entities.push(entity.clone());
            modules_by_table.insert(tk.clone(), mod_name.clone());
            let short = tk.strip_prefix("public.").unwrap_or(tk);
            let prefix = if i == 0 {
                String::new()
            } else {
                format!("/{short}")
            };
            let access = access_map.get(tk).unwrap_or(&TableAccess::NoRls);
            let mut eps = crud::endpoints_for(&entity.name, access);
            // questions.rs forbids public on a tenant-owned entity: downgrade + advise.
            let tenant_owned = tenant_entity
                .as_ref()
                .is_some_and(|te| entity.belongs_to.iter().any(|b| &b.entity == te));
            if tenant_owned && eps.iter().any(|e| e.public) {
                crud::strip_public(&mut eps);
                gaps.push(GapItem {
                    kind: GapKind::RlsPolicy,
                    source: format!("{tk} public-read policy"),
                    location: "schema.sql".into(),
                    reason: "public read on a tenant-owned entity would leak across tenants".into(),
                    original: String::new(),
                    suggested: "downgraded to auth-required; re-model as a public non-tenant entity if truly public".into(),
                    severity: Severity::Advisory,
                });
            }
            crud::prefix_paths(&mut eps, &prefix);
            m_endpoints.extend(eps);
            let new_path = if i == 0 {
                format!("/{mod_name}")
            } else {
                format!("/{mod_name}/{short}")
            };
            endpoint_map.push((format!("/rest/v1/{short}"), new_path));
        }
        modules.push(ModuleDesign {
            name: mod_name,
            mount: None,
            description: None,
            entities: m_entities,
            endpoints: m_endpoints,
            subroutes: vec![],
            dependencies: vec![],
        });
    }

    // Dependencies: auth (+oauth) + db (tenancy/realtime/storage all need it).
    let mut dep_set: BTreeSet<String> = auth_out.dependencies.iter().cloned().collect();
    dep_set.insert("db".into());
    let dependencies: Vec<String> = dep_set.into_iter().collect();

    let tenancy = tenant_entity.clone().map(|entity| Tenancy {
        entity,
        member_roles: det.member_roles.clone(),
    });

    // Realtime.
    let realtime = if db.publications.contains_key("supabase_realtime") {
        let rt = realtimemap::build_realtime(&db.publications, &table_to_entity);
        gaps.extend(rt.gaps);
        (!rt.changes.is_empty()).then_some(RealtimeDesign {
            changes: rt.changes,
            broadcast: vec![],
            presence: vec![],
        })
    } else {
        None
    };

    // Storage.
    let storage = if let Some(json) = &fe.buckets_json {
        let so = storagemap::build_storage(json, &db, "User")?;
        let design = so.to_design();
        gaps.extend(so.gaps);
        design
    } else {
        None
    };

    // Jobs (cron).
    let mut jobs = Vec::new();
    if let Some(cron) = &fe.cron_sql {
        let jo = cronmap::build_jobs(cron);
        jobs = jo.jobs;
        gaps.extend(jo.gaps);
    }

    // Everything the translator will not guess → the gap queue.
    for f in &db.functions {
        gaps.push(GapItem {
            kind: GapKind::PgFunction,
            source: f.name.clone(),
            location: format!("schema.sql:{}", f.line),
            reason: "plpgsql function bodies are ported by the agent".into(),
            original: f.sql.clone(),
            suggested: "port to a Rust handler or job task".into(),
            severity: Severity::Advisory,
        });
    }
    for t in &db.triggers {
        gaps.push(GapItem {
            kind: GapKind::PgTrigger,
            source: t.name.clone(),
            location: format!("schema.sql:{}", t.line),
            reason: "triggers are separate work items — re-express as handler/job logic".into(),
            original: t.sql.clone(),
            suggested: "implement the trigger's effect in the owning handler".into(),
            severity: Severity::Advisory,
        });
    }
    for name in &fe.edge_names {
        gaps.push(GapItem {
            kind: GapKind::EdgeFunction,
            source: format!("edge function `{name}`"),
            location: format!("functions/{name}"),
            reason: "Edge Function (Deno) bodies are ported by the agent".into(),
            original: String::new(),
            suggested: "re-implement as a jerrycan handler or job task".into(),
            severity: Severity::Blocking,
        });
    }
    for (sql, line) in &db.unparsed {
        gaps.push(GapItem {
            kind: GapKind::PgFunction,
            source: "unparsed statement".into(),
            location: format!("schema.sql:{line}"),
            reason: "statement not understood by the translator — review".into(),
            original: sql.clone(),
            suggested: "translate by hand if it carries behavior; ignore if it is noise".into(),
            severity: Severity::Advisory,
        });
    }

    let name = opts.name.clone().unwrap_or_else(|| {
        kebab(
            opts.out_dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("app"),
        )
    });

    let design = Design {
        name,
        contract_version: 2,
        description: None,
        auth: Some(auth_out.auth.clone()),
        dependencies,
        tenancy,
        jobs,
        storage,
        realtime,
        modules,
    };

    // Same gate `jerrycan new` runs — the translator must produce a valid design.
    let questions = crate::platform::questions::validate(&design);
    if !questions.is_empty() {
        let list = questions
            .iter()
            .map(|q| format!("{} — {}", q.id, q.question))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(format!("translator bug — produced an invalid design: {list}"));
    }

    // Scaffold + derived schema (the normal generate handoff).
    let mut created = crate::platform::scaffold::scaffold(&opts.out_dir, &design)?;
    if let Some(rel) = crate::platform::schema::write_schema(&opts.out_dir, &design)? {
        created.push(rel);
    }

    // Seed pass (streamed). Offline streams the export CSVs; live (v1) does not
    // stream rows — it emits an empty seed + a blocking gap directing the offline
    // export for data (resolved ambiguity #9: bytes/rows are an offline step).
    let seed_summary = if let Some(export) = fe.seed {
        write_seed(
            &opts.out_dir,
            export,
            &db,
            &entity_by_table,
            opts.bulk_threshold,
            &mut gaps,
        )?
    } else {
        SeedWriter::new(&opts.out_dir, opts.bulk_threshold, 500).finish()?;
        gaps.push(GapItem {
            kind: GapKind::UnmappedType,
            source: "live-mode data seed".into(),
            location: "(--live)".into(),
            reason: "`--live` translates the schema only; table rows and object bytes are not streamed".into(),
            original: String::new(),
            suggested: "produce an offline export and run the offline migration to generate seed/".into(),
            severity: Severity::Blocking,
        });
        SeedSummary {
            tables: 0,
            bulk_tables: 0,
            rows: 0,
        }
    };

    // Gap report + MIGRATION.md.
    let mut sorted_gaps = gaps.clone();
    let report = gaps::render_gap_report(&mut sorted_gaps);
    std::fs::write(opts.out_dir.join("gap-report.json"), &report).map_err(|e| e.to_string())?;
    created.push("gap-report.json".into());

    let md = migrationmd::render(&design, &sorted_gaps, &seed_summary, &providers, &endpoint_map);
    std::fs::write(opts.out_dir.join("MIGRATION.md"), &md).map_err(|e| e.to_string())?;
    created.push("MIGRATION.md".into());

    // Hard gate: no secret survives into config/design/report/migration doc.
    let clean_targets: Vec<PathBuf> = ["design.json", "gap-report.json", "MIGRATION.md", "jerrycan.toml"]
        .iter()
        .map(|r| opts.out_dir.join(r))
        .collect();
    redact::assert_clean(&clean_targets)?;

    created.sort();
    created.dedup();
    Ok(MigrateOutput {
        design,
        gaps: sorted_gaps,
        created,
        seed: seed_summary,
    })
}

/// `--live`: read Postgres catalogs into the shared IR, then translate + emit.
/// Never streams table rows (the seed is an offline step). Never used in CI.
pub async fn run_migrate_live(conn: &str, opts: &MigrateOptions) -> Result<MigrateOutput, String> {
    let read = live::read_live(conn).await?;
    let fe = FrontendInputs {
        providers: read.providers,
        buckets_json: read.buckets_json,
        cron_sql: None,
        edge_names: vec![],
        seed: None,
    };
    emit_from_db(read.db, opts, fe)
}

fn providers_from_export(export: &export::Export) -> Vec<String> {
    let Some((_, _, path)) = export
        .data_files
        .iter()
        .find(|(schema, table, _)| schema == "auth" && table == "identities")
    else {
        return Vec::new();
    };
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let reader = seed::CsvReader::new(file);
    let Some(idx) = reader.headers().iter().position(|h| h == "provider") else {
        return Vec::new();
    };
    authmap::providers_from_identities(reader.filter_map(|r| r.ok()), idx)
}

#[allow(clippy::too_many_arguments)]
fn write_seed(
    out_dir: &Path,
    export: &export::Export,
    db: &pgmodel::PgDatabase,
    entity_by_table: &BTreeMap<String, Entity>,
    bulk_threshold: usize,
    gaps: &mut Vec<GapItem>,
) -> Result<SeedSummary, String> {
    let mut writer = SeedWriter::new(out_dir, bulk_threshold, 500);
    let mut tables = 0usize;
    let mut bulk_tables = 0usize;
    let mut total_rows = 0usize;

    for (schema, table, path) in &export.data_files {
        let key = format!("{schema}.{table}");
        // Determine target table + columns + optional projection.
        let (target, columns, projection): (String, Vec<SeedColumn>, Option<Vec<usize>>) =
            if schema == "public" && entity_by_table.contains_key(&key) {
                let headers = read_headers(path)?;
                let columns = headers
                    .iter()
                    .map(|h| SeedColumn {
                        name: h.clone(),
                        ty: seed_type_of(db, &key, h),
                    })
                    .collect();
                (table.clone(), columns, None)
            } else if schema == "auth" && table == "users" {
                let headers = read_headers(path)?;
                let mapping = authmap::user_seed_mapping();
                let mut cols = Vec::new();
                let mut idxs = Vec::new();
                for (src, dst) in mapping {
                    if let Some(i) = headers.iter().position(|h| h == src) {
                        let ty = if *dst == "id" {
                            SeedType::Uuid
                        } else {
                            SeedType::Text
                        };
                        cols.push(SeedColumn {
                            name: (*dst).to_string(),
                            ty,
                        });
                        idxs.push(i);
                    }
                }
                ("users".to_string(), cols, Some(idxs))
            } else {
                continue;
            };

        // Pass 1: count rows + flag suspected secrets (advisory, never redacted).
        let mut count = 0usize;
        let mut flagged: BTreeSet<usize> = BTreeSet::new();
        for row in seed::CsvReader::new(std::fs::File::open(path).map_err(|e| e.to_string())?) {
            let row = row?;
            count += 1;
            for (ci, cell) in row.iter().enumerate() {
                if flagged.contains(&ci) {
                    continue;
                }
                if let Some(v) = cell
                    && let Some(hit) = redact::scan(v).into_iter().next()
                {
                    flagged.insert(ci);
                    gaps.push(GapItem {
                        kind: GapKind::SuspectedSecret,
                        source: format!("{key} column #{ci}"),
                        location: path.display().to_string(),
                        reason: "a data cell looks like a secret (jwt/key/conn string) — verify before exposing".into(),
                        original: hit.preview,
                        suggested: "confirm it is legitimate user data; rotate if it is a leaked key".into(),
                        severity: Severity::Advisory,
                    });
                }
            }
        }

        // Pass 2: stream to the seed writer.
        let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
        let reader = seed::CsvReader::new(file);
        if let Some(idxs) = projection {
            let rows = reader.filter_map(|r| r.ok()).map(move |row| {
                idxs.iter()
                    .map(|&i| row.get(i).cloned().flatten())
                    .collect::<Vec<_>>()
            });
            writer.write_table(&target, &columns, rows, count)?;
        } else {
            let rows = reader.filter_map(|r| r.ok());
            writer.write_table(&target, &columns, rows, count)?;
        }

        tables += 1;
        total_rows += count;
        if count > bulk_threshold {
            bulk_tables += 1;
        }
    }

    writer.finish()?;
    Ok(SeedSummary {
        tables,
        bulk_tables,
        rows: total_rows,
    })
}

fn read_headers(path: &Path) -> Result<Vec<String>, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    Ok(seed::CsvReader::new(file).headers().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mini_export(root: &std::path::Path) {
        std::fs::write(
            root.join("schema.sql"),
            r#"
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
"#,
        )
        .unwrap();
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
        })
        .expect("pipeline runs");
        assert_eq!(out.design.contract_version, 2);
        assert!(
            crate::platform::questions::validate(&out.design).is_empty(),
            "translator output must be question-free: {:?}",
            crate::platform::questions::validate(&out.design)
        );
        assert_eq!(out.design.tenancy.as_ref().unwrap().entity, "Workspace");
        for rel in ["design.json", "gap-report.json", "MIGRATION.md", "seed/manifest.json"] {
            assert!(out_dir.join(rel).exists(), "{rel}");
        }
        let gaps = std::fs::read_to_string(out_dir.join("gap-report.json")).unwrap();
        assert!(gaps.contains("pg_function") && gaps.contains("audit"));

        let out_dir2 = tmp.path().join("app2");
        run_migrate(&MigrateOptions {
            export_dir,
            out_dir: out_dir2.clone(),
            name: Some("acme".into()),
            bulk_threshold: 5000,
        })
        .unwrap();
        for rel in ["design.json", "gap-report.json", "MIGRATION.md"] {
            assert_eq!(
                std::fs::read(out_dir.join(rel)).unwrap(),
                std::fs::read(out_dir2.join(rel)).unwrap(),
                "{rel} deterministic"
            );
        }
    }

    #[test]
    fn migration_md_carries_the_rotation_checklist_and_endpoint_mapping() {
        let tmp = tempfile::tempdir().unwrap();
        let export_dir = tmp.path().join("export");
        std::fs::create_dir_all(&export_dir).unwrap();
        mini_export(&export_dir);
        let out_dir = tmp.path().join("app");
        run_migrate(&MigrateOptions {
            export_dir,
            out_dir: out_dir.clone(),
            name: Some("acme".into()),
            bulk_threshold: 5000,
        })
        .unwrap();
        let md = std::fs::read_to_string(out_dir.join("MIGRATION.md")).unwrap();
        assert!(md.contains("## Secret rotation"), "rotation checklist present");
        assert!(
            md.contains("/rest/v1/customers") && md.contains("/customers"),
            "endpoint mapping present"
        );
        let migrate_at = md.find("jerrycan db migrate").expect("db migrate step");
        let seed_at = md.find("jerrycan db seed").expect("db seed step");
        assert!(migrate_at < seed_at, "migrate then seed, in order");
    }
}
