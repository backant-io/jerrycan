//! The jerrycan binary: CLI + `jerrycan mcp` (stdio MCP server).
#![forbid(unsafe_code)]

use clap::{Parser, Subcommand};
use jerrycan::platform::design::Design;
use jerrycan::platform::{
    EXIT_OK, EXIT_USAGE, Failure, checkpipe, genroute, mounting, package, questions, scaffold,
};
use std::path::{Path, PathBuf};

mod onboard;

#[derive(Parser)]
#[command(
    name = "jerrycan",
    version,
    about = "The AI-native Rust backend platform"
)]
struct Cli {
    /// Emit machine-readable JSON on stdout (same payload as the MCP tool).
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Scaffold a project from a validated design
    New {
        name: String,
        #[arg(long)]
        design: String,
    },
    /// Generate a route module, subroute, or dependency
    #[command(alias = "g")]
    Generate {
        #[command(subcommand)]
        what: GenerateCmd,
    },
    /// Show the route tree with module ownership
    List {
        #[command(subcommand)]
        what: ListCmd,
    },
    /// Run with auto-reload
    Dev {
        #[arg(long)]
        addr: Option<String>,
    },
    /// Verification gate: build + clippy + audit + deny + tests + jerrycan lints
    Check {
        #[arg(long)]
        module: Option<String>,
        /// Run every test target (cargo --no-fail-fast) and report per-module
        /// pass/fail counts, instead of stopping at the first failing target.
        /// For TDD: see the whole red→green picture in one run.
        #[arg(long, alias = "full-report")]
        no_fail_fast: bool,
    },
    /// Run the app's (or one module's) test suite
    Test {
        #[arg(long)]
        module: Option<String>,
    },
    /// Generate failing acceptance tests for a module from the design (TDD)
    GenTests {
        #[arg(long)]
        module: String,
    },
    /// AI-native docs, offline
    Docs {
        topic: Option<String>,
        #[arg(long)]
        search: Option<String>,
        /// List every page (slug + title + one-line summary). Implied when no
        /// topic and no --search are given.
        #[arg(long)]
        list: bool,
        /// Max search hits to return (default: enough to never hide a page).
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Explain a diagnostic code (JC#### / JL####)
    Explain { code: String },
    /// Wire an extension into the app (db, validate)
    Add { extension: String },
    /// Database commands
    Db {
        #[command(subcommand)]
        what: DbCmd,
    },
    /// Show (or rewrite) the derived schema.json data contract
    Schema {
        /// Rewrite schema.json from the current migrations and print its path
        #[arg(long)]
        write: bool,
    },
    /// Emit hardened deployment artifacts + SBOM after a green check
    Package {
        /// Emit a hardened multi-stage Dockerfile
        #[arg(long)]
        docker: bool,
        /// Build a release binary (musl static, host fallback)
        #[arg(long)]
        binary: bool,
        /// Emit hardened Kubernetes manifests (Deployment/Service/NetworkPolicy)
        #[arg(long)]
        k8s: bool,
        /// Emit a hardened systemd unit
        #[arg(long)]
        systemd: bool,
    },
    /// Generate a zero-touch deploy kit (run it with your platform API key)
    Deploy {
        /// Deploy target (currently: render)
        target: String,
    },
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
    /// Print the guided build runbook (design → scaffold → implement → check)
    Onboard {
        /// Write the skill/rules files for an agent instead of printing
        #[arg(long, requires = "agent")]
        emit_skill: bool,
        /// Target agent: claude-code | cursor | codex | windsurf | generic
        #[arg(long)]
        agent: Option<String>,
        /// Directory for project-level files (default: current directory)
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Serve MCP over stdio
    Mcp,
}

#[derive(Subcommand)]
enum DbCmd {
    /// Apply module-owned migrations (env JERRYCAN_DATABASE_URL or --url)
    Migrate {
        #[arg(long)]
        url: Option<String>,
    },
    /// Apply the migrated data seed (resumable)
    Seed {
        #[arg(long)]
        url: Option<String>,
    },
}

#[derive(Subcommand)]
enum GenerateCmd {
    /// New route-module crate, or subroute (`todos/comments`)
    Route { path: String },
    /// Module-scoped dependency stub
    Dep {
        name: String,
        #[arg(long)]
        module: String,
    },
    /// Next numbered dual-dialect migration pair for a module
    Migration {
        name: String,
        #[arg(long)]
        module: String,
    },
}

#[derive(Subcommand)]
enum ListCmd {
    Routes,
}

fn main() {
    // clap exits 2 on usage errors by default ONLY for some error kinds; force
    // the cli-ux.md contract: every parse failure is exit 2 with the message on stderr.
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            // --help/--version are "successful" parse errors: print to stdout, exit 0.
            // clap's own signal tells us which stream the message belongs on.
            if e.use_stderr() {
                eprint!("{e}");
                std::process::exit(EXIT_USAGE);
            }
            print!("{e}");
            std::process::exit(EXIT_OK);
        }
    };

    // The --json flag is consumed inside run(cli); capture it for the sink so a
    // failure can emit its machine envelope after run() returns.
    let json_mode = cli.json;
    let result: Result<(), Failure> = run(cli);
    match result {
        Ok(()) => std::process::exit(EXIT_OK),
        Err(f) => {
            // #28: EVERY --json failure carries exactly one JSON document on
            // stdout. Commands that already emitted their own machine payload
            // (questions list, check report) set json_emitted so we don't double.
            if json_mode && !f.json_emitted {
                println!(
                    "{}",
                    serde_json::json!({
                        "ok": false,
                        "code": f.code,
                        "error": f.message,
                        "hint": f.hint,
                    })
                );
            }
            eprintln!("error: {}", f.message);
            std::process::exit(f.exit);
        }
    }
}

fn run(cli: Cli) -> Result<(), Failure> {
    match cli.command {
        Cmd::New { name, design } => cmd_new(&name, &design, cli.json),
        Cmd::Generate { what } => match what {
            GenerateCmd::Route { path } => cmd_generate_route(&path, cli.json),
            GenerateCmd::Dep { name, module } => cmd_generate_dep(&name, &module, cli.json),
            GenerateCmd::Migration { name, module } => {
                cmd_generate_migration(&name, &module, cli.json)
            }
        },
        Cmd::List {
            what: ListCmd::Routes,
        } => cmd_list_routes(cli.json),
        Cmd::Dev { addr } => cmd_dev(addr.as_deref()),
        Cmd::Check {
            module,
            no_fail_fast,
        } => cmd_check(module.as_deref(), no_fail_fast, cli.json),
        Cmd::Test { module } => cmd_test(module.as_deref()),
        Cmd::GenTests { module } => cmd_gen_tests(&module, cli.json),
        Cmd::Docs {
            topic,
            search,
            list,
            limit,
        } => cmd_docs(topic.as_deref(), search.as_deref(), list, limit, cli.json),
        Cmd::Explain { code } => cmd_explain(&code, cli.json),
        Cmd::Add { extension } => cmd_add(&extension, cli.json),
        Cmd::Db {
            what: DbCmd::Migrate { url },
        } => cmd_db_migrate(url.as_deref(), cli.json),
        Cmd::Db {
            what: DbCmd::Seed { url },
        } => cmd_db_seed(url.as_deref(), cli.json),
        Cmd::Schema { write } => cmd_schema(write, cli.json),
        Cmd::Package {
            docker,
            binary,
            k8s,
            systemd,
        } => cmd_package(docker, binary, k8s, systemd, cli.json),
        Cmd::Deploy { target } => cmd_deploy(&target, cli.json),
        Cmd::Migrate {
            from,
            export_dir,
            live,
            out,
            name,
            bulk_threshold,
        } => cmd_migrate(
            &from,
            export_dir.as_deref(),
            live.as_deref(),
            out.as_deref(),
            name.as_deref(),
            bulk_threshold,
            cli.json,
        ),
        Cmd::Onboard {
            emit_skill,
            agent,
            dir,
        } => cmd_onboard(emit_skill, agent.as_deref(), dir, cli.json),
        Cmd::Mcp => jerrycan::platform::mcp::serve_stdio().map_err(Failure::environment),
    }
}

fn cmd_onboard(
    emit_skill: bool,
    agent: Option<&str>,
    dir: Option<PathBuf>,
    json_mode: bool,
) -> Result<(), Failure> {
    if emit_skill {
        let agent: onboard::Agent = agent
            .expect("clap `requires` guarantees --agent")
            .parse()
            .map_err(Failure::usage)?;
        let project_dir = dir.unwrap_or_else(|| PathBuf::from("."));
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| Failure::environment("HOME is not set"))?;
        let out = onboard::emit_skill(agent, &project_dir, &home)
            .map_err(|e| Failure::environment(format!("emit-skill: {e}")))?;
        if json_mode {
            println!(
                "{}",
                serde_json::json!({
                    // PathBuf → display strings: independent of serde feature flags.
                    "written": out.written.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
                    "unchanged": out.unchanged.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
                    "instructions": out.instructions,
                    "next_step": "run `jerrycan onboard` and follow the runbook",
                })
            );
        } else {
            for p in &out.written {
                println!("wrote {}", p.display());
            }
            for p in &out.unchanged {
                println!("unchanged {}", p.display());
            }
            if let Some(i) = &out.instructions {
                println!("{i}");
            }
        }
        return Ok(());
    }
    if json_mode {
        println!(
            "{}",
            serde_json::json!({
                "markdown": onboard::runbook(),
                "next_step": "follow the runbook phases in order, starting with the entry-path question",
            })
        );
    } else {
        println!("{}", onboard::runbook());
    }
    Ok(())
}

