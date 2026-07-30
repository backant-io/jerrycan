# Atomic reserve-if-capacity: document isolation + the safe cross-backend pattern (0.6.19) — #108

**Date:** 2026-07-30
**Status:** Approved design, pre-implementation
**Issues:** #108 (CORRECTNESS LANDMINE — jerrycan has no atomic reserve-if-capacity primitive and no transaction-isolation docs. An app's oversell-prevention (read capacity → insert) appears safe ONLY because jerrycan-db caps the SQLite pool at 1 connection (undocumented, lib.rs:59), accidentally serializing writes. The identical code on Postgres (pool=5) interleaves the read-then-insert and **silently oversells** — passing every SQLite test, breaking in production.)
**Ships as:** 0.6.19 — documentation of the per-backend isolation behavior + the safe atomic-reservation pattern, backed by a concurrency test (SQLite + Postgres). No codegen/API change (a type-safe codegen "capacity field" primitive is a deferred bigger follow-up — see Non-goals). The doc lives in the crate's embedded AI docs, so it ships to users.

## Root cause (verified)
`jerrycan-db` sets `max_connections`: SQLite = **1**, Postgres = **5** (lib.rs:57-61, comment "one connection for sqlite (memory correctness + writer lock)"). The single SQLite connection serializes ALL writes, so a `SELECT capacity` → (check) → `INSERT` reservation can never interleave. On Postgres's real pool, two requests both read the same remaining capacity, both pass the check, both insert → oversell. The framework's own docs (docs/ai/08-database.md) show only a bare `transaction()` and never state this, so an agent following them ships the race.

## The fix: document isolation + the atomic conditional-UPDATE pattern (both proven)

### A. Docs — a "Concurrency & atomic reservations" section in `docs/ai/08-database.md` (+ embedded twin)
Add a section (after the transactions section ~line 89), edited IDENTICALLY in `docs/ai/08-database.md` and `crates/jerrycan/embedded/ai/08-database.md` (embedded_sync gate):
1. **Per-backend isolation (state it plainly):** SQLite runs on a **single pooled connection** (pool max = 1) — every write serializes, so a read-then-write sequence is accidentally race-free. **Postgres uses a real connection pool** (concurrent writers) — a read-then-write reservation (read remaining capacity, then insert) is a **race** that can **oversell**. Code that passes every SQLite test can oversell on Postgres.
2. **The safe cross-backend pattern — one atomic conditional UPDATE.** To "reserve N of a limited resource," do NOT read-then-write. Do it in a single statement that both checks and reserves:
   ```sql
   UPDATE resource SET used = used + :n
   WHERE id = :id AND used + :n <= capacity
   ```
   Then check the affected-row count: **1 ⇒ reserved**, **0 ⇒ at capacity, reject**. A single UPDATE is atomic on BOTH backends (the row is locked for the write), so no two callers can both pass the capacity check. Show it as a runnable example using the generated repo's DB handle / sea-orm `Statement` (the raw-SQL escape hatch already documented), returning a domain 409 on the 0-row case.
3. **Optional stronger isolation (Postgres):** for a multi-row/derived-capacity reservation that a single UPDATE can't express, use `SELECT … FOR UPDATE` inside a `transaction()` to lock the capacity row(s) before computing — note it is a Postgres row lock (a no-op-but-harmless on SQLite, which already serializes). Keep this brief; the conditional UPDATE is the recommended default.
4. **Warning callout:** "A read-capacity-then-insert reservation passes every SQLite test and silently oversells on Postgres — use the atomic conditional UPDATE above."

### B. Prove it — a concurrency test (jerrycan-db)
Add a jerrycan-db test that runs N concurrent reservations against a capacity-K row and asserts EXACTLY K succeed (no oversell):
- **SQLite** (always runs): the pattern reserves at most K (serialized).
- **Postgres** (PG-gated, `JERRYCAN_TEST_PG_URL`): (i) demonstrate the **landmine** — a naive read-then-insert under concurrency oversells (asserts > K, documenting the hazard), and (ii) the **atomic conditional UPDATE** reserves exactly K (asserts == K). Spawn concurrent tasks against the real pool. This is the executable proof that the documented pattern is correct and the naive one is not.
- Local PG: container `jerrycan-pg` on `localhost:5433`; reset schema before the run (`DROP SCHEMA public CASCADE; CREATE SCHEMA public`).

### C. Document the pool sizing
State the pool sizes (SQLite 1, Postgres 5) and WHY (SQLite single-writer correctness) in the db docs, so the "undocumented pool=1" is no longer a hidden foundation of an app's correctness.

## Byte-identity / gates
- Docs + a new test only — no generated-code change; every existing design scaffolds byte-identically (determinism unaffected). embedded_sync green (twin edited identically).
- **Heavy eval gate (0.6.11 lesson):** `reference_eval` + `conformance` + `eval` `--include-ignored`. Plus the new jerrycan-db concurrency test (SQLite always; PG with the container). **Doc-test note (0.6.18 lesson):** any Rust code block in the doc that is a doc-test must compile — run `cargo test -p jerrycan --doc` and keep the example a compiling doc-test or a fenced non-test block (```text / ```sql) so `--doc` stays green.
- `cargo semver-checks` clean (no public API change).

## Success criteria
- The db docs (both twins) document the per-backend isolation, the pool sizes, and the atomic conditional-UPDATE reservation pattern with a compiling/verified example + a warning about the read-then-insert race.
- A concurrency test proves the atomic pattern reserves exactly capacity under concurrency on SQLite AND Postgres (and that the naive pattern oversells on Postgres).
- Doc-tests green (`--doc`); embedded_sync green; heavy gate green; published 0.6.19; #108 closed.

## Non-goals
- A type-safe **codegen** "capacity-constrained field / reserve endpoint" design surface that generates an atomic reserve method — the strongest fix, but a large new design capability; file as a follow-up. A generic stringly-typed `Db::reserve(table, col, …)` helper — un-jerrycan (type-erased footgun); the documented+proven pattern is the primitive here. Full serializable-isolation transaction wrappers.
