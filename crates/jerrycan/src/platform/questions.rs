//! Deterministic design validation → pointed questions (jerrycan_design's engine).

use super::design::*;
use serde::Serialize;

/// One pointed question. `id` is a JSON-pointer into the draft.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Question {
    pub id: String,
    pub question: String,
}

fn q(id: impl Into<String>, question: impl Into<String>) -> Question {
    Question {
        id: id.into(),
        question: question.into(),
    }
}

fn is_kebab(s: &str) -> bool {
    !s.is_empty()
        && s.starts_with(|c: char| c.is_ascii_lowercase())
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn is_snake(s: &str) -> bool {
    !s.is_empty()
        && s.starts_with(|c: char| c.is_ascii_lowercase())
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn is_pascal(s: &str) -> bool {
    !s.is_empty()
        && s.starts_with(|c: char| c.is_ascii_uppercase())
        && s.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Identifiers every generated route crate pulls into scope via
/// `use jerrycan::prelude::*;` (issue #114). This MIRRORS `jerrycan_core::prelude`
/// (crates/jerrycan-core/src/lib.rs) plus the re-exported `main` macro — the exact
/// glob the tool-owned `mod.rs`/`repo.rs`/`handlers.rs` write. An entity whose
/// PascalCase name equals one of these emits `pub struct {Name}` in `model.rs`,
/// which the sibling files glob-import via `use super::model::*;` ALONGSIDE the
/// prelude glob: two glob imports bring the same `{Name}` into scope, so every
/// reference is `E0659 ... is ambiguous` and the scaffolded crate does not compile
/// (the round-5 eval hit an entity named `Module`). Kept sorted for auditing; the
/// lowercase method fns / `main` can never collide with a PascalCase entity name
/// but are listed so the reserved set matches the glob exactly.
const RESERVED_PRELUDE_IDENTS: &[&str] = &[
    "App",
    "Clock",
    "CorsConfig",
    "CorsOrigins",
    "Created",
    "Dep",
    "Error",
    "Extension",
    "Headers",
    "IntoResponse",
    "Json",
    "Middleware",
    "MiddlewareFuture",
    "Module",
    "Multipart",
    "Next",
    "NoContent",
    "Path",
    "Query",
    "RawBody",
    "Redirect",
    "RequestCtx",
    "Result",
    "StreamBody",
    "TestApp",
    "TestPart",
    "delete",
    "get",
    "main",
    "now_rfc3339",
    "patch",
    "post",
    "put",
];

/// An enum `values` entry that is safe to interpolate unescaped into generated
/// Rust (issue #54): `^[A-Za-z0-9_-]+$`. Values reach a `"..."` string literal in
/// the generated deserialize allow-list, the 422 error text, and the testgen
/// fixture with no escaping, so anything with a quote/backslash/space would break
/// the generated crate at build time — validate the shape at design time instead.
fn is_enum_value(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

/// A validation message when a field's server-owned `default` (issue #53a) is
/// invalid, or `None` when the default is absent or valid. Three ways to be
/// wrong: (1) declared without a `db` dependency — the default is applied via the
/// db-mode request DTO, so it is silently inert in memory mode; (2) the value
/// does not type-check against the field's `type`; (3) the value is outside the
/// field's enum `values`. The default is written into a NOT-NULL column verbatim,
/// so a mistyped or out-of-enum literal is a design-time error, not a run-time
/// surprise. A `json` field accepts any JSON value.
fn default_type_error(f: &Field, wants_db: bool) -> Option<String> {
    let value = f.default.as_ref()?;
    if !wants_db {
        return Some(format!(
            "Field `{}` declares a `default` but the design has no `db` dependency — server-owned defaults are applied through the db-mode request DTO (add `db` to `dependencies`, or drop the default).",
            f.name
        ));
    }
    // JC0557 (#110): the `now` sentinel is a DYNAMIC server-set timestamp, valid
    // ONLY on a `datetime` field. Intercept it before the generic type/enum
    // acceptance below so a `"now"` elsewhere is refused (on a `string` it is
    // otherwise a legitimate literal — the refusal is what disambiguates the
    // timestamp intent) and a near-miss casing on a datetime field is never
    // silently mis-read as a static literal. The db requirement above already
    // covers `now`. The EXACT `"now"` on a `datetime` field is left to fall through
    // — it type-checks clean (datetime rides as a String) and carries no bounds.
    if let Some(s) = value.as_str() {
        if s == "now" && f.field_type != FieldType::Datetime {
            return Some(format!(
                "Field `{}` default \"now\" is a server-set-timestamp sentinel — valid only on a `datetime` field (issue #110). Use a static default here, or change the field `type` to `datetime`. See `jerrycan explain JC0557`.",
                f.name
            ));
        }
        if s != "now" && s.eq_ignore_ascii_case("now") && f.field_type == FieldType::Datetime {
            return Some(format!(
                "Field `{}` default {value} looks like the server-set-timestamp sentinel but is mis-cased — set `{}` to the current time on create with exactly `\"now\"` (lowercase), or give a full RFC3339 literal for a static default. See `jerrycan explain JC0557`.",
                f.name, f.name
            ));
        }
    }
    // Enum membership: a default on a string field with `values` must be listed.
    if let Some(values) = &f.values {
        return match value.as_str() {
            Some(s) if values.contains(&s.to_string()) => None,
            _ => Some(format!(
                "Field `{}` default {value} is not one of its enum values [{}] — the default must be a declared value.",
                f.name,
                values.join(", ")
            )),
        };
    }
    let ok = match f.field_type {
        FieldType::String | FieldType::Datetime | FieldType::Uuid => value.is_string(),
        FieldType::Integer => value.is_i64() || value.is_u64(),
        FieldType::Float => value.is_number(),
        FieldType::Boolean => value.is_boolean(),
        FieldType::Json => true,
    };
    if !ok {
        return Some(format!(
            "Field `{}` default {value} does not match its type `{:?}` — the server writes it verbatim, so it must be a valid {:?} literal.",
            f.name, f.field_type, f.field_type
        ));
    }
    // #80 (JC0552): the default must also satisfy the field's OWN range/length
    // constraints — an out-of-bounds default would plant a value the declared
    // bound forbids on every defaulted row.
    let bound = |b: Option<i64>| b.map_or_else(|| "unbounded".into(), |v: i64| v.to_string());
    if matches!(f.field_type, FieldType::Integer) && (f.min.is_some() || f.max.is_some()) {
        // A u64 beyond i64::MAX has no as_i64(): it exceeds any i64 `max` and
        // satisfies any i64 `min`.
        let out = match value.as_i64() {
            Some(v) => f.min.is_some_and(|mn| v < mn) || f.max.is_some_and(|mx| v > mx),
            None => f.max.is_some(),
        };
        if out {
            return Some(format!(
                "Field `{}` default {value} is outside its own declared range [min {}, max {}] — pick an in-range default, or relax the bound. See `jerrycan explain JC0552`.",
                f.name,
                bound(f.min),
                bound(f.max)
            ));
        }
    }
    if matches!(f.field_type, FieldType::String)
        && (f.min_len.is_some() || f.max_len.is_some())
        && let Some(s) = value.as_str()
    {
        let n = s.chars().count() as u64;
        if f.min_len.is_some_and(|mn| n < mn) || f.max_len.is_some_and(|mx| n > mx) {
            return Some(format!(
                "Field `{}` default {value} is {n} code points long, outside its own declared length bounds [min_len {}, max_len {}] — pick a default of a valid length, or relax the bound. See `jerrycan explain JC0552`.",
                f.name,
                f.min_len
                    .map_or_else(|| "unbounded".into(), |v: u64| v.to_string()),
                f.max_len
                    .map_or_else(|| "unbounded".into(), |v: u64| v.to_string())
            ));
        }
    }
    None
}

/// The entity an endpoint's repo operates on (mirrors genroute's resolution):
/// the request_body entity, else the success entity, else the module's first
/// entity. `None` when the module declares no entities. Kept in lockstep with
/// `genroute::endpoint_repo_entity` so design-time checks reason about the same
/// entity the generator wires.
fn endpoint_repo_entity<'a>(m: &'a ModuleDesign, ep: &'a Endpoint) -> Option<&'a str> {
    if m.entities.is_empty() {
        return None;
    }
    ep.request_body
        .as_ref()
        .and_then(|rb| rb.entity.as_deref())
        .or(ep.success.entity.as_deref())
        .or_else(|| m.entities.first().map(|e| e.name.as_str()))
}

/// The per-FIELD shape checks (#47 enum values + #80 range/length JC0552 + the pk
/// `write_only` JC0554 + `default` type-check) for one field, pointed at `fptr`
/// (`.../fields/{j}`). Factored out of `check_relations_and_enums` so an inline-DTO
/// body's fields (issue #122) get the SAME JC0552/JC0543 checks as entity fields —
/// they must not be silently skipped. Byte-identical questions for entity fields
/// (same pointers, same messages) since `fptr` reconstructs the old pointer.
fn check_field_shape(f: &Field, fptr: &str, wants_db: bool, qs: &mut Vec<Question>) {
    if let Some(ref values) = f.values {
        if !matches!(f.field_type, FieldType::String) {
            qs.push(q(
                format!("{fptr}/values"),
                format!(
                    "Field `{}` declares enum `values` but its type is not string — enum values are only allowed on string fields.",
                    f.name
                ),
            ));
        } else if values.is_empty() {
            qs.push(q(
                format!("{fptr}/values"),
                format!(
                    "Field `{}` declares an empty `values` list — list at least one allowed value or drop the field.",
                    f.name
                ),
            ));
        } else if let Some(bad) = values.iter().find(|v| !is_enum_value(v)) {
            // JC0543 (#54): enum values are interpolated UNESCAPED into
            // generated Rust (the deserialize allow-list + 422 text in
            // genroute, the testgen fixture), so a quote or backslash
            // emits a crate that won't compile far from the design.
            // Constrain to an identifier-ish shape (which also excludes
            // spaces etc. under the same interpolation-safety rule).
            qs.push(q(
                format!("{fptr}/values"),
                format!(
                    "Field `{}` enum value `{bad}` is not an identifier (^[A-Za-z0-9_-]+$) — enum values are interpolated unescaped into generated Rust (the deserialize allow-list, the 422 error text, and the test fixtures), so a quote or backslash emits a crate that fails to compile; other non-identifier characters are rejected under the same rule. Use identifier-shaped values (letters, digits, `_`, `-`). See `jerrycan explain JC0543`.",
                    f.name
                ),
            ));
        }
    }
    // #80 (JC0552): field range/length constraints. Refuse
    // misplacement, empty ranges, contradictions with `values`, an
    // unfillable required field, an over-ceiling `min_len`, and ANY
    // constraint on the pk `id` — each pointed at the offending key.
    // The pk check runs first and swallows the rest: ids are
    // server-assigned, and the generated id probes and seeds assume
    // them free.
    if f.name == "id" {
        // JC0554 (#112): the pk id must be returned in every response
        // (the id-echo probe + every cross-scope test key on
        // `body["id"]`), so an EXPLICIT `write_only` on it would
        // response-hide the id and break the generated suite by
        // construction. The `password_hash` auto-classification never
        // applies to `id`, so only the explicit flag is refused here.
        if f.write_only {
            qs.push(q(
                format!("{fptr}/write_only"),
                "Field `id` is the primary key — `write_only` is not allowed on it: the id must be returned in every response (the generated id-echo probe and every cross-scope test key on `body[\"id\"]`), so hiding it breaks the generated suite by construction. Remove `write_only` from `id`. See `jerrycan explain JC0554`.".to_string(),
            ));
        }
        for (key, present) in [
            ("min", f.min.is_some()),
            ("max", f.max.is_some()),
            ("min_len", f.min_len.is_some()),
            ("max_len", f.max_len.is_some()),
        ] {
            if present {
                qs.push(q(
                    format!("{fptr}/{key}"),
                    format!(
                        "Field `id` is the primary key — `{key}` is not allowed on it: ids are server-assigned, and the generated id probes and seeds assume unconstrained ids. Drop `{key}`. See `jerrycan explain JC0552`."
                    ),
                ));
            }
        }
    } else {
        if f.min.is_some() || f.max.is_some() {
            if !matches!(f.field_type, FieldType::Integer) {
                for (key, present) in [("min", f.min.is_some()), ("max", f.max.is_some())] {
                    if present {
                        qs.push(q(
                            format!("{fptr}/{key}"),
                            format!(
                                "Field `{}` declares `{key}` but its type is not integer — `min`/`max` are an inclusive integer range, only allowed on integer fields. Use `min_len`/`max_len` to bound a string's length, or drop `{key}`. See `jerrycan explain JC0552`.",
                                f.name
                            ),
                        ));
                    }
                }
            } else if let (Some(mn), Some(mx)) = (f.min, f.max)
                && mn > mx
            {
                qs.push(q(
                    format!("{fptr}/min"),
                    format!(
                        "Field `{}` declares an empty range: min {mn} > max {mx} — no value can satisfy it, so no in-range fixture is derivable. Lower `min` or raise `max`. See `jerrycan explain JC0552`.",
                        f.name
                    ),
                ));
            } else if f.unique {
                // #80 (T3): the generated suite materializes up to
                // THREE distinct values per field — the probe
                // fixture plus the tenant-1 and tenant-2 seeds —
                // so a `unique` range below that collides on the
                // UNIQUE index and is un-greenable by construction.
                // An ABSENT bound substitutes its i64 extreme (T4):
                // a single `min` near i64::MAX (or `max` near
                // i64::MIN) leaves the same too-narrow range even
                // though only one bound was written.
                let mn = f.min.unwrap_or(i64::MIN);
                let mx = f.max.unwrap_or(i64::MAX);
                if (mx as i128) - (mn as i128) + 1 < 3 {
                    let key = if f.min.is_some() { "min" } else { "max" };
                    qs.push(q(
                        format!("{fptr}/{key}"),
                        format!(
                            "Field `{}` is `unique` but its range [min {mn}, max {mx}] admits only {} distinct value(s) — the generated seeds and probe fixture need up to 3 distinct in-range values (the request fixture and the two tenant seeds), so uniqueness cannot hold. Widen the range to at least 3 values, or drop `unique`. See `jerrycan explain JC0552`.",
                            f.name,
                            (mx as i128) - (mn as i128) + 1
                        ),
                    ));
                }
            }
        }
        if f.min_len.is_some() || f.max_len.is_some() {
            if !matches!(f.field_type, FieldType::String) {
                for (key, present) in [
                    ("min_len", f.min_len.is_some()),
                    ("max_len", f.max_len.is_some()),
                ] {
                    if present {
                        qs.push(q(
                            format!("{fptr}/{key}"),
                            format!(
                                "Field `{}` declares `{key}` but its type is not string — `min_len`/`max_len` bound a string's length in Unicode code points. Use `min`/`max` for an integer range, or drop `{key}`. See `jerrycan explain JC0552`.",
                                f.name
                            ),
                        ));
                    }
                }
            } else if f.values.is_some() {
                let key = if f.min_len.is_some() {
                    "min_len"
                } else {
                    "max_len"
                };
                qs.push(q(
                    format!("{fptr}/{key}"),
                    format!(
                        "Field `{}` combines enum `values` with `{key}` — the enum already fixes the exact allowed strings, so a length bound is contradictory. Drop `{key}` (or drop `values`). See `jerrycan explain JC0552`.",
                        f.name
                    ),
                ));
            } else if let (Some(mn), Some(mx)) = (f.min_len, f.max_len)
                && mn > mx
            {
                qs.push(q(
                    format!("{fptr}/min_len"),
                    format!(
                        "Field `{}` declares an empty range: min_len {mn} > max_len {mx} — no value can satisfy it, so no in-range fixture is derivable. Lower `min_len` or raise `max_len`. See `jerrycan explain JC0552`.",
                        f.name
                    ),
                ));
            } else if f.max_len == Some(0) && f.required {
                qs.push(q(
                    format!("{fptr}/max_len"),
                    format!(
                        "Field `{}` is required but declares `max_len: 0` — an unfillable field: no value satisfies a zero-length required string. Raise `max_len`, or make the field optional (`required: false`). See `jerrycan explain JC0552`.",
                        f.name
                    ),
                ));
            } else if f.max_len == Some(0) && f.unique {
                // #80 (T3): the string twin of the unique-range
                // rule — `max_len: 0` admits ONLY the empty
                // string, so the seeds cannot be distinct. Any
                // max_len >= 1 is fine: the seed derivations lead
                // with distinct characters ('t'/'s'/a digit).
                qs.push(q(
                    format!("{fptr}/max_len"),
                    format!(
                        "Field `{}` is `unique` but declares `max_len: 0` — the empty string is the only possible value, so the generated seeds cannot derive distinct values. Raise `max_len`, or drop `unique`. See `jerrycan explain JC0552`.",
                        f.name
                    ),
                ));
            } else if f.min_len.is_some_and(|n| n > 4096) {
                qs.push(q(
                    format!("{fptr}/min_len"),
                    format!(
                        "Field `{}` declares min_len {} above the 4096 ceiling — generated test fixtures materialize a minimum-length value, so a larger bound is refused. Lower `min_len` to at most 4096. See `jerrycan explain JC0552`.",
                        f.name,
                        f.min_len.unwrap()
                    ),
                ));
            }
        }
    }
    // A server-owned `default` (issue #53a) must type-check against the
    // field type (and enum membership) — the server writes it verbatim
    // into a NOT-NULL column, so a mistyped literal would fail at run
    // time, not design time.
    if let Some(msg) = default_type_error(f, wants_db) {
        qs.push(q(format!("{fptr}/default"), msg));
    }
}

// The fixed `user_id` identity linkage (AUTH_IDENTITY_FK_COLUMN) lives in
// `design.rs` — shared with the server-owned-FK emission rule (issue #34).
// It reaches this module through the `use super::design::*` glob above.

/// A fatal design-shape conflict caught before any scaffolding — distinct from
/// the completeness questions `validate` returns (which a field edit can
/// answer). This one needs a structural redesign, so it carries a stable JC
/// code the CLI (`{ok:false, code, ...}`) and the MCP twin render.
#[derive(Debug)]
pub struct DesignConflict {
    pub code: &'static str,
    pub message: String,
    pub hint: String,
}

/// Reject a design that cannot be generated regardless of completeness. One rule
/// today (#27): `tenancy.entity` must not BE the auth identity entity. When it
/// is, the tenant's derived fk column equals the membership table's fixed
/// `user_id` column, so the auth_0001 migration declares `user_id` twice and
/// dies with `duplicate column name: user_id` — mid-scaffold, on a half-written
/// tree. Catch it up front instead. Shared by the CLI and MCP so they can't drift.
pub fn design_conflict(d: &Design) -> Option<DesignConflict> {
    if let Some(tenancy) = &d.tenancy
        && Design::fk_column(&tenancy.entity) == AUTH_IDENTITY_FK_COLUMN
    {
        let entity = &tenancy.entity;
        return Some(DesignConflict {
            code: "JC0540",
            message: format!(
                "tenancy.entity `{entity}` is the auth identity entity — its derived foreign key column `{AUTH_IDENTITY_FK_COLUMN}` collides with the membership table's authenticated-user column, so scaffolding would die with `duplicate column name: {AUTH_IDENTITY_FK_COLUMN}`. A user cannot be their own tenant org. For per-user data, drop the `tenancy` block and give each owned entity a `belongs_to` `{entity}` plus tenant-scoped guard methods (all_for/get_for); for orgs/teams, point tenancy.entity at a separate tenant entity (e.g. Org or Workspace). See `jerrycan docs tenancy` / `jerrycan explain JC0540`."
            ),
            hint: format!(
                "per-user data → `belongs_to` `{entity}` + scoped guard methods; orgs/teams → a separate tenant entity (Org/Workspace)"
            ),
        });
    }
    // JC0548 (#107): with tenancy, `member_roles` backs the generated member-
    // management surface — `member_roles[0]` is the admin role, and every role
    // is interpolated UNESCAPED into generated Rust string literals — so the
    // list must be non-empty, duplicate-free, and identifier-shaped.
    if let Some(conflict) = member_roles_conflict(d) {
        return Some(conflict);
    }
    // JC0541 (#44): an entity literally named `{X}Request` collides with the
    // `{X}Request` DTO/OpenAPI component generated for an entity `X` that omits a
    // server-owned field. Two `struct XRequest` would fail to compile in genroute and
    // silently overwrite each other in the OpenAPI schema map. Only a REAL collision
    // fires — `X` must actually mint the DTO (db mode + a server-owned omission) — so
    // an ordinary `*Request` name that shadows nothing is never rejected.
    if let Some(conflict) = request_dto_name_collision(d) {
        return Some(conflict);
    }
    // JC0542 (#65): sibling routes that name a shared path position's `{param}`
    // differently panic at `App::build` (JC0500), a clean-scaffold-then-mid-test
    // failure. Caught here as a fatal conflict — it needs a rename or a restructure.
    if let Some(conflict) = router_param_conflict(d) {
        return Some(conflict);
    }
    None
}

/// The JC0548 check (issue #107): a tenancy design's `member_roles` must be
/// non-empty (`member_roles[0]` is the admin role — it gates add/re-role/remove
/// and the last-admin rule, and seeds the creator's membership), duplicate-free
/// (the list becomes the generated `MEMBER_ROLES` allow-list and the OpenAPI
/// `role` enum), and identifier-shaped — the same `^[A-Za-z0-9_-]+$` charset
/// JC0543 enforces for enum values, because role names are interpolated
/// UNESCAPED into generated Rust string literals (the `MEMBER_ROLES` const, the
/// membership seed SQL, `require_role("…")` gates), so a quote or backslash
/// emits a crate that fails to compile far from the design. One message per
/// failure mode, naming the offending role.
fn member_roles_conflict(d: &Design) -> Option<DesignConflict> {
    let tenancy = d.tenancy.as_ref()?;
    let roles = &tenancy.member_roles;
    if roles.is_empty() {
        return Some(DesignConflict {
            code: "JC0548",
            message: "tenancy.member_roles is empty — the member-management surface needs at least one declared role: `member_roles[0]` is the admin role (it gates member add/re-role/remove and the last-admin rule), and the tenant creator's membership is seeded with it. Declare the roles a member may hold, admin first (e.g. [\"owner\", \"member\"]). See `jerrycan explain JC0548`.".to_string(),
            hint: "declare tenancy.member_roles, admin role first (e.g. [\"owner\", \"member\"])"
                .to_string(),
        });
    }
    let mut seen = std::collections::HashSet::new();
    for role in roles {
        if !is_enum_value(role) {
            return Some(DesignConflict {
                code: "JC0548",
                message: format!(
                    "tenancy.member_roles entry `{role}` is not an identifier (^[A-Za-z0-9_-]+$) — role names are interpolated unescaped into generated Rust string literals (the MEMBER_ROLES const, the membership seed, `require_role` gates), so a quote or backslash emits a crate that fails to compile; other non-identifier characters are rejected under the same rule. Use identifier-shaped roles (letters, digits, `_`, `-`). See `jerrycan explain JC0548`."
                ),
                hint: format!(
                    "rename role `{role}` to an identifier-shaped name (letters, digits, `_`, `-`)"
                ),
            });
        }
        if !seen.insert(role.as_str()) {
            return Some(DesignConflict {
                code: "JC0548",
                message: format!(
                    "tenancy.member_roles lists `{role}` more than once — the list becomes the generated MEMBER_ROLES allow-list and the OpenAPI `role` enum, and `member_roles[0]` is the admin role, so a duplicate makes the declared role set ambiguous. List each role exactly once. See `jerrycan explain JC0548`."
                ),
                hint: format!("remove the duplicate `{role}` from tenancy.member_roles"),
            });
        }
    }
    None
}

/// The JC0542 check (issue #65): the runtime router keys each path segment
/// position by a SINGLE `{param}` name (see `jerrycan-core` `router::Trie::insert`
/// — one global trie backs the whole app, so this spans every module + subroute).
/// Two routes that reach the same position through an identical static/param
/// prefix but name that position's parameter differently (`/tickets/{id}` vs
/// `/tickets/{ticket_id}/comments`) make `App::build` abort with JC0500
/// `conflicting path parameters` — after a clean scaffold, mid-test.
///
/// This is a structure-only twin of `router::Trie`, mirroring its insert EXACTLY
/// so the validator neither rejects a design the router accepts nor accepts one it
/// panics on: a static segment and a param segment DIVERGE into different children
/// (`/users/me` + `/users/{id}` is fine), two DIFFERENT literals diverge, the SAME
/// param name at a position agrees (`/{id}` + `/{id}/comments` is fine), two
/// DIFFERENT param names at the same node conflict, and a SECOND raw spelling
/// terminating at an occupied node (`/x` + `/x/` — the router drops empty
/// segments, #140) is the trie's `duplicate route registration`. Analyzed over the
/// mount-resolved route table (`genroute::route_map`), so subroute-mount params
/// (which occupy real positions) are included — PLUS the implicit member routes
/// (`genroute::implicit_member_routes`, issue #107), which `App::build` registers
/// for the tenant module but which never enter the design-endpoint table: a
/// tenant-module endpoint with a custom param name (`GET /{slug}`), or one
/// occupying a reserved `/{fk}/members` path, would otherwise pass validation and
/// abort at startup.
fn router_param_conflict(d: &Design) -> Option<DesignConflict> {
    /// A structure-only twin of `router::Node`: static children keyed by literal,
    /// at most ONE param slot carrying its name and the first route to set it,
    /// plus the trie's endpoint-occupancy marker (#140) — the RAW path of the
    /// first route to TERMINATE here. The runtime groups methods by raw path
    /// string (one `.route()` line per spelling), so a second spelling that
    /// collapses to the same node (`/x` vs `/x/` — empty segments are dropped)
    /// is a second `Trie::insert` and aborts with `duplicate route registration`;
    /// the same raw path re-terminating is the SAME line (methods merged), fine.
    #[derive(Default)]
    struct Node {
        statics: std::collections::HashMap<String, Node>,
        param: Option<(String, String, Box<Node>)>,
        route: Option<String>,
    }
    // The router registers routes from the LOAD-normalized design
    // (`normalize_tenant_detail_routes` rewrites the tenant's own `{id}` detail
    // params to `{tenant_fk}` in `Design::from_path` and the MCP merge paths).
    // Every production caller hands this twin a normalized design already;
    // re-normalizing a clone keeps it faithful for direct (test/programmatic)
    // callers too — idempotent, so never wrong — which matters now that the
    // member routes (named `{tenant_fk}` by construction) join the walk.
    let normalized;
    let d = if d.tenancy.is_some() {
        normalized = {
            let mut n = d.clone();
            n.normalize_tenant_detail_routes();
            n
        };
        &normalized
    } else {
        d
    };
    let design_routes = super::genroute::route_map(d);
    let member_routes = super::genroute::implicit_member_routes(d);
    // A design endpoint that OCCUPIES a reserved member path is a second
    // `.route()` registration of the same path — `App::build` aborts with
    // JC0500 `duplicate route registration` (methods don't disambiguate: the
    // member surface registers its own route lines). Compared as SEGMENT
    // vectors, not raw strings (#140): the router drops empty path segments
    // (`router::segments` filters them), so `/{fk}/members/` — the natural
    // hand-rolled spelling, collection routes are trailing-slash by convention
    // — and `//` variants land on the SAME trie node as the reserved pair. A
    // raw-string compare would pass them clean and abort at startup instead.
    // (Param names already agree here — a mismatch is the trie conflict below.)
    fn segs(p: &str) -> Vec<&str> {
        p.split('/').filter(|s| !s.is_empty()).collect()
    }
    if let Some(taken) = member_routes.iter().find(|mr| {
        let reserved = segs(&mr.path);
        design_routes.iter().any(|dr| segs(&dr.path) == reserved)
    }) {
        let tenant = &d
            .tenancy
            .as_ref()
            .expect("member routes imply tenancy")
            .entity;
        let fk = Design::fk_column(tenant);
        let path = &taken.path;
        return Some(DesignConflict {
            code: "JC0542",
            message: format!(
                "route `{path}` collides with the implicit member-management surface generated for the `{tenant}` tenancy (issue #107) — jerrycan reserves `/{{{fk}}}/members` and `/{{{fk}}}/members/{{user_id}}` under the tenant module, and a second registration of the same path aborts `App::build` at startup with JC0500 `duplicate route registration` (after a clean scaffold, mid-test). Move the design endpoint off the reserved path (any static segment other than `members`), or drop it and use the generated member surface. See `jerrycan explain JC0542`."
            ),
            hint: format!(
                "the member-management surface owns `{path}` — move or rename the design endpoint (a static segment other than `members` avoids the reserved pair), or rely on the generated member routes"
            ),
        });
    }
    let mut root = Node::default();
    for entry in design_routes.into_iter().chain(member_routes) {
        let path = entry.path;
        let mut node = &mut root;
        for seg in path.split('/').filter(|s| !s.is_empty()) {
            if let Some(name) = seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                // Mirror router.rs: ensure the param slot, then compare its name.
                if node.param.is_none() {
                    node.param = Some((name.to_string(), path.clone(), Box::default()));
                }
                let (existing, first_route, child) = node.param.as_mut().expect("just ensured");
                if existing != name {
                    return Some(DesignConflict {
                        code: "JC0542",
                        message: format!(
                            "routes `{first_route}` and `{path}` both take a path parameter at the same position but name it differently (`{{{existing}}}` vs `{{{name}}}`) — the router keys each path position by a single parameter name, so registering both aborts `App::build` at startup with JC0500 `conflicting path parameters` (after a clean scaffold, mid-test). Unify the name (use `{{{existing}}}` in BOTH routes, or `{{{name}}}` in both), or restructure so the position is not shared (mount the diverging routes under distinct static prefixes). See `jerrycan explain JC0542`."
                        ),
                        hint: format!(
                            "give the shared segment ONE parameter name across every sibling route (rename `{{{name}}}`→`{{{existing}}}` or vice versa), or restructure the nesting so the position is not shared"
                        ),
                    });
                }
                node = child;
            } else {
                node = node.statics.entry(seg.to_string()).or_default();
            }
        }
        // Mirror `Trie::insert`'s occupancy branch (#140): a DIFFERENT raw
        // spelling terminating at an occupied node is a second registration.
        if let Some(first_route) = &node.route {
            if first_route != &path {
                return Some(DesignConflict {
                    code: "JC0542",
                    message: format!(
                        "routes `{first_route}` and `{path}` are two spellings of the same route — the router drops empty path segments (a trailing or doubled `/`), so both occupy the same position and the second registration aborts `App::build` at startup with JC0500 `duplicate route registration` (after a clean scaffold, mid-test). Spell the path identically on both endpoints (identical spellings share one registration, each method intact), or move one endpoint to a distinct path. See `jerrycan explain JC0542`."
                    ),
                    hint: format!(
                        "`{first_route}` and `{path}` collapse to the same router position — use ONE spelling for both endpoints, or give one a distinct path"
                    ),
                });
            }
        } else {
            node.route = Some(path);
        }
    }
    None
}

/// The JC0541 check (issue #44): find an entity literally named `{base}Request`
/// whose `{base}` sibling generates a `{base}Request` DTO. Returns the collision, or
/// `None` when no `*Request` entity shadows a generated DTO name.
fn request_dto_name_collision(d: &Design) -> Option<DesignConflict> {
    fn collect<'a>(m: &'a ModuleDesign, out: &mut Vec<&'a str>) {
        out.extend(m.entities.iter().map(|e| e.name.as_str()));
        for sub in &m.subroutes {
            collect(sub, out);
        }
    }
    let mut names = Vec::new();
    for m in &d.modules {
        collect(m, &mut names);
    }
    for name in &names {
        let Some(base) = name.strip_suffix("Request") else {
            continue;
        };
        if base.is_empty() || !names.contains(&base) || !d.entity_generates_request_dto(base) {
            continue;
        }
        return Some(DesignConflict {
            code: "JC0541",
            message: format!(
                "entity `{name}` collides with the request DTO generated for entity `{base}`: a `{base}` request body that omits a server-owned field (an identity fk, a `default`, or a path-redundant parent fk) emits a `{base}Request` type — a Rust struct AND an OpenAPI `{base}Request` component. With an entity also literally named `{name}`, genroute would define `struct {name}` twice (a compile error) and the OpenAPI document would clobber one schema with the other. Rename the entity (e.g. `{base}Payload` or `{base}Submission`) so it no longer shadows the generated DTO. See `jerrycan explain JC0541`."
            ),
            hint: format!(
                "rename `{name}` (e.g. `{base}Payload`/`{base}Submission`) — the `{base}Request` name is reserved for the generated request DTO"
            ),
        });
    }
    None
}

/// Validate a parsed design. Empty result == complete (status: "complete").
pub fn validate(d: &Design) -> Vec<Question> {
    let mut qs = Vec::new();

    if !is_kebab(&d.name) {
        qs.push(q(
            "/name",
            format!(
                "`{}` is not kebab-case (^[a-z][a-z0-9-]*$) — what should the app be called?",
                d.name
            ),
        ));
    }
    if d.contract_version > 2 {
        qs.push(q(
            "/contract_version",
            "contract_version must be 0, 1, or 2 for this platform version.",
        ));
    }
    if d.modules.is_empty() {
        qs.push(q(
            "/modules",
            "No modules defined — what are the resource areas of this backend (each becomes a route crate)?",
        ));
    }
    // A top-level base_path is emitted verbatim into every mount, so it must be a
    // clean absolute path (like a module mount). Empty/`/` is a documented no-op.
    if let Some(base) = &d.base_path
        && !base.is_empty()
        && base != "/"
    {
        if !base.starts_with('/') {
            qs.push(q(
                "/base_path",
                format!("App base_path `{base}` must start with '/'."),
            ));
        }
        if base.contains("//") || base.ends_with('/') {
            qs.push(q(
                "/base_path",
                format!(
                    "App base_path `{base}` must not contain `//` or end with a trailing slash."
                ),
            ));
        }
    }

    // The `cors` block is emitted into `App::cors(CorsConfig::new(..))` (issue #21).
    // Validate the origins at design time so a misconfig is a pointed question, not
    // a runtime `App::build()` failure the deploy discovers on first boot.
    if let Some(cors) = &d.cors {
        if cors.origins.is_empty() {
            qs.push(q(
                "/cors/origins",
                "CORS is declared with no origins — list the allowed origins (exact scheme://host[:port]) or `*` for any origin.",
            ));
        }
        let is_wildcard = cors.origins.iter().any(|o| o == "*");
        if is_wildcard && cors.origins.len() > 1 {
            qs.push(q(
                "/cors/origins",
                "CORS origins mixes `*` with explicit origins — use either `*` (any origin) alone or an explicit allowlist.",
            ));
        }
        // Fetch spec: a credentialed cross-origin request cannot use a wildcard
        // origin. Core's `App::build` rejects the combination; catch it here so the
        // generated app never fails to boot on it.
        if is_wildcard && cors.allow_credentials {
            qs.push(q(
                "/cors/allow_credentials",
                "CORS allow_credentials cannot be combined with `*` origins (the Fetch spec forbids it) — list explicit origins instead.",
            ));
        }
        // Each explicit origin must be a bare origin (scheme://host[:port]) — no path
        // or trailing slash — since it is matched byte-for-byte against the request's
        // Origin header.
        for (i, o) in cors.origins.iter().enumerate() {
            if o == "*" {
                continue;
            }
            let well_formed = (o.starts_with("http://") || o.starts_with("https://"))
                && !o.ends_with('/')
                && o.matches('/').count() == 2;
            if !well_formed {
                qs.push(q(
                    format!("/cors/origins/{i}"),
                    format!("CORS origin `{o}` is not a bare origin — use scheme://host[:port] with no path or trailing slash (e.g. https://app.example)."),
                ));
            }
        }
    }

    let declared_roles: Vec<&str> = d
        .auth
        .as_ref()
        .map(|a| a.roles.iter().map(String::as_str).collect())
        .unwrap_or_default();
    let auth_declared = d.auth.is_some();

    let mut seen_module_names = std::collections::HashSet::new();
    for (i, m) in d.modules.iter().enumerate() {
        if !seen_module_names.insert(m.name.as_str()) {
            qs.push(q(
                format!("/modules/{i}/name"),
                format!(
                    "Module name `{}` is already used — module names must be unique.",
                    m.name
                ),
            ));
        }
        validate_module(
            m,
            &format!("/modules/{i}"),
            &declared_roles,
            auth_declared,
            d.wants_db(),
            &mut qs,
        );
    }

    // Role coherence: a guarded endpoint (auth_required or required_roles) needs
    // an active auth model — `auth.model: none`/absent can't resolve a session.
    if !d.wants_auth() {
        fn check_guards(m: &ModuleDesign, ptr: &str, qs: &mut Vec<Question>) {
            for (i, ep) in m.endpoints.iter().enumerate() {
                if ep.is_guarded() {
                    qs.push(q(
                        format!("{ptr}/endpoints/{i}"),
                        format!(
                            "Endpoint `{}` is guarded (auth_required/required_roles) but the design has no active auth — set auth.model to `session` or `jwt` first.",
                            ep.operation_id
                        ),
                    ));
                }
            }
            for (i, sub) in m.subroutes.iter().enumerate() {
                check_guards(sub, &format!("{ptr}/subroutes/{i}"), qs);
            }
        }
        for (i, m) in d.modules.iter().enumerate() {
            check_guards(m, &format!("/modules/{i}"), &mut qs);
        }
    }

    if d.wants_db() {
        // contract v1 stores json as a real column (Json); v0 has no json columns,
        // so a json field there is still an unsupported request.
        let json_ok = d.contract_version >= 1;
        fn check_db_fields(m: &ModuleDesign, ptr: &str, json_ok: bool, qs: &mut Vec<Question>) {
            for (i, e) in m.entities.iter().enumerate() {
                for (j, f) in e.fields.iter().enumerate() {
                    if !json_ok && matches!(f.field_type, FieldType::Json) {
                        qs.push(q(
                            format!("{ptr}/entities/{i}/fields/{j}/type"),
                            format!("Field `{}` has type json — json fields are not yet supported in db mode (store as string, or drop the db dependency; structured json columns are a contract-v1 candidate).", f.name),
                        ));
                    } else if f.name == "id"
                        && !matches!(
                            f.field_type,
                            FieldType::Integer | FieldType::String | FieldType::Uuid
                        )
                    {
                        // A declared `id` becomes the table's primary key.
                        qs.push(q(
                            format!("{ptr}/entities/{i}/fields/{j}/type"),
                            format!("Field `id` of entity `{}` becomes the table's primary key in db mode — it must be integer, string, or uuid.", e.name),
                        ));
                    }
                }
                // The fk column a belongs_to derives is generated; an explicit field
                // of the same name would collide with the derived column.
                for b in &e.belongs_to {
                    let derived = b.fk_column();
                    if let Some(j) = e.fields.iter().position(|f| f.name == derived) {
                        qs.push(q(
                            format!("{ptr}/entities/{i}/fields/{j}"),
                            format!(
                                "Field `{derived}` collides with the fk column derived from belongs_to `{}` — the fk column is derived from belongs_to; remove the explicit field or the belongs_to.",
                                b.entity
                            ),
                        ));
                    }
                }
            }
            for (i, sub) in m.subroutes.iter().enumerate() {
                check_db_fields(sub, &format!("{ptr}/subroutes/{i}"), json_ok, qs);
            }
        }
        for (i, m) in d.modules.iter().enumerate() {
            check_db_fields(m, &format!("/modules/{i}"), json_ok, &mut qs);
        }
    }

    // Contract v1 constructs: belongs_to targets and enum-value placement.
    // Collect every declared entity name (modules + subroutes) so a belongs_to
    // may target any entity anywhere in the design.
    let mut entity_names = std::collections::HashSet::new();
    fn collect_entity_names<'a>(m: &'a ModuleDesign, out: &mut std::collections::HashSet<&'a str>) {
        for e in &m.entities {
            out.insert(e.name.as_str());
        }
        for sub in &m.subroutes {
            collect_entity_names(sub, out);
        }
    }
    for m in &d.modules {
        collect_entity_names(m, &mut entity_names);
    }

    fn check_relations_and_enums(
        m: &ModuleDesign,
        ptr: &str,
        entity_names: &std::collections::HashSet<&str>,
        wants_db: bool,
        qs: &mut Vec<Question>,
    ) {
        for (i, e) in m.entities.iter().enumerate() {
            for (k, b) in e.belongs_to.iter().enumerate() {
                if !entity_names.contains(b.entity.as_str()) {
                    qs.push(q(
                        format!("{ptr}/entities/{i}/belongs_to/{k}"),
                        format!(
                            "belongs_to target `{}` is not a declared entity anywhere in the design — define it or fix the reference.",
                            b.entity
                        ),
                    ));
                }
            }
            for (j, f) in e.fields.iter().enumerate() {
                check_field_shape(f, &format!("{ptr}/entities/{i}/fields/{j}"), wants_db, qs);
            }
        }
        for (i, sub) in m.subroutes.iter().enumerate() {
            check_relations_and_enums(
                sub,
                &format!("{ptr}/subroutes/{i}"),
                entity_names,
                wants_db,
                qs,
            );
        }
    }
    let wants_db = d.wants_db();
    for (i, m) in d.modules.iter().enumerate() {
        check_relations_and_enums(
            m,
            &format!("/modules/{i}"),
            &entity_names,
            wants_db,
            &mut qs,
        );
    }

    // JC0544 (#60): a body-carrying create/update endpoint whose entity has a
    // path-redundant parent fk (R5's `entity_path_fk_columns`) but whose OWN path
    // lacks the matching `{param}`. The request DTO is per-entity, so the fk is
    // dropped for EVERY create of the entity; on a route that doesn't carry it in
    // the path the NOT-NULL column can be set from neither the body nor the path —
    // the route is un-implementable (the stub even references a `_{col}` binding
    // that doesn't exist). Reuses the R5 resolution — no duplicated fk logic.
    // `prefix` is the accumulated MOUNT of this module's ancestors; combined with the
    // module's own `effective_mount()` (trailing `/` trimmed) it resolves each
    // endpoint's full path — the SAME "resolved path" notion `entity_path_fk_columns`
    // and `endpoint_tenant_shape` use. A fk carried by the MOUNT (`/clubs/{club_id}`,
    // create at `POST /`) is therefore injectable and must NOT be flagged (issue #82),
    // even though `ep.path` alone lacks the param.
    fn check_dual_create_path_fk(
        d: &Design,
        m: &ModuleDesign,
        ptr: &str,
        prefix: &str,
        qs: &mut Vec<Question>,
    ) {
        let mount = m.effective_mount();
        let mount = mount.strip_suffix('/').unwrap_or(&mount);
        let base = format!("{prefix}{mount}");
        for (i, ep) in m.endpoints.iter().enumerate() {
            let Some(rb) = ep.request_body.as_ref() else {
                continue;
            };
            // An inline-DTO body (issue #122) has no entity and thus no parent fk to
            // relocate — the path-fk create check does not apply.
            let Some(entity) = rb.entity.as_deref() else {
                continue;
            };
            if !matches!(
                ep.method,
                HttpMethod::POST | HttpMethod::PUT | HttpMethod::PATCH
            ) {
                continue;
            }
            let resolved = format!("{base}{}", ep.path);
            let token = |col: &str| resolved.contains(&format!("{{{col}}}"));
            if let Some(col) = d
                .entity_path_fk_columns(entity)
                .into_iter()
                .find(|col| !token(col))
            {
                qs.push(q(
                    format!("{ptr}/endpoints/{i}"),
                    format!(
                        "Endpoint `{}` ({:?} {}) creates `{}`, whose parent foreign key `{col}` is supplied by a path parameter on a sibling nested route — so the generated `{}Request` body drops `{col}`, but this route's own path has no `{{{col}}}` to inject it from. The NOT-NULL `{col}` can be set from neither the body nor the path, so the route is un-implementable. Add `{{{col}}}` to this endpoint's path (mount it under the parent), or split `{}` into a separate entity for the standalone create so its request body keeps `{col}`. See `jerrycan explain JC0544`.",
                        ep.operation_id, ep.method, ep.path, entity, entity, entity
                    ),
                ));
            }
        }
        for (i, sub) in m.subroutes.iter().enumerate() {
            check_dual_create_path_fk(d, sub, &format!("{ptr}/subroutes/{i}"), &base, qs);
        }
    }
    for (i, m) in d.modules.iter().enumerate() {
        check_dual_create_path_fk(d, m, &format!("/modules/{i}"), "", &mut qs);
    }

    // Tenancy: the named entity must resolve, and the Tenant guard needs an
    // authenticated user to scope by.
    if let Some(ref tenancy) = d.tenancy {
        if !entity_names.contains(tenancy.entity.as_str()) {
            qs.push(q(
                "/tenancy/entity",
                format!(
                    "Tenancy entity `{}` is not a declared entity — define it or fix the reference.",
                    tenancy.entity
                ),
            ));
        }
        // The Tenant guard scopes by the authenticated principal, which only an
        // active auth *model* (session/jwt) produces — the bare `auth` dependency
        // stub does not.
        let active_auth_model = d
            .auth
            .as_ref()
            .map(|a| a.model != AuthModel::None)
            .unwrap_or(false);
        if !active_auth_model {
            qs.push(q(
                "/tenancy",
                "Tenancy is declared but the design has no active auth model — the Tenant guard needs an authenticated user; set auth.model to `session` or `jwt` first.",
            ));
        }

        // A `public` endpoint bypasses every guard — including the Tenant guard
        // that scopes a tenant-owned entity to its owner. If such an endpoint's
        // repo entity belongs_to the tenancy root, marking it public would expose
        // one tenant's rows to anyone. Flag the contradiction.
        fn check_public_on_tenant_owned(
            d: &Design,
            m: &ModuleDesign,
            ptr: &str,
            qs: &mut Vec<Question>,
        ) {
            for (i, ep) in m.endpoints.iter().enumerate() {
                // Tenant-owned directly OR transitively (#102): `tenant_path` resolves
                // a grandchild through its parent chain, so a public endpoint on a
                // deeply-owned entity is flagged too — matching the transitive
                // ownership the guard/lint recognize.
                if ep.public
                    && endpoint_repo_entity(m, ep).is_some_and(|name| d.tenant_path(name).is_some())
                {
                    qs.push(q(
                        format!("{ptr}/endpoints/{i}"),
                        "endpoint is public but its entity is tenant-owned — public endpoints bypass the Tenant guard; remove public or move the endpoint off the tenant-owned entity".to_string(),
                    ));
                }
            }
            for (i, sub) in m.subroutes.iter().enumerate() {
                check_public_on_tenant_owned(d, sub, &format!("{ptr}/subroutes/{i}"), qs);
            }
        }
        for (i, m) in d.modules.iter().enumerate() {
            check_public_on_tenant_owned(d, m, &format!("/modules/{i}"), &mut qs);
        }

        // JC0558 (#148): the tenant twin of JC0549(c). In an auth design, an
        // endpoint on the tenant entity OR a tenant-owned entity (directly or
        // transitively) that is neither guarded nor `public` is ANONYMOUS:
        // genroute emits the guard only under `mode.auth && ep.is_guarded()`
        // (genroute.rs:196), so such a handler gets no `Dep<Tenant>` and no
        // `CurrentUser` and any caller can read (or write) any tenant's rows —
        // with a green `jerrycan check`. `check_public_on_tenant_owned` above
        // keys on `ep.public`; the merely unguarded-non-public case was unpoliced
        // (JL0004 covers mutations only; in a childless tenant module JL0006 never
        // scans handlers.rs). Refuse it (correct-by-construction), exempting the
        // signature-authed webhook shape exactly as JL0004 does (it proves itself
        // by signature, not a session).
        //
        // Domain = the tenant ROOT (`name == tenancy.entity`) OR a tenant-owned
        // entity (`tenant_path(name).is_some()`). `tenant_path` deliberately
        // returns None for the tenant itself (design.rs:970 — "the tenant is not
        // tenant-owned"), so the root arm is spelled out explicitly: the issue's
        // headline case ("read any tenant's row by id") is the tenant's OWN
        // unguarded detail route, which `tenant_path` alone would miss.
        //
        // Entity resolution uses the STRICT resolver (`endpoint_repo_entity_strict`
        // — explicit signals only), NOT the lenient `endpoint_repo_entity` the
        // (public-only) mirror uses. WHY (Rule 9): this predicate fires on
        // NON-public endpoints, so it reaches the entity-less custom endpoints the
        // public-only check never touched (e.g. `GET /usage` returning custom JSON
        // in an entity-bearing module, or an entity-less join/leave subroute).
        // The lenient first-entity fallback would tie those to a tenant-owned
        // NEIGHBOR they never read and falsely refuse them; strict returns `None`
        // for an endpoint no explicit signal ties to a repo, so it fires only on a
        // real tenant-scoped read/write — the same strict narrowing JC0549(c) and
        // JC0550 use for exactly this security-sensitive reason.
        if active_auth_model {
            fn check_anonymous_on_tenant_scoped(
                d: &Design,
                tenant: &str,
                m: &ModuleDesign,
                ptr: &str,
                qs: &mut Vec<Question>,
            ) {
                for (i, ep) in m.endpoints.iter().enumerate() {
                    let Some(entity) = endpoint_repo_entity_strict(m, ep) else {
                        continue;
                    };
                    let tenant_scoped = entity == tenant || d.tenant_path(entity).is_some();
                    if tenant_scoped
                        && !ep.public
                        && !ep.is_guarded()
                        && !ep.declares_signature_auth()
                    {
                        qs.push(q(
                            format!("{ptr}/endpoints/{i}"),
                            format!(
                                "Endpoint `{}` ({:?} {}) is on the tenant-scoped entity `{}` but is neither authenticated nor `public` — it emits no `Dep<Tenant>` guard and no `CurrentUser`, so an anonymous caller could read or write any tenant's rows. Set `auth_required: true` so the membership guard scopes it. See `jerrycan explain JC0558`.",
                                ep.operation_id, ep.method, ep.path, entity,
                            ),
                        ));
                    }
                }
                for (i, sub) in m.subroutes.iter().enumerate() {
                    check_anonymous_on_tenant_scoped(
                        d,
                        tenant,
                        sub,
                        &format!("{ptr}/subroutes/{i}"),
                        qs,
                    );
                }
            }
            for (i, m) in d.modules.iter().enumerate() {
                check_anonymous_on_tenant_scoped(
                    d,
                    &tenancy.entity,
                    m,
                    &format!("/modules/{i}"),
                    &mut qs,
                );
            }
        }

        // JC0545 (#102): an entity that reaches the tenant through TWO or more
        // distinct `belongs_to` chains (a diamond) is ambiguous — `tenant_path`
        // resolves it to `None`, which would leave it UNSCOPED and re-open the
        // cross-tenant leak. Generation is gated on validation, so rejecting the
        // design here keeps a half-scoped entity from ever reaching the generator.
        fn check_ambiguous_tenant_path(
            d: &Design,
            m: &ModuleDesign,
            ptr: &str,
            qs: &mut Vec<Question>,
        ) {
            for (i, e) in m.entities.iter().enumerate() {
                if d.tenant_path_branch_count(&e.name) >= 2 {
                    qs.push(q(
                        format!("{ptr}/entities/{i}"),
                        format!(
                            "Entity `{}` reaches the tenant through more than one `belongs_to` path (a diamond graph), so jerrycan cannot decide which chain defines tenant ownership — guessing would scope its reads/writes to the wrong tenant and re-open the cross-tenant leak. Collapse its tenant ownership to a SINGLE `belongs_to` path (drop the redundant parent, or split the entity), so exactly one chain reaches the tenant. See `jerrycan explain JC0545`.",
                            e.name
                        ),
                    ));
                }
            }
            for (i, sub) in m.subroutes.iter().enumerate() {
                check_ambiguous_tenant_path(d, sub, &format!("{ptr}/subroutes/{i}"), qs);
            }
        }
        for (i, m) in d.modules.iter().enumerate() {
            check_ambiguous_tenant_path(d, m, &format!("/modules/{i}"), &mut qs);
        }

        // JC0553 (#141): with tenancy, jerrycan reserves the `{tenant}_members`
        // membership table and the `pub struct {Tenant}Member` row type (issue
        // #107) for the generated member surface. An entity (other than the
        // tenant) whose RESOLVED table name equals `{tenant}_members` — one named
        // `{Tenant}Member`, whose default table is exactly that, or one with an
        // explicit `table` override onto it — OR whose NAME equals `{Tenant}Member`
        // collides with that surface: the generator would emit the same table
        // twice (a raw `table "..._members" already exists` mid-scaffold, after a
        // clean `check`) or two `struct {Tenant}Member` definitions. Reserved
        // names are computed EXACTLY as the generator names them
        // (`{Design::to_snake(tenant)}_members`, `{tenant}Member`) so the refusal
        // matches what is reserved. Fail loud here with a rename suggestion.
        let reserved_members_table = format!("{}_members", Design::to_snake(&tenancy.entity));
        let reserved_member_struct = format!("{}Member", tenancy.entity);
        fn check_membership_collision(
            d: &Design,
            tenant: &str,
            reserved_table: &str,
            reserved_struct: &str,
            m: &ModuleDesign,
            ptr: &str,
            qs: &mut Vec<Question>,
        ) {
            for (i, e) in m.entities.iter().enumerate() {
                if e.name == tenant {
                    continue;
                }
                let name_collides = e.name == reserved_struct;
                let table_collides = d.table_name(&e.name) == reserved_table;
                if !name_collides && !table_collides {
                    continue;
                }
                // Point at the most actionable location: the name for a name (or
                // name+table) collision — renaming fixes both — else the entity.
                let (id, detail) = if name_collides {
                    (
                        format!("{ptr}/entities/{i}/name"),
                        if table_collides {
                            format!(
                                "its name matches the generated membership row type `pub struct {reserved_struct}` (issue #107), and its default table `{reserved_table}` is the membership table jerrycan generates for `{tenant}`"
                            )
                        } else {
                            format!(
                                "its name matches the generated membership row type `pub struct {reserved_struct}` (issue #107)"
                            )
                        },
                    )
                } else {
                    (
                        format!("{ptr}/entities/{i}"),
                        format!(
                            "its table `{reserved_table}` is the membership table jerrycan generates for `{tenant}`"
                        ),
                    )
                };
                qs.push(q(
                    id,
                    format!(
                        "Entity `{}` collides with the member surface jerrycan generates for tenant `{tenant}`: {detail} — the generator would emit that table/type twice and the scaffold would abort mid-migration with a raw `table \"{reserved_table}\" already exists` (after a clean `check`). Rename the entity (e.g. `{}Record` or a domain-specific name) so it no longer equals the reserved `{reserved_struct}` type or resolves to the reserved `{reserved_table}` table. See `jerrycan explain JC0553`.",
                        e.name, e.name
                    ),
                ));
            }
            for (i, sub) in m.subroutes.iter().enumerate() {
                check_membership_collision(
                    d,
                    tenant,
                    reserved_table,
                    reserved_struct,
                    sub,
                    &format!("{ptr}/subroutes/{i}"),
                    qs,
                );
            }
        }
        for (i, m) in d.modules.iter().enumerate() {
            check_membership_collision(
                d,
                &tenancy.entity,
                &reserved_members_table,
                &reserved_member_struct,
                m,
                &format!("/modules/{i}"),
                &mut qs,
            );
        }
    }

    // JC0550 (#88): the tenant entity's OWN detail route must address the tenant
    // by its pk fk. The load normalization rewrites only the literal `{id}`
    // (→ `{fk}`), so a route addressed by any OTHER param (`/{slug}`,
    // `/by-slug/{slug}`) survives it — and the membership guard, which verifies
    // the tenant NAMED BY THE PATH FK, cannot bind it: the handler would be
    // generated with a bare `CurrentUser` and NO membership check at all,
    // silently. Renaming the param is NOT a fix — the guard parses the path
    // value as the tenant PK type, so a slug would be reinterpreted as a pk —
    // hence a loud refusal (the JC0549 pattern: a latent unimplementable shape
    // becomes a clear fork). Checked over a normalized clone (the
    // `router_param_conflict` precedent): production callers hand `validate()` a
    // load-normalized design already, and re-normalizing is idempotent, so the
    // conventional `/{id}` passes for direct (test/programmatic) callers too.
    //
    // Two resolution rules keep the refusal honest (this check is
    // SECURITY-SENSITIVE, so it follows the JC0549(c) precedent):
    //   - The target entity is resolved with design.rs's STRICT resolver
    //     (`super::design::endpoint_repo_entity_strict` — qualified, because the
    //     module-local lenient `endpoint_repo_entity` above shadows the design.rs
    //     glob and LACKS the collection-creator arm). The local resolver both
    //     OVER-fires — a sibling's bodyless `DELETE /siblings/{id}` in the tenant
    //     module falls back to the first entity (the tenant) and is refused while
    //     the refusal's own message says to use `/{id}` — and UNDER-fires: a
    //     NON-FIRST tenant's bodyless `DELETE /{slug}` falls back to the first
    //     entity (a sibling) and the real no-membership-check hole ships
    //     question-free. Strict resolves the creator arm (the non-first tenant
    //     fires), returns the sibling's own entity (siblings never fire), and
    //     returns `None` for an entity-less custom endpoint (`GET
    //     /export/{format}` never fires). Accepted residual: a creator-less
    //     bodyless tenant detail route with a non-pk param in a single-entity
    //     module resolves to NO entity and is not caught — that shape carries no
    //     signal to resolve its entity (the #143 family).
    //   - The route is a hole only when the tenant fk appears in NONE of its
    //     path params — checked against the MOUNT-RESOLVED path (the node's
    //     `effective_mount()` + `ep.path`, exactly as `endpoint_tenant_shape`
    //     resolves it): a multi-param `/{fk}/{sub}` binds the fk, and so does a
    //     mount-carried fk (subroute mount `/{fk}/history`, path `/{year}`) —
    //     the guard membership-checks the full route either way — so only a
    //     route whose resolved path never names the fk fires.
    if let Some(tenancy) = &d.tenancy {
        let fk = Design::fk_column(&tenancy.entity);
        let normalized = {
            let mut n = d.clone();
            n.normalize_tenant_detail_routes();
            n
        };
        // The trailing path param — the one that addresses the detail row.
        fn trailing_path_param(path: &str) -> Option<&str> {
            let rest = &path[path.rfind('{')? + 1..];
            rest.find('}').map(|end| &rest[..end])
        }
        fn check_tenant_detail_param(
            tenant: &str,
            fk: &str,
            m: &ModuleDesign,
            ptr: &str,
            qs: &mut Vec<Question>,
        ) {
            let fk_token = format!("{{{fk}}}");
            let mount = m.effective_mount();
            let mount = mount.strip_suffix('/').unwrap_or(&mount);
            for (i, ep) in m.endpoints.iter().enumerate() {
                if super::design::endpoint_repo_entity_strict(m, ep) != Some(tenant) {
                    continue;
                }
                let Some(param) = trailing_path_param(&ep.path) else {
                    continue;
                };
                if !format!("{mount}{}", ep.path).contains(&fk_token) {
                    qs.push(q(
                        format!("{ptr}/endpoints/{i}"),
                        format!(
                            "Endpoint `{}` ({:?} {}) is the tenant `{}`'s own detail route but addresses it by `{{{}}}`, not its pk `{{{}}}` — the membership guard verifies the tenant named by the path fk, so a non-pk param (e.g. a slug) cannot be membership-checked and the route would run with no membership check at all. Use `/{{id}}` (auto-normalized to `/{{{}}}`) or `/{{{}}}` directly; slug-based tenant addressing is not yet supported. See `jerrycan explain JC0550`.",
                            ep.operation_id, ep.method, ep.path, tenant, param, fk, fk, fk
                        ),
                    ));
                }
            }
            for (i, sub) in m.subroutes.iter().enumerate() {
                check_tenant_detail_param(tenant, fk, sub, &format!("{ptr}/subroutes/{i}"), qs);
            }
        }
        for (i, m) in normalized.modules.iter().enumerate() {
            check_tenant_detail_param(&tenancy.entity, &fk, m, &format!("/modules/{i}"), &mut qs);
        }
    }

    // JC0549 (#105): the public-read/owner-write shape. `public_read` is a
    // modifier on the per-user ownership shape (#79) — reads public, writes
    // owner-scoped — so (a) it is valid ONLY on an identity-owned, non-tenant
    // entity in an auth design, and (b) every WRITE of a public_read entity must
    // stay owner-gated (a public/unguarded write is the open door: anyone could
    // create/mutate rows the public then reads). Independently of opt-in, (c)
    // closes the latent unguarded-per-user-GET bug: in db+auth mode a per-user
    // entity's repo emits ONLY the owner-scoped accessors and an unguarded
    // handler receives no session user, so an unguarded GET that has NOT opted
    // into public_read is unimplementable — refuse it with a clear fork instead
    // of generating a dead-end stub.
    fn check_public_read(d: &Design, m: &ModuleDesign, ptr: &str, qs: &mut Vec<Question>) {
        for (i, e) in m.entities.iter().enumerate() {
            if !e.public_read {
                continue;
            }
            if !d.wants_auth() {
                qs.push(q(
                    format!("{ptr}/entities/{i}"),
                    format!(
                        "Entity `{}` declares public_read but the design has no active auth model — public_read keeps WRITES owner-gated, which needs an authenticated session; set auth.model to `session` or `jwt`, or drop public_read. See `jerrycan explain JC0549`.",
                        e.name
                    ),
                ));
            }
            if !Design::has_identity_fk(e) {
                qs.push(q(
                    format!("{ptr}/entities/{i}"),
                    format!(
                        "Entity `{}` declares public_read but carries no identity fk — public_read is a modifier on the per-user ownership shape (reads public, writes owner-scoped), so the entity needs a `belongs_to` the auth identity entity; add it, or drop public_read. See `jerrycan explain JC0549`.",
                        e.name
                    ),
                ));
            }
            // Tenant-owned directly OR transitively (#102) — mirrors the
            // public-endpoint-on-tenant-owned rejection above: a public read
            // would bypass the Tenant guard and expose one tenant's rows to
            // anyone. public_read is identity-owned-only in v1.
            if d.tenant_path(&e.name).is_some() {
                qs.push(q(
                    format!("{ptr}/entities/{i}"),
                    format!(
                        "Entity `{}` declares public_read but is tenant-owned — public reads would bypass the Tenant guard and expose one tenant's rows to anyone; public_read is identity-owned-only, so remove public_read or move the entity off tenancy. See `jerrycan explain JC0549`.",
                        e.name
                    ),
                ));
            }
        }
        for (i, ep) in m.endpoints.iter().enumerate() {
            if matches!(ep.method, HttpMethod::GET) {
                // (c) The latent-bug closure. Resolved via the STRICT resolver
                // (`design::endpoint_repo_entity_strict` — explicit signals
                // only): an ENTITY-LESS GET (custom-JSON success, no body, no
                // `{param}` collection — the documented hand-written
                // `Json<serde_json::Value>` shape) reads NO entity's repo, so
                // the first-entity fallback must not tie it to a per-user
                // neighbor and falsely refuse an implementable `public: true`
                // custom GET as unimplementable. Per-user ownership IS
                // `Design::entity_is_per_user_owned` — the one shared classifier
                // (#105 §F); db mode is the extra gate because only `sql_repo`
                // suppresses the unscoped reads (a memory repo keeps plain
                // `all`/`get`, so the stub stays implementable there). `public:
                // true` is NOT a carve-out: it is just the second spelling of an
                // unguarded read (public cannot combine with a guard), and the
                // stub it generates is exactly as unimplementable — only the
                // `public_read` entity flag makes an open read coherent here.
                let Some(name) = endpoint_repo_entity_strict(m, ep) else {
                    continue;
                };
                let Some(e) = m.entities.iter().find(|e| e.name == name) else {
                    continue;
                };
                let per_user_owned = d.entity_is_per_user_owned(e);
                if !ep.is_guarded()
                    && d.wants_db()
                    && per_user_owned
                    && !d.entity_is_public_read(&e.name)
                {
                    qs.push(q(
                        format!("{ptr}/endpoints/{i}"),
                        format!(
                            "Endpoint `{}` is an unguarded read on the owner-scoped entity `{}`, which is unimplementable — the entity's repo emits only the owner-scoped accessors and an unguarded handler receives no session user. Set `public_read: true` on `{}` to make its reads public, or keep the GET authenticated (`auth_required: true`). See `jerrycan explain JC0549`.",
                            ep.operation_id, e.name, e.name
                        ),
                    ));
                }
            } else {
                // (b) A write must stay owner-gated regardless of the flag.
                // Writes keep the LENIENT resolution (the repo the generated
                // stub actually binds — first-entity fallback included): the
                // failure mode here is fail-CLOSED, so over-matching only asks.
                let Some(name) = endpoint_repo_entity(m, ep) else {
                    continue;
                };
                let Some(e) = m.entities.iter().find(|e| e.name == name) else {
                    continue;
                };
                if e.public_read && (ep.public || !ep.is_guarded()) {
                    qs.push(q(
                        format!("{ptr}/endpoints/{i}"),
                        format!(
                            "Endpoint `{}` ({:?} {}) is a write on the public_read entity `{}` but is public/unguarded — public_read makes READS public while WRITES stay owner-gated; set `auth_required: true` (and drop `public`) on every write of `{}`. See `jerrycan explain JC0549`.",
                            ep.operation_id, ep.method, ep.path, e.name, e.name
                        ),
                    ));
                }
            }
        }
        for (i, sub) in m.subroutes.iter().enumerate() {
            check_public_read(d, sub, &format!("{ptr}/subroutes/{i}"), qs);
        }
    }
    for (i, m) in d.modules.iter().enumerate() {
        check_public_read(d, m, &format!("/modules/{i}"), &mut qs);
    }

    // JC0559 (#115): a table-level composite `unique` group must be BUILDABLE — the
    // generator emits one `CREATE UNIQUE INDEX (col, …)` per group (genroute), so a
    // group is refused when it: has fewer than 2 columns (a single-column unique must
    // use `Field.unique`, not a 1-col group); names a column that is neither a
    // declared field nor a `belongs_to` fk column of the entity (the index would
    // reference a non-existent column and fail at migration apply); or duplicates
    // another group's column set (order-insensitive — redundant). Applies to every
    // design (no tenancy/auth prerequisite); an entity with an empty `unique` is inert
    // (byte-identity baseline).
    fn check_composite_unique(m: &ModuleDesign, ptr: &str, qs: &mut Vec<Question>) {
        for (i, e) in m.entities.iter().enumerate() {
            if e.unique.is_empty() {
                continue;
            }
            // Valid columns: declared fields ∪ belongs_to fk columns.
            let mut valid: std::collections::HashSet<String> =
                e.fields.iter().map(|f| f.name.clone()).collect();
            for b in &e.belongs_to {
                valid.insert(b.fk_column());
            }
            let mut seen_sets: Vec<std::collections::BTreeSet<String>> = Vec::new();
            for (g, group) in e.unique.iter().enumerate() {
                let gptr = format!("{ptr}/entities/{i}/unique/{g}");
                // Gate on the count of DISTINCT columns, not raw length: a repeated
                // column like `["a", "a"]` has 2 entries but 1 distinct column, so
                // `UNIQUE(a, a)` would silently make column `a` globally unique —
                // caught here alongside the true <2 case.
                let distinct: std::collections::BTreeSet<&String> = group.iter().collect();
                if distinct.len() < 2 {
                    qs.push(q(
                        gptr.clone(),
                        format!(
                            "Entity `{}` composite `unique` group #{g} has fewer than 2 DISTINCT columns ({group:?}) — a table-level composite UNIQUE expresses a `UNIQUE(a, b)` invariant a single field cannot, and a repeated column (`[\"a\", \"a\"]`) would silently make that lone column globally unique. For single-column uniqueness set `unique: true` on the field instead; a composite group needs at least 2 distinct columns. See `jerrycan explain JC0559`.",
                            e.name
                        ),
                    ));
                }
                for col in group {
                    if !valid.contains(col) {
                        qs.push(q(
                            gptr.clone(),
                            format!(
                                "Entity `{}` composite `unique` group #{g} names column `{col}`, which is neither a declared field nor a `belongs_to` fk column of `{}` — the generated `CREATE UNIQUE INDEX` would reference a column that does not exist and fail at migration apply. Use a declared field name or a belongs_to fk column (`snake_case(entity) + \"_id\"`). See `jerrycan explain JC0559`.",
                                e.name, e.name
                            ),
                        ));
                    }
                }
                let set: std::collections::BTreeSet<String> = group.iter().cloned().collect();
                if seen_sets.contains(&set) {
                    qs.push(q(
                        gptr,
                        format!(
                            "Entity `{}` composite `unique` group #{g} ({group:?}) duplicates an earlier group with the same column set (order does not matter) — a redundant index. List each column set once. See `jerrycan explain JC0559`.",
                            e.name
                        ),
                    ));
                } else {
                    seen_sets.push(set);
                }
            }
        }
        for (i, sub) in m.subroutes.iter().enumerate() {
            check_composite_unique(sub, &format!("{ptr}/subroutes/{i}"), qs);
        }
    }
    for (i, m) in d.modules.iter().enumerate() {
        check_composite_unique(m, &format!("/modules/{i}"), &mut qs);
    }

    // JC0560 (#119): a `belongs_to` fk column must be BUILDABLE and DISTINCT. The
    // fk column a belongs_to derives (`{as}_id` when aliased, else
    // `snake(entity)_id`) becomes a Model field AND a migration column, so per
    // entity: (1) every `as` must be snake_case (a malformed alias yields an
    // invalid column/Rust field); (2) no two belongs_to may derive the SAME fk
    // column — two un-aliased refs to one target both derive `snake(entity)_id`,
    // or an `as` collides with another belongs_to's fk — which would emit a
    // duplicate Model field and a duplicate migration column; (3) an fk column
    // must not collide with a declared field name or the pk `id`. The alias exists
    // precisely so two references to one entity (a ledger Transfer's
    // from_account/to_account, a self-referential Comment's parent) get DISTINCT
    // columns and distinct DDL constraint names. An entity with 0/1 un-aliased
    // belongs_to and no `as` is inert (byte-identity baseline).
    fn check_belongs_to_aliases(d: &Design, m: &ModuleDesign, ptr: &str, qs: &mut Vec<Question>) {
        // #119 Finding 1: an `as` alias must not land on a RESERVED fk column that
        // the target doesn't own — the identity fk `user_id` (in an auth design) or
        // the tenancy fk — else the alias hijacks identity/tenant scoping and the
        // generated Rust fails opaquely instead of a clean design-time refusal.
        let auth_active = d
            .auth
            .as_ref()
            .map(|a| a.model != AuthModel::None)
            .unwrap_or(false);
        let tenant_fk = d
            .tenancy
            .as_ref()
            .map(|t| (Design::fk_column(&t.entity), t.entity.clone()));
        for (i, e) in m.entities.iter().enumerate() {
            let field_names: std::collections::HashSet<&str> =
                e.fields.iter().map(|f| f.name.as_str()).collect();
            let mut seen: std::collections::HashMap<String, usize> =
                std::collections::HashMap::new();
            for (bi, b) in e.belongs_to.iter().enumerate() {
                let bptr = format!("{ptr}/entities/{i}/belongs_to/{bi}");
                // (1) A malformed alias yields a nonsense `{as}_id` column — refuse
                // it here and skip the collision checks (they'd be noise on garbage).
                if let Some(a) = &b.r#as
                    && !is_snake(a)
                {
                    qs.push(q(
                        bptr,
                        format!(
                            "Entity `{}` belongs_to `{}` has a malformed `as` alias `{a}` — an alias must be snake_case (^[a-z][a-z0-9_]*$); the fk column is derived as `{{as}}_id`. See `jerrycan explain JC0560`.",
                            e.name, b.entity
                        ),
                    ));
                    continue;
                }
                let col = b.fk_column();
                // (4) #119 Finding 1: reject an alias that lands on a reserved fk the
                // target doesn't own — hijacking identity/tenant scoping.
                if auth_active
                    && col == AUTH_IDENTITY_FK_COLUMN
                    && Design::fk_column(&b.entity) != AUTH_IDENTITY_FK_COLUMN
                {
                    qs.push(q(
                        bptr.clone(),
                        format!(
                            "Entity `{}` belongs_to `{}` with `as` deriving fk column `{col}` — that is the reserved identity fk (the authenticated user's column); only `belongs_to` the identity entity may own `{AUTH_IDENTITY_FK_COLUMN}`. Choose a different `as` alias. See `jerrycan explain JC0560`.",
                            e.name, b.entity
                        ),
                    ));
                }
                if let Some((tfk, tent)) = &tenant_fk
                    && col == *tfk
                    && b.entity != *tent
                {
                    qs.push(q(
                        bptr.clone(),
                        format!(
                            "Entity `{}` belongs_to `{}` with `as` deriving fk column `{col}` — that is the reserved tenancy fk for `{tent}`; aliasing the tenant fk breaks tenant scoping. Choose a different `as` alias. See `jerrycan explain JC0560`.",
                            e.name, b.entity
                        ),
                    ));
                }
                // (3) The fk column is generated; a same-named declared field or the
                // pk `id` would be a duplicate column.
                if col == "id" || field_names.contains(col.as_str()) {
                    let against = if col == "id" {
                        "the pk `id`".to_string()
                    } else {
                        format!("a declared field `{col}`")
                    };
                    qs.push(q(
                        bptr.clone(),
                        format!(
                            "Entity `{}` belongs_to `{}` derives fk column `{col}`, which collides with {against} — the fk column is generated, so it must not duplicate a declared field or the pk `id`. Rename the field or give the belongs_to a distinct `as`. See `jerrycan explain JC0560`.",
                            e.name, b.entity
                        ),
                    ));
                }
                // (2) Two belongs_to deriving the same fk column → duplicate column.
                if let Some(&first) = seen.get(&col) {
                    qs.push(q(
                        bptr,
                        format!(
                            "Entity `{}` belongs_to `{}` derives fk column `{col}`, the SAME column as belongs_to #{first} — two references to one entity collide into a duplicate column. Add a distinct `as` alias to at least one (e.g. `\"as\": \"from_account\"` → `from_account_id`). See `jerrycan explain JC0560`.",
                            e.name, b.entity
                        ),
                    ));
                } else {
                    seen.insert(col, bi);
                }
            }
        }
        for (i, sub) in m.subroutes.iter().enumerate() {
            check_belongs_to_aliases(d, sub, &format!("{ptr}/subroutes/{i}"), qs);
        }
    }
    for (i, m) in d.modules.iter().enumerate() {
        check_belongs_to_aliases(d, m, &format!("/modules/{i}"), &mut qs);
    }

    // Jobs require a database: the engine's default store is Postgres and the
    // generated `jobs(db)` wiring + JOBS_MIGRATIONS run over `jerrycan::db::Db`.
    // A jobs-without-db design can't compile, so reject it here (one error for the
    // whole jobs list, not per-job). The shallow cron-shape check below stays as
    // the design-time guard; the engine deep-parses each expression at serve and
    // fails loud (`Jobs::cron` panics on a bad expression), so a malformed-but-
    // cron-shaped schedule is caught there rather than adding a jerrycan-jobs dep
    // to the CLI just for validation.
    if d.wants_jobs() && !d.wants_db() {
        qs.push(q(
            "/jobs".to_string(),
            "Jobs require a database dependency — add `db` to `dependencies` (background jobs run over a Postgres store).".to_string(),
        ));
    }

    // Jobs: snake_case unique names; a present schedule must look cron-shaped
    // (full cron parsing arrives with the engine in v2.3).
    let mut seen_job_names = std::collections::HashSet::new();
    for (i, job) in d.jobs.iter().enumerate() {
        if !is_snake(&job.name) {
            qs.push(q(
                format!("/jobs/{i}/name"),
                format!(
                    "Job name `{}` must be snake_case (^[a-z][a-z0-9_]*$).",
                    job.name
                ),
            ));
        }
        if !seen_job_names.insert(job.name.as_str()) {
            qs.push(q(
                format!("/jobs/{i}/name"),
                format!(
                    "Job name `{}` is already used — job names must be unique.",
                    job.name
                ),
            ));
        }
        // The queue is interpolated RAW into generated Rust string literals
        // (`.queue("{q}", ...)` / `.cron(..., "{queue}")` in jobsgen.rs), so a
        // queue with a `"` (or any non-identifier char) breaks the generated
        // crate at build time, far from the design. Validate it like every other
        // identifier interpolated into generated Rust (is_snake job names, etc.).
        if let Some(ref queue) = job.queue
            && !is_snake(queue)
        {
            qs.push(q(
                format!("/jobs/{i}/queue"),
                format!("Job queue `{queue}` must be snake_case (^[a-z][a-z0-9_]*$)."),
            ));
        }
        if let Some(ref schedule) = job.schedule {
            let fields: Vec<&str> = schedule.split_whitespace().collect();
            let cron_shaped = fields.len() == 5
                && fields.iter().all(|f| {
                    !f.is_empty()
                        && f.chars()
                            .all(|c| c.is_ascii_digit() || matches!(c, '*' | ',' | '/' | '-'))
                });
            if !cron_shaped {
                qs.push(q(
                    format!("/jobs/{i}/schedule"),
                    format!(
                        "Schedule `{schedule}` is not a 5-field cron expression (minute hour day month weekday, each [0-9*,/-]).",
                    ),
                ));
            }
        }
    }

    // Storage (contract v2). Bucket names/mime patterns are interpolated into
    // generated Rust literals and mounts, so everything is validated up front
    // (the job-queue precedent: reject at design time, not at generated-crate
    // build time). NOTE: `visibility: public` + a tenant-scoped owner is
    // deliberately allowed (public read, scoped write) — no question.
    if let Some(ref storage) = d.storage {
        if d.contract_version < 2 {
            qs.push(q(
                "/storage",
                "The storage block requires contract_version 2 — bump contract_version (v0/v1 designs stay valid without storage).",
            ));
        }
        if !d.wants_db() {
            qs.push(q(
                "/storage",
                "Storage requires a database dependency — add `db` to `dependencies` (object metadata lives in the storage_objects table).",
            ));
        }
        let active_auth_model = d
            .auth
            .as_ref()
            .map(|a| a.model != AuthModel::None)
            .unwrap_or(false);
        if !active_auth_model {
            qs.push(q(
                "/storage",
                "Storage requires an active auth model — bucket mutations (upload/delete/sign) are always guarded; set auth.model to `session` or `jwt`.",
            ));
        }
        let module_mounts: std::collections::HashSet<String> =
            d.modules.iter().map(|m| m.effective_mount()).collect();
        // A custom base_path is emitted verbatim into every bucket mount, so it
        // must be a clean absolute path (leading `/`, no trailing/`//`), like a
        // module mount.
        if let Some(base) = &storage.base_path {
            if !base.starts_with('/') {
                qs.push(q(
                    "/storage/base_path",
                    format!("Storage base_path `{base}` must start with '/'."),
                ));
            }
            if base.contains("//") || (base.len() > 1 && base.ends_with('/')) {
                qs.push(q(
                    "/storage/base_path",
                    format!("Storage base_path `{base}` must not contain `//` or end with a trailing slash."),
                ));
            }
        }
        let base_path = storage.effective_base_path();
        let mut seen_buckets = std::collections::HashSet::new();
        for (i, b) in storage.buckets.iter().enumerate() {
            let bptr = format!("/storage/buckets/{i}");
            if !is_kebab(&b.name) {
                qs.push(q(
                    format!("{bptr}/name"),
                    format!("Bucket `{}` is not kebab-case (^[a-z][a-z0-9-]*$).", b.name),
                ));
            }
            let ident = b.name.replace('-', "_");
            if is_rust_keyword(&ident) {
                qs.push(q(
                    format!("{bptr}/name"),
                    format!("Bucket `{}` becomes the Rust module `{ident}`, which is a keyword — rename it.", b.name),
                ));
            }
            if !seen_buckets.insert(b.name.as_str()) {
                qs.push(q(
                    format!("{bptr}/name"),
                    format!(
                        "Bucket name `{}` is already used — bucket names must be unique.",
                        b.name
                    ),
                ));
            }
            let bucket_mount = format!("{base_path}/{}", b.name);
            if module_mounts.contains(&bucket_mount) {
                qs.push(q(
                    format!("{bptr}/name"),
                    format!("Bucket `{}` mounts at {bucket_mount} which collides with a module mount — rename the bucket, change storage.base_path, or remount the module.", b.name),
                ));
            }
            if let Some(ref owner) = b.owner
                && !entity_names.contains(owner.as_str())
            {
                qs.push(q(
                    format!("{bptr}/owner"),
                    format!("Bucket owner `{owner}` is not a declared entity anywhere in the design — define it or fix the reference."),
                ));
            }
            // JC0545, storage facet (0.5.4): a bucket owner that reaches the
            // tenant through TWO or more `belongs_to` chains (a diamond) makes
            // `tenant_path` resolve `None`, which would SILENTLY degrade the
            // bucket from tenant scope to plain per-user scope — no Tenant
            // guard, no tenant_id stamp. Refuse it at the bucket pointer (the
            // entity-level diamond check above fires too; this one names the
            // bucket that degrades). A tenant-itself or no-tenant-path owner
            // stays User/Unowned as before — only the ≥2-branch case is refused.
            if let Some(ref owner) = b.owner
                && d.tenant_path_branch_count(owner) >= 2
            {
                qs.push(q(
                    format!("{bptr}/owner"),
                    format!(
                        "Bucket `{}` is owned by `{owner}`, which reaches the tenant through more than one `belongs_to` path (a diamond graph) — jerrycan cannot decide which chain defines tenant ownership, and falling back to per-user scope would silently drop the Tenant guard and leak the bucket across tenants. Collapse `{owner}`'s tenant ownership to a SINGLE `belongs_to` path. See `jerrycan explain JC0545`.",
                        b.name
                    ),
                ));
            }
            if b.owner_prefix && b.owner.is_none() {
                qs.push(q(
                    format!("{bptr}/owner_prefix"),
                    format!("Bucket `{}` sets owner_prefix without an owner — owner_prefix stores keys under {{owner_id}}/… and needs `owner`.", b.name),
                ));
            }
            if let Some(ref max) = b.max_size
                && Design::parse_size(max).is_none()
            {
                qs.push(q(
                    format!("{bptr}/max_size"),
                    format!(
                        "max_size `{max}` is not a size — use ^[0-9]+(B|KB|MB|GB)?$ (e.g. \"5MB\")."
                    ),
                ));
            }
            for (j, m) in b.allowed_mime.iter().enumerate() {
                // The runtime matcher understands exactly type/subtype, type/*
                // and */*. A wildcard TYPE with a concrete subtype (`*/png`)
                // would parse here but can never match — every upload would
                // 415 — so it is rejected as malformed too.
                let well_formed = m.split_once('/').is_some_and(|(t, sub)| {
                    let seg_ok = |s: &str| {
                        !s.is_empty()
                            && s.bytes().all(|c| {
                                c.is_ascii_lowercase()
                                    || c.is_ascii_digit()
                                    || matches!(c, b'.' | b'+' | b'-')
                            })
                    };
                    (seg_ok(t) && (seg_ok(sub) || sub == "*")) || (t == "*" && sub == "*")
                });
                if !well_formed {
                    qs.push(q(
                        format!("{bptr}/allowed_mime/{j}"),
                        format!(
                            "`{m}` is not a supported mime pattern — use type/subtype, type/* or */* (lowercase)."
                        ),
                    ));
                }
            }
            // JC0556 (#132): `write_roles` gates blob upload/delete by tenant
            // role. It is only meaningful on a TENANT-scoped bucket (owner IS
            // the tenancy entity), and each entry must be a declared member_role.
            // A write gate declared where it emits NOTHING (a non-tenant/no-tenancy
            // bucket) would silently leave writes open — a security footgun — so
            // refuse it loud rather than ignore it.
            if !b.write_roles.is_empty() {
                match d.tenancy.as_ref() {
                    Some(t) if b.owner.as_deref() == Some(t.entity.as_str()) => {
                        for (k, wr) in b.write_roles.iter().enumerate() {
                            if !t.member_roles.iter().any(|r| r == wr) {
                                qs.push(q(
                                    format!("{bptr}/write_roles/{k}"),
                                    format!(
                                        "Bucket `{}` write_roles entry `{wr}` is not a declared tenancy member_role — each write role must be one of {:?}. See `jerrycan explain JC0556`.",
                                        b.name, t.member_roles
                                    ),
                                ));
                            }
                        }
                    }
                    _ => {
                        qs.push(q(
                            format!("{bptr}/write_roles"),
                            format!(
                                "Bucket `{}` sets write_roles but is not tenant-scoped (its owner is not the tenancy entity, or the design has no tenancy) — write_roles gates writes by TENANT role and emits no gate here, so a declared write restriction would silently do nothing. Drop write_roles, or make the bucket tenant-owned (owner = the tenancy entity). See `jerrycan explain JC0556`.",
                                b.name
                            ),
                        ));
                    }
                }
            }
        }
    }

    // Realtime (contract v2). Channel names/entities are interpolated into
    // generated wiring, so everything is validated up front. Scope-filtered
    // delivery of changes is the security pillar, so changes require an active
    // auth model; tenant-scoped topics require tenancy.
    if let Some(ref rt) = d.realtime {
        let active_auth_model = d
            .auth
            .as_ref()
            .map(|a| a.model != AuthModel::None)
            .unwrap_or(false);
        if d.contract_version < 2 {
            qs.push(q(
                "/realtime",
                "The realtime block requires contract_version 2 — bump contract_version (v0/v1 designs stay valid without realtime).",
            ));
        }
        if !d.wants_db() {
            qs.push(q(
                "/realtime",
                "Realtime requires a database dependency — add `db` to `dependencies` (Changes stream from Postgres).",
            ));
        }
        // Changes entities must exist and require an active auth model (delivery
        // is scope-filtered by the authenticated principal).
        if !rt.changes.is_empty() && !active_auth_model {
            qs.push(q(
                "/realtime/changes",
                "Realtime changes delivery is scope-filtered by the authenticated principal — set auth.model to `session` or `jwt`.",
            ));
        }
        for (i, entity) in rt.changes.iter().enumerate() {
            if !entity_names.contains(entity.as_str()) {
                qs.push(q(
                    format!("/realtime/changes/{i}"),
                    format!("Realtime changes entity `{entity}` is not a declared entity anywhere in the design — define it or fix the reference."),
                ));
            }
        }
        // JC0547 (#102's realtime facet): a changes entity that reaches the
        // tenant only TRANSITIVELY (a grandchild — its `tenant_path` carries
        // joins) cannot be scoped by a single row-image column: the tenant key
        // lives on an ancestor table neither CDC adapter can read from the row
        // alone, so the channel would fall back to `tenant_column: None` —
        // which the runtime treats as world-visible, broadcasting every
        // tenant's rows to every authenticated principal. Refuse at design
        // time rather than ship the silent leak. The tenant entity itself and
        // direct children (zero joins) stay legal.
        if d.tenancy.is_some() {
            for (i, entity) in rt.changes.iter().enumerate() {
                if d.tenant_path(entity).is_some_and(|p| !p.joins.is_empty()) {
                    qs.push(q(
                        format!("/realtime/changes/{i}"),
                        format!("Realtime `changes` on `{entity}` is not supported: the entity is only TRANSITIVELY tenant-owned (its tenant key lives on an ancestor table), so change events cannot be tenant-scoped from the row image and every tenant's rows would broadcast to every authenticated principal. The changes entity must be the tenant itself or a DIRECT child — flatten the relationship (give `{entity}` its own `belongs_to` the tenant) or drop it from `changes`. See `jerrycan explain JC0547`."),
                    ));
                }
            }
        }
        // JC0555 (#112/#167): the changes broadcast delivers the RAW database
        // row — every column — to subscribers over the WebSocket, so a
        // `write_only`/`password_hash` column on a changes entity is exposed to
        // every subscriber even though `#[serde(skip_serializing)]` hides it
        // from REST responses. write_only's REST hide does not reach the
        // realtime stream, so refuse the combination by construction until
        // column projection (#167) lets the broadcast omit hidden columns.
        for (i, entity) in rt.changes.iter().enumerate() {
            if let Some(ent) = d.find_entity(entity)
                && let Some(secret) = ent.fields.iter().find(|f| Design::field_is_write_only(f))
            {
                qs.push(q(
                    format!("/realtime/changes/{i}"),
                    format!("Realtime `changes` on `{entity}` is not allowed: its `{col}` column is write_only (response-hidden, `#[serde(skip_serializing)]`), but the changes broadcast delivers the raw database row — every column — to subscribers over the WebSocket, so `{col}` would be exposed to every subscriber even though it is hidden from REST responses. Remove `{col}` from `{entity}`, or drop `{entity}` from realtime `changes` (don't broadcast row changes for it — REST reads still hide the column). Column projection (#167) will lift this once the broadcast can omit hidden columns. See `jerrycan explain JC0555`.", col = secret.name),
                ));
            }
        }
        // Broadcast + presence topics: snake_case, unique within their list,
        // tenant scope needs tenancy, and any non-none scope needs auth.
        let mut check_topics = |topics: &[RealtimeTopic], kind: &str| {
            let mut seen = std::collections::HashSet::new();
            for (i, t) in topics.iter().enumerate() {
                let tptr = format!("/realtime/{kind}/{i}");
                if !is_snake(&t.name) {
                    qs.push(q(
                        format!("{tptr}/name"),
                        format!(
                            "Realtime {kind} topic `{}` is not snake_case (^[a-z][a-z0-9_]*$).",
                            t.name
                        ),
                    ));
                }
                if !seen.insert(t.name.as_str()) {
                    qs.push(q(
                        format!("{tptr}/name"),
                        format!("Realtime {kind} topic name `{}` is already used — topic names must be unique.", t.name),
                    ));
                }
                if t.scope == RealtimeScope::Tenant && d.tenancy.is_none() {
                    qs.push(q(
                        tptr.clone(),
                        format!("Realtime {kind} topic `{}` is tenant-scoped but the design has no tenancy — declare `tenancy` or use scope `auth`/`none`.", t.name),
                    ));
                }
                if t.scope != RealtimeScope::None && !active_auth_model {
                    qs.push(q(
                        tptr,
                        format!("Realtime {kind} topic `{}` needs an active auth model for its scope — set auth.model to `session` or `jwt` (or use scope `none`).", t.name),
                    ));
                }
            }
        };
        check_topics(&rt.broadcast, "broadcast");
        check_topics(&rt.presence, "presence");
    }

    qs
}

fn validate_module(
    m: &ModuleDesign,
    ptr: &str,
    declared_roles: &[&str],
    auth_declared: bool,
    wants_db: bool,
    qs: &mut Vec<Question>,
) {
    if !is_kebab(&m.name) {
        qs.push(q(
            format!("{ptr}/name"),
            format!("Module `{}` is not kebab-case — rename it.", m.name),
        ));
    }
    if let Some(ref mount) = m.mount {
        if !mount.starts_with('/') {
            qs.push(q(
                format!("{ptr}/mount"),
                format!("Mount `{mount}` must start with '/'."),
            ));
        }
        if mount.contains("//") || (mount.len() > 1 && mount.ends_with('/')) {
            qs.push(q(
                format!("{ptr}/mount"),
                format!("Mount `{mount}` must not contain `//` or end with a trailing slash."),
            ));
        }
    }
    for (i, e) in m.entities.iter().enumerate() {
        if !is_pascal(&e.name) {
            qs.push(q(
                format!("{ptr}/entities/{i}/name"),
                format!("Entity `{}` must be PascalCase.", e.name),
            ));
        }
        if is_rust_keyword(&e.name) {
            qs.push(q(
                format!("{ptr}/entities/{i}/name"),
                format!(
                    "Entity `{}` is a Rust keyword — it becomes a module/type name that no raw identifier can escape; rename it (e.g. a domain-specific name).",
                    e.name
                ),
            ));
        }
        // JC0546 (#114): an entity named after a `jerrycan::prelude` re-export
        // (the eval hit `Module`) emits `pub struct {Name}` in `model.rs`, which
        // the tool-owned `repo.rs`/`handlers.rs` glob-import via `use super::model::*;`
        // BESIDE `use jerrycan::prelude::*;`. Two glob imports then bring the same
        // `{Name}` into scope, so every reference is `E0659 ... is ambiguous` and the
        // scaffolded crate does not compile. Generation is gated on validation, so
        // rejecting the name here fails loud (like JC0545) instead of scaffolding an
        // app that won't build.
        if RESERVED_PRELUDE_IDENTS.contains(&e.name.as_str()) {
            qs.push(q(
                format!("{ptr}/entities/{i}/name"),
                format!(
                    "Entity `{name}` collides with `{name}`, an identifier re-exported by `jerrycan::prelude`: generated code writes `use jerrycan::prelude::*;` beside `use super::model::*;`, so the entity's `struct {name}` and the prelude's `{name}` are two glob imports of the same name — every reference is `E0659 ... is ambiguous` and the scaffolded crate does not compile. Rename the entity (e.g. `{name}Record` or a domain-specific name) so it no longer shadows a reserved prelude identifier. See `jerrycan explain JC0546`.",
                    name = e.name
                ),
            ));
        }
        // An explicit `table` override is used VERBATIM in DDL/queries, so it must
        // be a safe snake_case identifier — reject anything else up front.
        if let Some(table) = &e.table
            && !is_snake(table)
        {
            qs.push(q(
                format!("{ptr}/entities/{i}/table"),
                format!("Table override `{table}` must be snake_case (^[a-z][a-z0-9_]*$)."),
            ));
        }
        if e.fields.is_empty() {
            qs.push(q(
                format!("{ptr}/entities/{i}/fields"),
                format!(
                    "Entity `{}` has no fields — what data does it carry?",
                    e.name
                ),
            ));
        }
        for (j, f) in e.fields.iter().enumerate() {
            if !is_snake(&f.name) {
                qs.push(q(
                    format!("{ptr}/entities/{i}/fields/{j}/name"),
                    format!("Field `{}` must be snake_case.", f.name),
                ));
            }
            // A keyword field name is fine: codegen emits it as a raw identifier
            // (`type` → `r#type`) with a `#[serde(rename)]` so the wire name is
            // unchanged — a frozen external contract keeps its `type`/`match`/
            // `ref` field. Only `crate`/`self`/`super`, which no `r#` can escape,
            // are still rejected.
            if !can_be_rust_ident(&f.name) {
                qs.push(q(
                    format!("{ptr}/entities/{i}/fields/{j}/name"),
                    format!(
                        "Field `{name}` is a Rust keyword that no raw identifier can escape — rename (e.g. `{name}_field` or a domain-specific name).",
                        name = f.name
                    ),
                ));
            }
        }
    }
    if m.endpoints.is_empty() {
        qs.push(q(
            format!("{ptr}/endpoints"),
            format!(
                "Module `{}` has no endpoints — what operations does it expose?",
                m.name
            ),
        ));
    }

    let entity_names: Vec<&str> = m.entities.iter().map(|e| e.name.as_str()).collect();
    let mut seen_ops = std::collections::HashSet::new();
    let mut seen_routes = std::collections::HashSet::new();
    for (i, ep) in m.endpoints.iter().enumerate() {
        let eptr = format!("{ptr}/endpoints/{i}");
        if !is_snake(&ep.operation_id) {
            qs.push(q(
                format!("{eptr}/operation_id"),
                format!(
                    "operation_id `{}` must be snake_case (it becomes the handler fn name).",
                    ep.operation_id
                ),
            ));
        }
        if !seen_ops.insert(ep.operation_id.as_str()) {
            qs.push(q(
                format!("{eptr}/operation_id"),
                format!(
                    "operation_id `{}` is not unique within module `{}` — handler names must be unique.",
                    ep.operation_id, m.name
                ),
            ));
        }
        if !ep.path.starts_with('/') {
            qs.push(q(
                format!("{eptr}/path"),
                format!("Path `{}` must start with '/'.", ep.path),
            ));
        }
        let param_count = ep.path.matches('{').count();
        if param_count > 3 {
            qs.push(q(format!("{eptr}/path"), format!("Path `{}` has {param_count} parameters — at most three path parameters per endpoint are supported. Split the route or use a subroute.", ep.path)));
        }
        if ep.path.matches('{').count() != ep.path.matches('}').count() {
            qs.push(q(
                format!("{eptr}/path"),
                format!("Path `{}` has unbalanced braces.", ep.path),
            ));
        }
        if !seen_routes.insert((ep.method, ep.path.as_str())) {
            qs.push(q(
                format!("{eptr}/path"),
                format!(
                    "{:?} {} is already registered in module `{}` — routes must be unique.",
                    ep.method, ep.path, m.name
                ),
            ));
        }
        // Success is 2xx, or 3xx for a redirect endpoint (e.g. an OAuth
        // `connect` that 302s the browser to the provider). 1xx/4xx/5xx are not
        // success classes.
        if !(200..=399).contains(&ep.success.status) {
            qs.push(q(
                format!("{eptr}/success/status"),
                format!("Success status {} is not 2xx/3xx.", ep.success.status),
            ));
        }
        if let Some(ref ent) = ep.success.entity
            && !entity_names.contains(&ent.as_str())
        {
            qs.push(q(
                format!("{eptr}/success/entity"),
                format!(
                    "Entity `{ent}` is not defined in module `{}` — define it or fix the reference.",
                    m.name
                ),
            ));
        }
        if let Some(ref rb) = ep.request_body {
            // JC0561 (#122): a `request_body` is EITHER an entity ref (today's
            // shape) XOR an inline DTO (`fields`). Refuse both-set / neither-set,
            // an inline body on an operation with no name (the DTO
            // `{Pascal(operation_id)}Request` would be unnameable), and validate the
            // inline fields with the SAME per-field checks as entity fields.
            let has_entity = rb.entity.is_some();
            let has_fields = !rb.fields.is_empty();
            if has_entity && has_fields {
                qs.push(q(
                    format!("{eptr}/request_body"),
                    format!(
                        "Endpoint `{}` request_body declares BOTH an `entity` and inline `fields` — exactly one is allowed: a `request_body` is either a table-entity reference OR an ad-hoc inline DTO, never both. Drop one. See `jerrycan explain JC0561`.",
                        ep.operation_id
                    ),
                ));
            } else if !has_entity && !has_fields {
                qs.push(q(
                    format!("{eptr}/request_body"),
                    format!(
                        "Endpoint `{}` request_body declares NEITHER an `entity` nor inline `fields` — a `request_body` must be exactly one: a table-entity reference (`{{\"entity\": \"Todo\"}}`) OR an inline DTO (`{{\"fields\": [...]}}`). See `jerrycan explain JC0561`.",
                        ep.operation_id
                    ),
                ));
            } else if let Some(ent) = rb.entity.as_deref() {
                if !entity_names.contains(&ent) {
                    qs.push(q(
                        format!("{eptr}/request_body/entity"),
                        format!(
                            "Entity `{ent}` is not defined in module `{}` — define it or fix the reference.",
                            m.name
                        ),
                    ));
                }
            } else {
                // Inline DTO body (has_fields, no entity). It needs an operation_id
                // to be nameable, and every field must pass the same name/charset
                // and #80 constraint checks as an entity field, plus no duplicates.
                if ep.operation_id.trim().is_empty() {
                    qs.push(q(
                        format!("{eptr}/request_body/fields"),
                        "An inline `request_body` (`fields`) requires an `operation_id` — the generated request struct is named `{Pascal(operation_id)}Request`, so a nameless operation has no DTO name. Give the endpoint an operation_id. See `jerrycan explain JC0561`.".to_string(),
                    ));
                }
                let mut seen_fields = std::collections::HashSet::new();
                for (j, f) in rb.fields.iter().enumerate() {
                    let fptr = format!("{eptr}/request_body/fields/{j}");
                    if !is_snake(&f.name) {
                        qs.push(q(
                            format!("{fptr}/name"),
                            format!("Field `{}` must be snake_case.", f.name),
                        ));
                    }
                    if !can_be_rust_ident(&f.name) {
                        qs.push(q(
                            format!("{fptr}/name"),
                            format!(
                                "Field `{name}` is a Rust keyword that no raw identifier can escape — rename (e.g. `{name}_field` or a domain-specific name).",
                                name = f.name
                            ),
                        ));
                    }
                    if !seen_fields.insert(f.name.as_str()) {
                        qs.push(q(
                            format!("{fptr}/name"),
                            format!(
                                "Inline field `{}` is declared twice in this request_body — the generated `{}Request` struct would carry a duplicate field. Give each field a unique name. See `jerrycan explain JC0561`.",
                                f.name,
                                to_pascal(&ep.operation_id)
                            ),
                        ));
                    }
                    // The SAME #47/#80/default per-field checks as an entity field.
                    check_field_shape(f, &fptr, wants_db, qs);
                }
            }
        }
        for (j, ec) in ep.errors.iter().enumerate() {
            if !(400..=599).contains(&ec.status) {
                qs.push(q(
                    format!("{eptr}/errors/{j}/status"),
                    format!("Error status {} is not 4xx/5xx.", ec.status),
                ));
            }
            if let Some(ref code) = ec.code {
                let ok = code.len() == 6
                    && code.starts_with("JC")
                    && code[2..].chars().all(|c| c.is_ascii_digit());
                if !ok {
                    qs.push(q(
                        format!("{eptr}/errors/{j}/code"),
                        format!("`{code}` does not match ^JC[0-9]{{4}}$."),
                    ));
                }
            }
        }
        for role in &ep.required_roles {
            if !declared_roles.contains(&role.as_str()) {
                let hint = if auth_declared {
                    "add it to auth.roles or fix the reference"
                } else {
                    "declare auth { model, roles } first"
                };
                qs.push(q(
                    format!("{eptr}/required_roles"),
                    format!("Role `{role}` is not declared in auth.roles — {hint}."),
                ));
            }
        }
        // `public` marks a genuinely unauthenticated route (login/register); it
        // contradicts any guard. Flag the combination so a design can't claim both.
        if ep.public && ep.auth_required {
            qs.push(q(
                eptr.clone(),
                format!(
                    "Endpoint `{}` is marked public but also auth_required — a public route is unauthenticated by design; drop one.",
                    ep.operation_id
                ),
            ));
        }
        if ep.public && !ep.required_roles.is_empty() {
            qs.push(q(
                eptr.clone(),
                format!(
                    "Endpoint `{}` is marked public but declares required_roles — a public route is unauthenticated by design; drop the roles or the public flag.",
                    ep.operation_id
                ),
            ));
        }
    }

    for (i, sub) in m.subroutes.iter().enumerate() {
        validate_module(
            sub,
            &format!("{ptr}/subroutes/{i}"),
            declared_roles,
            auth_declared,
            wants_db,
            qs,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::design::tests::{MINIMAL, V1_FULL, V2_REALTIME, V2_STORAGE};

    fn design(json: &str) -> Design {
        serde_json::from_str(json).unwrap()
    }

    /// Authenticate every non-public endpoint of a design so no tenant /
    /// tenant-owned read is anonymous. The shared tenancy fixtures (V1_FULL /
    /// V2_STORAGE / V2_REALTIME) predate JC0558 (#148) and carry unguarded reads
    /// on their tenant/tenant-owned entities; tests that only care that the
    /// fixture is otherwise question-free (realtime, storage, tenancy checks)
    /// guard the reads first so the JC0558 refusal doesn't mask the assertion.
    fn guard_reads(mut d: Design) -> Design {
        fn walk(m: &mut ModuleDesign) {
            for ep in &mut m.endpoints {
                if !ep.public {
                    ep.auth_required = true;
                }
            }
            for sub in &mut m.subroutes {
                walk(sub);
            }
        }
        for m in &mut d.modules {
            walk(m);
        }
        d
    }

    /// JC0561 (#122): a well-formed inline-DTO `request_body` validates clean, and
    /// the four malformed shapes (both entity+fields, neither, an inline field with
    /// a bad #80 constraint, a duplicate inline field name) are refused with a
    /// JC0561/JC0552 message.
    #[test]
    fn inline_request_body_is_validated_with_jc0561() {
        let base = |body: &str| {
            format!(
                r#"{{
                "name": "shop-api", "contract_version": 0, "dependencies": [],
                "modules": [{{
                    "name": "checkout",
                    "endpoints": [
                        {{ "operation_id": "checkout", "method": "POST", "path": "/",
                          "request_body": {body},
                          "success": {{ "status": 200 }} }}
                    ]
                }}]
            }}"#
            )
        };

        // A clean inline body → no questions.
        let ok = design(&base(
            r#"{ "fields": [ { "name": "coupon", "type": "string" }, { "name": "total", "type": "integer" } ] }"#,
        ));
        assert!(
            validate(&ok).is_empty(),
            "clean inline body: {:?}",
            validate(&ok)
        );

        // BOTH entity and fields → JC0561.
        let both = design(&base(
            r#"{ "entity": "Order", "fields": [ { "name": "coupon", "type": "string" } ] }"#,
        ));
        assert!(
            validate(&both)
                .iter()
                .any(|q| q.question.contains("JC0561") && q.question.contains("BOTH")),
            "both entity+fields must trip JC0561: {:?}",
            validate(&both)
        );

        // NEITHER → JC0561.
        let neither = design(&base(r#"{ }"#));
        assert!(
            validate(&neither)
                .iter()
                .any(|q| q.question.contains("JC0561") && q.question.contains("NEITHER")),
            "empty request_body must trip JC0561: {:?}",
            validate(&neither)
        );

        // An inline field with a misplaced #80 constraint reuses JC0552.
        let bad_constraint = design(&base(
            r#"{ "fields": [ { "name": "coupon", "type": "string", "min": 1 } ] }"#,
        ));
        assert!(
            validate(&bad_constraint)
                .iter()
                .any(|q| q.question.contains("JC0552")),
            "inline field constraint must be validated (JC0552): {:?}",
            validate(&bad_constraint)
        );

        // A duplicate inline field name → JC0561.
        let dup = design(&base(
            r#"{ "fields": [ { "name": "coupon", "type": "string" }, { "name": "coupon", "type": "string" } ] }"#,
        ));
        assert!(
            validate(&dup)
                .iter()
                .any(|q| q.question.contains("JC0561") && q.question.contains("twice")),
            "duplicate inline field must trip JC0561: {:?}",
            validate(&dup)
        );
    }

    #[test]
    fn valid_realtime_design_is_question_free() {
        let d = guard_reads(serde_json::from_str(V2_REALTIME).unwrap());
        assert!(validate(&d).is_empty(), "{:?}", validate(&d));
    }

    #[test]
    fn realtime_requires_contract_v2() {
        let mut d: Design = serde_json::from_str(V2_REALTIME).unwrap();
        d.contract_version = 1;
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.id == "/realtime" && q.question.contains("contract_version"))
        );
    }

    #[test]
    fn realtime_changes_entities_must_exist() {
        let mut d: Design = serde_json::from_str(V2_REALTIME).unwrap();
        d.realtime.as_mut().unwrap().changes[0] = "Ghost".into();
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.id == "/realtime/changes/0" && q.question.contains("Ghost"))
        );
    }

    #[test]
    fn realtime_requires_db_and_changes_require_active_auth() {
        let mut d: Design = serde_json::from_str(V2_REALTIME).unwrap();
        d.dependencies.retain(|x| x != "db");
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.id == "/realtime" && q.question.contains("db"))
        );

        let mut d2: Design = serde_json::from_str(V2_REALTIME).unwrap();
        d2.auth = None;
        assert!(
            validate(&d2)
                .iter()
                .any(|q| q.id == "/realtime/changes" && q.question.contains("auth"))
        );
    }

    /// JC0547 (#102's realtime facet): a `changes` entity that reaches the
    /// tenant only transitively (a grandchild) has no tenant key in its row
    /// image, so its channel would get `tenant_column: None` — which the
    /// runtime treats as world-visible, silently broadcasting every tenant's
    /// rows to every authenticated principal. The design must be REFUSED up
    /// front; the tenant entity itself and direct children stay legal.
    #[test]
    fn transitive_changes_entity_is_refused_with_jc0547() {
        let mut d: Design = serde_json::from_str(V2_REALTIME).unwrap();
        // Contact → Lead → Workspace: a grandchild of the tenant.
        d.modules[1].entities.push(
            serde_json::from_value(serde_json::json!({
                "name": "Contact",
                "belongs_to": [{ "entity": "Lead" }],
                "fields": [{ "name": "email", "type": "string" }]
            }))
            .unwrap(),
        );
        d.realtime.as_mut().unwrap().changes.push("Contact".into());
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.id == "/realtime/changes/1" && q.question.contains("JC0547")),
            "{:?}",
            validate(&d)
        );

        // A DIRECT child (Lead) and the tenant entity itself (Workspace) are
        // both scopable from the row image — no JC0547, no other question.
        let mut ok = guard_reads(serde_json::from_str(V2_REALTIME).unwrap());
        ok.realtime
            .as_mut()
            .unwrap()
            .changes
            .push("Workspace".into());
        assert!(validate(&ok).is_empty(), "{:?}", validate(&ok));
    }

    /// JC0555 (#112/#167): a `write_only`/`password_hash` column on a realtime
    /// `changes` entity leaks over the WebSocket — the changes broadcast ships
    /// the RAW row (every column), bypassing the REST-side skip_serializing that
    /// hides the column from responses. Refuse the combination; REST-hiding
    /// alone (no `changes`) stays clean, and a changes entity with no secret
    /// column stays clean. Closes the egress hole T1's `skip_serializing` leaves
    /// open until column projection (#167) lands.
    #[test]
    fn write_only_column_on_a_changes_entity_is_refused_with_jc0555() {
        let password_hash = serde_json::json!({ "name": "password_hash", "type": "string" });

        // Lead is broadcast in `changes` (V2_REALTIME) and now carries a
        // password_hash (auto-hidden by name) → the raw-row broadcast would leak
        // it → JC0555, naming the offending column.
        let mut leak: Design = serde_json::from_str(V2_REALTIME).unwrap();
        leak.modules[1].entities[0]
            .fields
            .push(serde_json::from_value(password_hash.clone()).unwrap());
        assert!(
            validate(&leak).iter().any(|q| q.id == "/realtime/changes/0"
                && q.question.contains("JC0555")
                && q.question.contains("password_hash")),
            "a password_hash column on a changes entity must trip JC0555: {:?}",
            validate(&leak)
        );

        // Same secret column, but the entity is NOT broadcast — the REST-side
        // skip_serializing already hides it, so there is no realtime leak and no
        // JC0555 (REST-hidden is enough).
        let mut rest_only: Design = serde_json::from_str(V2_REALTIME).unwrap();
        rest_only.modules[1].entities[0]
            .fields
            .push(serde_json::from_value(password_hash).unwrap());
        rest_only.realtime.as_mut().unwrap().changes.clear();
        assert!(
            !validate(&rest_only)
                .iter()
                .any(|q| q.question.contains("JC0555")),
            "a write_only column with no realtime changes must not trip JC0555: {:?}",
            validate(&rest_only)
        );

        // A changes entity with no secret column (the base fixture) stays clean.
        let clean: Design = serde_json::from_str(V2_REALTIME).unwrap();
        assert!(
            !validate(&clean)
                .iter()
                .any(|q| q.question.contains("JC0555")),
            "a changes entity with no write_only column must not trip JC0555: {:?}",
            validate(&clean)
        );

        // The EXPLICIT `write_only: true` flag (not just the password_hash name)
        // trips it too — the classifier covers both.
        let mut explicit: Design = serde_json::from_str(V2_REALTIME).unwrap();
        explicit.modules[1].entities[0].fields.push(
            serde_json::from_value(serde_json::json!({
                "name": "api_token", "type": "string", "write_only": true
            }))
            .unwrap(),
        );
        assert!(
            validate(&explicit)
                .iter()
                .any(|q| q.id == "/realtime/changes/0"
                    && q.question.contains("JC0555")
                    && q.question.contains("api_token")),
            "an explicit write_only column on a changes entity must trip JC0555: {:?}",
            validate(&explicit)
        );
    }

    #[test]
    fn tenant_scoped_topics_require_tenancy_and_snake_case_unique_names() {
        let mut d: Design = serde_json::from_str(V2_REALTIME).unwrap();
        d.tenancy = None;
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.id == "/realtime/broadcast/0" && q.question.contains("tenancy"))
        );

        let mut d2: Design = serde_json::from_str(V2_REALTIME).unwrap();
        d2.realtime.as_mut().unwrap().broadcast.push(RealtimeTopic {
            name: "Deal-Room".into(),
            scope: RealtimeScope::None,
        });
        assert!(
            validate(&d2)
                .iter()
                .any(|q| q.id == "/realtime/broadcast/1/name" && q.question.contains("snake_case"))
        );

        let mut d3: Design = serde_json::from_str(V2_REALTIME).unwrap();
        let dup = d3.realtime.as_ref().unwrap().broadcast[0].clone();
        d3.realtime.as_mut().unwrap().broadcast.push(dup);
        assert!(
            validate(&d3)
                .iter()
                .any(|q| q.id == "/realtime/broadcast/1/name" && q.question.contains("unique"))
        );
    }

    #[test]
    fn contract_version_2_is_now_valid_and_3_is_not() {
        let ok: Design = serde_json::from_str(V2_STORAGE).unwrap();
        assert!(
            !validate(&ok).iter().any(|q| q.id == "/contract_version"),
            "{:?}",
            validate(&ok)
        );
        let mut bad: Design = serde_json::from_str(V2_STORAGE).unwrap();
        bad.contract_version = 3;
        assert!(validate(&bad).iter().any(|q| q.id == "/contract_version"));
    }

    #[test]
    fn v2_storage_fixture_is_question_free() {
        let d = guard_reads(serde_json::from_str(V2_STORAGE).unwrap());
        assert!(validate(&d).is_empty(), "{:?}", validate(&d));
    }

    #[test]
    fn storage_requires_contract_v2_db_and_an_active_auth_model() {
        // v1 + storage: rejected (v2 owns the block).
        let mut d: Design = serde_json::from_str(V2_STORAGE).unwrap();
        d.contract_version = 1;
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.id == "/storage" && q.question.contains("contract_version 2"))
        );
        // storage without db: rejected (metadata table).
        let mut d: Design = serde_json::from_str(V2_STORAGE).unwrap();
        d.dependencies.retain(|dep| dep != "db");
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.id == "/storage" && q.question.contains("db"))
        );
        // storage without an active auth model: rejected (mutations are always guarded).
        let mut d: Design = serde_json::from_str(V2_STORAGE).unwrap();
        d.auth = None;
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.id == "/storage" && q.question.contains("auth"))
        );
    }

    #[test]
    fn bucket_names_owners_and_rules_are_validated() {
        // Bad kebab name.
        let mut d: Design = serde_json::from_str(V2_STORAGE).unwrap();
        d.storage.as_mut().unwrap().buckets[0].name = "Avatars".into();
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.id == "/storage/buckets/0/name")
        );
        // A name whose snake ident is a Rust keyword breaks the generated crate.
        let mut d: Design = serde_json::from_str(V2_STORAGE).unwrap();
        d.storage.as_mut().unwrap().buckets[0].name = "match".into();
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.id == "/storage/buckets/0/name" && q.question.contains("keyword"))
        );
        // Duplicate bucket names.
        let mut d: Design = serde_json::from_str(V2_STORAGE).unwrap();
        let dup = d.storage.as_ref().unwrap().buckets[0].clone();
        d.storage.as_mut().unwrap().buckets.push(dup);
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.id == "/storage/buckets/2/name" && q.question.contains("unique"))
        );
        // Unknown owner entity.
        let mut d: Design = serde_json::from_str(V2_STORAGE).unwrap();
        d.storage.as_mut().unwrap().buckets[0].owner = Some("Ghost".into());
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.id == "/storage/buckets/0/owner" && q.question.contains("Ghost"))
        );
        // owner_prefix without owner.
        let mut d: Design = serde_json::from_str(V2_STORAGE).unwrap();
        d.storage.as_mut().unwrap().buckets[1].owner = None;
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.id == "/storage/buckets/1/owner_prefix")
        );
        // Unparseable max_size.
        let mut d: Design = serde_json::from_str(V2_STORAGE).unwrap();
        d.storage.as_mut().unwrap().buckets[0].max_size = Some("lots".into());
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.id == "/storage/buckets/0/max_size")
        );
        // A mime entry that could break generated string literals.
        let mut d: Design = serde_json::from_str(V2_STORAGE).unwrap();
        d.storage.as_mut().unwrap().buckets[0].allowed_mime = vec!["image/\"png".into()];
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.id == "/storage/buckets/0/allowed_mime/0")
        );
        // A wildcard TYPE with a concrete subtype (`*/png`) is dead: the
        // runtime matcher only understands `type/subtype`, `type/*` and `*/*`,
        // so `*/png` would silently 415 every upload — reject at design time.
        let mut d: Design = serde_json::from_str(V2_STORAGE).unwrap();
        d.storage.as_mut().unwrap().buckets[0].allowed_mime = vec!["*/png".into()];
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.id == "/storage/buckets/0/allowed_mime/0"),
            "*/png must be rejected — it can never match"
        );
        // The supported wildcard shapes stay valid.
        let mut d: Design = serde_json::from_str(V2_STORAGE).unwrap();
        d.storage.as_mut().unwrap().buckets[0].allowed_mime =
            vec!["*/*".into(), "image/*".into(), "application/pdf".into()];
        assert!(
            !validate(&d)
                .iter()
                .any(|q| q.id.starts_with("/storage/buckets/0/allowed_mime")),
            "*/*, type/* and type/subtype are all valid"
        );
    }

    /// JC0556 (#132): `write_roles` gates blob upload/delete by tenant role. It
    /// must name declared member_roles AND sit on a tenant-scoped bucket — a
    /// write gate that emits nothing (non-tenant / no-tenancy) would silently
    /// leave writes open, so it is refused loud rather than ignored.
    #[test]
    fn write_roles_are_validated_with_jc0556() {
        // An entry that is not a declared member_role, on the tenant-scoped
        // invoices bucket (owner Org == tenancy.entity).
        let mut d: Design = serde_json::from_str(V2_STORAGE).unwrap();
        d.storage.as_mut().unwrap().buckets[1].write_roles = vec!["ghost".into()];
        assert!(
            validate(&d).iter().any(
                |q| q.id == "/storage/buckets/1/write_roles/0" && q.question.contains("JC0556")
            ),
            "undeclared write role → JC0556: {:?}",
            validate(&d)
        );
        // A declared member_role on the tenant bucket is accepted.
        let mut ok: Design = serde_json::from_str(V2_STORAGE).unwrap();
        ok.storage.as_mut().unwrap().buckets[1].write_roles = vec!["owner".into()];
        assert!(
            !validate(&ok)
                .iter()
                .any(|q| q.id.starts_with("/storage/buckets/1/write_roles")),
            "a declared member_role on a tenant bucket is valid: {:?}",
            validate(&ok)
        );
        // write_roles on a NON-tenant bucket (avatars, owner User) is refused —
        // the gate would emit nothing there.
        let mut nt: Design = serde_json::from_str(V2_STORAGE).unwrap();
        nt.storage.as_mut().unwrap().buckets[0].write_roles = vec!["owner".into()];
        assert!(
            validate(&nt)
                .iter()
                .any(|q| q.id == "/storage/buckets/0/write_roles" && q.question.contains("JC0556")),
            "write_roles on a non-tenant bucket → JC0556: {:?}",
            validate(&nt)
        );
        // write_roles with no tenancy at all is refused the same way.
        let mut no_ten: Design = serde_json::from_str(V2_STORAGE).unwrap();
        no_ten.tenancy = None;
        no_ten.storage.as_mut().unwrap().buckets[1].write_roles = vec!["owner".into()];
        assert!(
            validate(&no_ten)
                .iter()
                .any(|q| q.id == "/storage/buckets/1/write_roles" && q.question.contains("JC0556")),
            "write_roles without tenancy → JC0556: {:?}",
            validate(&no_ten)
        );
    }

    /// JC0545, storage facet (0.5.4): a bucket owner that reaches the tenant
    /// through TWO `belongs_to` chains (a diamond) makes `tenant_path` resolve
    /// `None`, which would SILENTLY degrade the bucket from tenant scope to
    /// plain per-user scope — no Tenant guard, no tenant_id stamp, a
    /// cross-tenant leak with `check` green. The bucket itself must be
    /// refused; a single (even transitive) chain stays legal.
    #[test]
    fn diamond_bucket_owner_is_refused_with_jc0545() {
        let diamond = |d: &mut Design| {
            // User (avatars' owner) → {Team, Project} → Org: two chains.
            for parent in [
                r#"{ "name": "Team", "belongs_to": [{ "entity": "Org" }],
                     "fields": [{ "name": "id", "type": "integer" }] }"#,
                r#"{ "name": "Project", "belongs_to": [{ "entity": "Org" }],
                     "fields": [{ "name": "id", "type": "integer" }] }"#,
            ] {
                d.modules[0]
                    .entities
                    .push(serde_json::from_str(parent).unwrap());
            }
            d.modules[0].entities[1].belongs_to = vec![
                serde_json::from_str(r#"{ "entity": "Team" }"#).unwrap(),
                serde_json::from_str(r#"{ "entity": "Project" }"#).unwrap(),
            ];
        };
        let mut d: Design = serde_json::from_str(V2_STORAGE).unwrap();
        diamond(&mut d);
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.id == "/storage/buckets/0/owner" && q.question.contains("JC0545")),
            "diamond bucket owner → JC0545 at the bucket pointer: {:?}",
            validate(&d)
        );
        // A single transitive chain (drop the Project leg) is NOT ambiguous —
        // the bucket becomes UserInTenant, no refusal.
        let mut single: Design = serde_json::from_str(V2_STORAGE).unwrap();
        diamond(&mut single);
        single.modules[0].entities[1].belongs_to =
            vec![serde_json::from_str(r#"{ "entity": "Team" }"#).unwrap()];
        assert!(
            !validate(&single)
                .iter()
                .any(|q| q.id.starts_with("/storage/buckets/0")),
            "a unique transitive chain is legal: {:?}",
            validate(&single)
        );
    }

    #[test]
    fn bucket_mounts_must_not_collide_with_module_mounts() {
        // WHY: buckets mount at {base_path}/<name> beside the modules — a
        // collision would shadow routes silently at serve time (issue #8). Under
        // the default /storage prefix, a bucket named `avatars` no longer
        // collides with a module at `/orgs`; the collision needs a module mounted
        // at the bucket's actual path (`/storage/avatars`).
        let base = guard_reads(serde_json::from_str(V2_STORAGE).unwrap());
        assert!(
            validate(&base).is_empty(),
            "default /storage prefix keeps buckets clear of the /orgs module: {:?}",
            validate(&base)
        );
        // A module remounted onto the bucket's storage path collides.
        let mut d = base.clone();
        d.modules[0].mount = Some("/storage/avatars".into());
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.id == "/storage/buckets/0/name" && q.question.contains("collides")),
            "a module at /storage/avatars collides with the avatars bucket: {:?}",
            validate(&d)
        );
        // A custom base_path recomputes the collision against the new prefix.
        let mut d2: Design = serde_json::from_str(V2_STORAGE).unwrap();
        d2.storage.as_mut().unwrap().base_path = Some("/files".into());
        d2.modules[0].mount = Some("/files/avatars".into());
        assert!(
            validate(&d2)
                .iter()
                .any(|q| q.id == "/storage/buckets/0/name" && q.question.contains("collides")),
            "collision follows the custom base_path: {:?}",
            validate(&d2)
        );
    }

    #[test]
    fn complete_design_yields_no_questions() {
        assert!(validate(&design(MINIMAL)).is_empty());
    }

    /// Inject a `cors` block into MINIMAL (which is otherwise question-free).
    fn with_cors(cors_json: &str) -> Design {
        design(&MINIMAL.replace(
            "\"contract_version\": 0,",
            &format!("\"contract_version\": 0, \"cors\": {cors_json},"),
        ))
    }

    /// A well-formed cors block yields NO questions — a valid cross-origin SPA
    /// policy (issue #21) must validate clean.
    #[test]
    fn well_formed_cors_block_is_question_free() {
        let d = with_cors(
            r#"{ "origins": ["https://app.example", "http://localhost:3000"],
                 "methods": ["GET", "POST"], "headers": ["content-type"],
                 "allow_credentials": true }"#,
        );
        assert!(validate(&d).is_empty(), "{:?}", validate(&d));
        // `*` alone (no credentials) is also valid.
        let any = with_cors(r#"{ "origins": ["*"] }"#);
        assert!(validate(&any).is_empty(), "{:?}", validate(&any));
    }

    /// The CORS footguns become pointed questions, not runtime boot failures:
    /// empty origins, `*` mixed with an allowlist, `*` + credentials (Fetch-spec
    /// forbidden — core's App::build rejects it), and a non-bare origin.
    #[test]
    fn cors_misconfig_yields_pointed_questions() {
        // Empty origins.
        let d = with_cors(r#"{ "origins": [] }"#);
        assert!(
            validate(&d).iter().any(|q| q.id == "/cors/origins"),
            "empty origins must be a question: {:?}",
            validate(&d)
        );
        // `*` mixed with an explicit origin.
        let d = with_cors(r#"{ "origins": ["*", "https://app.example"] }"#);
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.id == "/cors/origins" && q.question.contains("mixes")),
            "mixing `*` with explicit origins must be a question: {:?}",
            validate(&d)
        );
        // `*` + credentials — the Fetch-spec violation core rejects at build time.
        let d = with_cors(r#"{ "origins": ["*"], "allow_credentials": true }"#);
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.id == "/cors/allow_credentials"),
            "`*` + credentials must be caught at design time: {:?}",
            validate(&d)
        );
        // A non-bare origin (has a path / trailing slash / no scheme).
        for bad in [
            "https://app.example/",
            "app.example",
            "https://app.example/app",
        ] {
            let d = with_cors(&format!(r#"{{ "origins": ["{bad}"] }}"#));
            assert!(
                validate(&d).iter().any(|q| q.id == "/cors/origins/0"),
                "malformed origin `{bad}` must be a question: {:?}",
                validate(&d)
            );
        }
    }

    #[test]
    fn bad_names_yield_pointed_questions_with_json_pointer_ids() {
        let d = design(&MINIMAL.replace("\"name\": \"demo-api\"", "\"name\": \"Demo API\""));
        let qs = validate(&d);
        assert!(
            qs.iter()
                .any(|q| q.id == "/name" && q.question.contains("kebab-case")),
            "{qs:?}"
        );
    }

    #[test]
    fn duplicate_operation_ids_and_routes_are_caught() {
        let d = design(&MINIMAL.replace(
            "\"operation_id\": \"create_todo\"",
            "\"operation_id\": \"list_todos\"",
        ));
        let qs = validate(&d);
        assert!(
            qs.iter()
                .any(|q| q.id.starts_with("/modules/0/endpoints") && q.question.contains("unique"))
        );

        let d2 = design(&MINIMAL.replace(
            "{ \"operation_id\": \"create_todo\", \"method\": \"POST\", \"path\": \"/\",",
            "{ \"operation_id\": \"create_todo\", \"method\": \"GET\", \"path\": \"/\",",
        ));
        let qs2 = validate(&d2);
        assert!(
            qs2.iter()
                .any(|q| q.question.contains("GET /") && q.question.contains("already")),
            "{qs2:?}"
        );
    }

    #[test]
    fn roles_must_be_declared_and_entities_must_exist() {
        let d = design(&MINIMAL.replace(
            "\"required_roles\": [\"admin\"]",
            "\"required_roles\": [\"superuser\"]",
        ));
        let qs = validate(&d);
        assert!(
            qs.iter()
                .any(|q| q.question.contains("superuser") && q.question.contains("auth.roles"))
        );

        let d2 = design(&MINIMAL.replace(
            "\"request_body\": { \"entity\": \"Todo\" }",
            "\"request_body\": { \"entity\": \"Ghost\" }",
        ));
        let qs2 = validate(&d2);
        assert!(qs2.iter().any(|q| q.question.contains("Ghost")));
    }

    #[test]
    fn status_ranges_and_path_shape_are_enforced() {
        // 3xx is a valid success class (redirect endpoints, e.g. OAuth connect).
        let ok3xx = design(&MINIMAL.replace("\"status\": 204", "\"status\": 302"));
        assert!(
            !validate(&ok3xx)
                .iter()
                .any(|q| q.question.contains("success")),
            "302 is a valid (redirect) success status"
        );
        // A 5xx success status is not a success class and must be rejected.
        let d = design(&MINIMAL.replace("\"status\": 204", "\"status\": 500"));
        assert!(validate(&d).iter().any(|q| q.question.contains("2xx/3xx")));
        let d2 = design(&MINIMAL.replace("\"path\": \"/{id}\"", "\"path\": \"{id}\""));
        assert!(
            validate(&d2)
                .iter()
                .any(|q| q.question.contains("start with '/'"))
        );
    }

    #[test]
    fn paths_allow_up_to_three_params_and_validate_mount_prefix() {
        // Two params: now legal (multi-param Path landed in core).
        let d = design(&MINIMAL.replace("\"path\": \"/{id}\"", "\"path\": \"/{id}/tags/{tag}\""));
        assert!(
            !validate(&d)
                .iter()
                .any(|q| q.question.contains("path parameter")),
            "two params must be accepted now"
        );
        // Four params: rejected.
        let d4 = design(&MINIMAL.replace("\"path\": \"/{id}\"", "\"path\": \"/{a}/{b}/{c}/{d}\""));
        assert!(
            validate(&d4)
                .iter()
                .any(|q| q.question.contains("three path parameters"))
        );

        let d2 = design(&MINIMAL.replace(
            "\"name\": \"comments\",",
            "\"name\": \"comments\", \"mount\": \"comments\",",
        ));
        assert!(
            validate(&d2)
                .iter()
                .any(|q| q.id.contains("/mount") && q.question.contains("start with '/'"))
        );

        // A path parameter in a mount prefix is now fully supported: a handler's
        // single Path<T> binds the leaf-most param, tuples address all root→leaf.
        // The validator must NOT discourage it (only the syntax rules apply).
        let d3 = design(&MINIMAL.replace(
            "\"name\": \"comments\",",
            "\"name\": \"comments\", \"mount\": \"/{comment_id}\",",
        ));
        assert!(
            !validate(&d3).iter().any(|q| q.id.contains("/mount")),
            "a param-carrying mount prefix must raise no mount question now: {:?}",
            validate(&d3)
        );
    }

    #[test]
    fn nested_subroute_violations_carry_full_json_pointers() {
        let d = design(&MINIMAL.replace(
            "\"operation_id\": \"list_comments\"",
            "\"operation_id\": \"List-Comments\"",
        ));
        let qs = validate(&d);
        assert!(
            qs.iter()
                .any(|q| q.id == "/modules/0/subroutes/0/endpoints/0/operation_id"),
            "{qs:?}"
        );
    }

    #[test]
    fn unbalanced_path_braces_yield_a_question() {
        let d = design(&MINIMAL.replace("\"path\": \"/{id}\"", "\"path\": \"/{id\""));
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.question.contains("unbalanced braces")),
            "unbalanced braces must be flagged"
        );
    }

    #[test]
    fn json_fields_are_rejected_in_db_mode() {
        // MINIMAL already declares `["db"]`; flip a field to json.
        let d = design(&MINIMAL.replace("\"type\": \"boolean\"", "\"type\": \"json\""));
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.question.contains("json") && q.question.contains("db mode")),
            "db mode can't store json fields yet"
        );
    }

    #[test]
    fn raw_escapable_keyword_field_names_are_accepted() {
        // WHY: `type`/`match`/`ref` are common field names in frozen external
        // wire contracts. Codegen raw-escapes them (`r#type`) with a serde
        // rename, so forcing a rename would push a permanent wire↔storage
        // mapping into every handler. Validation must NOT flag them.
        for kw in ["type", "match", "ref"] {
            let d = design(&MINIMAL.replace("\"name\": \"title\"", &format!("\"name\": \"{kw}\"")));
            assert!(
                !validate(&d)
                    .iter()
                    .any(|q| q.id.contains("/fields/") && q.question.contains("keyword")),
                "keyword field `{kw}` is raw-escapable and must be accepted"
            );
        }
    }

    #[test]
    fn unescapable_keyword_field_names_are_still_rejected() {
        // `self`/`crate`/`super` are keywords no raw identifier can escape
        // (`r#self` is invalid Rust), so a field named one still can't compile.
        for kw in ["self", "crate", "super"] {
            let d = design(&MINIMAL.replace("\"name\": \"title\"", &format!("\"name\": \"{kw}\"")));
            assert!(
                validate(&d)
                    .iter()
                    .any(|q| q.id.contains("/fields/") && q.question.contains("keyword")),
                "unescapable keyword field `{kw}` must be flagged"
            );
        }
    }

    #[test]
    fn required_roles_need_a_role_in_auth_roles_and_auth_model() {
        let mut v: serde_json::Value = serde_json::from_str(MINIMAL).unwrap();
        v["auth"] = serde_json::json!({ "model": "none" });
        v["modules"][0]["endpoints"][2]["required_roles"] = serde_json::json!(["admin"]);
        let d: Design = serde_json::from_value(v).unwrap();
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.question.contains("auth.model") || q.question.contains("auth.roles"))
        );
    }

    #[test]
    fn mount_rejects_trailing_slash_and_double_slash() {
        let d = design(&MINIMAL.replace(
            "\"name\": \"comments\",",
            "\"name\": \"comments\", \"mount\": \"/x/\",",
        ));
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.id.contains("/mount") && q.question.contains("trailing slash")),
            "trailing-slash mount must be flagged"
        );
    }

    #[test]
    fn belongs_to_must_target_a_declared_entity() {
        let mut d: Design = serde_json::from_str(V1_FULL).unwrap();
        d.modules[1].entities[0].belongs_to[0].entity = "Ghost".into();
        let qs = validate(&d);
        assert!(
            qs.iter()
                .any(|q| q.id == "/modules/1/entities/0/belongs_to/0"
                    && q.question.contains("Ghost")),
            "{qs:?}"
        );
    }

    #[test]
    fn tenancy_entity_must_exist() {
        let mut d: Design = serde_json::from_str(V1_FULL).unwrap();
        d.tenancy.as_mut().unwrap().entity = "Nope".into();
        assert!(validate(&d).iter().any(|q| q.id == "/tenancy/entity"));
    }

    #[test]
    fn tenancy_requires_active_auth() {
        let mut d: Design = serde_json::from_str(V1_FULL).unwrap();
        d.auth = None;
        assert!(validate(&d).iter().any(|q| q.id == "/tenancy"));
    }

    /// #27: a design whose `tenancy.entity` IS the auth identity entity is
    /// otherwise complete (no completeness question), yet cannot scaffold — the
    /// generated `{tenant}_members` table would derive the same fixed `user_id`
    /// column twice. `design_conflict` rejects it up front with JC0540, so the
    /// CLI fails loud before writing a byte instead of dying mid-migration with a
    /// raw SQLite `duplicate column name: user_id`.
    #[test]
    fn tenancy_entity_as_auth_identity_is_a_design_conflict() {
        let fixture = include_str!("../../tests/fixtures/tenant-is-identity.design.json");
        let d: Design = serde_json::from_str(fixture).unwrap();
        // Completeness is clean — the conflict is what the new rule catches.
        assert!(
            validate(&d).is_empty(),
            "fixture must be otherwise complete: {:?}",
            validate(&d)
        );
        let conflict = design_conflict(&d).expect("tenant==identity must be a conflict");
        assert_eq!(conflict.code, "JC0540");
        // Names both fixes: per-user → belongs_to; orgs/teams → a separate entity.
        assert!(
            conflict.message.contains("belongs_to") && conflict.message.contains("tenant entity"),
            "{}",
            conflict.message
        );
        assert!(!conflict.hint.is_empty());
    }

    /// The comparison is derived from the fixed membership `user_id` column, so a
    /// tenancy over a SEPARATE tenant entity (the reference shape) is never
    /// flagged — only the entity whose fk column collides with the identity is.
    #[test]
    fn separate_tenant_entity_is_not_a_conflict() {
        let d: Design = serde_json::from_str(V1_FULL).unwrap();
        assert!(
            design_conflict(&d).is_none(),
            "Workspace tenancy must not be flagged"
        );
        // No tenancy at all: nothing to conflict.
        let plain: Design = serde_json::from_str(MINIMAL).unwrap();
        assert!(design_conflict(&plain).is_none());
    }

    /// JC0553 (#141): with tenancy, jerrycan reserves `{tenant}_members` (the
    /// membership table) and `pub struct {Tenant}Member` (the member row type,
    /// issue #107). An entity that resolves to that table, or is named
    /// `{Tenant}Member`, collides with the generated member surface — today a
    /// clean `check` then a raw `table "..._members" already exists`
    /// mid-scaffold. `validate` must refuse it up front, naming the entity, the
    /// tenant, and the reserved name/table.
    #[test]
    fn entity_colliding_with_the_membership_surface_is_refused_with_jc0553() {
        // V1_FULL tenancy is `Workspace` → reserved table `workspace_members`,
        // reserved struct `WorkspaceMember`.
        // (a) An entity NAMED `WorkspaceMember` collides on the struct type AND —
        // via its default table `workspace_members` — the membership table.
        let mut by_name: Design = serde_json::from_str(V1_FULL).unwrap();
        by_name.modules[0].entities.push(
            serde_json::from_str(
                r#"{ "name": "WorkspaceMember", "fields": [{ "name": "note", "type": "string" }] }"#,
            )
            .unwrap(),
        );
        let qs = validate(&by_name);
        let hit = qs
            .iter()
            .find(|q| q.question.contains("JC0553"))
            .unwrap_or_else(|| panic!("WorkspaceMember must trip JC0553: {qs:?}"));
        assert!(
            hit.question.contains("WorkspaceMember")
                && hit.question.contains("`Workspace`")
                && hit.question.contains("workspace_members")
                && hit.question.to_lowercase().contains("rename"),
            "message names the entity, tenant, reserved table, and the rename fix: {}",
            hit.question
        );
        // Points at the entity NAME — the actionable fix location for a name clash.
        assert_eq!(hit.id, "/modules/0/entities/1/name");

        // (b) A differently-NAMED entity whose explicit `table` override IS the
        // membership table collides on the table alone.
        let mut by_table: Design = serde_json::from_str(V1_FULL).unwrap();
        by_table.modules[0].entities.push(
            serde_json::from_str(
                r#"{ "name": "Seat", "table": "workspace_members", "fields": [{ "name": "note", "type": "string" }] }"#,
            )
            .unwrap(),
        );
        let qs = validate(&by_table);
        let hit = qs
            .iter()
            .find(|q| q.question.contains("JC0553"))
            .unwrap_or_else(|| {
                panic!("a table override onto workspace_members must trip JC0553: {qs:?}")
            });
        assert!(
            hit.question.contains("`Seat`") && hit.question.contains("workspace_members"),
            "table-collision message names the entity and the reserved table: {}",
            hit.question
        );
        assert_eq!(hit.id, "/modules/0/entities/1");
    }

    /// The refusal is precise: a non-colliding entity in a tenancy design is
    /// clean, and a `{Tenant}Member`-named entity in a design with NO tenancy is
    /// clean — no membership surface is generated, so nothing is reserved.
    #[test]
    fn membership_collision_check_is_scoped_to_tenancy_and_real_collisions() {
        // (a) A plain, non-colliding entity in the SAME tenancy design: no JC0553.
        let mut ok: Design = serde_json::from_str(V1_FULL).unwrap();
        ok.modules[0].entities.push(
            serde_json::from_str(
                r#"{ "name": "Post", "fields": [{ "name": "title", "type": "string" }] }"#,
            )
            .unwrap(),
        );
        assert!(
            !validate(&ok).iter().any(|q| q.question.contains("JC0553")),
            "a non-colliding entity must not trip JC0553: {:?}",
            validate(&ok)
        );
        // (b) A `WorkspaceMember` entity with NO tenancy: no membership surface is
        // generated, so there is nothing to collide with.
        let mut no_tenancy: Design = serde_json::from_str(V1_FULL).unwrap();
        no_tenancy.tenancy = None;
        no_tenancy.modules[0].entities.push(
            serde_json::from_str(
                r#"{ "name": "WorkspaceMember", "fields": [{ "name": "note", "type": "string" }] }"#,
            )
            .unwrap(),
        );
        assert!(
            !validate(&no_tenancy)
                .iter()
                .any(|q| q.question.contains("JC0553")),
            "no tenancy → no membership table → no collision: {:?}",
            validate(&no_tenancy)
        );
    }

    // ---- #115 (JC0559): composite / multi-column UNIQUE ------------------------

    /// A single-module db design whose `Like` entity declares a composite `unique`
    /// over its two belongs_to fk columns — the primary #115 shape (a like per
    /// (user, post)). Question-free as written: the group is buildable.
    const COMPOSITE_UNIQUE: &str = r#"{
        "name": "likes-api", "contract_version": 1,
        "dependencies": ["db"],
        "modules": [{
            "name": "engagement",
            "entities": [
                { "name": "User", "fields": [{ "name": "email", "type": "string" }] },
                { "name": "Post", "fields": [{ "name": "title", "type": "string" }] },
                { "name": "Like",
                  "belongs_to": [{ "entity": "User" }, { "entity": "Post" }],
                  "unique": [["user_id", "post_id"]],
                  "fields": [{ "name": "reaction", "type": "string" }] }
            ],
            "endpoints": [
                { "operation_id": "create_like", "method": "POST", "path": "/",
                  "request_body": { "entity": "Like" },
                  "success": { "status": 201, "entity": "Like" } }
            ]
        }]
    }"#;

    /// JC0559 (#115): a valid composite `unique` over two belongs_to fk columns
    /// passes clean — the buildable baseline the three refusals are measured
    /// against, and the byte-identity witness (a field column would pass too).
    #[test]
    fn valid_composite_unique_group_over_fk_columns_passes() {
        assert!(
            !validate(&design(COMPOSITE_UNIQUE))
                .iter()
                .any(|q| q.question.contains("JC0559")),
            "a buildable fk-pair composite unique must not trip JC0559: {:?}",
            validate(&design(COMPOSITE_UNIQUE))
        );
    }

    /// A group with fewer than 2 columns is a footgun that duplicates the field
    /// flag — refused, steering to `Field.unique`.
    #[test]
    fn composite_unique_group_under_two_columns_is_refused_with_jc0559() {
        let one_col = COMPOSITE_UNIQUE.replace(r#"[["user_id", "post_id"]]"#, r#"[["user_id"]]"#);
        let qs = validate(&design(&one_col));
        let hit = qs
            .iter()
            .find(|q| q.question.contains("JC0559"))
            .unwrap_or_else(|| panic!("a 1-col group must trip JC0559: {qs:?}"));
        assert!(
            hit.question.contains("fewer than 2 DISTINCT columns")
                && hit.question.contains("unique: true"),
            "message must name the <2-distinct-col rule and the Field.unique remedy: {}",
            hit.question
        );
        assert_eq!(hit.id, "/modules/0/entities/2/unique/0");
    }

    /// A repeated column (`["a", "a"]`) has 2 entries but 1 DISTINCT column, so
    /// `UNIQUE(a, a)` would silently make column `a` globally unique — refused by
    /// the distinct-count gate, not the raw-length one.
    #[test]
    fn composite_unique_group_with_a_repeated_column_is_refused_with_jc0559() {
        let repeated =
            COMPOSITE_UNIQUE.replace(r#"[["user_id", "post_id"]]"#, r#"[["user_id", "user_id"]]"#);
        let qs = validate(&design(&repeated));
        let hit = qs
            .iter()
            .find(|q| q.question.contains("JC0559"))
            .unwrap_or_else(|| panic!("a repeated-column group must trip JC0559: {qs:?}"));
        assert!(
            hit.question.contains("fewer than 2 DISTINCT columns"),
            "message must name the distinct-count rule for a repeated column: {}",
            hit.question
        );
        assert_eq!(hit.id, "/modules/0/entities/2/unique/0");
    }

    /// A column that is neither a declared field nor a belongs_to fk column would
    /// emit a `CREATE UNIQUE INDEX` that fails at apply — refused loud at `check`.
    #[test]
    fn composite_unique_group_with_unknown_column_is_refused_with_jc0559() {
        let bad = COMPOSITE_UNIQUE.replace(
            r#"[["user_id", "post_id"]]"#,
            r#"[["user_id", "widget_id"]]"#,
        );
        let qs = validate(&design(&bad));
        let hit = qs
            .iter()
            .find(|q| q.question.contains("JC0559"))
            .unwrap_or_else(|| panic!("an unknown column must trip JC0559: {qs:?}"));
        assert!(
            hit.question.contains("widget_id") && hit.question.contains("does not exist"),
            "message must name the offending column and the apply-time failure: {}",
            hit.question
        );
    }

    /// A duplicate group (same column set, order-insensitive) is a redundant
    /// index — refused.
    #[test]
    fn composite_unique_duplicate_group_is_refused_with_jc0559() {
        // Two groups over the same column set, columns reversed — order-insensitive.
        let dup = COMPOSITE_UNIQUE.replace(
            r#"[["user_id", "post_id"]]"#,
            r#"[["user_id", "post_id"], ["post_id", "user_id"]]"#,
        );
        let qs = validate(&design(&dup));
        let hit = qs
            .iter()
            .find(|q| q.question.contains("JC0559") && q.question.contains("duplicates"))
            .unwrap_or_else(|| panic!("a duplicate group must trip JC0559: {qs:?}"));
        assert!(
            hit.question.contains("order does not matter"),
            "message must state order-insensitivity: {}",
            hit.question
        );
        assert_eq!(hit.id, "/modules/0/entities/2/unique/1");
    }

    /// Byte-identity floor: an entity with NO composite `unique` raises no JC0559
    /// (the whole check is inert unless a group is declared).
    #[test]
    fn no_composite_unique_raises_no_jc0559() {
        assert!(
            !validate(&design(MINIMAL))
                .iter()
                .any(|q| q.question.contains("JC0559")),
            "a design with no composite unique must never mention JC0559"
        );
    }

    // ---- #119 (JC0560): belongs_to fk alias -----------------------------------

    /// A ledger with two aliased refs to Account (from/to) AND a self-referential
    /// Comment — the whole point of #119. Fk columns from_account_id/to_account_id/
    /// parent_id are all distinct, so it validates clean.
    const FK_ALIAS: &str = r#"{
        "name": "ledger-api", "contract_version": 1,
        "dependencies": ["db"],
        "modules": [{
            "name": "ledger",
            "entities": [
                { "name": "Account", "fields": [{ "name": "name", "type": "string" }] },
                { "name": "Transfer",
                  "belongs_to": [
                      { "entity": "Account", "as": "from_account" },
                      { "entity": "Account", "as": "to_account" }
                  ],
                  "fields": [{ "name": "amount", "type": "integer" }] },
                { "name": "Comment",
                  "belongs_to": [{ "entity": "Comment", "as": "parent" }],
                  "fields": [{ "name": "body", "type": "string" }] }
            ],
            "endpoints": [
                { "operation_id": "create_transfer", "method": "POST", "path": "/transfers",
                  "request_body": { "entity": "Transfer" },
                  "success": { "status": 201, "entity": "Transfer" } }
            ]
        }]
    }"#;

    /// JC0560 (#119): the buildable baseline — two distinct aliases to one entity
    /// and a self-reference pass clean. This is the acceptance witness the refusals
    /// are measured against.
    #[test]
    fn valid_two_ref_and_self_ref_aliases_pass() {
        let qs = validate(&design(FK_ALIAS));
        assert!(
            !qs.iter().any(|q| q.question.contains("JC0560")),
            "a two-ref + self-ref alias design must not trip JC0560: {qs:?}"
        );
    }

    /// Two UN-aliased belongs_to to the same entity both derive `account_id` — a
    /// duplicate Model field + migration column. Refused with the add-`as` fork.
    #[test]
    fn two_unaliased_refs_to_one_entity_are_refused_with_jc0560() {
        let dup = FK_ALIAS.replace(
            r#"{ "entity": "Account", "as": "from_account" },
                      { "entity": "Account", "as": "to_account" }"#,
            r#"{ "entity": "Account" },
                      { "entity": "Account" }"#,
        );
        let qs = validate(&design(&dup));
        let hit = qs
            .iter()
            .find(|q| q.question.contains("JC0560"))
            .unwrap_or_else(|| panic!("two un-aliased refs must trip JC0560: {qs:?}"));
        assert!(
            hit.question.contains("account_id") && hit.question.contains("SAME column"),
            "message must name the colliding column and the add-`as` remedy: {}",
            hit.question
        );
    }

    /// An `as` whose `{as}_id` collides with a DECLARED field is a duplicate column
    /// — refused (the field/fk name space is shared).
    #[test]
    fn alias_colliding_with_a_declared_field_is_refused_with_jc0560() {
        // Add a field `from_account_id` alongside `belongs_to Account as from_account`.
        let clash = FK_ALIAS.replace(
            r#""fields": [{ "name": "amount", "type": "integer" }] }"#,
            r#""fields": [{ "name": "amount", "type": "integer" }, { "name": "from_account_id", "type": "integer" }] }"#,
        );
        let qs = validate(&design(&clash));
        let hit = qs
            .iter()
            .find(|q| q.question.contains("JC0560") && q.question.contains("declared field"))
            .unwrap_or_else(|| panic!("an fk/field collision must trip JC0560: {qs:?}"));
        assert!(
            hit.question.contains("from_account_id"),
            "message must name the colliding column: {}",
            hit.question
        );
    }

    /// A malformed `as` (not snake_case) yields an invalid column/Rust field —
    /// refused before the collision checks even run.
    #[test]
    fn malformed_as_alias_is_refused_with_jc0560() {
        let bad = FK_ALIAS.replace(r#""as": "from_account""#, r#""as": "FromAccount""#);
        let qs = validate(&design(&bad));
        let hit = qs
            .iter()
            .find(|q| q.question.contains("JC0560"))
            .unwrap_or_else(|| panic!("a malformed `as` must trip JC0560: {qs:?}"));
        assert!(
            hit.question.contains("malformed") && hit.question.contains("FromAccount"),
            "message must name the malformed alias and the snake_case rule: {}",
            hit.question
        );
    }

    /// Byte-identity floor: a design with a single un-aliased belongs_to per target
    /// and no `as` raises no JC0560 (the check is inert unless an alias or a
    /// same-entity collision exists).
    #[test]
    fn plain_belongs_to_raises_no_jc0560() {
        assert!(
            !validate(&design(COMPOSITE_UNIQUE))
                .iter()
                .any(|q| q.question.contains("JC0560")),
            "an unaliased belongs_to design must never mention JC0560"
        );
    }

    /// #119 Finding 1: an `as` deriving the reserved identity fk `user_id` on a
    /// target that isn't the identity entity (here `Account`) would hijack per-user
    /// scoping and fail as opaque generated Rust — refused at design time.
    #[test]
    fn alias_landing_on_the_identity_fk_is_refused_with_jc0560() {
        const D: &str = r#"{
            "name": "chat-api", "contract_version": 1,
            "dependencies": ["db", "auth"],
            "auth": { "model": "session" },
            "modules": [{
                "name": "messages",
                "entities": [
                    { "name": "Account", "fields": [{ "name": "name", "type": "string" }] },
                    { "name": "Message",
                      "belongs_to": [{ "entity": "Account", "as": "user" }],
                      "fields": [{ "name": "body", "type": "string" }] }
                ],
                "endpoints": [
                    { "operation_id": "create_message", "method": "POST", "path": "/messages",
                      "request_body": { "entity": "Message" },
                      "success": { "status": 201, "entity": "Message" } }
                ]
            }]
        }"#;
        let qs = validate(&design(D));
        let hit = qs
            .iter()
            .find(|q| q.question.contains("JC0560") && q.question.contains("identity fk"))
            .unwrap_or_else(|| {
                panic!("`as: user` on a non-identity target must trip JC0560: {qs:?}")
            });
        assert!(
            hit.question.contains("user_id"),
            "message must name the reserved identity fk: {}",
            hit.question
        );
    }

    /// #119 Finding 1: an `as` deriving the reserved tenancy fk on a non-tenant
    /// target would break tenant scoping — refused at design time.
    #[test]
    fn alias_landing_on_the_tenancy_fk_is_refused_with_jc0560() {
        const D: &str = r#"{
            "name": "org-api", "contract_version": 1,
            "dependencies": ["db", "auth"],
            "auth": { "model": "session", "roles": ["owner"] },
            "tenancy": { "entity": "Workspace", "member_roles": ["owner"] },
            "modules": [{
                "name": "workspaces",
                "entities": [
                    { "name": "Workspace", "fields": [{ "name": "name", "type": "string" }] },
                    { "name": "Vendor", "fields": [{ "name": "name", "type": "string" }] },
                    { "name": "Note",
                      "belongs_to": [{ "entity": "Vendor", "as": "workspace" }],
                      "fields": [{ "name": "body", "type": "string" }] }
                ],
                "endpoints": [
                    { "operation_id": "create_workspace", "method": "POST", "path": "/",
                      "auth_required": true,
                      "request_body": { "entity": "Workspace" },
                      "success": { "status": 201, "entity": "Workspace" } }
                ]
            }]
        }"#;
        let qs = validate(&design(D));
        assert!(
            qs.iter()
                .any(|q| q.question.contains("JC0560") && q.question.contains("tenancy fk")),
            "`as: workspace` (the tenancy fk) on a non-tenant target must trip JC0560: {qs:?}"
        );
    }

    // ---- #107 (JC0548): tenancy member_roles content ---------------------------

    /// JC0548 (#107): `member_roles[0]` is the admin role the generated member
    /// surface gates on, and every role is interpolated UNESCAPED into generated
    /// Rust string literals — so with tenancy the list must be non-empty,
    /// duplicate-free, and identifier-shaped (JC0543's charset). Each failure
    /// mode gets its own message naming the offender; without the check, an
    /// empty list silently demotes the admin convention to a fallback role and a
    /// quoted role emits a crate that fails to compile far from the design.
    #[test]
    fn tenancy_member_roles_must_be_nonempty_unique_identifiers() {
        let base: Design = serde_json::from_str(V1_FULL).unwrap();
        // Empty: the admin-role convention has nothing to stand on.
        let mut empty = base.clone();
        empty.tenancy.as_mut().unwrap().member_roles = vec![];
        let c = design_conflict(&empty).expect("empty member_roles must be a conflict");
        assert_eq!(c.code, "JC0548");
        assert!(
            c.message.contains("empty") && c.message.contains("admin"),
            "empty message names the failure and the admin convention: {}",
            c.message
        );
        assert!(!c.hint.is_empty());
        // Duplicate: names the repeated role.
        let mut dup = base.clone();
        dup.tenancy.as_mut().unwrap().member_roles =
            vec!["owner".into(), "member".into(), "owner".into()];
        let c = design_conflict(&dup).expect("duplicated member_roles must be a conflict");
        assert_eq!(c.code, "JC0548");
        assert!(
            c.message.contains("`owner`") && c.message.contains("more than once"),
            "duplicate message names the repeated role: {}",
            c.message
        );
        // Charset: a quote breaks the unescaped interpolation into generated Rust
        // (the same rule JC0543 enforces for enum values); names the offender.
        let mut bad = base.clone();
        bad.tenancy.as_mut().unwrap().member_roles = vec!["owner".into(), "ow\"ner".into()];
        let c = design_conflict(&bad).expect("non-identifier role must be a conflict");
        assert_eq!(c.code, "JC0548");
        assert!(
            c.message.contains("ow\"ner") && c.message.contains("[A-Za-z0-9_-]"),
            "charset message names the offending role and the rule: {}",
            c.message
        );
        // Identifier-shaped, duplicate-free lists pass — including the shipped
        // tenancy conformance design.
        assert!(design_conflict(&base).is_none(), "valid roles must pass");
        let mut hyphen = base.clone();
        hyphen.tenancy.as_mut().unwrap().member_roles =
            vec!["team-lead".into(), "read_only".into(), "Member2".into()];
        assert!(
            design_conflict(&hyphen).is_none(),
            "`-`/`_`/mixed-case roles are identifier-shaped: {:?}",
            design_conflict(&hyphen).map(|c| c.message)
        );
        let reference: Design = serde_json::from_str(CONFORMANCE_REFERENCE).unwrap();
        assert!(
            design_conflict(&reference).is_none(),
            "conformance member_roles must not trip JC0548: {:?}",
            design_conflict(&reference).map(|c| c.message)
        );
    }

    /// Issue #44 (positive): an entity literally named `{X}Request` alongside an
    /// entity `X` whose guarded body omits the identity fk generates two `XRequest`
    /// definitions (Rust struct + OpenAPI component). `design_conflict` rejects it up
    /// front with JC0541 and names the rename fix — cheap insurance for agent-authored
    /// designs, since genroute would otherwise die with a duplicate-struct compile
    /// error mid-scaffold.
    #[test]
    fn entity_shadowing_a_generated_request_dto_is_a_conflict() {
        // Collection (db + auth + identity fk) mints a `CollectionRequest` DTO; an
        // entity literally named `CollectionRequest` collides with it.
        let d: Design = serde_json::from_str(
            r#"{
            "name": "clash", "contract_version": 1,
            "auth": { "model": "session", "roles": ["admin"] },
            "dependencies": ["db", "auth"],
            "modules": [{
                "name": "collections",
                "entities": [
                    { "name": "User", "fields": [{ "name": "email", "type": "string" }] },
                    { "name": "Collection",
                      "belongs_to": [{ "entity": "User", "on_delete": "cascade" }],
                      "fields": [{ "name": "title", "type": "string" }] },
                    { "name": "CollectionRequest", "fields": [{ "name": "note", "type": "string" }] }
                ],
                "endpoints": [
                    { "operation_id": "create_collection", "method": "POST", "path": "/",
                      "auth_required": true,
                      "request_body": { "entity": "Collection" },
                      "success": { "status": 201, "entity": "Collection" } }
                ]
            }]
        }"#,
        )
        .unwrap();
        let conflict = design_conflict(&d).expect("XRequest shadowing a DTO must be a conflict");
        assert_eq!(conflict.code, "JC0541");
        assert!(
            conflict.message.contains("CollectionRequest")
                && conflict.message.contains("Collection")
                && conflict.message.to_lowercase().contains("rename"),
            "message names the collision and the rename fix: {}",
            conflict.message
        );
        assert!(conflict.hint.contains("rename"), "{}", conflict.hint);
    }

    /// Issue #44 (negative): the lint fires ONLY on a REAL collision, not on any
    /// `*Request` suffix. (a) A `{X}Request` entity whose `X` sibling generates NO DTO
    /// (memory mode, no omission) is fine — nothing is shadowed. (b) A `*Request`
    /// entity with no matching base entity is fine. (c) A base `X` that mints a DTO
    /// but has no `XRequest` sibling is fine.
    #[test]
    fn request_suffix_without_a_real_collision_is_not_flagged() {
        // (a) Same names, but MEMORY mode → Collection mints no DTO → no collision.
        let mem: Design = serde_json::from_str(
            r#"{
            "name": "ok-mem", "contract_version": 1,
            "auth": { "model": "session", "roles": ["admin"] },
            "dependencies": ["auth"],
            "modules": [{
                "name": "collections",
                "entities": [
                    { "name": "User", "fields": [{ "name": "email", "type": "string" }] },
                    { "name": "Collection",
                      "belongs_to": [{ "entity": "User", "on_delete": "cascade" }],
                      "fields": [{ "name": "title", "type": "string" }] },
                    { "name": "CollectionRequest", "fields": [{ "name": "note", "type": "string" }] }
                ],
                "endpoints": [
                    { "operation_id": "create_collection", "method": "POST", "path": "/",
                      "auth_required": true,
                      "request_body": { "entity": "Collection" },
                      "success": { "status": 201, "entity": "Collection" } }
                ]
            }]
        }"#,
        )
        .unwrap();
        assert!(
            design_conflict(&mem).is_none(),
            "memory mode mints no DTO — no collision"
        );
        // (b) A lone `*Request` entity with no matching base entity is fine.
        let orphan: Design = serde_json::from_str(
            r#"{
            "name": "ok-orphan", "contract_version": 1, "dependencies": ["db"],
            "modules": [{
                "name": "audit",
                "entities": [
                    { "name": "AuditRequest", "fields": [{ "name": "note", "type": "string" }] }
                ],
                "endpoints": [
                    { "operation_id": "create_audit", "method": "POST", "path": "/",
                      "request_body": { "entity": "AuditRequest" },
                      "success": { "status": 201, "entity": "AuditRequest" } }
                ]
            }]
        }"#,
        )
        .unwrap();
        assert!(
            design_conflict(&orphan).is_none(),
            "a `*Request` name shadowing nothing is fine"
        );
        // (c) A base that mints a DTO but has no `XRequest` sibling is fine.
        let d: Design = serde_json::from_str(SERVER_FK_LITE).unwrap();
        assert!(
            design_conflict(&d).is_none(),
            "a generated DTO with no shadowing entity is fine"
        );
    }

    /// A minimal db+auth+identity-fk design (Collection mints CollectionRequest) with
    /// NO shadowing entity — the JC0541 negative control.
    const SERVER_FK_LITE: &str = r#"{
        "name": "lite", "contract_version": 1,
        "auth": { "model": "session", "roles": ["admin"] },
        "dependencies": ["db", "auth"],
        "modules": [{
            "name": "collections",
            "entities": [
                { "name": "User", "fields": [{ "name": "email", "type": "string" }] },
                { "name": "Collection",
                  "belongs_to": [{ "entity": "User", "on_delete": "cascade" }],
                  "fields": [{ "name": "title", "type": "string" }] }
            ],
            "endpoints": [
                { "operation_id": "create_collection", "method": "POST", "path": "/",
                  "auth_required": true,
                  "request_body": { "entity": "Collection" },
                  "success": { "status": 201, "entity": "Collection" } }
            ]
        }]
    }"#;

    // ---- #105 (JC0549): public-read/owner-write shape --------------------------

    /// A complete, valid public-read/owner-write design (#105): identity-owned
    /// `Post` opted into `public_read`, unguarded GETs (the public reads), and
    /// guarded writes. Entity order puts `Post` first so the bodyless DELETE's
    /// repo-entity fallback resolves to it, mirroring genroute.
    const PUBLIC_READ_FEED: &str = r#"{
        "name": "feed", "contract_version": 1,
        "auth": { "model": "session", "roles": ["admin"] },
        "dependencies": ["db", "auth"],
        "modules": [{
            "name": "posts",
            "entities": [
                { "name": "Post", "public_read": true,
                  "belongs_to": [{ "entity": "User", "on_delete": "cascade" }],
                  "fields": [{ "name": "title", "type": "string" }] },
                { "name": "User", "fields": [{ "name": "email", "type": "string" }] }
            ],
            "endpoints": [
                { "operation_id": "list_posts", "method": "GET", "path": "/",
                  "success": { "status": 200, "entity": "Post", "list": true } },
                { "operation_id": "get_post", "method": "GET", "path": "/{id}",
                  "success": { "status": 200, "entity": "Post" } },
                { "operation_id": "create_post", "method": "POST", "path": "/",
                  "auth_required": true,
                  "request_body": { "entity": "Post" },
                  "success": { "status": 201, "entity": "Post" } },
                { "operation_id": "update_post", "method": "PUT", "path": "/{id}",
                  "auth_required": true,
                  "request_body": { "entity": "Post" },
                  "success": { "status": 200, "entity": "Post" } },
                { "operation_id": "delete_post", "method": "DELETE", "path": "/{id}",
                  "auth_required": true,
                  "success": { "status": 204 } }
            ]
        }]
    }"#;

    fn jc0549(qs: &[Question]) -> Vec<&Question> {
        qs.iter()
            .filter(|q| q.question.contains("JC0549"))
            .collect()
    }

    /// #105 (positive): the blessed shape — public_read on an identity-owned,
    /// non-tenant entity in an auth design, with unguarded GETs and guarded
    /// writes — is exactly what the flag exists for, so it must sail through
    /// validation untouched.
    #[test]
    fn public_read_per_user_design_passes_validation() {
        let d: Design = serde_json::from_str(PUBLIC_READ_FEED).unwrap();
        let qs = validate(&d);
        assert!(
            qs.is_empty(),
            "a valid public_read design must pass: {qs:?}"
        );
        assert!(
            design_conflict(&d).is_none(),
            "no structural conflict either"
        );
    }

    /// #105: public_read on a tenant-owned entity would let its public reads
    /// bypass the Tenant guard (one tenant's rows exposed to anyone) — refused,
    /// mirroring the public-endpoint-on-tenant-owned rejection.
    #[test]
    fn public_read_on_tenant_owned_entity_is_rejected() {
        let mut d: Design = serde_json::from_str(PUBLIC_READ_FEED).unwrap();
        d.tenancy = Some(
            serde_json::from_str(r#"{ "entity": "Org", "member_roles": ["owner"] }"#).unwrap(),
        );
        d.modules[0].entities.push(
            serde_json::from_str(
                r#"{ "name": "Org", "fields": [{ "name": "label", "type": "string" }] }"#,
            )
            .unwrap(),
        );
        d.modules[0].entities[0]
            .belongs_to
            .push(serde_json::from_str(r#"{ "entity": "Org" }"#).unwrap());
        let qs = validate(&d);
        let hits = jc0549(&qs);
        assert!(
            hits.iter()
                .any(|q| q.question.contains("tenant-owned") && q.question.contains("Post")),
            "tenant-owned public_read must raise JC0549 naming the entity: {qs:?}"
        );
    }

    /// #105: public_read is a modifier on the per-user ownership shape — an
    /// entity with no identity fk has no owner to scope writes to, so the flag
    /// is meaningless and refused with the belongs_to fix.
    #[test]
    fn public_read_on_non_identity_entity_is_rejected() {
        let mut d: Design = serde_json::from_str(PUBLIC_READ_FEED).unwrap();
        d.modules[0].entities[0].belongs_to.clear();
        let qs = validate(&d);
        let hits = jc0549(&qs);
        assert!(
            hits.iter()
                .any(|q| q.question.contains("identity fk") && q.question.contains("Post")),
            "public_read without an identity fk must raise JC0549: {qs:?}"
        );
    }

    /// #105: without an active auth model there is no session to owner-gate the
    /// writes with — public_read would be public-read/public-write.
    #[test]
    fn public_read_without_auth_model_is_rejected() {
        let mut d: Design = serde_json::from_str(PUBLIC_READ_FEED).unwrap();
        d.auth = None;
        d.dependencies.retain(|dep| dep != "auth");
        let qs = validate(&d);
        let hits = jc0549(&qs);
        assert!(
            hits.iter()
                .any(|q| q.question.contains("no active auth model")),
            "public_read without auth must raise JC0549: {qs:?}"
        );
    }

    /// #105: the open door. public_read makes READS public; a write that is
    /// itself public or unguarded would let anyone mutate the rows everyone
    /// reads — every write of the entity must stay owner-gated.
    #[test]
    fn public_or_unguarded_write_on_public_read_entity_is_rejected() {
        // (a) an explicit `public: true` write.
        let mut d: Design = serde_json::from_str(PUBLIC_READ_FEED).unwrap();
        let create = d.modules[0]
            .endpoints
            .iter_mut()
            .find(|ep| ep.operation_id == "create_post")
            .unwrap();
        create.auth_required = false;
        create.public = true;
        let qs = validate(&d);
        assert!(
            jc0549(&qs)
                .iter()
                .any(|q| q.question.contains("create_post") && q.question.contains("write")),
            "a public write on a public_read entity must raise JC0549: {qs:?}"
        );
        // (b) a merely unguarded write (no auth_required, not public).
        let mut d2: Design = serde_json::from_str(PUBLIC_READ_FEED).unwrap();
        d2.modules[0]
            .endpoints
            .iter_mut()
            .find(|ep| ep.operation_id == "delete_post")
            .unwrap()
            .auth_required = false;
        let qs2 = validate(&d2);
        assert!(
            jc0549(&qs2)
                .iter()
                .any(|q| q.question.contains("delete_post")),
            "an unguarded write on a public_read entity must raise JC0549: {qs2:?}"
        );
    }

    /// #105 §E (the latent-bug closure, independent of opt-in): an unguarded GET
    /// on a per-user owner-scoped entity that has NOT opted into public_read
    /// generates an unimplementable stub today — the repo emits only owner-scoped
    /// accessors while the handler gets no session user. JC0549 turns the silent
    /// dead-end into a clear fork: opt into public_read, or guard the GET.
    #[test]
    fn unguarded_get_on_owner_scoped_entity_without_public_read_is_rejected() {
        let mut d: Design = serde_json::from_str(PUBLIC_READ_FEED).unwrap();
        d.modules[0].entities[0].public_read = false;
        let qs = validate(&d);
        let hits = jc0549(&qs);
        assert_eq!(
            hits.len(),
            2,
            "both unguarded GETs (list + detail) must be flagged: {qs:?}"
        );
        assert!(
            hits.iter().all(|q| q.question.contains("unimplementable")
                && q.question.contains("public_read: true")
                && q.question.contains("auth_required: true")),
            "the message must present the fork (opt in, or guard the GET): {qs:?}"
        );

        // Memory mode keeps the unscoped repo reads, so the stub IS
        // implementable there — no JC0549.
        let mut mem: Design = serde_json::from_str(PUBLIC_READ_FEED).unwrap();
        mem.modules[0].entities[0].public_read = false;
        mem.dependencies.retain(|dep| dep != "db");
        assert!(
            jc0549(&validate(&mem)).is_empty(),
            "memory mode has no owner-scoped repo suppression"
        );
    }

    /// #105 §E, the `public: true` spelling of the same residual (the shape the
    /// Supabase migrator used to emit for a public-read owner table): `public`
    /// is just an unguarded read with the lint carve-out, and its stub is
    /// exactly as unimplementable — the old `!ep.public` exemption let it slide
    /// through validation into a dead-end scaffold. Only the `public_read`
    /// entity flag makes an open read coherent; WITH the flag the same
    /// public-marked GETs are the blessed migrated-feed shape.
    #[test]
    fn public_get_on_owner_scoped_entity_without_public_read_is_rejected() {
        let mut d: Design = serde_json::from_str(PUBLIC_READ_FEED).unwrap();
        d.modules[0].entities[0].public_read = false;
        for ep in &mut d.modules[0].endpoints {
            if matches!(ep.method, HttpMethod::GET) {
                ep.public = true;
            }
        }
        let qs = validate(&d);
        let hits = jc0549(&qs);
        assert_eq!(
            hits.len(),
            2,
            "both public GETs (list + detail) must be flagged: {qs:?}"
        );

        let mut ok: Design = serde_json::from_str(PUBLIC_READ_FEED).unwrap();
        for ep in &mut ok.modules[0].endpoints {
            if matches!(ep.method, HttpMethod::GET) {
                ep.public = true;
            }
        }
        let qs2 = validate(&ok);
        assert!(
            qs2.is_empty(),
            "public GETs + the public_read flag is the migrated feed shape: {qs2:?}"
        );
    }

    // ---- #148 (JC0558): anonymous read/write on tenant / tenant-owned ----------

    fn jc0558(qs: &[Question]) -> Vec<&Question> {
        qs.iter()
            .filter(|q| q.question.contains("JC0558"))
            .collect()
    }

    /// A SAFE tenancy design: a tenant root (`Club`) and a tenant-owned child
    /// (`Event` belongs_to Club), every read authenticated. It must validate
    /// question-free — that is the byte-identity baseline JC0558 preserves.
    const JC0558_BASE: &str = r#"{
        "name": "clubs", "contract_version": 1,
        "auth": { "model": "session", "roles": ["owner", "member"] },
        "dependencies": ["db", "auth"],
        "tenancy": { "entity": "Club", "member_roles": ["owner", "member"] },
        "modules": [
            { "name": "clubs",
              "entities": [{ "name": "Club", "fields": [
                  { "name": "id", "type": "integer" },
                  { "name": "name", "type": "string" } ]}],
              "endpoints": [
                  { "operation_id": "list_clubs", "method": "GET", "path": "/",
                    "auth_required": true,
                    "success": { "status": 200, "entity": "Club", "list": true } },
                  { "operation_id": "show_club", "method": "GET", "path": "/{id}",
                    "auth_required": true,
                    "success": { "status": 200, "entity": "Club" } }
              ] },
            { "name": "events",
              "entities": [{ "name": "Event",
                  "belongs_to": [{ "entity": "Club", "on_delete": "cascade" }],
                  "fields": [
                      { "name": "id", "type": "integer" },
                      { "name": "title", "type": "string" } ]}],
              "endpoints": [
                  { "operation_id": "list_events", "method": "GET", "path": "/",
                    "auth_required": true,
                    "success": { "status": 200, "entity": "Event", "list": true } }
              ] }
        ]
    }"#;

    /// (c) The blessed shape: every tenant/tenant-owned read is authenticated,
    /// so no handler is anonymous — JC0558 must stay silent and the whole design
    /// is question-free (the safe-design byte-identity baseline).
    #[test]
    fn jc0558_does_not_fire_when_every_tenant_read_is_authenticated() {
        let d: Design = serde_json::from_str(JC0558_BASE).unwrap();
        let qs = validate(&d);
        assert!(
            qs.is_empty(),
            "the safe tenancy design must pass clean: {qs:?}"
        );
    }

    /// (a) An unguarded, non-`public` GET on the TENANT ROOT entity is anonymous
    /// — genroute emits no Dep<Tenant>/CurrentUser, so any caller reads any
    /// tenant's row by id. JC0558 must refuse it, naming the tenant entity.
    #[test]
    fn jc0558_fires_on_an_unguarded_read_of_the_tenant_entity() {
        let mut d: Design = serde_json::from_str(JC0558_BASE).unwrap();
        // show_club is the tenant's own detail route (`GET /{id}` on Club).
        d.modules[0]
            .endpoints
            .iter_mut()
            .find(|ep| ep.operation_id == "show_club")
            .unwrap()
            .auth_required = false;
        let qs = validate(&d);
        assert!(
            jc0558(&qs)
                .iter()
                .any(|q| q.question.contains("show_club") && q.question.contains("`Club`")),
            "an unguarded read on the tenant root must trip JC0558 naming Club: {qs:?}"
        );
    }

    /// (b) The same anonymous shape on a tenant-OWNED (belongs_to) entity — an
    /// unguarded read of `Event` exposes every tenant's events. JC0558 fires.
    #[test]
    fn jc0558_fires_on_an_unguarded_read_of_a_tenant_owned_entity() {
        let mut d: Design = serde_json::from_str(JC0558_BASE).unwrap();
        d.modules[1]
            .endpoints
            .iter_mut()
            .find(|ep| ep.operation_id == "list_events")
            .unwrap()
            .auth_required = false;
        let qs = validate(&d);
        assert!(
            jc0558(&qs)
                .iter()
                .any(|q| q.question.contains("list_events") && q.question.contains("`Event`")),
            "an unguarded read on a tenant-owned entity must trip JC0558 naming Event: {qs:?}"
        );
    }

    /// (d) The `!ep.public` exemption: a `public` route bypasses JC0558. On a
    /// tenant-OWNED entity the open-read contradiction is the separate
    /// public-on-tenant-owned refusal; JC0558 itself must not double-fire.
    #[test]
    fn jc0558_does_not_fire_on_a_public_route() {
        let mut d: Design = serde_json::from_str(JC0558_BASE).unwrap();
        let read = d.modules[1]
            .endpoints
            .iter_mut()
            .find(|ep| ep.operation_id == "list_events")
            .unwrap();
        read.auth_required = false;
        read.public = true;
        let qs = validate(&d);
        assert!(
            jc0558(&qs).is_empty(),
            "JC0558 must not fire on a public route (the public-on-tenant-owned refusal owns it): {qs:?}"
        );
        // The public read on a tenant-owned entity IS refused — by the
        // public-on-tenant-owned check, proving JC0558 correctly stood down.
        assert!(
            qs.iter()
                .any(|q| q.question.contains("public") && q.question.contains("tenant-owned")),
            "a public read on a tenant-owned entity is still refused (just not by JC0558): {qs:?}"
        );
    }

    /// (e) An entity-less join/leave subroute (the membership escape hatch) reads
    /// no repo — the STRICT resolver returns None, so JC0558 must not tie it to a
    /// tenant-owned neighbor via a first-entity fallback and falsely refuse it.
    #[test]
    fn jc0558_does_not_fire_on_an_entityless_subroute() {
        let mut d: Design = serde_json::from_str(JC0558_BASE).unwrap();
        // Graft an entity-less subroute under the clubs module with an unguarded,
        // non-public endpoint. No entities ⇒ strict resolves to None ⇒ no fire.
        d.modules[0].subroutes.push(
            serde_json::from_str(
                r#"{ "name": "membership", "mount": "/{club_id}/membership",
                     "endpoints": [
                         { "operation_id": "join_club", "method": "POST", "path": "/join",
                           "success": { "status": 200 } } ] }"#,
            )
            .unwrap(),
        );
        let qs = validate(&d);
        assert!(
            jc0558(&qs).is_empty(),
            "an entity-less subroute reads no tenant repo — JC0558 must not fire: {qs:?}"
        );
    }

    /// (f) A signature-authenticated webhook (Stripe-style) is intentionally
    /// unguarded — it proves itself by signature (JL0004 exempts it, so JC0558
    /// must too). The exemption is load-bearing: without the signature error the
    /// SAME unguarded endpoint on the tenant-owned entity WOULD trip JC0558.
    #[test]
    fn jc0558_does_not_fire_on_a_signature_authed_webhook() {
        let webhook = |signature: bool| -> Design {
            let mut d: Design = serde_json::from_str(JC0558_BASE).unwrap();
            let errors = if signature {
                r#", "errors": [{ "status": 400, "when": "Stripe signature is missing or invalid" }]"#
            } else {
                ""
            };
            d.modules[1].endpoints.push(
                serde_json::from_str(&format!(
                    r#"{{ "operation_id": "event_webhook", "method": "POST", "path": "/webhook",
                         "success": {{ "status": 200, "entity": "Event" }}{errors} }}"#
                ))
                .unwrap(),
            );
            d
        };
        // With the signature error case: exempt — no JC0558 for the webhook.
        let qs = validate(&webhook(true));
        assert!(
            !jc0558(&qs)
                .iter()
                .any(|q| q.question.contains("event_webhook")),
            "a signature-authed webhook must be exempt from JC0558: {qs:?}"
        );
        // Without it: the exemption is what suppresses the refusal (Rule 9).
        let qs_bare = validate(&webhook(false));
        assert!(
            jc0558(&qs_bare)
                .iter()
                .any(|q| q.question.contains("event_webhook")),
            "an unguarded non-signature webhook on a tenant-owned entity DOES trip JC0558: {qs_bare:?}"
        );
    }

    /// (g) A per-user (identity-owned, NON-tenant) entity's unguarded read is
    /// JC0549's lane, not JC0558's: `tenant_path` is None, so the tenant twin
    /// must stay out of it (JC0549(c) claims the unguarded per-user read).
    #[test]
    fn jc0558_does_not_fire_on_a_per_user_non_tenant_entity() {
        let d: Design = serde_json::from_str(
            r#"{
            "name": "mixed", "contract_version": 1,
            "auth": { "model": "session", "roles": ["owner"] },
            "dependencies": ["db", "auth"],
            "tenancy": { "entity": "Club", "member_roles": ["owner"] },
            "modules": [
                { "name": "clubs",
                  "entities": [
                      { "name": "Club", "fields": [
                          { "name": "id", "type": "integer" },
                          { "name": "name", "type": "string" } ]},
                      { "name": "User", "fields": [
                          { "name": "id", "type": "integer" },
                          { "name": "email", "type": "string" } ]}
                  ],
                  "endpoints": [
                      { "operation_id": "show_club", "method": "GET", "path": "/{id}",
                        "auth_required": true,
                        "success": { "status": 200, "entity": "Club" } }
                  ] },
                { "name": "notes",
                  "entities": [{ "name": "Note",
                      "belongs_to": [{ "entity": "User" }],
                      "fields": [
                          { "name": "id", "type": "integer" },
                          { "name": "body", "type": "string" } ]}],
                  "endpoints": [
                      { "operation_id": "list_notes", "method": "GET", "path": "/",
                        "success": { "status": 200, "entity": "Note", "list": true } }
                  ] }
            ]
        }"#,
        )
        .unwrap();
        let qs = validate(&d);
        assert!(
            jc0558(&qs).is_empty(),
            "an unguarded per-user (non-tenant) read is JC0549's domain, not JC0558's: {qs:?}"
        );
        // The boundary: JC0549(c) owns that unguarded per-user read.
        assert!(
            qs.iter().any(|q| q.question.contains("JC0549")),
            "JC0549(c) claims the unguarded per-user read (the twin's lane): {qs:?}"
        );
    }

    /// The strict-resolution pin (#105 whole-branch review): an ENTITY-LESS
    /// `public: true` GET (`GET /config`, custom-JSON success, no body, no
    /// `{param}` — the documented hand-written `Json<serde_json::Value>`
    /// shape) in a PLAIN per-user module VALIDATES question-free. WHY (Rule
    /// 9): JC0549(c) resolved the endpoint's repo entity through the lenient
    /// first-entity fallback, tied `/config` to the owner-scoped `Post` it
    /// never reads, and FALSELY refused an implementable design base accepted
    /// — the check must fire only for GETs an explicit signal ties to the
    /// owner-scoped entity. The unguarded-read refusal on the REAL entity
    /// reads (explicit `success.entity`/`{id}`) must keep firing.
    #[test]
    fn entityless_public_get_in_a_plain_per_user_module_passes_validation() {
        let mut d: Design = serde_json::from_str(PUBLIC_READ_FEED).unwrap();
        d.modules[0].entities[0].public_read = false;
        // Guard the entity reads (the valid #79 prong) so only /config varies.
        for ep in &mut d.modules[0].endpoints {
            ep.auth_required = true;
        }
        let config: Endpoint = serde_json::from_str(
            r#"{ "operation_id": "get_config", "method": "GET", "path": "/config",
                 "public": true, "success": { "status": 200 } }"#,
        )
        .unwrap();
        d.modules[0].endpoints.push(config);
        let qs = validate(&d);
        assert!(
            qs.is_empty(),
            "an entity-less public custom-JSON GET reads no entity's repo and must not be refused: {qs:?}"
        );

        // The refusal still fires when an EXPLICIT signal ties the unguarded
        // GET to the owner-scoped entity — the strict resolver narrows the
        // check, it does not disable it.
        let mut still: Design = serde_json::from_str(PUBLIC_READ_FEED).unwrap();
        still.modules[0].entities[0].public_read = false;
        let qs2 = validate(&still);
        assert_eq!(
            jc0549(&qs2).len(),
            2,
            "explicit unguarded reads of the owner-scoped entity stay refused: {qs2:?}"
        );
    }

    /// #105 §E (negative): guarding the GETs is the other prong of the fork —
    /// a fully owner-scoped entity (#79) with authenticated reads stays exactly
    /// as valid as before.
    #[test]
    fn guarded_get_on_owner_scoped_entity_passes_validation() {
        let mut d: Design = serde_json::from_str(PUBLIC_READ_FEED).unwrap();
        d.modules[0].entities[0].public_read = false;
        for ep in &mut d.modules[0].endpoints {
            ep.auth_required = true;
        }
        let qs = validate(&d);
        assert!(
            qs.is_empty(),
            "the guarded per-user shape must keep passing: {qs:?}"
        );
    }

    #[test]
    fn jobs_validate_name_uniqueness_and_cron_shape() {
        let mut d: Design = serde_json::from_str(V1_FULL).unwrap();
        d.jobs[0].schedule = Some("not cron".into());
        assert!(validate(&d).iter().any(|q| q.id == "/jobs/0/schedule"));
        let mut d2: Design = serde_json::from_str(V1_FULL).unwrap();
        d2.jobs.push(d2.jobs[0].clone());
        assert!(validate(&d2).iter().any(|q| q.id == "/jobs/1/name"));
    }

    #[test]
    fn jobs_validate_queue_is_snake_case() {
        // The queue is interpolated RAW into generated Rust string literals
        // (`.queue("{q}", ...)`); a `"` in the queue would break the generated
        // crate at build time, far from the design. Validation must reject a
        // non-identifier queue up front, mirroring the job-name check.
        let mut d: Design = serde_json::from_str(V1_FULL).unwrap();
        d.jobs[0].queue = Some("not a queue\"".into());
        assert!(
            validate(&d).iter().any(|q| q.id == "/jobs/0/queue"),
            "a non-snake_case job queue must be a validation error"
        );
        // A valid snake_case queue passes.
        let mut ok: Design = serde_json::from_str(V1_FULL).unwrap();
        ok.jobs[0].queue = Some("billing".into());
        assert!(!validate(&ok).iter().any(|q| q.id == "/jobs/0/queue"));
    }

    #[test]
    fn jobs_require_a_database_dependency() {
        // Jobs run over a Postgres store; the generated `jobs(db)` wiring +
        // JOBS_MIGRATIONS need `jerrycan::db::Db`. A jobs-without-db design can't
        // compile, so validation rejects it before generation.
        let mut d: Design = serde_json::from_str(V1_FULL).unwrap();
        d.dependencies.retain(|dep| dep != "db");
        assert!(d.wants_jobs() && !d.wants_db());
        assert!(
            validate(&d).iter().any(|q| q.id == "/jobs"),
            "jobs without a db dependency must be a validation error"
        );
        // With db present (the unmodified fixture), no jobs-require-db error.
        let ok: Design = serde_json::from_str(V1_FULL).unwrap();
        assert!(!validate(&ok).iter().any(|q| q.id == "/jobs"));
    }

    #[test]
    fn enum_values_only_on_string_fields_and_nonempty() {
        let mut d: Design = serde_json::from_str(V1_FULL).unwrap();
        d.modules[0].entities[0].fields[0].values = Some(vec!["x".into()]); // id: integer
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.id == "/modules/0/entities/0/fields/0/values")
        );
        let mut d2: Design = serde_json::from_str(V1_FULL).unwrap();
        d2.modules[0].entities[0].fields[1].values = Some(vec![]); // empty
        assert!(
            validate(&d2)
                .iter()
                .any(|q| q.id == "/modules/0/entities/0/fields/1/values")
        );
    }

    #[test]
    fn field_default_must_type_check_against_field_type_and_enum() {
        // Issue #53a: a server-owned `default` is written verbatim into a NOT-NULL
        // column, so a mistyped or out-of-enum literal is a design-time error, not
        // a run-time surprise. Valid defaults raise no question.
        let base = |field: &str| {
            design(&format!(
                r#"{{ "name": "news", "contract_version": 0, "dependencies": ["db"],
                    "modules": [{{ "name": "subs",
                        "entities": [{{ "name": "Subscriber", "fields": [{field}] }}],
                        "endpoints": [{{ "operation_id": "create_subscriber", "method": "POST", "path": "/",
                            "request_body": {{ "entity": "Subscriber" }},
                            "success": {{ "status": 201, "entity": "Subscriber" }} }}] }}] }}"#
            ))
        };
        // A boolean default `false` and an enum default `"active"` are valid.
        let ok = base(
            r#"{ "name": "confirmed", "type": "boolean", "default": false },
               { "name": "status", "type": "string", "values": ["active", "expired"], "default": "active" }"#,
        );
        assert!(
            !validate(&ok).iter().any(|q| q.id.ends_with("/default")),
            "valid defaults raise no question: {:?}",
            validate(&ok)
        );
        // A string literal on a boolean field is rejected.
        let bad_type = base(r#"{ "name": "confirmed", "type": "boolean", "default": "false" }"#);
        assert!(
            validate(&bad_type)
                .iter()
                .any(|q| q.id == "/modules/0/entities/0/fields/0/default"),
            "mistyped default must be a question: {:?}",
            validate(&bad_type)
        );
        // A default outside the enum `values` is rejected.
        let bad_enum = base(
            r#"{ "name": "status", "type": "string", "values": ["active", "expired"], "default": "draft" }"#,
        );
        assert!(
            validate(&bad_enum)
                .iter()
                .any(|q| q.id == "/modules/0/entities/0/fields/0/default"),
            "out-of-enum default must be a question: {:?}",
            validate(&bad_enum)
        );
        // A default without a `db` dependency is inert (no request DTO) → rejected.
        let no_db: Design = serde_json::from_str(
            r#"{ "name": "news", "contract_version": 0, "dependencies": [],
                "modules": [{ "name": "subs",
                    "entities": [{ "name": "Subscriber", "fields": [
                        { "name": "email", "type": "string" },
                        { "name": "confirmed", "type": "boolean", "default": false } ] }],
                    "endpoints": [{ "operation_id": "list_subs", "method": "GET", "path": "/",
                        "success": { "status": 200 } }] }] }"#,
        )
        .unwrap();
        assert!(
            validate(&no_db)
                .iter()
                .any(|q| q.id == "/modules/0/entities/0/fields/1/default"
                    && q.question.contains("no `db` dependency")),
            "a default without db must be a question: {:?}",
            validate(&no_db)
        );
    }

    #[test]
    fn now_default_is_clean_on_datetime_and_jc0557_elsewhere() {
        // Issue #110: the `"now"` sentinel is a DYNAMIC server-set timestamp valid
        // ONLY on a `datetime` field. The exact lowercase `"now"` on a datetime is
        // clean (the server sets it via now_rfc3339); `"now"` on any other type, or a
        // mis-cased near-miss on a datetime, is JC0557 — never silently stored as a
        // literal.
        let base = |field: &str| {
            design(&format!(
                r#"{{ "name": "notes", "contract_version": 0, "dependencies": ["db"],
                    "modules": [{{ "name": "notes",
                        "entities": [{{ "name": "Note", "fields": [{field}] }}],
                        "endpoints": [{{ "operation_id": "create_note", "method": "POST", "path": "/",
                            "request_body": {{ "entity": "Note" }},
                            "success": {{ "status": 201, "entity": "Note" }} }}] }}] }}"#
            ))
        };
        // Exact `"now"` on a datetime field raises NO question.
        let ok = base(r#"{ "name": "created_at", "type": "datetime", "default": "now" }"#);
        assert!(
            !validate(&ok).iter().any(|q| q.id.ends_with("/default")),
            "a datetime default:\"now\" is clean: {:?}",
            validate(&ok)
        );
        // `"now"` on a string field → JC0557 (it would otherwise be a valid literal).
        let bad_string = base(r#"{ "name": "label", "type": "string", "default": "now" }"#);
        assert!(
            validate(&bad_string)
                .iter()
                .any(|q| q.id.ends_with("/fields/0/default")
                    && q.question.contains("JC0557")
                    && q.question.contains("datetime")),
            "\"now\" on a string field must be JC0557: {:?}",
            validate(&bad_string)
        );
        // `"now"` on an integer field → JC0557 (not the generic type-mismatch).
        let bad_int = base(r#"{ "name": "count", "type": "integer", "default": "now" }"#);
        assert!(
            validate(&bad_int)
                .iter()
                .any(|q| q.id.ends_with("/fields/0/default") && q.question.contains("JC0557")),
            "\"now\" on an integer field must be JC0557: {:?}",
            validate(&bad_int)
        );
        // A mis-cased near-miss (`"NOW"`) on a datetime field → JC0557 (never read
        // as a static literal).
        let bad_case = base(r#"{ "name": "created_at", "type": "datetime", "default": "NOW" }"#);
        assert!(
            validate(&bad_case)
                .iter()
                .any(|q| q.id.ends_with("/fields/0/default")
                    && q.question.contains("JC0557")
                    && q.question.contains("mis-cased")),
            "\"NOW\" on a datetime field must be JC0557: {:?}",
            validate(&bad_case)
        );
        // A `default:"now"` without `db` reuses the existing db-required path.
        let no_db: Design = serde_json::from_str(
            r#"{ "name": "notes", "contract_version": 0, "dependencies": [],
                "modules": [{ "name": "notes",
                    "entities": [{ "name": "Note", "fields": [
                        { "name": "created_at", "type": "datetime", "default": "now" } ] }],
                    "endpoints": [{ "operation_id": "list_notes", "method": "GET", "path": "/",
                        "success": { "status": 200 } }] }] }"#,
        )
        .unwrap();
        assert!(
            validate(&no_db)
                .iter()
                .any(|q| q.id.ends_with("/default") && q.question.contains("no `db` dependency")),
            "a now default without db reuses the db-required message: {:?}",
            validate(&no_db)
        );
    }

    // ---- #80 (JC0552): field range/length constraints -------------------------

    /// One entity, one create endpoint, the given field JSON — the minimal
    /// otherwise-clean design the constraint checks run against.
    fn constraint_base(field: &str) -> Design {
        design(&format!(
            r#"{{ "name": "shop", "contract_version": 0, "dependencies": ["db"],
                "modules": [{{ "name": "items",
                    "entities": [{{ "name": "Item", "fields": [{field}] }}],
                    "endpoints": [{{ "operation_id": "create_item", "method": "POST", "path": "/",
                        "request_body": {{ "entity": "Item" }},
                        "success": {{ "status": 201, "entity": "Item" }} }}] }}] }}"#
        ))
    }

    /// WHY (#80): a declared, well-placed constraint is the whole point of the
    /// feature — it must validate CLEAN so the design proceeds to generation
    /// (min_len exactly at the 4096 ceiling included).
    #[test]
    fn valid_field_constraints_are_question_free() {
        let ok = constraint_base(
            r#"{ "name": "quantity", "type": "integer", "min": 1, "max": 600 },
               { "name": "bio", "type": "string", "max_len": 280 },
               { "name": "body", "type": "string", "min_len": 4096, "required": false }"#,
        );
        assert!(validate(&ok).is_empty(), "{:?}", validate(&ok));
    }

    /// `min`/`max` are an integer range — on a string or json field no generated
    /// comparison exists, so placement is refused per offending key.
    #[test]
    fn range_keys_are_refused_on_non_integer_fields() {
        let qs = validate(&constraint_base(
            r#"{ "name": "bio", "type": "string", "min": 1 }"#,
        ));
        assert!(
            qs.iter()
                .any(|q| q.id == "/modules/0/entities/0/fields/0/min"
                    && q.question.contains("integer")
                    && q.question.contains("JC0552")),
            "{qs:?}"
        );
        let qs = validate(&constraint_base(
            r#"{ "name": "custom", "type": "json", "max": 5 }"#,
        ));
        assert!(
            qs.iter()
                .any(|q| q.id == "/modules/0/entities/0/fields/0/max"
                    && q.question.contains("JC0552")),
            "{qs:?}"
        );
    }

    /// `min_len`/`max_len` count Unicode code points of a string — on an integer
    /// field there is no length to bound.
    #[test]
    fn length_keys_are_refused_on_non_string_fields() {
        let qs = validate(&constraint_base(
            r#"{ "name": "quantity", "type": "integer", "max_len": 10 }"#,
        ));
        assert!(
            qs.iter()
                .any(|q| q.id == "/modules/0/entities/0/fields/0/max_len"
                    && q.question.contains("string")
                    && q.question.contains("JC0552")),
            "{qs:?}"
        );
    }

    /// min > max (and min_len > max_len) is an empty range: no value satisfies
    /// it, so no in-range fixture is derivable — un-greenable by construction.
    #[test]
    fn empty_ranges_are_refused() {
        let qs = validate(&constraint_base(
            r#"{ "name": "quantity", "type": "integer", "min": 10, "max": 1 }"#,
        ));
        assert!(
            qs.iter()
                .any(|q| q.id == "/modules/0/entities/0/fields/0/min"
                    && q.question.contains("empty range")
                    && q.question.contains("JC0552")),
            "{qs:?}"
        );
        let qs = validate(&constraint_base(
            r#"{ "name": "bio", "type": "string", "min_len": 10, "max_len": 2 }"#,
        ));
        assert!(
            qs.iter()
                .any(|q| q.id == "/modules/0/entities/0/fields/0/min_len"
                    && q.question.contains("empty range")
                    && q.question.contains("JC0552")),
            "{qs:?}"
        );
    }

    /// Enum `values` already fix the exact allowed strings — a length bound on
    /// top is a contradiction (mirrors "values only on string").
    #[test]
    fn length_keys_are_refused_alongside_enum_values() {
        let qs = validate(&constraint_base(
            r#"{ "name": "status", "type": "string", "values": ["active", "expired"], "max_len": 5 }"#,
        ));
        assert!(
            qs.iter()
                .any(|q| q.id == "/modules/0/entities/0/fields/0/max_len"
                    && q.question.contains("values")
                    && q.question.contains("JC0552")),
            "{qs:?}"
        );
    }

    /// A required string with `max_len: 0` can never be filled; the same bound
    /// on an optional field is legal (the client omits it).
    #[test]
    fn max_len_zero_on_a_required_field_is_unfillable() {
        let qs = validate(&constraint_base(
            r#"{ "name": "bio", "type": "string", "max_len": 0 }"#,
        ));
        assert!(
            qs.iter()
                .any(|q| q.id == "/modules/0/entities/0/fields/0/max_len"
                    && q.question.contains("required")
                    && q.question.contains("JC0552")),
            "{qs:?}"
        );
        let optional = constraint_base(
            r#"{ "name": "title", "type": "string" },
               { "name": "bio", "type": "string", "max_len": 0, "required": false }"#,
        );
        assert!(
            !validate(&optional)
                .iter()
                .any(|q| q.question.contains("JC0552")),
            "{:?}",
            validate(&optional)
        );
    }

    /// The pk `id` is server-assigned — the id-echo probe and the seeds assume
    /// free ids, so ANY constraint key on `id` is refused (even a well-typed one).
    #[test]
    fn constraint_keys_on_the_pk_id_are_refused() {
        let mut d: Design = serde_json::from_str(V1_FULL).unwrap();
        d.modules[0].entities[0].fields[0].min = Some(1); // id: integer
        let qs = validate(&d);
        assert!(
            qs.iter()
                .any(|q| q.id == "/modules/0/entities/0/fields/0/min"
                    && q.question.contains("primary key")
                    && q.question.contains("JC0552")),
            "{qs:?}"
        );
        let mut d2: Design = serde_json::from_str(V1_FULL).unwrap();
        d2.modules[0].entities[0].fields[0].max_len = Some(8);
        let qs = validate(&d2);
        assert!(
            qs.iter()
                .any(|q| q.id == "/modules/0/entities/0/fields/0/max_len"
                    && q.question.contains("primary key")),
            "{qs:?}"
        );
    }

    /// #112 (JC0554): an EXPLICIT `write_only: true` on the pk `id` is refused —
    /// the id must be returned in every response (the id-echo probe + every
    /// cross-scope test key on `body["id"]`), so response-hiding it breaks the
    /// generated suite by construction. The same flag on any OTHER field — even
    /// alongside `unique` — is fine (write_only is orthogonal to DB/input keys).
    #[test]
    fn explicit_write_only_on_the_pk_id_is_refused_with_jc0554() {
        let base = |id_extra: &str, token_extra: &str| -> Design {
            design(&format!(
                r#"{{ "name": "secrets-api", "contract_version": 1, "dependencies": ["db"],
                    "modules": [{{ "name": "accounts",
                        "entities": [{{ "name": "Account", "fields": [
                            {{ "name": "id", "type": "integer"{id_extra} }},
                            {{ "name": "api_token", "type": "string"{token_extra} }} ] }}],
                        "endpoints": [{{ "operation_id": "list_accounts", "method": "GET",
                            "path": "/",
                            "success": {{ "status": 200, "entity": "Account", "list": true }} }}]
                    }}] }}"#
            ))
        };
        // write_only on a NON-id field, even `unique`, is accepted (no refusal).
        let ok = base("", r#", "write_only": true, "unique": true"#);
        assert!(
            validate(&ok).is_empty(),
            "write_only on a normal field (even unique) is fine: {:?}",
            validate(&ok)
        );
        // Explicit write_only on the pk id → JC0554.
        let bad = base(r#", "write_only": true"#, "");
        let qs = validate(&bad);
        assert!(
            qs.iter()
                .any(|q| q.id == "/modules/0/entities/0/fields/0/write_only"
                    && q.question.contains("primary key")
                    && q.question.contains("JC0554")),
            "explicit write_only on id must raise JC0554: {qs:?}"
        );
    }

    /// testgen must materialize `"a".repeat(min_len)` — an unbounded `min_len`
    /// would emit a multi-megabyte fixture, so it is capped at 4096.
    #[test]
    fn min_len_above_the_fixture_ceiling_is_refused() {
        let qs = validate(&constraint_base(
            r#"{ "name": "body", "type": "string", "min_len": 4097 }"#,
        ));
        assert!(
            qs.iter()
                .any(|q| q.id == "/modules/0/entities/0/fields/0/min_len"
                    && q.question.contains("4096")
                    && q.question.contains("JC0552")),
            "{qs:?}"
        );
    }

    /// #80 (T3): a `unique` field needs room for the distinct values the
    /// generated suite materializes — the probe fixture plus the tenant-1 and
    /// tenant-2 seeds, i.e. up to THREE distinct in-range values per field. A
    /// range admitting fewer makes the design un-greenable by construction
    /// (the seeds would collide on the UNIQUE index), so it is refused.
    #[test]
    fn unique_with_a_range_below_three_values_is_refused() {
        for field in [
            r#"{ "name": "slots", "type": "integer", "unique": true, "min": 5, "max": 5 }"#,
            r#"{ "name": "slots", "type": "integer", "unique": true, "min": 1, "max": 2 }"#,
        ] {
            let qs = validate(&constraint_base(field));
            assert!(
                qs.iter()
                    .any(|q| q.id == "/modules/0/entities/0/fields/0/min"
                        && q.question.contains("unique")
                        && q.question.contains("JC0552")),
                "{field}: {qs:?}"
            );
        }
        // Three or more distinct values: clean (the seeds fit).
        let ok = constraint_base(
            r#"{ "name": "slots", "type": "integer", "unique": true, "min": 1, "max": 600 },
               { "name": "trio", "type": "integer", "unique": true, "min": 1, "max": 3 }"#,
        );
        assert!(validate(&ok).is_empty(), "{:?}", validate(&ok));
        // Without `unique` a single-value range is legal (no distinctness need).
        let ok = constraint_base(r#"{ "name": "slots", "type": "integer", "min": 5, "max": 5 }"#);
        assert!(validate(&ok).is_empty(), "{:?}", validate(&ok));
    }

    /// #80 (T4): the cardinality rule must hold with a SINGLE declared bound
    /// too — the absent bound substitutes its i64 extreme. A `unique` field
    /// with only `min: i64::MAX - 1` admits two representable values, so the
    /// distinct seed derivation collides exactly as with a written `max`; the
    /// pre-fix arm required BOTH bounds and let this un-greenable design
    /// validate clean.
    #[test]
    fn unique_with_a_single_bound_near_the_extreme_is_refused() {
        // min-only at the top extreme: [i64::MAX - 1, i64::MAX] = 2 values.
        let qs = validate(&constraint_base(
            r#"{ "name": "slots", "type": "integer", "unique": true, "min": 9223372036854775806 }"#,
        ));
        assert!(
            qs.iter()
                .any(|q| q.id == "/modules/0/entities/0/fields/0/min"
                    && q.question.contains("unique")
                    && q.question.contains("JC0552")),
            "{qs:?}"
        );
        // max-only at the bottom extreme: [i64::MIN, i64::MIN + 1] = 2 values.
        let qs = validate(&constraint_base(
            r#"{ "name": "slots", "type": "integer", "unique": true, "max": -9223372036854775807 }"#,
        ));
        assert!(
            qs.iter()
                .any(|q| q.id == "/modules/0/entities/0/fields/0/max"
                    && q.question.contains("unique")
                    && q.question.contains("JC0552")),
            "{qs:?}"
        );
        // An open-ended single bound leaves astronomic room: clean.
        let ok =
            constraint_base(r#"{ "name": "slots", "type": "integer", "unique": true, "min": 1 }"#);
        assert!(validate(&ok).is_empty(), "{:?}", validate(&ok));
    }

    /// #80 (T3): the string twin — `max_len: 0` admits ONLY the empty string,
    /// so a `unique` field can never seed distinct values. Any `max_len >= 1`
    /// stays clean: the seed derivations lead with distinct characters
    /// ('t'/'s'/a digit), so even one code point suffices.
    #[test]
    fn unique_with_max_len_zero_is_refused() {
        let qs = validate(&constraint_base(
            r#"{ "name": "slug", "type": "string", "unique": true, "max_len": 0, "required": false }"#,
        ));
        assert!(
            qs.iter()
                .any(|q| q.id == "/modules/0/entities/0/fields/0/max_len"
                    && q.question.contains("unique")
                    && q.question.contains("JC0552")),
            "{qs:?}"
        );
        let ok = constraint_base(
            r#"{ "name": "slug", "type": "string", "unique": true, "max_len": 1 }"#,
        );
        assert!(validate(&ok).is_empty(), "{:?}", validate(&ok));
    }

    /// A server-owned `default` is written verbatim, so it must satisfy the
    /// field's OWN constraints — an out-of-bounds default would plant a value
    /// the declared bound forbids on every defaulted row.
    #[test]
    fn default_must_satisfy_the_fields_own_constraints() {
        let qs = validate(&constraint_base(
            r#"{ "name": "quantity", "type": "integer", "min": 1, "max": 600, "default": 601 }"#,
        ));
        assert!(
            qs.iter()
                .any(|q| q.id == "/modules/0/entities/0/fields/0/default"
                    && q.question.contains("range")
                    && q.question.contains("JC0552")),
            "{qs:?}"
        );
        let qs = validate(&constraint_base(
            r#"{ "name": "bio", "type": "string", "max_len": 3, "default": "toolong" }"#,
        ));
        assert!(
            qs.iter()
                .any(|q| q.id == "/modules/0/entities/0/fields/0/default"
                    && q.question.contains("length")
                    && q.question.contains("JC0552")),
            "{qs:?}"
        );
        // In-range defaults stay clean — length counts CODE POINTS, not bytes.
        let ok = constraint_base(
            r#"{ "name": "quantity", "type": "integer", "min": 1, "max": 600, "default": 42 },
               { "name": "bio", "type": "string", "max_len": 5, "default": "héllo" }"#,
        );
        assert!(validate(&ok).is_empty(), "{:?}", validate(&ok));
    }

    #[test]
    fn explicit_fk_named_field_conflicts_with_belongs_to() {
        let mut d: Design = serde_json::from_str(V1_FULL).unwrap();
        d.modules[1].entities[0].fields.push(Field {
            name: "workspace_id".into(),
            field_type: FieldType::Integer,
            required: true,
            unique: false,
            index: false,
            values: None,
            default: None,
            min: None,
            max: None,
            min_len: None,
            max_len: None,
            write_only: false,
        });
        assert!(
            validate(&d)
                .iter()
                .any(|q| q.id.ends_with("/fields/3") && q.question.contains("derived")),
            "{:?}",
            validate(&d)
        );
    }

    #[test]
    fn public_endpoint_cannot_also_be_auth_required() {
        // A public endpoint that also demands auth contradicts itself: `public`
        // is the JL0004 carve-out for genuinely unauthenticated routes (login/
        // register), so combining it with a guard is a design error. WHY (Rule 9):
        // the flag exists to mark a route as needing NO credential — a guarded
        // public route would silently re-trip the very lint it claims exemption from.
        let mut v: serde_json::Value = serde_json::from_str(MINIMAL).unwrap();
        v["modules"][0]["endpoints"][1]["public"] = serde_json::json!(true);
        v["modules"][0]["endpoints"][1]["auth_required"] = serde_json::json!(true);
        let d: Design = serde_json::from_value(v).unwrap();
        let qs = validate(&d);
        assert!(
            qs.iter().any(|q| q.id == "/modules/0/endpoints/1"
                && q.question.contains("public")
                && q.question.contains("auth_required")),
            "{qs:?}"
        );
    }

    #[test]
    fn public_endpoint_cannot_require_roles() {
        let mut v: serde_json::Value = serde_json::from_str(MINIMAL).unwrap();
        v["modules"][0]["endpoints"][2]["public"] = serde_json::json!(true);
        // endpoints[2] (delete_todo) already declares required_roles: ["admin"].
        let d: Design = serde_json::from_value(v).unwrap();
        let qs = validate(&d);
        assert!(
            qs.iter().any(|q| q.id == "/modules/0/endpoints/2"
                && q.question.contains("public")
                && q.question.contains("required_roles")),
            "{qs:?}"
        );
    }

    #[test]
    fn reference_shaped_v1_design_is_question_free() {
        let d = guard_reads(serde_json::from_str(V1_FULL).unwrap());
        assert!(validate(&d).is_empty(), "{:?}", validate(&d));
    }

    #[test]
    fn public_endpoint_on_tenant_owned_entity_is_rejected() {
        // A public endpoint skips every guard — including the Tenant guard that
        // scopes a tenant-owned entity to its owner. WHY (Rule 9): marking such an
        // endpoint public would expose one tenant's rows to anyone, silently
        // defeating tenancy; the design must not be able to claim that exemption
        // on an entity that belongs_to the tenancy root.
        let mut d: Design = serde_json::from_str(V1_FULL).unwrap();
        // V1_FULL module[1] is `leads`; its `Lead` entity belongs_to Workspace
        // (the tenancy entity), and list_leads resolves to Lead via success.entity.
        d.modules[1].endpoints[0].public = true;
        let qs = validate(&d);
        assert!(
            qs.iter().any(|q| q.id == "/modules/1/endpoints/0"
                && q.question.contains("public")
                && q.question.contains("tenant-owned")),
            "{qs:?}"
        );
    }

    #[test]
    fn public_endpoints_on_non_tenant_owned_entities_do_not_false_positive() {
        // The reference-slice north-star design has public register/login in the
        // `users` module; User is NOT tenant-owned, so the resolution (request_body
        // entity for register, first-entity fallback for login) must not flag them.
        let reference = include_str!("../../../../conformance/designs/reference-slice.design.json");
        let d: Design = serde_json::from_str(reference).unwrap();
        let qs = validate(&d);
        assert!(
            qs.is_empty(),
            "reference-slice must validate question-free; public users endpoints must not false-positive: {qs:?}"
        );
    }

    // ---- #65 (JC0542): sibling routes with conflicting path-param names -------

    const CONFORMANCE_REFERENCE: &str =
        include_str!("../../../../conformance/designs/reference-slice.design.json");
    const CONFORMANCE_TODO: &str =
        include_str!("../../../../conformance/designs/todo-api.design.json");

    /// The HelpDesk repro (hit by 4/5 eval builds): `/{id}` and `/{ticket_id}/comments`
    /// in one module share segment position 2 but name its param differently. The
    /// runtime router (one global trie) aborts `App::build` with JC0500 after a clean
    /// scaffold; `design_conflict` must reject it up front with JC0542 naming BOTH
    /// routes, BOTH names, and BOTH remedies (unify / restructure).
    #[test]
    fn sibling_routes_with_different_param_names_are_a_conflict() {
        let d: Design = serde_json::from_str(
            r#"{
            "name": "helpdesk", "contract_version": 1, "dependencies": ["db"],
            "modules": [{
                "name": "tickets",
                "entities": [
                    { "name": "Ticket", "fields": [{ "name": "subject", "type": "string" }] },
                    { "name": "Comment", "fields": [{ "name": "body", "type": "string" }] }
                ],
                "endpoints": [
                    { "operation_id": "show_ticket", "method": "GET", "path": "/{id}",
                      "success": { "status": 200, "entity": "Ticket" } },
                    { "operation_id": "list_comments", "method": "GET", "path": "/{ticket_id}/comments",
                      "success": { "status": 200, "entity": "Comment", "list": true } }
                ]
            }]
        }"#,
        )
        .unwrap();
        let c = design_conflict(&d).expect("mismatched sibling param names must be a conflict");
        assert_eq!(c.code, "JC0542");
        assert!(
            c.message.contains("/tickets/{id}")
                && c.message.contains("/tickets/{ticket_id}/comments"),
            "names both conflicting routes: {}",
            c.message
        );
        assert!(
            c.message.contains("{id}") && c.message.contains("{ticket_id}"),
            "names both param names: {}",
            c.message
        );
        assert!(
            c.message.to_lowercase().contains("unify")
                && c.message.to_lowercase().contains("restructure"),
            "names both remedies: {}",
            c.message
        );
        assert!(!c.hint.is_empty());
    }

    /// The router accepts several sibling shapes the validator must NOT reject:
    /// (a) the SAME param name at a shared position (`/{id}` + `/{id}/comments`),
    /// (b) a literal vs a param at a position (`/{id}` + `/archive` — distinct trie
    /// children), and (c) a param-carrying subroute mount whose name is consistent
    /// with the parent. Plus BOTH shipped conformance designs.
    #[test]
    fn consistent_and_divergent_sibling_paths_are_not_conflicts() {
        // (a) same param name at the shared position.
        let same: Design = serde_json::from_str(
            r#"{
            "name": "ok-same", "contract_version": 1, "dependencies": ["db"],
            "modules": [{ "name": "tickets",
                "entities": [{ "name": "Ticket", "fields": [{ "name": "s", "type": "string" }] }],
                "endpoints": [
                    { "operation_id": "show_ticket", "method": "GET", "path": "/{id}",
                      "success": { "status": 200, "entity": "Ticket" } },
                    { "operation_id": "list_comments", "method": "GET", "path": "/{id}/comments",
                      "success": { "status": 200 } }
                ] }]
        }"#,
        )
        .unwrap();
        assert!(
            design_conflict(&same).is_none(),
            "identical param names at the shared position are fine"
        );
        // (b) a literal segment vs a param at the same position diverges.
        let literal: Design = serde_json::from_str(
            r#"{
            "name": "ok-literal", "contract_version": 1, "dependencies": ["db"],
            "modules": [{ "name": "tickets",
                "entities": [{ "name": "Ticket", "fields": [{ "name": "s", "type": "string" }] }],
                "endpoints": [
                    { "operation_id": "show_ticket", "method": "GET", "path": "/{id}",
                      "success": { "status": 200, "entity": "Ticket" } },
                    { "operation_id": "list_archived", "method": "GET", "path": "/archive",
                      "success": { "status": 200 } }
                ] }]
        }"#,
        )
        .unwrap();
        assert!(
            design_conflict(&literal).is_none(),
            "a literal and a param at the same position are distinct trie children"
        );
        // (c) a param-carrying subroute mount whose param agrees with the parent.
        let mounted: Design = serde_json::from_str(
            r#"{
            "name": "ok-mount", "contract_version": 1, "dependencies": ["db"],
            "modules": [{ "name": "workspaces", "mount": "/ws",
                "entities": [{ "name": "Ws", "fields": [{ "name": "s", "type": "string" }] }],
                "endpoints": [
                    { "operation_id": "show_ws", "method": "GET", "path": "/{id}",
                      "success": { "status": 200, "entity": "Ws" } }
                ],
                "subroutes": [{ "name": "leads", "mount": "/{id}/leads",
                    "endpoints": [
                        { "operation_id": "show_lead", "method": "GET", "path": "/{lead_id}",
                          "success": { "status": 200 } }
                    ] }]
            }]
        }"#,
        )
        .unwrap();
        assert!(
            design_conflict(&mounted).is_none(),
            "a param-mount child consistent with the parent must not be flagged: {:?}",
            design_conflict(&mounted).map(|c| c.message)
        );
        // Both shipped conformance designs (params all named `id`) stay clean.
        for src in [CONFORMANCE_REFERENCE, CONFORMANCE_TODO] {
            let d: Design = serde_json::from_str(src).unwrap();
            assert!(
                design_conflict(&d).is_none(),
                "conformance design must not trip JC0542: {:?}",
                design_conflict(&d).map(|c| c.message)
            );
        }
    }

    // ---- #107: the implicit member routes join the JC0542 walk ----------------

    /// A minimal Club-tenancy design (db + auth, so the member surface is
    /// emitted) whose tenant module has one detail endpoint at `detail_path`.
    fn club_tenancy(detail_path: &str) -> Design {
        serde_json::from_str(&format!(
            r#"{{
            "name": "clubs-api", "contract_version": 1,
            "auth": {{ "model": "session", "roles": ["owner", "member"] }},
            "dependencies": ["db", "auth"],
            "tenancy": {{ "entity": "Club", "member_roles": ["owner", "member"] }},
            "modules": [{{
                "name": "clubs",
                "entities": [{{ "name": "Club", "fields": [
                    {{ "name": "id", "type": "integer" }},
                    {{ "name": "slug", "type": "string" }} ]}}],
                "endpoints": [
                    {{ "operation_id": "show_club", "method": "GET", "path": "{detail_path}",
                      "auth_required": true,
                      "success": {{ "status": 200, "entity": "Club" }} }}
                ]
            }}]
        }}"#
        ))
        .unwrap()
    }

    /// A tenant module with a CUSTOM-param endpoint — `GET /{slug}` where the
    /// conventional `/{id}` would have been normalized to `/{club_id}`. The
    /// tool-owned member routes register `/{club_id}/members…` at `App::build`,
    /// so `{slug}` vs `{club_id}` at the shared position aborts startup with
    /// JC0500 — previously AFTER a clean check, breaking the "green check ⇒ it
    /// runs" promise. The walk must include the member routes and reject the
    /// design up front with JC0542 naming both routes.
    #[test]
    fn tenant_custom_param_collides_with_the_member_routes() {
        let c = design_conflict(&club_tenancy("/{slug}"))
            .expect("a custom tenant param must conflict with the member routes");
        assert_eq!(c.code, "JC0542");
        assert!(
            c.message.contains("/clubs/{slug}") && c.message.contains("/clubs/{club_id}/members"),
            "names the design route AND the member route: {}",
            c.message
        );
        assert!(
            c.message.contains("{slug}") && c.message.contains("{club_id}"),
            "names both param names: {}",
            c.message
        );
        // #88: the SAME `/{slug}` design is also refused by validate() — it is
        // the tenant's own detail route addressed by a non-pk param (JC0550).
        // Both codes are legitimate: JC0542 names the router conflict with the
        // member routes, JC0550 the unverifiable membership.
        let qs = validate(&club_tenancy("/{slug}"));
        assert!(
            qs.iter().any(|q| q.question.contains("JC0550")),
            "a db+auth `/{{slug}}` tenant detail route must also trip JC0550: {qs:?}"
        );
    }

    /// The conventional shape stays green: a tenant detail route written as
    /// `/{id}` is load-normalized to `/{club_id}` (`normalize_tenant_detail_routes`),
    /// which AGREES with the member routes' param — the walk must apply the same
    /// normalization the router sees, so the design passes exactly when
    /// `App::build` succeeds (no false positive on every ordinary tenancy app).
    #[test]
    fn normalized_tenant_detail_route_agrees_with_the_member_routes() {
        assert!(
            design_conflict(&club_tenancy("/{id}")).is_none(),
            "the normalized `/{{club_id}}` detail route must not conflict: {:?}",
            design_conflict(&club_tenancy("/{id}")).map(|c| c.message)
        );
        // Same param name written explicitly: also fine.
        assert!(
            design_conflict(&club_tenancy("/{club_id}")).is_none(),
            "an explicit `/{{club_id}}` detail route must not conflict"
        );
    }

    /// A design endpoint OCCUPYING a reserved member path (`GET /{club_id}/members`)
    /// is a second `.route()` registration of the same path — `App::build` aborts
    /// with JC0500 `duplicate route registration` (methods don't disambiguate).
    /// JC0542 must reject it at design time, naming the reserved surface.
    #[test]
    fn design_endpoint_on_a_reserved_member_path_is_a_conflict() {
        let c = design_conflict(&club_tenancy("/{club_id}/members"))
            .expect("a design endpoint on the reserved member path must conflict");
        assert_eq!(c.code, "JC0542");
        assert!(
            c.message.contains("/clubs/{club_id}/members")
                && c.message.contains("member-management"),
            "names the reserved path and the member surface: {}",
            c.message
        );
        assert!(
            c.message.contains("duplicate route registration"),
            "states the startup failure it prevents: {}",
            c.message
        );
        assert!(!c.hint.is_empty());
    }

    /// #140: SLASH-VARIANTS of a reserved member path — `/{club_id}/members/`
    /// (the natural hand-rolled shape: collection routes are trailing-slash by
    /// convention), a doubled-slash spelling, and the item path with a trailing
    /// slash. The router drops empty path segments (`segments()` filters them),
    /// so each spelling registers the SAME trie node as the member surface and
    /// `App::build` aborts with JC0500 `duplicate route registration` —
    /// previously AFTER a clean check (the occupancy compare was raw-string).
    /// The check must compare segment vectors and reject every spelling up
    /// front with JC0542.
    #[test]
    fn slash_variants_of_a_reserved_member_path_are_a_conflict() {
        for shape in [
            "/{club_id}/members/",
            "/{club_id}//members",
            "/{club_id}/members/{user_id}/",
        ] {
            let c = design_conflict(&club_tenancy(shape))
                .unwrap_or_else(|| panic!("`{shape}` must conflict with the member surface"));
            assert_eq!(c.code, "JC0542", "`{shape}` must be JC0542");
            assert!(
                c.message.contains("member-management"),
                "`{shape}` must name the member surface: {}",
                c.message
            );
        }
    }

    // ---- #88 (JC0550): a non-pk tenant detail param is refused -----------------

    /// #88: a tenant entity's OWN detail route addressed by a non-pk param
    /// survives normalization (only the literal `{id}` is renamed), the
    /// membership guard cannot bind an fk, and the handler was generated with
    /// a bare `CurrentUser` and NO membership check at all — silently. JC0550
    /// turns that silent hole into a design-time refusal naming the operation,
    /// the offending param, and the fk. Covers BOTH the first-position
    /// `/{slug}` (which db+auth JC0542 also catches via the member routes) and
    /// the non-first-position `/by-slug/{slug}`, which JC0542 cannot see (the
    /// static segment diverges from `{club_id}` in the trie) — the reachable
    /// silent hole.
    #[test]
    fn tenant_own_non_pk_detail_param_is_refused_with_jc0550() {
        for path in ["/{slug}", "/by-slug/{slug}"] {
            let qs = validate(&club_tenancy(path));
            let hit = qs
                .iter()
                .find(|q| q.question.contains("JC0550"))
                .unwrap_or_else(|| panic!("`{path}` must trip JC0550: {qs:?}"));
            assert!(
                hit.question.contains("show_club")
                    && hit.question.contains("GET")
                    && hit.question.contains("`Club`")
                    && hit.question.contains("{slug}")
                    && hit.question.contains("{club_id}"),
                "JC0550 must name the operation, method, tenant, param, and fk: {}",
                hit.question
            );
        }
    }

    /// The pk shapes stay green: the conventional `/{id}` (load-normalized to
    /// `/{club_id}`) and the explicit `/{club_id}` both address the tenant by
    /// its pk, so the guard binds the fk by name and JC0550 must not fire —
    /// every ordinary tenancy app validates exactly as before.
    #[test]
    fn tenant_pk_detail_routes_do_not_trip_jc0550() {
        for path in ["/{id}", "/{club_id}"] {
            let qs = validate(&club_tenancy(path));
            assert!(
                !qs.iter().any(|q| q.question.contains("JC0550")),
                "`{path}` must not trip JC0550: {qs:?}"
            );
        }
    }

    /// A tenant-strict endpoint whose fk arrives via the SUBROUTE MOUNT
    /// (mount `/{club_id}/history`, path `/{year}`, success = tenant) is
    /// fully guard-bound: `endpoint_tenant_shape` resolves the route as
    /// `mount + path`, sees the fk, classifies it PathScoped, and genroute
    /// emits the membership-checking `Dep<Tenant>` from the full route. WHY
    /// (Rule 9): JC0550 matching `ep.path` alone falsely refused this shape
    /// (fail-closed, but a refusal of a correct design) — the predicate must
    /// match the same mount-resolved path the guard actually binds.
    #[test]
    fn mount_carried_tenant_fk_does_not_trip_jc0550() {
        let d: Design = serde_json::from_str(
            r#"{ "name": "clubs-api", "contract_version": 1,
                "auth": { "model": "session", "roles": ["owner", "member"] },
                "dependencies": ["db", "auth"],
                "tenancy": { "entity": "Club", "member_roles": ["owner", "member"] },
                "modules": [
                    { "name": "clubs",
                      "entities": [{ "name": "Club", "fields": [
                          { "name": "name", "type": "string" } ]}],
                      "endpoints": [
                          { "operation_id": "get_club", "method": "GET", "path": "/{id}",
                            "auth_required": true,
                            "success": { "status": 200, "entity": "Club" } } ],
                      "subroutes": [
                          { "name": "history", "mount": "/{club_id}/history",
                            "entities": [{ "name": "Snapshot",
                                "belongs_to": [{ "entity": "Club" }],
                                "fields": [{ "name": "year", "type": "integer" }] }],
                            "endpoints": [
                                { "operation_id": "get_club_year", "method": "GET",
                                  "path": "/{year}", "auth_required": true,
                                  "success": { "status": 200, "entity": "Club" } } ] } ] }
                ] }"#,
        )
        .unwrap();
        let qs = validate(&d);
        assert!(
            !qs.iter().any(|q| q.question.contains("JC0550")),
            "a mount-carried tenant fk is guard-bound from the full route — \
             JC0550 must not refuse it: {qs:?}"
        );
    }

    /// A CHILD entity's `/{slug}` detail route is NOT the tenant's own detail
    /// route — its handler binds the child's repo, and child scoping is
    /// JL0006's domain, not the tenant guard's fk binding — so JC0550 must
    /// stay silent (only the tenant's OWN detail route is refused).
    #[test]
    fn child_entity_slug_detail_route_does_not_trip_jc0550() {
        let d: Design = serde_json::from_str(
            r#"{ "name": "clubs-api", "contract_version": 1,
                "auth": { "model": "session", "roles": ["owner", "member"] },
                "dependencies": ["db", "auth"],
                "tenancy": { "entity": "Club", "member_roles": ["owner", "member"] },
                "modules": [
                    { "name": "clubs",
                      "entities": [{ "name": "Club", "fields": [
                          { "name": "id", "type": "integer" },
                          { "name": "name", "type": "string" } ]}],
                      "endpoints": [
                          { "operation_id": "get_club", "method": "GET", "path": "/{id}",
                            "auth_required": true,
                            "success": { "status": 200, "entity": "Club" } } ] },
                    { "name": "books",
                      "entities": [{ "name": "Book",
                          "belongs_to": [{ "entity": "Club" }],
                          "fields": [{ "name": "id", "type": "integer" },
                                     { "name": "slug", "type": "string" }] }],
                      "endpoints": [
                          { "operation_id": "get_book", "method": "GET", "path": "/{slug}",
                            "auth_required": true,
                            "success": { "status": 200, "entity": "Book" } } ] }
                ] }"#,
        )
        .unwrap();
        let qs = validate(&d);
        assert!(
            !qs.iter().any(|q| q.question.contains("JC0550")),
            "a child entity's `/{{slug}}` detail route must not trip JC0550: {qs:?}"
        );
    }

    /// A SIBLING entity hosted in the tenant module with a bodyless
    /// `DELETE /trophies/{id}` (204, no `success.entity`) is NOT the tenant's
    /// own detail route: with a `POST /trophies` creator the strict resolver
    /// binds the SIBLING, and without one it binds no entity at all — JC0550
    /// must stay silent in both shapes. The module-local lenient resolver's
    /// first-entity fallback resolved the with-creator shape to the TENANT and
    /// falsely refused `{id}` — while the refusal's own message says to use
    /// `/{id}`.
    #[test]
    fn sibling_bodyless_delete_in_tenant_module_does_not_trip_jc0550() {
        for with_creator in [true, false] {
            let creator = if with_creator {
                r#"{ "operation_id": "create_trophy", "method": "POST", "path": "/trophies",
                     "request_body": { "entity": "Trophy" },
                     "success": { "status": 201, "entity": "Trophy" } },"#
            } else {
                ""
            };
            let d: Design = serde_json::from_str(&format!(
                r#"{{ "name": "clubs-api", "contract_version": 1,
                    "auth": {{ "model": "session", "roles": ["owner", "member"] }},
                    "dependencies": ["db", "auth"],
                    "tenancy": {{ "entity": "Club", "member_roles": ["owner", "member"] }},
                    "modules": [{{
                        "name": "clubs",
                        "entities": [
                            {{ "name": "Club", "fields": [
                                {{ "name": "id", "type": "integer" }},
                                {{ "name": "name", "type": "string" }} ]}},
                            {{ "name": "Trophy",
                               "belongs_to": [{{ "entity": "Club" }}],
                               "fields": [
                                {{ "name": "id", "type": "integer" }},
                                {{ "name": "title", "type": "string" }} ]}}],
                        "endpoints": [
                            {{ "operation_id": "get_club", "method": "GET", "path": "/{{id}}",
                               "auth_required": true,
                               "success": {{ "status": 200, "entity": "Club" }} }},
                            {creator}
                            {{ "operation_id": "delete_trophy", "method": "DELETE",
                               "path": "/trophies/{{id}}", "auth_required": true,
                               "success": {{ "status": 204 }} }}
                        ]
                    }}]
                }}"#
            ))
            .unwrap();
            let qs = validate(&d);
            assert!(
                !qs.iter().any(|q| q.question.contains("JC0550")),
                "a sibling's bodyless `DELETE /trophies/{{id}}` (creator present: {with_creator}) must not trip JC0550: {qs:?}"
            );
        }
    }

    /// The under-fire hole, closed: the tenant declared as a NON-FIRST entity
    /// in its module, with a bodyless `DELETE /{slug}` whose collection creator
    /// (`POST /`, body = tenant) IS the tenant. The lenient first-entity
    /// fallback resolved it to the sibling and shipped the no-membership-check
    /// hole question-free; the strict creator arm resolves the tenant and
    /// refuses. Asserted for db+auth AND db-less/memory-mode tenancy — in
    /// memory mode there is no implicit member surface, so JC0542 cannot catch
    /// the shape and JC0550 is the ONLY design-time refusal.
    #[test]
    fn non_first_tenant_bodyless_slug_delete_trips_jc0550() {
        for deps in [r#"["db", "auth"]"#, r#"["auth"]"#] {
            let d: Design = serde_json::from_str(&format!(
                r#"{{ "name": "clubs-api", "contract_version": 1,
                    "auth": {{ "model": "session", "roles": ["owner", "member"] }},
                    "dependencies": {deps},
                    "tenancy": {{ "entity": "Club", "member_roles": ["owner", "member"] }},
                    "modules": [{{
                        "name": "clubs",
                        "entities": [
                            {{ "name": "Trophy",
                               "belongs_to": [{{ "entity": "Club" }}],
                               "fields": [
                                {{ "name": "id", "type": "integer" }},
                                {{ "name": "title", "type": "string" }} ]}},
                            {{ "name": "Club", "fields": [
                                {{ "name": "id", "type": "integer" }},
                                {{ "name": "slug", "type": "string" }} ]}}],
                        "endpoints": [
                            {{ "operation_id": "create_club", "method": "POST", "path": "/",
                               "request_body": {{ "entity": "Club" }},
                               "success": {{ "status": 201, "entity": "Club" }} }},
                            {{ "operation_id": "delete_club", "method": "DELETE", "path": "/{{slug}}",
                               "auth_required": true,
                               "success": {{ "status": 204 }} }}
                        ]
                    }}]
                }}"#
            ))
            .unwrap();
            let qs = validate(&d);
            let hit = qs
                .iter()
                .find(|q| q.question.contains("JC0550"))
                .unwrap_or_else(|| {
                    panic!(
                        "deps {deps}: a non-first tenant's bodyless `DELETE /{{slug}}` must trip JC0550: {qs:?}"
                    )
                });
            assert!(
                hit.question.contains("delete_club")
                    && hit.question.contains("{slug}")
                    && hit.question.contains("{club_id}"),
                "JC0550 must name the operation, param, and fk: {}",
                hit.question
            );
        }
    }

    /// An entity-less CUSTOM endpoint in a single-entity tenant module — a
    /// `GET /export/{format}` with a custom-JSON success (no `success.entity`,
    /// no body, no creator at `/export`) — reads NO entity's repo, so it is
    /// not the tenant's detail route: strict resolution returns `None` and
    /// JC0550 must stay silent. The lenient fallback tied it to the tenant and
    /// falsely refused `{format}` as an unverifiable tenant param.
    #[test]
    fn entity_less_custom_get_does_not_trip_jc0550() {
        let d: Design = serde_json::from_str(
            r#"{ "name": "clubs-api", "contract_version": 1,
                "auth": { "model": "session", "roles": ["owner", "member"] },
                "dependencies": ["db", "auth"],
                "tenancy": { "entity": "Club", "member_roles": ["owner", "member"] },
                "modules": [{
                    "name": "clubs",
                    "entities": [{ "name": "Club", "fields": [
                        { "name": "id", "type": "integer" },
                        { "name": "name", "type": "string" } ]}],
                    "endpoints": [
                        { "operation_id": "create_club", "method": "POST", "path": "/",
                          "request_body": { "entity": "Club" },
                          "success": { "status": 201, "entity": "Club" } },
                        { "operation_id": "export_clubs", "method": "GET",
                          "path": "/export/{format}", "auth_required": true,
                          "success": { "status": 200 } }
                    ]
                }]
            }"#,
        )
        .unwrap();
        let qs = validate(&d);
        assert!(
            !qs.iter().any(|q| q.question.contains("JC0550")),
            "an entity-less custom `GET /export/{{format}}` must not trip JC0550: {qs:?}"
        );
    }

    /// A multi-param tenant route that CARRIES the fk — `GET /{club_id}/{version}`
    /// on the tenant — binds the fk (the guard membership-checks the tenant
    /// named by it), so JC0550 must not fire: the predicate is "no fk among the
    /// path params", not "the trailing param is not the fk".
    #[test]
    fn multi_param_tenant_route_with_fk_present_does_not_trip_jc0550() {
        let d: Design = serde_json::from_str(
            r#"{ "name": "clubs-api", "contract_version": 1,
                "auth": { "model": "session", "roles": ["owner", "member"] },
                "dependencies": ["db", "auth"],
                "tenancy": { "entity": "Club", "member_roles": ["owner", "member"] },
                "modules": [{
                    "name": "clubs",
                    "entities": [{ "name": "Club", "fields": [
                        { "name": "id", "type": "integer" },
                        { "name": "name", "type": "string" } ]}],
                    "endpoints": [
                        { "operation_id": "get_club_version", "method": "GET",
                          "path": "/{club_id}/{version}", "auth_required": true,
                          "success": { "status": 200, "entity": "Club" } }
                    ]
                }]
            }"#,
        )
        .unwrap();
        let qs = validate(&d);
        assert!(
            !qs.iter().any(|q| q.question.contains("JC0550")),
            "a multi-param tenant route carrying the fk must not trip JC0550: {qs:?}"
        );
    }

    /// #140: two spellings of ONE route — `GET /archive` and `POST /archive/`
    /// — emit two `.route()` lines (`route_lines` groups by RAW path), which
    /// the router collapses onto one trie node: the second registration aborts
    /// `App::build` with JC0500 `duplicate route registration`, after a clean
    /// check. The twin must mirror the trie's endpoint occupancy so the design
    /// fails `check` up front, naming both spellings.
    #[test]
    fn slash_variant_duplicate_design_routes_are_a_conflict() {
        let d: Design = serde_json::from_str(
            r#"{
            "name": "dup-slash", "contract_version": 1, "dependencies": ["db"],
            "modules": [{ "name": "tickets",
                "entities": [{ "name": "Ticket", "fields": [{ "name": "s", "type": "string" }] }],
                "endpoints": [
                    { "operation_id": "list_archived", "method": "GET", "path": "/archive",
                      "success": { "status": 200 } },
                    { "operation_id": "archive_ticket", "method": "POST", "path": "/archive/",
                      "success": { "status": 201 } }
                ] }]
        }"#,
        )
        .unwrap();
        let c = design_conflict(&d).expect("slash-variant duplicate routes must be a conflict");
        assert_eq!(c.code, "JC0542");
        assert!(
            c.message.contains("`/tickets/archive`") && c.message.contains("`/tickets/archive/`"),
            "names both spellings: {}",
            c.message
        );
        assert!(
            c.message.contains("duplicate route registration"),
            "states the startup failure it prevents: {}",
            c.message
        );
        assert!(!c.hint.is_empty());
    }

    // ---- #54 (JC0543): enum value content ------------------------------------

    /// An enum value with a space (or quote/backslash) breaks the UNESCAPED
    /// interpolation into generated Rust; validation rejects it at design time with
    /// JC0543 guidance, naming the offending value. Identifier-shaped values
    /// (letters, digits, `_`, `-`) pass.
    #[test]
    fn enum_values_must_be_identifier_shaped() {
        // V1_FULL module[0]=workspaces, entity[0]=Workspace, field[1]=plan (string enum).
        let mut bad: Design = serde_json::from_str(V1_FULL).unwrap();
        bad.modules[0].entities[0].fields[1].values = Some(vec!["in progress".into()]);
        assert!(
            validate(&bad)
                .iter()
                .any(|q| q.id == "/modules/0/entities/0/fields/1/values"
                    && q.question.contains("JC0543")
                    && q.question.contains("in progress")),
            "a space-bearing enum value must be rejected: {:?}",
            validate(&bad)
        );
        // A quote is rejected too (the direct interpolation footgun).
        let mut quoted: Design = serde_json::from_str(V1_FULL).unwrap();
        quoted.modules[0].entities[0].fields[1].values = Some(vec!["a\"b".into()]);
        assert!(
            validate(&quoted)
                .iter()
                .any(|q| q.id.ends_with("/values") && q.question.contains("JC0543"))
        );
        // `-` and `_` and mixed case are legitimate identifier shapes.
        let mut ok: Design = serde_json::from_str(V1_FULL).unwrap();
        ok.modules[0].entities[0].fields[1].values =
            Some(vec!["in-progress".into(), "on_hold".into(), "Done2".into()]);
        assert!(
            !validate(&ok).iter().any(|q| q.id.ends_with("/values")),
            "identifier-shaped values must pass: {:?}",
            validate(&ok)
        );
        // Both shipped conformance designs' enum values stay clean.
        for src in [CONFORMANCE_REFERENCE, CONFORMANCE_TODO] {
            let d: Design = serde_json::from_str(src).unwrap();
            assert!(
                !validate(&d).iter().any(|q| q.question.contains("JC0543")),
                "conformance enum values must not trip JC0543"
            );
        }
    }

    // ---- #60 (JC0544): dual-create path-fk omission --------------------------

    /// The dual-create shape from issue #60: `Checkin belongs_to Habit`, with a
    /// nested `POST /{habit_id}/checkins` AND a standalone `POST /checkins`. The
    /// per-entity `CheckinRequest` drops `habit_id` for both, so the standalone
    /// route can set the NOT-NULL fk from neither the body nor the path — it is
    /// un-implementable. Validation flags ONLY the standalone route with JC0544,
    /// naming the route and BOTH fixes; the nested route (which carries the param)
    /// is left alone.
    #[test]
    fn dual_create_standalone_route_missing_path_fk_is_flagged() {
        let d: Design = serde_json::from_str(DUAL_CREATE).unwrap();
        let qs = validate(&d);
        let flagged: Vec<&Question> = qs
            .iter()
            .filter(|q| q.question.contains("JC0544"))
            .collect();
        assert_eq!(
            flagged.len(),
            1,
            "only the standalone POST /checkins is un-implementable: {qs:?}"
        );
        let f = flagged[0];
        assert!(
            f.question.contains("create_checkin_flat") && f.question.contains("habit_id"),
            "names the un-implementable route and the fk: {}",
            f.question
        );
        assert!(
            f.question.to_lowercase().contains("split") && f.question.contains("{habit_id}"),
            "names both fixes (add the path param / split the entity): {}",
            f.question
        );
    }

    /// A nested-ONLY create (`POST /{habit_id}/checkins`, no standalone) carries the
    /// fk in its path, so it is implementable and must NOT be flagged — and both
    /// shipped conformance designs (no dual-create shape) stay JC0544-free.
    #[test]
    fn nested_only_create_and_conformance_shapes_do_not_trip_dual_create() {
        let nested: Design = serde_json::from_str(NESTED_ONLY).unwrap();
        assert!(
            !validate(&nested)
                .iter()
                .any(|q| q.question.contains("JC0544")),
            "a nested-only create carries its fk in the path: {:?}",
            validate(&nested)
        );
        for src in [CONFORMANCE_REFERENCE, CONFORMANCE_TODO] {
            let d: Design = serde_json::from_str(src).unwrap();
            assert!(
                !validate(&d).iter().any(|q| q.question.contains("JC0544")),
                "conformance design must not trip JC0544"
            );
        }
    }

    /// Issue #82 + #125-create: a tenant child mounted at `/clubs/{club_id}` whose
    /// create is `POST /` carries its parent fk in the MOUNT. Now that
    /// `entity_path_fk_columns` is mount-aware it reports `club_id` as path-redundant
    /// — but the RESOLVED path DOES carry `{club_id}`, so the route IS implementable
    /// (the handler injects the path/tenant value). JC0544 must resolve the mount the
    /// same way and NOT flag it; otherwise `require_complete` would block the exact
    /// nested-mount app 0.5.2 blesses. (A mount-BLIND JC0544 would fire here because
    /// `ep.path == "/"` lacks `{club_id}` even though the mount supplies it.)
    #[test]
    fn nested_mount_create_carries_fk_in_the_mount_and_is_not_flagged() {
        let d: Design = serde_json::from_str(NESTED_MOUNT_CREATE).unwrap();
        assert!(
            !validate(&d).iter().any(|q| q.question.contains("JC0544")),
            "a mount-nested create carries its fk in the RESOLVED path, so it must not \
             trip JC0544: {:?}",
            validate(&d)
        );
    }

    // ---- #114 (JC0546): entity name collides with a prelude re-export ---------

    /// An entity named `Module` (a `jerrycan::prelude` re-export) makes the
    /// generated crate emit `pub struct Module` beside `use jerrycan::prelude::*;`
    /// and `use super::model::*;` — two glob imports of `Module`, so every
    /// reference is `E0659 ... is ambiguous` and the scaffold does not compile.
    /// `validate` must reject the design up front with JC0546 (fail-loud like
    /// JC0545), naming the entity, the reserved identifier, and the rename fix —
    /// so `jerrycan new` never scaffolds a crate that won't build.
    #[test]
    fn entity_named_after_a_prelude_reexport_is_rejected_with_jc0546() {
        // MINIMAL's PascalCase entity `Todo` (name + every reference) → `Module`.
        let d = design(&MINIMAL.replace("Todo", "Module"));
        let qs = validate(&d);
        let flagged: Vec<&Question> = qs
            .iter()
            .filter(|q| q.question.contains("JC0546"))
            .collect();
        assert_eq!(
            flagged.len(),
            1,
            "the single `Module` entity is flagged exactly once: {qs:?}"
        );
        let f = flagged[0];
        assert!(
            f.id.ends_with("/name"),
            "the question points at the entity's /name: {}",
            f.id
        );
        assert!(
            f.question.contains("Module")
                && f.question.to_lowercase().contains("prelude")
                && f.question.to_lowercase().contains("rename"),
            "names the entity, the reserved prelude identifier, and the rename fix: {}",
            f.question
        );
    }

    /// The default `Todo` name shadows nothing, and the shipped conformance
    /// designs use ordinary entity names — none may trip JC0546 (the negative
    /// control: the guard fires only on a real reserved-name collision).
    #[test]
    fn ordinary_entity_names_do_not_trip_jc0546() {
        assert!(
            !validate(&design(MINIMAL))
                .iter()
                .any(|q| q.question.contains("JC0546")),
            "an ordinary entity name is not a prelude collision"
        );
        for src in [CONFORMANCE_REFERENCE, CONFORMANCE_TODO] {
            let d: Design = serde_json::from_str(src).unwrap();
            assert!(
                !validate(&d).iter().any(|q| q.question.contains("JC0546")),
                "conformance design must not trip JC0546"
            );
        }
    }

    /// #129 drift tripwire: `RESERVED_PRELUDE_IDENTS` is a HAND-MAINTAINED mirror
    /// of the identifiers `jerrycan::prelude` re-exports (the glob every generated
    /// route crate writes: `use jerrycan::prelude::*;`). If a future `pub use` is
    /// added to the prelude but NOT to the set, JC0546 stops firing for it and an
    /// entity named after it silently reopens #114 — the scaffold emits an
    /// uncompilable `E0659` crate. This reads the prelude SOURCE (jerrycan-core's
    /// `pub mod prelude` + the facade's re-exported `main`) and asserts the set is
    /// a SUPERSET of every re-exported ident, so any drift fails CI. Test-only;
    /// like `embedded_sync`, it skips in a published tarball where the sibling
    /// core-crate source is absent.
    #[test]
    fn reserved_prelude_idents_is_a_superset_of_the_prelude_reexports() {
        use std::path::Path;

        // The body of the single `pub mod prelude { ... }` block, by brace match.
        fn prelude_body(src: &str) -> String {
            let start = src.find("pub mod prelude").expect("a prelude module");
            let open = start + src[start..].find('{').expect("prelude has a body");
            let mut depth = 0usize;
            for (i, b) in src[open..].bytes().enumerate() {
                match b {
                    b'{' => depth += 1,
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            return src[open + 1..open + i].to_string();
                        }
                    }
                    _ => {}
                }
            }
            panic!("unbalanced braces in the prelude module");
        }

        // (idents, globs) re-exported by a prelude body. Handles `pub use a::b::C;`,
        // `... as D;`, grouped `pub use a::{B, C as D};`, and glob `pub use x::*;`
        // (reported separately — a glob cannot be enumerated against the set).
        fn reexports(body: &str) -> (Vec<String>, Vec<String>) {
            fn push_item(
                item: &str,
                prefix: &str,
                idents: &mut Vec<String>,
                globs: &mut Vec<String>,
            ) {
                let item = item.trim();
                if item.is_empty() {
                    return;
                }
                if item == "*" {
                    globs.push(prefix.trim_end_matches("::").to_string());
                } else if let Some(base) = item.strip_suffix("::*") {
                    globs.push(format!("{prefix}{base}"));
                } else if let Some((_, rename)) = item.split_once(" as ") {
                    idents.push(rename.trim().to_string());
                } else {
                    idents.push(item.rsplit("::").next().unwrap().trim().to_string());
                }
            }
            // Strip line comments so a `//` in a doc line can't leak a token.
            let cleaned: String = body
                .lines()
                .map(|l| l.split_once("//").map_or(l, |(code, _)| code))
                .collect::<Vec<_>>()
                .join("\n");
            let mut idents = Vec::new();
            let mut globs = Vec::new();
            for stmt in cleaned.split(';') {
                let Some(rest) = stmt.trim().strip_prefix("pub use ") else {
                    continue;
                };
                let rest = rest.trim();
                if let Some(brace) = rest.find("::{") {
                    let prefix = &rest[..brace + 2]; // path up to and incl. the trailing "::"
                    let group = rest[brace + 3..].trim_end_matches('}');
                    for item in group.split(',') {
                        push_item(item, prefix, &mut idents, &mut globs);
                    }
                } else {
                    push_item(rest, "", &mut idents, &mut globs);
                }
            }
            (idents, globs)
        }

        let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR")); // crates/jerrycan
        let core_lib = crate_dir.join("../jerrycan-core/src/lib.rs");
        if !core_lib.exists() {
            return; // published tarball: the sibling core-crate source is absent
        }
        let facade_lib = crate_dir.join("src/lib.rs");
        let core_src = std::fs::read_to_string(&core_lib).unwrap();
        let facade_src = std::fs::read_to_string(&facade_lib).unwrap();

        let (core_idents, core_globs) = reexports(&prelude_body(&core_src));
        let (facade_idents, facade_globs) = reexports(&prelude_body(&facade_src));

        // The core prelude re-exports concrete items only. A glob there could not
        // be statically enumerated against the set — fail loud, never pass blind.
        assert!(
            core_globs.is_empty(),
            "jerrycan-core prelude gained a glob re-export {core_globs:?}: RESERVED_PRELUDE_IDENTS cannot mirror a glob — enumerate it or update this tripwire (#129)"
        );
        // The facade prelude's ONLY permitted glob is the core prelude it re-exports
        // (enumerated above via `core_idents`); any OTHER glob is un-mirrorable.
        assert_eq!(
            facade_globs,
            vec!["jerrycan_core::prelude".to_string()],
            "facade prelude globs changed to {facade_globs:?}: only `jerrycan_core::prelude::*` is enumerable — a new glob reopens the #129 drift blind spot"
        );

        // Every ident the generated `use jerrycan::prelude::*;` pulls into scope
        // must be reserved, or an entity named after it reopens #114 (JC0546 goes
        // silent). Superset — extra reserved names are fine, missing ones are not.
        let reexported: Vec<String> = core_idents.into_iter().chain(facade_idents).collect();
        assert!(
            !reexported.is_empty(),
            "the prelude parser found no re-exports — the parser or the prelude shape changed"
        );
        for id in &reexported {
            assert!(
                RESERVED_PRELUDE_IDENTS.contains(&id.as_str()),
                "`jerrycan::prelude` re-exports `{id}` but RESERVED_PRELUDE_IDENTS (questions.rs) omits it — add `\"{id}\"`, or an entity named `{id}` scaffolds an uncompilable E0659 crate (#114/#129). Current set: {RESERVED_PRELUDE_IDENTS:?}"
            );
        }
    }

    /// A tenant child created at `POST /` under a module mounted at `/clubs/{club_id}`
    /// — the fk `club_id` is supplied by the MOUNT, not `ep.path`.
    const NESTED_MOUNT_CREATE: &str = r#"{ "name": "clubs-api", "contract_version": 1,
        "auth": { "model": "session", "roles": ["owner", "member"] },
        "dependencies": ["db", "auth"],
        "tenancy": { "entity": "Club", "member_roles": ["owner", "member"] },
        "modules": [
            { "name": "clubs",
              "entities": [{ "name": "Club", "fields": [
                  { "name": "id", "type": "integer" }, { "name": "name", "type": "string" } ]}],
              "endpoints": [
                  { "operation_id": "create_club", "method": "POST", "path": "/", "auth_required": true,
                    "request_body": { "entity": "Club" },
                    "success": { "status": 201, "entity": "Club" } } ] },
            { "name": "books", "mount": "/clubs/{club_id}",
              "entities": [{ "name": "Book",
                  "belongs_to": [{ "entity": "Club" }],
                  "fields": [{ "name": "id", "type": "integer" }, { "name": "title", "type": "string" }] }],
              "endpoints": [
                  { "operation_id": "create_book", "method": "POST", "path": "/", "auth_required": true,
                    "request_body": { "entity": "Book" },
                    "success": { "status": 201, "entity": "Book" } } ] }
        ] }"#;

    /// The #60 repro: one entity created both nested and standalone.
    const DUAL_CREATE: &str = r#"{
        "name": "habits", "contract_version": 1, "dependencies": ["db"],
        "modules": [{
            "name": "habits",
            "entities": [
                { "name": "Habit", "fields": [
                    { "name": "id", "type": "integer" }, { "name": "title", "type": "string" } ] },
                { "name": "Checkin",
                  "belongs_to": [{ "entity": "Habit", "on_delete": "cascade" }],
                  "fields": [
                    { "name": "id", "type": "integer" }, { "name": "note", "type": "string" } ] }
            ],
            "endpoints": [
                { "operation_id": "create_checkin_nested", "method": "POST", "path": "/{habit_id}/checkins",
                  "request_body": { "entity": "Checkin" },
                  "success": { "status": 201, "entity": "Checkin" } },
                { "operation_id": "create_checkin_flat", "method": "POST", "path": "/checkins",
                  "request_body": { "entity": "Checkin" },
                  "success": { "status": 201, "entity": "Checkin" } }
            ]
        }]
    }"#;

    /// The same entity created ONLY under its parent's path — implementable.
    const NESTED_ONLY: &str = r#"{
        "name": "habits", "contract_version": 1, "dependencies": ["db"],
        "modules": [{
            "name": "habits",
            "entities": [
                { "name": "Habit", "fields": [
                    { "name": "id", "type": "integer" }, { "name": "title", "type": "string" } ] },
                { "name": "Checkin",
                  "belongs_to": [{ "entity": "Habit", "on_delete": "cascade" }],
                  "fields": [
                    { "name": "id", "type": "integer" }, { "name": "note", "type": "string" } ] }
            ],
            "endpoints": [
                { "operation_id": "create_checkin_nested", "method": "POST", "path": "/{habit_id}/checkins",
                  "request_body": { "entity": "Checkin" },
                  "success": { "status": 201, "entity": "Checkin" } }
            ]
        }]
    }"#;
}
