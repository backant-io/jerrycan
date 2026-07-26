# Non-entity (inline-DTO) request body + auth-aware probe:skip TODO (0.6.15) — #122

**Date:** 2026-07-26
**Status:** Approved design, pre-implementation
**Issues:** #122 (MEDIUM — `design.json`'s `request_body` can only reference a table entity. A custom action whose body is an ad-hoc DTO (not a row) — `POST /checkout { items, coupon }`, `POST /search { query, filters }` — can't be declared, so the builder must `probe:skip` the endpoint and hand-write a DTO invisible to OpenAPI/schema. As a side effect the `probe:skip` path emits auth-flavored credential/401 TODO wording even in apps with no auth. Hit by round-5 app: commerce.)
**Ships as:** 0.6.15 — an additive contract feature (inline-DTO body) + a papercut fix (auth-aware TODO). Byte-identical for every `request_body` that references an entity (today's shape) and every `probe:skip` in an auth design.

## Part A — inline-DTO request body

### A1. Contract: `request_body` = entity XOR inline fields
`RequestBody` (design.rs:522) is `{ entity: String }`. Change to accept EITHER an entity ref (today) OR inline fields:
```rust
#[serde(deny_unknown_fields)]
pub struct RequestBody {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
    /// Inline DTO body (issue #122): a custom-action body that is not a table row.
    /// Exactly one of `entity` / `fields` is set (JC0561). Reuses `Field` (types +
    /// #80 constraints). No pk, no belongs_to — a plain request struct.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<Field>,
}
```
`entity` becomes `Option` — **audit every `rb.entity` access** (genroute.rs:159, 553, 1086, 1101, and the `request_body.as_ref()?.entity` in `request_dto_name`:171; openapi.rs; schema.rs) and branch on inline vs entity. A helper `RequestBody::is_inline(&self) -> bool` = `self.entity.is_none()` keeps the sites readable.

### A2. Inline DTO struct name + emitter
An inline body needs a name, so the endpoint MUST have an `operation_id` (JC0561 if missing). The DTO is `{Pascal(operation_id)}Request` (e.g. `checkout` → `CheckoutRequest`). Add `inline_request_dto_rs(op_id, fields, design) -> String` — a PLAIN struct (NO pk `id`, NO belongs_to, NO server-owned omission — none of the entity machinery applies), reusing the field-emission loop from `request_dto_rs` (genroute.rs:1161-1175): `keyword_field_attrs`, `rust_ident`, `constraint_validate_attr` (the #80 validators), `f.field_type.rust_type()`, required→`T` / optional→`#[serde(default)] Option<T>`. Emit it in the module's DTO section wherever `{Entity}Request` structs are emitted, once per inline-body endpoint (dedupe by op_id).

### A3. Handler param + DTO-name plumbing
- `request_dto_name` (genroute.rs:150): for an inline body, return `Some("{Pascal(op_id)}Request")`. For an entity body, unchanged.
- `endpoint_takes_request_dto` / `endpoint_uses_request_dto`: an inline body ALWAYS takes the DTO (there is no plain-entity fallback). The handler param (genroute.rs:248-256): the `Some(name)` branch already emits `Json(_body): Json<{name}>` — inline flows through it; ensure the `None` fallback (which uses `rb.entity`) is never reached for an inline body.
- Server-owned-fk steer / DTO doc: an inline DTO has no server-owned fields, so emit no omission doc for it.

### A4. OpenAPI + schema
- openapi.rs: the request schema for an inline body is built from `fields` (mirror the entity request-schema builder, but iterate `rb.fields` — a plain object schema with the field types + required set + #80 constraints). Register `{Pascal(op_id)}Request` in components. Every `rb.entity` access in openapi.rs branches on inline.
- schema.rs (`SchemaContract`): the endpoint's request shape carries the inline DTO (field names + types), so `jerrycan`'s schema output describes the custom action. Mirror the entity request-shape emission.

### A5. Validation — `JC0561` (next free after JC0560)
Register **JC0561** (codes.rs + explain + completeness test, mirror JC0560) and refuse:
- a `request_body` with BOTH `entity` and non-empty `fields`, or NEITHER (exactly one required).
- an inline body on an endpoint with no `operation_id` (the DTO would be unnameable).
- an inline `fields` that is otherwise invalid — reuse the existing per-`Field` validation (name charset, type, #80 constraint sanity, no duplicate field names). If the existing field-validation is entity-scoped, factor the field-level checks so the inline fields get the same JC0552/name checks (do NOT silently skip them).
Message cites `jerrycan explain JC0561`.

### A6. testgen for an inline body
An inline-body custom action has no seedable row, so a full happy-path probe may not be derivable — emit a create-style probe that POSTs a fixture body built from the inline fields (reuse `fixture_json`/`fixture_value` over `rb.fields`) and asserts the success status, OR, when the success needs a credential the generator can't synthesize, the existing `probe:skip` path (now auth-aware, Part B). Do NOT emit an entity-isolation/id probe (there is no entity). Keep it minimal; the goal is that the endpoint is IN the contract (OpenAPI/schema), not exhaustive testing.

## Part B — auth-aware `probe:skip` TODO
The `probe:skip` fallback emits credential/401-flavored TODO wording unconditionally (testgen.rs ~:670-695, the "write your own test file … its `_without_auth_is_401` guard test is already generated" text and any credential-mention). In a design with NO auth model (`design.wants_auth() == false` / no `auth`), that wording is misleading. Make the TODO text auth-aware: mention the credential/401 guidance ONLY when the design has an active auth model; otherwise emit a plain "write your own success test for this custom action" TODO with no auth references. Gate on the existing auth-model predicate. Byte-identical for auth designs.

## Byte-identity + gates
- An entity `request_body` (today's shape) serializes/derives identically — `entity` is still present, `fields` empty/skipped; every branched site takes the entity path. Prove via `determinism.rs` + base-vs-HEAD scaffold `diff -r`.
- A `probe:skip` in an auth design keeps its wording; only no-auth designs change (Part B) — pin with a no-auth-app testgen assertion.
- **Heavy eval gate (0.6.11 lesson):** `reference_eval` + `conformance` + `eval` `--include-ignored` before done.
- **Fixture proof:** a design with `POST /checkout` whose `request_body` is `{ "fields": [ {"name":"coupon","type":"string"}, {"name":"total","type":"integer"} ] }` and no auth → scaffold: the handler takes `Json<CheckoutRequest>`, `CheckoutRequest { coupon: String, total: i64 }` is emitted, OpenAPI documents it, `jerrycan check` is green on a correct handler, and the `probe:skip` (if used) TODO has NO auth wording. Add as a conformance/unit fixture.
- `cargo semver-checks`: `RequestBody.entity` changes `String → Option<String>` — this is a **field-type change on a public struct**. Verify: `RequestBody` is a `pub` platform-crate type; a type change may trip a semver lint. If it does, it is still an additive *capability* (every existing JSON round-trips), so scope-allow the lint on `crates/jerrycan` (as 0.6.1 did for `constructible_struct_adds_field`) — do NOT bump to 0.7. Confirm at release-prep.

## Success criteria
- `POST /checkout` with an inline `fields` body → `CheckoutRequest` DTO, handler `Json<CheckoutRequest>`, OpenAPI + schema document it, `jerrycan check` green.
- `request_body` with both entity+fields, or neither, or inline without operation_id → **JC0561**.
- A `probe:skip` endpoint in a NO-auth design emits a TODO with no credential/401 wording; in an auth design the wording is unchanged.
- Every entity `request_body` is byte-identical; heavy gate green; semver clean-or-scope-allowed; published 0.6.15.

## Non-goals
- A pure SCALAR body (`Json<String>`) — model it as a single-field inline DTO. A `response` inline DTO (this is request-only; success stays entity/status). Reusing an inline DTO across endpoints (name is per-operation).
