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
        diagnostics: vec![
            Diagnostic {
                code: "E0308".into(),
                file: Some("x.rs".into()),
                line: Some(1),
                message: "m".into(),
                suggestion: None,
                doc_url: None,
            },
            // The #123a honest-check diagnostic serializes through the SAME
            // shape as every other class — code+message required, `file` naming
            // the missing acceptance file, no doc_url.
            Diagnostic {
                code: "JC0551".into(),
                file: Some("crates/routes/todos/tests/acceptance.rs".into()),
                line: None,
                message: "no acceptance tests for module `todos` — run `jerrycan gen-tests --module todos`".into(),
                suggestion: Some("run `jerrycan gen-tests --module todos`".into()),
                doc_url: None,
            },
        ],
        test_modules: vec![],
        next_step: "fix the build diagnostics".into(),
    };
    let v = serde_json::to_value(&report).unwrap();
    assert_eq!(v["ok"], false);
    assert_eq!(v["diagnostics"][0]["code"], "E0308");
    assert!(v["next_step"].is_string());
    // Optional fields are OMITTED when None (matches outputSchema: only code+message required).
    assert!(v["diagnostics"][0].get("suggestion").is_none());
    assert_eq!(v["diagnostics"][1]["code"], "JC0551");
    assert!(
        v["diagnostics"][1]["message"]
            .as_str()
            .unwrap()
            .contains("gen-tests --module todos"),
        "JC0551 message names the fix command"
    );
    assert!(v["diagnostics"][1].get("line").is_none());
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

/// #123a: a freshly-scaffolded, never-gen-tested app must trip JC0551 for every
/// top-level module with endpoints — and stop tripping it the moment gen-tests
/// writes the acceptance files. WHY (Rule 9): `check` folded a zero-test `cargo
/// test` (exit 0) into ok:true, so a scaffold that never ran gen-tests read
/// green; this step is what makes that green honest.
#[test]
fn jc0551_fires_on_a_never_gen_tested_scaffold_and_clears_after_gen_tests() {
    use jerrycan::platform::checkpipe::missing_acceptance_tests;
    let (_tmp, root, design) = scaffolded();

    // Fresh scaffold: BOTH golden modules (todos incl. its comments subroute,
    // users) have endpoints and no acceptance file — one JC0551 each, message
    // naming the module and the exact gen-tests command.
    let ds = missing_acceptance_tests(&root, &design, None);
    assert_eq!(
        ds.len(),
        2,
        "one JC0551 per endpoint-bearing module: {ds:?}"
    );
    assert!(ds.iter().all(|d| d.code == "JC0551"));
    for m in ["todos", "users"] {
        assert!(
            ds.iter().any(|d| d.message
                == format!(
                    "no acceptance tests for module `{m}` — run `jerrycan gen-tests --module {m}`"
                )
                && d.file.as_deref() == Some(&*format!("crates/routes/{m}/tests/acceptance.rs"))),
            "JC0551 for `{m}` with the exact message: {ds:?}"
        );
    }

    // Module scope narrows to that module (mirrors test_packages).
    let scoped = missing_acceptance_tests(&root, &design, Some("todos"));
    assert_eq!(scoped.len(), 1, "{scoped:?}");
    assert!(scoped[0].message.contains("`todos`"));

    // gen-tests one module: its JC0551 clears, the other still fires.
    jerrycan::platform::testgen::write_acceptance(&root, &design, "todos").unwrap();
    let ds = missing_acceptance_tests(&root, &design, None);
    assert_eq!(ds.len(), 1, "{ds:?}");
    assert!(ds[0].message.contains("`users`"));

    // gen-tests the rest: green is earned, not hollow.
    jerrycan::platform::testgen::write_acceptance(&root, &design, "users").unwrap();
    assert!(missing_acceptance_tests(&root, &design, None).is_empty());
}

/// #123a: FILE existence is the signal, NOT test count. An all-TODO design
/// (every endpoint `probe:"skip"`) gen-tests to a banner-only acceptance file
/// with ZERO #[tokio::test] fns — that file still satisfies JC0551, because
/// jerrycan never demands tests the design says cannot be probed. Only a
/// module with no file at all (never gen-tested) trips; a module with zero
/// endpoints anywhere in its tree is exempt (nothing to test).
#[test]
fn jc0551_is_satisfied_by_a_banner_only_acceptance_file_and_exempts_endpointless_modules() {
    use jerrycan::platform::checkpipe::missing_acceptance_tests;
    const ALL_TODO: &str = r#"{
      "name": "webhooks-only",
      "contract_version": 0,
      "auth": { "model": "none" },
      "dependencies": [],
      "modules": [
        {
          "name": "billing",
          "entities": [{ "name": "Invoice", "fields": [{ "name": "total", "type": "string" }] }],
          "endpoints": [
            { "operation_id": "stripe_webhook", "method": "POST", "path": "/webhook",
              "probe": "skip",
              "request_body": { "entity": "Invoice" },
              "success": { "status": 201, "entity": "Invoice" } }
          ]
        },
        { "name": "docs", "endpoints": [] }
      ]
    }"#;
    let tmp = tempfile::tempdir().unwrap();
    let design: Design = serde_json::from_str(ALL_TODO).unwrap();
    let root = tmp.path().join("app");
    scaffold::scaffold(&root, &design).unwrap();

    // Never gen-tested: only `billing` (has an endpoint) trips — the
    // endpointless `docs` module is exempt.
    let ds = missing_acceptance_tests(&root, &design, None);
    assert_eq!(ds.len(), 1, "{ds:?}");
    assert!(ds[0].message.contains("`billing`"));

    // gen-tests writes a banner-only file (the skip probe leaves zero tests) —
    // and that file alone clears JC0551.
    let (_rel, expected_failing) =
        jerrycan::platform::testgen::write_acceptance(&root, &design, "billing").unwrap();
    let acceptance =
        std::fs::read_to_string(root.join("crates/routes/billing/tests/acceptance.rs")).unwrap();
    assert!(
        !acceptance.contains("#[tokio::test]") && expected_failing == 0,
        "the all-TODO design must emit a banner-only file:\n{acceptance}"
    );
    assert!(
        missing_acceptance_tests(&root, &design, None).is_empty(),
        "a banner-only acceptance file satisfies JC0551 (file existence, not test count)"
    );
}

