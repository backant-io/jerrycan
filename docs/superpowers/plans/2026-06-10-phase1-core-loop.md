# jerrycan Phase 1 — Core Loop Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the `jerrycan` platform binary — CLI + MCP stdio server — that deterministically scaffolds crate-per-module workspaces from `design.json`, regenerates mounting, verifies with machine-readable diagnostics, and proves the Phase 1 exit criterion: *an agent generates a working multi-module in-memory CRUD service via MCP only*.

**Architecture:** All platform logic lives in `crates/jerrycan/src/platform/` behind a default-on `cli` cargo feature (generated apps depend on the facade with `default-features = false`, so none of it reaches app builds). One shared core (`design` → `scaffold`/`generate` → `check`) is rendered two ways: clap CLI commands and MCP `tools/call` — the cli-ux.md "one pipeline, two renderings" promise is structural. The MCP server is a hand-rolled, synchronous, newline-delimited JSON-RPC 2.0 loop over stdio (~300 lines, zero new deps) serving the frozen contracts in `docs/contracts/mcp-tools.json` — which the binary embeds, so `tools/list` can never drift from the contract file.

**Tech Stack:** Existing workspace deps + `clap 4` (derive, cli-feature-only) + `tempfile` (dev-dep). External tools invoked by `check`: `cargo`, `cargo-audit`, `cargo-deny` (missing → exit 3 with install hint, per cli-ux.md). No template engine (own 20-line `{{placeholder}}` substitution), no file-watch crate (mtime polling), no MCP SDK (hand-rolled; migration to an SDK is a tracked fallback if client compat issues ever appear).

**Source contracts (read before implementing anything):**
- `docs/contracts/mcp-tools.json` — tool names/schemas the MCP server MUST serve verbatim
- `docs/contracts/design-schema.json` — what `design.json` may contain
- `docs/contracts/cli-ux.md` — commands, exit codes (0 ok / 1 gate failed / 2 usage / 3 environment), output conventions (`--json` = the MCP payload on stdout; human text on stderr)
- `docs/superpowers/specs/2026-06-09-jerrycan-design.md` §5 (generated app anatomy), §7 (platform)

**Scope decision (surfaced, not blended):** This plan covers the *platform core loop* only — the Phase 1 roadmap row. The framework-hardening items in `docs/phase1-backlog.md` (security headers, timeouts, graceful shutdown, percent-decoding, accept-loop backoff, panic→500) are deliberately a separate follow-up plan ("Phase 1b") so this plan exits exactly at the roadmap's exit criterion. `jerrycan_gen_tests` (Phase 2) and `jerrycan_package` (Phase 3) are served by the MCP as structured not-implemented responses with honest `next_step` hints.

---

## File Structure

```
crates/jerrycan/
├── Cargo.toml                      # MODIFY: [[bin]], cli feature, clap, tempfile dev-dep
├── src/
│   ├── lib.rs                      # MODIFY: + #[cfg(feature="cli")] pub mod platform;
│   ├── main.rs                     # CREATE: clap tree → dispatch → exit codes
│   └── platform/
│       ├── mod.rs                  # CREATE: module wiring + Outcome/exit-code types
│       ├── design.rs               # CREATE: typed Design structs (serde, deny_unknown_fields)
│       ├── questions.rs            # CREATE: validation → pointed Questions (the design engine)
│       ├── templates.rs            # CREATE: embedded templates + render()
│       ├── scaffold.rs             # CREATE: design → workspace on disk (new)
│       ├── mounting.rs             # CREATE: deterministic app/main.rs + workspace-members regenerator
│       ├── genroute.rs             # CREATE: route/subroute/dep generation incl. handler-signature mapping
│       ├── checkpipe.rs            # CREATE: build/clippy/test/audit/deny orchestration → Diagnostics
│       ├── lints.rs                # CREATE: JL0001 public-surface, JL0002 naming, JL0003 generated-drift
│       ├── docsidx.rs              # CREATE: embedded docs/ai search + get
│       └── mcp.rs                  # CREATE: JSON-RPC 2.0 stdio loop + tools/call dispatch
├── tests/
│   ├── cli.rs                      # CREATE: fast CLI tests (usage, --json shapes, list/docs)
│   ├── generation.rs               # CREATE: scaffold/generate/mounting determinism (tempdir)
│   ├── checkpipe.rs                # CREATE: diagnostic parsing on fixtures; lint tests
│   ├── mcp.rs                      # CREATE: JSON-RPC harness over the spawned binary
│   └── conformance.rs              # CREATE: #[ignore] heavy: full CLI loop + agent-sim MCP loop
conformance/
├── designs/todo-api.design.json    # CREATE: golden multi-module design (todos + nested comments + users)
└── fixtures/                       # CREATE: canned handler bodies the "agent" injects
    ├── todos_handlers.rs
    ├── comments_handlers.rs
    └── users_handlers.rs
docs/contracts/mcp-tools.json       # MODIFY: jerrycan_design gains optional `draft` input (v0 amendment)
crates/jerrycan/tests/contracts.rs  # MODIFY: pin the amendment
docs/ai/                            # MODIFY: close the 6 documented gaps (backlog "Docs page additions")
.github/workflows/ci.yml            # MODIFY: install audit/deny, run heavy tests
README.md / specs roadmap           # MODIFY: flip Phase 1 row at the end
```

**Conventions for every task** (identical to Phase 0 execution): run from repo root; before EVERY commit `cargo fmt --all` && `cargo clippy --workspace --all-targets -- -D warnings` && `cargo test --workspace` green; commit messages plain "what changed" (no Co-Authored-By/Claude lines, repo rule); `#![forbid(unsafe_code)]` everywhere; heavy tests are `#[ignore]` and run explicitly in CI.

---

### Task 1: Contract v0 amendment — `jerrycan_design` accepts a `draft`

The design tool is deterministic (no model inside): the AGENT authors the design; the tool **forces specificity** by validating and returning pointed questions. The frozen contract has `requirements` (prose) + `answers` but no slot for the structured draft. Amend v0 additively.

**Files:**
- Modify: `docs/contracts/mcp-tools.json` (jerrycan_design inputSchema)
- Modify: `crates/jerrycan/tests/contracts.rs`

- [ ] **Step 1: Write the failing assertion**

In `mcp_tools_contract_holds_its_invariants` (crates/jerrycan/tests/contracts.rs), after the existing design_path assertion, add:

```rust
    let design_tool = tools
        .iter()
        .find(|t| t["name"] == "jerrycan_design")
        .expect("design tool");
    assert!(
        design_tool["inputSchema"]["properties"]["draft"]["type"] == "object",
        "jerrycan_design must accept a structured draft object (deterministic validation engine)"
    );
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p jerrycan --test contracts`
Expected: FAIL — `draft` property absent.

- [ ] **Step 3: Amend the contract**

In `docs/contracts/mcp-tools.json`, inside `jerrycan_design.inputSchema.properties`, after `"requirements"`, add:

```json
          "draft": { "type": "object", "description": "A structured design draft conforming to design-schema.json. The tool validates it deterministically: violations come back as pointed questions; a complete draft is written to disk and returned with design_path. Omit on a first exploratory call to receive the design template." },
```

Also update `jerrycan_design.description` to:

```json
      "description": "Turn requirements into a validated design.json. The agent authors the design; this tool enforces specificity: call without a draft to get the design template, call with a draft to validate it — violations return pointed questions (not code), a complete draft is written to design.json and echoed with design_path. Call repeatedly until status=complete.",
```

- [ ] **Step 4: Amend the design schema with `mount` (subroute/module mount prefix)**

Generators need to know WHERE a module mounts. v0 default is `/` + module name; an explicit `mount` makes it overridable. In `docs/contracts/design-schema.json`, inside `$defs.module.properties`, after `"name"`, add:

```json
        "mount": { "type": "string", "pattern": "^/", "description": "Mount prefix for this module (under the app for top-level modules, under the parent for subroutes). Defaults to '/' + name. v0 limitation: path parameters in mount prefixes interact with single-param Path extraction — prefer parameter-free mounts until multi-param Path lands." },
```

And add to the contracts test (`design_schema_is_module_grouped_and_recursive`):

```rust
    assert_eq!(doc["$defs"]["module"]["properties"]["mount"]["pattern"], "^/");
```

- [ ] **Step 5: Run to verify pass**

Run: `cargo test -p jerrycan --test contracts` → 3 passed. `python3 -m json.tool docs/contracts/mcp-tools.json > /dev/null` and same for `design-schema.json` → silent.

- [ ] **Step 6: Commit**

```bash
git add docs/contracts/mcp-tools.json docs/contracts/design-schema.json crates/jerrycan/tests/contracts.rs
git commit -m "Amend contracts: jerrycan_design draft input and module mount prefix"
```

---

### Task 2: Binary skeleton — clap tree, exit codes, `--json` plumbing

**Files:**
- Modify: `crates/jerrycan/Cargo.toml`
- Modify: `crates/jerrycan/src/lib.rs`
- Create: `crates/jerrycan/src/main.rs`
- Create: `crates/jerrycan/src/platform/mod.rs`
- Create: `crates/jerrycan/tests/cli.rs`

- [ ] **Step 1: Write the failing tests**

Create `crates/jerrycan/tests/cli.rs`:

```rust
//! Fast CLI contract tests. Exit codes per docs/contracts/cli-ux.md:
//! 0 ok · 1 gate failed · 2 usage error · 3 environment error.

use std::process::Command;

fn jerrycan() -> Command {
    Command::new(env!("CARGO_BIN_EXE_jerrycan"))
}

#[test]
fn version_prints_and_exits_zero() {
    let out = jerrycan().arg("--version").output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn unknown_flag_is_usage_error_exit_2() {
    let out = jerrycan().arg("--definitely-not-a-flag").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn missing_required_arg_is_usage_error_exit_2() {
    // `new` requires --design; no interactive prompts ever (cli-ux.md non-goals).
    let out = jerrycan().args(["new", "demo"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--design"), "must name the exact missing flag: {err}");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p jerrycan --test cli`
Expected: compile FAILURE — no binary target exists yet.

- [ ] **Step 3: Wire the binary**

Replace `crates/jerrycan/Cargo.toml` with:

```toml
[package]
name = "jerrycan"
description = "The AI-native Rust backend platform: framework, CLI, and MCP server. Name reservation; development at https://jerrycan.cc — real releases begin at 0.1.0."
version.workspace = true
edition.workspace = true
license.workspace = true
homepage.workspace = true

[features]
# Generated apps depend on this crate with `default-features = false` (lib facade
# only). The default ON keeps `cargo install jerrycan` working for the platform.
default = ["cli"]
cli = ["dep:clap", "dep:serde", "dep:serde_json"]

[[bin]]
name = "jerrycan"
required-features = ["cli"]

[dependencies]
jerrycan-core = { path = "../jerrycan-core", version = "0.0.0" }
jerrycan-macros = { path = "../jerrycan-macros", version = "0.0.0" }
clap = { version = "4", features = ["derive"], optional = true }
serde = { workspace = true, optional = true }
serde_json = { workspace = true, optional = true }

[dev-dependencies]
tokio.workspace = true
serde.workspace = true
serde_json.workspace = true
tempfile = "3"
```

In `crates/jerrycan/src/lib.rs`, after the existing re-exports, add:

```rust
#[cfg(feature = "cli")]
pub mod platform;
```

Create `crates/jerrycan/src/platform/mod.rs`:

```rust
//! The jerrycan platform: shared core consumed by both the CLI (main.rs) and
//! the MCP server (platform::mcp). One pipeline, two renderings (cli-ux.md).
#![allow(clippy::module_name_repetitions)]

pub mod design;
pub mod questions;
pub mod templates;
pub mod scaffold;
pub mod mounting;
pub mod genroute;
pub mod checkpipe;
pub mod lints;
pub mod docsidx;
pub mod mcp;

/// Exit codes per docs/contracts/cli-ux.md.
pub const EXIT_OK: i32 = 0;
pub const EXIT_GATE_FAILED: i32 = 1;
pub const EXIT_USAGE: i32 = 2;
pub const EXIT_ENVIRONMENT: i32 = 3;

/// A platform-level failure that knows its exit code and (in --json mode) its payload.
#[derive(Debug)]
pub struct Failure {
    pub exit: i32,
    pub message: String,
}

impl Failure {
    pub fn usage(msg: impl Into<String>) -> Self {
        Self { exit: EXIT_USAGE, message: msg.into() }
    }
    pub fn environment(msg: impl Into<String>) -> Self {
        Self { exit: EXIT_ENVIRONMENT, message: msg.into() }
    }
    pub fn gate(msg: impl Into<String>) -> Self {
        Self { exit: EXIT_GATE_FAILED, message: msg.into() }
    }
}

pub type PResult<T> = Result<T, Failure>;
```

For this task only, create the other platform modules as one-line stubs so the tree compiles (each replaced by its own task): `design.rs`, `questions.rs`, `templates.rs`, `scaffold.rs`, `mounting.rs`, `genroute.rs`, `checkpipe.rs`, `lints.rs`, `docsidx.rs`, `mcp.rs`, each containing exactly:

```rust
//! Implemented in a later Phase 1 task.
```

Create `crates/jerrycan/src/main.rs`:

```rust
//! The jerrycan binary: CLI + `jerrycan mcp` (stdio MCP server).
#![forbid(unsafe_code)]

use clap::{Parser, Subcommand};
use jerrycan::platform::{Failure, EXIT_OK, EXIT_USAGE};

#[derive(Parser)]
#[command(name = "jerrycan", version, about = "The AI-native Rust backend platform")]
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
            use clap::error::ErrorKind;
            if matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) {
                print!("{e}");
                std::process::exit(EXIT_OK);
            }
            eprint!("{e}");
            std::process::exit(EXIT_USAGE);
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
        // Every arm is replaced by its own task; until then, fail honestly.
        _ => Err(Failure::usage("this command lands in a later Phase 1 task")),
    }
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p jerrycan --test cli`
Expected: 3 tests PASS (`new demo` without `--design` is a clap missing-arg error → exit 2 naming `--design`). Then the full gate: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace` — note the facade's lib tests and doc-tests must still pass with the feature present.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan Cargo.lock
git commit -m "Add jerrycan binary skeleton with clap CLI tree and contract exit codes"
```

---
### Task 3: Typed `Design` model (`design.rs`)

The single source of truth both generators and lints read. Mirrors `docs/contracts/design-schema.json` exactly — `deny_unknown_fields` is the serde twin of `additionalProperties: false`.

**Files:**
- Replace stub: `crates/jerrycan/src/platform/design.rs`

- [ ] **Step 1: Write the failing tests (unit tests in design.rs)**

Create `crates/jerrycan/src/platform/design.rs` with the tests first:

```rust
//! Typed model of design.json (docs/contracts/design-schema.json).
//! `deny_unknown_fields` mirrors the schema's `additionalProperties: false`.

use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) const MINIMAL: &str = r#"{
        "name": "demo-api",
        "contract_version": 0,
        "auth": { "model": "session", "roles": ["admin"] },
        "dependencies": ["db"],
        "modules": [{
            "name": "todos",
            "entities": [{ "name": "Todo", "fields": [
                { "name": "title", "type": "string" },
                { "name": "done", "type": "boolean", "required": false }
            ]}],
            "endpoints": [
                { "operation_id": "list_todos", "method": "GET", "path": "/",
                  "success": { "status": 200, "entity": "Todo", "list": true } },
                { "operation_id": "create_todo", "method": "POST", "path": "/",
                  "request_body": { "entity": "Todo" },
                  "success": { "status": 201, "entity": "Todo" } },
                { "operation_id": "delete_todo", "method": "DELETE", "path": "/{id}",
                  "required_roles": ["admin"],
                  "success": { "status": 204 },
                  "errors": [{ "status": 404, "code": "JC0404", "when": "unknown id" }] }
            ],
            "subroutes": [{
                "name": "comments",
                "endpoints": [{ "operation_id": "list_comments", "method": "GET", "path": "/",
                                "success": { "status": 200 } }]
            }]
        }]
    }"#;

    #[test]
    fn minimal_design_round_trips() {
        let d: Design = serde_json::from_str(MINIMAL).unwrap();
        assert_eq!(d.name, "demo-api");
        assert_eq!(d.modules[0].endpoints.len(), 3);
        assert_eq!(d.modules[0].subroutes[0].name, "comments");
        assert!(d.modules[0].entities[0].fields[0].required); // default true
        assert!(!d.modules[0].entities[0].fields[1].required);
        let back = serde_json::to_string(&d).unwrap();
        let _re: Design = serde_json::from_str(&back).unwrap(); // serializable both ways
    }

    #[test]
    fn unknown_fields_are_rejected_like_additional_properties_false() {
        let bad = MINIMAL.replacen("\"name\": \"demo-api\",", "\"name\": \"demo-api\", \"surprise\": 1,", 1);
        assert!(serde_json::from_str::<Design>(&bad).is_err());
    }

    #[test]
    fn method_enum_rejects_options() {
        let bad = MINIMAL.replace("\"GET\"", "\"OPTIONS\"");
        assert!(serde_json::from_str::<Design>(&bad).is_err());
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p jerrycan design` → compile FAILURE (`Design` undefined).

