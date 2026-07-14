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
fn duplicate_compiler_messages_are_deduped() {
    // cargo can emit the same error across build units (lib + its tests); the
    // agent should see one diagnostic, not N copies of the same (code,file,line,message).
    let doubled = format!("{RUSTC_ERR}\n{RUSTC_ERR}");
    let ds = parse_cargo_json(&doubled, "build");
    assert_eq!(ds.len(), 1, "identical diagnostics collapse to one");
    assert_eq!(ds[0].code, "E0308");
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
        test_modules: vec![],
        next_step: "fix the build diagnostics".into(),
    };
    let v = serde_json::to_value(&report).unwrap();
    assert_eq!(v["ok"], false);
    assert_eq!(v["diagnostics"][0]["code"], "E0308");
    assert!(v["next_step"].is_string());
    // Optional fields are OMITTED when None (matches outputSchema: only code+message required).
    assert!(v["diagnostics"][0].get("suggestion").is_none());
    // The default (fail-fast) payload never grows a `test_modules` key: an empty
    // tally is skipped so the wire shape stays byte-identical to pre-flag runs.
    assert!(
        v.get("test_modules").is_none(),
        "empty test_modules must be omitted, not serialized as []"
    );
}

#[test]
fn test_result_lines_sum_pass_fail_across_binaries() {
    // A package with a unit-test bin and an integration bin emits two
    // `test result:` lines; the tally is their sum, not just the first.
    let out = "\
running 2 tests
test a ... ok
test b ... ok
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

running 3 tests
test c ... ok
test d ... FAILED
test e ... FAILED
test result: FAILED. 1 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s
";
    assert_eq!(parse_test_counts(out), (3, 2));
}

#[test]
fn no_fail_fast_reports_every_failing_target_not_just_the_first() {
    // The whole point of --no-fail-fast: a TDD consumer with several red targets
    // must see the FULL red→green picture in one run. Two packages both fail;
    // the aggregation must carry BOTH tallies AND every failing test name — a
    // fail-fast run would have stopped after `route-todos` and hidden the rest.
    let todos = "\
running 3 tests
test todos::create ... FAILED
test todos::list ... ok
test todos::delete ... FAILED
test result: FAILED. 1 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
";
    let users = "\
running 2 tests
test users::login ... FAILED
test users::logout ... ok
test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
";
    let runs = vec![
        PackageRun {
            module: "route-todos".into(),
            stdout: todos.into(),
            stderr: String::new(),
            success: false,
        },
        PackageRun {
            module: "route-users".into(),
            stdout: users.into(),
            stderr: String::new(),
            success: false,
        },
    ];
    let (diags, tallies) = aggregate_packages(&runs);

    // Per-module counts for EVERY target, in order — not just the first failing one.
    assert_eq!(tallies.len(), 2);
    assert_eq!(tallies[0].module, "route-todos");
    assert_eq!((tallies[0].passed, tallies[0].failed), (1, 2));
    assert_eq!(tallies[1].module, "route-users");
    assert_eq!((tallies[1].passed, tallies[1].failed), (1, 1));

    // The full failure set spans both targets (2 + 1), so the consumer never has
    // to re-run cargo to discover the second target's reds.
    assert_eq!(diags.len(), 3, "all three failures surface: {diags:?}");
    let msgs: String = diags.iter().map(|d| d.message.clone()).collect();
    for failing in ["todos::create", "todos::delete", "users::login"] {
        assert!(msgs.contains(failing), "missing {failing} in {msgs}");
    }
    assert!(diags.iter().all(|d| d.code == "TEST0001"));
}

#[test]
fn harness_failure_surfaces_as_test0002_with_module_attribution() {
    // A package that fails without any `... FAILED` line (link/harness error)
    // must still be attributed to its module, not swallowed.
    let runs = vec![PackageRun {
        module: "route-todos".into(),
        stdout: String::new(),
        stderr: "error: linking with `cc` failed".into(),
        success: false,
    }];
    let (diags, tallies) = aggregate_packages(&runs);
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].code, "TEST0002");
    assert!(diags[0].message.contains("route-todos"));
    assert_eq!((tallies[0].passed, tallies[0].failed), (0, 0));
}

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
    assert!(
        ds.iter()
            .any(|d| d.code == "JL0001" && d.file.as_deref().unwrap().contains("todos/src/lib.rs")),
        "{ds:?}"
    );
}

#[test]
fn jl0002_flags_missing_handlers() {
    let (_tmp, root, design) = scaffolded();
    let handlers = root.join("crates/routes/users/src/handlers.rs");
    let content = std::fs::read_to_string(&handlers)
        .unwrap()
        .replace("list_users", "list_everyone");
    std::fs::write(&handlers, content).unwrap();
    let ds = lints::run(&root, &design);
    assert!(
        ds.iter()
            .any(|d| d.code == "JL0002" && d.message.contains("list_users")),
        "{ds:?}"
    );
}

#[test]
fn jl0003_flags_hand_edited_generated_files() {
    let (_tmp, root, design) = scaffolded();
    let main_rs = root.join("crates/app/src/main.rs");
    let content = std::fs::read_to_string(&main_rs)
        .unwrap()
        .replace("App::new()", "App::new() // tweaked");
    std::fs::write(&main_rs, content).unwrap();
    let ds = lints::run(&root, &design);
    assert!(ds.iter().any(|d| d.code == "JL0003"), "{ds:?}");
}
