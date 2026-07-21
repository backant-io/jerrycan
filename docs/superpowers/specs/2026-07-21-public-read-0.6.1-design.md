# Public-read / owner-write ownership shape (0.6.1) — #105

**Date:** 2026-07-21
**Status:** Approved design, pre-implementation
**Issues:** #105 (no public-read/owner-write shape — auctions, feeds, job boards, blogs can't express "anyone reads, only the owner writes")
**Origin:** round-5 eval (feed/auction/jobs apps).
**Ships as:** 0.6.1 — additive, opt-in per entity (serde-default absent → every existing design byte-identical; framework Rust API unchanged, `cargo semver-checks` clean).

## Problem
An identity-owned entity (`belongs_to` the auth identity, #79) today is either fully owner-scoped (all reads+writes gated to the caller) or, if a GET is made unguarded, **unimplementable** (a latent bug — see §E). There is no "reads are public, writes are owner-scoped" shape, which is the mainstream feed/post/listing model. **Precedent exists in-repo:** storage `visibility: public` buckets already do exactly this (open reads + guarded owner-stamped writes, `storagegen.rs:170,197-254`; blessed by validation `questions.rs:946-950`). This lifts that shape to HTTP entities.

## Design

### A. Contract: `public_read` on the entity
Add `public_read: bool` (serde-default `false`, `skip_serializing_if not`) to `Entity` (design.rs:145-155). It is a **modifier on the per-user (#79) ownership shape** — valid ONLY on an identity-owned, non-tenant entity with an auth model. When `true`:
- **Reads** (GET list + detail) are **public** (unguarded, unscoped): the handler takes no `CurrentUser`, and the repo emits the unscoped `all()`/`get()`. A public list returns the **whole collection** (every owner's rows — the feed intent; stated explicitly).
- **Writes** (POST/PUT/PATCH/DELETE) stay **owner-scoped and guarded** exactly as #79: `auth_required`, server-injected `user_id` on create (#34), `owner_scoped_methods` (`update_for`/`remove_for` keyed on `user_id`, 404 for a non-owner — hide existence), unscoped `update`/`remove` suppressed.

### B. Generator (the third repo state + guarding split)
- `sql_repo` (genroute.rs:2162-2178) gains a THIRD state (today suppression is all-or-nothing): for a `public_read` per-user entity, emit the unscoped `all`/`get` **reads**, keep `owner_scoped_methods` **writes**, and still suppress unscoped `update`/`remove`. (`insert` always emitted.)
- `handler_params` (genroute.rs:234-240): a GET on a `public_read` entity is treated as **unguarded** (no `CurrentUser`) regardless of its declared `auth_required`; writes stay guarded. Drive this from the entity flag so it's correct-by-construction (the user doesn't hand-set `auth_required:false` per GET).
- `owner_scope_comment` (genroute.rs:459-480) branches: for a `public_read` read handler steer to the unscoped `repo.all()`/`get(id)` (public), not `all_for(_user.0.id)`; writes keep the owner-scoped steering. (This also fixes the §E latent bug's incoherent stub.)
- OpenAPI: no change needed — `openapi.rs:46-52` already omits `security` on unguarded ops; the public GETs lose their security stanza automatically, writes keep it.

### C. Lint (JL0006 false-positive)
The identity-owned handler scan (lints.rs:167-190, needles `all/get/remove/update`) would flag the now-legitimate unscoped `repo.all()`/`get()` in a public-read module. For a `public_read` module, restrict the needles to `remove/update` (writes only) — mirroring the existing per-module `flag_insert` config (lints.rs:217-219). JL0004 (unguarded-mutation) MUST keep firing — the flag never exempts a write.

### D. Validation (`JC0549`) + security
Writes on a public-read entity being public is the open door — refuse it. New **`JC0549`** (next free; codes.rs registry) raised in questions.rs, firing when a design uses `public_read: true` AND any of:
- a write endpoint (POST/PUT/PATCH/DELETE) of that entity is `public: true` or not `auth_required` (writes must stay owner-gated);
- the entity is NOT identity-owned (`has_identity_fk`, design.rs:1033) or IS tenant-owned (mirror the tenant-public rejection questions.rs:814-844) — public_read is identity-owned-only in v1;
- no auth model exists (writes need a session — mirror questions.rs:558-570).

### E. Close the latent unguarded-per-user-GET bug (do it here)
Independently of opt-in: an unguarded GET on a per-user entity that has NOT opted into `public_read` is currently expressible (`auth_required:false`, not `public`) and generates an incoherent, unimplementable stub (`owner_scope_comment` steers to `all_for(_user.0.id)` but `handler_params` emitted no `_user` and the repo has no unscoped read). Add validation: a per-user entity's GET may be unguarded ONLY if the entity is `public_read` — otherwise `JC0549` (or a sibling message) "an unguarded read on an owner-scoped entity is unimplementable; set `public_read: true` to make reads public, or keep the GET authenticated." Turns a silent dead-end into a clear fork.

### F. Sync triple + tests + docs
- **Sync triple:** the `public_read` awareness lands in ALL THREE mirror sites at once — `entity_is_per_user_owned`/emission (genroute.rs:2017), testgen (testgen.rs:1184), lints (lints.rs:382) — the drift that shipped #102-class holes.
- **Testgen:** a `public_read` variant of `per_user_isolation_test` (testgen.rs:1203-1296): **anon GET list → 200 containing another user's row** (proves public read), anon GET detail → 200, anon POST → 401 (writes still guarded), **non-owner PUT/DELETE → 404** with the row surviving (owner-write), owner PUT → 200. New `SECURITY (#105)` doc-comment sibling to the `SECURITY (#79)` one.
- **Docs:** `docs/ai/00-designing.md` (public-flag semantics), `docs/ai/10-auth.md`/`14-tenancy.md` (ownership shapes) + their embedded twins (embedded_sync byte-identity gate) — add the public-read/owner-write shape; today they teach "public is for login/register/webhooks only".
- **Conformance:** add a `public_read` entity (a `Post`/`Listing`) to a fixture so the shape is exercised end-to-end.

## Non-goals / documented
- **No tenancy composition** in v1 (public_read is identity-owned-only; tenant-public stays rejected). A tenant-scoped public-read is a later extension.
- **No field-level redaction:** a public read serializes the whole entity including the `user_id` owner fk (fine for posts; field hiding is #112 `write_only`).
- **Realtime scope stays orthogonal** (a public_read entity's `changes` topic keeps its declared `RealtimeScope`).
- "public detail, private list" (per-endpoint split) is not expressible via the entity flag — a later per-endpoint spelling if needed.

## Success criteria
- A `public_read` identity-owned entity: anon GET list returns every owner's rows (200); anon write → 401; non-owner write → 404 (row survives); owner write → 200. The generated acceptance test proves all four.
- A `public_read` on a tenant-owned/non-identity/no-auth design → `JC0549`. An unguarded GET on a non-`public_read` per-user entity → `JC0549` (was: unimplementable stub).
- JL0006 stays silent on the public-read module's unscoped reads but fires on an unscoped write; JL0004 still fires on any unguarded write.
- Non-opt-in designs byte-identical; `cargo semver-checks` clean; heavy gate green; published 0.6.1.
