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
> flat route) the request body — and it MUST be in the caller's membership set. A
> membership miss on a **read** is `404` (no existence leak); a wrong role is `403`.
> A list with no specific tenant returns the caller's whole membership set.
>
> **What the generator verifies for you:** on a **path-scoped** route the guard
> verifies the path tenant before the handler runs; on a **flat** route both reads
> AND writes go through generated membership-checked methods — reads via
> `all_for_memberships`/`get_for_memberships`, writes via `create_for_memberships`/
> `update_for_memberships`/`remove_for_memberships`, which verify `body.{fk}` is in
> the caller's membership set (RLS `WITH CHECK`) and reject a non-member tenant with
> `403`. You don't hand-write the membership check on any shape. See "Flat writes".

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

### Public reads on per-user data (`public_read`)
The third ownership shape — **public-read / owner-write** — is the feed/blog/
listing model: anyone (even anonymous) reads every owner's rows, but only the
owner writes. Set `"public_read": true` on the per-user entity (valid ONLY on an
identity-owned, non-tenant entity in an auth design; anything else is `JC0549`):

- **Reads open up:** the GET handlers take no `CurrentUser` (even if declared
  `auth_required`), the repo gets its unscoped `all()`/`get()` back, the OpenAPI
  operations drop their `security` stanza, and no 401 tests are generated for the
  reads. A public list serves the **whole collection**, deliberately.
- **Writes stay exactly as owner-scoped:** guarded, server-injected `user_id` on
  create, `update_for`/`remove_for` (a non-owner's update/delete → 404, hiding
  existence); the unscoped `update()`/`remove()` stay un-generated, and a write
  endpoint marked `public`/unguarded is rejected. A GET with `required_roles`
  keeps its guard — an explicit role demand outranks the flag.
- The generated isolation test proves the full contract: an anonymous list
  returns another user's row (200), an anonymous create 401s, a non-owner
  update/delete 404s with the row surviving, and the owner's update succeeds.

Without the flag, an unguarded (or `public`) GET on a per-user entity is refused
as unimplementable (`JC0549`) — the owner-scoped repo has no unscoped read to
call. Tenant-owned entities cannot opt in: their reads stay membership-gated.

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

### Flat writes — the body tenant fk is verified for you (`WITH CHECK`)

Writes on a flat route are membership-checked by generated code, just like reads — you
do **not** hand-write the check. The generated repo emits three checked accessors, and
the flat `create`/`update`/`delete` stubs are steered to them:

- `create_for_memberships(user_id, item)` — the body carries the tenant fk; it is
  verified `∈` your membership set before the insert (`403` otherwise), exactly the
  RLS `WITH CHECK` the read side mirrors.
- `update_for_memberships(user_id, id, item)` — updates only a row whose current
  tenant is in your set (a row outside it is `404`), and refuses to move the row to
  another tenant (a changed tenant fk is `403`).
- `remove_for_memberships(user_id, id)` — deletes only a row whose tenant is in your
  set (outside it, `0` rows → `404`), never a cross-tenant delete.

So a user in workspace 1 who `POST`s `{workspace_id: 2}` — a tenant they don't belong
to — gets `403` from `create_for_memberships`. The flat cross-tenant write is closed
when the create goes through that generated checked method: the handler is steered to
it, and `JL0006` flags a bare `insert` that skips it — steer + lint, not a suppressed
method.

