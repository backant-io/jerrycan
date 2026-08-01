# Multi-tenant realtime is buildable: tenant-partitioned publish + WS tenant-select + zero-membership connect (0.7.2) — #104

**Date:** 2026-08-01
**Status:** Approved design, pre-implementation
**Issues:** #104 (the realtime + many-membership tenancy seam is unbuildable. (a) **Tenant-partitioned broadcast is impossible** — a `tenant`-scoped topic REFUSES server publish (JC0403, `broadcast.rs:59-71`) while an `auth`-scoped topic DELIVERS cross-workspace, so "workspace A's members get A's messages, not B's" has no expressible form. (b) **Many-membership WS binds an arbitrary FIRST-membership tenant** — the single-tenant-per-user assumption T2 fixed for HTTP is still live on the WebSocket principal-resolve; a user in workspaces A+B can't choose which their socket scopes to. (c) **A zero-membership authenticated user is 403'd off `/realtime`** even when no declared topic needs a tenant.) Folds in the #50 residual (tenant-partitioned server publish).
**Ships as:** 0.7.2 — a `jerrycan-realtime` runtime addition (`publish_to`) + a `realtimegen` resolver change (WS tenant-select) + generated publish-wiring for tenant topics. Likely a MINOR: `publish_to` is additive; the resolver change is byte-identical for a SINGLE-membership user (their one tenant is selected exactly as today), and more-permissive for zero/multi-membership (a bug fix). Confirm with `cargo semver-checks`.

## The delivery infra already partitions — the gap is the publish + the resolve
`deliver`/`scope_allows` already partition a Tenant-scoped topic per tenant: a socket receives a Tenant-scoped event ONLY when `principal.tenant_id == event.tenant_id` (`channel.rs:104-106`). So the fix is (1) a server publish that SETS `event.tenant_id`, and (2) a WS resolve that sets `principal.tenant_id` to a VERIFIED chosen tenant.

