# Composite / multi-column UNIQUE (0.6.13) — #115

**Date:** 2026-07-26
**Status:** Approved design, pre-implementation
**Issues:** #115 (MEDIUM — `Field.unique` is per-field only; there is no way to declare a composite `UNIQUE(a, b)`, so a "one row per (a,b)" invariant — a like per (user, post), an enrollment per (user, course) — can't be expressed. Builders are forced into a racy SELECT-then-INSERT with a TOCTOU window and no DB backstop. Hit by 4 round-5 eval apps: feed, jobs, events, lms.)
**Ships as:** 0.6.13 — an additive design-contract feature. Byte-identical for every entity that declares no composite `unique` (serde-default empty, skipped on serialize).

## The 409 is already free
`db_error` (jerrycan-db/src/lib.rs:272-279) maps ANY unique-constraint violation to `Error::conflict` → **409 JC0409** at runtime. So a composite UNIQUE index needs NO new runtime/error code — a duplicate `(a,b)` insert already surfaces as 409. The precedent is the membership table's `UNIQUE(user_id, fk)`, rendered as a standalone `CREATE UNIQUE INDEX` (genroute.rs:2705-2745).

## A. Contract: `unique` groups on the entity
Add to `Entity` (design.rs:145-166, which is `#[serde(deny_unknown_fields)]` — so the field MUST be declared):
```rust
/// Table-level composite UNIQUE constraints (issue #115): each inner vec is one
/// `UNIQUE(col, …)` over ≥2 columns, so a "one row per (a,b)" invariant is a DB
/// constraint (a duplicate is 409, no TOCTOU) instead of a racy SELECT-then-INSERT.
/// Each column is a field name OR a `belongs_to` fk column. Single-column
/// uniqueness stays `Field.unique`. Serde-default empty + skipped so every existing
/// design round-trips byte-identically.
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub unique: Vec<Vec<String>>,
```
Mirror in `docs/contracts/design-schema.json` (pinned by `tests/contracts.rs`) — an array of arrays of strings, default `[]`.

## B. Migration DDL — one `CREATE UNIQUE INDEX` per group
In the per-entity table/index emission (genroute.rs, the block that builds each entity's `Table::create()` + its single-column `indexes`, just above the membership block ~2700), after the entity's own table is pushed, emit for each group in `e.unique`:
```rust
let mut uniq = Index::create();
uniq.unique()
    .name(format!("idx_{table}_{}", cols.join("_")))   // deterministic, collision-free per table+cols
    .table(Alias::new(table.clone()));
for col in group { uniq.col(Alias::new(col.clone())); }
out.push_str(&schema_sql(&uniq, backend_is_pg));
out.push_str(";\n\n");
```
Mirror the membership composite-index emission verbatim (genroute.rs:2738-2745) — same `Index::create().unique().name().table().col()…` → `schema_sql` path, so it renders correctly on both SQLite and Postgres. The group's columns are used as-authored (they are real column names — a field column or a `belongs_to` fk column, both of which already exist in the table DDL). Deterministic order (author order); no sorting.

## C. Validation — `JC0559` (next free after JC0558)
In the per-entity validation loop (questions.rs), register **JC0559** (codes.rs + `explain` + completeness test, mirroring the JC0558 precedent) and refuse a composite `unique` group that is unbuildable:
- a group with **< 2 columns** — a single-column unique must use `Field.unique` (a 1-col group is a footgun / duplicates the field flag); refuse with the fork.
- a column that is **neither a declared field nor a `belongs_to` fk column** of the entity (`e.fields` names ∪ `{ Design::fk_column(&b.entity) for b in e.belongs_to }`) — a typo'd/absent column would emit a migration that fails at apply; refuse loud at `check`.
- a **duplicate group** (same column set, order-insensitive) — redundant, refuse.
Message references `jerrycan explain JC0559`, matching the coded-question convention.

## D. testgen — a composite-unique 409 conflict test
Emit `{entity}_{cols}_composite_unique_conflict_is_409` for each group on an entity whose create endpoint exists: create a first row, then POST a second row that **shares the group's column values but differs in pk** (and in every OTHER unique/unique-field column, so ONLY the composite constraint trips) → assert **409**. Model on the membership duplicate-add 409 pattern (genroute.rs "duplicate adds (409)"; the last-admin 409 tests in testgen.rs:1927-1936 show the assert shape). Respect the #85 seed nuance: the two create bodies must be identical on the group columns and distinct on the pk. For a group over `belongs_to` fk columns, seed the referenced parent rows first (reuse the tenant/parent seed helpers). Counted toward `expected_failing` only if it is RED on stubs (a correct stub inserting both rows makes the 2nd 409 via the DB constraint — so it PASSES once the handler calls `insert`; classify like the existing unique-field probe at testgen.rs:1955-1969).

## E. OpenAPI — document the 409 on create
For an entity with ≥1 composite `unique` group, add a `"409"` response to its create (POST) operation: `"409": { "description": "a row with the same (col, …) already exists" }`. Mirror the member-surface 409 doc (openapi.rs:364). Reads/updates unaffected.

## F. Docs + byte-identity
- Document `unique: [["a","b"]]` on an entity in the field/entity reference (`docs/ai/` — find via `grep -rln '"unique"' docs/ai/`) + the embedded twin (embedded_sync gate — edit BOTH identically): a table-level composite UNIQUE = a DB-enforced "one row per (a,…)", a duplicate is 409; each column is a field or a belongs_to fk; single-column uniqueness stays `field.unique`.
- Byte-identity: an entity with empty `unique` emits no index, no test, no 409, no validation — byte-identical. Prove via `determinism.rs` + base-vs-HEAD scaffold `diff -r` on a conformance app.
- **Heavy eval gate (0.6.11 lesson):** run `reference_eval` + `conformance` + `eval` `--include-ignored` before done — the per-PR gate `#[ignore]`s them.
- Add a conformance/unit fixture with a composite-unique entity (e.g. a `Like { user_id, post_id }` with `unique: [["user_id","post_id"]]`) proving: the migration has `CREATE UNIQUE INDEX … (user_id, post_id)`, the 409 test is emitted, and a scaffolded correct handler passes `jerrycan check`.

## Success criteria
- An entity with `unique: [["user_id","post_id"]]`: the migration emits `CREATE UNIQUE INDEX … ON likes (user_id, post_id)`; a duplicate `(user_id, post_id)` insert is **409**; the generated acceptance suite includes the composite-unique 409 test; OpenAPI documents the 409.
- A bad group (<2 cols, unknown column, duplicate) → **JC0559**.
- An entity with no composite `unique` is byte-identical; `cargo semver-checks` clean (Entity gains a serde-default field — additive; confirm no lint); heavy gate green; published 0.6.13.

## Non-goals
- Partial/`WHERE`-filtered unique indexes; `NULLS NOT DISTINCT`; deferrable constraints. Composite unique spanning a non-fk related table. A composite that duplicates an existing single-column `field.unique` (the <2-col refusal already steers there).
