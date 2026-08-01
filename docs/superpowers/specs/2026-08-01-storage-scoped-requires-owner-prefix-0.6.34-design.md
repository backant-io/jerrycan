# A scoped storage bucket requires owner_prefix (0.6.34) — #133

**Date:** 2026-08-01
**Status:** Approved design, pre-implementation
**Issues:** #133 (a bucket WITHOUT `owner_prefix` shares ONE global key namespace across all owners. `put_object`'s duplicate check (`meta::key_exists`, unscoped) then leaks a cross-owner EXISTENCE ORACLE — owner B uploading owner A's key gets a 409 (learns A has it) — and lets owners SQUAT each other's keys. `owner_prefix: true` is immune (keys are `{owner}/…`, so the path, the unique index, and the check are all naturally per-owner). Prior analysis (on the issue) showed the naive "just scope the check" fix CORRUPTS data — the blob write lands at the shared path, so B overwrites A's bytes before the 409 with no compensation — and the full scope-the-path-and-index fix breaks the Supabase-parity global-namespace contract (a 0.7 change). The proportionate, non-corrupting, in-convention 0.6.x fix is to REFUSE the footgun at design time: a bucket that is OWNER-SCOPED must set `owner_prefix`.)
**Ships as:** 0.6.34 — a new design-validation code **JC0565** in `crates/jerrycan/src/platform/questions.rs` (bucket loop ~2079) that refuses an owner-scoped bucket lacking `owner_prefix`. Precedented by JC0558 (validation-tightening in a 0.6.x minor). Byte-identical for any design whose scoped buckets already set `owner_prefix` (or have no owner). This IS codegen-adjacent (validation), so run the heavy gate.

## The rule (JC0565)
A bucket is OWNER-SCOPED when it has an `owner` — i.e. `storagegen::bucket_scope(design, b)` is NOT `BucketScope::Unowned` (it is `User`, `UserInTenant`, or `Tenant`). Refuse such a bucket when `!b.owner_prefix`:
> "Bucket `<name>` is owned by `<owner>` but has no `owner_prefix`, so all owners share ONE global key namespace — one owner can learn of or squat another owner's keys (the cross-owner key oracle, #133). Set `owner_prefix: true` for per-owner key isolation (keys become `{owner}/…`). An intentionally SHARED bucket should have no `owner` (Unowned) instead. See `jerrycan explain JC0565`."
- Fires ONLY for owner-scoped buckets. An **Unowned** bucket (no `owner`) keeps `owner_prefix: false` valid — no per-owner scope, no oracle. Do NOT flag it.
- `owner_prefix` changes only the KEY LAYOUT (prefixed), not read-visibility (which the scope already governs), so requiring it on a scoped bucket never changes who-sees-what — it only removes the cross-owner collision/oracle. A metadata-only-ownership use case is served identically.

## Register + explain
Add JC0565 to `codes.rs` (mirror the JC0564 entry: title/cause/fix, `doc: "jerrycan docs storage"`) + a WHY test asserting `jerrycan explain JC0565` names the owner-scoped + owner_prefix rule.

## Fixture ripple (find + fix ALL scoped-no-prefix buckets)
Grep every inline/fixture design for a bucket with an `owner` and no `owner_prefix` and fix each (set `owner_prefix: true`, OR drop the `owner` if the bucket is genuinely shared):
- `crates/jerrycan/src/platform/design.rs` — the inline example with `avatars` (`{ "name": "avatars", "visibility": "public", "owner": "User" }`, no owner_prefix, ~line 1937) + its round-trip test assertions (~line 2230 asserts `!owner_prefix`). `avatars` should be `owner_prefix: true` (public per-user avatars).
- `crates/jerrycan/src/platform/storagegen.rs` test fixtures (the `owner_prefix: false` cases ~line 752): CHECK each — if the bucket is Unowned (no owner), it is VALID and stays; if owner-scoped, fix it (owner_prefix:true) OR make it Unowned to keep testing the `owner_prefix:false` generation path (a `owner_prefix:false` codegen test needs an UNOWNED bucket now).
- `conformance/designs/*.json`, `testgen.rs`, docs examples — grep for `"owner"` + bucket and fix any scoped-no-prefix bucket. (reference-slice has no storage block — confirmed, no ripple there.)
The reference `owner_prefix:false` codegen/negative-control test (storagegen.rs:652/752) must be re-pointed at an UNOWNED bucket so it still exercises the `owner_prefix:false` emission (JC0565 no longer permits a scoped one).

## Docs (byte-identical twins)
`docs/ai/18-storage.md` + `crates/jerrycan/embedded/ai/18-storage.md` already describe the #133 oracle + recommend `owner_prefix` (~lines 40-44). Update "recommended/Set `owner_prefix: true` for isolation" to state it is now **required** on an owner-scoped bucket (JC0565 refuses otherwise); an intentionally shared bucket is Unowned. Keep the twins byte-identical.

## Tests
- **Validation unit tests** (questions.rs): an owner-scoped bucket without `owner_prefix` → JC0565 fires; the SAME bucket with `owner_prefix: true` → clean; an UNOWNED bucket without `owner_prefix` → clean (no false positive); a Tenant-scoped and a User-scoped case both fire.
- codes.rs WHY test for JC0565.
- Update the storagegen fixtures/tests per the ripple above; the `owner_prefix` isolation test (storagegen.rs:654) is unchanged (it already uses a prefixed bucket).

## Gates
- `cargo test -p jerrycan` (validation + codes + storagegen tests) green.
- **Heavy eval gate:** `reference_eval` + `conformance` + `eval` + `genroute_compile` `--include-ignored` green (a storage fixture may be exercised; confirm no design newly-fails JC0565 unexpectedly). Local PG available.
- `cargo fmt`/`clippy -D warnings`; determinism + embedded_sync twin green.

## Success criteria
- An owner-scoped bucket without `owner_prefix` is refused (JC0565) with an actionable message; an Unowned bucket is unaffected; a scoped bucket with `owner_prefix` is clean.
- All scoped-no-prefix fixtures fixed; docs twins updated (required, not recommended); heavy gate green; published 0.6.34; #133 closed.

## Non-goals
- The full scope-the-blob-path-and-unique-index fix (a 0.7 Supabase-parity contract change — `owner_prefix` already provides the isolation). Defaulting `owner_prefix` true (a breaking key-layout change for existing deployments — refusing is the non-silent choice). Changing `key_exists`/the runtime (the design-time refusal removes the unsafe shape before it ships).
