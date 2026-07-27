# Classification honesty — resolver alignment + mixed-shape refusal + memory-default docs (0.6.16)

**Date:** 2026-07-27
**Status:** Approved design, pre-implementation
**Issues:** #143, #171, #175, #161 — four review-filed residuals in the same family: the validator / lint / steer classifies an endpoint or entity DIFFERENTLY from what codegen actually targets, so a broken-stub / dishonest-green ships (or a docs gap). None is a live security hole; all are "green when it should be red-or-refused." Batched as one cleanup release (the user asked to clear the small residuals first).
**Ships as:** 0.6.16 — validation refinements (#143, #175) + one genroute emission fix (#171) + a docs note (#161). Byte-identical except for the specific broken shapes each part targets.

## Part A — #143: public bodyless mutation mis-attributed by the lenient resolver escapes JC0549(b)
`check_public_read`'s write-leg (questions.rs:~1550) resolves an endpoint's entity with the **lenient** `endpoint_repo_entity` (first-entity fallback), with a comment claiming "over-matching only asks (fail-CLOSED)." But lenient can also **under**-match: a bodyless `public`/unguarded `DELETE /{id}` in a multi-entity module where the `public_read` entity is **not first** resolves to the *first* entity; if that first entity isn't `public_read`, the `e.public_read && (ep.public || !ep.is_guarded())` guard is false → **JC0549(b) never fires**, and the design ships green with an unimplementable stub for the public_read entity's write.

**Fix:** make the write-leg fire when EITHER the lenient OR the **strict** (`endpoint_repo_entity_strict`, #56 collection-creator resolution, no fallback) resolution lands on a `public_read` entity for a `public`/unguarded write. Concretely: resolve both; for each resolved entity that is `public_read`, if the write is `public` or `!is_guarded()`, refuse (dedupe so one endpoint yields one question). This keeps the existing over-match behavior AND catches the strict-resolved non-first case. If BOTH resolutions are `None`/non-public_read, no change (byte-identical). **Regression test:** a multi-entity module with the `public_read` entity SECOND + a bodyless `public` `DELETE /{id}` that #56-resolves to it → JC0549(b) fires (was: green). Confirm a single-entity / first-entity public_read design is unchanged.

## Part B — #171: anonymous handler on a tenant-owned module gets a lenient tenant-owned repo + a broken scope comment
`handler_params` (genroute.rs:~194) binds `_repo: Dep<{e}Repo>` via the **lenient** `endpoint_repo_entity`, and `tenant_scope_comment` emits a membership-set scope comment — both fire for an **anonymous** (no guard param) custom GET on a tenant-owned module (e.g. reference-slice `/usage`), so the stub holds a tenant-owned repo and a comment that references `_user.0.id`/`_id` params that **don't exist in the signature** (won't compile if followed; "fixing" toward the unscoped repo method is a cross-tenant read).

**Fix (minimal, byte-identity-scoped):** suppress the **membership-set scope comment** (`tenant_scope_comment`) when the handler is anonymous — i.e. `mode.auth && !ep.is_guarded()` (no `Dep<Tenant>` and no `CurrentUser`) on a tenant-owned entity. The comment steers to a `_user`-scoped call that is impossible without a session, so on an anonymous handler it is pure misdirection; dropping it is strictly an improvement. **Leave the `_repo` binding as-is** (for `/usage` the ApiKeyRepo binding is the intended repo — the broken part is the comment, not the binding). Emit instead a short honest TODO on an anonymous tenant-owned handler: "// custom action on a tenant-owned entity with no session — scope this read yourself; there is no _user/_tenant to scope by." Byte-identical for every guarded handler and every non-tenant handler. Prove with a determinism run + a unit test on the reference-slice `/usage` shape (no `_user.0.id`/`_id` reference in the emitted comment).

## Part C — #175: refuse a mixed-shape tenant entity (JC0562)
`entity_is_flat_tenant_owned` (genroute.rs:~1515) is `!saw_path_scoped && saw_flat`; an entity with BOTH a `MembershipSet` flat write AND a `PathScoped` route resolves to `false` → the `*_for_memberships` methods are withheld, but the flat-write steer still fires → method-not-found behind a green check (the #116 class, for a shape no design has today).

**Fix:** register **JC0562** (next free after JC0561; codes.rs + explain + completeness test, mirror JC0561) and refuse, in the per-entity validation loop, an entity that is reachable by BOTH a `MembershipSet` and a `PathScoped` tenant route across the design (reuse the `scan` logic — expose a `Design::entity_tenant_shapes(e) -> (bool saw_path, bool saw_flat)` helper, or replicate the scan in questions.rs). Message: "Entity `X` mixes a flat (body-fk) write and a path-scoped route — the generator can emit only one scoping shape, so following the generated steer would call a `*_for_memberships` method that isn't emitted. Give `X` a single shape: make every route path-scoped (`/{fk}/…`), or every route flat. See `jerrycan explain JC0562`." Zero-design impact (no corpus design is mixed — determinism + heavy gate stay green); pure make-impossible. **Tests:** a synthetic mixed-shape design → JC0562; a pure-flat and a pure-path-scoped design → no JC0562.

## Part D — #161: document the memory-mode absent-optional-bounded default (docs only)
In **memory** mode an optional field is emitted bare-typed with `#[serde(default)]`, so an absent value materializes as the serde default (`0`/`""`) — which can sit outside a declared `min` bound (db mode stores NULL — honest absence). Low severity; the issue's chosen resolution is **document, not change**. Add a note to `docs/ai/08-database.md` (the field/constraint reference) + the embedded twin (`crates/jerrycan/embedded/ai/08-database.md`, byte-identical — embedded_sync gate): "In memory mode, an absent optional field reads back as its type default (`0`/`""`), which may sit outside a `min`/`max` bound; db mode stores NULL (true absence). Set a `default` to control the absent value, or use db mode for NULL semantics." Edit BOTH twins identically.

## Byte-identity + gates
- Part A/C are validation-only; Part B changes emission ONLY for an anonymous tenant-owned custom handler (no corpus design except reference-slice `/usage` has it — verify the reference-slice scaffold delta is limited to that comment). `determinism.rs` + base-vs-HEAD scaffold `diff -r`.
- **Heavy eval gate (0.6.11 lesson):** `reference_eval` + `conformance` + `eval` `--include-ignored` before done (Part B changes reference-slice's `/usage` handler stub comment — confirm the reference battery stays green).
- `cargo semver-checks` clean (validation + internal genroute + docs — no public API change).

## Success criteria
- #143: a public bodyless DELETE on a non-first public_read entity → JC0549(b); existing designs unchanged.
- #171: the anonymous `/usage` stub emits no `_user`/`_id`-referencing scope comment (an honest TODO instead); guarded handlers byte-identical.
- #175: a mixed-shape entity → JC0562; single-shape designs unaffected.
- #161: the memory-default semantic is documented in both doc twins.
- Heavy gate green; semver clean; published 0.6.16; #143/#171/#175/#161 closed.

## Non-goals
- Emitting BOTH method sets for a mixed entity (#175 refusal is the interim; no demand). Strict-resolving the `_repo` binding globally (#171 keeps the binding, drops only the comment). Changing memory-mode optional emission to `Option<T>` (#161 is docs-only per the issue).
