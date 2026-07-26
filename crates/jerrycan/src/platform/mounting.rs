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
        // Pre-wrapped exactly as rustfmt formats it (issue #128): the one-line
        // form's `OpenApi::new(..)` argument exceeds rustfmt's fn_call_width, so
        // `cargo fmt` would rewrap it and trip JL0003 on a file the agent never
        // touched. Emitting the wrapped form keeps `cargo fmt` a no-op here.
        block.push_str(
            "        .extend(jerrycan::validate::OpenApi::new(include_str!(\n            \"../../../openapi.json\"\n        )))\n",
        );
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

/// One `.mount("<path>", <callee>)` builder line, pre-wrapped exactly as rustfmt
/// formats it (issue #128). rustfmt keeps a two-argument method call on one line
/// only while its arguments fit `fn_call_width` (60); a long module/bucket name
/// (e.g. `organization_invitations` → `route_organization_invitations::module()`,
/// or `/storage/<long-bucket>`) pushes the args past 60, so `cargo fmt` would
/// rewrap the line to the one-arg-per-line block form and trip JL0003 on a file
/// the agent never touched. Emitting that form up front keeps `cargo fmt` a
/// no-op. Boundary is empirical against the pinned toolchain's rustfmt: an
/// argument text of width 60 stays inline, 61 rewraps.
fn mount_line(path: &str, callee: &str) -> String {
    let args = format!("\"{path}\", {callee}");
    if args.chars().count() <= 60 {
        format!("        .mount({args})\n")
    } else {
        format!("        .mount(\n            \"{path}\",\n            {callee},\n        )\n")
    }
}

