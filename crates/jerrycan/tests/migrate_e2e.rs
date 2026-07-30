//! The program capstone (spec §Eval gate): the reference export migrates into an
//! app that compiles, applies + seeds against a real database, and whose
//! `jerrycan check` reports the HONEST verdict — buildable, with the generated
//! isolation tests (cross-tenant negative controls for tenancy/REST, storage, and
//! realtime) compiled in, and RED only on the documented cron-task stub the
//! migrator leaves as agent work (#152). A dishonest "green right after migrate"
//! is what this capstone deliberately does NOT assert — a stub-handled migration
//! cannot honestly reach green. Requires Postgres (JERRYCAN_TEST_PG_URL) and the
//! jerrycan binary; never skipped in the eval/pre-publish pipelines, `#[ignore]`d
//! elsewhere.
use std::process::Command;

fn jerrycan() -> Command {
    Command::new(env!("CARGO_BIN_EXE_jerrycan"))
}

#[test]
#[ignore = "capstone eval: needs postgres (JERRYCAN_TEST_PG_URL) — run with --ignored in the eval job"]
fn migrated_reference_app_checks_honest_red_only_on_the_cron_stub() {
    let pg = std::env::var("JERRYCAN_TEST_PG_URL").expect("JERRYCAN_TEST_PG_URL");
    let tmp = tempfile::tempdir().unwrap();
    let app = tmp.path().join("acme-crm");
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../conformance/fixtures/supabase-export");

    // 1. Migrate (offline, deterministic).
    let out = jerrycan()
        .args(["migrate", "--from", "supabase"])
        .arg(&fixture)
        .arg("--out")
        .arg(&app)
        .args(["--name", "acme-crm", "--bulk-threshold", "100"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "migrate failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // 2. Apply migrations + seed against a real database.
    for step in [vec!["db", "migrate"], vec!["db", "seed"]] {
        let out = jerrycan()
            .args(&step)
            .current_dir(&app)
            .env("JERRYCAN_DATABASE_URL", &pg)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{step:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // 3. The gate — assert the HONEST verdict, not a dishonest green (#152). A
    //    migrated app is a SCAFFOLD with documented gaps: the cron task bodies are
    //    agent work (a `pg_function` gap — `migrate::cronmap`), so `nightly_digest`'s
    //    generated jobs acceptance test fails until the agent implements the stub.
    //    So `jerrycan check` is honestly RED — but ONLY at the tests class, and its
    //    ONLY diagnostic is that one expected cron-stub test. Every EARLIER class
    //    (compile/clippy/audit/deny + `db migrate`/`db seed` above) is green, which
    //    proves the translation is buildable and correct: `check` reaches the tests
    //    class at all only because migration + compilation succeeded, and the
    //    generated cross-tenant negative controls (tenancy REST-404, storage,
    //    realtime scope filtering) compiled into the suite. A WRONG translation
    //    would fail an earlier class or add an UNEXPECTED diagnostic here, so this
    //    still catches it — without asserting a green that a stub-handled migration
    //    can never honestly reach.
    //    (`hourly-sync`'s `@hourly` is a non-5-field `cron_job` gap the migrator
    //    never emits as a job, so `nightly_digest` is the sole stub-task test.)
    let out = jerrycan()
        .args(["--json", "check"])
        .current_dir(&app)
        .env("JERRYCAN_DATABASE_URL", &pg)
        .output()
        .unwrap();
    let payload: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("`--json check` emits a JSON document");
    assert_eq!(
        payload["ok"], false,
        "a freshly-migrated app has a stub cron task, so `check` is honestly RED — a green here would be the #152 dishonest capstone: {payload}"
    );
    let diags = payload["diagnostics"]
        .as_array()
        .expect("a red check reports a diagnostics array");
    assert_eq!(
        diags.len(),
        1,
        "the ONLY expected red is the single cron-stub jobs test; an extra diagnostic means a translation regression (an unexpected class went red): {payload}"
    );
    assert_eq!(
        diags[0]["code"], "TEST0001",
        "the expected red is a tests-class failure (TEST0001) — NOT a compile/clippy/audit/deny error, which would mean a broken translation: {payload}"
    );
    let msg = diags[0]["message"].as_str().unwrap_or("");
    assert!(
        msg.contains("nightly_digest"),
        "the expected red is the `nightly_digest` cron-task stub (agent work), got: {msg}"
    );
}