The example below is the generated create's `WITH CHECK`, standalone (user `7` is a
member of workspace 1, not 3):
```rust
# use jerrycan::prelude::*;
# use jerrycan::auth::Session;
# use jerrycan::db::sea_orm::{ConnectionTrait, Statement};
# use jerrycan::db::{db_error, Db};
# use serde::{Deserialize, Serialize};
# #[derive(Serialize, Deserialize, Clone)]
# struct SessionUser { id: String, role: String }
# type CurrentUser = Session<SessionUser>;
// A repo over a flat tenant-owned table; the create verifies the BODY tenant fk.
struct CustomerRepo { db: Db }
impl CustomerRepo {
    // The generated `create_for_memberships` shape: the body's tenant fk MUST be in
    // the caller's membership set (RLS `WITH CHECK`) — a non-member tenant is 403.
    async fn create_for_memberships(&self, user_id: String, workspace_id: i64, name: String) -> Result<i64> {
        let member = self.db.conn().query_one(Statement::from_sql_and_values(
            self.db.conn().get_database_backend(),
            self.db.sql("SELECT 1 FROM workspace_members WHERE user_id = ? AND workspace_id = ? LIMIT 1"),
            [user_id.into(), workspace_id.into()],
        )).await.map_err(db_error)?;
        if member.is_none() {
            return Err(Error::forbidden()); // 403: writing into a non-member tenant
        }
        self.db.conn().execute(Statement::from_sql_and_values(
            self.db.conn().get_database_backend(),
            self.db.sql("INSERT INTO customers (workspace_id, name) VALUES (?, ?)"),
            [workspace_id.into(), name.into()],
        )).await.map_err(db_error)?;
        Ok(workspace_id)
    }
}

// The flat create handler takes the session guard and passes `user.0.id` — the checked
// method does the `WITH CHECK`, so the handler never trusts the body tenant fk itself.
async fn create_customer(repo: Dep<CustomerRepo>, user: CurrentUser, workspace_id: i64) -> Result<i64> {
    repo.create_for_memberships(user.0.id, workspace_id, "Alice".into()).await
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
// User 7 belongs to workspace 1 — NOT 3.
db.conn().execute_unprepared(
    "INSERT INTO workspace_members (user_id, workspace_id, role) VALUES ('7', 1, 'member')",
).await.unwrap();
let repo = CustomerRepo { db };

// Creating in your OWN tenant works; creating in a tenant you don't belong to is 403.
assert!(repo.create_for_memberships("7".into(), 1, "Alice".into()).await.is_ok());
assert!(repo.create_for_memberships("7".into(), 3, "Mallory".into()).await.is_err());
# let _ = create_customer; }); }
```

