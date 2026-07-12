//! The reference Supabase export migrates deterministically and safely.
use jerrycan::platform::migrate::{MigrateOptions, run_migrate};
use jerrycan::platform::questions;

fn fixture() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance/fixtures/supabase-export")
}

fn migrate_into(dir: &std::path::Path) -> jerrycan::platform::migrate::MigrateOutput {
    run_migrate(&MigrateOptions {
        export_dir: fixture(),
        out_dir: dir.to_path_buf(),
        name: Some("acme-crm".into()),
        bulk_threshold: 100,
    })
    .expect("reference export migrates")
}

#[test]
fn the_reference_export_yields_a_green_v2_design() {
    let tmp = tempfile::tempdir().unwrap();
    let out = migrate_into(tmp.path());
    assert!(
        questions::validate(&out.design).is_empty(),
        "{:?}",
        questions::validate(&out.design)
    );
    assert_eq!(out.design.tenancy.as_ref().unwrap().entity, "Workspace");
    assert!(out.design.storage.is_some() && out.design.realtime.is_some());
    assert_eq!(out.design.jobs.len(), 1, "5-field cron mapped; @hourly gapped");
}

#[test]
fn expected_gaps_exactly_no_more_no_less_by_kind() {
    use jerrycan::platform::migrate::gaps::GapKind::*;
    let tmp = tempfile::tempdir().unwrap();
    let out = migrate_into(tmp.path());
    let mut kinds: Vec<_> = out.gaps.iter().map(|g| g.kind).collect();
    kinds.sort();
    // share-policy → RlsPolicy; plpgsql fn + trigger + cron body → PgFunction×2, PgTrigger;
    // edge fn → EdgeFunction; @hourly → CronJob; planted JWT in data → SuspectedSecret;
    // Broadcast + Presence advisories.
    for want in [
        RlsPolicy,
        PgFunction,
        PgTrigger,
        EdgeFunction,
        CronJob,
        SuspectedSecret,
        Broadcast,
        Presence,
    ] {
        assert!(kinds.contains(&want), "missing {want:?}: {kinds:?}");
    }
}

#[test]
fn no_secret_survives_into_any_emitted_artifact() {
    let tmp = tempfile::tempdir().unwrap();
    migrate_into(tmp.path());
    for rel in ["design.json", "MIGRATION.md", "gap-report.json"] {
        let text = std::fs::read_to_string(tmp.path().join(rel)).unwrap();
        assert!(
            jerrycan::platform::migrate::redact::scan(&text).is_empty(),
            "{rel} leaked a secret"
        );
    }
    let md = std::fs::read_to_string(tmp.path().join("MIGRATION.md")).unwrap();
    assert!(md.contains("Secret rotation"), "rotation checklist present");
}

#[test]
fn the_bulk_table_took_the_resumable_path() {
    let tmp = tempfile::tempdir().unwrap();
    migrate_into(tmp.path());
    assert!(
        tmp.path().join("seed/bulk/events.csv").exists(),
        "300 rows > threshold 100"
    );
    let manifest = std::fs::read_to_string(tmp.path().join("seed/manifest.json")).unwrap();
    assert!(manifest.contains("\"mode\": \"bulk\""));
}
