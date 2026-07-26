# Dynamic `now` default for datetime fields (0.6.10) — #110

**Date:** 2026-07-26
**Status:** Approved design, pre-implementation
**Issues:** #110 (the design contract's `default` only accepts static literals — a server-set timestamp like `created_at`/`applied_at`/`sent_at` can't declare its intent and is forced into a lossy `required:false` workaround; every eval app with a server timestamp hit this).
**Ships as:** 0.6.10 — an additive design-contract feature. One dependency-free `jerrycan-core` helper (additive) + codegen. Byte-identical for any datetime field that doesn't use `default:"now"`.

## Decision: handler-set (Option A), NOT a DB DEFAULT
The generated repo `insert` returns **only the id** (genroute.rs:2318-2341, 1418-1431) — it never reads back the persisted row — and datetime rides as a **TEXT** column (genroute.rs:2481) with no column DEFAULTs emitted. So a DB `DEFAULT CURRENT_TIMESTAMP` would (a) never appear in the response without rearchitecting every insert to `RETURNING *` (out of scope, byte-identity-breaking) and (b) produce a non-RFC3339, backend-divergent string. Instead, reuse the #53 server-owned-default pipeline: a `now` field is **omitted from the request DTO** and the **handler sets it to the current time** — matching datetime-as-`String` and paving the cowpath agents already hand-roll.

## A. Contract: `"default": "now"` sentinel on a datetime field
No schema/model change needed — `Field.default: Option<serde_json::Value>` (design.rs:190) already accepts any JSON value, and the schema's `default` is untyped. The exact lowercase string `"now"` on a `type: "datetime"` field is the sentinel. Add a classifier `Design::field_is_now_default(f) -> bool` = `f.field_type == Datetime && f.default == Some(json!("now"))`. (`"now"` is unambiguous: there is no RFC3339 parse for datetime defaults today, and `"now"` is never a valid timestamp — no collision with a legitimate static datetime default.)

## B. Validation — `JC0557` (next free after JC0556)
In `default_type_error` (questions.rs:105) / the field-validation call site (:897), intercept the `now` intent **before** the generic datetime `is_string()` acceptance:
- `"now"` on a **non-datetime** field (`string`/`integer`/`boolean`/`float`/`uuid`/`json`) → **`JC0557`** ("`now` is a server-set-timestamp sentinel — valid only on a `datetime` field; use a static default here, or change the type to `datetime`"). (On `string`, `"now"` is otherwise a legitimate literal, so the refusal is what disambiguates intent.)
- a non-lowercase casing (`"NOW"`, `"Now"`) on a datetime field → `JC0557` with a hint pointing at the exact `"now"` (never silently mis-read a near-miss as a literal).
- `now` requires a `db` dependency (memory mode: reuse the existing db-required path in default_type_error, :107-112).
Register JC0557 in codes.rs + `explain` + completeness test.

## C. Emission — reuse #53 omission; server-set on create; immutable on update
- **Create DTO omission (reused):** a `now` field `f.default.is_some()`, so it's already dropped from `{Entity}Request` on CREATE (genroute.rs:1128), from the OpenAPI request schema (openapi.rs:251), and from the testgen create probe body (testgen.rs:229). No new omission code for create.
- **Update DTO — DIVERGE from #53 (server-owned, set-once):** a static `default` field is KEPT (client-settable) on UPDATE, but a `now`/timestamp field is **set-once and immutable** — a client must not rewrite `created_at`. So OMIT a `now`-default field from `{Entity}UpdateRequest` too (a small, deliberate divergence: extend the update-DTO drop predicate — genroute.rs:1084-1092/1128 and openapi.rs update-schema — to also drop `field_is_now_default` fields). Document the divergence.
- **Handler steer (create):** in `server_owned_fk_comment` (genroute.rs:595-632), a `now` field's steer says: set `{field}` to the current time via **`jerrycan::now_rfc3339()`** (not a static literal). Keep static-default fields' existing steer. On UPDATE the field is omitted from the DTO, so no update steer (the handler leaves it untouched — it stays as stored).
- **Response (reused):** the field stays in the entity Model → present in every response (openapi.rs:110-140 response schema unchanged).

## D. Runtime helper (jerrycan-core, additive)
Add `pub fn now_rfc3339() -> String` (RFC3339 UTC, e.g. `chrono::Utc::now().to_rfc3339()` — or extend the existing `Clock` DI with an `.now_rfc3339()` if cleaner) to jerrycan-core, **prelude-exported** so the generated handler can call `now_rfc3339()` under `use jerrycan::prelude::*`. Add `now_rfc3339` to `RESERVED_PRELUDE_IDENTS` (questions.rs, JC0546) — the 0.6.7 #129 drift tripwire will FAIL if this is missed, so it's self-enforcing. Semver: additive public API (jerrycan-core minor). This removes the agent's need to add `chrono` ad hoc and guarantees a correct RFC3339 format matching the datetime fixtures/OpenAPI.

## E. testgen / OpenAPI (mostly reused — verify)
- testgen: the happy-path probe asserts only status + `body["id"]` (testgen.rs:718-727), and the create fixture omits default fields (:229) → a `now` field's dynamic value breaks nothing; it's omitted from the request. Verify no test asserts a now-field value.
- OpenAPI: request omits (server-owned), response present — reused; verify the update schema also omits the `now` field (per §C).

## F. Docs + conformance
- `docs/ai/00-designing.md` (the `default` field docs ~:211, and ~:299) + `docs/ai/08-database.md` (or the field reference) + embedded twins (embedded_sync gate): document `"default": "now"` on a datetime field = a server-set-on-create, immutable-on-update timestamp, set via `now_rfc3339()`, omitted from both request DTOs, present in responses. Note the deferred forms (`now+Nd`, now-on-update).
- Conformance e2e (db mode): a design with a `created_at` datetime `default:"now"` → scaffold → the create DTO omits `created_at`, the handler steer references `now_rfc3339()`, the response includes `created_at`, and `jerrycan check` is green on a correct handler. Model on an existing conformance e2e.

## Non-goals (deferred / documented)
- `"now+7d"` relative form (needs an offset parser — no eval demand). now-on-**update** (`updated_at`/`touch`/`on_update` — needs new update-overwrite machinery that doesn't exist). Reverse-migrator round-trip (`DEFAULT now()`→`default:"now"`). RFC3339 parse-validation of *static* datetime defaults (a pre-existing footgun — a possible follow-up, not v1). Native datetime types (Phase 2).

## Success criteria
- A `datetime` field with `"default": "now"`: omitted from `{Entity}Request` AND `{Entity}UpdateRequest`; the create handler steer points at `now_rfc3339()`; the field is present in responses; a correct scaffolded app passes `jerrycan check`.
- `"now"` on a non-datetime field (or a bad casing) → `JC0557`.
- A datetime field WITHOUT `default:"now"` is byte-identical; `now_rfc3339` is in `RESERVED_PRELUDE_IDENTS` (drift tripwire green); heavy gate green; `cargo semver-checks` clean (additive core helper); published 0.6.10.
