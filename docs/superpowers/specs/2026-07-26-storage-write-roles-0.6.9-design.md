# Storage write-role authz + owner_prefix guidance (0.6.9) — #132 (+#133 mitigation)

**Date:** 2026-07-26
**Status:** Approved design, pre-implementation
**Issues:** #132 (HIGH authz bypass — blob `upload`/`remove`/`sign` take a bare `Dep<Tenant>`, so any member role can write/delete; combined with `owner_id = tenant.id()` stamping, a **read-only-role member (viewer) can upload bytes and delete others' uploads**, while the File metadata endpoints correctly `require_role`). #133 (storage key oracle — mitigated here by docs; the proper fix is deferred, see the #133 re-scope comment: the naive `key_exists` scoping corrupts data).
**Ships as:** 0.6.9 — a storage-authz security fix. Codegen + validation only, no runtime-crate change. Additive: a bucket with no `write_roles` is byte-identical (any member writes, today's behavior); only buckets that opt into `write_roles` gain the role gate.

## Problem
Blob write handlers `upload` (storagegen.rs:286-292) and `remove` (:295-300) interpolate the bare `{guard}` (`tenant: Dep<Tenant>`) with **zero** `require_role` (`grep require_role storagegen.rs` = 0). A `Tenant`-scoped bucket stamps `owner_id = tenant.id()` (:121), so every member is "the owner" → a member holding a read-only role POSTs bytes (201) and DELETEs others' uploads. (`sign` — storagegen.rs:302-307 — issues a signed **download** URL, a read grant; it is NOT write.)

## Design

### A. Contract: `write_roles` on the bucket
Add `write_roles: Vec<String>` to `BucketDesign` (design.rs:363-383), mirroring `allowed_mime` (`#[serde(default, skip_serializing_if = "Vec::is_empty")]` + a `// #132` doc comment) — empty default = any member (backward-compatible, byte-identical). Mirror in `docs/contracts/design-schema.json` (pinned by `tests/contracts.rs`).

### B. Generator — gate the write ops (tenant-scoped buckets)
- Emit a role check as the FIRST statement of `upload` (storagegen.rs:288, after the `{`) and `remove` (:297), ONLY when the bucket has a `Tenant` guard (`BucketScope::Tenant`, storagegen.rs:126) AND `write_roles` is non-empty. Do NOT gate `sign` (read grant), `download`/`list` (reads).
- **Multi-role:** `Tenant::require_role` (scaffold.rs:80, → `require_role` guard.rs:42-48 → 403) is single-role exact-match. For `write_roles`: a 1-element vec → `tenant.require_role("{role}")?;` (mirror the member-surface pattern genroute.rs:2895/2912/2934). For ≥2 roles, add a `require_any_role(&self, roles: &[&str]) -> Result<()>` helper to the generated `Tenant` impl (scaffold.rs:76-83, mirroring `require_role` → `forbidden()` when none match) and emit `tenant.require_any_role(&["a","b"])?;`. Always emit the ≥1 form via the multi-role helper for uniformity, OR the single `require_role` for a 1-element vec — pick one and keep it consistent.
- **Scope:** v1 gates `Tenant`-scoped buckets. `UserInTenant` (object owned by the user, but the user has a tenant role) is DEFERRED — gating a user's writes to their OWN object by tenant role is a separate decision; document it as a v1 non-goal (no `require_role` emitted there). `User`/`Unowned` scopes carry no role — never gated.
- **Byte-identity:** empty `write_roles` → no emission → generated bucket module byte-identical. Only `write_roles` buckets change (pinned by a no-drift test).

### C. Validation — `JC0556`
In the per-bucket validation loop (questions.rs:1479-1578), register **`JC0556`** (next free after JC0555; codes.rs + `explain` + completeness test) and refuse:
- a `write_roles` entry not in `tenancy.member_roles` (design.rs:273) — each must be a declared member role;
- `write_roles` on a **non-tenant-scoped** bucket (or a design with no `tenancy`) — it's meaningless there and silently ignoring a declared write gate is a security footgun; refuse loud (mirror the JC0545 storage-facet / JC0549/0550 refusal style). (Message references `jerrycan explain JC0556`, matching the loop's plain-`q()`+coded-doc convention.)

### D. Acceptance test — prove a non-write-role member is 403
Extend `storagegen::bucket_tests` (storagegen.rs:473-587): when `write_roles` is set on a tenant-scoped bucket, emit `{ident}_upload_by_non_writer_is_403` + `{ident}_remove_by_non_writer_is_403` — a member holding a role NOT in `write_roles` gets 403 on upload/remove, while a write-role member succeeds (mirror `{ident}_download_by_non_member_is_403` :522-526). **Test-infra:** the acceptance seed (storagegen.rs:379-382) seeds both members with `member_roles[0]` (admin); extend it to seed a third principal / a member with a NON-write role so the 403 case is exercised. This seed extension is the main scope cost.

### E. #133 mitigation (docs)
Document in the storage doc (`docs/ai/18-storage.md` — or wherever buckets are documented — + the embedded twin, embedded_sync gate): an **owned** bucket (tenant- or user-scoped) shares a single key namespace across owners UNLESS `owner_prefix: true` is set; set `owner_prefix` for per-owner key isolation (it prefixes the stored key/blob path by owner, avoiding the cross-owner key oracle/squatting of #133). Do NOT change the runtime (the naive `key_exists` scoping corrupts data — see the #133 re-scope; the proper stored-key-namespacing fix is deferred).

## Non-goals / documented
- `UserInTenant` write-role gating (v1 gates `Tenant` only). Per-object ACLs. A read-role/write-role split beyond `write_roles` (write only). The #133 runtime stored-key namespacing (deferred — migration-bearing). Gating `sign` (it's a read grant).

## Success criteria
- A tenant-scoped bucket with `write_roles: ["admin"]`: a member holding only a non-write role gets **403** on `upload`/`remove` (was: 201/204); a write-role member succeeds; `sign`/`download` are unaffected. The generated acceptance suite proves the 403.
- `write_roles` with an undeclared role, or on a non-tenant bucket → `JC0556`.
- A bucket with no `write_roles` is byte-identical; heavy gate green; `cargo semver-checks` clean; published 0.6.9. The storage doc guides `owner_prefix` for per-owner key isolation.
