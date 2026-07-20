# Transitive Tenancy (#102 / #103) — 0.5.1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make tenant-ownership transitive so a member of Org A can never read/write Org B's grandchild rows (Contact→Account→Org), and make the JL0006 guardrail depth-aware and fail-loud.

**Architecture:** One resolver (`Design::tenant_path`) walks the `belongs_to` chain from any entity to the tenant and returns the JOIN path. Every ownership decision (route shape, scoped repo methods, lint, isolation test) keys off it. Direct children resolve to a **zero-join** path, so their generated output stays **byte-identical**; only transitive (grandchild+) entities gain JOIN-based raw SQL. Ambiguous (diamond) graphs are a design-time hard error (`JC0545`).

**Tech Stack:** Rust; `sea-orm` raw statements via `self.db.sql(...)`; `syn` for AST-based lint; the existing `jerrycan` generator crate (`crates/jerrycan/src/platform/`).

## Global Constraints

- **Byte-identity for direct apps:** any entity whose `tenant_path` has zero joins MUST produce output identical to today. Every method branches `if path.joins.is_empty() { <existing template verbatim> } else { <join template> }`. Do not "clean up" the existing direct templates.
- **The security boundary is the membership JOIN, never the URL path.** A nested-grandchild path param (`account_id`) is advisory.
- **Raw SQL uses identifiers only** from `TenantPath` (table/column names derived by `Design` helpers) — never interpolate a user *value*; values go through bound `?` params exactly as today.
- **Parent PK is always `id`; the tenant fk column is `Design::fk_column(&tenancy.entity)`; a child's fk to a parent is `Design::fk_column(&parent)`.**
- **`cargo semver-checks` must stay clean** — the framework public Rust API is additive-only. `TenantPath`/`JoinLink` are `pub(crate)`.
- **Ships as 0.5.1.** All 11 workspace crates bump together at release (Task 7), not before.
- Pre-commit runs `cargo fmt --all --check` and `cargo clippy --workspace --all-features -- -D warnings`; every commit must pass both.
- New unit tests live beside the code they test (in-module `#[cfg(test)]`), matching the existing `design.rs` / `genroute.rs` test convention.
- **Test-helper names in this plan are illustrative** (`design_from_json`, `find_route`, `run_lints`, `scaffold_*`, `generate_isolation_tests`, `module_for`, …). Before writing a task's tests, read that module's existing `#[cfg(test)]` block and **bind to the real helpers** — e.g. reuse whatever `tenant_owned_walks_modules_and_subroutes` (design.rs) uses to build a `Design`, and the existing lint/testgen tests' scaffold helpers. `Design::find_entity` and `Design::table_name` are real. The **assertions** (the JOIN SQL, the status codes, the codes) are the contract; adapt only the helper plumbing to the module's actual API.

---

## File Structure

- `crates/jerrycan/src/platform/design.rs` — **new** `TenantPath`/`JoinLink` types + `tenant_path` resolver; refactor `tenant_owned`, `endpoint_tenant_shape` onto it. Owns the transitive-ownership truth.
- `crates/jerrycan/src/platform/codes.rs` — register `JC0545` (ambiguous path) and `JL0008` (unscannable handler).
- `crates/jerrycan/src/platform/validate.rs` (or wherever JC0542/JC0544 are raised — confirm during Task 1) — surface `JC0545` from the design validator.
- `crates/jerrycan/src/platform/genroute.rs` — `scoped_methods` and the ownership predicates (`entity_is_flat_tenant_owned`, `entity_is_per_user_owned`) branch on `tenant_path`; join templates for reads (Task 3) and writes (Task 4).
- `crates/jerrycan/src/platform/lints.rs` — JL0006 becomes AST-based + nested-path-aware + `JL0008` fail-loud (Task 5).
- `crates/jerrycan/src/platform/testgen.rs` — isolation test seeds the intermediate chain for transitive entities (Task 6).
- `conformance/` — a 3-level-graph app proving red-on-unscoped / green-on-scoped (Task 7).
- `CHANGELOG.md`, `Cargo.toml` version fields — release (Task 7).

---

## Task 1: `tenant_path` resolver + `JC0545`

**Files:**
- Modify: `crates/jerrycan/src/platform/design.rs` (add types + `tenant_path`, near `tenant_owned` ~line 723 and `collect_tenant_owned` ~1060)
- Modify: `crates/jerrycan/src/platform/codes.rs` (register `JC0545` after `JC0544` ~line 219)
- Modify: the design validator that raises `JC0542/JC0544` (grep `"JC0544"` to find the raise site)
- Test: in-module tests in `design.rs`

