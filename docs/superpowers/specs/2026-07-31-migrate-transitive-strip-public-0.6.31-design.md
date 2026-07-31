# migrate strip_public uses the transitive tenant-owned classifier (0.6.31) — #144

**Date:** 2026-07-31
**Status:** Approved design, pre-implementation
**Issues:** #144 (the Supabase migrator's `strip_public` tenant-owned check (`migrate/mod.rs:350-352`) tests **direct** `belongs_to == tenant` only, while the framework's ownership classification is **transitive** (`Design::tenant_path`, since #102). A source table that is *transitively* tenant-owned (a grandchild) carrying a public-SELECT policy would slip past the direct check — the migrator's ownership reasoning is shallower than the framework's. Currently UNREACHABLE via the released PublicRead→`public_read` flip (#105 only flips identity-owned per-user tables, where `tenant_path` is `None`), so no shipped path hits it — a latent inconsistency, hardening not a live bug.)
**Ships as:** 0.6.31 — a `crates/jerrycan/src/platform/migrate/mod.rs` change to the `tenant_owned` test inside the CRUD-endpoint emission loop. Byte-identical for every migrate input whose tenant-owned tables are all DIRECT children (the common case); only a transitively-tenant-owned + public-SELECT source changes (its public reads are now correctly stripped instead of leaking through).

## Root cause + fix
At `migrate/mod.rs:350`:
```rust
let tenant_owned = tenant_entity
    .as_ref()
    .is_some_and(|te| entity.belongs_to.iter().any(|b| &b.entity == te));   // DIRECT only
```
The framework's `Design::tenant_path` is NOT usable here: this runs INSIDE the entity-emission loop, BEFORE the `Design` is assembled (`let mut design = Design { … }` at ~line 494). But the full detected entity set IS in scope — `entity_by_table: BTreeMap<String, Entity>` (used at ~line 308) holds every entity, and each `Entity` carries `.name` and `.belongs_to` (each `belongs_to.entity` is a target entity NAME). So replace the direct check with a **local transitive reachability walk** over the detected entities, mirroring `tenant_path`'s belongs_to-chain logic:

```rust
// #144: transitive tenant ownership — a grandchild (Contact → Account → Org)
// is tenant-owned even though it does not DIRECTLY belongs_to the tenant. Walk
// the belongs_to chain (matching the framework's Design::tenant_path) so the
// migrator's ownership view is as deep as the framework's. Cycle-guarded.
let tenant_owned = tenant_entity.as_ref().is_some_and(|te| {
    reaches_tenant(&entity.name, te, &by_name, &mut std::collections::BTreeSet::new())
});
```
with a helper (place near the strip_public loop or as a free fn in the module):
```rust
/// Does `entity` reach `tenant` by following `belongs_to` edges? A direct child
/// (belongs_to == tenant) returns true; a grandchild returns true through its
/// parent chain; an unowned table returns false. `seen` guards a belongs_to cycle.
fn reaches_tenant(
    entity: &str,
    tenant: &str,
    by_name: &std::collections::BTreeMap<String, &Entity>,
    seen: &mut std::collections::BTreeSet<String>,
) -> bool {
    if entity == tenant {
        return true;
    }
    if !seen.insert(entity.to_string()) {
        return false; // cycle
    }
    let Some(e) = by_name.get(entity) else { return false };
    e.belongs_to
        .iter()
        .any(|b| b.entity == *tenant || reaches_tenant(&b.entity, tenant, by_name, seen))
}
```
Build a **name→entity** map once (the existing `entity_by_table` is keyed by TABLE, but `belongs_to.entity` is a NAME): `let by_name: BTreeMap<String, &Entity> = entity_by_table.values().map(|e| (e.name.clone(), e)).collect();` before the emission loop (or reuse an existing name-keyed map if one already exists — grep first). Note the tenant itself is NOT tenant-owned (`entity == tenant` returns true only as the recursion base for a child's parent; the tenant table's own `entity.name == te` — make sure the tenant's OWN entity is NOT classified tenant-owned-and-stripped: guard `entity.name != te` at the call site, matching `tenant_path(tenant) == None`).

**Important — the tenant root must stay excluded.** `Design::tenant_path(tenant_entity)` is `None` (the tenant does not belong to itself), so the tenant's OWN table is NOT tenant-owned. Preserve that: the walk's base `entity == tenant` returns true, which would wrongly mark the tenant table itself as tenant-owned. Gate the call so the tenant's own entity is skipped (e.g. `tenant_entity != Some(&entity.name) && reaches_tenant(...)`), matching `tenant_path`'s `None`-for-root.

## Tests (migrate tests — mirror the existing strip_public / tenancy migrate tests)
1. **Direct child unchanged (byte-identity witness):** a source with a tenant + a DIRECT child carrying a public-SELECT policy still strips public + emits the advisory exactly as before.
2. **Transitive grandchild now stripped (the #144 fix):** a source `Org (tenant) ← Account ← Contact`, where `Contact` (a grandchild) carries a public-SELECT policy → `strip_public` fires (public reads removed) + the RlsPolicy advisory is emitted, where the direct-only check would have LEFT it public. Assert the grandchild's endpoints are no longer public.
3. **Tenant root not stripped:** the tenant's OWN table with a public policy is NOT classified tenant-owned by this walk (matches `tenant_path(tenant) == None`) — its handling is unchanged.
4. **Unowned table unchanged:** a non-tenant-owned table keeps its public reads.

## Gates
- `cargo test -p jerrycan` (migrate tests) green.
- **Heavy eval gate (0.6.11 lesson):** `reference_eval` + `conformance` + `eval` `--include-ignored` — migrate is exercised by the migrate_e2e capstone; confirm it stays green (the migrator's Design output for existing fixtures is unchanged — only a transitively-tenant-owned + public source differs). NOTE: conformance can flake on #118 shared-target — re-run a suspicious unrelated failure alone.
- `cargo fmt`/`clippy -D warnings`; byte-identity (`determinism.rs`) — the generated framework code is unchanged; only the migrator's endpoint access for a transitively-tenant-owned public source changes.

## Success criteria
- `strip_public`'s tenant-owned test is transitive (matches `Design::tenant_path`): a transitively-tenant-owned source with a public-SELECT policy has its public reads stripped + an advisory emitted, exactly as a direct child does; the tenant root and unowned tables are unchanged.
- Byte-identical for direct-child / unowned inputs; heavy gate + migrate_e2e green; published 0.6.31; #144 closed.

## Non-goals
- Building the `Design` earlier to call `tenant_path` directly (the local walk is simpler and avoids reordering the migrate pipeline). Changing the framework's `tenant_path` or the PublicRead flip. The reverse-map follow-up noted in #115's memory (separate).
