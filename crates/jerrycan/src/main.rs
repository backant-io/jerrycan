//! The jerrycan binary: CLI + `jerrycan mcp` (stdio MCP server).
#![forbid(unsafe_code)]

use clap::{Parser, Subcommand};
use jerrycan::platform::design::Design;
use jerrycan::platform::{EXIT_OK, EXIT_USAGE, Failure, genroute, mounting, questions, scaffold};
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
        _ => Err(Failure::usage("this command lands in a later Phase 1 task")),
    }
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
    let mut routes = Vec::new();
    fn walk(
        m: &jerrycan::platform::design::ModuleDesign,
        prefix: &str,
        top: &str,
        routes: &mut Vec<serde_json::Value>,
    ) {
        let base = format!("{}{}", prefix, m.effective_mount());
        for ep in &m.endpoints {
            let full = format!("{}{}", base.trim_end_matches('/'), ep.path);
            routes.push(serde_json::json!({
                "method": format!("{:?}", ep.method),
                "path": full,
                "module": top,
                "handler": ep.operation_id,
            }));
        }
        for sub in &m.subroutes {
            walk(sub, &base, top, routes);
        }
    }
    for m in &design.modules {
        walk(m, "", &m.name, &mut routes);
    }
    let payload = serde_json::json!({ "routes": routes });
    let mut human = String::new();
    for r in payload["routes"].as_array().unwrap() {
        human.push_str(&format!(
            "{:6} {}  →  {}::{}\n",
            r["method"].as_str().unwrap(),
            r["path"].as_str().unwrap(),
            r["module"].as_str().unwrap(),
            r["handler"].as_str().unwrap()
        ));
    }
    emit(json_mode, &payload, human.trim_end());
    Ok(())
}