fn cmd_docs(
    topic: Option<&str>,
    query: Option<&str>,
    list: bool,
    limit: Option<usize>,
    json_mode: bool,
) -> Result<(), Failure> {
    use jerrycan::platform::docsidx;
    // Enumerate the whole surface: explicit --list, or the bare `jerrycan docs`
    // (no topic, no --search) so an agent gets the page index in one call.
    if list || (topic.is_none() && query.is_none()) {
        let pages = docsidx::list();
        if json_mode {
            println!("{}", serde_json::json!({ "pages": pages }));
        } else {
            for p in &pages {
                println!("{}  — {}", p.page, p.summary);
            }
        }
        return Ok(());
    }
    if let Some(q) = query {
        // Default the cap to the page count so a broad query never silently hides
        // a page (the search scores each page at most once); --limit overrides.
        let limit = limit.unwrap_or_else(|| docsidx::PAGES.len());
        let results = docsidx::search(q, limit);
        let payload = serde_json::json!({ "results": results });
        if json_mode {
            println!("{payload}");
        } else {
            for r in payload["results"].as_array().unwrap() {
                println!(
                    "{} ({}#{})",
                    r["snippet"].as_str().unwrap(),
                    r["page"].as_str().unwrap(),
                    r["anchor"].as_str().unwrap_or("")
                );
            }
        }
        return Ok(());
    }
    let Some(topic) = topic else {
        // Unreachable: handled by the list branch above, but keep the guard.
        return Err(Failure::usage(
            "provide a topic (`jerrycan docs dependencies`), --search <query>, or --list",
        ));
    };
    let (page, anchor) = match topic.split_once('#') {
        Some((p, a)) => (p, Some(a)),
        None => (topic, None),
    };
    let md = docsidx::get(page, anchor).ok_or_else(|| {
        let names: Vec<&str> = docsidx::PAGES.iter().map(|(n, _)| *n).collect();
        Failure::usage(format!(
            "unknown docs page `{page}` — available: {}",
            names.join(", ")
        ))
    })?;
    if json_mode {
        println!("{}", serde_json::json!({ "markdown": md }));
    } else {
        println!("{md}");
    }
    Ok(())
}