**Interfaces:**
- Produces: `Design::tenant_path(&self, entity: &str) -> Option<TenantPath>`; `pub(crate) struct TenantPath { joins: Vec<JoinLink>, anchor_table: String, tenant_fk: String, entity_table: String }`; `pub(crate) struct JoinLink { child_table: String, child_fk: String, parent_table: String }`. `TenantPath` gains SQL helpers in Task 3.
- Consumed by: Tasks 2–6.

**Definitions (put in a doc comment):** the resolver walks `belongs_to` from `entity` toward `tenancy.entity`. The **anchor** is the entity that *directly* `belongs_to` the tenant; `joins` connect `entity`'s table down to the anchor's table (empty when `entity` is itself the anchor = direct child). `tenant_fk = fk_column(tenant)` lives on `anchor_table`. Each join is `child_table.child_fk = parent_table.id`.

- [ ] **Step 1: Write failing tests**

```rust
// in design.rs #[cfg(test)] mod tests
fn org_account_contact() -> Design {
    // Org (tenant) ; Account belongs_to Org ; Contact belongs_to Account
    design_from_json(r#"{ "...": "..." }"#) // build via the test helper used by tenant_owned_walks_modules_and_subroutes
}

#[test]
fn tenant_path_direct_child_has_no_joins() {
    let d = org_account_contact();
    let p = d.tenant_path("Account").expect("Account is tenant-owned");
    assert!(p.joins.is_empty(), "direct child = zero joins");
    assert_eq!(p.tenant_fk, "org_id");
    assert_eq!(p.anchor_table, d.table_name("Account"));
}

#[test]
fn tenant_path_grandchild_joins_through_parent() {
    let d = org_account_contact();
    let p = d.tenant_path("Contact").expect("Contact is transitively tenant-owned");
    assert_eq!(p.joins.len(), 1);
    assert_eq!(p.joins[0].child_table, d.table_name("Contact"));
    assert_eq!(p.joins[0].child_fk, "account_id");
    assert_eq!(p.joins[0].parent_table, d.table_name("Account"));
    assert_eq!(p.anchor_table, d.table_name("Account"));
    assert_eq!(p.tenant_fk, "org_id");
}

#[test]
fn tenant_path_none_for_unowned_entity() {
    let d = org_account_contact();
    assert!(d.tenant_path("Org").is_none(), "the tenant itself is not tenant-owned");
}

#[test]
fn tenant_path_ambiguous_diamond_raises_jc0545() {
    // Contact belongs_to [Account, Region]; both reach Org → ambiguous
    let d = diamond_design();
    let diags = d.validate();               // whatever entrypoint runs JC0542/JC0544
    assert!(diags.iter().any(|x| x.code == "JC0545"), "diamond → JC0545");
    assert!(d.tenant_path("Contact").is_none(), "ambiguous resolves to None");
}

#[test]
fn tenant_path_cycle_does_not_hang() {
    let d = cyclic_belongs_to_design();     // A→B, B→A, tenant elsewhere
    let _ = d.tenant_path("A");             // must return, not loop
}
```

- [ ] **Step 2: Run — expect FAIL** (`cargo test -p jerrycan tenant_path` → `tenant_path` not found).

