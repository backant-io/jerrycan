# Round-3 cleanup: inline-DTO default/write_only + inline reject on /{id} + storage sign devex (0.7.10) — #235 + #236 + #237

**Date:** 2026-08-02
**Status:** Approved design, pre-implementation (AUDIT round 3 findings)
**Issues:** #235 (inline-DTO field emission ignores default + write_only — silent contract inversion), #236 (inline 422 reject probe dropped on a `/{id}` no-creator action), #237 (storage `sign` per-request 500 in dev when JERRYCAN_SECRET unset).
**Ships as:** 0.7.10 — a genroute emission fix (#235), a testgen fix (#236), and a `jerrycan-storage` devex fix (#237). Patch bump 0.7.9 → 0.7.10. Entity-body + non-inline designs byte-identical.

## Part A (#235) — inline-DTO field emission must not silently invert default/write_only
`inline_request_dto_rs` (`genroute.rs:1610`, field loop 1624-1636) emits each inline field purely on `f.required`, ignoring `f.default` and `write_only_attr` (the entity path drops now/static-default fields from the create DTO via `field_is_now_default` `genroute.rs:884,1509`, and applies `write_only_attr` `genroute.rs:997,1345`). So `default:"now"` (#110) on an inline field makes it a REQUIRED, client-supplied, backdatable field — the #110 contract ("server-set, immutable, omitted from DTOs") is fully inverted; static defaults are never applied; `write_only` is dropped.

**Investigate the cleanest mapping and pick ONE (state which):**
- **(a) Mirror the entity path** — OMIT default-carrying inline fields from the generated request DTO (client can't send them), apply `write_only_attr` to write_only inline fields. For the server-set value on a custom action (whose handler is an agent-owned stub), document that the agent supplies it (the field is simply absent from `{Op}Request`). This matches "omitted from DTOs".
- **(b) Refuse with a clear JC error** — if an inline custom-action field carries `default`/`write_only` and there is no sensible codegen server-set path, refuse the design with a new JC code ("`default`/`write_only` is not supported on an inline request_body field — omit it and set it in the handler"). This is make-impossible + honest.
The BUG to kill is the SILENT INVERSION (a `default:"now"` inline field becoming client-required + backdatable with no diagnostic). Whichever you pick, the outcome must be: no inline field with a `default` is a required client field with the default un-applied, and `write_only` is not silently dropped. Add generated coverage or a validation test.

## Part B (#236) — inline reject probe on the `/{id}` no-creator / skipped-creator branches
`push_inline_reject_test` is emitted on the seeded (`testgen.rs:888`) and param-0 (`815`) branches, but the param-count==1 NO-creator (`889-908`) and skipped-creator (`816-840`) branches emit only an `AGENT TODO`. The inline 422 precedes any id lookup (its own comment `testgen.rs:885-887` — needs no seeded row), and `concrete_mount_base(&full_path)` is already available there (used for the seedless 401 at `:904`). Emit `push_inline_reject_test` on those two branches too (constraint AND enum reject, guarded-cookie-threaded, counted in `reject`), so a constrained inline action on a `/{id}` path with no creator gets its 422 coverage. Test: a design like `POST /{id}/apply` with `amount` (min/max) + `tier` (enum), no creator → the generated suite now has the two reject probes (was TODO-only).

## Part C (#237) — storage sign is honest in dev, not a raw 500
`crates/jerrycan-storage/src/lib.rs:173-178` — `sign` returns HTTP 500 (`JC0500` "JERRYCAN_SECRET is required for signed URLs") in the default dev config (JERRYCAN_SECRET unset), asymmetric with auth's dev fallback and reading as a server bug. Make it honest:
- Emit a ONE-TIME boot/first-use log WARNING that signed URLs are disabled until `JERRYCAN_SECRET` is set (so the operator learns at startup, not per failed request).
- AND/OR return a clearer status than a raw 500 for this known-config case (e.g. a `501 Not Implemented` / `503`-style "signing not configured" with the same honest message) so it doesn't read as a crash. Keep the security posture (never sign with the insecure dev key). Pick the least-surprising; state the choice. If a boot-time hard-fail-when-a-bucket-needs-signing is cleaner, that's acceptable too (fail fast at startup rather than per request). LOW — keep it small.

## Tests
- #235: an inline action with a `default:"now"` field → the chosen behavior (field omitted from `{Op}Request` OR the design refused) — a unit test; a `write_only` inline field handled; an inline action with NO default → byte-identical.
- #236: the `/{id}` no-creator constrained inline action emits both reject probes (testgen unit); the seeded/param-0 paths unchanged.
- #237: `sign` without JERRYCAN_SECRET produces the boot warning / clearer status (a jerrycan-storage unit test); with it set → 200 unchanged.
- Entity-body + existing inline designs byte-identical; determinism + embedded_sync green.

## Gates
- `cargo test -p jerrycan` + `-p jerrycan-storage` green; the heavy eval gate (`reference_eval`/`conformance`/`eval`/`genroute_compile --include-ignored`) + realtime CDC gate green.
- `cargo fmt`/`clippy -D warnings`; `cargo doc -D warnings`; `cargo semver-checks`; determinism + embedded_sync.

## Version + success criteria
0.7.10. A `default`/`write_only` on an inline request_body field no longer silently inverts the contract (#235); a constrained inline action on a `/{id}` no-creator path gets its 422 reject coverage (#236); storage `sign` in the default dev config surfaces an honest boot warning / clear status instead of a raw per-request 500 (#237). Entity + non-inline byte-identical; heavy gate green; published 0.7.10; #235 + #236 + #237 closed. **Board → 0 → audit Round 4.**

## Non-goals
- Any change to the entity-body path (unchanged). The multi-param-path no-reject-probe symmetric gap (separate, pre-existing). Signing with the dev key (never — the security posture stays).
