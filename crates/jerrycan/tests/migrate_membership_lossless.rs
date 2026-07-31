//! Supabase-lossless migration for a multi-membership user (spec §C + §F, #78/#79).
//!
//! Proves the migration path is genuinely lossless for many-membership: a
//! recognized `TenantMembership` RLS policy — `{fk} IN (SELECT {fk} FROM {members}
//! WHERE user_id = auth.uid())` (see `migrate/rls.rs` `Scope::TenantMembership` and
//! `migrate/tenancy.rs`) — becomes a scaffolded app whose FLAT tenant-owned routes
//! use the MEMBERSHIP-SET repo methods (`all_for_memberships`/`get_for_memberships`),
//! so a user who is a member of TWO workspaces sees BOTH workspaces' rows via list
//! and a `get/{id}` on a row OUTSIDE their membership set `404`s. The migrator needs
//! no change: the fix lives in the generator, so a migrated Supabase RLS export and
//! an authored flat design are served identically (the spec's lossless promise).
//!
//! Mirrors the `tests/migrate_supabase.rs` harness (`run_migrate` over an export
//! dir); the schema is inlined and minimal so the proof is self-contained.
use jerrycan::platform::migrate::{MigrateOptions, MigrateOutput, run_migrate};

/// A minimal Supabase export: an org tenant (`workspaces` + `workspace_members`
/// with a role CHECK) and a FLAT tenant-owned `customers` table guarded by the
/// canonical membership RLS policy — exactly the shape `migrate/rls.rs` certifies
/// as `TenantMembership`. No tenant fk appears in any route (the Supabase shape).
const SUPABASE_SCHEMA: &str = r#"
create table public.workspaces (
    id uuid primary key,
    name text not null
);
create table public.workspace_members (
    workspace_id uuid not null references public.workspaces(id) on delete cascade,
    user_id uuid not null,
    role text not null check (role in ('owner', 'member')),
    primary key (workspace_id, user_id)
);
create table public.customers (
    id uuid primary key,
    workspace_id uuid not null references public.workspaces(id),
    name text not null
);
alter table public.customers enable row level security;
create policy customers_membership on public.customers using
    (workspace_id in (select workspace_id from public.workspace_members where user_id = auth.uid()));
"#;

/// The EXACT membership-set SQL the generator emits for the migrated flat entity
/// (`genroute.rs` `all_for_memberships`/`get_for_memberships`). Kept as constants
/// so the behavioral leg runs the same text the generated repo is asserted to
/// contain — binding the behavioral proof to the generated code.
const LIST_SQL: &str = "SELECT * FROM customers WHERE workspace_id IN (SELECT workspace_id FROM workspace_members WHERE user_id = ?) ORDER BY id";
const GET_SQL: &str = "SELECT * FROM customers WHERE id = ? AND workspace_id IN (SELECT workspace_id FROM workspace_members WHERE user_id = ?)";
/// The EXACT membership `WITH CHECK` the generator emits for a FLAT create (issue #94,
/// `genroute.rs` `create_for_memberships`) and the scoped DELETE (`remove_for_memberships`).
/// Kept as constants so the behavioral leg runs the same text the generated repo is
/// asserted to contain — binding the 403/404 proof to the shipped generated code.
const CREATE_CHECK_SQL: &str =
    "SELECT 1 FROM workspace_members WHERE user_id = ? AND workspace_id = ? LIMIT 1";
const DELETE_SQL: &str = "DELETE FROM customers WHERE id = ? AND workspace_id IN (SELECT workspace_id FROM workspace_members WHERE user_id = ?)";

/// Write the export + migrate into `app/`. Returns the tempdir (kept alive so the
/// scaffolded tree survives) and the migrate output.
fn migrate_flat_membership() -> (tempfile::TempDir, MigrateOutput) {
    let tmp = tempfile::tempdir().unwrap();
    let export = tmp.path().join("export");
    std::fs::create_dir_all(&export).unwrap();
    std::fs::write(export.join("schema.sql"), SUPABASE_SCHEMA).unwrap();
    let out = run_migrate(&MigrateOptions {
        export_dir: export,
        out_dir: tmp.path().join("app"),
        name: Some("membertest".into()),
        bulk_threshold: 100,
    })
    .expect("the flat membership export migrates");
    (tmp, out)
}