(Path-scoped and per-user writes are covered differently: the guard verified the path
tenant (or the owner scope fixed the user), and `update_for`/`remove_for` key on that
verified `tenant.id()`/session id — so those never read a tenant fk from the body at
all, and they pin the row's pk to the PATH `id`, never the body `item.id`. A
hand-written variant must pin the id to the path param too: a body id could address
another tenant's or user's row (issue #92).)

## Creating a tenant auto-seeds membership + membership-filtered list
There is **no hand-written membership INSERT**. The tenant entity's own repo gets
these generated methods, and its collection handlers are steered to the first two:

- `create_with_membership(user_id, item)` inserts the tenant AND seeds the creator
  into `{tenant}_members` as the first declared `member_role`, in one transaction —
  so a fresh tenant is never memberless and the guard admits the creator on the very
  next request. "Creator becomes organizer" is guaranteed, not an agent TODO.
- `all_for_member(user_id)` lists ONLY the tenants the caller belongs to
  (`JOIN {tenant}_members ... WHERE user_id = ?`), never the unscoped `all()`.
- `members_of(fk)`, `add_member(fk, user_id, role)`, `set_member_role(fk, user_id,
  role)`, `remove_member(fk, user_id)`, plus the `count_admins(fk)` helper, back
  the generated member-management routes (next section) — real SQL against
  `{tenant}_members`, keyed on the path tenant fk the guard verified. You rarely
  call these yourself: the generated member handlers already do.

The generated collection handlers carry a stub comment naming these methods, so a
`POST /clubs/` create routes to `create_with_membership(_user.0.id, ...)` and a
`GET /clubs/` list to `all_for_member(_user.0.id)`. This is what makes the guard
above work end to end: the create is what puts the membership row in place.

## Member management is generated — list, add, set-role, remove (no raw SQL)
Every tenancy app gets a complete, tool-owned member-management surface — you never
hand-write an `INSERT INTO {tenant}_members ...` to invite someone. Four routes are
registered on the tenant module, path-scoped under the tenant fk, so the same
membership-verified `Tenant` guard runs first (a non-member of the addressed tenant
gets `404` before any handler):

- `GET /{tenant}/{fk}/members` — the roster `[{id, user_id, role}]`. Any member may
  read it; the guard is the whole gate.
- `POST /{tenant}/{fk}/members` — add `{user_id, role}` → `201`. Admin-gated. A
  duplicate membership is `409` (the `UNIQUE(user_id, fk)` index); `user_id` is
  opaque (no FK to a user table — migrated-uuid support), so existence is not
  DB-verified.
- `PATCH /{tenant}/{fk}/members/{user_id}` — set `{role}` → `204`. Admin-gated; an
  unknown member is `404`.
- `DELETE /{tenant}/{fk}/members/{user_id}` — remove → `204`. Admin-gated, EXCEPT
  self-removal: any member may DELETE their **own** membership ("leave") without
  the admin role. An unknown member is `404`.

The rules the generated handlers and repo enforce:

- **The admin role is `member_roles[0]`** — position 0 of the design's
  `tenancy.member_roles`, by convention (this is why `JC0548` requires the list to
  be non-empty and duplicate-free). Writes call
  `tenant.require_role(member_roles[0])`; a non-admin write is `403`.
- **`role ∈ member_roles`** on add and set-role — an out-of-set role is `422` (the
  membership table has no DB CHECK on the role column; the generated code is the
  validator).
- **Last-admin protection**: removing or demoting the last member holding the
  admin role is `409` — a tenant can never be left admin-less (nobody could manage
  members again). This applies to self-removal too.

The routes are first-class: they appear in the generated OpenAPI (with the `role`
enum pinned to the declared `member_roles`) and in `jerrycan routes`, and the
generated acceptance suite covers them (list, add, non-admin 403, set-role,
remove, last-admin 409, self-leave, out-of-set-role 422). Those tests pass on a
fresh scaffold — the handlers are real generated code, not stubs — and turn red
only if the surface breaks. A single-role design emits only the four
role-independent tests (list, add, last-admin remove 409, out-of-set-role 422) —
the other five need a seeded non-admin member, which a one-role design cannot
express.

## Errors you'll hit
- No session (missing/invalid cookie or bearer) → `401 JC0401`. The `Tenant` factory
  never runs; the session guard already rejected.
- Not a member of the **addressed** tenant on a path-scoped or flat read → `404`
  (no existence leak — a non-member cannot tell a real tenant from a missing one).
- A write to a tenant the caller doesn't belong to, or a `require_role` mismatch →
  `403 JC0403`.
- On the member routes: an out-of-set `role` on add/set-role → `422`; a duplicate
  add, or removing/demoting the last admin → `409`; a non-admin member write
  (other than self-removal) → `403`.
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
- Don't trust a client-sent tenant id in a body or a header as the scope. On reads
  and path-scoped routes the scope comes from the verified guard or the
  membership-set methods, never from unverified request data. On a **flat write**
  (see "Flat writes") the body's tenant fk IS verified for you — route the write
  through `create_for_memberships`/`update_for_memberships`/`remove_for_memberships`,
  never the unscoped `insert`/`update`/`remove`, which would trust the body fk.
- Don't hand-write the membership seed on tenant create. `create_with_membership`
  does it in one transaction; a hand-rolled INSERT is the thing that gets dropped and
  locks the creator out of their own tenant.
- Don't hand-write member invite/remove/list routes or raw `{tenant}_members` SQL.
  The generated member surface (above) already ships them with the admin gate, the
  last-admin rule, and role validation — a hand-rolled variant is where those
  protections silently go missing.
- Don't share one membership seed across tests. Each acceptance test uses its own
  `sqlite::memory:` and seeds its own `{tenant}_members` rows, so a leaked membership
  can't make an isolation test pass by accident.