- [ ] **Step 3: Implement the model (above the tests)**

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Design {
    pub name: String,
    pub contract_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<Auth>,
    /// App-scoped dependency names the generator must provide on App.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<String>,
    pub modules: Vec<ModuleDesign>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Auth {
    pub model: AuthModel,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthModel {
    None,
    Session,
    Jwt,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModuleDesign {
    pub name: String,
    /// Mount prefix; defaults to "/" + name (see `effective_mount`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mount: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<Entity>,
    pub endpoints: Vec<Endpoint>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subroutes: Vec<ModuleDesign>,
    /// Module-scoped dependency names the generator must stub.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entity {
    pub name: String,
    pub fields: Vec<Field>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Field {
    pub name: String,
    #[serde(rename = "type")]
    pub field_type: FieldType,
    #[serde(default = "default_true")]
    pub required: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FieldType {
    String,
    Integer,
    Float,
    Boolean,
    Datetime,
    Uuid,
    Json,
}

impl FieldType {
    /// The Rust type the generator emits. datetime/uuid ride as String until
    /// jerrycan-validate lands richer types in Phase 2 (documented in templates).
    pub fn rust_type(self) -> &'static str {
        match self {
            FieldType::String | FieldType::Datetime | FieldType::Uuid => "String",
            FieldType::Integer => "i64",
            FieldType::Float => "f64",
            FieldType::Boolean => "bool",
            FieldType::Json => "serde_json::Value",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Endpoint {
    pub operation_id: String,
    pub method: HttpMethod,
    pub path: String,
    #[serde(default)]
    pub auth_required: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_roles: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_body: Option<RequestBody>,
    pub success: Success,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<ErrorCase>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HttpMethod {
    GET,
    POST,
    PUT,
    PATCH,
    DELETE,
}

impl HttpMethod {
    /// The jerrycan-core free-fn name used in generated `module()` route tables.
    pub fn builder_fn(self) -> &'static str {
        match self {
            HttpMethod::GET => "get",
            HttpMethod::POST => "post",
            HttpMethod::PUT => "put",
            HttpMethod::PATCH => "patch",
            HttpMethod::DELETE => "delete",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestBody {
    pub entity: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Success {
    pub status: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
    #[serde(default)]
    pub list: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorCase {
    pub status: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub when: String,
}

impl ModuleDesign {
    /// Where this module mounts (under the app, or under its parent for subroutes).
    pub fn effective_mount(&self) -> String {
        self.mount.clone().unwrap_or_else(|| format!("/{}", self.name))
    }
}

impl Design {
    pub fn from_path(path: &std::path::Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        serde_json::from_str(&raw).map_err(|e| format!("invalid design.json: {e}"))
    }
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p jerrycan design` → 3 tests PASS. Full gate green.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan/src/platform/design.rs
git commit -m "Add typed Design model mirroring the design.json schema"
```

---

### Task 4: The questioning engine (`questions.rs`)

Deterministic validation that returns **pointed questions** (id + question), exactly what `jerrycan_design` sends back for incomplete drafts. IDs are JSON-pointer paths so an agent can patch precisely.

**Files:**
- Replace stub: `crates/jerrycan/src/platform/questions.rs`

- [ ] **Step 1: Write the failing tests**

```rust
//! Deterministic design validation → pointed questions (jerrycan_design's engine).

use super::design::*;
use serde::Serialize;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::design::tests::MINIMAL;

    fn design(json: &str) -> Design {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn complete_design_yields_no_questions() {
        assert!(validate(&design(MINIMAL)).is_empty());
    }

    #[test]
    fn bad_names_yield_pointed_questions_with_json_pointer_ids() {
        let d = design(&MINIMAL.replace("\"name\": \"demo-api\"", "\"name\": \"Demo API\""));
        let qs = validate(&d);
        assert!(qs.iter().any(|q| q.id == "/name" && q.question.contains("kebab-case")), "{qs:?}");
    }

    #[test]
    fn duplicate_operation_ids_and_routes_are_caught() {
        let d = design(&MINIMAL.replace("\"operation_id\": \"create_todo\"", "\"operation_id\": \"list_todos\""));
        let qs = validate(&d);
        assert!(qs.iter().any(|q| q.id.starts_with("/modules/0/endpoints") && q.question.contains("unique")));

        let d2 = design(&MINIMAL.replace(
            "{ \"operation_id\": \"create_todo\", \"method\": \"POST\", \"path\": \"/\",",
            "{ \"operation_id\": \"create_todo\", \"method\": \"GET\", \"path\": \"/\",",
        ));
        let qs2 = validate(&d2);
        assert!(qs2.iter().any(|q| q.question.contains("GET /") && q.question.contains("already")), "{qs2:?}");
    }

    #[test]
    fn roles_must_be_declared_and_entities_must_exist() {
        let d = design(&MINIMAL.replace("\"required_roles\": [\"admin\"]", "\"required_roles\": [\"superuser\"]"));
        let qs = validate(&d);
        assert!(qs.iter().any(|q| q.question.contains("superuser") && q.question.contains("auth.roles")));

        let d2 = design(&MINIMAL.replace("\"request_body\": { \"entity\": \"Todo\" }", "\"request_body\": { \"entity\": \"Ghost\" }"));
        let qs2 = validate(&d2);
        assert!(qs2.iter().any(|q| q.question.contains("Ghost")));
    }

    #[test]
    fn status_ranges_and_path_shape_are_enforced() {
        let d = design(&MINIMAL.replace("\"status\": 204", "\"status\": 302"));
        assert!(validate(&d).iter().any(|q| q.question.contains("2xx")));
        let d2 = design(&MINIMAL.replace("\"path\": \"/{id}\"", "\"path\": \"{id}\""));
        assert!(validate(&d2).iter().any(|q| q.question.contains("start with '/'")));
    }

    #[test]
    fn v0_limits_one_path_param_and_validates_mount_prefix() {
        let d = design(&MINIMAL.replace("\"path\": \"/{id}\"", "\"path\": \"/{id}/tags/{tag}\""));
        assert!(validate(&d).iter().any(|q| q.question.contains("one path parameter")), "v0 single-param limit");

        let d2 = design(&MINIMAL.replace("\"name\": \"comments\",", "\"name\": \"comments\", \"mount\": \"comments\","));
        assert!(validate(&d2).iter().any(|q| q.id.contains("/mount") && q.question.contains("start with '/'")));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p jerrycan questions` → compile FAILURE (`validate`, `Question` undefined). Also make `mod tests` in design.rs expose `MINIMAL`: it already declares `pub(crate) const MINIMAL` inside `#[cfg(test)] mod tests` — add `pub(crate) use` visibility exactly as written in Task 3 (the path `crate::platform::design::tests::MINIMAL` works because both are `#[cfg(test)]` in the same crate).

- [ ] **Step 3: Implement (above the tests)**

```rust
/// One pointed question. `id` is a JSON-pointer into the draft.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Question {
    pub id: String,
    pub question: String,
}

fn q(id: impl Into<String>, question: impl Into<String>) -> Question {
    Question { id: id.into(), question: question.into() }
}

fn is_kebab(s: &str) -> bool {
    !s.is_empty()
        && s.starts_with(|c: char| c.is_ascii_lowercase())
        && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn is_snake(s: &str) -> bool {
    !s.is_empty()
        && s.starts_with(|c: char| c.is_ascii_lowercase())
        && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn is_pascal(s: &str) -> bool {
    !s.is_empty()
        && s.starts_with(|c: char| c.is_ascii_uppercase())
        && s.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Validate a parsed design. Empty result == complete (status: "complete").
pub fn validate(d: &Design) -> Vec<Question> {
    let mut qs = Vec::new();

    if !is_kebab(&d.name) {
        qs.push(q("/name", format!("`{}` is not kebab-case (^[a-z][a-z0-9-]*$) — what should the app be called?", d.name)));
    }
    if d.contract_version != 0 {
        qs.push(q("/contract_version", "contract_version must be 0 for this platform version."));
    }
    if d.modules.is_empty() {
        qs.push(q("/modules", "No modules defined — what are the resource areas of this backend (each becomes a route crate)?"));
    }

    let declared_roles: Vec<&str> = d.auth.as_ref().map(|a| a.roles.iter().map(String::as_str).collect()).unwrap_or_default();
    let auth_declared = d.auth.is_some();

    let mut seen_module_names = std::collections::HashSet::new();
    for (i, m) in d.modules.iter().enumerate() {
        if !seen_module_names.insert(m.name.as_str()) {
            qs.push(q(format!("/modules/{i}/name"), format!("Module name `{}` is already used — module names must be unique.", m.name)));
        }
        validate_module(m, &format!("/modules/{i}"), &declared_roles, auth_declared, &mut qs);
    }
    qs
}

fn validate_module(
    m: &ModuleDesign,
    ptr: &str,
    declared_roles: &[&str],
    auth_declared: bool,
    qs: &mut Vec<Question>,
) {
    if !is_kebab(&m.name) {
        qs.push(q(format!("{ptr}/name"), format!("Module `{}` is not kebab-case — rename it.", m.name)));
    }
    if let Some(ref mount) = m.mount {
        if !mount.starts_with('/') {
            qs.push(q(format!("{ptr}/mount"), format!("Mount `{mount}` must start with '/'.")));
        }
    }
    for (i, e) in m.entities.iter().enumerate() {
        if !is_pascal(&e.name) {
            qs.push(q(format!("{ptr}/entities/{i}/name"), format!("Entity `{}` must be PascalCase.", e.name)));
        }
        if e.fields.is_empty() {
            qs.push(q(format!("{ptr}/entities/{i}/fields"), format!("Entity `{}` has no fields — what data does it carry?", e.name)));
        }
        for (j, f) in e.fields.iter().enumerate() {
            if !is_snake(&f.name) {
                qs.push(q(format!("{ptr}/entities/{i}/fields/{j}/name"), format!("Field `{}` must be snake_case.", f.name)));
            }
        }
    }
    if m.endpoints.is_empty() {
        qs.push(q(format!("{ptr}/endpoints"), format!("Module `{}` has no endpoints — what operations does it expose?", m.name)));
    }

    let entity_names: Vec<&str> = m.entities.iter().map(|e| e.name.as_str()).collect();
    let mut seen_ops = std::collections::HashSet::new();
    let mut seen_routes = std::collections::HashSet::new();
    for (i, ep) in m.endpoints.iter().enumerate() {
        let eptr = format!("{ptr}/endpoints/{i}");
        if !is_snake(&ep.operation_id) {
            qs.push(q(format!("{eptr}/operation_id"), format!("operation_id `{}` must be snake_case (it becomes the handler fn name).", ep.operation_id)));
        }
        if !seen_ops.insert(ep.operation_id.as_str()) {
            qs.push(q(format!("{eptr}/operation_id"), format!("operation_id `{}` is not unique within module `{}` — handler names must be unique.", ep.operation_id, m.name)));
        }
        if !ep.path.starts_with('/') {
            qs.push(q(format!("{eptr}/path"), format!("Path `{}` must start with '/'.", ep.path)));
        }
        let param_count = ep.path.matches('{').count();
        if param_count > 1 {
            qs.push(q(format!("{eptr}/path"), format!("Path `{}` has {param_count} parameters — v0 supports one path parameter per endpoint (multi-param Path is on the roadmap). Split the route or use a subroute.", ep.path)));
        }
        if !seen_routes.insert((ep.method, ep.path.as_str())) {
            qs.push(q(format!("{eptr}/path"), format!("{:?} {} is already registered in module `{}` — routes must be unique.", ep.method, ep.path, m.name)));
        }
        if !(200..=299).contains(&ep.success.status) {
            qs.push(q(format!("{eptr}/success/status"), format!("Success status {} is not 2xx.", ep.success.status)));
        }
        if let Some(ref ent) = ep.success.entity {
            if !entity_names.contains(&ent.as_str()) {
                qs.push(q(format!("{eptr}/success/entity"), format!("Entity `{ent}` is not defined in module `{}` — define it or fix the reference.", m.name)));
            }
        }
        if let Some(ref rb) = ep.request_body {
            if !entity_names.contains(&rb.entity.as_str()) {
                qs.push(q(format!("{eptr}/request_body/entity"), format!("Entity `{}` is not defined in module `{}` — define it or fix the reference.", rb.entity, m.name)));
            }
        }
        for (j, ec) in ep.errors.iter().enumerate() {
            if !(400..=599).contains(&ec.status) {
                qs.push(q(format!("{eptr}/errors/{j}/status"), format!("Error status {} is not 4xx/5xx.", ec.status)));
            }
            if let Some(ref code) = ec.code {
                let ok = code.len() == 6 && code.starts_with("JC") && code[2..].chars().all(|c| c.is_ascii_digit());
                if !ok {
                    qs.push(q(format!("{eptr}/errors/{j}/code"), format!("`{code}` does not match ^JC[0-9]{{4}}$.")));
                }
            }
        }
        for role in &ep.required_roles {
            if !declared_roles.contains(&role.as_str()) {
                let hint = if auth_declared { "add it to auth.roles or fix the reference" } else { "declare auth { model, roles } first" };
                qs.push(q(format!("{eptr}/required_roles"), format!("Role `{role}` is not declared in auth.roles — {hint}.")));
            }
        }
    }

    for (i, sub) in m.subroutes.iter().enumerate() {
        validate_module(sub, &format!("{ptr}/subroutes/{i}"), declared_roles, auth_declared, qs);
    }
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p jerrycan questions` → 5 tests PASS. Full gate green.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan/src/platform/questions.rs crates/jerrycan/src/platform/design.rs
git commit -m "Add deterministic design validation with pointed JSON-pointer questions"
```

---
### Task 5: Embedded templates + `render()` (`templates.rs`)

Static scaffolding text lives here; dynamic per-module code is built in Task 6. No template engine — a 20-line `{{key}}` substitution.

**Files:**
- Replace stub: `crates/jerrycan/src/platform/templates.rs`

- [ ] **Step 1: Write the failing tests**

```rust
//! Embedded scaffolding templates + the {{key}} renderer. Static text only;
//! per-module code generation lives in genroute.rs.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_substitutes_all_keys_and_rejects_leftovers() {
        let out = render("hi {{who}}, {{who}}! v{{n}}", &[("who", "agent"), ("n", "0")]).unwrap();
        assert_eq!(out, "hi agent, agent! v0");
        let err = render("oops {{missing}}", &[]).unwrap_err();
        assert!(err.contains("missing"));
    }

    #[test]
    fn jerrycan_dep_spec_defaults_and_honors_env_override() {
        // NB: env mutation — run serially-safe by using a unique var read at call time.
        let default = jerrycan_dep_spec_from(None);
        assert_eq!(default, "jerrycan = { version = \"0.0.0\", default-features = false }");
        let local = jerrycan_dep_spec_from(Some("jerrycan = { path = \"/x\", default-features = false }".into()));
        assert!(local.contains("path = \"/x\""));
    }

    #[test]
    fn workspace_template_has_member_markers() {
        assert!(WORKSPACE_CARGO.contains("# jerrycan:members:begin"));
        assert!(WORKSPACE_CARGO.contains("# jerrycan:members:end"));
        assert!(APP_CARGO.contains("# jerrycan:route-deps:begin"));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p jerrycan templates` → compile FAILURE.

- [ ] **Step 3: Implement (above the tests)**

```rust
/// Substitute every `{{key}}`. Unreplaced keys are an error — templates can't rot silently.
pub fn render(template: &str, vars: &[(&str, &str)]) -> Result<String, String> {
    let mut out = template.to_string();
    for (k, v) in vars {
        out = out.replace(&format!("{{{{{k}}}}}"), v);
    }
    if let Some(start) = out.find("{{") {
        let tail: String = out[start..].chars().take(40).collect();
        return Err(format!("unsubstituted template key near `{tail}`"));
    }
    Ok(out)
}

/// How generated apps depend on the framework. Overridable for local development
/// and conformance tests via JERRYCAN_FRAMEWORK_DEP (a full Cargo dep line).
pub fn jerrycan_dep_spec() -> String {
    jerrycan_dep_spec_from(std::env::var("JERRYCAN_FRAMEWORK_DEP").ok())
}

pub(crate) fn jerrycan_dep_spec_from(env: Option<String>) -> String {
    env.unwrap_or_else(|| "jerrycan = { version = \"0.0.0\", default-features = false }".to_string())
}

pub const WORKSPACE_CARGO: &str = r#"[workspace]
resolver = "3"
members = [
    "crates/app",
    "crates/shared",
    # jerrycan:members:begin (generated — do not edit between markers)
{{members}}    # jerrycan:members:end
]

[workspace.package]
version = "0.1.0"
edition = "2024"

[workspace.dependencies]
{{jerrycan_dep}}
tokio = { version = "1", features = ["macros", "rt-multi-thread", "net", "time", "sync"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
"#;

pub const JERRYCAN_TOML: &str = r#"# jerrycan app configuration (layered: defaults < this file < JERRYCAN_* env)
name = "{{name}}"
"#;

pub const GITIGNORE: &str = "target/\n";

pub const APP_CARGO: &str = r#"[package]
name = "app"
version.workspace = true
edition.workspace = true

[dependencies]
jerrycan.workspace = true
tokio.workspace = true
shared = { path = "../shared" }
# jerrycan:route-deps:begin (generated — do not edit between markers)
{{route_deps}}# jerrycan:route-deps:end
"#;

pub const SHARED_CARGO: &str = r#"[package]
name = "shared"
version.workspace = true
edition.workspace = true

[dependencies]
serde.workspace = true
"#;

pub const SHARED_LIB: &str = r#"//! Cross-module DTOs only — keep deliberately tiny (a jerrycan lint guards growth).
#![forbid(unsafe_code)]
"#;

pub const ROUTE_CARGO: &str = r#"[package]
name = "route-{{name}}"
version.workspace = true
edition.workspace = true

[dependencies]
jerrycan.workspace = true
serde.workspace = true
serde_json.workspace = true
shared = { path = "../../shared" }

[dev-dependencies]
tokio.workspace = true
"#;
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p jerrycan templates` → 3 tests PASS. Full gate green.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan/src/platform/templates.rs
git commit -m "Add embedded scaffolding templates with placeholder renderer"
```

---

### Task 6: Module code generation (`genroute.rs`)

The largest unit: turns a `ModuleDesign` into a route crate (and its fractal subroutes). **File-ownership model — the load-bearing rule of the whole platform:**

| File | Owner | Regeneration |
|---|---|---|
| route `Cargo.toml`, `lib.rs`, `subroutes/**/mod.rs` | **tool** | always overwritten (route tables live here) |
| `handlers.rs`, `model.rs`, `repo.rs`, `deps.rs` | **agent** | created if missing, NEVER overwritten |

Agents register custom module deps/middleware through the agent-owned `deps::configure(Module) -> Module` hook that tool-owned `lib.rs` calls — so regeneration never clobbers agent work and tool files never need hand-edits.

**Handler signature mapping (deterministic, documented here, tested below):**
- path `{x}` → `Path(_x): Path<i64>` (one param max — validated in Task 4; ids are i64 in v0)
- `request_body.entity = E` → `Json(_body): Json<E>`
- module has entities → `_repo: Dep<{E}Repo>` where E = request_body entity, else success entity, else first module entity
- return: `204 → Result<NoContent>` · `201 → Result<Created<E|serde_json::Value>>` · other 2xx → `Result<Json<Vec<E>>>` (list) / `Result<Json<E>>` / `Result<Json<serde_json::Value>>` (no entity)
- stub bodies: `Err(Error::internal("<op> not implemented — replace this stub"))` (compiles for every signature; params underscore-prefixed so generated apps build warning-free)

**Files:**
- Replace stub: `crates/jerrycan/src/platform/genroute.rs`

- [ ] **Step 1: Write the failing tests**

Start `genroute.rs` with:

```rust
//! Per-module code generation: route crates, fractal subroutes, handler stubs.
//! Ownership rule: Cargo.toml/lib.rs/subroutes mod.rs files are TOOL-owned
//! (always rewritten); handlers/model/repo/deps are AGENT-owned (create-once).

use super::design::*;
use super::templates::{jerrycan_dep_spec, render, ROUTE_CARGO};
use std::fs;
use std::path::Path;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::design::tests::MINIMAL;

    fn todos() -> ModuleDesign {
        let d: Design = serde_json::from_str(MINIMAL).unwrap();
        d.modules.into_iter().next().unwrap()
    }

    #[test]
    fn handler_signatures_follow_the_mapping_rules() {
        let m = todos();
        let h = handlers_rs(&m);
        assert!(h.contains("pub(crate) async fn list_todos(_repo: Dep<TodoRepo>) -> Result<Json<Vec<Todo>>>"), "{h}");
        assert!(h.contains("pub(crate) async fn create_todo(_repo: Dep<TodoRepo>, Json(_body): Json<Todo>) -> Result<Created<Todo>>"), "{h}");
        assert!(h.contains("pub(crate) async fn delete_todo(_repo: Dep<TodoRepo>, Path(_id): Path<i64>) -> Result<NoContent>"), "{h}");
        assert!(h.contains("not implemented — replace this stub"));
    }

    #[test]
    fn lib_rs_groups_routes_by_path_and_mounts_subroutes() {
        let m = todos();
        let lib = lib_rs(&m);
        assert!(lib.contains("pub fn module() -> Module"), "{lib}");
        assert!(lib.contains(".route(\"/\", get(handlers::list_todos).post(handlers::create_todo))"), "{lib}");
        assert!(lib.contains(".route(\"/{id}\", delete(handlers::delete_todo))"), "{lib}");
        assert!(lib.contains(".mount(\"/comments\", subroutes::comments::module())"), "{lib}");
        assert!(lib.contains(".provide(repo::TodoRepo::new())"), "{lib}");
        assert!(lib.contains("deps::configure("), "agent hook must wrap the module: {lib}");
        assert!(lib.contains("#![forbid(unsafe_code)]"));
    }

    #[test]
    fn model_and_repo_are_generated_from_entities() {
        let m = todos();
        let model = model_rs(&m).unwrap();
        assert!(model.contains("pub struct Todo"));
        assert!(model.contains("pub title: String"));
        assert!(model.contains("#[serde(default)]\n    pub done: bool"), "{model}");
        let repo = repo_rs(&m).unwrap();
        assert!(repo.contains("pub struct TodoRepo"));
        for method in ["pub fn all(", "pub fn get(", "pub fn insert(", "pub fn remove("] {
            assert!(repo.contains(method), "{repo}");
        }
    }

    #[test]
    fn write_module_respects_the_ownership_rule() {
        let tmp = tempfile::tempdir().unwrap();
        let routes = tmp.path().join("crates/routes");
        let m = todos();

        let created = write_module(&routes, &m).unwrap();
        assert!(created.iter().any(|p| p.ends_with("todos/src/lib.rs")));
        assert!(created.iter().any(|p| p.ends_with("todos/src/subroutes/comments/mod.rs")));

        // Agent edits handlers.rs; tool hand-edits lib.rs (illegally).
        let handlers = routes.join("todos/src/handlers.rs");
        fs::write(&handlers, "// AGENT CODE\n").unwrap();
        let lib = routes.join("todos/src/lib.rs");
        fs::write(&lib, "// hand edit\n").unwrap();

        write_module(&routes, &m).unwrap();
        assert_eq!(fs::read_to_string(&handlers).unwrap(), "// AGENT CODE\n", "agent-owned: preserved");
        assert!(fs::read_to_string(&lib).unwrap().contains("pub fn module()"), "tool-owned: restored");
    }

    #[test]
    fn subroutes_without_entities_have_no_model_or_repo() {
        let m = todos();
        let sub = &m.subroutes[0];
        assert!(model_rs(sub).is_none());
        assert!(repo_rs(sub).is_none());
        let h = handlers_rs(sub);
        assert!(h.contains("pub(crate) async fn list_comments() -> Result<Json<serde_json::Value>>"), "{h}");
    }
}
```

NOTE for the implementer: this test expects `MINIMAL`'s `list_comments` success to be `{ "status": 200 }` (no entity) — mapping → `Json<serde_json::Value>`. That is what Task 3 defined.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p jerrycan genroute` → compile FAILURE.

- [ ] **Step 3: Implement (above the tests)**

```rust
/// snake/ident helpers: crate `route-todo-list` → ident `route_todo_list`.
pub fn crate_ident(module_name: &str) -> String {
    format!("route_{}", module_name.replace('-', "_"))
}

fn endpoint_repo_entity<'a>(m: &'a ModuleDesign, ep: &'a Endpoint) -> Option<&'a str> {
    if m.entities.is_empty() {
        return None;
    }
    ep.request_body
        .as_ref()
        .map(|rb| rb.entity.as_str())
        .or(ep.success.entity.as_deref())
        .or_else(|| m.entities.first().map(|e| e.name.as_str()))
}

fn return_type(ep: &Endpoint) -> String {
    let entity = ep.success.entity.as_deref();
    match (ep.success.status, entity, ep.success.list) {
        (204, _, _) => "Result<NoContent>".to_string(),
        (201, Some(e), _) => format!("Result<Created<{e}>>"),
        (201, None, _) => "Result<Created<serde_json::Value>>".to_string(),
        (_, Some(e), true) => format!("Result<Json<Vec<{e}>>>"),
        (_, Some(e), false) => format!("Result<Json<{e}>>"),
        (_, None, _) => "Result<Json<serde_json::Value>>".to_string(),
    }
}

fn path_param(ep: &Endpoint) -> Option<String> {
    let start = ep.path.find('{')?;
    let end = ep.path[start..].find('}')? + start;
    Some(ep.path[start + 1..end].to_string())
}

fn handler_params(m: &ModuleDesign, ep: &Endpoint) -> String {
    let mut params = Vec::new();
    if let Some(e) = endpoint_repo_entity(m, ep) {
        params.push(format!("_repo: Dep<{e}Repo>"));
    }
    if let Some(p) = path_param(ep) {
        params.push(format!("Path(_{p}): Path<i64>"));
    }
    if let Some(ref rb) = ep.request_body {
        params.push(format!("Json(_body): Json<{}>", rb.entity));
    }
    params.join(", ")
}

pub(crate) fn handlers_rs(m: &ModuleDesign) -> String {
    let mut uses = String::from("use jerrycan::prelude::*;\n");
    let mentions_entities = m
        .endpoints
        .iter()
        .any(|ep| ep.request_body.is_some() || ep.success.entity.is_some());
    if mentions_entities {
        uses.push_str("use super::model::*;\n");
    }
    if !m.entities.is_empty() {
        uses.push_str("use super::repo::*;\n");
    }
    let mut out = format!(
        "//! Handlers for `{}` — thin: extract → call → respond.\n//! Generated stubs return 500 until implemented.\n{uses}\n",
        m.name
    );
    for ep in &m.endpoints {
        out.push_str(&format!(
            "pub(crate) async fn {op}({params}) -> {ret} {{\n    Err(Error::internal(\"{op} not implemented — replace this stub\"))\n}}\n\n",
            op = ep.operation_id,
            params = handler_params(m, ep),
            ret = return_type(ep),
        ));
    }
    out
}

pub(crate) fn model_rs(m: &ModuleDesign) -> Option<String> {
    if m.entities.is_empty() {
        return None;
    }
    let mut out = String::from("//! Entities and DTOs for this module.\nuse serde::{Deserialize, Serialize};\n\n");
    for e in &m.entities {
        out.push_str("#[derive(Debug, Clone, Serialize, Deserialize)]\npub struct ");
        out.push_str(&e.name);
        out.push_str(" {\n");
        for f in &e.fields {
            if !f.required {
                out.push_str("    #[serde(default)]\n");
            }
            out.push_str(&format!("    pub {}: {},\n", f.name, f.field_type.rust_type()));
        }
        out.push_str("}\n\n");
    }
    Some(out)
}

pub(crate) fn repo_rs(m: &ModuleDesign) -> Option<String> {
    if m.entities.is_empty() {
        return None;
    }
    let mut out = String::from(
        "//! In-memory data access (Phase 1; jerrycan-db replaces this in Phase 2).\nuse super::model::*;\nuse std::collections::BTreeMap;\nuse std::sync::Mutex;\nuse std::sync::atomic::{AtomicI64, Ordering};\n\n",
    );
    for e in &m.entities {
        let n = &e.name;
        out.push_str(&format!(
            r#"pub struct {n}Repo {{
    items: Mutex<BTreeMap<i64, {n}>>,
    next_id: AtomicI64,
}}

impl {n}Repo {{
    pub fn new() -> Self {{
        Self {{ items: Mutex::new(BTreeMap::new()), next_id: AtomicI64::new(1) }}
    }}
    pub fn all(&self) -> Vec<{n}> {{
        self.items.lock().unwrap().values().cloned().collect()
    }}
    pub fn get(&self, id: i64) -> Option<{n}> {{
        self.items.lock().unwrap().get(&id).cloned()
    }}
    pub fn insert(&self, item: {n}) -> i64 {{
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        self.items.lock().unwrap().insert(id, item);
        id
    }}
    pub fn remove(&self, id: i64) -> bool {{
        self.items.lock().unwrap().remove(&id).is_some()
    }}
}}

impl Default for {n}Repo {{
    fn default() -> Self {{
        Self::new()
    }}
}}

"#
        ));
    }
    Some(out)
}

pub(crate) fn deps_rs(m: &ModuleDesign) -> String {
    let mut out = String::from(
        "//! Agent-owned: module-scoped dependencies and middleware.\nuse jerrycan::prelude::*;\n\n/// Called by the tool-owned lib.rs — register module deps/middleware here;\n/// regeneration never touches this file.\npub(crate) fn configure(module: Module) -> Module {\n",
    );
    for dep in &m.dependencies {
        out.push_str(&format!("    // declared dependency `{dep}`: define a type and .provide/.provide_dep it here\n"));
    }
    out.push_str("    module\n}\n");
    out
}

/// Route-table lines: endpoints grouped by path (first-seen order), first
/// method via the free fn, the rest chained.
fn route_lines(m: &ModuleDesign, indent: &str) -> String {
    let mut order: Vec<&str> = Vec::new();
    let mut by_path: std::collections::HashMap<&str, Vec<&Endpoint>> = std::collections::HashMap::new();
    for ep in &m.endpoints {
        if !by_path.contains_key(ep.path.as_str()) {
            order.push(&ep.path);
        }
        by_path.entry(&ep.path).or_default().push(ep);
    }
    let mut out = String::new();
    for path in order {
        let eps = &by_path[path];
        let mut chain = format!("{}(handlers::{})", eps[0].method.builder_fn(), eps[0].operation_id);
        for ep in &eps[1..] {
            chain.push_str(&format!(".{}(handlers::{})", ep.method.builder_fn(), ep.operation_id));
        }
        out.push_str(&format!("{indent}.route(\"{path}\", {chain})\n"));
    }
    out
}

fn module_body(m: &ModuleDesign, indent: &str) -> String {
    let mut body = format!("{indent}Module::new(\"{}\")\n", m.name);
    for e in &m.entities {
        body.push_str(&format!("{indent}    .provide(repo::{}Repo::new())\n", e.name));
    }
    body.push_str(&route_lines(m, &format!("{indent}    ")));
    for sub in &m.subroutes {
        body.push_str(&format!(
            "{indent}    .mount(\"{}\", subroutes::{}::module())\n",
            sub.effective_mount(),
            sub.name.replace('-', "_"),
        ));
    }
    body
}

fn mod_decls(m: &ModuleDesign) -> String {
    let mut out = String::from("mod deps;\nmod handlers;\n");
    if !m.entities.is_empty() {
        out.push_str("mod model;\nmod repo;\n");
    }
    if !m.subroutes.is_empty() {
        out.push_str("mod subroutes;\n");
    }
    out
}

pub(crate) fn lib_rs(m: &ModuleDesign) -> String {
    format!(
        "//! Route module `{name}` — TOOL-OWNED, regenerated by `jerrycan generate`.\n//! The sole public item is `module()`; agent code lives in handlers/model/repo/deps.\n#![forbid(unsafe_code)]\n\n{mods}\nuse jerrycan::prelude::*;\n\n/// Build this module's routes, subroutes, and scoped dependencies.\npub fn module() -> Module {{\n    deps::configure(\n{body}    )\n}}\n",
        name = m.name,
        mods = mod_decls(m),
        body = module_body(m, "        "),
    )
}

fn subroute_mod_rs(m: &ModuleDesign) -> String {
    format!(
        "//! Subroute `{name}` — TOOL-OWNED mod.rs; same fractal shape as a module.\n\n{mods}\nuse jerrycan::prelude::*;\n\npub(crate) fn module() -> Module {{\n    deps::configure(\n{body}    )\n}}\n",
        name = m.name,
        mods = mod_decls(m),
        body = module_body(m, "        "),
    )
}

fn write_tool_owned(path: &Path, content: &str, created: &mut Vec<String>, root: &Path) -> Result<(), String> {
    fs::create_dir_all(path.parent().expect("file path has parent")).map_err(|e| e.to_string())?;
    fs::write(path, content).map_err(|e| format!("write {}: {e}", path.display()))?;
    created.push(rel(path, root));
    Ok(())
}

fn write_agent_owned(path: &Path, content: &str, created: &mut Vec<String>, root: &Path) -> Result<(), String> {
    if path.exists() {
        return Ok(()); // never clobber agent work
    }
    write_tool_owned(path, content, created, root)
}

fn rel(path: &Path, root: &Path) -> String {
    path.strip_prefix(root).unwrap_or(path).display().to_string()
}

/// Write (or refresh) one top-level route crate under `routes_dir`
/// (= <app>/crates/routes). Returns paths written, relative to routes_dir's parent's parent.
pub fn write_module(routes_dir: &Path, m: &ModuleDesign) -> Result<Vec<String>, String> {
    let root = routes_dir.ancestors().nth(2).unwrap_or(routes_dir).to_path_buf();
    let crate_dir = routes_dir.join(&m.name);
    let src = crate_dir.join("src");
    let mut created = Vec::new();

    let cargo = render(ROUTE_CARGO, &[("name", &m.name)])?;
    write_tool_owned(&crate_dir.join("Cargo.toml"), &cargo, &mut created, &root)?;
    write_tool_owned(&src.join("lib.rs"), &lib_rs(m), &mut created, &root)?;
    write_unit_files(&src, m, &mut created, &root)?;
    write_subroutes(&src, m, &mut created, &root)?;
    // jerrycan_dep_spec is consumed by the workspace manifest (scaffold), not here;
    // referenced so the module split stays honest:
    let _ = jerrycan_dep_spec;
    Ok(created)
}

/// The agent-owned file set shared by modules and subroutes.
fn write_unit_files(dir: &Path, m: &ModuleDesign, created: &mut Vec<String>, root: &Path) -> Result<(), String> {
    write_agent_owned(&dir.join("handlers.rs"), &handlers_rs(m), created, root)?;
    write_agent_owned(&dir.join("deps.rs"), &deps_rs(m), created, root)?;
    if let Some(model) = model_rs(m) {
        write_agent_owned(&dir.join("model.rs"), &model, created, root)?;
    }
    if let Some(repo) = repo_rs(m) {
        write_agent_owned(&dir.join("repo.rs"), &repo, created, root)?;
    }
    Ok(())
}

fn write_subroutes(src: &Path, m: &ModuleDesign, created: &mut Vec<String>, root: &Path) -> Result<(), String> {
    if m.subroutes.is_empty() {
        return Ok(());
    }
    let sub_root = src.join("subroutes");
    let mut decls = String::from("//! TOOL-OWNED: subroute declarations.\n");
    for sub in &m.subroutes {
        decls.push_str(&format!("pub(crate) mod {};\n", sub.name.replace('-', "_")));
    }
    write_tool_owned(&sub_root.join("mod.rs"), &decls, created, root)?;
    for sub in &m.subroutes {
        let dir = sub_root.join(sub.name.replace('-', "_"));
        write_tool_owned(&dir.join("mod.rs"), &subroute_mod_rs(sub), created, root)?;
        write_unit_files(&dir, sub, created, root)?;
        write_subroutes(&dir, sub, created, root)?; // arbitrary depth
    }
    Ok(())
}

/// `jerrycan generate dep <name> --module <m>`: record in design + remind in deps.rs.
pub fn add_dependency(design: &mut Design, module_path: &str, dep: &str) -> Result<(), String> {
    let m = module_by_path_mut(design, module_path)
        .ok_or_else(|| format!("module `{module_path}` not found in design.json"))?;
    if !m.dependencies.iter().any(|d| d == dep) {
        m.dependencies.push(dep.to_string());
    }
    Ok(())
}

pub fn module_by_path<'a>(design: &'a Design, path: &str) -> Option<&'a ModuleDesign> {
    let mut parts = path.split('/');
    let mut cur = design.modules.iter().find(|m| m.name == parts.next()?)?;
    for part in parts {
        cur = cur.subroutes.iter().find(|s| s.name == part)?;
    }
    Some(cur)
}

pub fn module_by_path_mut<'a>(design: &'a mut Design, path: &str) -> Option<&'a mut ModuleDesign> {
    let mut parts = path.split('/');
    let first = parts.next()?;
    let mut cur = design.modules.iter_mut().find(|m| m.name == first)?;
    for part in parts {
        cur = cur.subroutes.iter_mut().find(|s| s.name == part)?;
    }
    Some(cur)
}
```

(If clippy objects to the `let _ = jerrycan_dep_spec;` reference, delete that line AND the `jerrycan_dep_spec` import — it belongs to scaffold.rs in Task 7; the import was speculative. Record whichever you did.)

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p jerrycan genroute` → 5 tests PASS. Full gate green.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan/src/platform/genroute.rs
git commit -m "Add route-module code generation with agent/tool file ownership"
```

---
### Task 7: Scaffold + the mounting regenerator (`scaffold.rs`, `mounting.rs`)

**Files:**
- Replace stubs: `crates/jerrycan/src/platform/scaffold.rs`, `crates/jerrycan/src/platform/mounting.rs`
- Create: `crates/jerrycan/tests/generation.rs`

- [ ] **Step 1: Write the failing integration tests**

Create `crates/jerrycan/tests/generation.rs`:

```rust
//! Scaffold + mounting determinism. Everything here is fast (no cargo builds).

use jerrycan::platform::design::Design;
use jerrycan::platform::{mounting, scaffold};
use std::fs;

const DESIGN: &str = include_str!("../../../conformance/designs/todo-api.design.json");

fn design() -> Design {
    serde_json::from_str(DESIGN).unwrap()
}

#[test]
fn scaffold_creates_the_fractal_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("todo-api");
    let created = scaffold::scaffold(&root, &design()).unwrap();

    for expected in [
        "Cargo.toml",
        "jerrycan.toml",
        "design.json",
        ".gitignore",
        "crates/app/Cargo.toml",
        "crates/app/src/main.rs",
        "crates/shared/Cargo.toml",
        "crates/shared/src/lib.rs",
        "crates/routes/todos/Cargo.toml",
        "crates/routes/todos/src/lib.rs",
        "crates/routes/todos/src/handlers.rs",
        "crates/routes/todos/src/model.rs",
        "crates/routes/todos/src/repo.rs",
        "crates/routes/todos/src/deps.rs",
        "crates/routes/todos/src/subroutes/comments/mod.rs",
        "crates/routes/users/src/lib.rs",
    ] {
        assert!(root.join(expected).exists(), "missing {expected}; created={created:?}");
    }
}

#[test]
fn mounting_is_sorted_and_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("todo-api");
    scaffold::scaffold(&root, &design()).unwrap();

    let main1 = fs::read_to_string(root.join("crates/app/src/main.rs")).unwrap();
    assert!(main1.contains("GENERATED by jerrycan"));
    let todos_pos = main1.find(".mount(\"/todos\", route_todos::module())").unwrap();
    let users_pos = main1.find(".mount(\"/users\", route_users::module())").unwrap();
    assert!(todos_pos < users_pos, "mounts must be sorted by module name");

    let ws1 = fs::read_to_string(root.join("Cargo.toml")).unwrap();
    assert!(ws1.contains("\"crates/routes/todos\","));

    // Regenerating changes nothing — byte-identical (determinism contract).
    mounting::regenerate(&root, &design()).unwrap();
    let main2 = fs::read_to_string(root.join("crates/app/src/main.rs")).unwrap();
    let ws2 = fs::read_to_string(root.join("Cargo.toml")).unwrap();
    assert_eq!(main1, main2);
    assert_eq!(ws1, ws2);
}

#[test]
fn scaffold_refuses_a_nonempty_target() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("busy");
    fs::create_dir_all(root.join("stuff")).unwrap();
    let err = scaffold::scaffold(&root, &design()).unwrap_err();
    assert!(err.contains("not empty"), "{err}");
}

#[test]
fn expected_main_matches_what_regenerate_writes() {
    // JL0003 (generated-drift lint) depends on this equivalence.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("todo-api");
    scaffold::scaffold(&root, &design()).unwrap();
    let on_disk = fs::read_to_string(root.join("crates/app/src/main.rs")).unwrap();
    assert_eq!(on_disk, mounting::expected_main(&design()));
}
```

Create `conformance/designs/todo-api.design.json` — the golden multi-module design used by every heavy test from here on:

```json
{
  "name": "todo-api",
  "contract_version": 0,
  "description": "Conformance golden app: multi-module in-memory CRUD",
  "auth": { "model": "none" },
  "modules": [
    {
      "name": "todos",
      "entities": [
        { "name": "Todo", "fields": [
          { "name": "title", "type": "string" },
          { "name": "done", "type": "boolean", "required": false }
        ]}
      ],
      "endpoints": [
        { "operation_id": "list_todos", "method": "GET", "path": "/",
          "success": { "status": 200, "entity": "Todo", "list": true } },
        { "operation_id": "create_todo", "method": "POST", "path": "/",
          "request_body": { "entity": "Todo" },
          "success": { "status": 201, "entity": "Todo" } },
        { "operation_id": "show_todo", "method": "GET", "path": "/{id}",
          "success": { "status": 200, "entity": "Todo" },
          "errors": [{ "status": 404, "code": "JC0404", "when": "unknown id" }] },
        { "operation_id": "delete_todo", "method": "DELETE", "path": "/{id}",
          "success": { "status": 204 },
          "errors": [{ "status": 404, "code": "JC0404", "when": "unknown id" }] }
      ],
      "subroutes": [
        {
          "name": "comments",
          "entities": [
            { "name": "Comment", "fields": [{ "name": "text", "type": "string" }] }
          ],
          "endpoints": [
            { "operation_id": "list_comments", "method": "GET", "path": "/",
              "success": { "status": 200, "entity": "Comment", "list": true } },
            { "operation_id": "create_comment", "method": "POST", "path": "/",
              "request_body": { "entity": "Comment" },
              "success": { "status": 201, "entity": "Comment" } }
          ]
        }
      ]
    },
    {
      "name": "users",
      "entities": [
        { "name": "User", "fields": [{ "name": "username", "type": "string" }] }
      ],
      "endpoints": [
        { "operation_id": "list_users", "method": "GET", "path": "/",
          "success": { "status": 200, "entity": "User", "list": true } },
        { "operation_id": "create_user", "method": "POST", "path": "/",
          "request_body": { "entity": "User" },
          "success": { "status": 201, "entity": "User" } }
      ]
    }
  ]
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p jerrycan --test generation` → compile FAILURE (`scaffold`, `mounting` empty stubs).

- [ ] **Step 3: Implement `mounting.rs`**

```rust
//! The deterministic mounting regenerator: app/src/main.rs (whole file),
//! workspace members, app route-deps. Sorted, idempotent, byte-stable —
//! JL0003 compares against exactly this output.

use super::design::Design;
use super::genroute::crate_ident;
use std::fs;
use std::path::Path;

/// The complete, tool-owned app/src/main.rs for this design.
pub fn expected_main(design: &Design) -> String {
    let mut modules: Vec<_> = design.modules.iter().collect();
    modules.sort_by(|a, b| a.name.cmp(&b.name));

    let mut body = String::new();
    for dep in &design.dependencies {
        body.push_str(&format!(
            "        // app dependency `{dep}`: provide here once its extension lands (Phase 2+)\n"
        ));
    }
    for m in &modules {
        body.push_str(&format!(
            "        .mount(\"{}\", {}::module())\n",
            m.effective_mount(),
            crate_ident(&m.name)
        ));
    }

    format!(
        "//! GENERATED by jerrycan — do not hand-edit; `jerrycan generate` rewrites this file.\nuse jerrycan::prelude::*;\n\n#[jerrycan::main]\nasync fn main() -> Result<()> {{\n    App::new()\n{body}        .serve()\n        .await\n}}\n"
    )
}

/// Replace the lines between marker lines (markers stay). Fails loud if markers vanished.
fn splice(content: &str, begin: &str, end: &str, replacement: &str) -> Result<String, String> {
    let b = content
        .find(begin)
        .ok_or_else(|| format!("marker `{begin}` missing — file was hand-edited; restore it or re-scaffold"))?;
    let line_end = content[b..].find('\n').map(|i| b + i + 1).unwrap_or(content.len());
    let e = content
        .find(end)
        .ok_or_else(|| format!("marker `{end}` missing — file was hand-edited; restore it or re-scaffold"))?;
    if e < line_end {
        return Err(format!("marker `{end}` precedes `{begin}`"));
    }
    let e_line_start = content[..e].rfind('\n').map(|i| i + 1).unwrap_or(0);
    Ok(format!("{}{}{}", &content[..line_end], replacement, &content[e_line_start..]))
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

    // 2. workspace members.
    let ws_path = app_root.join("Cargo.toml");
    let ws = fs::read_to_string(&ws_path).map_err(|e| format!("read {}: {e}", ws_path.display()))?;
    let members: String = modules
        .iter()
        .map(|m| format!("    \"crates/routes/{}\",\n", m.name))
        .collect();
    let ws2 = splice(&ws, "# jerrycan:members:begin", "# jerrycan:members:end", &members)?;
    if ws2 != ws {
        fs::write(&ws_path, &ws2).map_err(|e| e.to_string())?;
    }
    modified.push("Cargo.toml".to_string());

    // 3. app route-deps.
    let app_cargo_path = app_root.join("crates/app/Cargo.toml");
    let ac = fs::read_to_string(&app_cargo_path).map_err(|e| format!("read {}: {e}", app_cargo_path.display()))?;
    let deps: String = modules
        .iter()
        .map(|m| format!("route-{} = {{ path = \"../routes/{}\" }}\n", m.name, m.name))
        .collect();
    let ac2 = splice(&ac, "# jerrycan:route-deps:begin", "# jerrycan:route-deps:end", &deps)?;
    if ac2 != ac {
        fs::write(&app_cargo_path, &ac2).map_err(|e| e.to_string())?;
    }
    modified.push("crates/app/Cargo.toml".to_string());

    Ok(modified)
}
```

- [ ] **Step 4: Implement `scaffold.rs`**

```rust
//! `jerrycan new`: design → complete crate-per-module workspace on disk.

use super::design::Design;
use super::genroute;
use super::mounting;
use super::templates::*;
use std::fs;
use std::path::Path;

/// Canonical on-disk form of design.json (pretty, trailing newline) — both
/// scaffold and the MCP design tool write exactly this, so diffs stay clean.
pub fn canonical_design_json(design: &Design) -> String {
    let mut s = serde_json::to_string_pretty(design).expect("design serializes");
    s.push('\n');
    s
}

pub fn scaffold(target: &Path, design: &Design) -> Result<Vec<String>, String> {
    if target.exists() && target.read_dir().map_err(|e| e.to_string())?.next().is_some() {
        return Err(format!("target directory {} is not empty", target.display()));
    }
    let mut created = Vec::new();
    let mut write = |rel: &str, content: &str| -> Result<(), String> {
        let path = target.join(rel);
        fs::create_dir_all(path.parent().expect("parent")).map_err(|e| e.to_string())?;
        fs::write(&path, content).map_err(|e| format!("write {rel}: {e}"))?;
        created.push(rel.to_string());
        Ok(())
    };

    let dep_line = jerrycan_dep_spec();
    write("Cargo.toml", &render(WORKSPACE_CARGO, &[("members", ""), ("jerrycan_dep", &dep_line)])?)?;
    write("jerrycan.toml", &render(JERRYCAN_TOML, &[("name", &design.name)])?)?;
    write(".gitignore", GITIGNORE)?;
    write("design.json", &canonical_design_json(design))?;
    write("crates/app/Cargo.toml", &render(APP_CARGO, &[("route_deps", "")])?)?;
    write("crates/shared/Cargo.toml", SHARED_CARGO)?;
    write("crates/shared/src/lib.rs", SHARED_LIB)?;

    let routes_dir = target.join("crates/routes");
    for m in &design.modules {
        created.extend(genroute::write_module(&routes_dir, m)?);
    }
    created.extend(mounting::regenerate(target, design)?);
    created.sort();
    created.dedup();
    Ok(created)
}
```

- [ ] **Step 5: Run to verify pass**

Run: `cargo test -p jerrycan --test generation` → 4 tests PASS. Also `cargo test -p jerrycan` (unit tests still green). Full gate green.

- [ ] **Step 6: Commit**

```bash
git add crates/jerrycan/src/platform/scaffold.rs crates/jerrycan/src/platform/mounting.rs crates/jerrycan/tests/generation.rs conformance/designs/todo-api.design.json
git commit -m "Add scaffold and deterministic mounting regenerator with golden design"
```

---

### Task 8: CLI wiring — `new`, `generate route|dep`, `list routes`

**Files:**
- Modify: `crates/jerrycan/src/main.rs` (the `run` fn)
- Modify: `crates/jerrycan/tests/cli.rs`

- [ ] **Step 1: Write the failing tests (append to tests/cli.rs)**

```rust
const GOLDEN: &str = include_str!("../../../conformance/designs/todo-api.design.json");

#[test]
fn new_scaffolds_and_emits_json_output() {
    let tmp = tempfile::tempdir().unwrap();
    let design_path = tmp.path().join("design.json");
    std::fs::write(&design_path, GOLDEN).unwrap();
    let app_dir = tmp.path().join("todo-api");

    let out = jerrycan()
        .args(["--json", "new"])
        .arg(&app_dir)
        .arg("--design")
        .arg(&design_path)
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));

    // --json: stdout is exactly one JSON document matching the MCP outputSchema.
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is JSON");
    assert!(payload["created"].as_array().unwrap().len() > 10);
    assert!(payload["next_step"].as_str().unwrap().contains("check"));
    assert!(app_dir.join("crates/routes/todos/src/lib.rs").exists());
}

#[test]
fn new_with_invalid_design_returns_questions_and_exit_1() {
    let tmp = tempfile::tempdir().unwrap();
    let design_path = tmp.path().join("design.json");
    std::fs::write(&design_path, GOLDEN.replace("\"todo-api\"", "\"Todo API\"")).unwrap();

    let out = jerrycan()
        .args(["--json", "new"])
        .arg(tmp.path().join("x"))
        .arg("--design")
        .arg(&design_path)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "incomplete design = gate failure");
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(payload["status"], "questions");
    assert!(payload["questions"][0]["id"].as_str().unwrap().starts_with("/name"));
}

#[test]
fn generate_route_adds_a_module_and_rewires_mounting() {
    let tmp = tempfile::tempdir().unwrap();
    let design_path = tmp.path().join("design.json");
    std::fs::write(&design_path, GOLDEN).unwrap();
    let app_dir = tmp.path().join("todo-api");
    assert!(jerrycan().args(["new"]).arg(&app_dir).arg("--design").arg(&design_path).status().unwrap().success());

    // Add a module to the app's design.json (the agent's edit), then generate.
    let dj = app_dir.join("design.json");
    let mut design: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&dj).unwrap()).unwrap();
    design["modules"].as_array_mut().unwrap().push(serde_json::json!({
        "name": "tags",
        "endpoints": [{ "operation_id": "list_tags", "method": "GET", "path": "/",
                        "success": { "status": 200 } }]
    }));
    std::fs::write(&dj, serde_json::to_string_pretty(&design).unwrap()).unwrap();

    let out = jerrycan()
        .current_dir(&app_dir)
        .args(["--json", "generate", "route", "tags"])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(payload["created"].as_array().unwrap().iter().any(|p| p.as_str().unwrap().contains("routes/tags")));
    assert!(payload["modified"].as_array().unwrap().iter().any(|p| p == "crates/app/src/main.rs"));
    let main_rs = std::fs::read_to_string(app_dir.join("crates/app/src/main.rs")).unwrap();
    assert!(main_rs.contains(".mount(\"/tags\", route_tags::module())"));
}

#[test]
fn generate_route_for_unknown_module_is_usage_error() {
    let tmp = tempfile::tempdir().unwrap();
    let design_path = tmp.path().join("design.json");
    std::fs::write(&design_path, GOLDEN).unwrap();
    let app_dir = tmp.path().join("todo-api");
    assert!(jerrycan().args(["new"]).arg(&app_dir).arg("--design").arg(&design_path).status().unwrap().success());

    let out = jerrycan().current_dir(&app_dir).args(["generate", "route", "ghosts"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("design.json"));
}

#[test]
fn list_routes_walks_the_module_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let design_path = tmp.path().join("design.json");
    std::fs::write(&design_path, GOLDEN).unwrap();
    let app_dir = tmp.path().join("todo-api");
    assert!(jerrycan().args(["new"]).arg(&app_dir).arg("--design").arg(&design_path).status().unwrap().success());

    let out = jerrycan().current_dir(&app_dir).args(["--json", "list", "routes"]).output().unwrap();
    assert!(out.status.success());
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let routes = payload["routes"].as_array().unwrap();
    let find = |method: &str, path: &str| {
        routes.iter().any(|r| r["method"] == method && r["path"] == path)
    };
    assert!(find("GET", "/todos/"), "{routes:?}");
    assert!(find("DELETE", "/todos/{id}"));
    assert!(find("GET", "/todos/comments/"), "subroute paths compose: {routes:?}");
    assert!(find("POST", "/users/"));
    let todo = routes.iter().find(|r| r["path"] == "/todos/").unwrap();
    assert_eq!(todo["module"], "todos");
    assert_eq!(todo["handler"], "list_todos");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p jerrycan --test cli` → new tests FAIL (commands return the Task 2 stub error).

- [ ] **Step 3: Implement the command arms in `main.rs`**

Replace the `run` fn and add helpers:

```rust
use jerrycan::platform::design::Design;
use jerrycan::platform::{genroute, mounting, questions, scaffold};
use std::path::{Path, PathBuf};

fn run(cli: Cli) -> Result<(), Failure> {
    match cli.command {
        Cmd::New { name, design } => cmd_new(&name, &design, cli.json),
        Cmd::Generate { what } => match what {
            GenerateCmd::Route { path } => cmd_generate_route(&path, cli.json),
            GenerateCmd::Dep { name, module } => cmd_generate_dep(&name, &module, cli.json),
        },
        Cmd::List { what: ListCmd::Routes } => cmd_list_routes(cli.json),
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
    emit(json_mode, &payload, &format!("scaffolded {} files into {target}", payload["created"].as_array().map(Vec::len).unwrap_or(0)));
    Ok(())
}

/// The app root = cwd for post-scaffold commands (the MCP twin takes `directory`).
fn app_root() -> Result<PathBuf, Failure> {
    let cwd = std::env::current_dir().map_err(|e| Failure::environment(e.to_string()))?;
    if cwd.join("design.json").exists() {
        Ok(cwd)
    } else {
        Err(Failure::usage("no design.json here — run inside a jerrycan app (or scaffold one with `jerrycan new`)"))
    }
}

fn cmd_generate_route(module_path: &str, json_mode: bool) -> Result<(), Failure> {
    let root = app_root()?;
    let design = load_design(&root.join("design.json"))?;
    require_complete(&design, json_mode)?;
    let top = module_path.split('/').next().expect("split yields at least one");
    if genroute::module_by_path(&design, module_path).is_none() {
        return Err(Failure::usage(format!(
            "module `{module_path}` is not in design.json — add it there first (the design is the source of truth)"
        )));
    }
    let top_module = design.modules.iter().find(|m| m.name == top).expect("checked above");
    let created = genroute::write_module(&root.join("crates/routes"), top_module).map_err(Failure::gate)?;
    let modified = mounting::regenerate(&root, &design).map_err(Failure::gate)?;
    let payload = serde_json::json!({
        "created": created,
        "modified": modified,
        "next_step": format!("implement crates/routes/{top}/src/handlers.rs, then jerrycan check --module {top}"),
    });
    emit(json_mode, &payload, &format!("generated `{module_path}` and rewired mounting"));
    Ok(())
}

fn cmd_generate_dep(name: &str, module: &str, json_mode: bool) -> Result<(), Failure> {
    let root = app_root()?;
    let mut design = load_design(&root.join("design.json"))?;
    genroute::add_dependency(&mut design, module, name).map_err(Failure::usage)?;
    std::fs::write(root.join("design.json"), scaffold::canonical_design_json(&design))
        .map_err(|e| Failure::gate(e.to_string()))?;
    let payload = serde_json::json!({
        "created": [],
        "modified": ["design.json"],
        "next_step": format!("define `{name}` in crates/routes/{}/src/deps.rs (configure hook)", module.split('/').next().unwrap_or(module)),
    });
    emit(json_mode, &payload, &format!("recorded dependency `{name}` on module `{module}`"));
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
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p jerrycan --test cli` → 8 tests PASS (3 old + 5 new). Full gate green.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan/src/main.rs crates/jerrycan/tests/cli.rs
git commit -m "Wire new, generate, and list routes CLI commands"
```

---

### Task 9: Heavy proof — the scaffolded app COMPILES and SERVES

**Files:**
- Create: `crates/jerrycan/tests/conformance.rs` (first heavy test)

- [ ] **Step 1: Write the heavy test (`#[ignore]` — run explicitly; CI runs it always)**

```rust
//! Heavy conformance tests (#[ignore]): real cargo builds of generated apps.
//! Run with: cargo test -p jerrycan --test conformance -- --include-ignored

use std::path::{Path, PathBuf};
use std::process::Command;

const GOLDEN: &str = include_str!("../../../conformance/designs/todo-api.design.json");

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(2).unwrap().to_path_buf()
}

/// Scaffold the golden app wired to the LOCAL framework (path dep).
fn scaffold_golden(tmp: &Path) -> PathBuf {
    let design = tmp.join("design.json");
    std::fs::write(&design, GOLDEN).unwrap();
    let app = tmp.join("todo-api");
    let dep = format!(
        "jerrycan = {{ path = \"{}\", default-features = false }}",
        repo_root().join("crates/jerrycan").display()
    );
    let st = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .env("JERRYCAN_FRAMEWORK_DEP", &dep)
        .arg("new").arg(&app).arg("--design").arg(&design)
        .status()
        .unwrap();
    assert!(st.success());
    app
}

#[test]
#[ignore = "heavy: full cargo build of a generated workspace"]
fn scaffolded_app_builds_with_zero_warnings() {
    let tmp = tempfile::tempdir().unwrap();
    let app = scaffold_golden(tmp.path());
    let out = Command::new("cargo")
        .current_dir(&app)
        .env("RUSTFLAGS", "-D warnings")
        .args(["build", "--workspace"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "generated app must build warning-free:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
```

- [ ] **Step 2: Run it**

Run: `cargo test -p jerrycan --test conformance -- --include-ignored`
Expected: PASS (several minutes the first time). **If it fails, the generator templates are wrong — fix genroute/templates, not the test.** Common first-run issues and their owners: unused imports in generated handlers (fix the conditional-imports logic in `handlers_rs`), missing serde features (fix WORKSPACE_CARGO), route-crate name/ident mismatches (fix `crate_ident`).

- [ ] **Step 3: Commit**

```bash
git add crates/jerrycan/tests/conformance.rs
git commit -m "Prove scaffolded golden app builds warning-free"
```

---
### Task 10: Check pipeline — cargo JSON diagnostics (`checkpipe.rs`)

**Files:**
- Replace stub: `crates/jerrycan/src/platform/checkpipe.rs`
- Create: `crates/jerrycan/tests/checkpipe.rs`

- [ ] **Step 1: Write the failing tests**

Create `crates/jerrycan/tests/checkpipe.rs`:

```rust
//! Diagnostic parsing on canned fixtures — no real cargo invocations here.

use jerrycan::platform::checkpipe::*;

// One real `cargo build --message-format=json` error line (trimmed to relevant keys).
const RUSTC_ERR: &str = r##"{"reason":"compiler-message","message":{"code":{"code":"E0308"},"level":"error","message":"mismatched types","spans":[{"file_name":"crates/routes/todos/src/handlers.rs","line_start":12,"is_primary":true}],"children":[{"level":"help","message":"try wrapping in Json(...)","spans":[]}]}}
{"reason":"build-finished","success":false}"##;

#[test]
fn cargo_json_errors_become_diagnostics() {
    let ds = parse_cargo_json(RUSTC_ERR, "build");
    assert_eq!(ds.len(), 1);
    let d = &ds[0];
    assert_eq!(d.code, "E0308");
    assert_eq!(d.file.as_deref(), Some("crates/routes/todos/src/handlers.rs"));
    assert_eq!(d.line, Some(12));
    assert_eq!(d.message, "mismatched types");
    assert_eq!(d.suggestion.as_deref(), Some("try wrapping in Json(...)"));
    assert!(d.doc_url.as_deref().unwrap().contains("E0308"));
}

#[test]
fn warnings_are_ignored_but_errors_without_code_still_surface() {
    let mixed = r##"{"reason":"compiler-message","message":{"code":null,"level":"warning","message":"unused","spans":[]}}
{"reason":"compiler-message","message":{"code":null,"level":"error","message":"aborting due to previous error","spans":[]}}"##;
    let ds = parse_cargo_json(mixed, "clippy");
    assert_eq!(ds.len(), 1);
    assert_eq!(ds[0].code, "CLIPPY");
}

#[test]
fn libtest_failures_become_diagnostics() {
    let out = "running 3 tests\ntest todos::lists ... ok\ntest todos::creates ... FAILED\ntest users::lists ... ok\n\nfailures:\n    todos::creates\n";
    let ds = parse_test_output(out);
    assert_eq!(ds.len(), 1);
    assert_eq!(ds[0].code, "TEST0001");
    assert!(ds[0].message.contains("todos::creates"));
}

#[test]
fn report_serializes_to_the_mcp_check_shape() {
    let report = CheckReport {
        ok: false,
        diagnostics: vec![Diagnostic {
            code: "E0308".into(),
            file: Some("x.rs".into()),
            line: Some(1),
            message: "m".into(),
            suggestion: None,
            doc_url: None,
        }],
        next_step: "fix the build diagnostics".into(),
    };
    let v = serde_json::to_value(&report).unwrap();
    assert_eq!(v["ok"], false);
    assert_eq!(v["diagnostics"][0]["code"], "E0308");
    assert!(v["next_step"].is_string());
    // Optional fields are OMITTED when None (matches outputSchema: only code+message required).
    assert!(v["diagnostics"][0].get("suggestion").is_none());
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p jerrycan --test checkpipe` → compile FAILURE.

- [ ] **Step 3: Implement**

Replace `crates/jerrycan/src/platform/checkpipe.rs`:

```rust
//! The verification gate: build → clippy → audit → deny → tests → jerrycan lints.
//! First failing CLASS stops the pipeline; within a class, ALL diagnostics are
//! collected (cli-ux.md). One diagnostics shape, rendered by CLI and MCP alike.

use serde::Serialize;
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, Serialize)]
pub struct Diagnostic {
    pub code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc_url: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CheckReport {
    pub ok: bool,
    pub diagnostics: Vec<Diagnostic>,
    pub next_step: String,
}

/// Parse `--message-format=json` output (one JSON object per line).
pub fn parse_cargo_json(stdout: &str, source: &str) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for line in stdout.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if v["reason"] != "compiler-message" {
            continue;
        }
        let msg = &v["message"];
        if msg["level"] != "error" {
            continue;
        }
        let code = msg["code"]["code"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| source.to_uppercase());
        let primary = msg["spans"]
            .as_array()
            .and_then(|s| s.iter().find(|sp| sp["is_primary"] == true));
        let suggestion = msg["children"]
            .as_array()
            .and_then(|cs| cs.iter().find(|c| c["level"] == "help"))
            .and_then(|c| c["message"].as_str())
            .map(str::to_string);
        let doc_url = code
            .starts_with('E')
            .then(|| format!("https://doc.rust-lang.org/error_codes/{code}.html"));
        out.push(Diagnostic {
            code,
            file: primary.and_then(|p| p["file_name"].as_str()).map(str::to_string),
            line: primary.and_then(|p| p["line_start"].as_u64()),
            message: msg["message"].as_str().unwrap_or("").to_string(),
            suggestion,
            doc_url,
        });
    }
    out
}

/// Parse human libtest output for failures ("test path::name ... FAILED").
pub fn parse_test_output(stdout: &str) -> Vec<Diagnostic> {
    stdout
        .lines()
        .filter_map(|l| {
            let l = l.strip_prefix("test ")?;
            let name = l.strip_suffix(" ... FAILED")?;
            Some(Diagnostic {
                code: "TEST0001".into(),
                file: None,
                line: None,
                message: format!("test {name} failed"),
                suggestion: Some("run `jerrycan test` and read the failure output".into()),
                doc_url: None,
            })
        })
        .collect()
}

fn cargo_in(root: &Path) -> Command {
    let mut c = Command::new("cargo");
    c.current_dir(root);
    c
}

fn package_args(module: Option<&str>) -> Vec<String> {
    match module {
        Some(m) => vec!["-p".into(), format!("route-{m}")],
        None => vec!["--workspace".into()],
    }
}

pub fn run_build(root: &Path, module: Option<&str>) -> Result<Vec<Diagnostic>, String> {
    let out = cargo_in(root)
        .arg("build")
        .args(package_args(module))
        .arg("--message-format=json")
        .output()
        .map_err(|e| format!("cargo not runnable: {e}"))?;
    Ok(parse_cargo_json(&String::from_utf8_lossy(&out.stdout), "build"))
}

pub fn run_clippy(root: &Path, module: Option<&str>) -> Result<Vec<Diagnostic>, String> {
    let out = cargo_in(root)
        .arg("clippy")
        .args(package_args(module))
        .args(["--all-targets", "--message-format=json", "--", "-D", "warnings"])
        .output()
        .map_err(|e| format!("cargo clippy not runnable: {e}"))?;
    Ok(parse_cargo_json(&String::from_utf8_lossy(&out.stdout), "clippy"))
}

pub fn run_tests(root: &Path, module: Option<&str>) -> Result<Vec<Diagnostic>, String> {
    let out = cargo_in(root)
        .arg("test")
        .args(package_args(module))
        .output()
        .map_err(|e| format!("cargo test not runnable: {e}"))?;
    let mut ds = parse_test_output(&String::from_utf8_lossy(&out.stdout));
    if !out.status.success() && ds.is_empty() {
        // Compile error inside tests, or harness failure — surface stderr tail.
        let err = String::from_utf8_lossy(&out.stderr);
        ds.push(Diagnostic {
            code: "TEST0002".into(),
            file: None,
            line: None,
            message: format!("test run failed: {}", err.chars().rev().take(400).collect::<String>().chars().rev().collect::<String>()),
            suggestion: None,
            doc_url: None,
        });
    }
    Ok(ds)
}

/// External tool steps. A missing tool is an ENVIRONMENT failure (exit 3), not a gate failure.
pub enum ToolStep {
    Missing(String),
    Ran(Vec<Diagnostic>),
}

fn external_tool(root: &Path, tool: &str, args: &[&str], code: &str, install: &str) -> Result<ToolStep, String> {
    let probe = Command::new("cargo").args([tool, "--version"]).output();
    if !probe.map(|o| o.status.success()).unwrap_or(false) {
        return Ok(ToolStep::Missing(format!(
            "cargo-{tool} is not installed — install with `cargo install {install}`"
        )));
    }
    let out = cargo_in(root).arg(tool).args(args).output().map_err(|e| e.to_string())?;
    if out.status.success() {
        return Ok(ToolStep::Ran(Vec::new()));
    }
    let tail: String = String::from_utf8_lossy(&out.stderr)
        .lines()
        .chain(String::from_utf8_lossy(&out.stdout).lines())
        .filter(|l| !l.trim().is_empty())
        .take(12)
        .collect::<Vec<_>>()
        .join("\n");
    Ok(ToolStep::Ran(vec![Diagnostic {
        code: code.into(),
        file: None,
        line: None,
        message: format!("cargo {tool} failed:\n{tail}"),
        suggestion: None,
        doc_url: None,
    }]))
}

pub fn run_audit(root: &Path) -> Result<ToolStep, String> {
    external_tool(root, "audit", &[], "AUDIT0001", "cargo-audit")
}

pub fn run_deny(root: &Path) -> Result<ToolStep, String> {
    external_tool(root, "deny", &["check"], "DENY0001", "cargo-deny")
}
```

- [ ] **Step 4: Run to verify pass** — `cargo test -p jerrycan --test checkpipe` → 4 PASS. Full gate green.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan/src/platform/checkpipe.rs crates/jerrycan/tests/checkpipe.rs
git commit -m "Add check pipeline with machine-readable cargo diagnostics"
```

---

### Task 11: jerrycan lints — JL0001/JL0002/JL0003 (`lints.rs`)

**Files:**
- Replace stub: `crates/jerrycan/src/platform/lints.rs`
- Modify: `crates/jerrycan/tests/checkpipe.rs` (append lint tests)

- [ ] **Step 1: Write the failing tests (append to tests/checkpipe.rs)**

```rust
use jerrycan::platform::design::Design;
use jerrycan::platform::{lints, scaffold};

const GOLDEN: &str = include_str!("../../../conformance/designs/todo-api.design.json");

fn scaffolded() -> (tempfile::TempDir, std::path::PathBuf, Design) {
    let tmp = tempfile::tempdir().unwrap();
    let design: Design = serde_json::from_str(GOLDEN).unwrap();
    let root = tmp.path().join("app");
    scaffold::scaffold(&root, &design).unwrap();
    (tmp, root, design)
}

#[test]
fn fresh_scaffold_is_lint_clean() {
    let (_tmp, root, design) = scaffolded();
    assert!(lints::run(&root, &design).is_empty());
}

#[test]
fn jl0001_flags_extra_public_surface() {
    let (_tmp, root, design) = scaffolded();
    let lib = root.join("crates/routes/todos/src/lib.rs");
    let mut content = std::fs::read_to_string(&lib).unwrap();
    content.push_str("\npub fn leak() {}\n");
    std::fs::write(&lib, content).unwrap();
    let ds = lints::run(&root, &design);
    assert!(ds.iter().any(|d| d.code == "JL0001" && d.file.as_deref().unwrap().contains("todos/src/lib.rs")), "{ds:?}");
}

#[test]
fn jl0002_flags_missing_handlers() {
    let (_tmp, root, design) = scaffolded();
    let handlers = root.join("crates/routes/users/src/handlers.rs");
    let content = std::fs::read_to_string(&handlers).unwrap().replace("list_users", "list_everyone");
    std::fs::write(&handlers, content).unwrap();
    let ds = lints::run(&root, &design);
    assert!(ds.iter().any(|d| d.code == "JL0002" && d.message.contains("list_users")), "{ds:?}");
}

#[test]
fn jl0003_flags_hand_edited_generated_files() {
    let (_tmp, root, design) = scaffolded();
    let main_rs = root.join("crates/app/src/main.rs");
    let content = std::fs::read_to_string(&main_rs).unwrap().replace("App::new()", "App::new() // tweaked");
    std::fs::write(&main_rs, content).unwrap();
    let ds = lints::run(&root, &design);
    assert!(ds.iter().any(|d| d.code == "JL0003"), "{ds:?}");
}
```

- [ ] **Step 2: Run to verify failure** — compile FAILURE (`lints::run` missing).

- [ ] **Step 3: Implement `lints.rs`**

```rust
//! jerrycan-specific lints (spec §5.3 ring 3). v0 set:
//! JL0001 route-crate lib.rs exports more than `module()`
//! JL0002 handler names don't match design operation_ids
//! JL0003 a generated (tool-owned) file was hand-edited

use super::checkpipe::Diagnostic;
use super::design::{Design, ModuleDesign};
use super::mounting;
use std::path::Path;

fn d(code: &str, file: Option<String>, line: Option<u64>, message: String, suggestion: &str, doc: &str) -> Diagnostic {
    Diagnostic {
        code: code.into(),
        file,
        line,
        message,
        suggestion: Some(suggestion.into()),
        doc_url: Some(doc.into()),
    }
}

pub fn run(root: &Path, design: &Design) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for m in &design.modules {
        lint_public_surface(root, m, &mut out);
        lint_handlers(root, m, &format!("crates/routes/{}/src", m.name), &mut out);
    }
    lint_generated_drift(root, design, &mut out);
    out
}

/// JL0001: scan a route crate's lib.rs for public items besides `pub fn module()`.
fn lint_public_surface(root: &Path, m: &ModuleDesign, out: &mut Vec<Diagnostic>) {
    let rel = format!("crates/routes/{}/src/lib.rs", m.name);
    let Ok(content) = std::fs::read_to_string(root.join(&rel)) else { return };
    for (i, line) in content.lines().enumerate() {
        let t = line.trim_start();
        if !t.starts_with("pub ") || t.starts_with("pub(") {
            continue;
        }
        if t.starts_with("pub fn module(") {
            continue;
        }
        out.push(d(
            "JL0001",
            Some(rel.clone()),
            Some(i as u64 + 1),
            format!("route crate `{}` exports more than `module()`: `{}`", m.name, t.trim_end()),
            "make it pub(crate), move shared types to the shared crate, or expose via module()",
            "jerrycan docs modules#anti-patterns",
        ));
    }
}

/// JL0002: every design endpoint needs `async fn <operation_id>(` in its unit's handlers.rs.
fn lint_handlers(root: &Path, m: &ModuleDesign, src_rel: &str, out: &mut Vec<Diagnostic>) {
    let rel = format!("{src_rel}/handlers.rs");
    let content = std::fs::read_to_string(root.join(&rel)).unwrap_or_default();
    for ep in &m.endpoints {
        if !content.contains(&format!("async fn {}(", ep.operation_id)) {
            out.push(d(
                "JL0002",
                Some(rel.clone()),
                None,
                format!("handler `{}` (from design.json) is missing in {rel}", ep.operation_id),
                "add the handler with that exact name, or fix the design's operation_id",
                "jerrycan docs modules",
            ));
        }
    }
    for sub in &m.subroutes {
        lint_handlers(root, sub, &format!("{src_rel}/subroutes/{}", sub.name.replace('-', "_")), out);
    }
}

/// JL0003: tool-owned app/src/main.rs must equal the regenerator's output exactly.
fn lint_generated_drift(root: &Path, design: &Design, out: &mut Vec<Diagnostic>) {
    let rel = "crates/app/src/main.rs";
    let on_disk = std::fs::read_to_string(root.join(rel)).unwrap_or_default();
    if on_disk != mounting::expected_main(design) {
        out.push(d(
            "JL0003",
            Some(rel.into()),
            None,
            "generated file drifted from the design (hand-edited, or design.json changed without regenerating)".into(),
            "run `jerrycan generate route <module>` to regenerate mounting; never hand-edit GENERATED files",
            "jerrycan docs app#anti-patterns",
        ));
    }
}
```

- [ ] **Step 4: Run to verify pass** — `cargo test -p jerrycan --test checkpipe` → 8 PASS. Full gate green.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan/src/platform/lints.rs crates/jerrycan/tests/checkpipe.rs
git commit -m "Add jerrycan lints: public surface, handler naming, generated drift"
```

---

### Task 12: `jerrycan check` + `jerrycan test` commands

**Files:**
- Modify: `crates/jerrycan/src/main.rs`
- Modify: `crates/jerrycan/tests/conformance.rs` (heavy check test)

- [ ] **Step 1: Implement the check orchestration (in main.rs)**

Wire `Cmd::Check`/`Cmd::Test` in `run()`:

```rust
        Cmd::Check { module } => cmd_check(module.as_deref(), cli.json),
        Cmd::Test { module } => cmd_test(module.as_deref()),
```

And add (with `use jerrycan::platform::{checkpipe, lints};`):

```rust
fn cmd_check(module: Option<&str>, json_mode: bool) -> Result<(), Failure> {
    let root = app_root()?;
    let design = load_design(&root.join("design.json"))?;

    // Order per cli-ux.md: build → clippy → audit → deny → tests → jerrycan lints.
    // First failing class stops the pipeline; all diagnostics in that class are kept.
    let mut diagnostics = Vec::new();
    let mut failed_class: Option<&str> = None;

    let classes: Vec<(&str, Box<dyn FnOnce() -> Result<Vec<checkpipe::Diagnostic>, Failure>>)> = vec![
        ("build", Box::new(|| checkpipe::run_build(&root, module).map_err(Failure::environment))),
        ("clippy", Box::new(|| checkpipe::run_clippy(&root, module).map_err(Failure::environment))),
        ("audit", Box::new(|| tool(checkpipe::run_audit(&root)))),
        ("deny", Box::new(|| tool(checkpipe::run_deny(&root)))),
        ("tests", Box::new(|| checkpipe::run_tests(&root, module).map_err(Failure::environment))),
        ("jerrycan lints", Box::new(|| Ok(lints::run(&root, &design)))),
    ];
    for (name, step) in classes {
        let ds = step()?;
        if !ds.is_empty() {
            diagnostics = ds;
            failed_class = Some(name);
            break;
        }
    }

    let ok = failed_class.is_none();
    let next_step = match failed_class {
        None => "all green — implement remaining stubs, or proceed toward packaging (Phase 3)".to_string(),
        Some(c) => format!("fix the {c} diagnostics, then re-run jerrycan check"),
    };
    let report = checkpipe::CheckReport { ok, diagnostics, next_step };
    if json_mode {
        println!("{}", serde_json::to_string(&report).expect("report serializes"));
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
    if ok {
        eprintln!("check: all green");
        Ok(())
    } else {
        Err(Failure::gate(format!("{} failed", failed_class.expect("set when !ok"))))
    }
}

fn tool(step: Result<checkpipe::ToolStep, String>) -> Result<Vec<checkpipe::Diagnostic>, Failure> {
    match step.map_err(Failure::environment)? {
        checkpipe::ToolStep::Missing(hint) => Err(Failure::environment(hint)),
        checkpipe::ToolStep::Ran(ds) => Ok(ds),
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
    let status = c.status().map_err(|e| Failure::environment(e.to_string()))?;
    if status.success() {
        Ok(())
    } else {
        Err(Failure::gate("test suite failed".into()))
    }
}
```

Borrow note (pre-solved): the `classes` vec of boxed `FnOnce` closures each borrow `root`/`design`/`module` — they are all `&`-captures used inside one function scope, which compiles because the closures are consumed (FnOnce) within the same scope. If the borrow checker objects to `root` moving into multiple closures, capture by reference explicitly: `let root = &root; let design = &design;` before building the vec.

NOTE on gate-failure exit: `Failure::gate` after the JSON was already printed gives exit 1 AND the payload on stdout — exactly cli-ux.md (`--json`: stdout carries the report; exit code still 1; the human summary goes to stderr). The double-print of diagnostics (loop above) only goes to stderr, so stdout remains exactly one JSON document.

- [ ] **Step 2: Add the heavy conformance check test (append to tests/conformance.rs)**

```rust
#[test]
#[ignore = "heavy: full verification pipeline incl. cargo-audit/cargo-deny"]
fn fresh_scaffold_passes_jerrycan_check() {
    let tmp = tempfile::tempdir().unwrap();
    let app = scaffold_golden(tmp.path());
    let out = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
        .current_dir(&app)
        .args(["--json", "check"])
        .output()
        .unwrap();
    let payload: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("check --json emits one JSON document");
    assert_eq!(payload["ok"], true, "diagnostics: {}", payload["diagnostics"]);
    assert!(out.status.success());
}
```

- [ ] **Step 3: Run**

Fast: `cargo test -p jerrycan --test cli` (still green).
Heavy (requires `cargo install cargo-audit cargo-deny` once): `cargo test -p jerrycan --test conformance -- --include-ignored` → both heavy tests PASS. The deny step needs a `deny.toml` in generated apps — **if `cargo deny check` fails on the fresh scaffold for lack of config, add this template to Task 5's templates.rs as `DENY_TOML` and write it in scaffold.rs (record the addition):**

```toml
# Generated by jerrycan — dependency policy for `jerrycan check`.
[licenses]
allow = ["MIT", "Apache-2.0", "Unicode-3.0", "BSD-3-Clause", "ISC", "Zlib"]
[advisories]
yanked = "deny"
[bans]
[sources]
unknown-registry = "deny"
unknown-git = "deny"
```

- [ ] **Step 4: Commit**

```bash
git add crates/jerrycan/src/main.rs crates/jerrycan/tests/conformance.rs crates/jerrycan/src/platform/templates.rs crates/jerrycan/src/platform/scaffold.rs
git commit -m "Add jerrycan check verification gate and test command"
```

---
### Task 13: Docs index + `jerrycan docs` (`docsidx.rs`)

**Files:**
- Replace stub: `crates/jerrycan/src/platform/docsidx.rs`
- Modify: `crates/jerrycan/src/main.rs`
- Modify: `crates/jerrycan/tests/cli.rs`

- [ ] **Step 1: Write the failing tests**

In `docsidx.rs` (unit tests):

```rust
//! Embedded AI-native docs (docs/ai) + search. The same bytes the doc-tests run.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_returns_whole_pages_and_anchored_sections() {
        let page = get("dependencies", None).unwrap();
        assert!(page.contains("# Dependencies"));
        let section = get("dependencies", Some("errors-youll-hit")).unwrap();
        assert!(section.contains("JC1001"));
        assert!(!section.contains("## Minimal example"), "section slice only");
        assert!(get("nonsense", None).is_none());
    }

    #[test]
    fn search_finds_pages_with_anchors_and_snippets() {
        let results = search("override_dep", 5);
        assert!(!results.is_empty());
        assert_eq!(results[0].page, "testing");
        assert!(results[0].snippet.to_lowercase().contains("override"));
        assert!(search("zzz-not-a-real-term", 5).is_empty());
    }
}
```

Append to `tests/cli.rs`:

```rust
#[test]
fn docs_command_prints_pages_and_searches() {
    let out = jerrycan().args(["docs", "dependencies"]).output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("# Dependencies"));

    let out = jerrycan().args(["--json", "docs", "--search", "override_dep"]).output().unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(payload["results"][0]["page"], "testing");

    let out = jerrycan().args(["docs", "nope"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}
```

- [ ] **Step 2: Run to verify failure** — compile FAILURE.

- [ ] **Step 3: Implement `docsidx.rs`**

```rust
use serde::Serialize;

/// (topic, markdown) — embedded at compile time from the SAME files the
/// doc-tests execute, so served docs can never drift from verified docs.
pub const PAGES: &[(&str, &str)] = &[
    ("app", include_str!("../../../../docs/ai/01-app.md")),
    ("modules", include_str!("../../../../docs/ai/02-modules.md")),
    ("extractors", include_str!("../../../../docs/ai/03-extractors.md")),
    ("dependencies", include_str!("../../../../docs/ai/04-dependencies.md")),
    ("errors", include_str!("../../../../docs/ai/05-errors.md")),
    ("middleware", include_str!("../../../../docs/ai/06-middleware.md")),
    ("testing", include_str!("../../../../docs/ai/07-testing.md")),
];

fn slug(heading: &str) -> String {
    heading
        .trim_start_matches('#')
        .trim()
        .to_lowercase()
        .chars()
        .filter_map(|c| match c {
            'a'..='z' | '0'..='9' => Some(c),
            ' ' => Some('-'),
            _ => None,
        })
        .collect()
}

/// A whole page, or one `##` section by slug ("errors-youll-hit").
pub fn get(page: &str, anchor: Option<&str>) -> Option<String> {
    let (_, md) = PAGES.iter().find(|(name, _)| *name == page)?;
    let Some(anchor) = anchor else { return Some((*md).to_string()) };
    let mut collecting = false;
    let mut out = String::new();
    for line in md.lines() {
        if line.starts_with("## ") {
            if collecting {
                break;
            }
            collecting = slug(line) == anchor;
        }
        if collecting {
            out.push_str(line);
            out.push('\n');
        }
    }
    (!out.is_empty()).then_some(out)
}

#[derive(Debug, Serialize)]
pub struct SearchHit {
    pub page: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
    pub snippet: String,
}

/// Case-insensitive substring search; hits ranked by per-page match count.
pub fn search(query: &str, limit: usize) -> Vec<SearchHit> {
    let q = query.to_lowercase();
    let mut scored: Vec<(usize, SearchHit)> = Vec::new();
    for (name, md) in PAGES {
        let mut count = 0;
        let mut first: Option<(Option<String>, String)> = None;
        let mut current_anchor: Option<String> = None;
        for line in md.lines() {
            if line.starts_with("## ") {
                current_anchor = Some(slug(line));
            }
            if line.to_lowercase().contains(&q) {
                count += 1;
                if first.is_none() {
                    first = Some((current_anchor.clone(), line.trim().to_string()));
                }
            }
        }
        if let Some((anchor, snippet)) = first {
            scored.push((count, SearchHit { page: (*name).to_string(), anchor, snippet }));
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    scored.into_iter().take(limit).map(|(_, h)| h).collect()
}
```

Wire `Cmd::Docs` in `run()`:

```rust
        Cmd::Docs { topic, search } => cmd_docs(topic.as_deref(), search.as_deref(), cli.json),
```

```rust
fn cmd_docs(topic: Option<&str>, query: Option<&str>, json_mode: bool) -> Result<(), Failure> {
    use jerrycan::platform::docsidx;
    if let Some(q) = query {
        let results = docsidx::search(q, 5);
        let payload = serde_json::json!({ "results": results });
        if json_mode {
            println!("{payload}");
        } else {
            for r in payload["results"].as_array().unwrap() {
                println!("{} ({}#{})", r["snippet"].as_str().unwrap(), r["page"].as_str().unwrap(), r["anchor"].as_str().unwrap_or(""));
            }
        }
        return Ok(());
    }
    let Some(topic) = topic else {
        return Err(Failure::usage("provide a topic (`jerrycan docs dependencies`) or --search <query>"));
    };
    let (page, anchor) = match topic.split_once('#') {
        Some((p, a)) => (p, Some(a)),
        None => (topic, None),
    };
    let md = docsidx::get(page, anchor).ok_or_else(|| {
        let names: Vec<&str> = docsidx::PAGES.iter().map(|(n, _)| *n).collect();
        Failure::usage(format!("unknown docs page `{page}` — available: {}", names.join(", ")))
    })?;
    if json_mode {
        println!("{}", serde_json::json!({ "markdown": md }));
    } else {
        println!("{md}");
    }
    Ok(())
}
```

- [ ] **Step 4: Run to verify pass** — `cargo test -p jerrycan` and `--test cli` → all PASS (docs page text goes to STDOUT here because the page IS the result, per cli-ux output conventions). Full gate green.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan/src/platform/docsidx.rs crates/jerrycan/src/main.rs crates/jerrycan/tests/cli.rs
git commit -m "Add embedded docs index with search and docs command"
```

---

### Task 14: `jerrycan dev` — mtime-polling auto-reload

**Files:**
- Modify: `crates/jerrycan/src/main.rs`

- [ ] **Step 1: Implement (no automated test — interactive process; the mtime scanner gets a unit test)**

Wire `Cmd::Dev` and add:

```rust
        Cmd::Dev { addr } => cmd_dev(addr.as_deref()),
```

```rust
/// Newest mtime across all Rust sources + manifests under the app.
fn newest_mtime(root: &Path) -> std::time::SystemTime {
    fn walk(dir: &Path, newest: &mut std::time::SystemTime) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n == "target") {
                    continue;
                }
                walk(&path, newest);
            } else if path.extension().is_some_and(|e| e == "rs")
                || path.file_name().is_some_and(|n| n == "Cargo.toml" || n == "design.json")
            {
                if let Ok(m) = entry.metadata().and_then(|m| m.modified()) {
                    if m > *newest {
                        *newest = m;
                    }
                }
            }
        }
    }
    let mut newest = std::time::SystemTime::UNIX_EPOCH;
    walk(root, &mut newest);
    newest
}

fn cmd_dev(addr: Option<&str>) -> Result<(), Failure> {
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
            c.spawn().map_err(|e| Failure::environment(format!("cargo run failed to start: {e}")))?
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
```

Add a unit test for the scanner (in `platform/mod.rs` move `newest_mtime` there as `pub fn newest_mtime` and import it from main.rs — testable home):

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn newest_mtime_sees_rs_files_and_skips_target() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::create_dir_all(tmp.path().join("target")).unwrap();
        std::fs::write(tmp.path().join("src/a.rs"), "x").unwrap();
        let t1 = super::newest_mtime(tmp.path());
        assert!(t1 > std::time::SystemTime::UNIX_EPOCH);
        std::fs::write(tmp.path().join("target/junk.rs"), "y").unwrap();
        assert_eq!(super::newest_mtime(tmp.path()), t1, "target/ is ignored");
    }
}
```

(Manual verification step: scaffold the golden app, `jerrycan dev`, touch a handler file, watch the restart message. Record that you did it.)

- [ ] **Step 2: Run gates and commit**

`cargo test --workspace` green; clippy/fmt green.

```bash
git add crates/jerrycan/src/main.rs crates/jerrycan/src/platform/mod.rs
git commit -m "Add dev command with mtime-polling auto-reload"
```

---

### Task 15: MCP stdio server — JSON-RPC core (`mcp.rs`)

Newline-delimited JSON-RPC 2.0 over stdio (the MCP stdio transport). Synchronous loop; `tools/list` serves the embedded contract file verbatim — zero drift by construction.

**Files:**
- Replace stub: `crates/jerrycan/src/platform/mcp.rs`
- Modify: `crates/jerrycan/src/main.rs` (wire `Cmd::Mcp`)
- Create: `crates/jerrycan/tests/mcp.rs`

- [ ] **Step 1: Write the failing tests**

Create `crates/jerrycan/tests/mcp.rs`:

```rust
//! Drives the real binary over stdio with raw JSON-RPC lines.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

struct McpClient {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: i64,
}

impl McpClient {
    fn start_in(dir: &std::path::Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_jerrycan"))
            .arg("mcp")
            .current_dir(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut c = Self { child, stdin, stdout, next_id: 1 };
        let init = c.request("initialize", serde_json::json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}));
        assert_eq!(init["serverInfo"]["name"], "jerrycan");
        c.notify("notifications/initialized", serde_json::json!({}));
        c
    }

    fn request(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        let id = self.next_id;
        self.next_id += 1;
        let msg = serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(self.stdin, "{msg}").unwrap();
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["id"], id, "response id matches: {v}");
        assert!(v.get("error").is_none(), "unexpected JSON-RPC error: {v}");
        v["result"].clone()
    }

    fn notify(&mut self, method: &str, params: serde_json::Value) {
        let msg = serde_json::json!({"jsonrpc": "2.0", "method": method, "params": params});
        writeln!(self.stdin, "{msg}").unwrap();
    }

    /// tools/call returning the parsed inner JSON payload.
    fn call_tool(&mut self, name: &str, args: serde_json::Value) -> (bool, serde_json::Value) {
        let result = self.request("tools/call", serde_json::json!({"name": name, "arguments": args}));
        let is_error = result["isError"].as_bool().unwrap_or(false);
        let text = result["content"][0]["text"].as_str().expect("text content");
        (is_error, serde_json::from_str(text).expect("payload is JSON"))
    }

    fn shutdown(mut self) {
        drop(self.stdin);
        let status = self.child.wait().unwrap();
        assert!(status.success(), "clean exit on stdin EOF");
        drop(self.stdout);
    }
}

#[test]
fn initialize_list_and_unknown_method() {
    let tmp = tempfile::tempdir().unwrap();
    let mut c = McpClient::start_in(tmp.path());

    let tools = c.request("tools/list", serde_json::json!({}));
    let names: Vec<&str> = tools["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names.len(), 9, "all 9 contract tools served");
    assert!(names.contains(&"jerrycan_design") && names.contains(&"jerrycan_check"));

    // Unknown method → -32601, server keeps running.
    let msg = serde_json::json!({"jsonrpc": "2.0", "id": 99, "method": "bogus/method", "params": {}});
    writeln!(c.stdin, "{msg}").unwrap();
    let mut line = String::new();
    c.stdout.read_line(&mut line).unwrap();
    let v: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(v["error"]["code"], -32601);

    let pong = c.request("ping", serde_json::json!({}));
    assert!(pong.as_object().unwrap().is_empty());
    c.shutdown();
}

#[test]
fn docs_tools_work_through_mcp() {
    let tmp = tempfile::tempdir().unwrap();
    let mut c = McpClient::start_in(tmp.path());
    let (err, payload) = c.call_tool("jerrycan_docs_search", serde_json::json!({"query": "override_dep"}));
    assert!(!err);
    assert_eq!(payload["results"][0]["page"], "testing");
    let (err, payload) = c.call_tool("jerrycan_docs_get", serde_json::json!({"page": "errors"}));
    assert!(!err);
    assert!(payload["markdown"].as_str().unwrap().contains("JC0404"));
    c.shutdown();
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p jerrycan --test mcp` → tests FAIL (Mcp arm returns the stub usage error).

- [ ] **Step 3: Implement the core loop in `mcp.rs`**

```rust
//! Hand-rolled MCP server: newline-delimited JSON-RPC 2.0 over stdio.
//! tools/list serves the embedded contract file — drift is impossible.

use serde_json::{json, Value};
use std::io::{BufRead, Write};

/// The frozen tool contracts, embedded at compile time.
pub const CONTRACTS: &str = include_str!("../../../../docs/contracts/mcp-tools.json");

const PROTOCOL_VERSION: &str = "2025-06-18";

pub fn serve_stdio() -> Result<(), String> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(response) = handle_message(&line) {
            let mut out = stdout.lock();
            writeln!(out, "{response}").map_err(|e| e.to_string())?;
            out.flush().map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn rpc_error(id: Value, code: i64, message: &str) -> String {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}).to_string()
}

fn rpc_result(id: Value, result: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string()
}

/// Handle one message; None for notifications (no response).
pub fn handle_message(line: &str) -> Option<String> {
    let Ok(msg) = serde_json::from_str::<Value>(line) else {
        return Some(rpc_error(Value::Null, -32700, "parse error"));
    };
    let id = msg["id"].clone();
    let method = msg["method"].as_str().unwrap_or("");
    let params = &msg["params"];

    if id.is_null() {
        return None; // notification (initialized, cancelled, …): no response
    }
    let result = match method {
        "initialize" => json!({
            "protocolVersion": params["protocolVersion"].as_str().unwrap_or(PROTOCOL_VERSION),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "jerrycan", "version": env!("CARGO_PKG_VERSION") },
        }),
        "ping" => json!({}),
        "tools/list" => {
            let contracts: Value = serde_json::from_str(CONTRACTS).expect("embedded contract is valid JSON");
            let tools: Vec<Value> = contracts["tools"]
                .as_array()
                .expect("tools array")
                .iter()
                .map(|t| {
                    json!({
                        "name": t["name"],
                        "description": t["description"],
                        "inputSchema": t["inputSchema"],
                    })
                })
                .collect();
            json!({ "tools": tools })
        }
        "tools/call" => {
            let name = params["name"].as_str().unwrap_or("");
            let (is_error, payload) = super::mcp_dispatch::dispatch(name, &params["arguments"]);
            json!({
                "content": [{ "type": "text", "text": payload.to_string() }],
                "isError": is_error,
            })
        }
        _ => return Some(rpc_error(id, -32601, &format!("method not found: {method}"))),
    };
    Some(rpc_result(id, result))
}
```

Add `pub mod mcp_dispatch;` to `platform/mod.rs` and create `crates/jerrycan/src/platform/mcp_dispatch.rs` with a stub that makes Task 15's tests pass for docs tools only (Task 16 completes it):

```rust
//! tools/call dispatch — the MCP twins of the CLI commands.

use serde_json::{json, Value};

pub fn dispatch(name: &str, args: &Value) -> (bool, Value) {
    match name {
        "jerrycan_docs_search" => {
            let query = args["query"].as_str().unwrap_or("");
            let limit = args["limit"].as_u64().unwrap_or(5) as usize;
            (false, json!({ "results": super::docsidx::search(query, limit) }))
        }
        "jerrycan_docs_get" => {
            let page = args["page"].as_str().unwrap_or("");
            match super::docsidx::get(page, args["anchor"].as_str()) {
                Some(md) => (false, json!({ "markdown": md })),
                None => (true, json!({ "error": format!("unknown docs page `{page}`") })),
            }
        }
        other => (true, json!({ "error": format!("tool `{other}` lands in Task 16") })),
    }
}
```

Wire in `main.rs`:

```rust
        Cmd::Mcp => jerrycan::platform::mcp::serve_stdio().map_err(Failure::environment),
```

- [ ] **Step 4: Run to verify pass** — `cargo test -p jerrycan --test mcp` → 2 PASS. Full gate green.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan/src/platform/mcp.rs crates/jerrycan/src/platform/mcp_dispatch.rs crates/jerrycan/src/platform/mod.rs crates/jerrycan/src/main.rs crates/jerrycan/tests/mcp.rs
git commit -m "Add MCP stdio server with JSON-RPC core and docs tools"
```

---
### Task 16: MCP workflow tools — design/scaffold/generate/check/list (+ honest stubs)

**Files:**
- Modify: `crates/jerrycan/src/platform/mcp_dispatch.rs`
- Modify: `crates/jerrycan/src/platform/checkpipe.rs` (extract `run_all`)
- Modify: `crates/jerrycan/src/platform/genroute.rs` (extract `route_map`)
- Modify: `crates/jerrycan/src/main.rs` (reuse both extractions)
- Modify: `crates/jerrycan/tests/mcp.rs`

- [ ] **Step 1: Extract shared cores so CLI and MCP run literally the same code**

(a) Move the 6-class pipeline from `cmd_check` (main.rs) into `checkpipe.rs`:

```rust
/// The whole gate. Err(String) = environment problem (missing tool), not a gate failure.
pub fn run_all(
    root: &Path,
    design: &crate::platform::design::Design,
    module: Option<&str>,
) -> Result<CheckReport, String> {
    let mut diagnostics = Vec::new();
    let mut failed_class: Option<&str> = None;

    let steps: [(&str, Box<dyn FnOnce() -> Result<Vec<Diagnostic>, String>>); 6] = [
        ("build", Box::new(|| run_build(root, module))),
        ("clippy", Box::new(|| run_clippy(root, module))),
        ("audit", Box::new(|| match run_audit(root)? {
            ToolStep::Missing(hint) => Err(hint),
            ToolStep::Ran(ds) => Ok(ds),
        })),
        ("deny", Box::new(|| match run_deny(root)? {
            ToolStep::Missing(hint) => Err(hint),
            ToolStep::Ran(ds) => Ok(ds),
        })),
        ("tests", Box::new(|| run_tests(root, module))),
        ("jerrycan lints", Box::new(|| Ok(super::lints::run(root, design)))),
    ];
    for (name, step) in steps {
        let ds = step()?;
        if !ds.is_empty() {
            diagnostics = ds;
            failed_class = Some(name);
            break;
        }
    }
    let ok = failed_class.is_none();
    let next_step = match failed_class {
        None => "all green — implement remaining stubs, or proceed toward packaging (Phase 3)".to_string(),
        Some(c) => format!("fix the {c} diagnostics, then re-run jerrycan check"),
    };
    Ok(CheckReport { ok, diagnostics, next_step })
}
```

`cmd_check` in main.rs shrinks to: load design → `checkpipe::run_all(...)` (`Err` → `Failure::environment`) → print JSON/human → exit. Re-run `cargo test -p jerrycan --test cli` and the heavy check test to prove behavior is unchanged.

(b) Move the route walk from `cmd_list_routes` into `genroute.rs`:

```rust
#[derive(Debug, serde::Serialize)]
pub struct RouteEntry {
    pub method: String,
    pub path: String,
    pub module: String,
    pub handler: String,
}

pub fn route_map(design: &Design) -> Vec<RouteEntry> {
    fn walk(m: &ModuleDesign, prefix: &str, top: &str, out: &mut Vec<RouteEntry>) {
        let base = format!("{}{}", prefix, m.effective_mount());
        for ep in &m.endpoints {
            out.push(RouteEntry {
                method: format!("{:?}", ep.method),
                path: format!("{}{}", base.trim_end_matches('/'), ep.path),
                module: top.to_string(),
                handler: ep.operation_id.clone(),
            });
        }
        for sub in &m.subroutes {
            walk(sub, &base, top, out);
        }
    }
    let mut out = Vec::new();
    for m in &design.modules {
        walk(m, "", &m.name, &mut out);
    }
    out
}
```

`cmd_list_routes` now serializes `route_map(&design)`. (`serde` is already a cli-feature dep; add `use serde::Serialize;` if needed.)

- [ ] **Step 2: Write the failing MCP workflow tests (append to tests/mcp.rs)**

```rust
const GOLDEN: &str = include_str!("../../../conformance/designs/todo-api.design.json");

#[test]
fn design_tool_questions_then_completes() {
    let tmp = tempfile::tempdir().unwrap();
    let mut c = McpClient::start_in(tmp.path());

    // No draft → the template + a pointed ask, never code.
    let (err, payload) = c.call_tool("jerrycan_design", serde_json::json!({"requirements": "todo backend"}));
    assert!(!err);
    assert_eq!(payload["status"], "questions");
    assert!(payload["questions"][0]["question"].as_str().unwrap().contains("draft"));

    // Broken draft → pointed questions with JSON-pointer ids.
    let mut bad: serde_json::Value = serde_json::from_str(GOLDEN).unwrap();
    bad["name"] = serde_json::json!("Todo API");
    let (err, payload) = c.call_tool("jerrycan_design", serde_json::json!({"requirements": "todo backend", "draft": bad}));
    assert!(!err);
    assert_eq!(payload["status"], "questions");
    assert_eq!(payload["questions"][0]["id"], "/name");

    // Complete draft → written to disk, design_path returned.
    let good: serde_json::Value = serde_json::from_str(GOLDEN).unwrap();
    let (err, payload) = c.call_tool("jerrycan_design", serde_json::json!({"requirements": "todo backend", "draft": good}));
    assert!(!err);
    assert_eq!(payload["status"], "complete");
    let design_path = payload["design_path"].as_str().unwrap();
    assert!(std::path::Path::new(design_path).exists());
    assert!(payload["next_step"].as_str().unwrap().contains("scaffold"));
    c.shutdown();
}

#[test]
fn scaffold_generate_and_list_through_mcp() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("design.json"), GOLDEN).unwrap();
    let mut c = McpClient::start_in(tmp.path());

    let app_dir = tmp.path().join("todo-api");
    let (err, payload) = c.call_tool("jerrycan_scaffold", serde_json::json!({
        "design_path": tmp.path().join("design.json").to_str().unwrap(),
        "directory": app_dir.to_str().unwrap(),
    }));
    assert!(!err, "{payload}");
    assert!(payload["created"].as_array().unwrap().len() > 10);

    // Incremental generate with a design_slice (the MCP-only path).
    let (err, payload) = c.call_tool("jerrycan_generate", serde_json::json!({
        "kind": "route",
        "path": "tags",
        "directory": app_dir.to_str().unwrap(),
        "design_slice": { "name": "tags", "endpoints": [
            { "operation_id": "list_tags", "method": "GET", "path": "/", "success": { "status": 200 } }
        ]},
    }));
    assert!(!err, "{payload}");
    assert!(payload["modified"].as_array().unwrap().iter().any(|p| p == "crates/app/src/main.rs"));
    assert!(app_dir.join("crates/routes/tags/src/lib.rs").exists());

    let (err, payload) = c.call_tool("jerrycan_list_routes", serde_json::json!({"directory": app_dir.to_str().unwrap()}));
    assert!(!err);
    assert!(payload["routes"].as_array().unwrap().iter().any(|r| r["path"] == "/tags/"));

    // Phase-gated tools answer honestly instead of pretending.
    let (err, payload) = c.call_tool("jerrycan_gen_tests", serde_json::json!({"module": "todos", "directory": app_dir.to_str().unwrap()}));
    assert!(err);
    assert!(payload["error"].as_str().unwrap().contains("Phase 2"));
    let (err, payload) = c.call_tool("jerrycan_package", serde_json::json!({"target": "binary"}));
    assert!(err);
    assert!(payload["error"].as_str().unwrap().contains("Phase 3"));
    c.shutdown();
}
```

- [ ] **Step 3: Run to verify failure** — the new tests FAIL (dispatch stubs).

- [ ] **Step 4: Complete `mcp_dispatch.rs`**

Replace the `other =>` arm with the full dispatch (keeping docs arms):

```rust
use super::design::Design;
use super::{checkpipe, genroute, mounting, questions, scaffold};
use std::path::{Path, PathBuf};

fn root_from(args: &Value) -> PathBuf {
    args["directory"].as_str().map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

fn err_payload(msg: impl Into<String>) -> (bool, Value) {
    (true, json!({ "error": msg.into() }))
}

pub fn dispatch(name: &str, args: &Value) -> (bool, Value) {
    match name {
        "jerrycan_docs_search" => {
            let query = args["query"].as_str().unwrap_or("");
            let limit = args["limit"].as_u64().unwrap_or(5) as usize;
            (false, json!({ "results": super::docsidx::search(query, limit) }))
        }
        "jerrycan_docs_get" => {
            let page = args["page"].as_str().unwrap_or("");
            match super::docsidx::get(page, args["anchor"].as_str()) {
                Some(md) => (false, json!({ "markdown": md })),
                None => (true, json!({ "error": format!("unknown docs page `{page}`") })),
            }
        }

        "jerrycan_design" => {
            let Some(draft) = args.get("draft").filter(|d| !d.is_null()) else {
                let template = include_str!("../../../../conformance/designs/todo-api.design.json");
                return (false, json!({
                    "status": "questions",
                    "questions": [{
                        "id": "/",
                        "question": format!(
                            "Provide a structured `draft` conforming to design-schema.json. Be specific: modules, entities+fields, endpoints with operation_id/method/path/success/errors. Worked example:\n{template}"
                        ),
                    }],
                    "next_step": "author the draft from the requirements, then call jerrycan_design again with it",
                }));
            };
            let design: Design = match serde_json::from_value(draft.clone()) {
                Ok(d) => d,
                Err(e) => {
                    return (false, json!({
                        "status": "questions",
                        "questions": [{ "id": "/", "question": format!("draft does not parse against design-schema.json: {e}") }],
                        "next_step": "fix the draft and call jerrycan_design again",
                    }))
                }
            };
            let qs = questions::validate(&design);
            if !qs.is_empty() {
                return (false, json!({
                    "status": "questions",
                    "questions": qs,
                    "next_step": "answer each question by fixing the draft, then call jerrycan_design again",
                }));
            }
            let path = args["revision_of"].as_str().map(PathBuf::from).unwrap_or_else(|| PathBuf::from("design.json"));
            if let Err(e) = std::fs::write(&path, scaffold::canonical_design_json(&design)) {
                return err_payload(format!("cannot write {}: {e}", path.display()));
            }
            let abs = path.canonicalize().unwrap_or(path);
            (false, json!({
                "status": "complete",
                "design": serde_json::to_value(&design).expect("design serializes"),
                "design_path": abs.display().to_string(),
                "next_step": "call jerrycan_scaffold with this design_path and a target directory",
            }))
        }

        "jerrycan_scaffold" => {
            let (Some(design_path), Some(directory)) = (args["design_path"].as_str(), args["directory"].as_str()) else {
                return err_payload("design_path and directory are required");
            };
            let design = match Design::from_path(Path::new(design_path)) {
                Ok(d) => d,
                Err(e) => return err_payload(e),
            };
            let qs = questions::validate(&design);
            if !qs.is_empty() {
                return (true, json!({ "error": "design is incomplete", "questions": qs }));
            }
            match scaffold::scaffold(Path::new(directory), &design) {
                Ok(created) => (false, json!({
                    "created": created,
                    "next_step": "implement the handler stubs (see jerrycan_list_routes), then jerrycan_check",
                })),
                Err(e) => err_payload(e),
            }
        }

        "jerrycan_generate" => {
            let root = root_from(args);
            let design_path = root.join("design.json");
            let mut design = match Design::from_path(&design_path) {
                Ok(d) => d,
                Err(e) => return err_payload(e),
            };
            let kind = args["kind"].as_str().unwrap_or("");
            let path = args["path"].as_str().unwrap_or("");
            match kind {
                "route" | "subroute" => {
                    if let Some(slice) = args.get("design_slice").filter(|s| !s.is_null()) {
                        let module: super::design::ModuleDesign = match serde_json::from_value(slice.clone()) {
                            Ok(m) => m,
                            Err(e) => return err_payload(format!("design_slice does not parse: {e}")),
                        };
                        if kind == "route" {
                            design.modules.retain(|m| m.name != module.name);
                            design.modules.push(module);
                        } else {
                            let Some((parent_path, _)) = path.rsplit_once('/') else {
                                return err_payload("subroute path must be parent/child");
                            };
                            let Some(parent) = genroute::module_by_path_mut(&mut design, parent_path) else {
                                return err_payload(format!("parent module `{parent_path}` not found"));
                            };
                            parent.subroutes.retain(|s| s.name != module.name);
                            parent.subroutes.push(module);
                        }
                    }
                    let qs = questions::validate(&design);
                    if !qs.is_empty() {
                        return (true, json!({ "error": "design would become incomplete", "questions": qs }));
                    }
                    if genroute::module_by_path(&design, path).is_none() {
                        return err_payload(format!("module `{path}` not in design.json — pass a design_slice or edit the design first"));
                    }
                    if let Err(e) = std::fs::write(&design_path, scaffold::canonical_design_json(&design)) {
                        return err_payload(e.to_string());
                    }
                    let top_name = path.split('/').next().expect("nonempty");
                    let top = design.modules.iter().find(|m| m.name == top_name).expect("validated above");
                    let created = match genroute::write_module(&root.join("crates/routes"), top) {
                        Ok(c) => c,
                        Err(e) => return err_payload(e),
                    };
                    let modified = match mounting::regenerate(&root, &design) {
                        Ok(m) => m,
                        Err(e) => return err_payload(e),
                    };
                    (false, json!({
                        "created": created,
                        "modified": modified,
                        "next_step": format!("implement crates/routes/{top_name}/src/handlers.rs, then jerrycan_check"),
                    }))
                }
                "dependency" => {
                    let Some(module) = args["module"].as_str() else {
                        return err_payload("`module` is required for kind=dependency");
                    };
                    if let Err(e) = genroute::add_dependency(&mut design, module, path) {
                        return err_payload(e);
                    }
                    if let Err(e) = std::fs::write(&design_path, scaffold::canonical_design_json(&design)) {
                        return err_payload(e.to_string());
                    }
                    (false, json!({
                        "created": [],
                        "modified": ["design.json"],
                        "next_step": format!("define `{path}` in the module's deps.rs configure() hook"),
                    }))
                }
                other => err_payload(format!("unknown kind `{other}`")),
            }
        }

        "jerrycan_check" => {
            let root = root_from(args);
            let design = match Design::from_path(&root.join("design.json")) {
                Ok(d) => d,
                Err(e) => return err_payload(e),
            };
            match checkpipe::run_all(&root, &design, args["module"].as_str()) {
                Ok(report) => (false, serde_json::to_value(&report).expect("report serializes")),
                Err(env) => err_payload(env),
            }
        }

        "jerrycan_list_routes" => {
            let root = root_from(args);
            match Design::from_path(&root.join("design.json")) {
                Ok(design) => (false, json!({ "routes": genroute::route_map(&design) })),
                Err(e) => err_payload(e),
            }
        }

        "jerrycan_gen_tests" => (true, json!({
            "error": "jerrycan_gen_tests arrives in Phase 2 (per the roadmap)",
            "next_step": "implement the handler stubs and verify with jerrycan_check",
        })),
        "jerrycan_package" => (true, json!({
            "error": "jerrycan_package arrives in Phase 3 (per the roadmap)",
            "next_step": "verify with jerrycan_check; packaging targets land with jerrycan-observe/auth",
        })),

        other => (true, json!({ "error": format!("unknown tool `{other}`") })),
    }
}
```

- [ ] **Step 5: Run to verify pass** — `cargo test -p jerrycan --test mcp` → 4 PASS; `--test cli` still green (refactors). Full gate green.

- [ ] **Step 6: Commit**

```bash
git add crates/jerrycan/src/platform crates/jerrycan/src/main.rs crates/jerrycan/tests/mcp.rs
git commit -m "Add MCP workflow tools sharing the CLI's check and generation cores"
```

---

### Task 17: Agent-sim — the Phase 1 exit criterion as a test

**Files:**
- Create: `crates/jerrycan/tests/common/mod.rs` (move McpClient here; `tests/mcp.rs` gains `mod common; use common::McpClient;`)
- Create: `conformance/fixtures/todos_handlers.rs`, `comments_handlers.rs`, `users_handlers.rs`
- Modify: `crates/jerrycan/tests/conformance.rs`

- [ ] **Step 1: Extract `McpClient` into `tests/common/mod.rs`**

Move the whole `McpClient` impl from `tests/mcp.rs` verbatim into `crates/jerrycan/tests/common/mod.rs` (make the struct, its FIELDS (`stdin`/`stdout` are poked directly by one test), and methods `pub`, add `pub fn start_in_with_env(dir, envs: &[(&str, &str)])` that applies env vars to the Command; `start_in` delegates with `&[]`). Both test files use `mod common;`. Re-run `cargo test -p jerrycan --test mcp` → still green.

- [ ] **Step 2: Write the fixtures (the "agent's" handler implementations)**

`conformance/fixtures/todos_handlers.rs`:

```rust
//! Conformance fixture: the agent's implementation of the todos handlers.
use super::model::*;
use super::repo::*;
use jerrycan::prelude::*;

pub(crate) async fn list_todos(repo: Dep<TodoRepo>) -> Result<Json<Vec<Todo>>> {
    Ok(Json(repo.all()))
}

pub(crate) async fn create_todo(repo: Dep<TodoRepo>, Json(body): Json<Todo>) -> Result<Created<Todo>> {
    repo.insert(body.clone());
    Ok(Created(body))
}

pub(crate) async fn show_todo(repo: Dep<TodoRepo>, Path(id): Path<i64>) -> Result<Json<Todo>> {
    repo.get(id).map(Json).ok_or_else(Error::not_found)
}

pub(crate) async fn delete_todo(repo: Dep<TodoRepo>, Path(id): Path<i64>) -> Result<NoContent> {
    if repo.remove(id) {
        Ok(NoContent)
    } else {
        Err(Error::not_found())
    }
}
```

`conformance/fixtures/comments_handlers.rs`:

```rust
//! Conformance fixture: the agent's implementation of the comments handlers.
use super::model::*;
use super::repo::*;
use jerrycan::prelude::*;

pub(crate) async fn list_comments(repo: Dep<CommentRepo>) -> Result<Json<Vec<Comment>>> {
    Ok(Json(repo.all()))
}

pub(crate) async fn create_comment(repo: Dep<CommentRepo>, Json(body): Json<Comment>) -> Result<Created<Comment>> {
    repo.insert(body.clone());
    Ok(Created(body))
}
```

`conformance/fixtures/users_handlers.rs`:

```rust
//! Conformance fixture: the agent's implementation of the users handlers.
use super::model::*;
use super::repo::*;
use jerrycan::prelude::*;

pub(crate) async fn list_users(repo: Dep<UserRepo>) -> Result<Json<Vec<User>>> {
    Ok(Json(repo.all()))
}

pub(crate) async fn create_user(repo: Dep<UserRepo>, Json(body): Json<User>) -> Result<Created<User>> {
    repo.insert(body.clone());
    Ok(Created(body))
}
```

- [ ] **Step 3: Write the agent-sim test (append to tests/conformance.rs, plus `mod common;` at top)**

```rust
use std::io::{Read, Write as IoWrite};

/// THE Phase 1 exit criterion: an agent builds a working multi-module CRUD
/// service via MCP only (design → scaffold → implement → check → serve).
#[test]
#[ignore = "heavy: MCP loop + cargo build + live HTTP round-trips"]
fn agent_generates_working_crud_service_via_mcp_only() {
    let tmp = tempfile::tempdir().unwrap();
    let dep = format!(
        "jerrycan = {{ path = \"{}\", default-features = false }}",
        repo_root().join("crates/jerrycan").display()
    );
    let mut c = common::McpClient::start_in_with_env(tmp.path(), &[("JERRYCAN_FRAMEWORK_DEP", &dep)]);

    // 1. design: draft in, validated design.json out.
    let draft: serde_json::Value = serde_json::from_str(GOLDEN).unwrap();
    let (err, payload) = c.call_tool("jerrycan_design", serde_json::json!({"requirements": "multi-module todo backend", "draft": draft}));
    assert!(!err, "{payload}");
    assert_eq!(payload["status"], "complete");
    let design_path = payload["design_path"].as_str().unwrap().to_string();

    // 2. scaffold.
    let app = tmp.path().join("todo-api");
    let (err, payload) = c.call_tool("jerrycan_scaffold", serde_json::json!({"design_path": design_path, "directory": app.to_str().unwrap()}));
    assert!(!err, "{payload}");

    // 3. the "agent" implements the handlers (canned fixtures).
    for (fixture, target) in [
        ("todos_handlers.rs", "crates/routes/todos/src/handlers.rs"),
        ("comments_handlers.rs", "crates/routes/todos/src/subroutes/comments/handlers.rs"),
        ("users_handlers.rs", "crates/routes/users/src/handlers.rs"),
    ] {
        std::fs::copy(repo_root().join("conformance/fixtures").join(fixture), app.join(target)).unwrap();
    }

    // 4. verify: the full gate must be green.
    let (err, payload) = c.call_tool("jerrycan_check", serde_json::json!({"directory": app.to_str().unwrap()}));
    assert!(!err, "{payload}");
    assert_eq!(payload["ok"], true, "diagnostics: {}", payload["diagnostics"]);
    c.shutdown();

    // 5. serve and exercise the CRUD loop over real HTTP.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let addr = format!("127.0.0.1:{port}");
    let mut server = Command::new("cargo")
        .current_dir(&app)
        .env("JERRYCAN_ADDR", &addr)
        .args(["run", "-p", "app"])
        .spawn()
        .unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    let mut connected = None;
    while std::time::Instant::now() < deadline {
        if let Ok(s) = std::net::TcpStream::connect(&addr) {
            connected = Some(s);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    let http = |req: String| -> String {
        let mut s = std::net::TcpStream::connect(&addr).unwrap();
        s.write_all(req.as_bytes()).unwrap();
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).unwrap();
        String::from_utf8_lossy(&buf).into_owned()
    };
    drop(connected.expect("generated app started serving within 120s"));

    let res = http("GET /todos/ HTTP/1.1\r\nHost: l\r\nConnection: close\r\n\r\n".into());
    assert!(res.starts_with("HTTP/1.1 200"), "{res}");
    assert!(res.ends_with("[]"), "empty store first: {res}");

    let body = r#"{"title":"ship phase 1"}"#;
    let res = http(format!(
        "POST /todos/ HTTP/1.1\r\nHost: l\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    ));
    assert!(res.starts_with("HTTP/1.1 201"), "{res}");

    let res = http("GET /todos/1 HTTP/1.1\r\nHost: l\r\nConnection: close\r\n\r\n".into());
    assert!(res.starts_with("HTTP/1.1 200") && res.contains("ship phase 1"), "{res}");

    let res = http("GET /users/ HTTP/1.1\r\nHost: l\r\nConnection: close\r\n\r\n".into());
    assert!(res.starts_with("HTTP/1.1 200"), "multi-module proof: {res}");

    let res = http("DELETE /todos/1 HTTP/1.1\r\nHost: l\r\nConnection: close\r\n\r\n".into());
    assert!(res.starts_with("HTTP/1.1 204"), "{res}");

    let res = http("GET /todos/1 HTTP/1.1\r\nHost: l\r\nConnection: close\r\n\r\n".into());
    assert!(res.starts_with("HTTP/1.1 404"), "{res}");

    let _ = server.kill();
    let _ = server.wait();
}
```

- [ ] **Step 4: Run the full heavy suite**

Run: `cargo test -p jerrycan --test conformance -- --include-ignored`
Expected: all 3 heavy tests PASS. **This green run IS the Phase 1 exit criterion.** Budget 10–20 minutes cold.

- [ ] **Step 5: Commit**

```bash
git add crates/jerrycan/tests conformance/fixtures
git commit -m "Add agent-sim conformance proving the MCP-only CRUD exit criterion"
```

---

### Task 18: Close the 6 documented docs gaps

**Files:**
- Modify: `docs/ai/01-app.md`, `02-modules.md`, `03-extractors.md`, `05-errors.md`, `07-testing.md`
- Modify: `docs/phase1-backlog.md` (remove the now-done section)

- [ ] **Step 1: Apply the additions** (each is doc-tested — `cargo test --doc -p jerrycan` gates every one)

(1) **01-app.md** — in the Signature block, replace `.provide(())` with a meaningful type: change the hidden prelude line to also define `# struct AppConfig { greeting: &'static str }` and the visible line to `.provide(AppConfig { greeting: "hi" })   // .provide(value) — app-wide singleton dependency`.

(2) **01-app.md** — append to the Variations section:

````markdown
Bind explicitly (tests, port 0, socket activation) with `serve_with`; plain
`serve()` reads `JERRYCAN_ADDR` (default `127.0.0.1:8000`):
```rust,no_run
# use jerrycan::prelude::*;
# async fn demo() -> Result<()> {
let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await
    .map_err(|e| Error::internal(format!("bind: {e}")))?;
App::new().route("/ping", get(|| async { "pong" })).serve_with(listener).await
# }
```
````

(3) **02-modules.md** — append to Variations:

````markdown
Full CRUD method sets chain off one route entry — `put`/`patch`/`delete` work
exactly like `get`/`post`:
```rust
# use jerrycan::prelude::*;
# async fn show() -> &'static str { "s" }
# async fn replace() -> &'static str { "r" }
# async fn update() -> &'static str { "u" }
# async fn remove() -> Result<NoContent> { Ok(NoContent) }
let m = Module::new("items")
    .route("/{id}", get(show).put(replace).patch(update).delete(remove));
# let _ = App::new().mount("/items", m).into_test();
```
````

(4) **03-extractors.md** — append to Variations:

````markdown
Query fields are REQUIRED by default — a missing `?limit=` is `400 JC0400`.
Make pagination optional with `Option<T>` or `#[serde(default)]`:
```rust
# use jerrycan::prelude::*;
# use serde::Deserialize;
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
#[derive(Deserialize)]
struct Page { limit: Option<u32>, #[serde(default)] offset: u32 }

async fn list(Query(p): Query<Page>) -> String {
    format!("limit={:?} offset={}", p.limit, p.offset)
}

let t = App::new().route("/items", get(list)).into_test();
assert_eq!(t.get("/items").await.text(), "limit=None offset=0"); // no query string: fine
# }); }
```
````

(5) **05-errors.md** — append to Variations:

````markdown
The full constructor set (prefer these over raw `Error::new`):
```rust
# use jerrycan::prelude::*;
let all = [
    Error::bad_request("bad input"),      // 400 JC0400
    Error::not_found(),                   // 404 JC0404
    Error::method_not_allowed(),          // 405 JC0405
    Error::payload_too_large(),           // 413 JC0413
    Error::unprocessable("bad field"),    // 422 JC0422
    Error::internal("boom"),              // 500 JC0500
];
assert_eq!(all[0].code(), "JC0400");
```
````

(6) **07-testing.md** — append to Variations:

````markdown
Assert on response headers with `headers()`:
```rust
# use jerrycan::prelude::*;
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
let t = App::new().route("/", get(|| async { Json(42) })).into_test();
let res = t.get("/").await;
assert_eq!(res.headers()["content-type"], "application/json");
# }); }
```
````

Then delete the `## Docs page additions (gaps found in review)` section from `docs/phase1-backlog.md` (all six are done).

- [ ] **Step 2: Run** — `cargo test --doc -p jerrycan` → 27 doc-tests PASS (21 + 6 new). Full gate green.

- [ ] **Step 3: Commit**

```bash
git add docs/ai docs/phase1-backlog.md
git commit -m "Close docs gaps: serve_with, optional query fields, CRUD chaining, error constructors, headers"
```

---

### Task 19: CI for the platform + roadmap flip + final gate

**Files:**
- Modify: `.github/workflows/ci.yml`
- Modify: `README.md` (roadmap row)

- [ ] **Step 1: Extend CI**

In `.github/workflows/ci.yml`, after the `Swatinem/rust-cache@v2` step, add:

```yaml
      - name: Install verification tools
        uses: taiki-e/install-action@v2
        with:
          tool: cargo-audit,cargo-deny
```

And after the existing `Tests` step, add:

```yaml
      - name: Conformance (heavy: generated apps must build, check, and serve)
        run: cargo test -p jerrycan --test conformance -- --include-ignored
```

- [ ] **Step 2: Flip the README roadmap row**

`README.md`: change the Phase 1 row to `| **1 — Core loop** | \`jerrycan\` CLI (new/generate/dev/check) + MCP server | ✅ core loop (framework hardening → Phase 1b) |` and move `next` to the Phase 2 row.

- [ ] **Step 3: Run the complete Phase 1 exit gate locally**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p jerrycan --test conformance -- --include-ignored
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
```

ALL green = Phase 1 (core loop) complete: an agent can design → scaffold → implement → verify → serve a multi-module CRUD backend through the MCP tools alone.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/ci.yml README.md
git commit -m "Run conformance suite in CI and mark Phase 1 core loop complete"
```

---

## Execution notes

- **Order:** strictly 1 → 19. Task 9's heavy test is the first generator-reality check — expect template fixes there, record them. Task 17's agent-sim is the exit criterion.
- **Heavy tests** (`#[ignore]`): require `cargo install cargo-audit cargo-deny` locally (or `taiki-e/install-action` in CI) and network for the advisory DB. Everything else stays fast.
- **Unreachable-arm trap:** `run()` keeps a `_ =>` fallback while commands land task-by-task. When the LAST `Cmd` variant is wired (Task 15's `Mcp`), DELETE the `_ =>` arm — it becomes an unreachable pattern and fails `-D warnings`.
- **Pre-solved traps:** clap usage-exit-2 handling (Task 2's try_parse match); `include_str!` paths from `src/platform/` are `../../../../` to repo root; generated handler params are underscore-prefixed so generated apps pass `-D warnings`; the `classes` FnOnce-vec borrow note in Task 12; MCP responses must be SINGLE LINES (serde_json `to_string`, never `to_string_pretty`).
- **Deviation protocol** (same as Phase 0): plan code that fails to compile → fix minimally and record; design-level failures → BLOCKED with the compiler error. Every commit passes fmt/clippy/test gates first.
- **Out of scope (tracked):** framework hardening (Phase 1b plan from docs/phase1-backlog.md), `jerrycan_gen_tests` (Phase 2), `jerrycan_package` (Phase 3), MCP resources (spec §7.2 — tools only in v0), SDK-based MCP transport (fallback if client compat issues appear).