## Part 1 — tenant-partitioned server publish (`publish_to`)
Add `RealtimeHandle::publish_to(&self, tenant_id: &str, topic: &str, payload: serde_json::Value) -> Result<()>` (`broadcast.rs`): the partitioned twin of `publish`. It goes through the same `publish_from_server` path but sets `event.tenant_id = Some(tenant_id)` (not `None`). Then:
- A **Tenant-scoped** topic: `publish_to(tenant, …)` is ALLOWED (partitioned → only that tenant's sockets). `publish(…)` (un-partitioned) STAYS `JC0403` for a Tenant-scoped topic (delivering to all tenants would leak) — keep that guard, but its message now points at `publish_to` as the supported path.
- A **None/Auth** topic: `publish_to` is accepted but `tenant_id` does not gate delivery (scope already admits) — or reject `publish_to` on a non-Tenant topic with a clear message (pick the least-surprising; a Tenant topic is the intended use). State the choice.
- **Generated wiring (realtimegen):** a mutating handler in a design with a server-publishable **Tenant-scoped** broadcast topic gets `_rt: Dep<RealtimeHandle>` + a stub comment showing `_rt.publish_to(_tenant.id(), "<topic>", payload)` (mirrors the #50 `publish` wiring for none/auth topics). Gate on the topic scope.

## Part 2 + 4 — WS resolve: choose a verified tenant, never abort on no-tenant (realtimegen resolver)
The generated principal resolver (`realtimegen.rs` `resolver_rs`, the `tenant_block` ~line 131) currently does `let tenant = ctx.resolve::<shared::Tenant>().await?` — which binds an arbitrary membership and PROPAGATES a non-member/zero-member error (aborting the upgrade). Replace the tenant leg with a MEMBERSHIP-AWARE resolve:
1. Read an OPTIONAL `?tenant=<id>` from the WS connect query (browsers can't set headers on a WS, same channel as the `?token=` from #117).
2. If `?tenant=` is present: VERIFY the session user is a member of that tenant (a `SELECT 1 FROM {tenant}_members WHERE {fk}=? AND user_id=?` — the `MEMBERSHIP_PRINCIPAL_COLUMN`). Member ⇒ `principal.tenant_id = Some(that tenant)`. NON-member ⇒ REFUSE the upgrade (403 — you asked for a tenant you're not in). This is the make-impossible guard: a socket can NEVER scope to a tenant the user isn't verified in.
3. If `?tenant=` is ABSENT: resolve the user's memberships. EXACTLY ONE ⇒ `principal.tenant_id = Some(it)` (BYTE-IDENTICAL to today's single-membership behavior). ZERO or MORE-THAN-ONE ⇒ `principal.tenant_id = None` (connect, but reach only None/Auth topics — a multi-membership user MUST pass `?tenant=` to reach Tenant topics; a zero-membership user is NOT 403'd — fixes (c)).
- Never abort the upgrade for "no tenant" — only for an explicit `?tenant=` the user is not a member of. A `None` tenant_id reaches only scope-`none`/`auth` topics (Tenant topics reject `None` at JOIN, `channel.rs:89-92`) — no escalation.

## Security invariant (MUST hold — verify)
`principal.tenant_id` is set ONLY to a tenant the user is VERIFIED a member of (explicit `?tenant=` + membership check, or the sole-membership fallback). A Tenant-scoped topic (join AND delivery) admits a socket ONLY when `principal.tenant_id == the topic/event tenant`. Therefore: no socket ever receives another tenant's Tenant-scoped events, and `publish_to(A, …)` reaches only tenant-A sockets. A `None` principal.tenant_id reaches only None/Auth topics. This is the realtime analogue of the HTTP membership-verified scoping (#102/#78).

## Dynamic per-entity topics (part of #104's "fix direction") — OUT OF SCOPE, documented
Per-ROOM isolation WITHIN a tenant (dynamic `room:{id}` topics) is NOT required for correctness or security: a tenant-scoped topic + `publish_to(tenant)` already isolates ACROSS tenants (the security boundary), and per-room fan-out within a tenant is a non-sensitive client-side filter (all recipients are already in the tenant). Document this in `18-realtime.md`: for per-room UX, publish room-tagged payloads to the tenant topic and filter client-side; true dynamic topics are a future ergonomic enhancement. (This is why #104 closes on parts 1+2+4 — the "unbuildable"/leak headline — without a blocking dynamic-topic dependency.)

## Tests (jerrycan-realtime + realtimegen + acceptance)
- **jerrycan-realtime unit/WS:** `publish_to(A, topic, payload)` delivers to a tenant-A socket and NOT a tenant-B socket on a Tenant-scoped topic; `publish` (un-partitioned) still `JC0403` on a Tenant topic. A WS with `?tenant=A` for a member ⇒ `tenant_id=Some(A)`; for a non-member ⇒ upgrade refused; absent `?tenant=` with one membership ⇒ that tenant; with zero ⇒ `None` (connects, reaches an `auth`/`none` topic, rejected from a Tenant topic); with two ⇒ `None`.
- **realtimegen unit:** the resolver emits the `?tenant=` + membership-verify + sole-membership-fallback + no-abort logic; a mutating handler with a Tenant-scoped broadcast gets the `_rt.publish_to(_tenant.id(), …)` stub. A non-tenancy / no-server-publishable design is byte-identical.
- **genroute_compile / reference-slice:** a tenancy realtime design's generated resolver + a `publish_to` handler compile under strict clippy. If the reference-slice has a realtime tenancy topic, its generated resolver changes → update the battery.
- Docs twins (`18-realtime.md`) byte-identical.

## Gates
- `cargo test -p jerrycan-realtime` + `-p jerrycan` green.
- **Heavy eval gate:** `reference_eval` + `conformance` + `eval` + `genroute_compile` `--include-ignored` green (the reference-slice realtime resolver changes — single-membership fixtures stay byte-identical; a multi-membership WS test proves the select). Local PG (`wal_level=logical`) available for the realtime changes leg.
- `cargo fmt`/`clippy -D warnings`; `cargo doc --workspace --no-deps --all-features` with `RUSTDOCFLAGS=-D warnings` (the #150 CI-only trap — run it locally); `cargo semver-checks`; determinism + embedded_sync.

## Success criteria
- A server handler can `publish_to(tenant, topic, payload)` to a Tenant-scoped topic and reach ONLY that tenant's sockets (tenant-partitioned broadcast is now expressible); a multi-membership WS chooses its tenant via `?tenant=` (membership-verified, non-members refused); a zero-membership authenticated user connects (reaching None/Auth topics) instead of a 403. No socket receives another tenant's Tenant-scoped events.
- Single-membership designs byte-identical; heavy gate + determinism green; `cargo doc -D warnings` clean; published 0.7.2; #104 closed. **This empties the board.**

## Non-goals
- Dynamic per-entity topics (documented workaround above; future enhancement). Changing the scope model or the `changes`/presence channels. Re-authenticating mid-connection. Removing the `publish` (un-partitioned) `JC0403` guard on Tenant topics (keep it — `publish_to` is the supported path).
