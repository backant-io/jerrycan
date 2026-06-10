//! The jerrycan binary: CLI + `jerrycan mcp` (stdio MCP server).
#![forbid(unsafe_code)]

use clap::{Parser, Subcommand};
use jerrycan::platform::design::Design;
use jerrycan::platform::{
    EXIT_OK, EXIT_USAGE, Failure, checkpipe, genroute, mounting, questions, scaffold,
};
use std::path::{Path, PathBuf};

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
    },
    /// Run the app's (or one module's) test suite
    Test {
        #[arg(long)]
        module: Option<String>,
    },
    /// AI-native docs, offline
    Docs {
        topic: Option<String>,
        #[arg(long)]
        search: Option<String>,
    },
    /// Serve MCP over stdio
    Mcp,
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

    let result: Result<(), Failure> = run(cli);
    match result {
        Ok(()) => std::process::exit(EXIT_OK),
        Err(f) => {
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
        },
        Cmd::List {
            what: ListCmd::Routes,
        } => cmd_list_routes(cli.json),
        Cmd::Dev { addr } => cmd_dev(addr.as_deref()),
        Cmd::Check { module } => cmd_check(module.as_deref(), cli.json),
        Cmd::Test { module } => cmd_test(module.as_deref()),
        Cmd::Docs { topic, search } => cmd_docs(topic.as_deref(), search.as_deref(), cli.json),
        Cmd::Mcp => jerrycan::platform::mcp::serve_stdio().map_err(Failure::environment),
    }
}

fn cmd_docs(topic: Option<&str>, query: Option<&str>, json_mode: bool) -> Result<(), Failure> {
    use jerrycan::platform::docsidx;
    if let Some(q) = query {
        let results = docsidx::search(q, 5);
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
        return Err(Failure::usage(
            "provide a topic (`jerrycan docs dependencies`) or --search <query>",
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

fn emit(json_mode: bool, payload: &serde_json::Value, human: &str) {
    if json_mode {
        println!("{payload}");
    }
    eprintln!("{human}");
}

fn load_design(path: &Path) -> Result<Design, Failure> {
    Design::from_path(path).map_err(Failure::usage)
}

/// Validate; on questions emit the jerrycan_design-shaped payload and exit 1.
fn require_complete(design: &Design, json_mode: bool) -> Result<(), Failure> {
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
    Err(Failure::gate(human))
}

fn cmd_new(target: &str, design_path: &str, json_mode: bool) -> Result<(), Failure> {
    let design = load_design(Path::new(design_path))?;
    require_complete(&design, json_mode)?;
    let created = scaffold::scaffold(Path::new(target), &design).map_err(Failure::gate)?;
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
    let created =
        genroute::write_module(&root.join("crates/routes"), top_module).map_err(Failure::gate)?;
    let modified = mounting::regenerate(&root, &design).map_err(Failure::gate)?;
    let payload = serde_json::json!({
        "created": created,
        "modified": modified,
        "next_step": format!("implement crates/routes/{top}/src/handlers.rs, then jerrycan check --module {top}"),
    });
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

fn cmd_list_routes(json_mode: bool) -> Result<(), Failure> {
    let root = app_root()?;
    let design = load_design(&root.join("design.json"))?;
    let routes = genroute::route_map(&design);
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

fn cmd_check(module: Option<&str>, json_mode: bool) -> Result<(), Failure> {
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
    let report = checkpipe::run_all(&root, &design, module).map_err(Failure::environment)?;
    if json_mode {
        println!(
            "{}",
            serde_json::to_string(&report).expect("report serializes")
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
        Err(Failure::gate(report.next_step))
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