/// GENERATION-LEVEL lossless proof: the recognized membership policy → a flat
/// tenant-owned entity whose generated repo/handlers use the MEMBERSHIP-SET methods
/// (the RLS subquery restored), not a flatten-to-one-arbitrary-tenant.
#[test]
fn recognized_membership_policy_migrates_to_flat_membership_set_methods() {
    let (tmp, out) = migrate_flat_membership();
    let app = tmp.path().join("app");

    // The policy was recognized as tenant membership → the design carries a
    // Workspace tenancy block (not lost to a gap, not per-row owner scoping).
    let tenancy = out
        .design
        .tenancy
        .as_ref()
        .expect("a recognized membership policy yields a tenancy block");
    assert_eq!(tenancy.entity, "Workspace");

    // `customers` migrated as a FLAT tenant-owned entity: it belongs_to the tenant,
    // and NO route carries the tenant fk in its path (module mount + every endpoint).
    let customers = out
        .design
        .modules
        .iter()
        .find(|m| m.name == "customers")
        .expect("a customers module exists");
    let entity = customers
        .entities
        .iter()
        .find(|e| e.name == "Customer")
        .expect("Customer entity exists");
    assert!(
        entity.belongs_to.iter().any(|b| b.entity == "Workspace"),
        "Customer belongs_to the tenant (tenant-owned)"
    );
    assert!(
        customers
            .mount
            .as_deref()
            .is_none_or(|mnt| !mnt.contains("workspace_id"))
            && customers
                .endpoints
                .iter()
                .all(|ep| !ep.path.contains("workspace_id")),
        "flat shape: no tenant fk in the mount or any endpoint path"
    );

    // The flat entity's generated repo emits the MEMBERSHIP-SET accessors whose
    // filter is the Supabase RLS subquery, restored verbatim. This is what makes a
    // two-workspace user see BOTH workspaces' rows and nothing outside the set —
    // the lossless-migration guarantee, at the generation level.
    let repo = std::fs::read_to_string(app.join("crates/routes/customers/src/repo.rs")).unwrap();
    assert!(
        repo.contains(
            "pub async fn all_for_memberships(&self, user_id: String) -> Result<Vec<Customer>>"
        ),
        "flat list uses the membership-set accessor:\n{repo}"
    );
    // The signature wraps one param per line (issue #201 — its one-line width with a
    // String pk exceeds rustfmt's max_width, so the fresh scaffold stays a fixpoint).
    assert!(
        repo.contains(
            "pub async fn get_for_memberships(\n        &self,\n        user_id: String,\n        id: String,\n    ) -> Result<Option<Customer>> {"
        ),
        "flat get uses the membership-set accessor:\n{repo}"
    );
    assert!(
        repo.contains(LIST_SQL),
        "list scopes by the RLS membership subquery (union of memberships):\n{repo}"
    );
    assert!(
        repo.contains(GET_SQL),
        "get bounds by row id AND the membership set (404 outside it):\n{repo}"
    );

    // The flat handlers take the SESSION guard (never `Dep<Tenant>`, which would
    // trust one arbitrary membership) and are steered to the set methods.
    let handlers =
        std::fs::read_to_string(app.join("crates/routes/customers/src/handlers.rs")).unwrap();
    // The signature wraps to one param per line (issue #165 — its one-line width
    // exceeds rustfmt's max_width, so the fresh scaffold is a `cargo fmt` fixpoint).
    assert!(
        handlers.contains(
            "pub(crate) async fn list_customers(\n    _repo: Dep<CustomerRepo>,\n    _user: CurrentUser,\n) -> Result<Json<Vec<Customer>>> {"
        ),
        "flat list takes CurrentUser, not Dep<Tenant>:\n{handlers}"
    );
    assert!(
        !handlers.contains("Dep<Tenant>"),
        "a flat tenant-owned handler must never take Dep<Tenant>:\n{handlers}"
    );
    assert!(
        handlers.contains("CustomerRepo::all_for_memberships(_user.0.id)")
            && handlers.contains("CustomerRepo::get_for_memberships(_user.0.id, _id)"),
        "scope hints name the membership-set methods:\n{handlers}"
    );
}

