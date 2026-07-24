# Field length/range constraints (0.6.5) — #80

**Date:** 2026-07-24
**Status:** Approved design, pre-implementation
**Issues:** #80 (no field length/range surface — the #1 contract gap by eval demand: all 10 apps × 3 rounds, 11+ hand-written `Validate` impls).
**Ships as:** 0.6.5 — additive design-contract feature. Four new optional field keys; every existing design byte-identical (serde-default absent). Whole feature lives in the `jerrycan` CLI crate's codegen — **no runtime-crate change, no `jerrycan-validate`/`-core` release**. Semver clean.

## Problem
The field contract (`deny_unknown_fields`: name/type/required/unique/index/values/default) can't express `max 100`, `1..=600`, `> 0`, `max_len 280`. Every spec with such a constraint forces the agent to hand-write a `Valid` impl — which then rejects the generator's `"test-value"` fixture, making the 2xx probe un-greenable, forcing `probe:"skip"`, which cascades to drop sibling `/{id}` success probes (#68). Declaring the constraint breaks the whole chain: the generator knows the bound, derives an in-range fixture, and no skip is needed.

## Design decision: extend the deserialize-validator, do NOT activate `Valid<T>`
Generated apps validate at **deserialize time** via `#[serde(deserialize_with = "de_{entity}_{field}")]` (the "R1" mechanism, issue #47) — surfaced as **422 `JC0422`** by the `Json` extractor. `values` already rides this. The dormant `jerrycan-validate` `Valid<T>`/`Validate` trait (structured multi-violation `details`) is **not** used by codegen. v1 extends the deserialize-validator: byte-safe, precedent-consistent, zero handler-signature churn, zero runtime-crate change. Accepted v1 limitations (documented, not fixed here): the 422 body is the unstructured `"invalid JSON body: …"` and serde stops at the first offending field. Activating `Valid<T>` for structured multi-field details is a tracked future enhancement (much larger blast radius, same eval payoff).

## A. Contract: four optional field keys
Add to `Field` (design.rs:169-191), mirroring the `default` precedent (`#[serde(default, skip_serializing_if = "Option::is_none")]` + a `#80` doc comment):
- `min: Option<i64>`, `max: Option<i64>` — inclusive integer range; **integer fields only** (v1).
- `min_len: Option<u64>`, `max_len: Option<u64>` — inclusive string length in **Unicode code points** (`chars().count()`, matching JSON Schema `minLength`/`maxLength`, not bytes); **string fields only**, and **not combined with `values`**. `u64` makes a negative length a parse error for free.

Mirror all four in the published `docs/contracts/design-schema.json` field def (add to `properties`, keep `additionalProperties: false`; `min_len`/`max_len` get `"minimum": 0`) — the schema↔model sync is pinned by `tests/contracts.rs`. `skip_serializing_if = Option::is_none` keeps `canonical_design_json` byte-identical for unconstrained fields.

## B. Design-time validation — `JC0552` (next free code)
In `check_relations_and_enums` (questions.rs, beside the `values`/`default` checks), register **`JC0552`** in codes.rs + `explain`, and refuse (each with a JSON-pointer id `{ptr}/entities/{i}/fields/{j}/<key>`):
- `min`/`max` on a non-integer field; `min_len`/`max_len` on a non-string field.
- `min > max` or `min_len > max_len` (empty range — no in-range fixture derivable; un-greenable by construction).
- `min_len`/`max_len` combined with `values` (the enum already fixes the strings; refuse the contradiction, mirroring "values only on string").
- `max_len == 0` on a `required` field (unfillable).
- any constraint key on the pk `id` field (the id-echo probe + seeds assume free ids — refuse).
- a `default` that violates its own field's constraints — extend the existing `default_type_error` (questions.rs:105-140) to range/length.
- a sanity ceiling on `min_len` (testgen must materialize `"a".repeat(min_len)` — cap at e.g. 4096 to bound fixture size).

(Negative lengths are already rejected by `u64` at parse + `"minimum": 0` in the schema.) Not contract-version-gated (like `default`/`public_read`).

## C. Generator (extend the `values` emitter — the template)
- `enum_validate_attr` (genroute.rs:705-715) → generalize to `constraint_validate_attr`: a field with `values` OR any constraint key gets the `#[serde(deserialize_with = "de_…")]` attr; unconstrained + no-values → returns `""` (the byte-identity gate — unchanged output for existing fields). Keep the three attach sites and their prefixes exact: `model_rs` (memory, `""`), `model_rs_db` (`"super::"`), `request_dto_rs` create+update (`""`).
- `enum_deserialize_fns` (genroute.rs:722-760) → emit the `de_{snake}_{field}` body for range/length in addition to the enum allow-list, with the **required** (`i64`/`String`) and **optional** (`Option<…>`, checks only when `Some`) variants. Body: deserialize the inner type, then check the bound, else `serde::de::Error::custom("{field} must be …")` (message names the bound). Length uses `.chars().count()`.
- **OpenAPI** (`openapi.rs`): change `field_schema(t: FieldType)` → `field_schema(f: &Field)` (both call sites: entity component + request DTO) and emit `minimum`/`maximum` (integer) and `minLength`/`maxLength` (string). Do NOT backfill `values`→`enum` (separate cleanup — would diff existing enum documents).
- **Migration CHECK** (defense-in-depth, mirrors `values` at genroute.rs:2433-2436): emit `CHECK (col BETWEEN min AND max)` / `CHECK (length(col) BETWEEN min_len AND max_len)` for constrained columns. (`length()` in SQLite counts characters, in Postgres counts characters — consistent for the CHECK.)
- Byte-identity gates everywhere: an unconstrained field's model/DTO/OpenAPI/DDL output is unchanged, pinned by a no-drift unit test (mirror genroute.rs:6707-6717) + `determinism.rs`.

## D. testgen (constraint-aware fixtures + the out-of-range probe)
- `fixture_value` (testgen.rs:13-35): derive an **in-range** value — integer clamped into `[min, max]` (default `1`, else nearest in-range); string of valid length (`"test-value"` if within `[min_len, max_len]`, else `"a".repeat(min_len)` / a truncation ≤ `max_len`). Keep the `seed_sql_value` / `seed_sql_value_n` (testgen.rs:1699-1744, unique variants `'seed-test-value'`/`1000`/`'test-value-{n}'`) in agreement — a `max`/`max_len` must not break the seeds (the invariant at testgen.rs:12).
- Out-of-range 422 probe: generalize the `bad_enum` corruption (`fixture_json`, testgen.rs:63-114) to `bad_constraint: Option<(field, out_of_range_literal)>` — `max+1`/`min-1` (i64 `checked_add`/`checked_sub`; if the bound is at the i64 extreme, skip that direction), `""`/under-min-len string / an over-`max_len` string. Emit one `{op}_rejects_out_of_range_{field}` per constrained request body on the create AND update paths (mirror the enum-reject sites testgen.rs:541-598), asserting **422**. **MUST increment `out.reject`** (testgen.rs:313-316/708) so `expected_failing` math (`test_count − reject`) stays correct. Skip the probe for a defaulted/omittable field on CREATE (a dropped field can't be rejected — the existing `first_enum_field` rule at testgen.rs:332-342).

## E. Docs + conformance
- `docs/ai/00-designing.md`: add the four keys to the field list (~:155-197); **rewrite the `probe:"skip"` caveat block (~:296-303)** — length/range are now expressible (declare them, no skip needed); email/url/regex/pattern remain the documented skip case. Update the byte-identical embedded twin `crates/jerrycan/embedded/ai/00-designing.md` (embedded_sync gate).
- **New** conformance design `conformance/designs/<name>.design.json` with a constrained field (an integer `min`/`max` + a string `max_len`) — auto-discovered by `contract_compat.rs` — plus an inline conformance test (correct handlers) proving the happy path is green AND the out-of-range request is 422 (model on `public_read_feed_goes_green` / `second_entity_id_probes_go_green`). Do NOT edit `todo-api.design.json` or `reference-slice.design.json` (goldens with hand-maintained fixtures).

## Non-goals (documented / tracked)
- `float` min/max (i64-only in v1; cheap follow-up). Regex/pattern + email/url formats (stay the `probe:"skip"` case). Per-item/array + cross-field constraints. Custom error messages + structured multi-violation `details` (the `Valid<T>` activation — tracked). Exclusive bounds. `values`→`enum` OpenAPI backfill. Eval-spec retrofits (inventory `quantity`, shortener url — follow-up).

## Success criteria
- A field with `min`/`max` (int) or `min_len`/`max_len` (string) generates a `de_*` validator that 422s an out-of-range body; the generated acceptance suite has an in-range happy path (green on correct handlers) AND an out-of-range `_rejects_out_of_range_{field}` 422 probe (reject-counted). OpenAPI carries `minimum`/`maximum`/`minLength`/`maxLength`; the migration has the CHECK.
- Bad combos (min>max, wrong type, values+len, constraint on id, out-of-range default, etc.) → `JC0552`.
- Every existing design byte-identical; `probe:"skip"` no longer needed for a length/range constraint; heavy gate green; `cargo semver-checks` clean; published 0.6.5.