fn cmd_explain(code: &str, json_mode: bool) -> Result<(), Failure> {
    let info = jerrycan::platform::codes::lookup(code).ok_or_else(|| {
        Failure::usage(format!(
            "unknown code `{code}` — see `jerrycan explain JC0404` for the format"
        ))
    })?;
    if json_mode {
        println!(
            "{}",
            serde_json::json!({
                "code": info.code, "title": info.title, "cause": info.cause, "fix": info.fix, "doc": info.doc,
            })
        );
    } else {
        println!("{} — {}", info.code, info.title);
        println!("\ncause: {}", info.cause);
        println!("fix:   {}", info.fix);
        println!("docs:  {}", info.doc);
    }
    Ok(())
}

fn emit(json_mode: bool, payload: &serde_json::Value, human: &str) {
    if json_mode {
        println!("{payload}");
    }
    eprintln!("{human}");
}

/// Surface issue #69a: agent-authored `mod`/`use` wiring that regenerating a
/// tool-owned file (lib.rs/mod.rs) DROPPED. Prints a loud stderr warning naming
/// each dropped line and its file (in EVERY mode — stderr is not the `--json`
/// channel), and returns the machine-readable list for the envelope's `warnings`.
/// The lines are gone from the rewritten file; the agent must re-home them. Empty
/// input prints nothing and returns `[]`, so a clean regeneration is unchanged.
fn dropped_warnings(dropped: &[(String, Vec<String>)]) -> Vec<serde_json::Value> {
    for (file, lines) in dropped {
        eprintln!(
            "warning: regenerating tool-owned {file} dropped {} agent-added line(s) it does not re-emit — NOT preserved:",
            lines.len()
        );
        for l in lines {
            eprintln!("  - {l}");
        }
        eprintln!(
            "  re-home this wiring in an AGENT-owned file (handlers.rs, or a module's model.rs) — see `jerrycan docs database` (Cross-module data access). lib.rs/mod.rs are tool-owned and rewritten on every `generate`."
        );
    }
    dropped
        .iter()
        .map(|(file, lines)| serde_json::json!({ "file": file, "dropped_lines": lines }))
        .collect()
}

fn load_design(path: &Path) -> Result<Design, Failure> {
    Design::from_path(path).map_err(Failure::usage)
}

