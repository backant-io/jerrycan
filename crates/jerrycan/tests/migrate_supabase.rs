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

/// Regression for issue #29: a Supabase-migrated app declares `auth.model = jwt`
/// (authmap forces it — Supabase auth IS JWT), so its scaffolded `shared` crate
/// MUST alias `CurrentUser` to the `Bearer<SessionUser>` guard. Before #29 the
/// migrated app silently got the cookie `Session` guard, so REST routes rejected
/// a real Bearer token. This pins the migrated tree ON DISK to the Bearer alias.
#[test]
fn migrated_jwt_app_scaffolds_the_bearer_guard_alias() {
    use jerrycan::platform::design::AuthModel;
    let tmp = tempfile::tempdir().unwrap();
    let out = migrate_into(tmp.path());
    assert_eq!(
        out.design.auth_model(),
        AuthModel::Jwt,
        "Supabase auth is JWT"
    );
    let shared = std::fs::read_to_string(tmp.path().join("crates/shared/src/lib.rs")).unwrap();
    assert!(
        shared.contains("pub type CurrentUser = jerrycan::auth::Bearer<SessionUser>;"),
        "migrated jwt app must emit the Bearer guard alias, not cookies: {shared}"
    );
    assert!(
        !shared.contains("jerrycan::auth::Session<SessionUser>"),
        "migrated jwt app must NOT emit the cookie Session guard: {shared}"
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

/// Regression for issue #303: a native `CREATE TYPE … AS ENUM` column (referenced
/// unqualified, as Supabase dumps and hand-written schemas do) must translate to a
/// `values`-constrained field — the same shape an inline `CHECK IN` produces — so
/// the column survives as a real field AND its seed data is carried (before #303 the
/// column dropped as "no deterministic design type", leaving a hollow id-only entity
/// and silently dropping the seed column).
#[test]
fn native_enum_columns_translate_to_field_values_and_keep_their_seed() {
    let src = tempfile::tempdir().unwrap();
    let root = src.path();
    std::fs::write(
        root.join("schema.sql"),
        "create type status as enum ('active', 'inactive');\n\
         create table public.tasks (\n\
             id uuid primary key,\n\
             title text not null,\n\
             status status not null\n\
         );\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("data")).unwrap();
    std::fs::write(
        root.join("data/public.tasks.csv"),
        "id,title,status\naaaaaaaa-0000-0000-0000-000000000001,ship it,active\n",
    )
    .unwrap();

    let out_tmp = tempfile::tempdir().unwrap();
    let out = run_migrate(&MigrateOptions {
        export_dir: root.to_path_buf(),
        out_dir: out_tmp.path().to_path_buf(),
        name: Some("tasks-app".into()),
        bulk_threshold: 100,
    })
    .expect("minimal enum export migrates");

    // (a) The design has a real `status` field constrained to the enum labels —
    //     not a dropped column / hollow id-only entity.
    let entity = out
        .design
        .modules
        .iter()
        .flat_map(|m| &m.entities)
        .find(|e| e.name == "Task")
        .expect("tasks table modeled as an entity");
    let status = entity
        .fields
        .iter()
        .find(|f| f.name == "status")
        .expect("native enum column survives as a field");
    assert_eq!(
        status.values.as_deref(),
        Some(&["active".to_string(), "inactive".to_string()][..]),
        "enum labels become field values"
    );
    assert!(
        !out.gaps.iter().any(|g| g.source.contains("tasks.status")),
        "the enum column must not gap: {:?}",
        out.gaps.iter().map(|g| &g.source).collect::<Vec<_>>()
    );

    // (b) The emitted seed still carries the `status` column and its value.
    let sql = std::fs::read_dir(out_tmp.path().join("seed/inline"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with("tasks.sql"))
        })
        .map(|p| std::fs::read_to_string(p).unwrap())
        .expect("a tasks inline seed file exists");
    assert!(
        sql.contains("status") && sql.contains("'active'"),
        "seed carries the enum column and its value: {sql}"
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
