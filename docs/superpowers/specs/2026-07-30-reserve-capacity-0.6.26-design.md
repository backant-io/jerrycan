# Type-safe atomic reserve-if-capacity primitive (0.6.26) — #187

**Date:** 2026-07-30
**Status:** Approved design, pre-implementation
**Issues:** #187 (follow-up to #108 — the stronger, make-impossible version. 0.6.19 shipped the docs + the proven atomic conditional-UPDATE pattern for "reserve N of a limited resource" (seats/stock/credits), but the agent still HAND-WRITES the atomic `UPDATE … WHERE used + n <= capacity`. A hand-written reservation is exactly the footgun #108 is about — a slip back to read-then-write silently oversells on Postgres. A generated, type-safe reserve method removes the footgun by construction.)
**Ships as:** 0.6.26 — an additive codegen feature. A new opt-in field declaration `reserve_against` generates a `{Entity}Repo::reserve` method emitting the #108-proven atomic UPDATE. **Byte-identical for every design that does not declare `reserve_against`** (no method emitted, no schema change).

## The design surface
A field declares `reserve_against: "<capacity_field>"` — it is the *counter* that increments, and the named field is the *capacity* ceiling:
```json
{
  "name": "bookings",
  "fields": [
    { "name": "capacity", "type": "integer" },
    { "name": "used", "type": "integer", "default": 0, "reserve_against": "capacity" }
  ]
}
```
Both are ordinary integer fields (normal DTO / DB columns); `reserve_against` only WIRES the generated method. `used` here carries `default: 0` (server-owned initial counter) — that is idiomatic but NOT required by this feature.

## The generated method (genroute.rs `sql_repo`, ~2469)
When an entity has exactly one field `f` with `f.reserve_against = Some(cap)`, emit on `{Entity}Repo` (SQL-backed repo only) an atomic reserve method. Mirror the EXACT raw-SQL idiom already used by the #138 member methods in this same file (`self.db.conn()`, `self.db.sql(...)`, `db_error`, `.rows_affected()`) and the worked example in `docs/ai/08-database.md:179-198`:
```rust
/// Atomically reserve `n` units of `{used}` against `{capacity}` in ONE
/// conditional UPDATE — correct on SQLite AND Postgres (all callers contend on
/// the SAME pk row, so the row lock + WHERE guard serialize them; no oversell).
/// `Ok(true)` ⇒ reserved; `Ok(false)` ⇒ at capacity, or no such row.
pub async fn reserve(&self, id: {key}, n: i64) -> Result<bool> {
    let stmt = jerrycan::db::sea_orm::Statement::from_sql_and_values(
        self.db.conn().get_database_backend(),
        self.db.sql("UPDATE {table} SET {used} = {used} + ? WHERE id = ? AND {used} + ? <= {capacity}"),
        [n.into(), id.into(), n.into()],
    );
    Ok(self.db.conn().execute(stmt).await.map_err(db_error)?.rows_affected() == 1)
}
```
- `{table}` = the entity's table name; `{used}` / `{capacity}` = the snake_case column names; `{key}` = the pk type (mirror how `insert`/`get` derive these in `sql_repo`). Quote any column name exactly as the existing methods quote `"key"` (the two column names here are user-chosen — reuse whatever quoting helper the file already applies to columns).
- **Confirm the exact in-scope names** (`self.db`, `db_error`, the `Statement`/`sql` path) by reading the #138 `remove_member`/`set_member_role` raw-SQL emission in `genroute.rs` and matching it — do NOT introduce a new import path. If the generated repo already imports `Statement`, use the short form; otherwise fully-qualify as above.
- **Method name `reserve`** (matches the issue). Exactly ONE `reserve_against` field per entity is allowed (JC0564 refuses >1), so the name is unambiguous. Do NOT emit a `release`/`refund` counterpart (YAGNI — not asked).
- Emitted ONLY for a SQL-backed entity (the `sql_repo` path). The memory-backed repo (`memory_repo_rs`) does NOT get it (JC0564 refuses `reserve_against` when the design has no `db` mode — see validation).

## Validation — JC0564 (design.rs `validate`, codes.rs)
Next free code is **JC0564** (JC0563 is the last used). Refuse, with a message naming the entity + field + the exact rule, when:
1. `reserve_against` names a field that does NOT exist on the same entity.
2. The field carrying `reserve_against` is not `integer` type.
3. The named capacity field is not `integer` type.
4. Either the counter field or the capacity field is the pk `id`.
5. `reserve_against` equals the field's own name (cannot reserve against itself).
6. More than one field on the entity carries `reserve_against`.
7. The design has no database (`reserve_against` requires a DB — refuse on a memory-only design / memory-backed entity).