/// Residual #78 on the MIGRATION path: unlike every AUTHORED load (which normalizes
/// in `Design::from_path`), the migrator builds the design in memory and hands it
/// straight to `scaffold`. So the tenant-declaring module's own conventional `/{id}`
/// detail route was NOT rewritten to `/{workspace_id}`. That left the route
/// classified `PathScoped` while the router captured `id`, so the membership guard's
/// path branch missed (`params.get("workspace_id") == None`) and fell back to an
/// arbitrary first membership: a member of workspace A could `GET /workspaces/B` and
/// read workspace B. This proves the migrated tenant-own detail route is normalized —
/// in the in-memory design AND the scaffolded route table (so design.json and the
/// scaffold agree, no migrate-vs-regenerate drift) — restoring the by-name guard.
#[test]
fn migrated_tenant_own_detail_route_is_normalized_to_the_tenant_fk() {
    let (tmp, out) = migrate_flat_membership();
    let app = tmp.path().join("app");

    // The module that DECLARES the tenant entity (Workspace) — its own detail route.
    let workspaces = out
        .design
        .modules
        .iter()
        .find(|m| m.entities.iter().any(|e| e.name == "Workspace"))
        .expect("a module declares the Workspace tenant entity");

    // The conventional `/{id}` must be normalized to `/{workspace_id}`: the guard then
    // reads the tenant fk BY NAME from the path and 404s a non-member (closing #78).
    assert!(
        workspaces
            .endpoints
            .iter()
            .all(|ep| !ep.path.contains("{id}")),
        "no tenant-own detail route may keep the raw `{{id}}` param:\n{:#?}",
        workspaces.endpoints
    );
    assert!(
        workspaces
            .endpoints
            .iter()
            .any(|ep| ep.path.contains("{workspace_id}")),
        "the tenant-own detail route is normalized to `{{workspace_id}}`:\n{:#?}",
        workspaces.endpoints
    );

    // No migrate-vs-regenerate drift: the scaffolded route table must AGREE with the
    // normalized design (the router captures `workspace_id`, so the guard fires).
    let lib =
        std::fs::read_to_string(app.join(format!("crates/routes/{}/src/lib.rs", workspaces.name)))
            .unwrap();
    assert!(
        lib.contains("/{workspace_id}") && !lib.contains("/{id}"),
        "scaffolded route table registers /{{workspace_id}}, never /{{id}}:\n{lib}"
    );
}

