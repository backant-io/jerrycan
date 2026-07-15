# Tenancy

## Purpose
jerrycan makes multi-tenancy first-class: declare a top-level `tenancy` block in
the design and the generator emits the membership table, a membership-checked
`Tenant` guard, tenant-scoped repo methods (`all_for`/`get_for`/`remove_for`) on
every entity that `belongs_to` the tenant, and cross-tenant isolation acceptance
tests (tenant A must never read tenant B's rows). Tenancy requires auth (a
session model supplies the caller identity) and db. It exists so an agent gets
data isolation by construction, not by remembering to filter every query.

## Per-user data without tenancy
Tenancy is for **org/team** entities with memberships — a `Workspace`, `Org`, or
`Team` that many users belong to and share rows within. It is the wrong tool for
**per-user** data (a user's own notes, tasks, uploads), where each row belongs to
exactly one identity.

For per-user ownership, do NOT declare a `tenancy` block. Instead give the owned
entity a `belongs_to` the identity entity your sessions resolve to (typically a
`User`/`Account`) — deriving an indexed owner fk — and scope EVERY read and write
by the session user's id in `repo.rs`/handlers, exactly as you would with a
tenant id.

The tenant entity must never BE the auth identity entity: a user cannot be their
own tenant org. `tenancy.entity` names a separate org/team the identity holds a
membership in. If the "tenant" and the logged-in user would be the same row, you
want per-user scoping, not tenancy.

## Signature
The design declares one tenant entity and the roles a membership can hold:
```json
{
  "tenancy": { "entity": "Workspace", "member_roles": ["owner", "member"] }
}
```
That generates, in the shared crate, a `Tenant` guard and its DI factory:
```rust
# use jerrycan::prelude::*;
# struct Tenant { id: i64, role: String }
impl Tenant {
    pub fn id(&self) -> i64 { self.id }                 // the caller's tenant id
    pub fn require_role(&self, role: &str) -> Result<()> // wrong role → 403
#       { jerrycan::auth::require_role(&self.role, role) }
}
# let _ = |t: &Tenant| (t.id(), t.require_role("owner"));
```
The factory looks the caller up in `{tenant}_members` (here `workspace_members`:
`user_id`, `workspace_id`, `role`) and rejects before the handler runs — `401`
without a session, `403` with no membership row. Handlers on tenant-owned routes
take `Dep<Tenant>` instead of the bare session guard.

## Minimal example
A standalone `Tenant` guard + factory (exactly what the generator emits) over
`sqlite::memory:` with a hand-created `workspace_members` table. A non-member
gets `403`; a seeded member gets `200` and sees their own tenant id:
```rust
# use jerrycan::prelude::*;
# use jerrycan::auth::{Auth, Session};
# use jerrycan::db::sea_orm::{ConnectionTrait, Statement};
# use jerrycan::db::{db_error, Db};
# use serde::{Deserialize, Serialize};
// The session payload (the shared crate's `SessionUser` + `CurrentUser` alias).
#[derive(Serialize, Deserialize, Clone)]
struct SessionUser { id: i64, role: String }
type CurrentUser = Session<SessionUser>;

// The generated `Tenant` guard: a membership-checked tenant id + role.
#[derive(Clone)]
struct Tenant { id: i64, role: String }
impl Tenant {
    fn id(&self) -> i64 { self.id }
    fn require_role(&self, role: &str) -> Result<()> {
        jerrycan::auth::require_role(&self.role, role)
    }
}

// The DI factory: resolves the caller's membership or rejects 403 before the
// handler. A missing session already failed `CurrentUser` with 401.
async fn tenant(user: CurrentUser, db: Dep<Db>) -> Result<Tenant> {
    let row = db
        .conn()
        .query_one(Statement::from_sql_and_values(
            db.conn().get_database_backend(),
            db.sql("SELECT workspace_id, role FROM workspace_members WHERE user_id = ?"),
            [user.0.id.into()],
        ))
        .await
        .map_err(db_error)?;
    let Some(row) = row else { return Err(Error::forbidden()) };
    Ok(Tenant {
        id: row.try_get("", "workspace_id").map_err(db_error)?,
        role: row.try_get("", "role").map_err(db_error)?,
    })
}

// A tenant-owned handler: the guard is the gate, `tenant.id()` is trusted.
async fn my_workspace(tenant: Dep<Tenant>) -> Json<i64> { Json(tenant.id()) }

# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
let db = Db::connect("sqlite::memory:").await.unwrap();
db.conn().execute_unprepared(
    "CREATE TABLE workspace_members (id INTEGER PRIMARY KEY AUTOINCREMENT, \
     user_id BIGINT NOT NULL, workspace_id BIGINT NOT NULL, role TEXT NOT NULL)",
).await.unwrap();
// User 7 belongs to workspace 1 as owner; user 9 belongs to no workspace.
db.conn().execute_unprepared(
    "INSERT INTO workspace_members (user_id, workspace_id, role) VALUES (7, 1, 'owner')",
).await.unwrap();

let auth = Auth::with_secret("a-very-long-development-secret-string!!");
let cookie = |id: i64| {
    let c = auth.sessions().set_cookie(&SessionUser { id, role: "user".into() }).unwrap();
    c.split(';').next().unwrap().to_string()
};
let member = cookie(7);
let outsider = cookie(9);

let t = App::new()
    .extend(db)
    .extend(auth)
    .provide_dep(tenant)                                 // register the guard app-wide
    .route("/me/workspace", get(my_workspace))
    .into_test();

use jerrycan::http::StatusCode;
assert_eq!(t.get("/me/workspace").await.status(), StatusCode::UNAUTHORIZED); // no session → 401
assert_eq!(t.get_with("/me/workspace", &[("cookie", &outsider)]).await.status(), StatusCode::FORBIDDEN); // no membership → 403
assert_eq!(t.get_with("/me/workspace", &[("cookie", &member)]).await.json::<i64>(), 1);
# }); }
```

## Variations
Tenant-scoped repo methods are what keep handlers honest: every entity that
`belongs_to` the tenant gets `all_for`/`get_for`/`remove_for`, each filtering by
the tenant fk so a query can only ever touch the caller's rows. Combine the
scoped method with `require_role` for a write gate:
```rust
# use jerrycan::prelude::*;
# use jerrycan::db::sea_orm::{ConnectionTrait, Statement};
# use jerrycan::db::{db_error, Db};
# struct Tenant { id: i64, role: String }
# impl Tenant {
#     fn id(&self) -> i64 { self.id }
#     fn require_role(&self, role: &str) -> Result<()> { jerrycan::auth::require_role(&self.role, role) }
# }
// A repo over a tenant-owned table; `all_for` scopes every read to one tenant.
struct LeadRepo { db: Db }
impl LeadRepo {
    async fn all_for(&self, workspace_id: i64) -> Result<Vec<String>> {
        let rows = self.db.conn().query_all(Statement::from_sql_and_values(
            self.db.conn().get_database_backend(),
            self.db.sql("SELECT name FROM leads WHERE workspace_id = ? ORDER BY id"),
            [workspace_id.into()],
        )).await.map_err(db_error)?;
        rows.iter().map(|r| r.try_get::<String>("", "name").map_err(db_error)).collect()
    }
}

// The handler scopes the read by `tenant.id()` — never a client-sent id.
async fn list_leads(tenant: Dep<Tenant>, repo: Dep<LeadRepo>) -> Result<Json<Vec<String>>> {
    Ok(Json(repo.all_for(tenant.id()).await?))
}
// A role-gated delete checks membership role first, then the scoped accessor.
async fn purge(tenant: Dep<Tenant>) -> Result<NoContent> {
    tenant.require_role("owner")?;                       // member but not owner → 403
    Ok(NoContent)
}

# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
let db = Db::connect("sqlite::memory:").await.unwrap();
db.conn().execute_unprepared(
    "CREATE TABLE leads (id INTEGER PRIMARY KEY AUTOINCREMENT, workspace_id BIGINT NOT NULL, name TEXT NOT NULL)",
).await.unwrap();
db.conn().execute_unprepared(
    "INSERT INTO leads (workspace_id, name) VALUES (1, 'mine'), (2, 'theirs')",
).await.unwrap();
let repo = LeadRepo { db };
// Tenant 1 sees only its own row; tenant 2's 'theirs' is unreachable.
assert_eq!(repo.all_for(1).await.unwrap(), vec!["mine".to_string()]);

let member = Tenant { id: 1, role: "member".into() };
let owner = Tenant { id: 1, role: "owner".into() };
assert!(member.require_role("owner").is_err());         // 403
assert!(owner.require_role("owner").is_ok());
# let _ = (list_leads, purge); }); }
```

## Errors you'll hit
- No session (missing/invalid cookie or bearer) → `401 JC0401`. The `Tenant`
  factory never runs; the session guard already rejected.
- Authenticated but no membership row in `{tenant}_members`, or a `require_role`
  mismatch → `403 JC0403`.
- `JL0006` (a generation lint, not a runtime error): a handler for a
  tenant-owned entity called an unscoped repo method (`all`/`get`/`remove`), so
  it could read or delete another tenant's rows. Fix: call the scoped accessor
  (`all_for`/`get_for`/`remove_for`) with `tenant.id()`. `jerrycan check`
  surfaces it; `jerrycan explain JL0006` prints the fix.

## Anti-patterns
- Don't run unscoped queries on tenant-owned tables. `Entity::find().all(...)`
  in a handler path leaks every tenant's rows — use the `_for` accessors. The
  generated isolation test goes red and JL0006 flags the call.
- Don't trust a client-sent workspace/tenant id (path param, body field). The
  `Tenant` guard is the only authority on which tenant the caller is; pass
  `tenant.id()` into the scoped methods, never an id from the request.
- Don't share one membership seed across tests. Each acceptance test uses its
  own `sqlite::memory:` and seeds its own `{tenant}_members` rows, so a leaked
  membership can't make an isolation test pass by accident.