Register JC0564 in `codes.rs` (mirror the JC0563 entry) with an `explain` string that names the rule, and add a WHY test in the codes.rs test module (mirror the JC0563 WHY test) asserting `jerrycan explain JC0564` names the reserve/capacity integer-fields rule.

## Tests
- **genroute emission (unit/golden):** an entity with `reserve_against` emits the `reserve` method whose body contains the exact atomic guard `SET {used} = {used} + ? WHERE id = ? AND {used} + ? <= {capacity}`; an entity WITHOUT `reserve_against` emits NO `reserve` method (byte-identity witness).
- **design validation (units):** each of the 7 JC0564 refusal cases fires; a well-formed `reserve_against` passes clean.
- **Race-safety proof (the #187 point).** The generated SQL is byte-identical to the #108-proven atomic UPDATE, whose no-oversell property is already proven by the existing Postgres concurrency test (jerrycan-db / the #108 spec 2026-07-30-atomic-reserve-0.6.19). To bind the GENERATED method to that proof, add a `reserve_against` counter+capacity pair to a reference/conformance fixture entity so the generated `reserve` compiles and is exercised by the heavy eval gate; then EITHER (preferred) add a focused PG-gated concurrency test (model on `crates/jerrycan/tests/last_admin_concurrency.rs`) that drives the generated method — fire K concurrent `reserve(id, 1)` on `capacity = C < K`, assert exactly C return `Ok(true)`, the rest `Ok(false)`, and the final `used == C` (never exceeds) — OR, if wiring a live generated app in-test is disproportionate, state that explicitly and rest the race-safety on the golden SQL-equivalence + the existing #108 PG test. Prefer the direct concurrency test if it is not disproportionate.
- Local PG container `jerrycan-pg` at `localhost:5433` (`wal_level=logical`) is available; reset schema first (`docker exec jerrycan-pg psql -U jerrycan -d jerrycan_test -c "DROP SCHEMA public CASCADE; CREATE SCHEMA public;"`).

## Docs (byte-identical twins)
In `docs/ai/08-database.md` AND its embedded twin `crates/jerrycan/embedded/ai/08-database.md`, under "Concurrency & atomic reservations", add a short subsection: the `reserve_against` field declaration generates a `{Entity}Repo::reserve(id, n) -> Result<bool>` method emitting the atomic UPDATE for you — prefer it over hand-writing the pattern. Keep the existing hand-written pattern (it still covers multi-row/derived-capacity cases the primitive does not). The two files must stay byte-identical (embedded_sync gate). If a doc-example adds a field to a struct used in a `--doc` example, update BOTH twins (0.6.18 lesson: `--doc` E0063 is CI-only).

## Gates
- `cargo test -p jerrycan` (genroute + validation + codes) green.
- **Heavy eval gate (0.6.11 lesson):** `reference_eval` + `conformance` + `eval` `--include-ignored` — the fixture now carries a `reserve_against` field, so the generated `reserve` method must compile and the batteries stay green.
- `cargo test --doc` (the 08-database doc example) green.
- `cargo fmt` / `clippy -D warnings`; `cargo semver-checks` (additive `Field` field + generated method — no breaking change; the new `Field.reserve_against` is serde-defaulted + skipped-when-none so every existing design round-trips byte-identically).

## Success criteria
- A design can declare `reserve_against` on an integer field; the generated `{Entity}Repo::reserve(id, n)` performs an atomic reserve-if-capacity (Ok(true) reserved / Ok(false) at-capacity) that does NOT oversell on Postgres — proven against the concurrency test (or documented equivalence to the #108 PG proof).
- JC0564 refuses all 7 malformed shapes with an actionable message.
- Designs without `reserve_against` are byte-identical; heavy gate + doc-test green; published 0.6.26; #187 closed.

## Non-goals
- A `release`/`refund` method, multi-field/entity-level capacity, or derived (multi-row) capacity (the hand-written `SELECT … FOR UPDATE` pattern already covers that — stays documented). More than one `reserve_against` field per entity. Any change to the request DTO / OpenAPI (the counter + capacity are ordinary fields; `reserve` is a repo method, not an HTTP route).