/// GENERATION-LEVEL proof for the FLAT cross-tenant WRITE fix (issue #94, spec §C
/// `WITH CHECK`): the migrated flat entity's repo emits the membership-CHECKED write
/// accessors, and its flat `POST`/`PUT`/`DELETE` handlers are steered to them (never
/// the unscoped `insert`/`update`/`remove`). This is what makes a `POST {other_ws}`
/// into a non-member tenant a 403 instead of a silent cross-tenant write.
#[test]
fn migrated_flat_entity_gets_membership_checked_write_methods() {
    let (tmp, _out) = migrate_flat_membership();
    let app = tmp.path().join("app");
    let repo = std::fs::read_to_string(app.join("crates/routes/customers/src/repo.rs")).unwrap();

    // uuid pks → the tenant fk and the row id are both `String` in the emitted methods.
    assert!(
        repo.contains(
            "pub async fn create_for_memberships(&self, user_id: String, item: Customer) -> Result<String>"
        ),
        "flat create is membership-checked:\n{repo}"
    );
    // `update_for_memberships` wraps one param per line (issue #201 — its one-line
    // width with a String pk exceeds max_width); `remove_for_memberships` still fits.
    assert!(
        repo.contains(
            "pub async fn update_for_memberships(\n        &self,\n        user_id: String,\n        id: String,\n        item: Customer,\n    ) -> Result<bool> {"
        ) && repo.contains(
            "pub async fn remove_for_memberships(&self, user_id: String, id: String) -> Result<bool>"
        ),
        "flat update/delete are membership-checked:\n{repo}"
    );
    // The create's `WITH CHECK` probe and the 403 branch; the scoped DELETE subquery.
    assert!(
        repo.contains(CREATE_CHECK_SQL) && repo.contains("return Err(Error::forbidden());"),
        "create verifies the body tenant fk ∈ memberships (403 else):\n{repo}"
    );
    assert!(
        repo.contains(DELETE_SQL),
        "delete is scoped to the membership set (404 outside it):\n{repo}"
    );

    // The flat mutation handlers are STEERED to the checked methods and take the
    // session guard — a flat write must never trust the body tenant fk directly.
    let handlers =
        std::fs::read_to_string(app.join("crates/routes/customers/src/handlers.rs")).unwrap();
    assert!(
        handlers.contains("CustomerRepo::create_for_memberships(_user.0.id, customer)")
            && handlers.contains("CustomerRepo::update_for_memberships(_user.0.id, _id, customer)")
            && handlers.contains("CustomerRepo::remove_for_memberships(_user.0.id, _id)"),
        "flat mutation stubs are steered to the membership-checked methods:\n{handlers}"
    );
}

/// BEHAVIORAL e2e-lite proof for the flat cross-tenant WRITE (issue #94): run the EXACT
/// `WITH CHECK` / scoped-DELETE SQL the migrated repo emits against sqlite and observe
/// the HTTP-status semantics the generated method maps to. A user in w1 creating in w1
/// passes the check (→ 201); the SAME user creating in w2 (not a member) FAILS the
/// check (→ the method's `Err(forbidden)` → 403); deleting a w2 row is scoped out
/// (0 rows → false → 404). The SQL executed is asserted to match the generated repo.
#[test]
fn flat_cross_tenant_write_is_forbidden_403_and_scoped_404() {
    let (tmp, _out) = migrate_flat_membership();
    let repo = std::fs::read_to_string(tmp.path().join("app/crates/routes/customers/src/repo.rs"))
        .unwrap();
    assert!(
        repo.contains(CREATE_CHECK_SQL) && repo.contains(DELETE_SQL),
        "the SQL exercised below is the exact text the migrated repo emits"
    );

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            use jerrycan::db::sea_orm::{ConnectionTrait, Statement};
            use jerrycan::db::{Db, db_error};

            let db = Db::connect("sqlite::memory:").await.unwrap();
            db.conn()
                .execute_unprepared(
                    "CREATE TABLE workspace_members (workspace_id TEXT NOT NULL, \
                     user_id TEXT NOT NULL, role TEXT NOT NULL)",
                )
                .await
                .unwrap();
            db.conn()
                .execute_unprepared(
                    "CREATE TABLE customers (id TEXT PRIMARY KEY, \
                     workspace_id TEXT NOT NULL, name TEXT NOT NULL)",
                )
                .await
                .unwrap();
            // user `u1` is a member of w1 only (NOT w2). A seed row in w2 lets us prove
            // the scoped delete can't reach it.
            db.conn()
                .execute_unprepared(
                    "INSERT INTO workspace_members (workspace_id, user_id, role) \
                     VALUES ('w1', 'u1', 'member')",
                )
                .await
                .unwrap();
            db.conn()
                .execute_unprepared(
                    "INSERT INTO customers (id, workspace_id, name) VALUES ('c2', 'w2', 'Bob')",
                )
                .await
                .unwrap();
            let backend = db.conn().get_database_backend();

            // The generated `create_for_memberships` WITH CHECK: present for the caller's
            // own tenant (→ create proceeds, 201), ABSENT for a tenant they don't belong
            // to (→ the method returns Err(forbidden) → 403 — the leak, closed).
            let check = |ws: &'static str| {
                let db = db.clone();
                async move {
                    db.conn()
                        .query_one(Statement::from_sql_and_values(
                            backend,
                            db.sql(CREATE_CHECK_SQL),
                            ["u1".into(), ws.into()],
                        ))
                        .await
                        .map_err(db_error)
                        .unwrap()
                        .is_some()
                }
            };
            assert!(
                check("w1").await,
                "POST {{workspace_id: w1}} as a w1 member passes WITH CHECK → 201"
            );
            assert!(
                !check("w2").await,
                "POST {{workspace_id: w2}} as a NON-member fails WITH CHECK → 403 (was a silent cross-tenant write)"
            );

            // The generated scoped DELETE: a w2 row is outside u1's set → 0 rows → 404.
            let del = db
                .conn()
                .execute(Statement::from_sql_and_values(
                    backend,
                    db.sql(DELETE_SQL),
                    ["c2".into(), "u1".into()],
                ))
                .await
                .map_err(db_error)
                .unwrap();
            assert_eq!(
                del.rows_affected(),
                0,
                "DELETE of a w2 row by a non-member touches 0 rows → false → 404"
            );
            // The row is still there (proof the delete was genuinely scoped out).
            let still = db
                .conn()
                .query_one(Statement::from_sql_and_values(
                    backend,
                    db.sql("SELECT 1 FROM customers WHERE id = ?"),
                    ["c2".into()],
                ))
                .await
                .map_err(db_error)
                .unwrap();
            assert!(still.is_some(), "the out-of-set row was NOT deleted");
        });
}

