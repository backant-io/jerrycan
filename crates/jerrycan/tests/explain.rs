//! `jerrycan explain <code>` + registry completeness.

use std::process::Command;

fn jerrycan() -> Command {
    Command::new(env!("CARGO_BIN_EXE_jerrycan"))
}

#[test]
fn explain_prints_title_cause_fix_and_doc() {
    let out = jerrycan().args(["explain", "JC0404"]).output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("JC0404"));
    assert!(text.to_lowercase().contains("not found"));
    assert!(text.contains("docs:") || text.contains("jerrycan docs"));
}

#[test]
fn explain_works_for_a_lint_code_and_is_case_insensitive() {
    let out = jerrycan().args(["explain", "jl0004"]).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("JL0004"));
}

#[test]
fn explain_unknown_code_is_usage_error() {
    let out = jerrycan().args(["explain", "JC9999"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn json_mode_explain_emits_structured_record() {
    let out = jerrycan()
        .args(["--json", "explain", "JC0510"])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["code"], "JC0510");
    assert!(v["title"].is_string() && v["fix"].is_string() && v["doc"].is_string());
}
