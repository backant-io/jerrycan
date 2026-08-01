# Realtime `changes` on a per-user entity is owner-scoped; trigger-DDL failure fails loud (0.7.3) — #216 + #212

**Date:** 2026-08-01
**Status:** Approved design, pre-implementation (AUDIT round 1 findings)
**Issues:**
- **#216 (SECURITY — cross-user leak).** A `changes` channel on a per-user (identity-owned, non-tenant) entity (e.g. `Note belongs_to User`) broadcasts EVERY user's rows to EVERY authenticated principal. `changes_spec` (`realtimegen.rs:44-69`) sets `tenant_column: None` for such an entity (it only scopes the tenant or a DIRECT tenant child); `change_visible` (`channel.rs:104-105`) then maps `(None, _) => true` → delivered to any principal, with no owner check. JC0547 only refuses *transitively-tenant-owned* changes (`tenant_path == None` for a per-user entity), so it scaffolds GREEN — while the REST repo for the SAME entity emits only owner-scoped `all_for`/`get_for`. This is the #113 tenant-leak on the per-user (#79/#150) axis. Unlike the transitive-tenant case, the row image carries the owner fk, so it IS scopable.
- **#212 (correctness — silent dead feed).** The trigger-fallback arm of `start_change_source` (`lib.rs:628-648`) logs an `ensure_triggers()` DDL failure to stderr and proceeds anyway (spawns `TriggerAdapter::run`, which `LISTEN`s with no triggers to ever `NOTIFY`), but — unlike its sibling failure branches (`lib.rs:593`, `599`) — NEVER sets `changes_unavailable`. So a client subscribing to a `changes:` channel is admitted (`Joined`) and then silently receives ZERO events forever. Trigger: a privilege-restricted Postgres where the `CREATE FUNCTION`/`TRIGGER` DDL is denied.
**Ships as:** 0.7.3 — a `jerrycan-realtime` engine fix (`channel.rs` owner-scope in `change_visible`, `lib.rs` fail-loud) + a `realtimegen` derivation (`owner_column` on `ChangeChannelSpec`). #216 changes generated realtime wiring for a per-user changes entity (now owner-scoped — the fix); byte-identical for tenant-scoped and genuinely-auth-only changes entities. #212 is runtime-only.

