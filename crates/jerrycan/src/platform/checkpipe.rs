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
        out.push(Diagnostic {
            code,
            file: primary
                .and_then(|p| p["file_name"].as_str())
                .map(str::to_string),
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
        let err = String::from_utf8_lossy(&out.stderr);
        ds.push(Diagnostic {
            code: "TEST0002".into(),
            file: None,
            line: None,
            message: format!(
                "test run failed: {}",
                err.chars()
                    .rev()
                    .take(400)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect::<String>()
            ),
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
