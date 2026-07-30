# Realtime changes: project write_only columns out of the broadcast (0.6.18) — #167

**Date:** 2026-07-30
**Status:** Approved design, pre-implementation
**Issues:** #167 (SECURITY — the proper fix for the realtime egress hole #112's REST `skip_serializing` doesn't cover: the `changes` channel delivers the RAW DB row, so a `write_only`/`password_hash` column on a `changes` entity is broadcast to every WebSocket subscriber. 0.6.8 shipped `JC0555` as an interim that REFUSES the combination by construction; this issue projects the columns out and LIFTS that restriction.)
**Ships as:** 0.6.18 — a realtime-engine projection + validation relax. Byte-identical broadcast for any entity with no write_only column (empty projection set).

## The leak + the fix point
`deliver_change` (jerrycan-realtime/src/lib.rs:526) builds the subscriber payload as `json!({ "type": op, "pk": ev.pk, "row": ev.row })`. `ev.row` carries **every column** — from the pgoutput WAL tuple decode (`changes/pgoutput.rs`, which cannot selectively decode) OR the trigger refetch (`changes/triggers.rs` `SELECT * … row_to_json`). Because the WAL path decodes all columns, the ONLY place that covers both sources is **stripping the row when building the payload**. Project there.

## A. Contract on the realtime engine: `ChangeChannelSpec.hidden_columns`
Add `pub hidden_columns: Vec<String>` to `ChangeChannelSpec` (lib.rs:41) — the column names to omit from the broadcast row (default empty = today's full-row behavior). `#[serde(default)]` / `Default` so existing construction stays valid; note the semver angle (§E).

## B. Project the row once in `deliver_change` (lib.rs:503-540)
Before the per-subscriber loop, compute the projected row ONCE (not per subscriber):
```rust
let projected_row = ev.row.as_ref().map(|r| project_row(r, &spec.hidden_columns));
```
where `project_row(row: &Value, hidden: &[String]) -> Value` returns `row` with the `hidden` keys removed when `row` is a JSON object (else `row` unchanged). Use `projected_row` in the visible payload: `json!({ "type": op_str, "pk": ev.pk, "row": projected_row })`. The delete-view payload (`json!({"type":"delete","pk":ev.pk})`) has no row — unchanged. Empty `hidden_columns` ⇒ `projected_row == ev.row` ⇒ byte-identical broadcast. The write_only value still transits the engine's own process memory (decoded from WAL / SELECTed by the trigger) but is NEVER sent to a subscriber — that is the security guarantee (#167 acceptance).

Realtime unit tests (lib.rs): the existing `secret`-delivered tests (~725-777) use an empty-`hidden_columns` spec and stay valid; ADD a test with `hidden_columns: vec!["secret".into()]` asserting the subscriber payload's `row` OMITS `secret` on insert AND update, and that every OTHER column is present.

## C. realtimegen wires the write_only column set
In realtimegen.rs (~:181, the `.changes(ChangeChannelSpec { … })` emission), populate `hidden_columns` from the entity's write_only columns: for the changes entity, collect the DB column name of every field where `Design::field_is_write_only(f)` (design.rs:1123 — includes the `password_hash` auto-hide). Emit `hidden_columns: vec!["col1".to_string(), …]` (empty vec when none — byte-identical). A no-drift/unit test: a `changes` entity with a `write_only` column emits that column in `hidden_columns`; an entity with none emits `vec![]`.

## D. Lift `JC0555` (questions.rs)
`JC0555` (questions.rs:2121-2134) refuses a `write_only`/`password_hash` column on a `changes` entity. With projection, the combination is SAFE (the column is never broadcast) — **remove the refusal**. Handle the code consistently with the codebase's convention for a retired code: remove the refusal check + the `write_only_column_on_a_changes_entity_is_refused_with_jc0555` test (questions.rs:2708). For the registry (codes.rs) — if the completeness test binds every registry code to an emission, remove the JC0555 registry entry too (JC0555 becomes a retired number; codes continue at the next free — do NOT reuse it). Add a REPLACEMENT test proving a `write_only` + `changes` design now **validates clean** (no JC0555) — the lift is the point.

## E. PG acceptance test (the security proof — use the local Postgres)
Add a heavy `#[ignore]` PG test (model on the existing realtime/changes PG tests; env `JERRYCAN_TEST_PG_URL`): scaffold a design with an entity carrying a `write_only` column (e.g. `secret`) AND a `changes` broadcast, serve it live, subscribe to `changes:{entity}`, then insert/update/delete a row and assert the subscriber receives every OTHER column but **never** `secret`. **Local run:** the container is `jerrycan-pg` on `localhost:5433` (`wal_level=logical`); reset the schema before the run (`docker exec jerrycan-pg psql -U jerrycan -d jerrycan_test -c "DROP SCHEMA public CASCADE; CREATE SCHEMA public;"`) — the PG tests assume a fresh DB. If a full live-serve WS harness is disproportionate, a unit-level proof at the `deliver_change` projection layer (already in §B) plus the realtimegen wiring test (§C) is an acceptable substitute for the heavy path — state which was used.

## E'. semver
`ChangeChannelSpec` gains a `pub` field → `constructible_struct_adds_field` may fire on `jerrycan-realtime` (the 0.6.1 precedent scope-allowed it on `jerrycan`). It IS additive (generated code always sets all fields; `#[serde(default)]`+`Default` keep existing construction compiling). If the release gate flags it, scope-allow on `crates/jerrycan-realtime` (mirroring 0.6.1) — do NOT bump to 0.7. Confirm at release-prep. (Related: #145 tracks re-enabling this lint once config structs are `#[non_exhaustive]`.)

## Gates
- **Heavy eval gate (0.6.11 lesson):** `reference_eval` + `conformance` + `eval` `--include-ignored`. Byte-identity: an entity with no write_only column emits `hidden_columns: vec![]` → identical broadcast + identical generated wiring (determinism green).
- The realtime crate's own test suite green (incl. the new projection test).

## Success criteria
- A `changes` entity with a `write_only`/`password_hash` column: every insert/update broadcasts all OTHER columns but NEVER the write_only column; a subscriber never receives it (§B unit test + §E PG proof).
- A `write_only` + `changes` design validates clean — `JC0555` no longer fires (lifted).
- An entity with no write_only column is byte-identical (broadcast + generated wiring); heavy gate green; semver additive/scope-allowed; published 0.6.18.

## Non-goals
- Column projection for the tenant-scope visibility check (unaffected — `change_visible` reads `ev.tenant_id`, not the row). Projecting at the WAL/trigger SELECT layer (the payload-build strip covers both sources with one change). Realtime broadcast/presence channels (this is the `changes` path only).
