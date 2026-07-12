//! The program capstone (spec §Eval gate): the reference export migrates into
//! an app whose `jerrycan check` is green — which runs the GENERATED isolation
//! tests (the cross-tenant negative controls for tenancy/REST, storage, and
//! realtime). Requires Postgres (JERRYCAN_TEST_PG_URL) and the jerrycan binary;
//! never skipped in the eval/pre-publish pipelines, `#[ignore]`d elsewhere.
use std::process::Command;

fn jerrycan() -> Command {
    Command::new(env!("CARGO_BIN_EXE_jerrycan"))
}

#[test]
#[ignore = "capstone eval: needs postgres (JERRYCAN_TEST_PG_URL) — run with --ignored in the eval job"]
fn migrated_reference_app_checks_green_with_negative_controls() {
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

    // 3. The gate: `jerrycan check` must be green. This compiles + tests the
    //    generated app, which includes the GENERATED cross-tenant negative
    //    controls — tenancy (REST 404 across workspaces), storage (a foreign
    //    tenant's object is not readable), and realtime (a scope-filtered change
    //    never arrives). A wrong translation cannot pass this gate.
    let out = jerrycan()
        .args(["check"])
        .current_dir(&app)
        .env("JERRYCAN_DATABASE_URL", &pg)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "check must be green (incl. cross-tenant negative controls): {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