/// Validate; on questions emit the jerrycan_design-shaped payload and exit 1.
fn require_complete(design: &Design, json_mode: bool) -> Result<(), Failure> {
    // #27: a fatal design-shape conflict (e.g. tenancy.entity is the auth
    // identity) is rejected before any file is written, carrying its JC code so
    // the --json sink emits `{ok:false, code, error, hint}`. Checked ahead of the
    // completeness questions: it needs a redesign, not a field edit.
    if let Some(conflict) = questions::design_conflict(design) {
        return Err(Failure::gate(conflict.message)
            .with_code(conflict.code)
            .with_hint(conflict.hint));
    }
    let qs = questions::validate(design);
    if qs.is_empty() {
        return Ok(());
    }
    let payload = serde_json::json!({
        "status": "questions",
        "questions": qs,
        "next_step": "answer the questions, fix design.json, and re-run",
    });
    if json_mode {
        println!("{payload}");
    }
    let mut human = String::from("design is incomplete:\n");
    for q in &qs {
        human.push_str(&format!("  {} — {}\n", q.id, q.question));
    }
    // The questions JSON above is this failure's stdout document (in --json
    // mode), so the sink must not add the generic envelope on top of it.
    let f = Failure::gate(human);
    Err(if json_mode { f.mark_json_emitted() } else { f })
}

fn cmd_new(target: &str, design_path: &str, json_mode: bool) -> Result<(), Failure> {
    let design = load_design(Path::new(design_path))?;
    require_complete(&design, json_mode)?;
    let mut created = scaffold::scaffold(Path::new(target), &design).map_err(Failure::gate)?;
    // db apps ship a derived schema.json contract (memory mode has no migrations).
    if let Some(rel) = jerrycan::platform::schema::write_schema(Path::new(target), &design)
        .map_err(Failure::gate)?
    {
        created.push(rel);
    }
    let payload = serde_json::json!({
        "created": created,
        "next_step": format!("cd {target} && jerrycan check — then implement the handler stubs"),
    });
    emit(
        json_mode,
        &payload,
        &format!(
            "scaffolded {} files into {target}",
            payload["created"].as_array().map(Vec::len).unwrap_or(0)
        ),
    );
    Ok(())
}

/// The app root = cwd for post-scaffold commands (the MCP twin takes `directory`).
fn app_root() -> Result<PathBuf, Failure> {
    let cwd = std::env::current_dir().map_err(|e| Failure::environment(e.to_string()))?;
    if cwd.join("design.json").exists() {
        Ok(cwd)
    } else {
        Err(Failure::usage(
            "no design.json here — run inside a jerrycan app (or scaffold one with `jerrycan new`) — if you're in a subdirectory, cd to the app root.",
        ))
    }
}

fn cmd_generate_route(module_path: &str, json_mode: bool) -> Result<(), Failure> {
    let root = app_root()?;
    let design = load_design(&root.join("design.json"))?;
    require_complete(&design, json_mode)?;
    let top = module_path
        .split('/')
        .next()
        .expect("split yields at least one");
    if genroute::module_by_path(&design, module_path).is_none() {
        return Err(Failure::usage(format!(
            "module `{module_path}` is not in design.json — add it there first (the design is the source of truth)"
        )));
    }
    let top_module = design
        .modules
        .iter()
        .find(|m| m.name == top)
        .expect("checked above");
    let mode = genroute::GenMode {
        db: design.wants_db(),
        auth: design.wants_auth(),
    };
    let (created, dropped) =
        genroute::write_module_reporting(&root.join("crates/routes"), top_module, mode, &design)
            .map_err(Failure::gate)?;
    let modified = mounting::regenerate(&root, &design).map_err(Failure::gate)?;
    let warnings = dropped_warnings(&dropped);
    let mut payload = serde_json::json!({
        "created": created,
        "modified": modified,
        "next_step": format!("implement crates/routes/{top}/src/handlers.rs, then jerrycan check --module {top} — note: regeneration mirrors design.json exactly; routes removed there are removed here (stale agent files are not deleted)"),
    });
    if !warnings.is_empty() {
        payload["warnings"] = serde_json::Value::Array(warnings);
    }
    emit(
        json_mode,
        &payload,
        &format!("generated `{module_path}` and rewired mounting"),
    );
    Ok(())
}

fn cmd_generate_dep(name: &str, module: &str, json_mode: bool) -> Result<(), Failure> {
    let root = app_root()?;
    let mut design = load_design(&root.join("design.json"))?;
    genroute::add_dependency(&mut design, module, name).map_err(Failure::usage)?;
    std::fs::write(
        root.join("design.json"),
        scaffold::canonical_design_json(&design),
    )
    .map_err(|e| Failure::gate(e.to_string()))?;
    let deps_rel = {
        let mut parts = module.split('/');
        let top = parts.next().unwrap_or(module);
        let mut p = format!("crates/routes/{top}/src");
        for sub in parts {
            p.push_str(&format!("/subroutes/{}", sub.replace('-', "_")));
        }
        format!("{p}/deps.rs")
    };
    let payload = serde_json::json!({
        "created": [],
        "modified": ["design.json"],
        "next_step": format!("define `{name}` in {deps_rel} (configure hook)"),
    });
    emit(
        json_mode,
        &payload,
        &format!("recorded dependency `{name}` on module `{module}`"),
    );
    Ok(())
}

