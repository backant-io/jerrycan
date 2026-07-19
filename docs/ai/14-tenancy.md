# Tenancy

## Purpose
jerrycan makes multi-tenancy first-class and **membership-verified**: declare a
top-level `tenancy` block in the design and the generator emits the membership
table, a membership-checked `Tenant` guard, tenant-scoped repo methods on every
entity that `belongs_to` the tenant, an auto-seeded membership on tenant create, a
membership-filtered tenant list, and cross-tenant isolation acceptance tests
(tenant A must never read tenant B's rows). Tenancy requires auth (a session model
supplies the caller identity) and db. It exists so an agent gets data isolation by
construction — not by remembering to filter every query, and not by trusting a
client-sent id.

A user can belong to **many** tenants — a person in several clubs, an account in
several workspaces. That is a first-class, safe shape: the tenant a request acts on
is taken from the **route** and verified against the caller's membership in *that*
tenant, never resolved to one arbitrary "membership of something".

## The rule
> The tenant id for an operation comes from the **route** — a path param, or (on a
> flat create) the request body — and it MUST be in the caller's membership set,
> verified by generated code before the handler scopes to it. A list with no
> specific tenant returns the caller's whole membership set. A membership miss on a
> **read** is `404` (no existence leak); a **write** to a non-member tenant, or a
> role the membership lacks, is `403`.

Two route shapes specialize that rule, and the generator picks the right one per
endpoint:

- **Path-scoped** — the path names the tenant fk (a nested mount like
  `/clubs/{club_id}/...`, or the tenant entity's own `/{club_id}` detail route).
  The guard verifies membership in *that* tenant and hands the handler a `Tenant`
  whose `id()` is provably the addressed tenant.
- **Membership-set (flat)** — a tenant-owned route with no tenant fk in its path
  (the Supabase-migrated shape, and any authored flat design). The generated repo
  methods scope to the caller's whole membership set — the RLS model, restored.

## Per-user data without tenancy
Tenancy is for **org/team** entities with memberships — a `Workspace`, `Org`, or
`Team` that many users belong to and share rows within. It is the wrong tool for
**per-user** data (a user's own notes, tasks, uploads), where each row belongs to
exactly one identity.

For per-user ownership, do NOT declare a `tenancy` block. Instead give the owned
entity a `belongs_to` the identity entity your sessions resolve to (typically a
`User`/`Account`), deriving an indexed owner fk. The generator then makes the leak
impossible: the repo emits **only** the owner-scoped accessors
(`all_for`/`get_for`/`update_for`/`remove_for`, each keyed by the session user's
id) — the unscoped `all()`/`get()`/`update()`/`remove()` are **not generated**, so
a handler cannot accidentally read or mutate another user's rows, and a generated
isolation test proves user B cannot reach user A's rows.

The tenant entity must never BE the auth identity entity: a user cannot be their
own tenant org, and the validator rejects such a design before scaffolding with
`JC0540` (`jerrycan explain JC0540`). `tenancy.entity` names a separate org/team
the identity holds a membership in. If the "tenant" and the logged-in user would be
the same row, you want per-user scoping, not tenancy.

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
    pub fn id(&self) -> i64 { self.id }                 // the addressed tenant id
    pub fn require_role(&self, role: &str) -> Result<()> // wrong role → 403
#       { jerrycan::auth::require_role(&self.role, role) }
}
# let _ = |t: &Tenant| (t.id(), t.require_role("owner"));
```
On a path-scoped route the factory reads the tenant fk from the URL and checks the
caller's membership in `{tenant}_members` for *that* tenant, rejecting before the
handler runs — `401` without a session, `404` when the caller is not a member of
the addressed tenant (no existence leak). Handlers on path-scoped routes take
`Dep<Tenant>` instead of the bare session guard; flat handlers take the session
guard directly and scope via the membership-set repo methods (below).

## Minimal example — path-scoped, membership-verified, many-tenants
The generated `Tenant` guard and its `tenant` factory (path-scoped branch), exactly
as emitted, over `sqlite::memory:`. User `7` belongs to **two** clubs; a request is
scoped to the club in its **path** and verified against that membership. A non-member
— or an outsider — gets `404`, never a leak of another club's data:
```rust
# use jerrycan::prelude::*;
# use jerrycan::auth::{Auth, Session};
# use jerrycan::db::sea_orm::{ConnectionTrait, Statement};
# use jerrycan::db::{db_error, Db};
# use jerrycan::extract::PathParams;
# use serde::{Deserialize, Serialize};
// The session payload (the shared crate's `SessionUser`). The generated
// membership table's `user_id` is TEXT — the stringified session id.
#[derive(Serialize, Deserialize, Clone)]
struct SessionUser { id: String, role: String }
type CurrentUser = Session<SessionUser>;

// The generated `Tenant` guard: a membership-verified tenant id + role.
#[derive(Clone)]
struct Tenant { id: i64, role: String }
impl Tenant {
    fn id(&self) -> i64 { self.id }
    fn require_role(&self, role: &str) -> Result<()> {
        jerrycan::auth::require_role(&self.role, role)
    }
}

// The generated DI factory. When the path names the tenant fk (`club_id`), it
// verifies membership in THAT club — a miss is 404, never an arbitrary membership.
async fn tenant(user: CurrentUser, db: Dep<Db>, params: PathParams) -> Result<Tenant> {
    if let Some(club_id) = params.get("club_id") {
        let club_id: i64 = match club_id.parse() {
            Ok(v) => v,
            Err(_) => return Err(Error::not_found()),
        };
        let row = db
            .conn()
            .query_one(Statement::from_sql_and_values(
                db.conn().get_database_backend(),
                db.sql("SELECT role FROM club_members WHERE user_id = ? AND club_id = ?"),
                [user.0.id.into(), club_id.into()],
            ))
            .await
            .map_err(db_error)?;
        let Some(row) = row else { return Err(Error::not_found()) };
        return Ok(Tenant { id: club_id, role: row.try_get("", "role").map_err(db_error)? });
    }
    // No tenant fk in the path (e.g. the tenant's own collection): fall back to the
    // caller's first membership. 403 on no membership at all.
    let row = db
        .conn()
        .query_one(Statement::from_sql_and_values(
            db.conn().get_database_backend(),
            db.sql("SELECT club_id, role FROM club_members WHERE user_id = ?"),
            [user.0.id.into()],
        ))
        .await
        .map_err(db_error)?;
    let Some(row) = row else { return Err(Error::forbidden()) };
    Ok(Tenant {
        id: row.try_get("", "club_id").map_err(db_error)?,
        role: row.try_get("", "role").map_err(db_error)?,
    })
}

// A path-scoped handler: `tenant.id()` is provably the club named in the URL.
async fn whoami(tenant: Dep<Tenant>) -> Json<i64> { Json(tenant.id()) }

# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
let db = Db::connect("sqlite::memory:").await.unwrap();
db.conn().execute_unprepared(
    "CREATE TABLE club_members (id INTEGER PRIMARY KEY AUTOINCREMENT, \
     user_id TEXT NOT NULL, club_id BIGINT NOT NULL, role TEXT NOT NULL)",
).await.unwrap();
// User 7 is a member of BOTH club 1 (organizer) and club 2 (member); user 9 of none.
db.conn().execute_unprepared(
    "INSERT INTO club_members (user_id, club_id, role) \
     VALUES ('7', 1, 'organizer'), ('7', 2, 'member')",
).await.unwrap();

let auth = Auth::with_secret("a-very-long-development-secret-string!!");
let cookie = |id: &str| {
    let c = auth.sessions().set_cookie(&SessionUser { id: id.into(), role: "user".into() }).unwrap();
    c.split(';').next().unwrap().to_string()
};
let member = cookie("7");
let outsider = cookie("9");

let t = App::new()
    .extend(db)
    .extend(auth)
    .provide_dep(tenant)                                 // register the guard app-wide
    .route("/clubs/{club_id}/whoami", get(whoami))
    .into_test();

use jerrycan::http::StatusCode;
// No session → 401 (the factory never runs).
assert_eq!(t.get("/clubs/1/whoami").await.status(), StatusCode::UNAUTHORIZED);
// Member of many clubs: the id follows the PATH, verified each time.
assert_eq!(t.get_with("/clubs/1/whoami", &[("cookie", &member)]).await.json::<i64>(), 1);
assert_eq!(t.get_with("/clubs/2/whoami", &[("cookie", &member)]).await.json::<i64>(), 2);
// A club user 7 is NOT a member of → 404 (no existence leak), never club 1 or 2's data.
assert_eq!(t.get_with("/clubs/3/whoami", &[("cookie", &member)]).await.status(), StatusCode::NOT_FOUND);
// An outsider on a real club → 404 as well (membership miss, not a 403 that would
// confirm the club exists).
assert_eq!(t.get_with("/clubs/1/whoami", &[("cookie", &outsider)]).await.status(), StatusCode::NOT_FOUND);
# }); }
```

## Flat (membership-set) routes — the Supabase shape
A tenant-owned entity whose routes carry **no** tenant fk in the path (the shape a
Supabase RLS export migrates to, and any authored flat design) is scoped to the
caller's whole membership set. The generated repo emits `all_for_memberships` and
`get_for_memberships`, whose filter is the Supabase RLS subquery restored verbatim —
`{fk} IN (SELECT {fk} FROM {tenant}_members WHERE user_id = ?)`. A multi-membership
user sees every tenant's rows they belong to (the union), and nothing outside the
set; a `get/{id}` for a row whose tenant is outside the set is `404`.

The example below is the generated filter, standalone (the generated method returns
full entities via `Entity::find().from_raw_sql(...)`; here it selects `name` to stay
self-contained). User `7` is a member of workspaces 1 and 2, not 3:
```rust
# use jerrycan::prelude::*;
# use jerrycan::auth::Session;
# use jerrycan::db::sea_orm::{ConnectionTrait, Statement};
# use jerrycan::db::{db_error, Db};
# use serde::{Deserialize, Serialize};
# #[derive(Serialize, Deserialize, Clone)]
# struct SessionUser { id: String, role: String }
# type CurrentUser = Session<SessionUser>;
# struct Tenant { id: i64, role: String }
# impl Tenant {
#     fn require_role(&self, role: &str) -> Result<()> { jerrycan::auth::require_role(&self.role, role) }
# }
// A repo over a flat tenant-owned table; both accessors scope by the membership SET.
struct CustomerRepo { db: Db }
impl CustomerRepo {
    async fn all_for_memberships(&self, user_id: String) -> Result<Vec<String>> {
        let rows = self.db.conn().query_all(Statement::from_sql_and_values(
            self.db.conn().get_database_backend(),
            self.db.sql(
                "SELECT name FROM customers \
                 WHERE workspace_id IN (SELECT workspace_id FROM workspace_members WHERE user_id = ?) \
                 ORDER BY id",
            ),
            [user_id.into()],
        )).await.map_err(db_error)?;
        rows.iter().map(|r| r.try_get::<String>("", "name").map_err(db_error)).collect()
    }
    async fn get_for_memberships(&self, user_id: String, id: i64) -> Result<Option<String>> {
        let row = self.db.conn().query_one(Statement::from_sql_and_values(
            self.db.conn().get_database_backend(),
            self.db.sql(
                "SELECT name FROM customers \
                 WHERE id = ? AND workspace_id IN (SELECT workspace_id FROM workspace_members WHERE user_id = ?)",
            ),
            [id.into(), user_id.into()],
        )).await.map_err(db_error)?;
        row.map(|r| r.try_get::<String>("", "name").map_err(db_error)).transpose()
    }
}

// The flat handler takes the session guard (never `Dep<Tenant>`) and scopes to the
// caller's memberships. A `None` from `get_for_memberships` becomes a 404.
async fn list_customers(repo: Dep<CustomerRepo>, user: CurrentUser) -> Result<Json<Vec<String>>> {
    Ok(Json(repo.all_for_memberships(user.0.id).await?))
}

# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
let db = Db::connect("sqlite::memory:").await.unwrap();
db.conn().execute_unprepared(
    "CREATE TABLE workspace_members (id INTEGER PRIMARY KEY AUTOINCREMENT, \
     user_id TEXT NOT NULL, workspace_id BIGINT NOT NULL, role TEXT NOT NULL)",
).await.unwrap();
db.conn().execute_unprepared(
    "CREATE TABLE customers (id INTEGER PRIMARY KEY AUTOINCREMENT, \
     workspace_id BIGINT NOT NULL, name TEXT NOT NULL)",
).await.unwrap();
// User 7 belongs to workspaces 1 and 2 — NOT 3.
db.conn().execute_unprepared(
    "INSERT INTO workspace_members (user_id, workspace_id, role) VALUES ('7', 1, 'member'), ('7', 2, 'member')",
).await.unwrap();
db.conn().execute_unprepared(
    "INSERT INTO customers (workspace_id, name) VALUES (1, 'Alice'), (2, 'Bob'), (3, 'Carol')",
).await.unwrap();
let repo = CustomerRepo { db };

// A multi-membership user sees BOTH their workspaces' rows (the union), never ws 3's.
assert_eq!(repo.all_for_memberships("7".into()).await.unwrap(), vec!["Alice".to_string(), "Bob".to_string()]);
// A row inside the set is readable; a row outside it is invisible → 404 at the handler.
assert_eq!(repo.get_for_memberships("7".into(), 1).await.unwrap(), Some("Alice".to_string()));
assert_eq!(repo.get_for_memberships("7".into(), 3).await.unwrap(), None);

// A role gate is orthogonal to scope: member but not owner → 403.
let member = Tenant { id: 1, role: "member".into() };
let owner = Tenant { id: 1, role: "owner".into() };
assert!(member.require_role("owner").is_err());
assert!(owner.require_role("owner").is_ok());
# let _ = list_customers; }); }
```

## Creating a tenant auto-seeds membership + membership-filtered list
There is **no hand-written membership INSERT**. The tenant entity's own repo gets two
generated methods, and its collection handlers are steered to them:

- `create_with_membership(user_id, item)` inserts the tenant AND seeds the creator
  into `{tenant}_members` as the first declared `member_role`, in one transaction —
  so a fresh tenant is never memberless and the guard admits the creator on the very
  next request. "Creator becomes organizer" is guaranteed, not an agent TODO.
- `all_for_member(user_id)` lists ONLY the tenants the caller belongs to
  (`JOIN {tenant}_members ... WHERE user_id = ?`), never the unscoped `all()`.

The generated collection handlers carry a stub comment naming these methods, so a
`POST /clubs/` create routes to `create_with_membership(_user.0.id, ...)` and a
`GET /clubs/` list to `all_for_member(_user.0.id)`. This is what makes the guard
above work end to end: the create is what puts the membership row in place.

## Errors you'll hit
- No session (missing/invalid cookie or bearer) → `401 JC0401`. The `Tenant` factory
  never runs; the session guard already rejected.
- Not a member of the **addressed** tenant on a path-scoped or flat read → `404`
  (no existence leak — a non-member cannot tell a real tenant from a missing one).
- A write to a tenant the caller doesn't belong to, or a `require_role` mismatch →
  `403 JC0403`.
- `JL0006` (a generation lint, not a runtime error): a handler for a tenant-owned or
  identity-owned entity called an unscoped repo method, so it could read or delete
  another tenant's (or user's) rows. Fix: call the scoped accessor for the route's
  shape — `all_for`/`get_for` with `_tenant.id()` (path-scoped),
  `all_for_memberships`/`get_for_memberships` with `_user.0.id` (flat), or the
  owner-scoped `all_for`/`get_for` (per-user). `jerrycan check` surfaces it;
  `jerrycan explain JL0006` prints the fix.

## Anti-patterns
- Don't run unscoped queries on tenant-owned tables. An `Entity::find().all(...)` in
  a handler path leaks every tenant's rows — use the scoped accessors. The generated
  isolation test goes red and JL0006 flags the call.
- Don't derive the tenant from the *user* and trust it across routes. The tenant is
  whatever the **route** addresses; the guard verifies the caller belongs to that
  tenant before the handler scopes to it. `tenant.id()` is safe precisely because it
  is the verified path tenant — not an arbitrary "first membership".
- Don't trust a client-sent tenant id in a body or a header as the scope. On a flat
  create the body's tenant fk is verified against the membership set (RLS
  `WITH CHECK` parity); everywhere else the scope comes from the verified guard or
  the membership-set methods, never from unverified request data.
- Don't hand-write the membership seed on tenant create. `create_with_membership`
  does it in one transaction; a hand-rolled INSERT is the thing that gets dropped and
  locks the creator out of their own tenant.
- Don't share one membership seed across tests. Each acceptance test uses its own
  `sqlite::memory:` and seeds its own `{tenant}_members` rows, so a leaked membership
  can't make an isolation test pass by accident.
