//! Proof: a design whose User/tenant pk is a uuid migrates LOSSLESSLY — the
//! generated app keys the user identity as a stringified pk (String / TEXT), so
//! login, tenant membership, and cross-tenant isolation all work. This is the
//! fast codegen half; the runtime half (the storage acceptance battery actually
//! passing on a uuid tenant) is the `#[ignore]`d end-to-end in genroute_compile.rs.

use jerrycan::platform::design::Design;
use jerrycan::platform::{questions, scaffold, testgen};

/// A uuid-pk tenancy design: Workspace (the tenant) and Lead (tenant-owned) both
/// key on a `uuid` id, and the auth users are uuid too. This is the shape a
/// migrated Supabase project produces (auth.users + workspace_members are uuid).
const UUID_TENANCY: &str = r#"{
    "name": "uuid-crm",
    "contract_version": 2,
    "dependencies": ["auth", "db"],
    "auth": { "model": "session", "roles": ["owner", "member"] },
    "tenancy": { "entity": "Workspace", "member_roles": ["owner", "member"] },
    "modules": [
        { "name": "workspaces",
          "entities": [
              { "name": "Workspace", "fields": [
                  { "name": "id", "type": "uuid" },
                  { "name": "name", "type": "string" } ] }
          ],
          "endpoints": [
              { "operation_id": "create_workspace", "method": "POST", "path": "/",
                "auth_required": true, "request_body": { "entity": "Workspace" },
                "success": { "status": 201, "entity": "Workspace" } }
          ] },
        { "name": "leads",
          "entities": [
              { "name": "Lead",
                "belongs_to": [{ "entity": "Workspace", "on_delete": "cascade" }],
                "fields": [
                    { "name": "id", "type": "uuid" },
                    { "name": "email", "type": "string" } ] }
          ],
          "endpoints": [
              { "operation_id": "create_lead", "method": "POST", "path": "/",
                "auth_required": true, "request_body": { "entity": "Lead" },
                "success": { "status": 201, "entity": "Lead" } },
              { "operation_id": "list_leads", "method": "GET", "path": "/",
                "auth_required": true,
                "success": { "status": 200, "entity": "Lead", "list": true } },
              { "operation_id": "get_lead", "method": "GET", "path": "/{id}",
                "auth_required": true,
                "success": { "status": 200, "entity": "Lead" } },
              { "operation_id": "delete_lead", "method": "DELETE", "path": "/{id}",
                "required_roles": ["owner"],
                "success": { "status": 204 } }
          ] }
    ]
}"#;

fn uuid_design() -> Design {
    let d: Design = serde_json::from_str(UUID_TENANCY).expect("UUID_TENANCY parses");
    // Fail loud if the fixture drifts out of contract — the whole proof rests on
    // it being a valid, generatable design (same gate `jerrycan new` runs).
    assert!(
        questions::validate(&d).is_empty(),
        "uuid design must be question-free: {:?}",
        questions::validate(&d)
    );
    d
}