fn cmd_generate_migration(name: &str, module: &str, json_mode: bool) -> Result<(), Failure> {
    let root = app_root()?;
    let created = genroute::generate_migration(&root, module, name).map_err(Failure::usage)?;
    // The numbered stem (e.g. `0002_add_due_index`) for the human line, read off
    // the sqlite file the generator just wrote.
    let stem = created
        .iter()
        .find_map(|p| {
            p.strip_suffix(".sql")
                .and_then(|s| s.split_once("migrations/sqlite/"))
                .map(|(_, stem)| stem)
        })
        .unwrap_or(name);
    let payload = serde_json::json!({
        "created": created,
        "next_step": "edit both dialect files, then run jerrycan check",
    });
    emit(
        json_mode,
        &payload,
        &format!("migration {stem} created (sqlite + postgres) — edit both, then jerrycan check"),
    );
    Ok(())
}

fn cmd_add(extension: &str, json_mode: bool) -> Result<(), Failure> {
    if !matches!(extension, "db" | "validate") {
        return Err(Failure::usage(format!(
            "unknown extension `{extension}` — available: db, validate"
        )));
    }
    let root = app_root()?;
    let design_path = root.join("design.json");
    let mut design = load_design(&design_path)?;
    if !design.dependencies.iter().any(|d| d == extension) {
        design.dependencies.push(extension.to_string());
    }
    std::fs::write(&design_path, scaffold::canonical_design_json(&design))
        .map_err(|e| Failure::gate(e.to_string()))?;
    // Regenerate every tool-owned surface for the new mode, and refresh route
    // crates' tool-owned files (repos/migrations are create-once and untouched
    // for EXISTING modules — agents migrate those by hand; new scaffolds get SQL).
    let mode = genroute::GenMode {
        db: design.wants_db(),
        auth: design.wants_auth(),
    };
    let mut all_dropped = Vec::new();
    for m in &design.modules {
        let (_created, dropped) =
            genroute::write_module_reporting(&root.join("crates/routes"), m, mode, &design)
                .map_err(Failure::gate)?;
        all_dropped.extend(dropped);
    }
    let mut modified = mounting::regenerate(&root, &design).map_err(Failure::gate)?;
    // Policy files are mode-dependent supply-chain gates; flipping the mode must
    // rewrite them too (else an existing app keeps memory-mode deny.toml and the
    // db build fails the license/audit gate).
    modified.extend(scaffold::write_policy_files(&root, &design).map_err(Failure::gate)?);
    modified.push("design.json".to_string());
    modified.sort();
    modified.dedup();
    let next_step = if extension == "db" {
        "`db` wired — policy files updated. NOTE: existing modules keep their in-memory repo.rs (agent-owned); rewrite each crates/routes/<m>/src/repo.rs to the SQL form (or delete it and re-run `jerrycan generate route <m>`) before the build will pass. Then jerrycan check.".to_string()
    } else {
        format!("`{extension}` wired — review the regenerated mounting, then jerrycan check")
    };
    let warnings = dropped_warnings(&all_dropped);
    let mut payload = serde_json::json!({
        "created": [],
        "modified": modified,
        "next_step": next_step,
    });
    if !warnings.is_empty() {
        payload["warnings"] = serde_json::Value::Array(warnings);
    }
    emit(json_mode, &payload, &format!("added `{extension}`"));
    Ok(())
}

fn cmd_db_migrate(url: Option<&str>, json_mode: bool) -> Result<(), Failure> {
    let root = app_root()?;
    let design = load_design(&root.join("design.json"))?;
    if !design.wants_db() {
        return Err(Failure::usage(
            "this app has no `db` dependency — run `jerrycan add db` first",
        ));
    }
    // Collect module-owned migrations exactly as the generated migrations.rs does.
    let pairs = mounting::collect_migrations(&root).map_err(Failure::gate)?;
    let url = url
        .map(str::to_string)
        .or_else(|| std::env::var("JERRYCAN_DATABASE_URL").ok())
        .ok_or_else(|| Failure::usage("provide --url or set JERRYCAN_DATABASE_URL"))?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Failure::environment(e.to_string()))?;
    let applied = runtime
        .block_on(async {
            let db = jerrycan::db::Db::connect(&url).await?;
            db.migrate_owned(&pairs).await
        })
        .map_err(|e| Failure::gate(e.message().to_string()))?;
    let payload = serde_json::json!({
        "applied": applied,
        "next_step": if applied.is_empty() { "database already up to date" } else { "migrations applied — jerrycan check" },
    });
    emit(
        json_mode,
        &payload,
        &format!(
            "applied {} migration(s)",
            payload["applied"].as_array().map(Vec::len).unwrap_or(0)
        ),
    );
    Ok(())
}

