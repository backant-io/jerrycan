# Atomic last-admin guard (0.6.24) — #138

**Date:** 2026-07-30
**Status:** Approved design, pre-implementation
**Issues:** #138 (the 0.6.0 member surface #107 blocks removing/demoting the sole admin via a `count_admins` READ followed by a SEPARATE DELETE/UPDATE (genroute.rs `set_member_role`:2203-2221, `remove_member`:2235-2249). Two concurrent admin-gated writes on one tenant can BOTH pass the check and leave the tenant with **zero admins** (member-management locked out). A plain transaction is insufficient — two READ-COMMITTED / deferred-SQLite txns both read `count==2` before either writes.)
**Ships as:** 0.6.24 — a codegen change to the generated membership repo methods (atomic check-and-act). Same 409/404 behavior; the fix is that the guard is now race-free. Byte-identical for any non-tenancy design (no member surface emitted).

## The fix: a single conditional statement, atomic per-statement on both backends
Replace the read-then-write in the two generated repo methods with ONE conditional write whose WHERE clause carries the last-admin guard, so the DB row-locks the write and no two callers can both pass the check.

### A. `remove_member` (genroute.rs:2224-2250)
```sql
DELETE FROM {members} WHERE user_id = ? AND {fk_col} = ?
  AND NOT (role = '{admin}'
           AND (SELECT COUNT(*) FROM {members} WHERE {fk_col} = ? AND role = '{admin}') <= 1)
```
- `rows_affected == 1` ⇒ removed ⇒ `Ok(true)`.
- `rows_affected == 0` ⇒ EITHER the target is the last admin (the guard blocked it — **409**) OR the member does not exist (**`Ok(false)` → 404**). These must NOT be conflated (a non-member remove must stay 404, not become a spurious 409). Disambiguate with ONE follow-up existence SELECT on the 0-path only (rare): if the row `(user_id, fk)` still EXISTS ⇒ it was the last admin ⇒ `Err(Error::conflict("cannot remove the last {admin}"))`; else ⇒ `Ok(false)`. The atomicity (the security fix) is entirely in the conditional DELETE; the follow-up read only picks the status code and has no correctness impact (the DELETE already protected the last admin).

### B. `set_member_role` (demote) (genroute.rs:2183-2222)
Keep the role-validation 422 (MEMBER_ROLES check) FIRST. Then ONE conditional UPDATE:
```sql
UPDATE {members} SET role = ? WHERE user_id = ? AND {fk_col} = ?
  AND NOT (role = '{admin}' AND ? <> '{admin}'
           AND (SELECT COUNT(*) FROM {members} WHERE {fk_col} = ? AND role = '{admin}') <= 1)
```
bind order: `[new_role (SET), user_id, fk, new_role (the `?<>'{admin}'` compare), fk (COUNT)]`. The `role = '{admin}'` reads the row's CURRENT role; `? <> '{admin}'` is the new role, so **re-affirming** the last admin's admin role (new == admin) makes `? <> '{admin}'` false ⇒ `NOT(...)` true ⇒ the UPDATE proceeds (a no-op role write, `rows_affected == 1` ⇒ `Ok(true)` — preserving the #107 "re-affirm the last admin stays" behavior). A genuine demote of the last admin ⇒ blocked ⇒ `rows_affected == 0`. Same 0-path disambiguation as §A: follow-up existence SELECT ⇒ 409 if the row exists (last-admin demote) else `Ok(false)`.

### C. count_admins
`count_admins` (genroute.rs:5897-region helper) is now used ONLY inside the conditional SQL's subquery (inlined), not as a separate pre-read. If the standalone `count_admins` method becomes unused after this change, remove it (and its test) OR keep it if still referenced elsewhere — verify with a grep. Do not leave a dead method that `-D warnings` flags.

## D. Behavior parity + tests
- The existing last-admin 409 acceptance/unit tests (`remove_{snake}_member_last_admin_is_409`, `set_{snake}_member_role_last_admin_demotion_is_409`, and the "re-affirm the last admin stays" test) MUST still pass — the observable behavior (409 on the sole-admin remove/demote, re-affirm allowed, normal remove/demote succeed) is unchanged. Update the count_admins-helper test if the method is removed.
- **Concurrency test (the #138 proof, PG-gated):** on a tenant with EXACTLY 2 admins, fire two concurrent `remove_member` (and separately two concurrent demote `set_member_role`) against the two admins; assert that AFTER both complete, the tenant still has **≥ 1 admin** (was: both succeed → 0 admins). Exactly one write wins; the other gets 409 (or a safe no-op). Model on the #108 concurrency test; local PG container `jerrycan-pg` at `localhost:5433` (reset schema first: `DROP SCHEMA public CASCADE; CREATE SCHEMA public`). If a full live-serve harness is disproportionate, a jerrycan-db-level test constructing the members table + two concurrent conditional statements suffices — state which.

## E. Byte-identity + gates
- Only the generated membership repo methods (`remove_member`/`set_member_role`) change; a non-tenancy design emits no member surface → byte-identical. A tenancy design's member methods change (that IS the fix) — update the genroute member-method golden/unit tests accordingly.
- **Heavy eval gate (0.6.11 lesson):** `reference_eval` + `conformance` + `eval` `--include-ignored` (reference-slice has a tenancy → its member surface changes; confirm the battery + the #107 member acceptance tests stay green).
- `cargo fmt`/`clippy -D warnings`; `cargo semver-checks` clean (internal codegen).

## Success criteria
- `remove_member`/`set_member_role` use a single atomic conditional statement; two concurrent admin-gated writes on one tenant can NEVER reach zero admins (concurrency test proves it on PG).
- The sole-admin remove/demote still 409s; re-affirming the last admin still succeeds; a normal remove/demote/ non-member 404 are unchanged.
- Non-tenancy designs byte-identical; heavy gate + #107 member tests green; published 0.6.24; #138 closed.

## Non-goals
- Changing the member-surface API or the 422 role validation. A general transaction-isolation escalation (the conditional statement is sufficient and cheaper). The last-admin rule's semantics (still: the sole admin can't be removed or demoted).