/// #156: JC0551 covers the jobs surface. A jobs-only design (cron jobs, NO
/// endpoint-bearing modules) never tripped the per-module check, so a design
/// whose only gen-tests-eligible surface is `crates/jobs/tests/acceptance.rs`
/// read ok:true with zero tests — the #123a hollow-green hole one surface
/// over. Same signal: FILE existence (a gen-tested all-TODO jobs file
/// satisfies it), missing → JC0551 naming the jobs acceptance file.
#[test]
fn jc0551_fires_for_a_jobs_only_design_without_the_jobs_acceptance_file() {
    use jerrycan::platform::checkpipe::missing_acceptance_tests;
    const JOBS_ONLY: &str = r#"{
      "name": "cron-only",
      "contract_version": 1,
      "dependencies": ["db"],
      "jobs": [{ "name": "nightly_cleanup", "schedule": "0 3 * * *" }],
      "modules": []
    }"#;
    let tmp = tempfile::tempdir().unwrap();
    let design: Design = serde_json::from_str(JOBS_ONLY).unwrap();
    let root = tmp.path().join("app");
    std::fs::create_dir_all(&root).unwrap();

    // No jobs acceptance file on disk: the jobs surface trips JC0551 — no
    // endpoint modules exist, so without this the design reads green untested.
    let ds = missing_acceptance_tests(&root, &design, None);
    assert_eq!(ds.len(), 1, "{ds:?}");
    assert_eq!(ds[0].code, "JC0551");
    assert_eq!(
        ds[0].file.as_deref(),
        Some("crates/jobs/tests/acceptance.rs")
    );
    assert_eq!(
        ds[0].message,
        "no acceptance tests for jobs — run `jerrycan gen-tests`"
    );

    // The same writer gen-tests uses clears it — file existence is the signal.
    jerrycan::platform::jobsgen::write_jobs_acceptance(&root, &design)
        .unwrap()
        .expect("a jobs design writes the jobs acceptance file");
    assert!(
        missing_acceptance_tests(&root, &design, None).is_empty(),
        "the gen-tested jobs acceptance file satisfies JC0551"
    );
}

/// #156: a design with BOTH endpoints and jobs requires BOTH acceptance files —
/// each surface clears independently, and module scope keeps narrowing to that
/// module's package (jobs are top-level, mirroring `test_packages`, which only
/// adds the `jobs` package in workspace scope).
#[test]
fn jc0551_requires_both_module_and_jobs_acceptance_files() {
    use jerrycan::platform::checkpipe::missing_acceptance_tests;
    const ENDPOINTS_AND_JOBS: &str = r#"{
      "name": "shop-api",
      "contract_version": 1,
      "dependencies": ["db"],
      "jobs": [{ "name": "send_receipts", "schedule": "0 * * * *" }],
      "modules": [
        {
          "name": "orders",
          "entities": [{ "name": "Order", "fields": [{ "name": "total", "type": "string" }] }],
          "endpoints": [
            { "operation_id": "create_order", "method": "POST", "path": "/",
              "request_body": { "entity": "Order" },
              "success": { "status": 201, "entity": "Order" } }
          ]
        }
      ]
    }"#;
    let tmp = tempfile::tempdir().unwrap();
    let design: Design = serde_json::from_str(ENDPOINTS_AND_JOBS).unwrap();
    let root = tmp.path().join("app");
    std::fs::create_dir_all(&root).unwrap();

    // Neither file on disk: one JC0551 per surface.
    let ds = missing_acceptance_tests(&root, &design, None);
    assert_eq!(ds.len(), 2, "module AND jobs each trip JC0551: {ds:?}");
    assert!(ds.iter().all(|d| d.code == "JC0551"));
    assert!(
        ds.iter()
            .any(|d| d.file.as_deref() == Some("crates/routes/orders/tests/acceptance.rs")),
        "{ds:?}"
    );
    assert!(
        ds.iter()
            .any(|d| d.file.as_deref() == Some("crates/jobs/tests/acceptance.rs")),
        "{ds:?}"
    );

    // Module scope narrows to that module's package — the top-level jobs
    // surface is a workspace concern (mirrors test_packages).
    let scoped = missing_acceptance_tests(&root, &design, Some("orders"));
    assert_eq!(scoped.len(), 1, "{scoped:?}");
    assert_eq!(
        scoped[0].file.as_deref(),
        Some("crates/routes/orders/tests/acceptance.rs")
    );

    // gen-tests the module: the jobs surface still fires.
    jerrycan::platform::testgen::write_acceptance(&root, &design, "orders").unwrap();
    let ds = missing_acceptance_tests(&root, &design, None);
    assert_eq!(ds.len(), 1, "{ds:?}");
    assert_eq!(
        ds[0].file.as_deref(),
        Some("crates/jobs/tests/acceptance.rs")
    );

    // gen-tests the jobs surface too: green is earned on both.
    jerrycan::platform::jobsgen::write_jobs_acceptance(&root, &design)
        .unwrap()
        .expect("a jobs design writes the jobs acceptance file");
    assert!(missing_acceptance_tests(&root, &design, None).is_empty());
}
