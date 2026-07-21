# Realtime tenancy: close the changes-broadcast leaks (0.5.3)

**Date:** 2026-07-20
**Status:** Approved design, pre-implementation
**Issues:** #113 (CRITICAL — realtime `changes` on the tenancy entity broadcasts every row to every principal), realtime facet of #102 (a transitive/grandchild `changes` entity is unscoped), #117 (anon locked out of scope-`none` topics)
**Origin:** round-5 eval (faceoff5) — collab app; and the realtimegen transitive facet deferred from 0.5.1.
**Ships as:** 0.5.3 (security patch; behavior change — non-members stop receiving another tenant's `changes` — but no framework Rust API break; `cargo semver-checks` clean).
**Explicitly deferred to 0.6.0:** #104 many-membership realtime (Principal membership-SET + `Join.tenant` wire protocol + tenant-partitioned `publish_to`) — a breaking `jerrycan-realtime` API change, its own slice.

## Problem
`realtimegen.rs:51-55` `changes_spec` sets a change channel's `tenant_column = Some(fk)` **only** when the entity **directly** `belongs_to` the tenant. Two holes follow, both ending at `channel.rs:105` `change_visible` `(None, _) => true` → the full row is delivered to **every authenticated principal, member or not**:
1. **#113 — the entity IS the tenant** (`changes: ["Workspace"]`, Workspace = tenancy.entity): an entity doesn't `belongs_to` itself → `tenant_column: None` → every Workspace row to everyone.
2. **Transitive** — a grandchild (`Contact → Account → Org`): no direct `belongs_to` tenant → `None` → same leak, silently, `check` green.

Separately, **#117**: `ws.rs:66-70` resolves the principal with `?` at WS upgrade whenever an auth model exists, so an anonymous client 401s at connect and can never reach a `scope: "none"` topic, contradicting `scope_allows(None, None) => Ok` (channel.rs:87).

## Design

### A. #113 — the tenant entity's own pk IS its tenant key (codegen-only)
In `changes_spec` (realtimegen.rs:43-57): when `entity == tenancy.entity`, set `tenant_column = Some(pk_column)` (the tenant's own pk, e.g. `"id"`). The runtime is already correct once the column is populated: CDC extracts `NEW."id"::text`, and `change_visible` matches it against `Principal.tenant_id` (the stringified tenant pk) — so a member of tenant T receives only tenant-T's own row, and non-members receive nothing. **Zero runtime change.** Update the codegen test `non_tenant_entity_gets_no_tenant_column…` (realtimegen.rs:413-422) and add a positive case (tenant-entity changes → `tenant_column = Some("id")`).

### B. Transitive `changes` → fail-loud `JC0547` (validator; stopgap, correct-by-construction)
A `changes` entity whose `design.tenant_path(entity)` has a **non-empty `joins`** (a grandchild) cannot be safely scoped by a single row-image column — the tenant key lives on an ancestor table, which neither CDC adapter can read from the row alone. Rather than ship a silent leak, **refuse at design time**: new `JC0547` ("realtime `changes` on a transitively tenant-owned entity is not supported — the changes entity must be the tenant itself or a DIRECT child; flatten the relationship or drop it from `changes`"). Raised in `questions.rs` (the realtime validation block ~944-1010, beside the topic-name/scope checks), gated on `tenancy.is_some() && tenant_path(e).map_or(false, |p| !p.joins.is_empty())`. This converts the transitive silent leak into a loud, correct refusal. (Full transitive realtime — a `ChangeChannelSpec` join-resolved tenant key — is a 0.6.0 capability, tracked.)

### C. #117 — anon reaches scope-`none` topics (runtime, ws.rs)
At WS upgrade (`ws.rs:66-70`), stop making a resolver error fatal. **Decision (avoid the silent-invalid-token downgrade):** swallow the resolver failure into `principal = None` ONLY when the request presents **no credential material** (no session cookie / no `?token=`); if credential material IS present but invalid, keep the 401 at upgrade. With no credential and no principal, the client can still `may_join` a `scope:"none"` topic (channel.rs:87), while `auth`/`tenant`/`changes` topics still 401 at join (channel.rs:59-61, 88-92) — no channel becomes open. The generated resolver (realtimegen.rs:62-119) passes a "has-credential-material" signal or the runtime inspects the request; keep the fatal path for present-but-invalid credentials.

### D. Real cross-tenant negative-control acceptance test (proof)
The generated realtime acceptance test is currently an `#[ignore]`d stub asserting only an env var (realtimegen.rs:221-284). Emit a REAL negative control for a tenancy design with `changes`: a member of tenant A subscribed to the changes topic does NOT receive a change to tenant B's row (and DOES receive tenant A's). This is the runtime proof #113 is closed (mirrors the in-crate negative controls at channel.rs:238-255). If a live-WS harness is infeasible in generated tests (no in-memory WS client documented — a known gap), assert it at the `change_visible`/`ChangeChannelSpec` level instead and note the limitation.

## Non-goals (0.6.0+)
- #104 many-membership realtime (one-socket-one-tenant → membership set; `Join.tenant`; tenant-partitioned `publish_to`) — breaking `Principal`/protocol change.
- Full transitive realtime (`ChangeChannelSpec` join-resolved tenant key + adapter changes) — the capability B refuses.
- #50 dynamic per-entity topics; presence-on-auth-scope cross-tenant footgun (validator warning) — tracked.

## Success criteria
- A `changes:["Workspace"]` (Workspace = tenant) design: a non-member receives NO Workspace rows; a member receives only their own tenant's row. (#113 closed.)
- A `changes` on a grandchild fails `check` with `JC0547` (no silent leak).
- An anonymous client connects to `/realtime` and joins a `scope:"none"` topic; an `auth`/`tenant`/`changes` topic still 401s; a present-but-invalid token still 401s at upgrade. (#117 closed.)
- Realtime apps with a DIRECT-child `changes` entity are byte-identical (already `Some(fk)`). Non-realtime apps byte-identical. `cargo semver-checks` clean.
- Generated realtime acceptance test carries a real (or `change_visible`-level) cross-tenant negative control.