/// The CORS wiring for `expected_main` (issue #21): `(preamble, layer)`. Both are
/// empty when the design declares no `cors` block. The preamble binds `cors_origins`
/// from `JERRYCAN_CORS_ORIGINS` (comma-separated; `*` ⇒ any) with the design's
/// origins as the fallback, so a cross-origin SPA can be re-pointed at deploy time
/// without editing this tool-owned file. The preamble then assembles a `cors`
/// binding — `CorsConfig::new(cors_origins)` plus the design's methods/headers/
/// credentials, one setter per line — and the layer installs `.cors(cors)`. `.cors`
/// is an order-independent setter, so it sits right after `.map_error_body(..)`.
fn cors_wiring(design: &Design) -> (String, String) {
    let Some(cors) = &design.cors else {
        return (String::new(), String::new());
    };
    let default_origins = if cors.is_any() {
        "CorsOrigins::any()".to_string()
    } else {
        let quoted = cors
            .origins
            .iter()
            .map(|o| format!("{o:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!("CorsOrigins::list([{quoted}])")
    };
    let preamble = format!(
        "    // CORS (issue #21): allowed origins come from the design but are\n\
         \x20   // overridable at deploy time via JERRYCAN_CORS_ORIGINS (comma-separated,\n\
         \x20   // `*` for any), so a cross-origin SPA can be re-pointed without a rebuild.\n\
         \x20   let cors_origins = match std::env::var(\"JERRYCAN_CORS_ORIGINS\") {{\n\
         \x20       Ok(v) if v.trim() == \"*\" => CorsOrigins::any(),\n\
         \x20       Ok(v) if !v.trim().is_empty() => {{\n\
         \x20           CorsOrigins::list(v.split(',').map(str::trim).filter(|s| !s.is_empty()))\n\
         \x20       }}\n\
         \x20       _ => {default_origins},\n\
         \x20   }};\n"
    );
    // The config is bound over several statements — ONE setter per line — instead
    // of a single chained `.cors(CorsConfig::new(..)..)` expression (issue #128): a
    // realistic methods/headers config makes the chained form exceed rustfmt's wrap
    // point, and rustfmt's multi-regime reflow of a `.cors(<long chain>)` line is
    // not something we can byte-match. Split into single-setter statements, the only
    // thing that can wrap is an array literal — which `cors_setter` pre-wraps itself
    // — so the whole block stays a `cargo fmt` no-op for every config.
    let mut builder = String::from("    let cors = CorsConfig::new(cors_origins);\n");
    if !cors.methods.is_empty() {
        let methods: Vec<String> = cors
            .methods
            .iter()
            .map(|m| format!("jerrycan::http::Method::{}", m.as_http_const()))
            .collect();
        builder.push_str(&cors_setter("allow_methods", &methods));
    }
    if !cors.headers.is_empty() {
        let headers: Vec<String> = cors.headers.iter().map(|h| format!("{h:?}")).collect();
        builder.push_str(&cors_setter("allow_headers", &headers));
    }
    if cors.allow_credentials {
        builder.push_str("    let cors = cors.allow_credentials(true);\n");
    }
    let layer = String::from("        .cors(cors)\n");
    (format!("{preamble}{builder}"), layer)
}

/// One `let cors = cors.<setter>([<items>]);` line for the CORS builder,
/// pre-wrapped exactly as rustfmt formats it (issue #128). rustfmt keeps a
/// multi-element array literal on one line only while the whole statement fits
/// its wrap point; a config with several methods/headers pushes it past, and
/// `cargo fmt` would rewrap the array one-item-per-line — tripping JL0003 on the
/// tool-owned main.rs the agent never touched. Emitting that form up front keeps
/// the block a fmt no-op. A single-element array never wraps (rustfmt overflows
/// the sole element), so it stays inline regardless of length. Boundary is
/// empirical against the pinned toolchain's rustfmt: a statement of width 98
/// stays inline, 99 rewraps.
fn cors_setter(setter: &str, items: &[String]) -> String {
    let single = format!("    let cors = cors.{setter}([{}]);", items.join(", "));
    if items.len() <= 1 || single.chars().count() <= 98 {
        format!("{single}\n")
    } else {
        let mut wrapped = format!("    let cors = cors.{setter}([\n");
        for item in items {
            wrapped.push_str(&format!("        {item},\n"));
        }
        wrapped.push_str("    ]);\n");
        wrapped
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
        mounts.push_str(&mount_line(
            &join_mount(base, &m.effective_mount()),
            &format!("{}::module()", crate_ident(&m.name)),
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
            mounts.push_str(&mount_line(
                &bucket_mount,
                &format!("storage::{}::module()", b.name.replace('-', "_")),
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
    // Module declarations. `errors` (the AGENT-owned error-body mapper, issue
    // #13) is present in EVERY mode — framework errors happen with or without a
    // db. db mode adds the AGENT-owned `boot` module (issue #12) and the
    // tool-owned `migrations` module. Emitted in ALPHABETICAL order (issue #120):
    // rustfmt's default `reorder_modules` sorts `mod` declarations, so an
    // unsorted scaffold would fail `cargo fmt --check` out of the box and trip
    // JL0003 (generated-file drift) on a file the agent never touched. Sorting
    // here keeps `cargo fmt` a no-op on a fresh scaffold.
    let mut mod_names = vec!["errors"];
    if design.wants_db() {
        mod_names.push("boot");
        mod_names.push("migrations");
    }
    mod_names.sort_unstable();
    let mut mods = mod_names
        .iter()
        .map(|name| format!("mod {name};\n"))
        .collect::<String>();
    mods.push('\n');
    let module_decls = mods;
    // The boot hook borrows `db` after all migrations and before it is moved into
    // the extension block. db mode only (the hook takes `&Db`).
    let boot_call = if design.wants_db() {
        "    boot::on_boot(&db).await?;\n"
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
    // CORS (issue #21): the preamble binds `cors_origins` (env-overridable) just
    // before App::new(); the layer installs `.cors(..)` after `.map_error_body(..)`.
    // Both empty unless the design declares a `cors` block.
    let (cors_preamble, cors_layer) = cors_wiring(design);

    format!(
        "//! GENERATED by jerrycan — do not hand-edit; `jerrycan generate` rewrites this file.\nuse jerrycan::prelude::*;\n\n{module_decls}#[jerrycan::main]\nasync fn main() -> Result<()> {{\n{logging}{db_preamble}{jobs_migrations}{storage_migrations}{boot_call}{cors_preamble}    App::new()\n        .map_error_body(errors::error_body)\n{cors_layer}{extensions}{tenant_dep}{mounts}        .serve()\n        .await\n}}\n"
    )
}

/// The AGENT-owned `crates/app/src/errors.rs`: the create-once error-body mapper
/// the generated main.rs wires via `.map_error_body(errors::error_body)` (issue
/// #13). Present in every mode. The default returns `None` (jerrycan's flat body,
/// details preserved); the agent returns `Some(json)` to render framework AND
/// handler errors in the app's own wire envelope.
pub(crate) const ERRORS_RS: &str = "//! AGENT-OWNED error-body mapper — create-once; `jerrycan generate` never\n\
//! overwrites this file. Reshape framework-emitted errors (the auth guard's\n\
//! 401, extractor 4xx, …) AND handler errors into your API's wire envelope.\n\
use jerrycan::http::StatusCode;\n\
use jerrycan::serde_json::Value;\n\
\n\
/// Map an error's (status, stable code, message) to your wire envelope. Return\n\
/// `Some(body)` to override the response body; `None` keeps jerrycan's default\n\
/// `{code, message[, details]}` body. Applies to EVERY error response.\n\
pub fn error_body(_status: StatusCode, _code: &str, _message: &str) -> Option<Value> {\n\
\x20   // Example — wrap every error as { \"error\": { code, message } }:\n\
\x20   //   Some(jerrycan::serde_json::json!({\n\
\x20   //       \"error\": { \"code\": _code, \"message\": _message }\n\
\x20   //   }))\n\
\x20   None\n\
}\n";

/// The AGENT-owned `crates/app/src/boot.rs`: a create-once startup hook the
/// generated main.rs calls after migrations (issue #12). Preserved across
/// `jerrycan generate` (written only when absent). db mode only.
pub(crate) const BOOT_RS: &str = "//! AGENT-OWNED startup hook — create-once; `jerrycan generate` never overwrites\n\
//! this file. `on_boot` runs after migrations and before the app serves. Put\n\
//! idempotent startup work here (e.g. a dev seed); it runs on EVERY boot.\n\
use jerrycan::Result;\n\
use jerrycan::db::Db;\n\
\n\
/// Startup hook: runs once per boot, after migrations, before serving.\n\
pub async fn on_boot(_db: &Db) -> Result<()> {\n\
\x20   // Example — an idempotent dev seed (safe to run on every boot):\n\
\x20   //   use jerrycan::db::sea_orm::ConnectionTrait;\n\
\x20   //   _db.conn()\n\
\x20   //       .execute_unprepared(\"INSERT INTO widgets (id, name) VALUES (1, 'demo') ON CONFLICT DO NOTHING\")\n\
\x20   //       .await\n\
\x20   //       .map_err(jerrycan::db::db_error)?;\n\
\x20   Ok(())\n\
}\n";

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

/// One `include_str!` struct-field line, pre-wrapped exactly as rustfmt formats
/// it (issue #128): single-line while the rendered line fits rustfmt's
/// `max_width` (100), the block-indented macro form beyond it. Long module
/// names/stems (e.g. `workspaces` postgres paths) otherwise get rewrapped by
/// `cargo fmt`, tripping JL0003 on a file the agent never touched.
fn include_str_field(indent: &str, field: &str, path: &str) -> String {
    let line = format!("{indent}{field}: include_str!(\"{path}\"),");
    if line.chars().count() <= 100 {
        format!("{line}\n")
    } else {
        format!("{indent}{field}: include_str!(\n{indent}    \"{path}\"\n{indent}),\n")
    }
}

/// The tool-owned `app/src/migrations.rs` aggregating module-owned migrations,
/// or None when the design has no `db` dependency. The emitted text is a
/// rustfmt fixpoint for every shape (issue #128): zero entries collapse to
/// `&[];`, a single entry to rustfmt's inlined `&[Migration { .. }]` form, and
/// over-wide `include_str!` lines are pre-wrapped — so `cargo fmt` never
/// rewrites this file and JL0003 stays quiet on it.
pub fn expected_migrations_rs(app_root: &Path, design: &Design) -> Result<Option<String>, String> {
    if !design.wants_db() {
        return Ok(None);
    }
    let scanned = scan_migrations(app_root)?;
    let header = "//! GENERATED by jerrycan — aggregates module-owned migrations; do not hand-edit.\nuse jerrycan::db::Migration;\n\n";
    let fields = |indent: &str, m: &ScannedMigration| {
        let module_snake = m.module.replace('-', "_");
        let stem = &m.file_stem;
        let module = &m.module;
        format!(
            "{indent}name: \"{module_snake}_{stem}\",\n{sqlite}{postgres}",
            sqlite = include_str_field(
                indent,
                "sqlite",
                &format!("../../routes/{module}/migrations/sqlite/{stem}.sql"),
            ),
            postgres = include_str_field(
                indent,
                "postgres",
                &format!("../../routes/{module}/migrations/postgres/{stem}.sql"),
            ),
        )
    };
    let body = match scanned.as_slice() {
        [] => "pub const MIGRATIONS: &[Migration] = &[];\n".to_string(),
        [only] => format!(
            "pub const MIGRATIONS: &[Migration] = &[Migration {{\n{}}}];\n",
            fields("    ", only)
        ),
        many => {
            let mut entries = String::new();
            for m in many {
                entries.push_str(&format!(
                    "    Migration {{\n{}    }},\n",
                    fields("        ", m)
                ));
            }
            format!("pub const MIGRATIONS: &[Migration] = &[\n{entries}];\n")
        }
    };
    Ok(Some(format!("{header}{body}")))
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

    // 1a0. app/src/errors.rs — the AGENT-OWNED error-body mapper `main.rs` wires
    // (issue #13). Every mode, CREATE-ONCE: written only when absent so
    // `jerrycan generate` never clobbers the agent's wire-envelope mapping.
    let errors_path = app_root.join("crates/app/src/errors.rs");
    if !errors_path.exists() {
        fs::create_dir_all(errors_path.parent().expect("parent")).map_err(|e| e.to_string())?;
        fs::write(&errors_path, ERRORS_RS).map_err(|e| e.to_string())?;
        modified.push("crates/app/src/errors.rs".to_string());
    }

    // 1a. app/src/boot.rs — the AGENT-OWNED startup hook `main.rs` calls after
    // migrations (issue #12). db mode only, and CREATE-ONCE: written only when
    // absent so `jerrycan generate` never clobbers the agent's seed/boot logic.
    let boot_path = app_root.join("crates/app/src/boot.rs");
    if design.wants_db() {
        if !boot_path.exists() {
            fs::write(&boot_path, BOOT_RS).map_err(|e| e.to_string())?;
            modified.push("crates/app/src/boot.rs".to_string());
        }
    } else if boot_path.exists() {
        // Memory mode declares no `mod boot;`, so a leftover would be unreferenced.
        fs::remove_file(&boot_path).map_err(|e| e.to_string())?;
        modified.push("crates/app/src/boot.rs".to_string());
    }

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

    /// db mode wires the agent-owned boot hook: `mod boot;` and a
    /// `boot::on_boot(&db)` call that runs AFTER migrations and BEFORE App::new
    /// (so it can seed over the live db before the db is moved into the app).
    #[test]
    fn expected_main_wires_the_boot_hook_after_migrations_in_db_mode() {
        let main = expected_main(&jobs_design());
        assert!(
            main.contains("mod boot;"),
            "declares the boot module: {main}"
        );
        let boot = main
            .find("boot::on_boot(&db).await?;")
            .expect("boot call present");
        let app_mig = main.find("db.migrate(migrations::MIGRATIONS)").unwrap();
        let app_new = main.find("App::new()").unwrap();
        assert!(
            app_mig < boot && boot < app_new,
            "boot runs after migrations, before App::new: {main}"
        );
    }

    /// #120: a fresh scaffold must be `cargo fmt --check` clean out of the box.
    /// rustfmt's default `reorder_modules` sorts `mod` declarations
    /// alphabetically, so main.rs must emit them already sorted — otherwise
    /// `cargo fmt` reorders an untouched generated file and trips JL0003
    /// (generated-file drift on a file the agent never edited).
    #[test]
    fn expected_main_declares_modules_in_sorted_order() {
        // db mode declares three modules (errors + boot + migrations); memory
        // mode declares only `errors` (a single line is trivially sorted).
        let main = expected_main(&jobs_design());
        let mods: Vec<&str> = main
            .lines()
            .filter_map(|l| l.strip_prefix("mod ").and_then(|r| r.strip_suffix(';')))
            .collect();
        assert!(
            mods.contains(&"boot") && mods.contains(&"errors") && mods.contains(&"migrations"),
            "db mode declares errors/boot/migrations: {mods:?}"
        );
        let mut sorted = mods.clone();
        sorted.sort_unstable();
        assert_eq!(
            mods, sorted,
            "mod declarations must be alphabetical so `cargo fmt` is a no-op (issue #120): {mods:?}"
        );
    }

    /// Memory mode (no db) has no boot hook — the hook takes `&Db`.
    #[test]
    fn memory_mode_has_no_boot_hook() {
        let d: Design = serde_json::from_str(
            r#"{ "name": "mem-app", "contract_version": 0, "dependencies": [],
                "modules": [{ "name": "m", "endpoints": [
                    { "operation_id": "list_m", "method": "GET", "path": "/",
                      "success": { "status": 200 } }] }] }"#,
        )
        .unwrap();
        let main = expected_main(&d);
        assert!(
            !main.contains("mod boot;") && !main.contains("on_boot"),
            "no boot wiring in memory mode: {main}"
        );
    }

    /// The agent-owned boot.rs is created once and PRESERVED across regeneration
    /// (like handlers/repos): `jerrycan generate` must never clobber the agent's
    /// seed/boot logic.
    #[test]
    fn boot_rs_is_created_once_and_preserved_across_regeneration() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("app");
        let d = jobs_design(); // db mode
        crate::platform::scaffold::scaffold(&root, &d).unwrap();
        let boot = root.join("crates/app/src/boot.rs");
        assert!(boot.exists(), "scaffold creates boot.rs in db mode");
        // The agent edits the hook…
        let custom = "//! my custom seed\nuse jerrycan::Result;\nuse jerrycan::db::Db;\npub async fn on_boot(_db: &Db) -> Result<()> {\n    Ok(())\n}\n";
        fs::write(&boot, custom).unwrap();
        // …and a regeneration must leave it untouched.
        regenerate(&root, &d).unwrap();
        assert_eq!(
            fs::read_to_string(&boot).unwrap(),
            custom,
            "regeneration must preserve the agent-owned boot.rs"
        );
    }

    /// EVERY generated app declares `mod errors;` and wires
    /// `.map_error_body(errors::error_body)` right after `App::new()` (issue #13)
    /// — framework errors happen in db and memory modes alike.
    #[test]
    fn expected_main_wires_the_error_body_mapper_in_every_mode() {
        let mem: Design = serde_json::from_str(
            r#"{ "name": "mem-app", "contract_version": 0, "dependencies": [],
                "modules": [{ "name": "m", "endpoints": [
                    { "operation_id": "list_m", "method": "GET", "path": "/",
                      "success": { "status": 200 } }] }] }"#,
        )
        .unwrap();
        for d in [jobs_design(), mem] {
            let main = expected_main(&d);
            assert!(
                main.contains("mod errors;"),
                "declares errors module: {main}"
            );
            let map = main
                .find(".map_error_body(errors::error_body)")
                .expect("wires the mapper");
            let app_new = main.find("App::new()").unwrap();
            assert!(app_new < map, "mapper wired on the App builder: {main}");
        }
    }

    /// The agent-owned errors.rs is created once and PRESERVED across
    /// regeneration (the agent's wire envelope must survive `jerrycan generate`).
    #[test]
    fn errors_rs_is_created_once_and_preserved_across_regeneration() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("app");
        let d = jobs_design();
        crate::platform::scaffold::scaffold(&root, &d).unwrap();
        let errors = root.join("crates/app/src/errors.rs");
        assert!(errors.exists(), "scaffold creates errors.rs");
        let custom = "//! my envelope\nuse jerrycan::http::StatusCode;\nuse jerrycan::serde_json::Value;\npub fn error_body(_s: StatusCode, c: &str, m: &str) -> Option<Value> {\n    Some(jerrycan::serde_json::json!({ \"error\": { \"code\": c, \"message\": m } }))\n}\n";
        fs::write(&errors, custom).unwrap();
        regenerate(&root, &d).unwrap();
        assert_eq!(
            fs::read_to_string(&errors).unwrap(),
            custom,
            "regeneration must preserve the agent-owned errors.rs"
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

    fn cors_design() -> Design {
        serde_json::from_str(
            r#"{
                "name": "cors-app", "contract_version": 0, "dependencies": [],
                "cors": {
                    "origins": ["https://app.example", "https://admin.example"],
                    "methods": ["GET", "POST"],
                    "headers": ["content-type", "authorization"],
                    "allow_credentials": true
                },
                "modules": [{ "name": "things",
                    "endpoints": [{ "operation_id": "list_things", "method": "GET", "path": "/",
                        "success": { "status": 200 } }] }]
            }"#,
        )
        .unwrap()
    }

    /// WHY: a cross-origin SPA (console on one origin, API on another) must work
    /// without hand-editing the tool-owned main.rs (that edit trips JL0003 and is
    /// wiped by the next `jerrycan generate`). A `cors` block therefore assembles a
    /// `cors` binding — `CorsConfig::new(..)` plus the design's methods/headers/
    /// credentials, one setter per line (issue #128) — and installs `.cors(cors)`
    /// right after `.map_error_body(..)`, before App::new's extensions/mounts (#21).
    #[test]
    fn expected_main_emits_the_cors_layer_from_the_design_block() {
        let main = expected_main(&cors_design());
        // The config is assembled one setter per line so `cargo fmt` never rewraps
        // a long methods/headers chain on the tool-owned main.rs (issue #128).
        for stmt in [
            "    let cors = CorsConfig::new(cors_origins);\n",
            "    let cors = cors.allow_methods([jerrycan::http::Method::GET, jerrycan::http::Method::POST]);\n",
            "    let cors = cors.allow_headers([\"content-type\", \"authorization\"]);\n",
            "    let cors = cors.allow_credentials(true);\n",
        ] {
            assert!(main.contains(stmt), "missing cors setter `{stmt}`:\n{main}");
        }
        // The layer references the assembled binding.
        assert!(
            main.contains("        .cors(cors)\n"),
            "the layer installs the assembled `cors` binding:\n{main}"
        );
        // The binding installs the design's origins as the env fallback list.
        assert!(
            main.contains(
                "CorsOrigins::list([\"https://app.example\", \"https://admin.example\"])"
            ),
            "the design origins are the env fallback:\n{main}"
        );
        // Order: .cors(cors) sits between .map_error_body(..) and the mounts.
        let map = main.find(".map_error_body(errors::error_body)").unwrap();
        let cors = main.find(".cors(cors)").unwrap();
        let serve = main.find(".serve()").unwrap();
        assert!(
            map < cors && cors < serve,
            "cors after map_error_body: {main}"
        );
    }

    /// The env-override contract: the generated wiring reads JERRYCAN_CORS_ORIGINS
    /// (comma-separated) and falls back to the design's origins — a deploy can
    /// re-point the allowed origins WITHOUT a rebuild or a tool-owned-file edit.
    #[test]
    fn expected_main_cors_reads_the_env_override_with_a_design_fallback() {
        let main = expected_main(&cors_design());
        assert!(
            main.contains("std::env::var(\"JERRYCAN_CORS_ORIGINS\")"),
            "the origins must be env-overridable at deploy time:\n{main}"
        );
        assert!(
            main.contains(
                "CorsOrigins::list(v.split(',').map(str::trim).filter(|s| !s.is_empty()))"
            ),
            "the env var is parsed comma-separated:\n{main}"
        );
        assert!(
            main.contains("Ok(v) if v.trim() == \"*\" => CorsOrigins::any()"),
            "a `*` env value means any origin:\n{main}"
        );
    }

    /// The `*` origins marker maps to CorsOrigins::any() as the fallback, and a
    /// minimal block (no methods/headers/credentials) emits a bare CorsConfig::new.
    #[test]
    fn expected_main_cors_any_origin_and_minimal_block() {
        let mut d = cors_design();
        let c = d.cors.as_mut().unwrap();
        c.origins = vec!["*".into()];
        c.methods.clear();
        c.headers.clear();
        c.allow_credentials = false;
        let main = expected_main(&d);
        assert!(
            main.contains("_ => CorsOrigins::any(),"),
            "`*` origins fall back to CorsOrigins::any():\n{main}"
        );
        // A minimal block binds a bare `cors` (no setter statements) and layers it.
        assert!(
            main.contains("    let cors = CorsConfig::new(cors_origins);\n")
                && main.contains("        .cors(cors)\n"),
            "a minimal block emits a bare CorsConfig::new binding:\n{main}"
        );
        assert!(
            !main.contains("allow_methods")
                && !main.contains("allow_headers")
                && !main.contains("allow_credentials"),
            "a minimal block chains no setters:\n{main}"
        );
    }

    /// No `cors` block ⇒ byte-for-byte no CORS wiring (no layer, no env binding).
    /// Guards the gating so v0/v1/v2 designs without CORS are unchanged.
    #[test]
    fn expected_main_without_cors_has_no_cors_wiring() {
        let mut d = cors_design();
        d.cors = None;
        let main = expected_main(&d);
        assert!(
            !main.contains(".cors(") && !main.contains("JERRYCAN_CORS_ORIGINS"),
            "no cors block ⇒ no cors wiring:\n{main}"
        );
    }
}
