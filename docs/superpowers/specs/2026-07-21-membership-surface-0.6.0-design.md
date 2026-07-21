# Membership-management surface (0.6.0) — #107

**Date:** 2026-07-21
**Status:** Approved design, pre-implementation
**Issues:** #107 (no add/remove/list-member surface — every tenancy app hand-seeds `{tenant}_members` via raw SQL)
**Origin:** round-5 eval (every many-membership app), plus the #107 investigation map.
**Ships as:** 0.6.0 — first minor of the 0.6 line; a new generated capability. Additive (existing routes unchanged; framework Rust API additive → `cargo semver-checks` clean) but it adds routes to every tenancy app's output, so an honest minor bump.
**Unblocks:** #132 (storage `write_roles` — a non-first-role member is now creatable), #106 (Supabase migrator maps the source membership table onto this surface for free), realtime roles.

## Problem
The `{tenant}_members` table (`user_id`, tenant fk, `role`) and the creator auto-seed
(`create_with_membership`, genroute.rs:1730-1822) exist, but there is **no generated way to
invite/remove/list members or change a role** — so every tenancy app hand-writes raw
`INSERT INTO {tenant}_members …`. The framework's own testgen/storagegen do the same
(testgen.rs:825, storagegen.rs:354), and they even seed an **inconsistent** fallback role
(`"owner"`) vs genroute's (`"member"`).

## Design

### A. A fully-generated, tool-owned member surface (correct-by-construction)
When `design.tenancy.is_some()`, generate a complete, **real-implementation** (not agent-stub)
member surface, mounted **path-scoped under the tenant module** so the existing `Dep<Tenant>`
guard (404s non-members) applies:

Routes (`{tenant}` = tenant module mount, `{fk}` = tenant fk param):
- `GET  /{tenant}/{fk}/members` — list members `[{id, user_id, role}]`. Any member (the guard suffices).
- `POST /{tenant}/{fk}/members` — add `{user_id, role}`. **Admin-gated** (`require_role(member_roles[0])`).
- `PATCH /{tenant}/{fk}/members/{user_id}` — set `{role}`. Admin-gated.
- `DELETE /{tenant}/{fk}/members/{user_id}` — remove. Admin-gated, **except self-removal** (a member may remove themselves = "leave").

Repo methods next to `create_with_membership`/`all_for_member` (genroute.rs:1730-1822), real SQL
against `{tenant}_members`, scoped to the path tenant fk (verified by the guard):
`members_of(tenant_fk)`, `add_member(tenant_fk, user_id, role)`, `set_member_role(tenant_fk, user_id, role)`,
`remove_member(tenant_fk, user_id)`, plus a `count_admins(tenant_fk)` helper for the last-admin check.
A duplicate add surfaces the `UNIQUE(user_id, fk)` index as **409** via `db_error` (jerrycan-db).

**Generation placement:** emit the handlers fully-implemented (tool-owned, like storagegen) within
the tenant module's routes crate, register the routes in `mounting.rs`, and **add them to the
generated OpenAPI** (unlike storage — these are first-class tenant routes and should be discoverable).
Gate every emitter on `design.tenancy.is_some()` so non-tenancy apps are byte-identical.

### B. Runtime authorization + integrity rules
- **Write role gate:** POST/PATCH/DELETE (non-self) call `tenant.require_role(member_roles[0])` (the admin role by convention — position 0). Reads need only membership.
- **`role ∈ member_roles`** on add/set-role → **422** (no DB CHECK backs the column; validate in the handler).
- **Last-admin lockout:** `remove_member`/`set_member_role` must refuse the operation if it would leave the tenant with **zero** members holding `member_roles[0]` → **409** (`count_admins` == 1 and the target is the last admin). Prevents locking everyone out.
- **Self-removal:** DELETE `{user_id}` where `user_id == caller` is allowed without the admin role (subject to the last-admin rule).
- **`user_id` is opaque** — no FK to a user table exists (migrated-uuid support); add-member accepts any string id and documents that existence isn't DB-verified.

### C. Contract validation + fallback consistency (`JC0548`)
- New design-time error **`JC0548`**: when `tenancy` is present, `member_roles` must be **non-empty and duplicate-free** (today it may be empty/duplicated, silently falling back to a role name). Raise `JC0548` from the tenancy validation block (questions.rs, beside JC0540). This makes `member_roles[0]` a reliable admin role.
- **Standardize the empty-role fallback:** with `JC0548` guaranteeing non-empty `member_roles`, remove the divergent fallbacks — genroute (`"member"`), testgen (`"owner"`, testgen.rs:844), storagegen (`"owner"`, storagegen.rs:375) all use `member_roles[0]` unconditionally.

### D. Tests + docs
- testgen: generate acceptance tests for the member surface — list (member sees the roster), add (admin adds a `member_roles[1]`-role user; a non-admin add is 403), set-role, remove, the **last-admin 409**, self-removal, and `role ∉ member_roles` 422.
- docs: `docs/ai/14-tenancy.md` ↔ `crates/jerrycan/embedded/ai/14-tenancy.md` (embedded_sync gate) — document the member surface + the admin-role convention; update the repo-method list (embedded 14-tenancy.md:78-84).
- Update the conformance tenancy fixture (`reference-slice.design.json`) goldens where the new routes/OpenAPI/tests legitimately appear.

## Non-goals / documented gaps
- **No live-socket revocation:** removing/re-roling a member does not disconnect or re-scope an already-connected realtime socket (`Principal` resolved once at connect) — documented; a proper fix rides the 0.6.x realtime rework (#104).
- No `invited_by`/`created_at` columns (create-once migrations only reach new apps; list response stays `{id,user_id,role}`) — a later additive column.
- Role hierarchy: `require_role` is exact-match single-role (guard.rs:42-48); "owner OR admin may invite" isn't expressible. The admin gate uses `member_roles[0]` only. A `require_any_role` extension is tracked, not in scope.
- The guard's arbitrary-first-membership fallback on no-fk-in-path routes (scaffold.rs:122-139) is unchanged (a storage/realtime concern) — member routes are path-scoped so unaffected.

## Success criteria
- A tenancy app generates a working member surface: an admin lists/adds/removes/re-roles members over HTTP (no raw SQL); a non-admin can list + self-leave but not manage others; removing the last admin is 409; an out-of-set role is 422.
- A design with empty/duplicated `member_roles` fails `check` with `JC0548`.
- Non-tenancy apps are byte-identical; the generated OpenAPI includes the member routes; `cargo semver-checks` clean; heavy gate green; published 0.6.0.