- [ ] **Step 3: Implement the resolver + types**

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JoinLink {
    pub child_table: String,
    pub child_fk: String,     // fk_column(parent) on child
    pub parent_table: String, // joined on parent_table.id
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TenantPath {
    /// JOINs from the entity's own table up to `anchor_table`. Empty ⇒ direct child.
    pub joins: Vec<JoinLink>,
    /// Table carrying the tenant fk (== entity_table for a direct child).
    pub anchor_table: String,
    /// The tenant fk column on `anchor_table`, e.g. `org_id`.
    pub tenant_fk: String,
    /// The entity's own table (the SELECT/DELETE target).
    pub entity_table: String,
}

impl Design {
    /// The unique `belongs_to` chain from `entity` to `tenancy.entity`, or `None`
    /// (not tenant-owned, ambiguous → JC0545, or no tenancy). Direct child = zero
    /// joins, so this subsumes the old direct predicate.
    pub(crate) fn tenant_path(&self, entity: &str) -> Option<TenantPath> {
        let tenancy = self.tenancy.as_ref()?;
        if entity == tenancy.entity {
            return None; // the tenant itself is not tenant-owned
        }
        let mut visited = std::collections::BTreeSet::new();
        let joins = self.walk_to_tenant(entity, &tenancy.entity, &mut visited)?;
        Some(TenantPath {
            anchor_table: joins
                .last()
                .map(|j| j.parent_table.clone())
                .unwrap_or_else(|| self.table_name(entity)),
            tenant_fk: Self::fk_column(&tenancy.entity),
            entity_table: self.table_name(entity),
            joins,
        })
    }

    /// Collects every distinct join chain from `entity` to the entity that
    /// directly belongs_to `tenant`. A chain of `vec![]` means `entity` itself is
    /// that anchor (direct child). Returns 0 chains (no path), 1 (unique), or ≥2
    /// (ambiguous). PURE — no diagnostics; the caller decides.
    fn tenant_path_chains(
        &self,
        entity: &str,
        tenant: &str,
        visited: &mut std::collections::BTreeSet<String>,
    ) -> Vec<Vec<JoinLink>> {
        let Some(e) = self.find_entity(entity) else { return Vec::new(); };
        if e.belongs_to.iter().any(|b| b.entity == tenant) {
            return vec![Vec::new()]; // direct anchor
        }
        if !visited.insert(entity.to_string()) {
            return Vec::new(); // cycle guard
        }
        let mut found = Vec::new();
        for b in &e.belongs_to {
            for rest in self.tenant_path_chains(&b.entity, tenant, visited) {
                let mut chain = vec![JoinLink {
                    child_table: self.table_name(entity),
                    child_fk: Self::fk_column(&b.entity),
                    parent_table: self.table_name(&b.entity),
                }];
                chain.extend(rest);
                found.push(chain);
            }
        }
        visited.remove(entity);
        found
    }

    /// How many distinct `belongs_to` chains reach the tenant (0/1/≥2). The
    /// validator raises JC0545 when this is ≥2.
    pub(crate) fn tenant_path_branch_count(&self, entity: &str) -> usize {
        let Some(t) = self.tenancy.as_ref() else { return 0; };
        if entity == t.entity { return 0; }
        self.tenant_path_chains(entity, &t.entity, &mut Default::default()).len()
    }
}
```

`tenant_path` itself uses `tenant_path_chains` and returns `Some` **only when exactly one** chain exists (ambiguous ⇒ `None`, so no half-scoped code is ever generated):
```rust
let mut chains = self.tenant_path_chains(entity, &tenancy.entity, &mut Default::default());
if chains.len() != 1 { return None; }   // 0 = not owned, ≥2 = ambiguous (JC0545 blocks generation)
let joins = chains.pop().unwrap();
```

**JC0545 surfacing (authoritative — this is what keeps ambiguity from silently leaking).** `tenant_path` returns `None` on ambiguity, which would leave the entity unscoped — so the validator MUST reject the design. Grep the JC0542/JC0544 raise site (`grep -n '"JC0544"' crates/jerrycan/src/platform/*.rs`) and, in that same design-walking pass, push `JC0545` for every entity where `design.tenant_path_branch_count(&e.name) >= 2`. Generation is gated on validation, so an ambiguous design never reaches the generator. The `tenant_path_ambiguous_diamond_raises_jc0545` test guards this coupling. Keep `tenant_path` pure (no diagnostics).

- [ ] **Step 4: Register `JC0545` in `codes.rs`** (after JC0544):

```rust
CodeInfo {
    code: "JC0545",
    title: "entity reaches the tenant through more than one path",
    cause: "an entity has two or more distinct `belongs_to` chains that each reach the tenant entity (a diamond graph), so jerrycan cannot decide which chain defines tenant ownership — guessing would scope reads/writes to the wrong tenant and re-open the cross-tenant leak",
    fix: "collapse the entity's tenant ownership to a SINGLE `belongs_to` path (drop the redundant parent, or split the entity), so exactly one chain reaches the tenant",
    doc: "jerrycan docs database",
},
```

- [ ] **Step 5: Rewrite `tenant_owned` and `collect_tenant_owned` onto `tenant_path`.** `tenant_owned()` keeps its `Vec<(&str, &str)>` signature (callers keep compiling) but now includes transitively-owned entities: iterate every module/subroute's entities and include `(module_name, entity_name)` where `self.tenant_path(entity).is_some()`. Remove the direct-only `collect_tenant_owned` body (replace its `belongs_to.any` test with the resolver). Keep the recursion over subroutes.

- [ ] **Step 6: Run — expect PASS.** `cargo test -p jerrycan` (resolver tests + all existing design.rs tests, incl. `tenant_owned_walks_modules_and_subroutes`, still green).

- [ ] **Step 7: Commit** — `git add -A && git commit -m "design: transitive tenant_path resolver + JC0545 ambiguous-path error"`

---

## Task 2: Route/ownership recognition becomes transitive

**Files:**
- Modify: `crates/jerrycan/src/platform/design.rs` — `endpoint_tenant_shape` (`owns_tenant_entity`, ~line 765)
- Modify: `crates/jerrycan/src/platform/genroute.rs` — `entity_is_flat_tenant_owned` (~gating line `!e.belongs_to.iter().any…`), `entity_is_per_user_owned`, `scoped_methods` gate (~line 5 of the fn)
- Test: in-module tests in both files

**Interfaces:**
- Consumes: `Design::tenant_path` (Task 1).
- Produces: no signature changes; behavior extends to grandchildren.

- [ ] **Step 1: Write failing tests**

```rust
// genroute.rs tests — grandchild now recognized
#[test]
fn grandchild_entity_is_tenant_owned_for_scoped_methods() {
    let d = org_account_contact();
    let contact = d.find_entity("Contact").unwrap();
    assert!(!scoped_methods(contact, &d).is_empty(),
        "grandchild Contact must get scoped methods (was empty pre-#102)");
}

// design.rs tests — grandchild route is MembershipSet, not None
#[test]
fn grandchild_flat_route_is_membership_set_not_none() {
    let d = org_account_contact_flat_contacts(); // /contacts, no tenant fk in path
    let (m, ep) = d.find_route("/contacts", HttpMethod::GET);
    assert_eq!(d.endpoint_tenant_shape(m, ep), TenantShape::MembershipSet);
}

// byte-identity guard — a direct-child design's shape output is unchanged
#[test]
fn direct_child_shape_unchanged() {
    let d = direct_tenant_design(); // Club(tenant) + Book belongs_to Club
    let (m, ep) = d.find_route("/clubs/{club_id}/books", HttpMethod::GET);
    assert!(matches!(d.endpoint_tenant_shape(m, ep), TenantShape::PathScoped { .. }));
}
```

- [ ] **Step 2: Run — expect FAIL** (`grandchild_*` fail; `direct_*` passes already).

- [ ] **Step 3: Implement — swap direct predicates for `tenant_path`**

`design.rs` `endpoint_tenant_shape`:
```rust
// was: let owns_tenant_entity = module.entities.iter()
//         .any(|e| e.belongs_to.iter().any(|b| b.entity == tenancy.entity));
let owns_tenant_entity = module
    .entities
    .iter()
    .any(|e| self.tenant_path(&e.name).is_some());
```

`genroute.rs` `scoped_methods` gate:
```rust
// was: if !e.belongs_to.iter().any(|b| b.entity == tenancy.entity) { return String::new(); }
if design.tenant_path(&e.name).is_none() {
    return String::new();
}
```

`genroute.rs` `entity_is_flat_tenant_owned`:
```rust
// was: if !e.belongs_to.iter().any(|b| b.entity == tenancy.entity) { return false; }
if design.tenant_path(&e.name).is_none() {
    return false;
}
```

`genroute.rs` `entity_is_per_user_owned` — a transitively tenant-owned entity is tenant-owned, NOT per-user:
```rust
mode.auth
    && Design::has_identity_fk(e)
    && !design.tenant_path(&e.name).is_some()
// i.e. && design.tenant_path(&e.name).is_none()
```

- [ ] **Step 4: Run — expect PASS**, plus the full existing suite (`cargo test -p jerrycan`) to confirm direct apps unchanged.

- [ ] **Step 5: Commit** — `git commit -am "genroute/design: recognize transitively tenant-owned entities"`

---

## Task 3: JOIN-based scoped READ methods

**Files:**
- Modify: `crates/jerrycan/src/platform/genroute.rs` — `scoped_methods`: `all_for`/`get_for` (~1205), `all_for_memberships`/`get_for_memberships` (~1262); add SQL helpers to `TenantPath` in `design.rs`
- Test: in-module tests in `genroute.rs`

**Interfaces:**
- Consumes: `TenantPath` (Task 1).
- Produces: `TenantPath::join_sql(&self) -> String`, `TenantPath::tenant_col(&self) -> String`.

- [ ] **Step 1: Add `TenantPath` SQL helpers (design.rs)**

```rust
impl TenantPath {
    /// `JOIN account ON contact.account_id = account.id …` — empty for a direct child.
    pub(crate) fn join_sql(&self) -> String {
        self.joins
            .iter()
            .map(|j| format!(
                " JOIN {p} ON {c}.{fk} = {p}.id",
                p = j.parent_table, c = j.child_table, fk = j.child_fk,
            ))
            .collect()
    }
    /// The qualified tenant fk column, e.g. `account.org_id`.
    pub(crate) fn tenant_col(&self) -> String {
        format!("{}.{}", self.anchor_table, self.tenant_fk)
    }
}
```

- [ ] **Step 2: Write failing tests**

```rust
#[test]
fn grandchild_all_for_memberships_joins_to_tenant() {
    let d = org_account_contact();
    let src = scoped_methods(d.find_entity("Contact").unwrap(), &d);
    assert!(src.contains("JOIN accounts ON contacts.account_id = accounts.id"));
    assert!(src.contains("WHERE accounts.org_id IN (SELECT org_id FROM org_members WHERE user_id = ?)"));
}

#[test]
fn direct_child_reads_are_byte_identical() {
    let d = direct_tenant_design(); // Book belongs_to Club(tenant)
    let src = scoped_methods(d.find_entity("Book").unwrap(), &d);
    // the typed builder form is preserved exactly for direct children
    assert!(src.contains(".filter(book::Column::ClubId.eq(club_id))"));
    assert!(!src.contains(" JOIN "));
}
```

- [ ] **Step 3: Run — expect FAIL** (grandchild has no JOIN yet).

- [ ] **Step 4: Implement — branch each read on `path.joins.is_empty()`**

Compute once at the top of `scoped_methods` (after the gate): `let path = design.tenant_path(entity).expect("gate ensured Some");`.

`all_for` / `get_for` / `remove_for` / `update_for` (the path-scoped family, ~1202–1255): keep the existing **typed-builder** block verbatim when `path.joins.is_empty()`; otherwise emit raw-SQL join forms:
```rust
// all_for (transitive)
"    pub async fn all_for(&self, {fk_col}: {fk_ty}) -> Result<Vec<{entity}>> {{\n\
     \x20       {snake}::Entity::find().from_raw_sql(sea_orm::Statement::from_sql_and_values(\n\
     \x20           self.db.conn().get_database_backend(),\n\
     \x20           self.db.sql(\"SELECT {table}.* FROM {table}{join_sql} WHERE {tenant_col} = ? ORDER BY {table}.id\"),\n\
     \x20           [{fk_col}.into()],\n\
     \x20       )).all(self.db.conn()).await.map_err(db_error)\n    }}\n"
// get_for (transitive): WHERE {table}.id = ? AND {tenant_col} = ?   → .one(...)
```
where `join_sql = path.join_sql()`, `tenant_col = path.tenant_col()`, `table = path.entity_table`.

`all_for_memberships` / `get_for_memberships` (~1262–1288): when `path.joins.is_empty()`, keep today's SQL verbatim; otherwise:
```sql
-- all_for_memberships
SELECT {table}.* FROM {table}{join_sql}
WHERE {tenant_col} IN (SELECT {tenant_fk} FROM {members} WHERE user_id = ?) ORDER BY {table}.id
-- get_for_memberships
SELECT {table}.* FROM {table}{join_sql}
WHERE {table}.id = ? AND {tenant_col} IN (SELECT {tenant_fk} FROM {members} WHERE user_id = ?)
```
(`{tenant_fk}` bare inside the members subquery; `{tenant_col}` qualified in the outer WHERE.)

- [ ] **Step 5: Run — expect PASS** + full suite (direct byte-identity).

- [ ] **Step 6: Commit** — `git commit -am "genroute: JOIN-based scoped reads for transitive tenant entities"`

---

## Task 4: JOIN-based scoped WRITE methods

**Files:**
- Modify: `crates/jerrycan/src/platform/genroute.rs` — the `membership_writes` block (`create_for_memberships` ~1134, `update_for_memberships` ~1156, `remove_for_memberships` ~1180) and path-scoped `remove_for`/`update_for` (transitive branch from Task 3)
- Test: in-module tests in `genroute.rs`

**Interfaces:**
- Consumes: `TenantPath` + helpers.
- Produces: transitive write templates. Needs the **immediate parent**: `path.joins.first()` gives `child_fk` (the body's parent fk column) and `parent_table`; `path.joins[1..]` are the joins from that parent up to the anchor.

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn grandchild_create_verifies_parent_resolves_to_member_tenant() {
    let d = org_account_contact_flat_contacts();
    let src = scoped_methods(d.find_entity("Contact").unwrap(), &d);
    // WITH CHECK resolves the tenant from the BODY's account_id, not a nonexistent org_id column
    assert!(src.contains("SELECT 1 FROM accounts WHERE accounts.id = ? AND accounts.org_id IN (SELECT org_id FROM org_members WHERE user_id = ?)"));
    assert!(src.contains("let parent_fk = item.account_id"));
}

#[test]
fn grandchild_update_pins_parent_fk() {
    let d = org_account_contact_flat_contacts();
    let src = scoped_methods(d.find_entity("Contact").unwrap(), &d);
    assert!(src.contains("if item.account_id != existing.account_id"));
}

#[test]
fn grandchild_remove_deletes_via_membership_subquery() {
    let d = org_account_contact_flat_contacts();
    let src = scoped_methods(d.find_entity("Contact").unwrap(), &d);
    assert!(src.contains("DELETE FROM contacts WHERE id = ? AND id IN (SELECT contacts.id FROM contacts JOIN accounts ON contacts.account_id = accounts.id WHERE accounts.org_id IN (SELECT org_id FROM org_members WHERE user_id = ?))"));
}
```

- [ ] **Step 2: Run — expect FAIL.**

- [ ] **Step 3: Implement — transitive branches**

For transitive entities, `fk_col`/`fk_pascal` (the tenant fk) do **not** exist on the row. Introduce, in the `membership_writes` block, transitive-aware locals derived from `path`:
- `parent_fk = path.joins[0].child_fk` (e.g. `account_id`) — a real column on the entity.
- `parent_table = path.joins[0].parent_table` (e.g. `accounts`).
- `parent_joins = join_sql for path.joins[1..]` (joins from the immediate parent up to the anchor; empty for a grandchild).
- `tenant_col = path.tenant_col()`.

`create_for_memberships` (transitive) — WITH CHECK resolves the tenant from the body's parent fk:
```rust
// let parent_fk = item.{parent_fk}{parent_fk_clone};
// SELECT 1 FROM {parent_table}{parent_joins}
//   WHERE {parent_table}.id = ? AND {tenant_col} IN (SELECT {tenant_fk} FROM {members} WHERE user_id = ?) LIMIT 1
// bound values: [parent_fk.into(), user_id.into()]  (order matches the ? positions)
// on None → Err(Error::forbidden()); else insert exactly as the direct branch does.
```
Keep the same `id_capture` / `create_return_insert` transaction body (the insert itself is unchanged — only the CHECK query differs).

`update_for_memberships` (transitive) — load via `get_for_memberships` (already join-scoped in Task 3), then pin the immediate parent fk (the simplest safe rule, matching the direct "pin the tenant fk" rule; a changed parent → 403, which strictly blocks cross-tenant moves):
```rust
// let Some(existing) = self.get_for_memberships(user_id, id{pk_clone}).await? else { return Ok(false); };
// if item.{parent_fk} != existing.{parent_fk} { return Err(Error::forbidden()); }
// let m = {snake}::ActiveModel { id: Set(id), {update_sets} };  // pk pinned to path id (#92)
// match m.update(...) { … }  // identical tail to the direct branch
```

`remove_for_memberships` (transitive) — self-referential membership subquery:
```sql
DELETE FROM {table} WHERE id = ? AND id IN (
  SELECT {table}.id FROM {table}{join_sql}
  WHERE {tenant_col} IN (SELECT {tenant_fk} FROM {members} WHERE user_id = ?)
)
```
bound values `[id.into(), user_id.into()]`.

`remove_for` / `update_for` (path-scoped, transitive branch opened in Task 3): mirror the above but with `WHERE {tenant_col} = ?` (path tenant id) instead of the membership subquery. `update_for` transitive loads via the transitive `get_for` and pins the pk.

Keep every **direct** branch byte-identical.

- [ ] **Step 4: Run — expect PASS** + full suite.

- [ ] **Step 5: Commit** — `git commit -am "genroute: JOIN-based membership-checked writes for transitive tenant entities"`

---

## Task 5: JL0006 AST-based + nested-path-aware + `JL0008` fail-loud

**Files:**
- Modify: `crates/jerrycan/src/platform/lints.rs` — `scan_unscoped` (~196–243), `lint_unscoped_tenant_queries` (~146)
- Modify: `crates/jerrycan/src/platform/codes.rs` — register `JL0008`
- Modify: `crates/jerrycan/src/platform/design.rs` — add `tenant_owned_handlers(&self) -> Vec<HandlerRef>` returning the module CHAIN so the lint can locate nested files
- Test: in-module tests in `lints.rs`
- Depends on: `syn` (already a workspace dep for macros; confirm it's available to `jerrycan` — if not, add `syn = { version = "2", features = ["full", "visit"] }` to `crates/jerrycan/Cargo.toml`)

**Interfaces:**
- Consumes: `Design::tenant_owned` semantics (Task 1).
- Produces: `Design::tenant_owned_handlers() -> Vec<HandlerRef { rel_path: String, is_flat: bool, owned_desc: &'static str, leak_desc: &'static str, suggestion: String }>` where `rel_path` is the real nested handler file (`crates/routes/{top}/src/{sub}/…/handlers.rs`).

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn jl0006_fires_on_unscoped_call_in_nested_handler() {
    let root = scaffold_org_account_contact_with_unscoped_grandchild_handler();
    let diags = run_lints(&root);
    assert!(diags.iter().any(|d| d.code == "JL0006" && d.file.as_deref() == Some("crates/routes/accounts/src/contacts/handlers.rs")),
        "JL0006 must reach the NESTED grandchild handler (was silently skipped, #103)");
}

#[test]
fn jl0006_ast_ignores_repo_all_in_a_comment() {
    let root = scaffold_with_handler_body("// repo.all() is the unscoped call we must avoid\n    let x = repo.all_for_memberships(u).await?;");
    let diags = run_lints(&root);
    assert!(!diags.iter().any(|d| d.code == "JL0006"), "a mention in a comment is not a call");
}

#[test]
fn jl0006_ast_catches_multiline_chain() {
    let root = scaffold_with_handler_body("let x = repo\n        .all()\n        .await?;");
    let diags = run_lints(&root);
    assert!(diags.iter().any(|d| d.code == "JL0006"), "multi-line chain must be caught (substring scan missed it)");
}

#[test]
fn jl0008_when_tenant_owned_handler_unparseable() {
    let root = scaffold_with_handler_body("fn broken( {{{ this does not parse");
    let diags = run_lints(&root);
    assert!(diags.iter().any(|d| d.code == "JL0008"), "unparseable tenant-owned handler → loud JL0008, never a silent skip");
}
```

- [ ] **Step 2: Run — expect FAIL.**

- [ ] **Step 3: Implement `tenant_owned_handlers` (design.rs).** Walk modules + subroutes; for each module whose entities include a tenant-owned one (`tenant_path(e).is_some()`), build the on-disk handler path from the module chain the same way the scaffold nests routes (top-level dir + subroute segments). Carry `is_flat` (`entity_is_flat_tenant_owned` for any owned entity in the module) and the existing wording/suggestion strings.

- [ ] **Step 4: Rewrite `scan_unscoped` as an AST visitor.** Read the file; `syn::parse_file`. On parse/read error for a handler the design says is tenant-owned, push `JL0008` (not `return`). Walk with a `syn::visit::Visit` impl collecting `ExprMethodCall` where the receiver is (transitively) the `repo` binding and the method is one of `all`/`get`/`remove`/`update` (+`insert` when `is_flat`) with the arg-arity rule preserved (`all` takes no args; `*_for*` excluded by name). Respect the `// jerrycan:allow JL0006` hatch by checking the call's line span against the source lines. Emit `JL0006` with `file`/`line` from the span.

```rust
struct UnscopedVisitor<'a> { hits: Vec<(usize /*line*/, &'static str /*method*/)>, flag_insert: bool, src: &'a [&'a str] }
impl<'a, 'ast> syn::visit::Visit<'ast> for UnscopedVisitor<'a> {
    fn visit_expr_method_call(&mut self, c: &'ast syn::ExprMethodCall) {
        let name = c.method.to_string();
        let flagged = matches!(name.as_str(), "all" | "get" | "remove" | "update")
            || (self.flag_insert && name == "insert");
        if flagged && receiver_is_repo(&c.receiver) && !name.ends_with("_for")
           && !name.contains("_for_member") {
            let line = c.method.span().start().line;
            if !self.src.get(line - 1).map_or(false, |l| l.trim_end().ends_with("// jerrycan:allow JL0006")) {
                self.hits.push((line, leak_word(name)));
            }
        }
        syn::visit::visit_expr_method_call(self, c); // recurse for chains
    }
}
```
`receiver_is_repo`: matches `Expr::Path` named `repo`, or a `MethodCall`/`Field` chain whose base path is `repo` (covers `repo.get(...)` and simple aliases resolvable syntactically; a genuinely aliased binding falls through to no-hit — acceptable, the steering trains `repo.` usage). Keep `require full` span info via `proc-macro2` `Span::start()` (enable `proc-macro2` `span-locations`).

- [ ] **Step 5: Register `JL0008` in `codes.rs`:**
```rust
// title: "tenant-owned handler could not be scanned for scoping"
// cause: "JL0006 must read and parse each tenant-owned module's handlers.rs to verify it uses the scoped accessors, but this file is missing, unreadable, or not valid Rust — so scoping could not be checked and an unscoped cross-tenant call could pass unseen"
// fix: "ensure the handler file exists and compiles (run `cargo check`); a scaffold is generated parseable — if you hand-edited it, fix the syntax so `jerrycan check` can verify tenant scoping"
```

- [ ] **Step 6: Run — expect PASS** + full lints suite (existing JL0006 direct-module tests still green).

- [ ] **Step 7: Commit** — `git commit -am "lints: AST-based, nested-path-aware JL0006 + fail-loud JL0008"`

---

## Task 6: Isolation tests for transitive entities

**Files:**
- Modify: `crates/jerrycan/src/platform/testgen.rs` — `tenant_owned_isolation_test` (~882), `seed_second_tenant` (~835)
- Test: in-module tests in `testgen.rs`

**Interfaces:**
- Consumes: `Design::tenant_path` + `TenantPath`.
- Produces: a generated isolation test for grandchild entities that seeds the intermediate chain in tenant 1.

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn grandchild_gets_a_transitive_isolation_test() {
    let d = org_account_contact_nested(); // /accounts/{account_id}/contacts
    let src = generate_isolation_tests(&d, module_for("contacts", &d));
    // seeds an Account in tenant 1, threads its id into the nested mount, asserts user 2 → 404
    assert!(src.contains("INSERT INTO \"accounts\" (id, org_id) VALUES"));
    assert!(src.contains("/accounts/") && src.contains("/contacts/"));
    assert!(src.contains("assert_eq!(other_get.status().as_u16(), 404"));
}
```

- [ ] **Step 2: Run — expect FAIL** (grandchild currently returns `String::new()` at testgen.rs:892).

- [ ] **Step 3: Implement.** Replace the direct-only entity finder (testgen.rs:888‑894) with `design.tenant_path(&e.name)`. When `path.joins` is non-empty:
  - **Seed the chain in tenant 1.** For each intermediate parent table from the anchor down to the entity's immediate parent (`path.joins`, reversed so parents exist before children), emit a raw-SQL insert linked to tenant 1: the anchor row carries `org_id = 1`; each lower parent carries its own parent fk = the id just seeded. Use fixed ids (e.g. `1`) per table; extend `seed_second_tenant` or add a `seed_tenant1_chain` helper.
  - **Build the probe URL** by substituting the seeded immediate-parent id into the nested mount (`/accounts/1/contacts`), or, for a flat mount, put the immediate-parent fk in the create body.
  - Create the Contact as user 1, then assert user 2 (Org 2 member) gets **404** on GET and DELETE, and (flat only) the row is absent from user 2's list. Keep the direct-child path byte-identical (empty joins ⇒ old code).

- [ ] **Step 4: Run — expect PASS** + full suite.

- [ ] **Step 5: Commit** — `git commit -am "testgen: cross-tenant isolation tests for transitive (grandchild) entities"`

---

## Task 7: Conformance proof, CHANGELOG, 0.5.1 release prep

**Files:**
- Create: `conformance/` app fixture with a 3-level graph (follow the existing conformance app layout — grep an existing `conformance/*/design.json`)
- Modify: `CHANGELOG.md`
- Modify: all `Cargo.toml` `version = "0.5.0"` → `"0.5.1"` (11 crates; use the repo's existing bump method — check `scripts/` for a version script before hand-editing)

**Interfaces:** none (integration proof + release).

- [ ] **Step 1: Add a conformance app** with `Org` (tenant) → `Account` (belongs_to Org) → `Contact` (belongs_to Account), a flat `/contacts` module and a nested `/accounts/{account_id}/contacts` module. Its generated isolation test (Task 6) must exist and pass. Add a deliberately-unscoped handler variant under `#[ignore]`/a negative fixture to assert JL0006 fires (or assert via a lints unit test — reuse Task 5's).

- [ ] **Step 2: Run the conformance suite** (the `#[ignore]`d heavy tests, `--test-threads=1`, per the gate docs): the 3-level app scaffolds, `jerrycan check` is green, the isolation test is red on an unscoped handler and green on the scoped one.

Run: `cargo test -p conformance -- --ignored --test-threads=1` (confirm exact invocation from `heavy.yml`).
Expected: PASS.

- [ ] **Step 3: `cargo semver-checks`** across the workspace — expect clean (framework API additive: `TenantPath`/`JoinLink` are `pub(crate)`; only new `CodeInfo` entries are public data).

- [ ] **Step 4: CHANGELOG** — a `## 0.5.1` section under a **Security** header: deep (multi-hop) tenant graphs are now membership-scoped by construction (closes #102); JL0006 is AST-based and nested-path-aware with a new fail-loud `JL0008` (closes #103); new `JC0545` rejects ambiguous diamond ownership. Note the one behavior change: a design where an entity reached the tenant through two paths (previously generated, silently leaking) now fails `jerrycan check` with `JC0545`.

- [ ] **Step 5: Bump to 0.5.1** (all 11 crates) via the repo's version method.

- [ ] **Step 6: Commit** — `git commit -am "conformance + release: transitive tenancy (#102/#103), 0.5.1"`

- [ ] **Step 7: STOP for release.** Do NOT publish from the plan. Publishing (heavy gate + `scripts/publish.sh`) is a controller step after the whole-branch security review, per the v0.5.0 process (whole-branch review caught 2 release blockers per-task reviews missed).

---

## Self-review notes (for the executor)

- **Byte-identity is the safety net for direct apps** — after Tasks 3/4, run the full `cargo test -p jerrycan` and diff a scaffolded direct-tenant app against a 0.5.0 scaffold if in doubt. Any change to a zero-join entity's output is a bug.
- **The membership JOIN is the boundary** — do not add path-param (`account_id`) verification for nested grandchildren in this wave; it's tracked for later and is not a leak.
- **`JC0545` before `tenant_path` returns** — an ambiguous entity must resolve to `None` so no half-scoped code is generated; the validator surfaces the error.
- **Whole-branch review is mandatory before release** (Task 7 Step 7) — this is a security fix; the v0.5.0 lesson was that only the whole-branch pass sees cross-task seams.