#[test]
fn uuid_user_and_tenant_pk_generate_a_string_identity_end_to_end() {
    let design = uuid_design();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("uuid-crm");
    scaffold::scaffold(&root, &design).unwrap();

    // 1. The app-wide session payload keys the user id as a String (stringified
    //    pk) — a uuid auth.users id round-trips through the cookie/JWT. And the
    //    tenant guard's id follows the uuid tenant pk (String), never i64.
    let shared = std::fs::read_to_string(root.join("crates/shared/src/lib.rs")).unwrap();
    assert!(
        shared.contains("pub struct SessionUser {") && shared.contains("pub id: String,"),
        "SessionUser.id is the stringified user pk:\n{shared}"
    );
    assert!(
        shared.contains("pub struct Tenant {") && shared.contains("pub async fn tenant("),
        "the membership-checked Tenant guard is generated:\n{shared}"
    );
    // The tenant id type follows the uuid Workspace pk (String), and the guard
    // binds the session user id (now a String) into the members lookup.
    assert!(
        shared.contains("pub id: String,\n    pub role: String,"),
        "Tenant.id is the uuid tenant pk (String):\n{shared}"
    );

    // 2. The membership table stores the user id as TEXT and the tenant fk as TEXT
    //    (uuid) — a bigint column could not hold a uuid user/tenant id.
    let members_ddl = std::fs::read_to_string(
        root.join("crates/routes/workspaces/migrations/sqlite/0001_create_tables.sql"),
    )
    .unwrap()
    .to_lowercase();
    assert!(
        members_ddl.contains("create table \"workspace_members\""),
        "membership table generated:\n{members_ddl}"
    );
    assert!(
        members_ddl.contains("\"user_id\" text"),
        "membership user_id is TEXT (stringified user pk):\n{members_ddl}"
    );
    assert!(
        members_ddl.contains("\"workspace_id\" text"),
        "membership tenant fk is TEXT (uuid tenant pk):\n{members_ddl}"
    );

    // The tenant-owned Lead table also carries a TEXT (uuid) tenant fk.
    let leads_ddl = std::fs::read_to_string(
        root.join("crates/routes/leads/migrations/sqlite/0001_create_tables.sql"),
    )
    .unwrap()
    .to_lowercase();
    assert!(
        leads_ddl.contains("\"workspace_id\" text"),
        "Lead's tenant fk is TEXT (uuid):\n{leads_ddl}"
    );

    // 3. The generated acceptance suite for the tenant-owned module carries the
    //    cross-tenant isolation control, mints session cookies via the stringified
    //    user id, and seeds membership rows into the TEXT-keyed members table.
    let leads = design.modules.iter().find(|m| m.name == "leads").unwrap();
    let acceptance = testgen::acceptance_rs(&design, leads);
    assert!(
        acceptance.contains("async fn tenant_a_cannot_read_tenant_b_leads()"),
        "cross-tenant isolation test generated:\n{acceptance}"
    );
    assert!(
        acceptance.contains("id: user_id.to_string()"),
        "the test cookie mints the session id as a stringified pk:\n{acceptance}"
    );
    assert!(
        acceptance.contains("INSERT INTO \\\"workspace_members\\\" (user_id, workspace_id, role)"),
        "the isolation test seeds membership rows (login + tenancy work):\n{acceptance}"
    );
    // REGRESSION (runtime): the isolation test interpolates the created id into
    // by-id URLs. A uuid pk is echoed as a JSON string, and `Value::String`'s
    // Display keeps the surrounding quotes — so a bare `format!("…/{}", &row["id"])`
    // produces `/leads/"<uuid>"` and every by-id request 404s. The id must be the
    // unquoted string.
    assert!(
        acceptance.contains("row[\"id\"].as_str().map(str::to_string)"),
        "isolation test must interpolate the UNQUOTED uuid id into by-id URLs:\n{acceptance}"
    );
    assert!(
        !acceptance.contains("let id = &row[\"id\"];"),
        "the raw `&Value` id (with JSON quotes) must never reach a URL path:\n{acceptance}"
    );

    // REGRESSION (runtime): a uuid/text pk insert must run the INSERT via
    // `Entity::insert(..).exec(..)` and return the KNOWN id. `ActiveModel::insert`
    // refetches the row after inserting, and on sqlite that refetch keys on the
    // integer rowid — for a text pk it finds nothing and fails at runtime with
    // "Failed to find inserted item" (RecordNotFound). String-only tests missed
    // this; every create on a migrated (uuid) entity 500s without the fix.
    let leads_repo = std::fs::read_to_string(root.join("crates/routes/leads/src/repo.rs")).unwrap();
    assert!(
        leads_repo.contains("Entity::insert(") && leads_repo.contains("Ok(id)"),
        "uuid-pk insert must exec + return the known id (not ActiveModel::insert's refetch):\n{leads_repo}"
    );
}
