//! The deterministic mounting regenerator: app/src/main.rs (whole file),
//! workspace members, app route-deps. Sorted, idempotent, byte-stable —
//! JL0003 compares against exactly this output.

use super::design::Design;
use super::genroute::crate_ident;
use super::templates::set_features;
use std::fs;
use std::path::Path;

/// Rewrite the workspace's `jerrycan = { … }` dependency line so its facade
/// features (`db`/`validate`) match the design's mode. Leaves other lines and
/// the path/version form untouched.
fn sync_facade_features(ws: &str, design: &Design) -> String {
    let features = design.facade_features();
    ws.lines()
        .map(|line| {
            if line.trim_start().starts_with("jerrycan = {") {
                set_features(line, &features)
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + if ws.ends_with('\n') { "\n" } else { "" }
}

/// The ordered `.extend(...)` block for `main`. Order is load-bearing: Auth
/// FIRST so the session/role guards resolve their extension, then Observe, then
/// jobs (needs the db, registered before `.extend(db)` moves it), then db, then
/// validate. Memory/db/validate-only modes keep their exact prior bytes
/// (auth/observe/jobs absent → no extra lines, db before validate as before).
fn extension_block(design: &Design) -> String {
    let mut block = String::new();
    if design.wants_auth() {
        block.push_str("        .extend(jerrycan::auth::Auth::from_env()?)\n");
    }
    if design.wants_observe() {
        block.push_str("        .extend(jerrycan::observe::Observe::new())\n");
    }
    // Storage provides itself (like Db) so handlers resolve Dep<Storage>; the
    // backend + signing key come from env (JERRYCAN_STORAGE / JERRYCAN_SECRET).
    if design.wants_storage() {
        block.push_str("        .extend(jerrycan::storage::Storage::from_env()?)\n");
    }
    // Jobs need the db: register the wired `Jobs` extension (the generated
    // `crates/jobs` crate's `jobs(db)` fn) with a CLONE, before `.extend(db)`
    // below moves `db`. wants_jobs implies wants_db (questions.rs enforces it).
    if design.wants_jobs() {
        block.push_str("        .extend(jobs::jobs(db.clone()))\n");
    }
    // Realtime needs the db (Changes + DDL reconcile): register the wired
    // extension (the generated `crates/realtime` crate's `realtime(db)` fn) with
    // a CLONE, before `.extend(db)` below moves it. wants_realtime implies
    // wants_db (questions.rs enforces it).
    if design.wants_realtime() {
        block.push_str("        .extend(realtime::realtime(db.clone()))\n");
    }
    if design.wants_db() {
        block.push_str("        .extend(db)\n");
    }
    if design.wants_validate() {
        block.push_str("        .extend(jerrycan::validate::OpenApi::new(include_str!(\"../../../openapi.json\")))\n");
    }
    block
}

/// Join the app-level base prefix onto a mount path. `base` is `""` or `/v1`
/// (no trailing slash); `mount` starts with `/`. A root module mount (`/`) under
/// a base yields the base itself (`/v1`, never `/v1/`); an empty base is a no-op.
fn join_mount(base: &str, mount: &str) -> String {
    if base.is_empty() {
        mount.to_string()
    } else if mount == "/" {
        base.to_string()
    } else {
        format!("{base}{mount}")
    }
}

/// The complete, tool-owned app/src/main.rs for this design.
pub fn expected_main(design: &Design) -> String {
    let mut modules: Vec<_> = design.modules.iter().collect();
    modules.sort_by(|a, b| a.name.cmp(&b.name));

    // App-level base prefix (issue #16): applied ONCE to every module and bucket
    // mount here at assembly. Health/metrics come from the Observe extension, not
    // these mounts, so they stay unprefixed.
    let base = design.base_prefix();
    let mut mounts = String::new();
    for dep in design.dependencies.iter().filter(|d| {
        !matches!(
            d.as_str(),
            "db" | "validate" | "auth" | "observe" | "storage" | "realtime"
        )
    }) {
        mounts.push_str(&format!(
            "        // app dependency `{dep}`: provide here once its extension lands\n"
        ));
    }
    for m in &modules {
        mounts.push_str(&format!(
            "        .mount(\"{}\", {}::module())\n",
            join_mount(base, &m.effective_mount()),
            crate_ident(&m.name)
        ));
    }
    // Buckets mount under the storage base path (`/storage/<name>` by default),
    // AFTER the module mounts, sorted by name (matches storagegen's sorted_buckets
    // order). The prefix keeps buckets clear of module route mounts (a `media`
    // bucket no longer shadows a `/media` module). The block, not a dependency, is
    // the gate; `storage` in `dependencies` stays a reserved (un-stubbed) name.
    if let Some(ref storage) = design.storage {
        let storage_base = storage.effective_base_path();
        let mut buckets: Vec<_> = storage.buckets.iter().collect();
        buckets.sort_by(|a, b| a.name.cmp(&b.name));
        for b in buckets {
            // Bucket mount = app base + storage base + bucket name.
            let bucket_mount = join_mount(base, &format!("{storage_base}/{}", b.name));
            mounts.push_str(&format!(
                "        .mount(\"{bucket_mount}\", storage::{}::module())\n",
                b.name.replace('-', "_")
            ));
        }
    }
    let extensions = extension_block(design);
    // Tenancy registers the membership-checked `Tenant` guard app-wide (after the
    // extensions it depends on — Auth + Db — and before the modules that consume
    // it via `Dep<shared::Tenant>`).
    let tenant_dep = if design.tenancy.is_some() {
        "        .provide_dep(shared::tenant)\n"
    } else {
        ""
    };

    // observe initializes logging before the App is built; db needs a module
    // decl + a connect/migrate preamble inside main. Both are absent otherwise.
    let logging = if design.wants_observe() {
        "    jerrycan::observe::init_logging();\n"
    } else {
        ""
    };
    let migrations_mod = if design.wants_db() {
        "mod migrations;\n\n"
    } else {
        ""
    };
    let db_preamble = if design.wants_db() {
        "    let db = jerrycan::db::Db::from_env().await?;\n    db.migrate(migrations::MIGRATIONS).await?;\n"
    } else {
        ""
    };
    // Jobs need their own tables: run JOBS_MIGRATIONS right after the app
    // migrations (both over the same `db`, before it is moved into the extension
    // block). Absent unless the design declares jobs.
    let jobs_migrations = if design.wants_jobs() {
        "    db.migrate(jerrycan::jobs::JOBS_MIGRATIONS).await?;\n"
    } else {
        ""
    };
    // Storage needs its metadata table: run STORAGE_MIGRATIONS right after the
    // app (and jobs) migrations, over the same `db`, before the move.
    let storage_migrations = if design.wants_storage() {
        "    db.migrate(jerrycan::storage::STORAGE_MIGRATIONS).await?;\n"
    } else {
        ""
    };

    format!(
        "//! GENERATED by jerrycan — do not hand-edit; `jerrycan generate` rewrites this file.\nuse jerrycan::prelude::*;\n\n{migrations_mod}#[jerrycan::main]\nasync fn main() -> Result<()> {{\n{logging}{db_preamble}{jobs_migrations}{storage_migrations}    App::new()\n{extensions}{tenant_dep}{mounts}        .serve()\n        .await\n}}\n"
    )
}

/// One scanned migration: owning module and file stem (twin existence verified).
pub(crate) struct ScannedMigration {
    pub(crate) module: String,
    pub(crate) file_stem: String,
}

/// Scan `crates/routes/*/migrations/sqlite/*.sql` for every module-owned
/// migration, sorted by module name then filename, requiring each one's
/// postgres twin to exist (missing → loud error). Shared by the aggregated
/// `migrations.rs` generator and the CLI's runtime loader.
pub(crate) fn scan_migrations(app_root: &Path) -> Result<Vec<ScannedMigration>, String> {
    let routes = app_root.join("crates/routes");
    let mut modules: Vec<String> = Vec::new();
    if let Ok(entries) = fs::read_dir(&routes) {
        for entry in entries.flatten() {
            if entry.path().join("migrations/sqlite").is_dir() {
                modules.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
    }
    modules.sort();

    let mut out = Vec::new();
    for module in modules {
        let sqlite_dir = routes.join(&module).join("migrations/sqlite");
        let mut files: Vec<String> = fs::read_dir(&sqlite_dir)
            .map_err(|e| format!("read {}: {e}", sqlite_dir.display()))?
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                if p.extension().is_some_and(|x| x == "sql") {
                    p.file_name().map(|n| n.to_string_lossy().into_owned())
                } else {
                    None
                }
            })
            .collect();
        files.sort();
        for file in files {
            let postgres_path = routes.join(&module).join("migrations/postgres").join(&file);
            if !postgres_path.exists() {
                return Err(format!(
                    "migration `{module}/migrations/sqlite/{file}` has no postgres twin at {} — both dialects are required",
                    postgres_path.display()
                ));
            }
            let file_stem = file.trim_end_matches(".sql").to_string();
            out.push(ScannedMigration {
                module: module.clone(),
                file_stem,
            });
        }
    }
    Ok(out)
}

/// The tool-owned `app/src/migrations.rs` aggregating module-owned migrations,
/// or None when the design has no `db` dependency.
pub fn expected_migrations_rs(app_root: &Path, design: &Design) -> Result<Option<String>, String> {
    if !design.wants_db() {
        return Ok(None);
    }
    let scanned = scan_migrations(app_root)?;
    let mut entries = String::new();
    for m in &scanned {
        let module_snake = m.module.replace('-', "_");
        entries.push_str(&format!(
            "    Migration {{\n        name: \"{module_snake}_{stem}\",\n        sqlite: include_str!(\"../../routes/{module}/migrations/sqlite/{stem}.sql\"),\n        postgres: include_str!(\"../../routes/{module}/migrations/postgres/{stem}.sql\"),\n    }},\n",
            stem = m.file_stem,
            module = m.module,
        ));
    }
    Ok(Some(format!(
        "//! GENERATED by jerrycan — aggregates module-owned migrations; do not hand-edit.\nuse jerrycan::db::Migration;\n\npub const MIGRATIONS: &[Migration] = &[\n{entries}];\n"
    )))
}

/// Load every module-owned migration's contents (the SAME scan as the
/// aggregated `migrations.rs`, twin-required, module-sorted). Consumed by
/// `jerrycan db migrate` to apply migrations from disk at runtime.
pub fn collect_migrations(app_root: &Path) -> Result<Vec<crate::db::OwnedMigration>, String> {
    let routes = app_root.join("crates/routes");
    let mut out = Vec::new();
    for m in scan_migrations(app_root)? {
        let module_snake = m.module.replace('-', "_");
        let sqlite_path = routes
            .join(&m.module)
            .join(format!("migrations/sqlite/{}.sql", m.file_stem));
        let postgres_path = routes
            .join(&m.module)
            .join(format!("migrations/postgres/{}.sql", m.file_stem));
        let sqlite = fs::read_to_string(&sqlite_path)
            .map_err(|e| format!("read {}: {e}", sqlite_path.display()))?;
        let postgres = fs::read_to_string(&postgres_path)
            .map_err(|e| format!("read {}: {e}", postgres_path.display()))?;
        out.push(crate::db::OwnedMigration {
            name: format!("{module_snake}_{}", m.file_stem),
            sqlite,
            postgres,
        });
    }
    Ok(out)
}

/// Replace the lines between marker lines (markers stay). Fails loud if markers vanished.
fn splice(content: &str, begin: &str, end: &str, replacement: &str) -> Result<String, String> {
    let b = content.find(begin).ok_or_else(|| {
        format!("marker `{begin}` missing — file was hand-edited; restore it or re-scaffold")
    })?;
    let line_end = content[b..]
        .find('\n')
        .map(|i| b + i + 1)
        .unwrap_or(content.len());
    let e = content.find(end).ok_or_else(|| {
        format!("marker `{end}` missing — file was hand-edited; restore it or re-scaffold")
    })?;
    if e < line_end {
        return Err(format!("marker `{end}` precedes `{begin}`"));
    }
    let e_line_start = content[..e].rfind('\n').map(|i| i + 1).unwrap_or(0);
    Ok(format!(
        "{}{}{}",
        &content[..line_end],
        replacement,
        &content[e_line_start..]
    ))
}

/// Regenerate every generator-owned mounting surface. Returns modified files.
pub fn regenerate(app_root: &Path, design: &Design) -> Result<Vec<String>, String> {
    let mut modules: Vec<_> = design.modules.iter().collect();
    modules.sort_by(|a, b| a.name.cmp(&b.name));
    let mut modified = Vec::new();

    // 1. app/src/main.rs — whole file.
    let main_path = app_root.join("crates/app/src/main.rs");
    fs::create_dir_all(main_path.parent().expect("parent")).map_err(|e| e.to_string())?;
    fs::write(&main_path, expected_main(design)).map_err(|e| e.to_string())?;
    modified.push("crates/app/src/main.rs".to_string());

    // 1b. app/src/migrations.rs — aggregated module migrations (db mode only).
    let migrations_path = app_root.join("crates/app/src/migrations.rs");
    match expected_migrations_rs(app_root, design)? {
        Some(content) => {
            fs::write(&migrations_path, content).map_err(|e| e.to_string())?;
            modified.push("crates/app/src/migrations.rs".to_string());
        }
        None => {
            // Memory mode: remove a stale migrations.rs if a prior db mode left one.
            if migrations_path.exists() {
                fs::remove_file(&migrations_path).map_err(|e| e.to_string())?;
                modified.push("crates/app/src/migrations.rs".to_string());
            }
        }
    }

    // 1c. openapi.json — tool-owned, emitted in every mode (the validate
    // extension includes it; harmless otherwise).
    let openapi_path = app_root.join("openapi.json");
    fs::write(&openapi_path, super::openapi::document_json(design)).map_err(|e| e.to_string())?;
    modified.push("openapi.json".to_string());

    // 1d. The top-level jobs crate (db + the `Jobs` wiring). Written when the
    // design declares jobs; removed if a prior design declared jobs and no longer
    // does (so a stale `crates/jobs` can't break the workspace build).
    let jobs_dir = app_root.join("crates/jobs");
    if design.wants_jobs() {
        modified.extend(super::jobsgen::write_jobs(app_root, design)?);
    } else if jobs_dir.exists() {
        fs::remove_dir_all(&jobs_dir).map_err(|e| e.to_string())?;
        modified.push("crates/jobs".to_string());
    }

    // 1e. The generated storage crate (bucket modules + tests). Written when
    // the design declares buckets; removed when a prior design declared them
    // and no longer does (a stale crates/storage would break the workspace).
    let storage_dir = app_root.join("crates/storage");
    if design.wants_storage() {
        modified.extend(super::storagegen::write_storage(app_root, design)?);
    } else if storage_dir.exists() {
        fs::remove_dir_all(&storage_dir).map_err(|e| e.to_string())?;
        modified.push("crates/storage".to_string());
    }

    // 1f. The generated realtime crate (channel wiring + acceptance tests).
    // Written when the design declares realtime; removed when a prior design
    // declared it and no longer does (a stale crates/realtime would break the
    // workspace build).
    let realtime_dir = app_root.join("crates/realtime");
    if design.wants_realtime() {
        modified.extend(super::realtimegen::write_realtime(app_root, design)?);
    } else if realtime_dir.exists() {
        fs::remove_dir_all(&realtime_dir).map_err(|e| e.to_string())?;
        modified.push("crates/realtime".to_string());
    }

    // 2. workspace members + facade features (kept in sync with the mode). The
    // jobs crate joins the members list (after the route crates) when present.
    let ws_path = app_root.join("Cargo.toml");
    let ws =
        fs::read_to_string(&ws_path).map_err(|e| format!("read {}: {e}", ws_path.display()))?;
    let mut members: String = modules
        .iter()
        .map(|m| format!("    \"crates/routes/{}\",\n", m.name))
        .collect();
    if design.wants_jobs() {
        members.push_str("    \"crates/jobs\",\n");
    }
    if design.wants_storage() {
        members.push_str("    \"crates/storage\",\n");
    }
    if design.wants_realtime() {
        members.push_str("    \"crates/realtime\",\n");
    }
    let ws2 = splice(
        &ws,
        "# jerrycan:members:begin",
        "# jerrycan:members:end",
        &members,
    )?;
    let ws3 = sync_facade_features(&ws2, design);
    if ws3 != ws {
        fs::write(&ws_path, &ws3).map_err(|e| e.to_string())?;
        modified.push("Cargo.toml".to_string());
    }

    // 3. app route-deps.
    let app_cargo_path = app_root.join("crates/app/Cargo.toml");
    let ac = fs::read_to_string(&app_cargo_path)
        .map_err(|e| format!("read {}: {e}", app_cargo_path.display()))?;
    let mut deps: String = modules
        .iter()
        .map(|m| format!("route-{} = {{ path = \"../routes/{}\" }}\n", m.name, m.name))
        .collect();
    if design.wants_jobs() {
        // main.rs references `jobs::jobs(db)`, so app depends on the jobs crate.
        deps.push_str("jobs = { path = \"../jobs\" }\n");
    }
    if design.wants_storage() {
        // main.rs references `storage::<bucket>::module()`.
        deps.push_str("storage = { path = \"../storage\" }\n");
    }
    if design.wants_realtime() {
        // main.rs references `realtime::realtime(db)`.
        deps.push_str("realtime = { path = \"../realtime\" }\n");
    }
    let ac2 = splice(
        &ac,
        "# jerrycan:route-deps:begin",
        "# jerrycan:route-deps:end",
        &deps,
    )?;
    if ac2 != ac {
        fs::write(&app_cargo_path, &ac2).map_err(|e| e.to_string())?;
        modified.push("crates/app/Cargo.toml".to_string());
    }

    Ok(modified)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal db+jobs design exercising the jobs wiring in `expected_main`
    /// without depending on a frozen fixture's shape.
    fn jobs_design() -> Design {
        serde_json::from_str(
            r#"{
                "name": "jobs-app", "contract_version": 1,
                "dependencies": ["db"],
                "jobs": [{ "name": "expire_trials", "schedule": "0 * * * *", "queue": "billing" }],
                "modules": [{ "name": "things",
                    "endpoints": [{ "operation_id": "list_things", "method": "GET", "path": "/",
                        "success": { "status": 200 } }] }]
            }"#,
        )
        .unwrap()
    }

    /// A jobs design wires the `Jobs` extension and runs JOBS_MIGRATIONS in
    /// main.rs. The extension is registered with a db CLONE before `.extend(db)`
    /// moves it (the worker needs the live db), and the jobs tables migrate right
    /// after the app migrations (same `db`, before the move).
    #[test]
    fn expected_main_wires_jobs_extension_and_migrations() {
        let main = expected_main(&jobs_design());
        // JOBS_MIGRATIONS run after the aggregated app migrations, before App::new.
        let app_mig = main.find("db.migrate(migrations::MIGRATIONS)").unwrap();
        let jobs_mig = main
            .find("db.migrate(jerrycan::jobs::JOBS_MIGRATIONS)")
            .unwrap();
        let app_new = main.find("App::new()").unwrap();
        assert!(
            app_mig < jobs_mig && jobs_mig < app_new,
            "jobs migrations run after app migrations, before App::new: {main}"
        );
        // The jobs extension is registered with a clone, BEFORE `.extend(db)` (which
        // moves db). The worker needs the live db, so jobs must precede the move.
        let jobs_ext = main.find(".extend(jobs::jobs(db.clone()))").unwrap();
        let db_ext = main.find(".extend(db)\n").unwrap();
        assert!(
            jobs_ext < db_ext,
            "jobs extension (db.clone()) must come before `.extend(db)` moves db: {main}"
        );
    }

    /// A design with NO jobs is byte-for-byte unchanged: no jobs extension, no
    /// JOBS_MIGRATIONS line. (Guards the wants_jobs gating.)
    #[test]
    fn expected_main_without_jobs_has_no_jobs_wiring() {
        let mut d = jobs_design();
        d.jobs.clear();
        let main = expected_main(&d);
        assert!(!main.contains("jobs::jobs"), "no jobs extension: {main}");
        assert!(
            !main.contains("JOBS_MIGRATIONS"),
            "no jobs migrations: {main}"
        );
    }

    fn realtime_design() -> Design {
        serde_json::from_str(crate::platform::design::tests::V2_REALTIME).unwrap()
    }

    #[test]
    fn expected_main_wires_realtime_extension_before_db_move() {
        let main = expected_main(&realtime_design());
        let rt = main
            .find(".extend(realtime::realtime(db.clone()))")
            .unwrap();
        let db = main.find(".extend(db)\n").unwrap();
        assert!(
            rt < db,
            "realtime registers with a db CLONE before .extend(db) moves it: {main}"
        );
        // No stub comment for the reserved name.
        assert!(!main.contains("app dependency `realtime`"), "{main}");
    }

    #[test]
    fn expected_main_without_realtime_has_no_realtime_wiring() {
        let mut d = realtime_design();
        d.realtime = None;
        assert!(!expected_main(&d).contains("realtime::realtime"));
    }

    fn storage_design() -> Design {
        serde_json::from_str(crate::platform::design::tests::V2_STORAGE).unwrap()
    }

    /// A storage design wires the Storage extension, runs STORAGE_MIGRATIONS
    /// after the app migrations, and mounts each bucket (sorted) AFTER the
    /// module mounts. Order is load-bearing: the extension precedes
    /// `.extend(db)`; migrations precede App::new().
    #[test]
    fn expected_main_wires_storage_extension_migrations_and_mounts() {
        let main = expected_main(&storage_design());
        let ext = main
            .find(".extend(jerrycan::storage::Storage::from_env()?)")
            .unwrap();
        let db_ext = main.find(".extend(db)\n").unwrap();
        assert!(ext < db_ext, "storage extension before the db move: {main}");
        let app_mig = main.find("db.migrate(migrations::MIGRATIONS)").unwrap();
        let st_mig = main
            .find("db.migrate(jerrycan::storage::STORAGE_MIGRATIONS)")
            .unwrap();
        let app_new = main.find("App::new()").unwrap();
        assert!(
            app_mig < st_mig && st_mig < app_new,
            "storage migrations after app migrations, before App::new: {main}"
        );
        let module_mount = main
            .find(".mount(\"/orgs\", route_orgs::module())")
            .unwrap();
        // Buckets mount under the default /storage prefix (issue #8), so they no
        // longer collide with module mounts.
        let avatars = main
            .find(".mount(\"/storage/avatars\", storage::avatars::module())")
            .unwrap();
        let invoices = main
            .find(".mount(\"/storage/invoices\", storage::invoices::module())")
            .unwrap();
        assert!(
            module_mount < avatars && avatars < invoices,
            "bucket mounts sorted, after modules: {main}"
        );
    }

    /// A custom `storage.base_path` overrides the default `/storage` prefix for
    /// every bucket mount (issue #8).
    #[test]
    fn expected_main_honors_custom_storage_base_path() {
        let mut d = storage_design();
        d.storage.as_mut().unwrap().base_path = Some("/files".into());
        let main = expected_main(&d);
        assert!(
            main.contains(".mount(\"/files/avatars\", storage::avatars::module())"),
            "custom base_path prefixes bucket mounts: {main}"
        );
        assert!(
            !main.contains("/storage/avatars"),
            "the default prefix is replaced, not appended: {main}"
        );
    }

    /// A top-level base_path prefixes EVERY module and bucket mount once (issue
    /// #16), while health (`/healthz`) and metrics (`/metrics`) — which come from
    /// the Observe extension, not these mounts — stay unprefixed.
    #[test]
    fn app_base_path_prefixes_module_and_bucket_mounts_but_not_health_or_metrics() {
        let mut d = storage_design();
        d.base_path = Some("/v1".into());
        let main = expected_main(&d);
        assert!(
            main.contains(".mount(\"/v1/orgs\", route_orgs::module())"),
            "module mount is prefixed: {main}"
        );
        assert!(
            main.contains(".mount(\"/v1/storage/avatars\", storage::avatars::module())"),
            "bucket mount is prefixed (app base + storage base): {main}"
        );
        // Health/metrics are never mounted here, so the prefix can't reach them.
        assert!(
            !main.contains("/v1/healthz") && !main.contains("/v1/metrics"),
            "health/metrics stay unprefixed: {main}"
        );
    }

    /// A root-mounted module under a base_path yields the base itself, never a
    /// dangling `/v1/`.
    #[test]
    fn app_base_path_joins_a_root_module_mount_without_a_trailing_slash() {
        let mut d: Design = serde_json::from_str(
            r#"{ "name": "root-api", "contract_version": 0, "dependencies": [],
                "base_path": "/v1",
                "modules": [{ "name": "api", "mount": "/",
                    "endpoints": [{ "operation_id": "root", "method": "GET", "path": "/",
                        "success": { "status": 200 } }] }] }"#,
        )
        .unwrap();
        d.modules.sort_by(|a, b| a.name.cmp(&b.name));
        let main = expected_main(&d);
        assert!(
            main.contains(".mount(\"/v1\", route_api::module())"),
            "root module mounts at the base, no trailing slash: {main}"
        );
    }

    /// Empty / `/` / absent base_path is a byte-for-byte no-op.
    #[test]
    fn empty_or_slash_base_path_is_a_no_op() {
        let d = storage_design();
        let baseline = expected_main(&d);
        for bp in [None, Some(String::new()), Some("/".to_string())] {
            let mut d2 = storage_design();
            d2.base_path = bp.clone();
            assert_eq!(
                expected_main(&d2),
                baseline,
                "base_path {bp:?} must not change any mount"
            );
        }
    }

    /// No storage block → byte-for-byte no storage wiring.
    #[test]
    fn expected_main_without_storage_has_no_storage_wiring() {
        let d: Design = serde_json::from_str(crate::platform::design::tests::V1_FULL).unwrap();
        let main = expected_main(&d);
        assert!(!main.contains("storage"), "no storage wiring: {main}");
    }

    /// `storage` is a RESERVED dependency name: listing it must not emit the
    /// "provide here" stub comment (the block, not the dependency, is the gate).
    #[test]
    fn storage_dependency_name_is_reserved_not_stubbed() {
        let mut d = storage_design();
        d.dependencies.push("storage".into());
        let main = expected_main(&d);
        assert!(!main.contains("app dependency `storage`"), "{main}");
    }
}
