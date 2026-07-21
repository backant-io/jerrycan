# Storage tenancy: transitive owners + honest status (0.5.4)

**Date:** 2026-07-20
**Status:** Approved design, pre-implementation
**Issues:** storage transitive gap (realtime/HTTP analogue of #102, deferred from 0.5.1), #109 (authenticated non-member gets a misleading 401 on a tenant bucket instead of 403/404)
**Origin:** round-5 eval (fileshare) + the 0.5.4 storage-tenancy investigation.
**Ships as:** 0.5.4 (security patch — a grandchild-owned bucket is currently UNSCOPED; plus honest status. One additive `jerrycan-core` public API; `cargo semver-checks` clean.)
**Deferred:** owner-write/shared-read role split (#132, gated on #107), cross-scope key oracle (#133), first-membership arbitrariness (storage facet of #104 → 0.6.0).

## Problem
1. **Transitive owner unscoping (security).** `owner_belongs_to_tenant()` (storagegen.rs:69-75) is the pre-0.5.1 DIRECT `belongs_to == tenant` predicate. A bucket whose `owner` is a *transitively* tenant-owned (grandchild) entity fails it → `bucket_scope()` (storagegen.rs:81) falls through to `BucketScope::User` → the bucket gets **no `Dep<Tenant>` guard and no `tenant_id` stamping** (storagegen.rs:113). Any authenticated user — member of nothing — can upload/read/delete rows in a bucket that was meant to be tenant-scoped. `check` is green.
2. **#109 misleading status.** The private `download` handler takes `Option<Dep<Tenant>>` (storagegen.rs opt_guard, :106/:114/:122/:130) so the same route also accepts a signed URL. But `impl FromRequest for Option<T>` (extract.rs:370-374) discards the inner error, and the fragment rebinds `None → Error::unauthorized()` (401). So an authenticated **non-member** whose tenant guard correctly produced **403** (scaffold.rs:134) is reported as **401** — a misleading status that hides the real authz outcome.

## Design

### A. Error-preserving optional extractor (jerrycan-core) + honest download status (#109)
The status is destroyed before the handler runs, so this cannot be codegen-only.
- **jerrycan-core:** add an error-preserving optional extractor beside `extract.rs:370` — `impl<T: FromRequest> FromRequest for Result<T, Error>` (returns `Ok(Ok(v))` on success, `Ok(Err(e))` on the inner extractor's error, never fails the request). Additive public API. (A `Result<T,Error>` extractor is the idiomatic shape; keep the existing `Option<T>` extractor as-is for genuine "absent is fine" cases.)
- **storagegen:** for the private `download` on a tenant-scoped bucket, bind the guard as `Result<Dep<Tenant>, Error>` and, when no valid signed URL is presented, `let tenant = tenant?;` — so a missing session surfaces **401** and an authenticated non-member surfaces the guard's **403** (and a foreign-tenant member still gets a scoped **404** from `get_scoped`). The signed-URL branch short-circuits before the bind, unchanged.
- Tests: update storagegen.rs:703-705 (was `Option<Dep<Tenant>>` + `ok_or_else(unauthorized)`) and the doc comment :221-223 ("reads as 401"); add a generated **non-member-403** acceptance negative control alongside the existing `download_without_auth_is_401` and tamper-401.

### B. Transitive bucket owner + ambiguity refusal
- **storagegen:** replace `owner_belongs_to_tenant` (delete :69-75) with `design.tenant_path(owner).is_some()` at the `bucket_scope` classification (:81). Storage needs `tenant_path` **only as a boolean classifier** — it resolves the tenant at guard time (`Dep<Tenant>`) and stamps `tenant_id` onto `storage_objects` itself, so (unlike HTTP #102) it needs no `join_sql`, and (unlike realtime #113) no refusal — the direct `Some(zero-join)` case subsumes today's behavior, so **direct-owned buckets are byte-identical** and only grandchild-owned buckets change (from the broken `User` scope to the correct `UserInTenant`, dragging the guard + stamp + acceptance tenant plumbing along). Add a transitive variant of the classification test (storagegen.rs:719-730).
- **Ambiguity refusal (questions.rs):** a diamond bucket owner makes `tenant_path` return `None`, which would *silently degrade* the bucket back to `User` scope. Refuse it: in bucket validation (~questions.rs:889), if `design.tenant_path_branch_count(owner) >= 2`, raise **`JC0545`** (the existing ambiguous-tenant-path error — reused, same meaning) so an ambiguous bucket owner fails `check` rather than degrading. (A bucket owner that is the tenant itself, or has no tenant path, stays `User`/`Unowned` as today — only the ≥2-branch case is refused.)

### Ordering / byte-identity
- A lands **with or before** B: B mints new `Result<Dep<Tenant>,Error>` download sites for the newly-`UserInTenant` grandchild buckets, i.e. new swallowed-403 surface if A isn't in place. Ship as one task.
- Byte-identity: direct-owned tenant buckets, user/unowned buckets, and non-storage apps are unchanged. Only (i) tenant-scoped `download` handlers (Option→Result shape, status change) and (ii) grandchild-owned buckets (User→UserInTenant) change — both intended. Update `storagegen` determinism/snapshot tests + the storage acceptance battery where they legitimately change; `docs/ai/18-storage.md` ↔ `crates/jerrycan/embedded/ai/18-storage.md` must move together (embedded_sync gate).

## Success criteria
- A bucket owned by a grandchild entity gets a `Dep<Tenant>` guard + `tenant_id` stamping; a non-member cannot access it (was: any authenticated user). Direct-owned buckets byte-identical.
- An authenticated non-member downloading a private tenant bucket gets **403** (not 401); an unauthenticated request still **401**; a foreign-tenant member **404**; a valid signed URL still works.
- A diamond bucket owner fails `check` with `JC0545`.
- `cargo semver-checks` clean (additive `jerrycan-core` extractor); heavy gate green; published 0.5.4.
