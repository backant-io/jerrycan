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
    assert_eq!(
        d.file.as_deref(),
        Some("crates/routes/todos/src/handlers.rs")
    );
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