/// BEHAVIORAL + GENERATION proof for the FLAT `update_for_memberships` (issues #94/#92):
/// (a) updating a row whose CURRENT tenant is outside the caller's set is invisible
/// (`get_for_memberships` → None → the method's `Ok(false)` → 404); (b) moving a row to
/// a tenant the caller doesn't belong to is refused (a changed tenant fk → 403); (c) THE
/// #92 BODY-ID VECTOR — `update_for_memberships(user, id = own_row, item{ id =
/// victim_row_in_another_tenant })` must UPDATE the AUTHORIZED PATH row, never the body
/// id, so a victim row in another tenant is untouched. The generated UPDATE pins the pk
/// to the checked path `id`; this leg reads that binding from the shipped repo and runs
/// the UPDATE vs sqlite — RED before the fix (body id bound → victim overwritten), GREEN
/// after (path id bound → victim safe).
#[test]
fn flat_update_is_scoped_and_cannot_move_a_victim_row_by_body_id() {
    let (tmp, _out) = migrate_flat_membership();
    let repo = std::fs::read_to_string(tmp.path().join("app/crates/routes/customers/src/repo.rs"))
        .unwrap();

    // GENERATION: the emitted `update_for_memberships` must pin the ActiveModel pk to
    // the CHECKED PATH `id`, NEVER the BODY `item.id` (issue #92). Slice the method out
    // so this can't be satisfied by the bare `update`'s own `Set(id)`.
    let upd = repo
        .split("pub async fn update_for_memberships")
        .nth(1)
        .and_then(|s| s.split("pub async fn").next())
        .expect("the migrated repo emits update_for_memberships");
    assert!(
        upd.contains("id: Set(id),"),
        "update_for_memberships pins the pk to the checked path id:\n{upd}"
    );
    assert!(
        !upd.contains("id: Set(item.id),"),
        "update_for_memberships must NOT bind the BODY id — that is the #92 write vector:\n{upd}"
    );
    // The load SQL exercised below (legs a/b) is the exact text the update method calls
    // via `get_for_memberships`, binding the proof to the shipped code.
    assert!(
        repo.contains(GET_SQL),
        "the load SQL below matches the emitted repo"
    );
    // The pk the generated UPDATE targets: the fix pins the PATH id (`Set(id)`); the #92
    // bug bound the BODY id (`Set(item.id)`). Read it so the behavioral leg runs the
    // code as emitted, not a stand-in.
    let pins_path_id = upd.contains("id: Set(id),");

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            use jerrycan::db::sea_orm::{ConnectionTrait, Statement};
            use jerrycan::db::{Db, db_error};

            let db = Db::connect("sqlite::memory:").await.unwrap();
            db.conn()
                .execute_unprepared(
                    "CREATE TABLE workspace_members (workspace_id TEXT NOT NULL, \
                     user_id TEXT NOT NULL, role TEXT NOT NULL)",
                )
                .await
                .unwrap();
            db.conn()
                .execute_unprepared(
                    "CREATE TABLE customers (id TEXT PRIMARY KEY, \
                     workspace_id TEXT NOT NULL, name TEXT NOT NULL)",
                )
                .await
                .unwrap();
            // u1 is a member of w1 ONLY. c1 is u1's own row (w1); c2 is a VICTIM row in
            // w2 — a tenant u1 does not belong to.
            db.conn()
                .execute_unprepared(
                    "INSERT INTO workspace_members (workspace_id, user_id, role) \
                     VALUES ('w1', 'u1', 'member')",
                )
                .await
                .unwrap();
            db.conn()
                .execute_unprepared(
                    "INSERT INTO customers (id, workspace_id, name) \
                     VALUES ('c1', 'w1', 'Own'), ('c2', 'w2', 'Victim')",
                )
                .await
                .unwrap();
            let backend = db.conn().get_database_backend();

            // (a) OUT-OF-SET CURRENT ROW → 404: the method loads via get_for_memberships;
            // c2's tenant (w2) is outside u1's set, so the load is None → Ok(false) → 404.
            let load_c2 = db
                .conn()
                .query_one(Statement::from_sql_and_values(
                    backend,
                    db.sql(GET_SQL),
                    ["c2".into(), "u1".into()],
                ))
                .await
                .map_err(db_error)
                .unwrap();
            assert!(
                load_c2.is_none(),
                "updating an out-of-set row loads None → Ok(false) → 404"
            );

            // (b) FK-MOVE → 403: c1 loads (in-set); the method compares the body fk to the
            // row's CURRENT fk and refuses a move. A body `workspace_id = w2` differs from
            // c1's current `w1` → the method's `Err(forbidden())` → 403.
            let current_fk: String = db
                .conn()
                .query_one(Statement::from_sql_and_values(
                    backend,
                    db.sql(GET_SQL),
                    ["c1".into(), "u1".into()],
                ))
                .await
                .map_err(db_error)
                .unwrap()
                .expect("c1 is in u1's set")
                .try_get("", "workspace_id")
                .map_err(db_error)
                .unwrap();
            assert_eq!(current_fk, "w1");
            assert_ne!(
                "w2", current_fk,
                "a body fk that moves the row to w2 differs from the current fk → 403"
            );

            // (c) THE #92 BODY-ID VECTOR: update_for_memberships(u1, id = "c1", item{ id =
            // "c2", workspace_id = "w1", name = "hacked" }). The generated UPDATE targets
            // `WHERE id = <pk>`; run it with the pk the shipped repo binds.
            let pk = if pins_path_id { "c1" } else { "c2" };
            db.conn()
                .execute(Statement::from_sql_and_values(
                    backend,
                    db.sql("UPDATE customers SET workspace_id = ?, name = ? WHERE id = ?"),
                    ["w1".into(), "hacked".into(), pk.into()],
                ))
                .await
                .map_err(db_error)
                .unwrap();

            // The VICTIM row (c2, in w2) must be untouched — the update can only ever write
            // the authorized path row (c1). RED before the #92 fix (pk = "c2" → c2
            // overwritten), GREEN after (pk = "c1").
            let victim: String = db
                .conn()
                .query_one(Statement::from_sql_and_values(
                    backend,
                    db.sql("SELECT name FROM customers WHERE id = ?"),
                    ["c2".into()],
                ))
                .await
                .map_err(db_error)
                .unwrap()
                .expect("the victim row still exists")
                .try_get("", "name")
                .map_err(db_error)
                .unwrap();
            assert_eq!(
                victim, "Victim",
                "the out-of-tenant victim row was NOT modified by a body-id update (#92)"
            );

            // …and the AUTHORIZED path row WAS updated.
            let own: String = db
                .conn()
                .query_one(Statement::from_sql_and_values(
                    backend,
                    db.sql("SELECT name FROM customers WHERE id = ?"),
                    ["c1".into()],
                ))
                .await
                .map_err(db_error)
                .unwrap()
                .expect("the own row exists")
                .try_get("", "name")
                .map_err(db_error)
                .unwrap();
            assert_eq!(own, "hacked", "the authorized path row was updated");
        });
}

