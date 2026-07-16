# Eval round-2 fixes (issues #35, #42–#53) — Implementation Plan

**Goal:** Fix everything the 2026-07-16 face-off confirmed (issues #47–#53), the round-1 review follow-ups (#42–#46), and the deferred token-weight work (#35). Maintainer pre-approved all fix directions ("I accept all proposals").

**Architecture:** Seven sequential packages, each its own branch/PR off green `main`, ordered by agent-impact ÷ effort (the face-off audit's ranking). Sequential because most touch `testgen.rs`/`genroute.rs`/docs and would conflict in parallel.

## Global Constraints (bind every package)

- Commits authored by the repo's git user (Pavel Hegler); NO Co-Authored-By/Claude/AI mentions; plain messages; body references the issue(s) ("Fixes #N" — one `fixes` keyword PER issue, GitHub only auto-closes the first otherwise).
- **embedded_sync**: `docs/ai/*.md` edits copied byte-identical to `crates/jerrycan/embedded/ai/`; `docs/SKILL.md` to `.claude/skills/jerrycan-backend/SKILL.md`.
- **Semver**: before committing, run the gate's exact check: `cargo semver-checks check-release -p jerrycan-core -p jerrycan-macros -p jerrycan-db -p jerrycan-auth -p jerrycan-validate -p jerrycan-observe -p jerrycan` — clean at 0.4.0 vs published 0.3.0. (`jerrycan-jobs`/`-realtime`/`-storage`/`-ratelimit` are unpublished — pub-API additions there are unchecked and fine.)
- **No-drift**: generator changes must keep output for designs NOT matching the new rule byte-identical (P4-round-1 method: scaffold both conformance designs with base+branch binaries, `diff -r`).
- Full `cargo test -p jerrycan` green; fmt/clippy via pre-commit hook; TDD with RED/GREEN evidence in the report.
- STOP after local commit — controller reviews, PRs, and merges on verified-green gate.

---

## R1 — #47: enum `values` must reject out-of-range input with 422, not 500

**Decision:** implement what the docs already promise — the generator emits validation for `values` fields so invalid enum input 422s BEFORE the DB. Do NOT merely remap CHECK violations in `db_error` (a CHECK violation can have other causes; and memory-mode has no DB at all).
**Anchors:** `db_error` special-cases only unique-violations (`jerrycan-db/src/lib.rs:264-277`); docs promise at `docs/ai/00-designing.md:159-160` and `:574-575`; testgen fixture enum-awareness `testgen.rs:13-16`.
**Scope:** genroute emission for db AND memory modes (both must 4xx); works whether or not the app enables the `validate` dependency (if the mechanism needs `validate`, the generator emits a plain match/allow-list check instead — zero new deps for the app). Update docs lines to state exactly what is generated. Testgen: ensure the "wrong enum value → 4xx" generated branch now passes (and the single-value-enum unreachable-branch note in SKILL, if still present, stays true).
**Acceptance:** new fixture matrix (db-mode + memory-mode, enum field): out-of-range POST → 422 in generated acceptance tests; e2e-lite scaffold proves it live; no-drift proof for enum-free designs; docs no longer promise anything unimplemented.

## R2 — #48 + #51: testgen truthfulness (format-aware fixtures + entity-aware seeding)

**Decision #48:** two-pronged: (a) if the design declares a recognized format for a field, derive the happy-path fixture from it (email→`user@example.com`, url→`https://example.com/x`, date→`2026-01-01`, uuid→a fixed valid v4); scout what format declarations the design contract actually supports and cover those; (b) for HAND-WRITTEN `Valid` impls the generator cannot see: document the interaction in the un-greenable-probes box (00-designing.md:198-201/231-238 region) and in SKILL Phase 5 — naming `probe:"skip"` as the remedy — so no agent re-discovers it from source.
**Decision #51:** seed each `/{id}` probe via ITS OWN entity's creator: walk the `belongs_to` chain, create required parents first, then the entity itself; only fall back to the module-root creator when the entity has no creator route (then emit the existing AGENT-TODO comment instead of a guaranteed-red probe).
**Anchors:** fixture `testgen.rs:13-26`; root-creator-only seeding `testgen.rs:71-76,181-204,252-258`.
**Acceptance:** J3's exact shape (module with root entity + second entity with own `/{id}` routes) generates GREEN update/delete probes on a correct scaffold; format-declared fields' happy probes pass against a validating handler; docs updated (+embedded sync); no-drift for unaffected designs.

## R3 — #49: cron must work (or fail loud) on SQLite

**Decision:** make it WORK: on non-Postgres backends the serve loop uses the existing in-memory single-node cron leader (`cron_tick_once`, `jerrycan-jobs/src/lib.rs:159`) instead of the Postgres leader path — SQLite is the dev default and dev cron should fire. Multi-node leader election stays Postgres-only, documented. If scouting reveals `cron_tick_once` is unsafe to run against a durable store (double-fire semantics), fall back to plan B: hard startup error on cron+non-Postgres — NEVER `Ok(0)` silence. State which path was taken and why in the report.
**Anchors:** `jobsgen.rs:141` unconditional `Jobs::postgres`; serve loop `jerrycan-jobs/src/lib.rs:338-348`; noop `postgres_store.rs:256-263`; the framework's own test `cron_tick_is_a_noop_on_non_postgres` (:921-930) must be updated to the new contract.
**Acceptance:** behavioral test proving a due cron job FIRES on SQLite (or, plan B, that serve refuses to start with a clear error); docs (15-jobs.md + embedded) state the single-node-vs-Postgres-leader semantics; jerrycan-jobs is unpublished → pub-API changes unchecked by the semver gate.

## R4 — #50: server-side realtime broadcast publish API

**Decision:** expose a handler-callable publish: `RealtimeHandle::publish(topic: &str, payload: serde_json::Value)` (or equivalent typed surface discovered while scouting) delegating to the crate-private `Hub`, resolvable as a dep in generated handlers; realtimegen emits the dep + a stub comment showing the one-liner. Update `18-realtime.md` (+embedded) with the REST-handler→broadcast pattern. Dynamic per-entity topics stay OUT (comment on #50 that it remains open for that half — do not close the issue if the fix text says "track dynamic topics here"; re-scope the issue in a comment instead).
**Anchors:** `jerrycan-realtime/src/lib.rs:381-384` (pub(crate) handle), `bus.rs:61,81`, `broadcast.rs:14,82`; J5's workaround as the anti-pattern to retire.
**Acceptance:** a generated realtime app's HTTP handler publishes to a broadcast topic first-class; a behavioral test proves subscriber receipt (the jerrycan-realtime crate's existing test harness patterns); realtime+jwt and realtime+session both compile (the P5-round-1 coupling test extends); jerrycan-realtime unpublished → semver-free.

## R5 — #53: defaulted/server-owned fields + path-redundant parent FKs omittable in request bodies

**Decision:** two rules, both generator-level: (a) an entity field with a design-declared default is OMITTED from the generated request DTO and OpenAPI request schema; the generated create path applies the default when absent (serde default on the DTO field wired to the design default); (b) on nested routes (`/parent/{id}/children`), the child's parent-FK comes from the PATH — omitted from the body DTO on those routes. Reuse the round-1 `endpoint_omits_identity_fk` architecture (one shared predicate, three surfaces: genroute DTO, testgen bodies, openapi schema). Same no-drift bar as #34.
**Anchors:** round-1 #34 machinery (`design.rs:596-626`, genroute/testgen/openapi call sites); J4's `#[serde(default)]` hand-fix as the target to make unnecessary; J2/J3's `habit_id`/`project_id` friction.
**Acceptance:** fixture matrix (defaulted field absent from DTO+schema+probe bodies and default applied; nested-route parent FK absent and path-injected; non-defaulted non-FK fields unchanged); e2e-lite: minimal `{"email":...}` J4-shaped POST → 201; no-drift proof.

## R6 — #42 + #43 + #44 + #45 + #46: small-batch hardening

One package, five contained fixes:
- **#42**: PUT/PATCH fixture cell for the identity-FK omission matrix + split the stub comment (create: inject `_user.0.id`; update: preserve existing owner — do NOT reassign).
- **#43**: gate openapi/testgen identity-FK omission db-consistently with genroute (or, if genroute's db-gating is the anomaly, align the other way — scout, pick, justify); request schemas must match the emitted DTO in memory mode.
- **#44**: new JC-code lint rejecting an entity literally named `{X}Request` when it collides with a generated DTO name; explain text names the rename fix.
- **#45**: centralize the test-credential header at emission sites (retire the post-hoc `.replace()` in storagegen) + inline comment at the no-`exp` mint sites.
- **#46**: 3xx-success endpoints get a `Redirect`-shaped stub (compiling, e.g. `Ok(Redirect::see_other(...))` placeholder with TODO comment) instead of `Result<Json<...>>`.
**Acceptance:** each has a pinned test; no-drift for outputs not matching the new rules; full suite green.

## R7 — #35: split 00-designing.md into a lean normative core + examples appendix

**Decision:** the mandated first read drops to a normative core (contract facts: top-level keys, field types, endpoint shape, un-greenable-probes box, gotchas table — target ≤10KB) and the worked examples move to a NEW page (`designing-examples`) registered in `docsidx.rs`, cross-referenced from the core ("worked examples: `jerrycan docs designing-examples`"). SKILL keeps mandating the core only. Every internal cross-reference updated; nothing deleted — moved.
**Anchors:** `docs/ai/00-designing.md` (24KB), `docsidx.rs:56-57` (slug registry), SKILL golden rule 3 + Phase 3 references; embedded_sync pair for BOTH pages (new embedded file required — check how docsidx maps slugs→files and how embedded_sync enumerates).
**Acceptance:** `jerrycan docs designing` serves the lean core; `jerrycan docs designing-examples` serves the appendix; `docs --list` shows both; embedded_sync green with the new pair; no contract fact lost (grep-audit: every `contract_version`/field-type/endpoint-key statement from the old page exists in exactly one of the two new pages); SKILL references updated. This is editorial — the reviewer must read both result pages end-to-end for coherence, not just diff hunks.

## Rollout

R1 → R2 → … → R7, each: implement (TDD) → task review → PR → gate verified green → merge → next. Ledger in `.superpowers/sdd/progress.md`. Issues closed per-package with one `fixes` keyword each; #50 gets a re-scope comment (dynamic topics) instead of full closure if R4 lands publish-only.