fn cmd_db_seed(url: Option<&str>, json_mode: bool) -> Result<(), Failure> {
    let root = app_root()?;
    let design = load_design(&root.join("design.json"))?;
    if !design.wants_db() {
        return Err(Failure::usage(
            "this app has no `db` dependency — the seed applier needs a database",
        ));
    }
    let url = url
        .map(str::to_string)
        .or_else(|| std::env::var("JERRYCAN_DATABASE_URL").ok())
        .ok_or_else(|| Failure::usage("provide --url or set JERRYCAN_DATABASE_URL"))?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Failure::environment(e.to_string()))?;
    let summary = runtime
        .block_on(async {
            let db = jerrycan::db::Db::connect(&url)
                .await
                .map_err(|e| e.message().to_string())?;
            jerrycan::platform::migrate::seed::apply(&root, &db).await
        })
        .map_err(Failure::gate)?;
    let payload = serde_json::json!({
        "applied_tables": summary.applied_tables,
        "resumed": summary.resumed,
        "next_step": "jerrycan check",
    });
    emit(
        json_mode,
        &payload,
        &format!("seeded {} file(s)/table(s)", summary.applied_tables.len()),
    );
    Ok(())
}

fn cmd_schema(write: bool, json_mode: bool) -> Result<(), Failure> {
    use jerrycan::platform::schema;
    let root = app_root()?;
    let design = load_design(&root.join("design.json"))?;
    if !design.wants_db() {
        return Err(Failure::usage(
            "this app has no `db` dependency — there is no schema contract to derive (run `jerrycan add db` first)",
        ));
    }

    if write {
        let rel = schema::write_schema(&root, &design)
            .map_err(Failure::gate)?
            .expect("write_schema returns a path in db mode");
        let path = root.join(&rel);
        let payload = serde_json::json!({
            "path": rel,
            "next_step": "commit schema.json — it is the reviewable data contract",
        });
        emit(json_mode, &payload, &format!("wrote {}", path.display()));
        return Ok(());
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Failure::environment(e.to_string()))?;
    let contract = runtime
        .block_on(schema::derive_schema(&root, &design))
        .map_err(Failure::gate)?;

    if json_mode {
        // --json: the contract verbatim, identical to the MCP tool payload.
        print!("{}", schema::render(&contract));
    } else {
        for table in &contract.tables {
            println!("{} ({})", table.name, table.module);
            let width = table
                .columns
                .iter()
                .map(|c| c.name.len())
                .max()
                .unwrap_or(0);
            for col in &table.columns {
                let flags = if col.pk {
                    " pk"
                } else if col.nullable {
                    " null"
                } else {
                    ""
                };
                println!("  {:width$}  {}{}", col.name, col.r#type, flags);
            }
        }
    }
    Ok(())
}

fn cmd_package(
    docker: bool,
    binary: bool,
    k8s: bool,
    systemd: bool,
    json_mode: bool,
) -> Result<(), Failure> {
    let root = app_root()?;
    let design = load_design(&root.join("design.json"))?;

    // The CLI and the MCP `jerrycan_package` tool share this one orchestration.
    let (artifacts, sbom) = package::run_package(&root, &design, docker, k8s, systemd, binary)
        .map_err(Failure::gate)?;

    let payload = serde_json::json!({
        "artifacts": artifacts,
        "sbom": sbom,
        "next_step": "deploy with your own tooling (kubectl apply -f deploy/k8s.yaml, docker build, scp the binary + systemd unit)",
    });
    emit(
        json_mode,
        &payload,
        &format!("packaged {} artifact(s)", artifacts.len()),
    );
    Ok(())
}

fn cmd_deploy(target: &str, json_mode: bool) -> Result<(), Failure> {
    let root = app_root()?;
    let design = load_design(&root.join("design.json"))?;
    let artifacts = jerrycan::platform::deploy::emit(target, &design).map_err(Failure::gate)?;
    for (rel, contents) in &artifacts {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| Failure::gate(e.to_string()))?;
        }
        std::fs::write(&path, contents).map_err(|e| Failure::gate(e.to_string()))?;
        // The scripts must be executable.
        #[cfg(unix)]
        if rel.ends_with(".sh") {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path)
                .map_err(|e| Failure::gate(e.to_string()))?
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).map_err(|e| Failure::gate(e.to_string()))?;
        }
    }
    // Keep deploy state (resource ids) out of version control. Derived from the
    // target so it stays correct as more targets land (not hardcoded `render`).
    let gitignore = root.join(".gitignore");
    let line = format!("deploy/{target}/.deploy-state.json");
    let line = line.as_str();
    let cur = std::fs::read_to_string(&gitignore).unwrap_or_default();
    if !cur.lines().any(|l| l.trim() == line) {
        let mut next = cur;
        if !next.is_empty() && !next.ends_with('\n') {
            next.push('\n');
        }
        next.push_str(line);
        next.push('\n');
        std::fs::write(&gitignore, next).map_err(|e| Failure::gate(e.to_string()))?;
    }
    let written: Vec<&str> = artifacts.iter().map(|(p, _)| p.as_str()).collect();
    let payload = serde_json::json!({
        "target": target,
        "artifacts": written,
        "next_step": format!(
            "set the platform key and run the script, e.g. `RENDER_API_KEY=… ./deploy/{target}/deploy.sh`"
        ),
    });
    emit(
        json_mode,
        &payload,
        &format!("deploy kit for `{target}` written"),
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cmd_migrate(
    from: &str,
    export_dir: Option<&Path>,
    live: Option<&str>,
    out: Option<&Path>,
    name: Option<&str>,
    bulk_threshold: usize,
    json_mode: bool,
) -> Result<(), Failure> {
    use jerrycan::platform::migrate::gaps::Severity;
    use jerrycan::platform::migrate::{self, MigrateOptions};

    if from != "supabase" {
        return Err(Failure::usage(format!(
            "unknown migration source `{from}` — supported: supabase"
        )));
    }
    // Default app name/out dir from the export dir (offline) or a required --name (live).
    let default_name = export_dir
        .and_then(|d| d.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("app")
        .to_string();
    let app_name = name.unwrap_or(&default_name).to_string();
    let out_dir = out
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(&app_name));

    let output = if let Some(conn) = live {
        eprintln!(
            "warning: --live reads a production database — offline export is the supported CI path"
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| Failure::environment(e.to_string()))?;
        runtime
            .block_on(migrate::run_migrate_live(
                conn,
                &MigrateOptions {
                    export_dir: PathBuf::new(),
                    out_dir: out_dir.clone(),
                    name: name.map(str::to_string),
                    bulk_threshold,
                },
            ))
            .map_err(Failure::gate)?
    } else {
        let dir = export_dir
            .ok_or_else(|| Failure::usage("provide the export directory (or --live <conn>)"))?;
        migrate::run_migrate(&MigrateOptions {
            export_dir: dir.to_path_buf(),
            out_dir: out_dir.clone(),
            name: name.map(str::to_string),
            bulk_threshold,
        })
        .map_err(Failure::gate)?
    };

    let blocking = output
        .gaps
        .iter()
        .filter(|g| g.severity == Severity::Blocking)
        .count();
    let advisory = output.gaps.len() - blocking;
    let first_module = output
        .design
        .modules
        .first()
        .map(|m| m.name.clone())
        .unwrap_or_else(|| "app".into());
    let bucket_count = output
        .design
        .storage
        .as_ref()
        .map(|s| s.buckets.len())
        .unwrap_or(0);
    let realtime_count = output
        .design
        .realtime
        .as_ref()
        .map(|r| r.changes.len())
        .unwrap_or(0);
    let entity_count: usize = output.design.modules.iter().map(|m| m.entities.len()).sum();
    let out_disp = out_dir.display().to_string();
    let payload = serde_json::json!({
        "created": output.created,
        "design": format!("{out_disp}/design.json"),
        "gap_report": { "path": format!("{out_disp}/gap-report.json"), "blocking": blocking, "advisory": advisory },
        "seed": { "tables": output.seed.tables, "bulk_tables": output.seed.bulk_tables, "rows": output.seed.rows },
        "next_step": format!("cd {out_disp} && jerrycan db migrate && jerrycan db seed && jerrycan gen-tests --module {first_module} && jerrycan check — then work gap-report.json top-down"),
    });
    emit(
        json_mode,
        &payload,
        &format!(
            "migrated {entity_count} entities, {bucket_count} buckets, {realtime_count} realtime channels — {} gap items ({blocking} blocking)",
            output.gaps.len()
        ),
    );
    Ok(())
}

fn cmd_list_routes(json_mode: bool) -> Result<(), Failure> {
    let root = app_root()?;
    let design = load_design(&root.join("design.json"))?;
    let mut routes = genroute::route_map(&design);
    // The tool-owned member-management routes (#107) are registered at
    // `App::build` but live outside the design's endpoint table — append them
    // so the listing shows the full live surface (empty without tenancy).
    routes.extend(genroute::implicit_member_routes(&design));
    let payload = serde_json::json!({ "routes": routes });
    let mut human = String::new();
    for r in &routes {
        human.push_str(&format!(
            "{:6} {}  →  {}::{}\n",
            r.method, r.path, r.module, r.handler
        ));
    }
    emit(json_mode, &payload, human.trim_end());
    Ok(())
}

fn cmd_check(module: Option<&str>, no_fail_fast: bool, json_mode: bool) -> Result<(), Failure> {
    let root = app_root()?;
    let design = load_design(&root.join("design.json"))?;

    // audit/deny are workspace-global supply-chain gates that run_all skips in
    // module scope; surface that to the human so a full check isn't forgotten.
    if module.is_some() {
        eprintln!(
            "note: audit/deny skipped in module scope — run a full `jerrycan check` before packaging"
        );
    }

    // Same shared core the MCP twin runs — drift between CLI and MCP is impossible.
    let report =
        checkpipe::run_all(&root, &design, module, no_fail_fast).map_err(Failure::environment)?;
    if json_mode {
        println!(
            "{}",
            serde_json::to_string(&report).expect("report serializes")
        );
    }
    // --no-fail-fast: show the per-target red→green split for the human too.
    for m in &report.test_modules {
        eprintln!(
            "tests[{}]: {} passed, {} failed",
            m.module, m.passed, m.failed
        );
    }
    for d in &report.diagnostics {
        eprintln!("error[{}]: {}", d.code, d.message);
        if let (Some(f), Some(l)) = (&d.file, d.line) {
            eprintln!("  --> {f}:{l}");
        } else if let Some(f) = &d.file {
            eprintln!("  --> {f}");
        }
        if let Some(s) = &d.suggestion {
            eprintln!("  = help: {s}");
        }
        if let Some(u) = &d.doc_url {
            eprintln!("  = docs: {u}");
        }
    }
    if report.ok {
        eprintln!("check: all green");
        Ok(())
    } else {
        // In --json mode the report above is this failure's stdout document, so
        // the sink must not add the generic envelope on top of it.
        let f = Failure::gate(report.next_step);
        Err(if json_mode { f.mark_json_emitted() } else { f })
    }
}

fn cmd_test(module: Option<&str>) -> Result<(), Failure> {
    let root = app_root()?;
    let mut c = std::process::Command::new("cargo");
    c.current_dir(&root).arg("test");
    match module {
        Some(m) => c.args(["-p", &format!("route-{m}")]),
        None => c.arg("--workspace"),
    };
    let status = c
        .status()
        .map_err(|e| Failure::environment(e.to_string()))?;
    if status.success() {
        Ok(())
    } else {
        Err(Failure::gate("test suite failed"))
    }
}

fn cmd_gen_tests(module: &str, json_mode: bool) -> Result<(), Failure> {
    let root = app_root()?;
    let design = load_design(&root.join("design.json"))?;
    let (rel, count) = jerrycan::platform::testgen::write_acceptance(&root, &design, module)
        .map_err(Failure::usage)?;
    // The declared jobs get their own tool-owned acceptance tests (direct
    // task-fn calls), which fail on the stubs exactly like the HTTP ones — so
    // they count toward `expected_failing` too.
    let jobs = jerrycan::platform::jobsgen::write_jobs_acceptance(&root, &design)
        .map_err(Failure::usage)?;
    let mut tests_created = vec![rel.clone()];
    let mut count = count;
    // The jobs acceptance tests are written ONCE (jobs are top-level, not
    // per-module), so `jobs_count` is added to the total exactly once.
    let has_jobs = jobs.is_some();
    if let Some((jobs_rel, jobs_count)) = jobs {
        tests_created.push(jobs_rel);
        count += jobs_count;
    }
    // When the design declares jobs, their acceptance tests live in package
    // `jobs` (not `route-{module}`), so point the operator at both.
    let next_step = if has_jobs {
        format!(
            "cargo test -p route-{module} && cargo test -p jobs (expect {count} failures total), implement handlers + job tasks, iterate"
        )
    } else {
        format!(
            "cargo test -p route-{module} (expect {count} failures), implement handlers, iterate"
        )
    };
    let payload = serde_json::json!({
        "tests_created": tests_created,
        "expected_failing": count,
        "next_step": next_step,
    });
    emit(
        json_mode,
        &payload,
        &format!("{count} acceptance tests written to {rel}"),
    );
    Ok(())
}

fn cmd_dev(addr: Option<&str>) -> Result<(), Failure> {
    use jerrycan::platform::newest_mtime;
    let root = app_root()?;
    eprintln!("jerrycan dev: watching {} (Ctrl-C to stop)", root.display());
    loop {
        let stamp = newest_mtime(&root);
        let mut child = {
            let mut c = std::process::Command::new("cargo");
            c.current_dir(&root).args(["run", "-p", "app"]);
            if let Some(a) = addr {
                c.env("JERRYCAN_ADDR", a);
            }
            c.spawn()
                .map_err(|e| Failure::environment(format!("cargo run failed to start: {e}")))?
        };
        // Poll for changes (or child exit, e.g. compile error) every 500ms.
        loop {
            std::thread::sleep(std::time::Duration::from_millis(500));
            if let Ok(Some(status)) = child.try_wait() {
                eprintln!("app exited ({status}); waiting for changes…");
                while newest_mtime(&root) <= stamp {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
                break;
            }
            if newest_mtime(&root) > stamp {
                eprintln!("change detected — restarting");
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
        }
    }
}