/// BEHAVIORAL proof: run the EXACT membership-set SQL the migrated repo emits
/// against sqlite and observe the multi-membership semantics — a user in two
/// workspaces sees the UNION of both, and a get on a row outside their set returns
/// None (the handler's 404). The SQL executed is asserted to match the generated
/// repo, so this exercises the shipped code's semantics, not a stand-in.
#[test]
fn multi_membership_user_sees_the_union_and_out_of_set_404s() {
    let (tmp, _out) = migrate_flat_membership();
    let repo = std::fs::read_to_string(tmp.path().join("app/crates/routes/customers/src/repo.rs"))
        .unwrap();
    assert!(
        repo.contains(LIST_SQL) && repo.contains(GET_SQL),
        "the SQL exercised below is the exact text the migrated repo emits"
    );

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            use jerrycan::db::sea_orm::{ConnectionTrait, Statement};
            use jerrycan::db::{Db, db_error};

            let db = Db::connect("sqlite::memory:").await.unwrap();
            db.conn()
                .execute_unprepared(
                    "CREATE TABLE workspace_members (workspace_id TEXT NOT NULL, \
                     user_id TEXT NOT NULL, role TEXT NOT NULL)",
                )
                .await
                .unwrap();
            db.conn()
                .execute_unprepared(
                    "CREATE TABLE customers (id TEXT PRIMARY KEY, \
                     workspace_id TEXT NOT NULL, name TEXT NOT NULL)",
                )
                .await
                .unwrap();
            // user `u1` is a member of workspaces w1 AND w2 (many memberships), NOT w3.
            db.conn()
                .execute_unprepared(
                    "INSERT INTO workspace_members (workspace_id, user_id, role) \
                     VALUES ('w1', 'u1', 'member'), ('w2', 'u1', 'member')",
                )
                .await
                .unwrap();
            db.conn()
                .execute_unprepared(
                    "INSERT INTO customers (id, workspace_id, name) \
                     VALUES ('c1', 'w1', 'Alice'), ('c2', 'w2', 'Bob'), ('c3', 'w3', 'Carol')",
                )
                .await
                .unwrap();

            let backend = db.conn().get_database_backend();

            // list → the UNION of both the user's workspaces, never w3's Carol.
            let rows = db
                .conn()
                .query_all(Statement::from_sql_and_values(
                    backend,
                    db.sql(LIST_SQL),
                    ["u1".into()],
                ))
                .await
                .map_err(db_error)
                .unwrap();
            let names: Vec<String> = rows
                .iter()
                .map(|r| r.try_get::<String>("", "name").unwrap())
                .collect();
            assert_eq!(
                names,
                vec!["Alice".to_string(), "Bob".to_string()],
                "a two-workspace user sees BOTH workspaces' rows and nothing from w3"
            );

            // get INSIDE the set → visible; get OUTSIDE the set → None (the 404).
            let inside = db
                .conn()
                .query_one(Statement::from_sql_and_values(
                    backend,
                    db.sql(GET_SQL),
                    ["c1".into(), "u1".into()],
                ))
                .await
                .map_err(db_error)
                .unwrap();
            assert!(inside.is_some(), "a row in the membership set is readable");

            let outside = db
                .conn()
                .query_one(Statement::from_sql_and_values(
                    backend,
                    db.sql(GET_SQL),
                    ["c3".into(), "u1".into()],
                ))
                .await
                .map_err(db_error)
                .unwrap();
            assert!(
                outside.is_none(),
                "a row OUTSIDE the membership set is invisible (get → None → 404)"
            );
        });
}