## Part A (#216) — owner-scope a per-user changes channel (parallel to the tenant path)
1. **`ChangeChannelSpec` (`lib.rs:41`):** add `pub owner_column: Option<String>` (the row's identity fk column, e.g. `user_id` — or the #150 configured `identity_fk_column`). It is `#[non_exhaustive]`-friendly (that struct's exhaustiveness — confirm it's not literal-constructed by downstream except the generated wiring, which we update).
2. **`ChangeEventView` (`channel.rs:43`):** add `pub(crate) owner_id: Option<String>` — the NEW row's owner-column value, extracted the SAME way `tenant_id` is (the changes source — `changes/mod.rs` + pgoutput/trigger row→event mapping — already extracts `tenant_id` from `tenant_column`; extract `owner_id` from `owner_column` identically, from the row image).
3. **`change_visible` (`channel.rs:98-110`):** nest the owner check UNDER the `tenant_column == None` arm — tenant scoping is unchanged; a per-user entity (tenant None, owner Some) delivers ONLY to its owner:
   ```rust
   match (&spec.tenant_column, &ev.tenant_id) {
       (Some(_), Some(t)) => p.tenant_id.as_deref() == Some(t.as_str()),
       (Some(_), None) => false, // tenant-scoped, no key → fail closed
       (None, _) => match (&spec.owner_column, &ev.owner_id) {
           (None, _) => true,                                   // genuinely auth-only (no tenant, no owner)
           (Some(_), Some(o)) => p.user_id.as_str() == o.as_str(), // per-user: only the owner
           (Some(_), None) => false,                            // owner-scoped, no key → fail closed
       },
   }
   ```
   `Principal.user_id` already exists (the resolver sets it, `realtimegen.rs:152`). **No owner-move delete-view is needed:** the identity fk is server-injected (#34) and owner-scoped writes can't change it (owner is immutable), so unlike the tenant path there is no `old_owner` case — `delete_view_for_old_tenant` is unchanged and no owner analogue is added. State this.
4. **`realtimegen changes_spec` (`realtimegen.rs:44`):** ALSO derive `owner_column` — when the entity is NOT tenant-scoped (`tenant_column` is None) AND `Design::entity_is_per_user_owned(entity)`, set `owner_column = Some(Design::identity_fk_column())` (the #150-aware identity fk, e.g. `user_id`); else `None`. Emit it in the `.changes(ChangeChannelSpec { … owner_column: {owner_lit} })` wiring (`realtimegen.rs:303`).
5. **The row must carry the owner column:** confirm the changes source's row projection (REPLICA IDENTITY FULL / trigger `row_to_json`, and the `hidden_columns` projection from #167) INCLUDES the owner column so `owner_id` is extractable — the owner fk (`user_id`) is not `write_only`, so it is present. If `hidden_columns` could hide it, ensure the OWNER column is never projected out (it must remain available to the filter even if hidden from the payload — mirror how `tenant_column` is handled: the filter reads it from the view, the payload may hide it).

## Part B (#212) — trigger-DDL failure fails loud
In `start_change_source` (`lib.rs:628-648`), the trigger arm's `ensure_triggers()` failure path must `hub.changes_unavailable.store(true, Ordering::Relaxed)` (mirroring the sibling branches at `lib.rs:593`/`599`) BEFORE returning/not-spawning — so a `changes:` subscribe is refused with JC0530 (`lib.rs:261-275`) instead of admitting a dead feed. Do NOT spawn `TriggerAdapter::run` when the DDL failed. Keep the stderr log.

## Security invariant (#216 — MUST hold, verify + test)
A `changes:` event for a per-user entity is delivered to a socket ONLY when the row's owner (`ev.owner_id`) equals the subscriber's `principal.user_id` — i.e. a client receives only changes to rows it OWNS, exactly matching the REST `get_for`/`all_for` owner-scoping and the `18-realtime.md` promise ("only a change you could have GET'd"). No cross-user delivery. A genuinely auth-only changes entity (no tenant, no identity fk) is unchanged (any principal). Tenant-scoped changes unchanged.

## Tests
- **jerrycan-realtime unit (`channel.rs` change_visible tests):** per-user spec (`tenant_column: None, owner_column: Some("user_id")`) — an event with `owner_id: Some("u1")` is visible to principal `user_id=u1`, NOT to `user_id=u2`; an auth-only spec (both None) is visible to any; tenant spec unchanged; owner-Some-but-owner_id-None fails closed.
- **jerrycan-realtime WS/delivery:** a per-user `changes:` subscriber receives ONLY their own row's insert/update/delete, not another user's (model on the tenant-partition `ws_live`/delivery tests).
- **realtimegen unit:** a per-user changes entity emits `owner_column: Some("user_id")` (or the #150 identity fk); a tenant-scoped entity emits `owner_column: None` (byte-identical); an auth-only entity emits `owner_column: None`.
- **#212:** a unit/integration test that a trigger-DDL failure sets `changes_unavailable` (a subsequent `changes:` join → JC0530), mirroring the sibling-branch behavior.
- genroute_compile / reference-slice: a per-user realtime changes design compiles; if the reference-slice has a per-user changes entity its wiring gains `owner_column` (update the battery); tenant-only realtime designs byte-identical.

## Gates
- `cargo test -p jerrycan-realtime` + `-p jerrycan` green.
- **Heavy eval gate:** `reference_eval` + `conformance` + `eval` + `genroute_compile` `--include-ignored` green (live PG, wal_level=logical, for the changes leg).
- `cargo fmt`/`clippy -D warnings`; `RUSTDOCFLAGS=-D warnings cargo doc --workspace --no-deps --all-features` clean (the CI-only trap — no public doc links a `pub(crate)` item); `cargo semver-checks` (the new `pub owner_column` field on `ChangeChannelSpec` — confirm it's `#[non_exhaustive]` so additive-minor); determinism + embedded_sync.

## Version + Success criteria
0.7.3 (MINOR — additive `owner_column` on a `#[non_exhaustive]` struct + a runtime fix). A per-user `changes` channel delivers a row only to its owner (no cross-user leak — the #216 fix); a trigger-DDL failure refuses `changes:` with JC0530 instead of a silent dead feed (#212); tenant-scoped + auth-only changes byte-identical; heavy gate + determinism + cargo-doc green; published 0.7.3; #216 + #212 closed.

## Non-goals
- Changing the tenant path or the broadcast/presence scopes. An owner-move delete-view (owner is immutable). The transitive-tenant JC0547 refusal (unchanged — that case is genuinely un-scopable). Dynamic topics.
