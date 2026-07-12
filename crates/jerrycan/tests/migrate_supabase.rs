//! The reference Supabase export migrates deterministically and safely.
use jerrycan::platform::migrate::{MigrateOptions, run_migrate};
use jerrycan::platform::questions;

fn fixture() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../conformance/fixtures/supabase-export")
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
    assert_eq!(
        out.design.jobs.len(),
        1,
        "5-field cron mapped; @hourly gapped"
    );
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
fn uuid_membership_rows_seed_losslessly_and_are_not_gapped() {
    // The reference auth.users are uuid, and workspace_members carries uuid
    // user_ids. The migration must SEED those membership rows into the generated
    // `workspace_members` table (so migrated users keep their tenancy — login +
    // membership work) rather than emitting the old blocking "not auto-seeded" gap.
    use jerrycan::platform::migrate::gaps::{GapKind, Severity};
    let tmp = tempfile::tempdir().unwrap();
    let out = migrate_into(tmp.path());

    // No blocking seed gap for the membership table.
    assert!(
        !out.gaps.iter().any(|g| g.kind == GapKind::SeedData
            && g.source.contains("workspace_members")
            && g.severity == Severity::Blocking),
        "membership rows must be seeded, not gapped: {:?}",
        out.gaps.iter().map(|g| &g.source).collect::<Vec<_>>()
    );

    // The seed manifest lists the members table with all 3 exported rows.
    let manifest = std::fs::read_to_string(tmp.path().join("seed/manifest.json")).unwrap();
    let m: serde_json::Value = serde_json::from_str(&manifest).unwrap();
    let members = m["tables"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["table"] == "workspace_members")
        .expect("workspace_members is seeded");
    assert_eq!(members["rows"], 3, "all three membership rows seeded");

    // The inline seed maps the columns to the generated table and carries every
    // uuid user id verbatim (lossless) — the FK workspace id and role too.
    let sql = std::fs::read_dir(tmp.path().join("seed/inline"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with("workspace_members.sql"))
        })
        .map(|p| std::fs::read_to_string(p).unwrap())
        .expect("a workspace_members inline seed file exists");
    assert!(
        sql.contains("INSERT INTO workspace_members (user_id, workspace_id, role) VALUES"),
        "columns mapped to the generated members table: {sql}"
    );
    for uid in [
        "11111111-1111-1111-1111-111111111111",
        "22222222-2222-2222-2222-222222222222",
        "33333333-3333-3333-3333-333333333333",
    ] {
        assert!(
            sql.contains(&format!("'{uid}'")),
            "uuid user id {uid} seeded verbatim: {sql}"
        );
    }
    assert!(
        sql.contains("'aaaaaaaa-0000-0000-0000-000000000001'") && sql.contains("'member'"),
        "workspace fk + role seeded: {sql}"
    );
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
