//! The verification gate: build → clippy → audit → deny → tests → jerrycan lints.
//! First failing CLASS stops the pipeline; within a class, ALL diagnostics are
//! collected (cli-ux.md). One diagnostics shape, rendered by CLI and MCP alike.

use serde::Serialize;
use std::collections::HashSet;
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

/// Per-test-target (cargo package) pass/fail tally, emitted only for
/// `--no-fail-fast` runs so a TDD consumer sees the whole red→green picture in
/// one shot instead of hand-counting `cargo test --no-fail-fast`.
#[derive(Debug, Clone, Serialize)]
pub struct ModuleTestResult {
    /// The cargo package the tests ran in (e.g. `route-todos`, `jobs`).
    pub module: String,
    pub passed: u64,
    pub failed: u64,
}

#[derive(Debug, Serialize)]
pub struct CheckReport {
    pub ok: bool,
    pub diagnostics: Vec<Diagnostic>,
    /// Per-target pass/fail counts, present only under `--no-fail-fast`. Omitted
    /// (not `[]`) in the default fail-fast run so the payload is byte-identical.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub test_modules: Vec<ModuleTestResult>,
    pub next_step: String,
}

/// Parse `--message-format=json` output (one JSON object per line).
pub fn parse_cargo_json(stdout: &str, source: &str) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    // cargo can repeat the same error across build units (e.g. lib + its tests);
    // dedup on (code, file, line, message) so the agent sees each error once.
    let mut seen: HashSet<(String, Option<String>, Option<u64>, String)> = HashSet::new();
    for line in stdout.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
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
        let file = primary
            .and_then(|p| p["file_name"].as_str())
            .map(str::to_string);
        let line_no = primary.and_then(|p| p["line_start"].as_u64());
        let message = msg["message"].as_str().unwrap_or("").to_string();
        if !seen.insert((code.clone(), file.clone(), line_no, message.clone())) {
            continue;
        }
        out.push(Diagnostic {
            code,
            file,
            line: line_no,
            message,
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

/// Sum every libtest `test result:` line in one package's output into
/// (passed, failed). A package emits one such line per test binary (unit tests,
/// each integration target, doctests), so summing gives the package total.
pub fn parse_test_counts(stdout: &str) -> (u64, u64) {
    let (mut passed, mut failed) = (0u64, 0u64);
    for line in stdout.lines() {
        let Some(rest) = line.trim_start().strip_prefix("test result:") else {
            continue;
        };
        // e.g. " FAILED. 4 passed; 1 failed; 0 ignored; …" — pull the integer
        // that immediately precedes each `passed`/`failed` word.
        let cleaned = rest.replace([';', '.', ','], " ");
        let toks: Vec<&str> = cleaned.split_whitespace().collect();
        for w in toks.windows(2) {
            if let Ok(n) = w[0].parse::<u64>() {
                match w[1] {
                    "passed" => passed += n,
                    "failed" => failed += n,
                    _ => {}
                }
            }
        }
    }
    (passed, failed)
}

/// The last ~400 chars of a captured stream, for surfacing a harness/compile
/// failure tail without dumping the whole log.
fn stream_tail(err: &str) -> String {
    err.chars()
        .rev()
        .take(400)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>()
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
    Ok(parse_cargo_json(
        &String::from_utf8_lossy(&out.stdout),
        "build",
    ))
}

pub fn run_clippy(root: &Path, module: Option<&str>) -> Result<Vec<Diagnostic>, String> {
    let out = cargo_in(root)
        .arg("clippy")
        .args(package_args(module))
        .args([
            "--all-targets",
            "--message-format=json",
            "--",
            "-D",
            "warnings",
        ])
        .output()
        .map_err(|e| format!("cargo clippy not runnable: {e}"))?;
    Ok(parse_cargo_json(
        &String::from_utf8_lossy(&out.stdout),
        "clippy",
    ))
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
        ds.push(Diagnostic {
            code: "TEST0002".into(),
            file: None,
            line: None,
            message: format!(
                "test run failed: {}",
                stream_tail(&String::from_utf8_lossy(&out.stderr))
            ),
            suggestion: None,
            doc_url: None,
        });
    }
    Ok(ds)
}

/// The cargo packages whose test targets carry the acceptance suite: one
/// `route-<module>` per top-level module (subroutes live in their parent's
/// crate), plus `jobs` when the design declares background jobs. Module scope
/// narrows to that single package (mirrors `package_args`).
pub fn test_packages(
    design: &crate::platform::design::Design,
    module: Option<&str>,
) -> Vec<String> {
    match module {
        Some(m) => vec![format!("route-{m}")],
        None => {
            let mut pkgs: Vec<String> = design
                .modules
                .iter()
                .map(|m| format!("route-{}", m.name))
                .collect();
            if design.wants_jobs() {
                pkgs.push("jobs".into());
            }
            pkgs
        }
    }
}

/// One package's captured `cargo test` result, threaded from the IO boundary
/// into the pure aggregator so the fold can be unit-tested without cargo.
pub struct PackageRun {
    pub module: String,
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
}

/// Fold one package's `cargo test --no-fail-fast` result into (diagnostics,
/// tally): EVERY failing test in this package becomes a diagnostic (not just the
/// first), and the tally records the package's total pass/fail split.
fn aggregate_package(run: &PackageRun) -> (Vec<Diagnostic>, ModuleTestResult) {
    let mut diags = parse_test_output(&run.stdout);
    let (passed, failed) = parse_test_counts(&run.stdout);
    if !run.success && diags.is_empty() {
        // Non-zero exit with no parsed failures = compile/harness problem.
        diags.push(Diagnostic {
            code: "TEST0002".into(),
            file: None,
            line: None,
            message: format!(
                "test run failed in {}: {}",
                run.module,
                stream_tail(&run.stderr)
            ),
            suggestion: None,
            doc_url: None,
        });
    }
    (
        diags,
        ModuleTestResult {
            module: run.module.clone(),
            passed,
            failed,
        },
    )
}

/// Pure fold over every package's captured output: the full failure set across
/// ALL packages (never truncated at the first failing one) plus one tally per
/// package, preserving input order. This is the invariant `--no-fail-fast`
/// exists to deliver, so it lives in a cargo-free function that a test can pin.
pub fn aggregate_packages(runs: &[PackageRun]) -> (Vec<Diagnostic>, Vec<ModuleTestResult>) {
    let mut all_diags = Vec::new();
    let mut tallies = Vec::new();
    for run in runs {
        let (diags, tally) = aggregate_package(run);
        all_diags.extend(diags);
        tallies.push(tally);
    }
    (all_diags, tallies)
}

/// `--no-fail-fast`: run every package's test target to completion (cargo's own
/// `--no-fail-fast` runs all targets WITHIN a package) and aggregate the results.
/// Running per package is what makes the tally attributable — cargo's `Running`
/// lines don't name the owning package, so a single `--workspace` run can't be
/// split back apart.
pub fn run_tests_full(
    root: &Path,
    packages: &[String],
) -> Result<(Vec<Diagnostic>, Vec<ModuleTestResult>), String> {
    let mut runs = Vec::with_capacity(packages.len());
    for pkg in packages {
        let out = cargo_in(root)
            .args(["test", "-p", pkg, "--no-fail-fast"])
            .output()
            .map_err(|e| format!("cargo test not runnable: {e}"))?;
        runs.push(PackageRun {
            module: pkg.clone(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            success: out.status.success(),
        });
    }
    Ok(aggregate_packages(&runs))
}

/// External tool steps. A missing tool is an ENVIRONMENT failure (exit 3), not a gate failure.
pub enum ToolStep {
    Missing(String),
    Ran(Vec<Diagnostic>),
}

fn external_tool(
    root: &Path,
    tool: &str,
    args: &[&str],
    code: &str,
    install: &str,
) -> Result<ToolStep, String> {
    let probe = Command::new("cargo").args([tool, "--version"]).output();
    if !probe.map(|o| o.status.success()).unwrap_or(false) {
        return Ok(ToolStep::Missing(format!(
            "cargo-{tool} is not installed — install with `cargo install {install}`"
        )));
    }
    let out = cargo_in(root)
        .arg(tool)
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
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

/// Sync bridge for the async schema-drift check: derive the contract on a
/// throwaway runtime (as `jerrycan db migrate` does) and compare it to the
/// committed schema.json. An Err here is an environment problem (the derivation
/// couldn't run), surfaced as a gate-stopping environment failure.
fn verify_schema(
    root: &Path,
    design: &crate::platform::design::Design,
) -> Result<Vec<Diagnostic>, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(crate::platform::schema::verify_fresh(root, design))
}

/// The whole gate. Err(String) = environment problem (missing tool), not a gate failure.
///
/// audit/deny are workspace-global supply-chain gates; module scope is for fast
/// iteration per cli-ux.md, so they are SKIPPED whenever `module` is `Some`
/// (run a full check before packaging). The CLI surfaces a stderr note about it.
pub fn run_all(
    root: &Path,
    design: &crate::platform::design::Design,
    module: Option<&str>,
    no_fail_fast: bool,
) -> Result<CheckReport, String> {
    let mut diagnostics = Vec::new();
    let mut failed_class: Option<&str> = None;
    // `--no-fail-fast` runs every package's tests and records per-package tallies
    // here (interior mutability so the boxed tests step can write them out while
    // the generic loop keeps its `-> Vec<Diagnostic>` shape). Empty otherwise, so
    // the default payload stays byte-identical.
    let packages = test_packages(design, module);
    let test_modules: std::cell::RefCell<Vec<ModuleTestResult>> =
        std::cell::RefCell::new(Vec::new());

    #[allow(clippy::type_complexity)]
    let mut steps: Vec<(&str, Box<dyn FnOnce() -> Result<Vec<Diagnostic>, String>>)> = vec![
        ("build", Box::new(|| run_build(root, module))),
        ("clippy", Box::new(|| run_clippy(root, module))),
    ];
    if module.is_none() {
        steps.push((
            "audit",
            Box::new(|| match run_audit(root)? {
                ToolStep::Missing(hint) => Err(hint),
                ToolStep::Ran(ds) => Ok(ds),
            }),
        ));
        steps.push((
            "deny",
            Box::new(|| match run_deny(root)? {
                ToolStep::Missing(hint) => Err(hint),
                ToolStep::Ran(ds) => Ok(ds),
            }),
        ));
    }
    steps.push((
        "tests",
        Box::new(|| {
            if no_fail_fast {
                let (ds, tallies) = run_tests_full(root, &packages)?;
                *test_modules.borrow_mut() = tallies;
                Ok(ds)
            } else {
                run_tests(root, module)
            }
        }),
    ));
    steps.push((
        "jerrycan lints",
        Box::new(|| Ok(super::lints::run(root, design))),
    ));
    // schema.json drift is a db-only gate: it only exists once migrations do.
    if module.is_none() && design.wants_db() {
        steps.push(("schema contract", Box::new(|| verify_schema(root, design))));
    }

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
        None => "all green — implement remaining stubs, or proceed toward packaging (Phase 3)"
            .to_string(),
        Some(c) => format!("fix the {c} diagnostics, then re-run jerrycan check"),
    };
    Ok(CheckReport {
        ok,
        diagnostics,
        test_modules: test_modules.into_inner(),
        next_step,
    })
}
