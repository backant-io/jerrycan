//! The single registry of stable diagnostic codes. `jerrycan explain` reads it;
//! a completeness test fails if any code emitted in source is missing here.

/// One diagnostic code's human explanation.
pub struct CodeInfo {
    pub code: &'static str,
    pub title: &'static str,
    pub cause: &'static str,
    pub fix: &'static str,
    pub doc: &'static str,
}

/// Every JC#### (framework runtime) and JL#### (jerrycan generation lint) code.
pub const REGISTRY: &[CodeInfo] = &[
    CodeInfo {
        code: "JC0400",
        title: "bad request",
        cause: "a path parameter or query string failed to parse, or the path had a malformed percent-encoding",
        fix: "send well-formed input; check the route's parameter types",
        doc: "jerrycan docs errors",
    },
    CodeInfo {
        code: "JC0401",
        title: "authentication required",
        cause: "no valid session cookie or bearer token was presented",
        fix: "log in (Session) or send Authorization: Bearer <jwt>",
        doc: "jerrycan docs auth",
    },
    CodeInfo {
        code: "JC0403",
        title: "forbidden",
        cause: "authenticated, but require_role rejected the user's role",
        fix: "use an account with the required role",
        doc: "jerrycan docs auth",
    },
    CodeInfo {
        code: "JC0404",
        title: "not found",
        cause: "no route matched the path, or a handler returned Error::not_found()",
        fix: "check the path; confirm the resource exists",
        doc: "jerrycan docs app",
    },
    CodeInfo {
        code: "JC0405",
        title: "method not allowed",
        cause: "the path exists but not for this HTTP method",
        fix: "use a method the route defines",
        doc: "jerrycan docs app",
    },
    CodeInfo {
        code: "JC0408",
        title: "request timeout",
        cause: "the request body was not received within the read budget",
        fix: "send the body promptly; raise body_read_timeout if legitimate",
        doc: "jerrycan docs app",
    },
    CodeInfo {
        code: "JC0409",
        title: "conflict",
        cause: "the write violates a unique key (e.g. a re-POSTed id)",
        fix: "use a fresh key, or treat as already-created (idempotent retry)",
        doc: "jerrycan docs database",
    },
    CodeInfo {
        code: "JC0413",
        title: "payload too large",
        cause: "the request body exceeded the size limit (default 1 MiB)",
        fix: "send a smaller body; raise the limit explicitly if needed",
        doc: "jerrycan docs app",
    },
    CodeInfo {
        code: "JC0415",
        title: "unsupported media type",
        cause: "the request's content type is not what the endpoint consumes: Multipart requires multipart/form-data with a boundary, and a storage bucket upload must match the bucket's allowed_mime allowlist",
        fix: "send the content type the endpoint declares; for uploads, multipart/form-data with a valid boundary parameter, or a Content-Type inside the bucket's allowed_mime list",
        doc: "jerrycan docs extractors",
    },
    CodeInfo {
        code: "JC0422",
        title: "unprocessable entity",
        cause: "the JSON body failed to parse, or Valid<T> found violations",
        fix: "fix the body to match the schema; read the details array",
        doc: "jerrycan docs validation",
    },
    CodeInfo {
        code: "JC0429",
        title: "too many requests",
        cause: "the client exceeded the configured rate limit for its identity (api-key → user → IP) in the current fixed window",
        fix: "slow down and retry after the Retry-After delay; raise the limit in the rate-limit extension config if the traffic is legitimate",
        doc: "jerrycan docs middleware",
    },
    CodeInfo {
        code: "JC0500",
        title: "internal error",
        cause: "an unexpected server-side failure (or a handler panicked)",
        fix: "check server logs; the cause is logged, never sent to the client",
        doc: "jerrycan docs errors",
    },
    CodeInfo {
        code: "JC0503",
        title: "handler timeout",
        cause: "the request exceeded the per-request handler budget (default 30s)",
        fix: "make the handler faster or raise handler_timeout",
        doc: "jerrycan docs app",
    },
    CodeInfo {
        code: "JC0510",
        title: "database error",
        cause: "a jerrycan-db query/connection failed",
        fix: "check JERRYCAN_DATABASE_URL and migrations; the sqlx detail is on stderr",
        doc: "jerrycan docs database",
    },
    CodeInfo {
        code: "JC0520",
        title: "schema contract is stale",
        cause: "schema.json does not match the schema derived from the module migrations",
        fix: "run jerrycan schema --write and commit the result",
        doc: "jerrycan docs database",
    },
    CodeInfo {
        code: "JC0521",
        title: "job failed",
        cause: "a background job returned an error and (after its retries) was moved to the dead-letter table, or failed irrecoverably",
        fix: "inspect the dead-letter table and the operator logs; fix the job handler and requeue the dead-lettered job",
        doc: "jerrycan docs jobs",
    },
    CodeInfo {
        code: "JC1001",
        title: "missing dependency",
        cause: "a handler asked for a Dep<T> with no registered provider",
        fix: "provide(value) or provide_dep(factory) on the app or module",
        doc: "jerrycan docs dependencies",
    },
    CodeInfo {
        code: "JC1002",
        title: "dependency cycle",
        cause: "dependency factories recursed past the depth limit (cycle or absurd chain)",
        fix: "break the cycle in your provide_dep graph",
        doc: "jerrycan docs dependencies",
    },
    CodeInfo {
        code: "JC1003",
        title: "dependency requires an HTTP request",
        cause: "a dependency factory used an HTTP extractor (Json/Path/Query/Headers) but was resolved in a task context (background job, startup)",
        fix: "restructure the factory to take only Dep<T> arguments, or resolve it inside a request",
        doc: "jerrycan docs dependencies",
    },
    CodeInfo {
        code: "JL0001",
        title: "leaky route crate",
        cause: "a route crate's lib.rs exports more than module()",
        fix: "make it pub(crate), or move shared types to the shared crate",
        doc: "jerrycan docs modules",
    },
    CodeInfo {
        code: "JL0002",
        title: "missing handler",
        cause: "a design endpoint has no matching handler fn",
        fix: "add the handler with the operation_id name, or fix the design",
        doc: "jerrycan docs modules",
    },
    CodeInfo {
        code: "JL0003",
        title: "generated drift",
        cause: "a tool-owned generated file was hand-edited or the design changed without regenerating",
        fix: "re-run jerrycan generate; never hand-edit GENERATED files",
        doc: "jerrycan docs app",
    },
    CodeInfo {
        code: "JL0004",
        title: "unguarded mutation",
        cause: "an auth design has a mutating route with no auth guard",
        fix: "set auth_required: true or required_roles on the endpoint",
        doc: "jerrycan docs auth",
    },
    CodeInfo {
        code: "JL0006",
        title: "cross-tenant data access",
        cause: "a handler for a tenant-owned entity used an unscoped repo method (all/get/remove), so it can read or delete another tenant's rows",
        fix: "call the tenant-scoped accessor (all_for/get_for/remove_for) with the current tenant's id",
        doc: "jerrycan docs database",
    },
    CodeInfo {
        code: "JL0007",
        title: "request-boundary escape",
        cause: "agent-owned module code calls into process/filesystem/network APIs directly — outside the framework's request boundary and threat model",
        fix: "use framework extensions for I/O; if genuinely intended, append `// jerrycan:allow JL0007` to the line",
        doc: "jerrycan docs errors",
    },
    CodeInfo {
        code: "JL0008",
        title: "tenant-owned handler could not be scanned for scoping",
        cause: "JL0006 must read and parse each tenant-owned module's handlers.rs to verify it uses the scoped accessors, but this file is missing, unreadable, or not valid Rust — so scoping could not be checked and an unscoped cross-tenant call could pass unseen",
        fix: "ensure the handler file exists and compiles (run `cargo check`); a scaffold is generated parseable — if you hand-edited it, fix the syntax so `jerrycan check` can verify tenant scoping",
        doc: "jerrycan docs database",
    },
    CodeInfo {
        code: "JC0540",
        title: "tenant entity is the auth identity",
        cause: "the design's tenancy.entity names the auth identity entity — its derived foreign key column is `user_id`, the same column the generated membership table already uses for the authenticated user, so a user cannot be their own tenant org and the auth_0001 migration would fail with `duplicate column name: user_id`",
        fix: "for per-user data, drop the tenancy block and give each owned entity a belongs_to the identity plus tenant-scoped guard methods (all_for/get_for); for orgs/teams, point tenancy.entity at a SEPARATE tenant entity (e.g. Org or Workspace) that users hold a membership in",
        doc: "jerrycan docs tenancy",
    },
    CodeInfo {
        code: "JC0541",
        title: "entity name shadows a generated request DTO",
        cause: "an entity is literally named `{X}Request` while another entity `X` omits a server-owned field (an identity fk, a `default`, or a path-redundant parent fk) and so generates a `{X}Request` DTO — the Rust struct and the OpenAPI component would be defined twice, a compile error plus a silently clobbered schema",
        fix: "rename the `{X}Request` entity (e.g. `{X}Payload` or `{X}Submission`); the `{X}Request` name is reserved for the generated request DTO of entity `X`",
        doc: "jerrycan docs validation",
    },
    CodeInfo {
        code: "JC0542",
        title: "conflicting path parameters across sibling routes",
        cause: "two routes reach the same path segment position through an identical prefix but name that position's `{param}` differently (e.g. `/tickets/{id}` and `/tickets/{ticket_id}/comments`) — the router keys each position by a SINGLE parameter name, so registering both aborts `App::build` at startup with JC0500 `conflicting path parameters` (after a clean scaffold, mid-test); with `tenancy`, the implicit member-management routes (`/{tenant_fk}/members`, `/{tenant_fk}/members/{user_id}`, issue #107) join this check, so a tenant-module endpoint with a custom param name, or one occupying a reserved member path, conflicts the same way",
        fix: "give the shared segment ONE parameter name in every route that reaches it (rename `{ticket_id}`→`{id}` or vice versa), or restructure the nesting so the position is not shared (mount the diverging routes under distinct static prefixes)",
        doc: "jerrycan docs app",
    },
    CodeInfo {
        code: "JC0543",
        title: "enum value is not an identifier",
        cause: "a string field's enum `values` entry contains a character outside ^[A-Za-z0-9_-]+$ — values are interpolated UNESCAPED into generated Rust (the deserialize allow-list, the 422 error text, and the test fixtures), so a quote or backslash emits a crate that fails to compile far from the design (other non-identifier characters are rejected for the same interpolation-safety rule)",
        fix: "use identifier-shaped enum values (ASCII letters, digits, `_`, `-`); keep any human display label out of the stored value",
        doc: "jerrycan docs validation",
    },
    CodeInfo {
        code: "JC0544",
        title: "nested-entity create route cannot supply its path-owned foreign key",
        cause: "an entity has a parent foreign key another route supplies from a path parameter, so the shared per-entity request DTO drops it for EVERY create of the entity — but this body-carrying create/update route's own path has no matching `{param}`, so the NOT-NULL column can be set from neither the body nor the path and the route is un-implementable",
        fix: "add the parent's `{fk}` path parameter to this route (mount it under the parent), or split the entity so the standalone route uses its own request body that keeps the fk",
        doc: "jerrycan docs validation",
    },
    CodeInfo {
        code: "JC0545",
        title: "entity reaches the tenant through more than one path",
        cause: "an entity has two or more distinct `belongs_to` chains that each reach the tenant entity (a diamond graph), so jerrycan cannot decide which chain defines tenant ownership — guessing would scope reads/writes to the wrong tenant and re-open the cross-tenant leak",
        fix: "collapse the entity's tenant ownership to a SINGLE `belongs_to` path (drop the redundant parent, or split the entity), so exactly one chain reaches the tenant",
        doc: "jerrycan docs database",
    },
    CodeInfo {
        code: "JC0546",
        title: "entity name collides with a prelude re-export",
        cause: "an entity is named the same as an identifier re-exported by `jerrycan::prelude` (e.g. `Module`, `Error`, `Json`) — generated modules write `use jerrycan::prelude::*;` next to `use super::model::*;`, so the entity's generated `struct` and the prelude item are two glob imports of the same name, and every reference is E0659 `... is ambiguous`; the scaffolded crate does not compile",
        fix: "rename the entity so its name is not a reserved prelude identifier (e.g. `{Name}Record` or a domain-specific name)",
        doc: "jerrycan docs validation",
    },
    CodeInfo {
        code: "JC0547",
        title: "realtime changes on a transitively tenant-owned entity",
        cause: "a realtime `changes` entity reaches the tenant only through an intermediate parent (a grandchild chain like Contact -> Account -> Org), so its row image carries no tenant key column — change events could not be tenant-scoped and every tenant's rows would broadcast to every authenticated principal",
        fix: "the changes entity must be the tenant itself or a DIRECT child of it: flatten the relationship (give the entity its own belongs_to the tenant) or drop it from `changes`",
        doc: "jerrycan docs realtime",
    },
    CodeInfo {
        code: "JC0548",
        title: "invalid tenancy member_roles",
        cause: "`tenancy.member_roles` is empty, repeats a role, or contains a role outside ^[A-Za-z0-9_-]+$ — `member_roles[0]` is the admin role the generated member-management surface gates on, the list becomes the generated MEMBER_ROLES allow-list and the OpenAPI `role` enum, and role names are interpolated UNESCAPED into generated Rust string literals (the MEMBER_ROLES const, the membership seed, `require_role` gates), so an empty or duplicated list breaks the admin-role convention and a quote or backslash emits a crate that fails to compile",
        fix: "declare a non-empty, duplicate-free member_roles list of identifier-shaped names (letters, digits, `_`, `-`), admin role first (e.g. [\"owner\", \"member\"])",
        doc: "jerrycan docs tenancy",
    },
    CodeInfo {
        code: "JC0549",
        title: "public_read misuse, or an unimplementable unguarded read on an owner-scoped entity",
        cause: "`public_read` demands the per-user owner-write shape: it fires when a write endpoint (POST/PUT/PATCH/DELETE) of a public_read entity is `public` or unguarded (the open door — public reads with open writes), when the entity is not identity-owned (no belongs_to the auth identity) or IS tenant-owned (public reads would bypass the Tenant guard), or when the design has no active auth model (owner-gated writes need a session); the SAME code also flags an unguarded GET on a per-user owner-scoped entity that has NOT opted into public_read — such a read is unimplementable, because the entity's repo emits only the owner-scoped accessors while the unguarded handler receives no session user",
        fix: "keep every write of a public_read entity `auth_required: true` (never `public`), give the entity a `belongs_to` the auth identity outside any tenancy, and set auth.model to `session` or `jwt`; for the unguarded read, either set `public_read: true` on the entity to make its reads public, or keep the GET authenticated (`auth_required: true`)",
        doc: "jerrycan docs auth",
    },
    CodeInfo {
        code: "JC0550",
        title: "tenant detail route addresses the tenant by a non-pk param",
        cause: "the tenant entity's own detail route carries a path parameter that is not the tenant's fk (e.g. `/{slug}` instead of `/{id}`/`/{club_id}`) — the membership guard verifies the tenant NAMED BY THE PATH FK and parses that path value as the tenant's pk type, so a non-pk param can neither be bound nor membership-checked, and the handler would be generated with no membership check at all (a silent cross-tenant read); renaming the param is not a fix, because a slug value is not a pk",
        fix: "address the tenant's own detail route by pk: use `/{id}` (auto-normalized to the tenant fk) or the explicit `/{fk}` directly; slug-based tenant addressing (resolving slug→pk before the membership query) is not yet supported",
        doc: "jerrycan docs tenancy",
    },
    CodeInfo {
        code: "JC0551",
        title: "no acceptance tests for a module with endpoints",
        cause: "`jerrycan check` found no `crates/routes/<module>/tests/acceptance.rs` for a top-level module that declares endpoints — the module was never gen-tested, so the tests step ran ZERO acceptance tests and its exit-0 green would be hollow (a never-tested scaffold must not read ok:true)",
        fix: "run `jerrycan gen-tests --module <module>` to generate the acceptance suite, then implement handlers until it passes; the FILE's existence is the signal (an all-TODO design's banner-only file satisfies it — jerrycan never demands tests the design cannot green)",
        doc: "jerrycan docs testing",
    },
    CodeInfo {
        code: "JC0552",
        title: "invalid field range/length constraint",
        cause: "a field's `min`/`max`/`min_len`/`max_len` constraint (#80) is unusable: range keys on a non-integer field or length keys on a non-string field, an empty range (min > max, or min_len > max_len), a length bound combined with enum `values` (the enum already fixes the exact allowed strings), `max_len: 0` on a required field (unfillable), any constraint key on the pk `id` (ids are server-assigned; the generated probes and seeds assume them free), a `min_len` above the 4096 fixture ceiling, a `default` outside the field's own bounds, or a `unique` field whose constraint admits fewer than 3 distinct values (`max - min + 1 < 3`, or `max_len: 0`) — the generated suite needs up to 3 distinct in-range values per field (the probe fixture and the two tenant seeds), so a narrower unique range collides on its own UNIQUE index; each of these would generate an app whose validator or acceptance fixtures could never pass",
        fix: "put `min`/`max` on integer fields and `min_len`/`max_len` on string fields (inclusive, length in Unicode code points), keep min <= max and min_len <= max_len with min_len at most 4096, drop length bounds from enum `values` fields and every constraint key from `id`, pick a `default` inside the field's declared bounds, and give a `unique` field's constraint room for at least 3 distinct values",
        doc: "jerrycan docs validation",
    },
    CodeInfo {
        code: "JC0553",
        title: "entity collides with the generated membership table/type",
        cause: "with `tenancy`, jerrycan generates a `{tenant}_members` membership table and a `pub struct {Tenant}Member` row type (issue #107) for the member-management surface; an entity other than the tenant whose RESOLVED table name equals `{tenant}_members` — one named `{Tenant}Member`, whose default table is exactly that, or one with an explicit `table` override onto it — or whose NAME equals `{Tenant}Member`, collides with that reserved surface: the generator would emit the same table twice (a raw `table \"{tenant}_members\" already exists` mid-scaffold, after a clean `check`) or two `struct {Tenant}Member` definitions",
        fix: "rename the offending entity (and/or drop its `table` override) so neither its name equals `{Tenant}Member` nor its table resolves to `{tenant}_members` — those are reserved for the generated member surface",
        doc: "jerrycan docs tenancy",
    },
    CodeInfo {
        code: "JC0554",
        title: "write_only on the primary key id",
        cause: "the pk `id` field declares `write_only: true` — but `write_only` response-hides a field (`#[serde(skip_serializing)]` on the generated Model, issue #112), and the id MUST be returned: the generated id-echo probe and every cross-scope acceptance test key on `body[\"id\"]`, so a hidden id breaks the generated suite by construction",
        fix: "remove `write_only` from the `id` field; ids are always returned. Put `write_only` on the secret columns that must not appear in responses (a `password_hash` is auto-hidden by name; add it to e.g. an `api_token`/`secret` field)",
        doc: "jerrycan docs validation",
    },
    // The code that once occupied this slot (#167) is RETIRED. It refused a
    // write_only/password_hash column on a realtime `changes` entity because the
    // broadcast shipped the raw row. The realtime engine now PROJECTS those
    // columns out of the broadcast (ChangeChannelSpec.hidden_columns +
    // deliver_change), so the combination is safe and no longer refused. The
    // number is retired, not reused — new codes continue past the highest in use.
    CodeInfo {
        code: "JC0556",
        title: "storage write_roles is unusable as declared",
        cause: "a bucket's `write_roles` (#132) — the tenant member roles allowed to upload/delete — is meaningless as written: either an entry is not a declared `tenancy.member_roles` role, or the bucket is not tenant-scoped (its `owner` is not the tenancy entity, or the design has no tenancy). A tenant-scoped bucket stamps `owner_id = tenant.id()`, so every member is 'the owner' and a read-only-role member could write; `write_roles` closes that by 403-ing a non-write role. On a non-tenant bucket no Tenant guard runs, so the declared gate would emit NOTHING and silently leave writes open — a security footgun, not a no-op",
        fix: "make the bucket tenant-owned (set `owner` to the tenancy entity) and list only declared member_roles in `write_roles`, or drop `write_roles` entirely (empty = any member may write, the backward-compatible default). Reads (download/list/sign) are never role-gated",
        doc: "jerrycan docs storage",
    },
    CodeInfo {
        code: "JC0557",
        title: "misplaced or mis-cased `now` default",
        cause: "the `now` default sentinel (#110) is a DYNAMIC server-set timestamp valid ONLY on a `datetime` field — the server writes the current time (`now_rfc3339()`) on create and omits the field from both request DTOs — but it was declared on a non-datetime field (`string`/`integer`/`boolean`/`float`/`uuid`/`json`, where `\"now\"` would otherwise be read as a literal), or a near-miss casing (`\"NOW\"`/`\"Now\"`) was written on a datetime field where it can neither be a static RFC3339 literal nor the exact sentinel",
        fix: "put `\"default\": \"now\"` (exact lowercase) on a `datetime` field to set it to the current time on create; on any other type use a static literal default, or change the field `type` to `datetime`; never rely on a mis-cased near-miss — the sentinel is exactly `\"now\"`",
        doc: "jerrycan docs designing",
    },
    CodeInfo {
        code: "JC0558",
        title: "anonymous read/write on the tenant or a tenant-owned entity",
        cause: "in an auth design with tenancy, an endpoint whose repo entity is the tenant entity OR a tenant-owned entity (directly or transitively) omits `auth_required` (serde default false) and is not `public` — genroute emits the Tenant guard only for a guarded endpoint, so this handler is generated with NO `Dep<Tenant>` and NO `CurrentUser`: it is fully anonymous and any caller could read (or write) any tenant's rows, yet `jerrycan check` was green. This is the tenant twin of JC0549(c)'s per-user unguarded-read refusal; the merely-unguarded-non-public case was previously unpoliced (the public-on-tenant-owned check keys on `public`, JL0004 covers mutations only, and a childless tenant module's handlers are never lint-scanned)",
        fix: "set `auth_required: true` on the endpoint so the membership guard scopes it to the caller's tenant; a signature-authenticated webhook (it declares a 4xx error whose `when` names a signature check) is exempt. Tenant-owned entities have no public-read in v1 — public reads would bypass the Tenant guard (#105 is per-user-only), so authenticate the read rather than making it `public`",
        doc: "jerrycan docs auth",
    },
    CodeInfo {
        code: "JC0559",
        title: "unbuildable composite unique group",
        cause: "a table-level composite `unique` group on an entity (issue #115) is unbuildable: it has FEWER THAN 2 DISTINCT columns (a single-column unique — or a repeated column like `[\"a\", \"a\"]`, which would silently make that lone column globally unique — must use the field's `unique` flag, not a group; the group is meant for a `UNIQUE(a, b)` a lone field cannot express), or it names a column that is NEITHER a declared field NOR a `belongs_to` fk column of the entity (`snake_case(entity) + \"_id\"`) — so the generated `CREATE UNIQUE INDEX` would reference a column that does not exist and fail at migration apply — or it DUPLICATES another group on the same entity (the same column set, order-insensitive), which is redundant",
        fix: "give each `unique` group at least 2 columns, each a declared field name or a belongs_to fk column of THIS entity; for single-column uniqueness set `unique: true` on the field instead; and list each column set once (order does not matter). The primary use is a composite over two belongs_to fk columns — a like per (user, post): `\"unique\": [[\"user_id\", \"post_id\"]]`",
        doc: "jerrycan docs designing",
    },
    CodeInfo {
        code: "JC0560",
        title: "colliding or malformed belongs_to fk alias",
        cause: "a `belongs_to` fk column (issue #119) is unbuildable: two `belongs_to` on the same entity derive the SAME fk column — two un-aliased refs to one target (both `snake(entity)_id`), or an `as` alias whose `{as}_id` equals another belongs_to's fk column — so the generated Model would carry a DUPLICATE fk field and the migration a duplicate column; OR an `{as}_id` (or the default `snake(entity)_id`) COLLIDES with a declared field name or the pk `id`; OR the `as` alias is MALFORMED (not snake_case `^[a-z][a-z0-9_]*$`, so the derived column and Rust field would be invalid); OR the `as` lands on a RESERVED fk the target doesn't own — the identity fk `user_id` (in an auth design) or the tenancy fk — which would hijack per-user/tenant scoping and fail as opaque generated Rust. The alias exists precisely so two references to one entity (a ledger Transfer's from_account/to_account, a self-referential Comment's parent) get DISTINCT fk columns and distinct DDL constraint names",
        fix: "give each `belongs_to` a distinct fk column: add an `as` alias to at least one of two refs to the same entity (`{ \"entity\": \"Account\", \"as\": \"from_account\" }` → `from_account_id`), so no two fk columns and no fk-vs-field/pk names collide; make every `as` snake_case (`^[a-z][a-z0-9_]*$`); and never alias onto the reserved `user_id` or tenancy fk (only the identity/tenancy entity owns those). A single un-aliased `belongs_to` per target needs no `as` (byte-identical)",
        doc: "jerrycan docs designing",
    },
    CodeInfo {
        code: "JC0561",
        title: "invalid request_body — entity XOR inline fields",
        cause: "a `request_body` (issue #122) is malformed: it declares BOTH an `entity` and inline `fields` (a body is a table-entity reference OR an ad-hoc inline DTO, never both), or NEITHER (it must be exactly one); or an inline (`fields`) body sits on an endpoint with no `operation_id`, so its generated request struct `{Pascal(operation_id)}Request` would be unnameable; or an inline field is itself invalid — a name that is not snake_case or is a Rust keyword no raw identifier can escape, a duplicate field name (the struct would carry two same-named fields), or a #80 range/length constraint that is misplaced or empty (validated exactly like an entity field via JC0552/JC0543); or the inline DTO's `{Pascal(operation_id)}Request` NAME collides with another emitted DTO — an entity's generated `{Entity}Request`/`{Entity}UpdateRequest`, an entity literally named `{X}Request`, or ANOTHER inline body's name in a different module (operation_id is unique only per-module, but the OpenAPI schema map is global) — which would define `struct {name}` twice (E0428) or clobber one OpenAPI schema with the other",
        fix: "give each `request_body` exactly one shape: `{\"entity\": \"Todo\"}` to deserialize a table row, or `{\"fields\": [{\"name\": \"coupon\", \"type\": \"string\"}, ...]}` for a custom-action DTO that is not a row; put the inline body on an endpoint with an `operation_id` (it names the DTO); make every inline field a snake_case non-keyword name, unique within the body, with well-formed #80 constraints; and choose an `operation_id` whose `{Pascal(operation_id)}Request` name is design-globally unique — it must not match an entity's generated DTO, an entity named `{X}Request`, or another inline body's name",
        doc: "jerrycan docs designing",
    },
    CodeInfo {
        code: "JC0562",
        title: "mixed-shape tenant entity (flat and path-scoped)",
        cause: "a tenant-owned entity (issue #175) is reachable by BOTH a flat (membership-set, body-fk) write AND a path-scoped (`/{fk}/…`) route across the design — but the generator emits only ONE scoping shape per entity. `entity_is_flat_tenant_owned` is `!saw_path_scoped && saw_flat`, so a mixed entity is classified NON-flat: its membership-checked `create_for_memberships`/`update_for_memberships`/`remove_for_memberships` accessors are withheld, yet the flat-write steer still fires — so following the generated comment would call a `*_for_memberships` method that isn't emitted (a `method not found` compile error behind a green `check`, the #116 class). No corpus design is mixed today; the refusal makes the broken shape impossible to ship",
        fix: "give the entity a SINGLE tenant-route shape. Make every route PATH-SCOPED — carry the tenant fk in the path (`/{fk}/…`), scoped by the verified path tenant via `all_for`/`get_for`/`update_for`/`remove_for` — OR make every route FLAT (no tenant fk in the path; the fk comes from the request body and is verified against the caller's memberships via `*_for_memberships`). Never mix the two shapes on one entity",
        doc: "jerrycan docs auth",
    },
    CodeInfo {
        code: "JC0563",
        title: "malformed rate_limit block",
        cause: "the top-level `rate_limit` block (issue #83) is unbuildable: `limit` is 0 (a 0-per-window limit rejects EVERY request — surely unintended), or `window` does not parse to a POSITIVE duration (it must be a bare number of seconds or an `<n>s`/`<n>m`/`<n>h`/`<n>d` string — e.g. \"1m\" — and becomes `Duration::from_secs(N)` in the generated `.extend(RateLimit::per_window(..))` wiring), or `api_key_header` is not a valid HTTP header name (`^[A-Za-z0-9-]+$`) — the header is interpolated into `RateLimit::api_key_header(..)`, so a bad token would panic the generated app at startup",
        fix: "set `limit` to a positive request count per window; give `window` a positive duration (\"30s\", \"1m\", \"1h\", \"1d\", or a bare seconds count); and make `api_key_header` a valid header token (^[A-Za-z0-9-]+$, e.g. \"x-api-key\") — or omit it to partition by the authenticated user then client IP (both unspoofable; an unauthenticated api-key header is client-controlled)",
        doc: "jerrycan docs middleware",
    },
    CodeInfo {
        code: "JC0564",
        title: "invalid reserve_against field",
        cause: "a field's `reserve_against` (issue #187) wires the generated atomic `{Entity}Repo::reserve(id, n)` method — the #108-proven conditional UPDATE `SET counter = counter + n WHERE id = ? AND counter + n <= capacity` — but the declaration is unbuildable: it names a capacity field that does not exist on the entity; the counter field (the one carrying `reserve_against`) is not `integer`; the named capacity field is not `integer`; the counter or the capacity is the primary key `id`; `reserve_against` names the field's OWN name (a field cannot reserve against itself); more than one field on the entity carries `reserve_against` (the generated `reserve` method name would be ambiguous); or the design has no database (the atomic UPDATE is emitted on the SQL-backed repo only — a memory repo cannot run it)",
        fix: "put `reserve_against` on exactly ONE integer counter field per entity, naming a SEPARATE integer capacity field (both ordinary columns, neither the pk `id`) — e.g. `{ \"name\": \"used\", \"type\": \"integer\", \"default\": 0, \"reserve_against\": \"capacity\" }` beside `{ \"name\": \"capacity\", \"type\": \"integer\" }`; and give the design a database (`db` in `dependencies`) so the SQL-backed `reserve` method is generated",
        doc: "jerrycan docs designing",
    },
    CodeInfo {
        code: "JC0530",
        title: "realtime requires postgres",
        cause: "the design declares realtime changes but the app is running on sqlite",
        fix: "point JERRYCAN_DATABASE_URL at a Postgres database (broadcast/presence channels work without it; changes channels need Postgres)",
        doc: "jerrycan docs realtime",
    },
    CodeInfo {
        code: "JC0531",
        title: "realtime replication unavailable",
        cause: "wal_level is not 'logical' or the role lacks REPLICATION, so changes run on the trigger + LISTEN/NOTIFY fallback (identical client behavior, weaker delivery guarantee)",
        fix: "set wal_level=logical and grant REPLICATION to the app role, then restart Postgres — realtime upgrades itself on next start",
        doc: "jerrycan docs realtime",
    },
];

/// Look up a code, case-insensitively.
pub fn lookup(code: &str) -> Option<&'static CodeInfo> {
    let upper = code.to_uppercase();
    REGISTRY.iter().find(|c| c.code == upper)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_is_case_insensitive() {
        assert_eq!(lookup("jc0404").unwrap().code, "JC0404");
        assert_eq!(lookup("JC0404").unwrap().code, "JC0404");
        // Built at runtime so this source file holds no code-shaped literal the
        // completeness walk would mistake for an emitted code.
        let absent = format!("JC{}", 9999);
        assert!(lookup(&absent).is_none());
    }

    #[test]
    fn realtime_codes_are_registered() {
        assert_eq!(
            lookup("JC0530").unwrap().title,
            "realtime requires postgres"
        );
        assert_eq!(
            lookup("JC0531").unwrap().title,
            "realtime replication unavailable"
        );
    }

    #[test]
    fn jc0415_covers_bucket_mime_allowlists() {
        // WHY: `jerrycan explain JC0415` is the agent's first stop when a
        // generated bucket rejects an upload — the registry must name the
        // allowlist cause, not just the Multipart boundary case.
        let info = lookup("JC0415").unwrap();
        assert!(info.cause.contains("allowed_mime"), "cause: {}", info.cause);
    }

    #[test]
    fn jc0540_explains_the_tenant_identity_conflict() {
        // WHY: JC0540 is the agent's stop after the CLI rejects a design whose
        // tenancy.entity is the auth identity — the registry must state the cause
        // ("a user cannot be their own tenant org") and BOTH fixes.
        let info = lookup("JC0540").unwrap();
        assert!(
            info.cause.contains("auth identity") || info.cause.contains("own tenant"),
            "cause: {}",
            info.cause
        );
        assert!(
            info.fix.contains("belongs_to") && info.fix.contains("tenant entity"),
            "fix must name both remedies: {}",
            info.fix
        );
    }

    #[test]
    fn design_time_codes_542_543_544_are_registered_and_name_their_remedies() {
        // WHY: JC0542/JC0543/JC0544 are the P-A validator's design-time fail-loud
        // codes (#65/#54/#60). `jerrycan explain <code>` reads this registry, so
        // each must be present and each explanation must name the concrete fix(es).
        for code in ["JC0542", "JC0543", "JC0544"] {
            let info = lookup(code).unwrap_or_else(|| panic!("{code} must be registered"));
            assert!(
                !info.title.is_empty() && !info.cause.is_empty() && !info.fix.is_empty(),
                "{code} needs a full explanation"
            );
        }
        // JC0542 names BOTH remedies: unify the name, or restructure the nesting.
        let router = lookup("JC0542").unwrap();
        assert!(
            router.fix.contains("ONE parameter name") && router.fix.contains("restructure"),
            "JC0542 must name both remedies: {}",
            router.fix
        );
        // JC0544 names BOTH remedies: add the path param, or split the entity.
        let dual = lookup("JC0544").unwrap();
        assert!(
            dual.fix.contains("path parameter") && dual.fix.contains("split"),
            "JC0544 must name both remedies: {}",
            dual.fix
        );
    }

    #[test]
    fn jc0548_names_all_three_member_roles_failure_modes() {
        // WHY: JC0548 is the agent's stop after `check` rejects a tenancy design's
        // member_roles (#107) — the registry must name all three ways to be wrong
        // (empty / duplicated / non-identifier), the admin-role convention the
        // list backs, and the unescaped-interpolation reason for the charset.
        let info = lookup("JC0548").unwrap();
        assert!(
            info.cause.contains("empty")
                && info.cause.contains("repeats")
                && info.cause.contains("[A-Za-z0-9_-]"),
            "cause must name all three failure modes: {}",
            info.cause
        );
        assert!(
            info.cause.contains("member_roles[0]") && info.cause.contains("UNESCAPED"),
            "cause must state the admin convention and the interpolation risk: {}",
            info.cause
        );
        assert!(
            info.fix.contains("non-empty") && info.fix.contains("admin role first"),
            "fix must state the required shape: {}",
            info.fix
        );
    }

    #[test]
    fn jc0547_names_the_transitive_changes_leak_and_both_remedies() {
        // WHY: JC0547 converts the transitive-changes silent cross-tenant
        // broadcast leak (#102's realtime facet) into a design-time refusal —
        // `jerrycan explain JC0547` must state the cause (no tenant key in the
        // row image) and name BOTH remedies (flatten, or drop from `changes`).
        let info = lookup("JC0547").unwrap();
        assert!(
            info.cause.contains("tenant key") && info.cause.contains("broadcast"),
            "cause must name the missing row-image tenant key and the leak: {}",
            info.cause
        );
        assert!(
            info.fix.contains("flatten") && info.fix.contains("drop"),
            "fix must name both remedies: {}",
            info.fix
        );
    }

    #[test]
    fn jc0550_names_the_fk_binding_and_both_pk_remedies() {
        // WHY: JC0550 converts the silent no-membership-check on a tenant
        // entity's own detail route addressed by a non-pk param (#88) into a
        // design-time refusal — `jerrycan explain JC0550` must state WHY a
        // rename is not the fix (the guard parses the path value as the tenant
        // pk) and name both pk-shaped remedies (`/{id}` or the explicit fk).
        let info = lookup("JC0550").unwrap();
        assert!(
            info.cause.contains("membership") && info.cause.contains("pk"),
            "cause must tie the path-fk binding to the membership guard: {}",
            info.cause
        );
        assert!(
            info.fix.contains("/{id}") && info.fix.contains("fk"),
            "fix must name both pk remedies: {}",
            info.fix
        );
    }

    #[test]
    fn jc0551_names_the_hollow_green_and_the_file_existence_signal() {
        // WHY: JC0551 converts the hollow green (#123a — `check` ok:true on a
        // never-gen-tested scaffold, because a zero-test `cargo test` exits 0)
        // into a red the agent can act on. `jerrycan explain JC0551` must state
        // the cause (zero acceptance tests ran) and that FILE existence — not
        // test count — is the signal, so an all-TODO gen-tested design never
        // false-alarms.
        let info = lookup("JC0551").unwrap();
        assert!(
            info.cause.contains("acceptance.rs") && info.cause.contains("hollow"),
            "cause must tie the missing file to the hollow green: {}",
            info.cause
        );
        assert!(
            info.fix.contains("gen-tests") && info.fix.contains("existence"),
            "fix must name gen-tests and the file-existence signal: {}",
            info.fix
        );
    }

    #[test]
    fn jc0552_names_the_constraint_rules_and_their_remedies() {
        // WHY: JC0552 is the agent's stop after design validation rejects a
        // field range/length constraint (#80) — the registry must name the
        // placement rule (min/max on integer, min_len/max_len on string), the
        // pk-id refusal, and the concrete fixes including the 4096 ceiling.
        let info = lookup("JC0552").unwrap();
        assert!(
            info.cause.contains("min_len") && info.cause.contains("pk `id`"),
            "cause must name the length keys and the pk-id refusal: {}",
            info.cause
        );
        // #80 T3: the unique-cardinality arm (a unique field needs room for
        // the 3 distinct values the generated seeds/fixture materialize).
        assert!(
            info.cause.contains("`unique`") && info.cause.contains("3 distinct"),
            "cause must name the unique-cardinality refusal: {}",
            info.cause
        );
        assert!(
            info.fix.contains("integer fields") && info.fix.contains("4096"),
            "fix must name the placement rule and the ceiling: {}",
            info.fix
        );
        assert!(
            info.fix.contains("at least 3 distinct values"),
            "fix must name the unique-cardinality remedy: {}",
            info.fix
        );
    }

    #[test]
    fn jc0553_names_the_membership_surface_collision_and_the_rename_remedy() {
        // WHY: JC0553 (#141) is the agent's stop after `check` refuses an entity
        // that would collide with the generated `{tenant}_members` membership
        // table or the `{Tenant}Member` row type (issue #107). The old failure
        // was an opaque raw "table already exists" mid-scaffold, so the registry
        // must name BOTH reserved artifacts and the rename remedy.
        let info = lookup("JC0553").unwrap();
        assert!(
            info.cause.contains("_members") && info.cause.contains("{Tenant}Member"),
            "cause must name the reserved membership table and row type: {}",
            info.cause
        );
        assert!(
            info.fix.to_lowercase().contains("rename"),
            "fix must name the rename remedy: {}",
            info.fix
        );
    }

    #[test]
    fn jc0554_names_the_id_must_be_returned_rule() {
        // WHY: JC0554 (#112) refuses an explicit `write_only: true` on the pk
        // `id`. `write_only` response-hides a field, but the id must be echoed in
        // every response (the id-echo probe + cross-scope tests key on
        // body["id"]). `jerrycan explain JC0554` must state the cause (the id must
        // be returned) and the remove-write_only remedy.
        let info = lookup("JC0554").unwrap();
        assert!(
            info.cause.contains("write_only") && info.cause.contains("body[\"id\"]"),
            "cause must tie write_only to the id-echo requirement: {}",
            info.cause
        );
        assert!(
            info.fix.to_lowercase().contains("remove") && info.fix.contains("write_only"),
            "fix must name the remove-write_only remedy: {}",
            info.fix
        );
    }

    #[test]
    fn jc0556_names_the_write_roles_footgun_and_both_remedies() {
        // WHY: JC0556 (#132) refuses a storage `write_roles` that is unusable —
        // an undeclared role, or a non-tenant bucket where the gate would emit
        // nothing and silently leave writes open. `jerrycan explain JC0556` must
        // state the footgun (a tenant bucket lets any member write, so a
        // non-tenant gate is a silent no-op) and name BOTH remedies (make it
        // tenant-owned with declared roles, or drop write_roles).
        let info = lookup("JC0556").unwrap();
        assert!(
            info.cause.contains("write_roles") && info.cause.contains("tenant-scoped"),
            "cause must tie write_roles to the tenant-scope requirement: {}",
            info.cause
        );
        assert!(
            info.fix.contains("tenant-owned") && info.fix.contains("drop"),
            "fix must name both remedies: {}",
            info.fix
        );
        // Reads must be documented as never gated (sign is a download grant).
        assert!(
            info.fix.contains("never role-gated"),
            "fix must state reads are never role-gated: {}",
            info.fix
        );
    }

    #[test]
    fn jc0557_names_the_datetime_only_rule_and_both_misuses() {
        // WHY: JC0557 (#110) refuses a misplaced/mis-cased `now` default. The old
        // failure was a `"now"` silently stored as a literal (or a mis-cased
        // near-miss). `jerrycan explain JC0557` must name the datetime-only rule,
        // BOTH misuses (wrong type, wrong casing), and the `now_rfc3339()` server
        // set — so the agent knows the sentinel is exactly `"now"` on a `datetime`.
        let info = lookup("JC0557").unwrap();
        assert!(
            info.cause.contains("datetime") && info.cause.contains("now_rfc3339"),
            "cause must tie the sentinel to a datetime field set via now_rfc3339: {}",
            info.cause
        );
        assert!(
            info.cause.contains("non-datetime") && info.cause.contains("casing"),
            "cause must name both misuses (wrong type, wrong casing): {}",
            info.cause
        );
        assert!(
            info.fix.contains("datetime") && info.fix.contains("static literal"),
            "fix must name both remedies (datetime type, or a static literal): {}",
            info.fix
        );
    }

    #[test]
    fn jc0558_names_the_anonymous_tenant_read_cause_and_the_auth_fix() {
        // WHY: JC0558 (#148) refuses an unguarded, non-`public` endpoint on the
        // tenant or a tenant-owned entity — genroute emits no Dep<Tenant>/no
        // CurrentUser, so the handler is anonymous and any caller reads/writes any
        // tenant's rows behind a green check. `jerrycan explain JC0558` must name
        // the anonymous-handler cause (no guard, no session param), the tenant /
        // tenant-owned domain, the `auth_required: true` fix, and the
        // signature-webhook exemption — and note there is no tenant-owned
        // public-read in v1.
        let info = lookup("JC0558").unwrap();
        assert!(
            info.cause.contains("tenant-owned")
                && info.cause.contains("Dep<Tenant>")
                && info.cause.contains("CurrentUser"),
            "cause must name the tenant-owned domain and the missing guard/session param: {}",
            info.cause
        );
        assert!(
            info.fix.contains("auth_required: true") && info.fix.contains("signature"),
            "fix must name the auth_required remedy and the signature-webhook exemption: {}",
            info.fix
        );
        assert!(
            info.fix.contains("public") && info.fix.contains("#105"),
            "fix must note tenant-owned entities have no public-read in v1 (#105 is per-user-only): {}",
            info.fix
        );
    }

    #[test]
    fn jc0559_names_the_composite_unique_rules_and_their_remedies() {
        // WHY: JC0559 (#115) refuses an unbuildable table-level composite `unique`
        // group — `jerrycan explain JC0559` must name all three failure modes
        // (fewer than 2 columns, an unknown column, a duplicate group) and the
        // remedies (≥2 columns each a field or a belongs_to fk, use `unique: true`
        // for a single column, list each set once), plus the fk-pair worked example.
        let info = lookup("JC0559").unwrap();
        assert!(
            info.cause.contains("FEWER THAN 2")
                && info.cause.contains("belongs_to` fk column")
                && info.cause.contains("DUPLICATES"),
            "cause must name all three failure modes: {}",
            info.cause
        );
        assert!(
            info.fix.contains("at least 2 columns")
                && info.fix.contains("unique: true")
                && info.fix.contains("[[\"user_id\", \"post_id\"]]"),
            "fix must name the remedies and the fk-pair example: {}",
            info.fix
        );
    }

    #[test]
    fn jc0560_names_the_fk_alias_rules_and_their_remedies() {
        // WHY: JC0560 (#119) refuses a colliding or malformed `belongs_to` fk alias
        // — `jerrycan explain JC0560` must name all three failure modes (two refs
        // deriving the SAME fk column, an fk column colliding with a field/pk, a
        // MALFORMED `as`) and the remedy (a distinct snake_case `as`), plus the
        // two-ref worked example.
        let info = lookup("JC0560").unwrap();
        assert!(
            info.cause.contains("SAME fk column")
                && info.cause.contains("COLLIDES")
                && info.cause.contains("MALFORMED"),
            "cause must name all three failure modes: {}",
            info.cause
        );
        assert!(
            info.fix.contains("as")
                && info.fix.contains("from_account")
                && info.fix.contains("snake_case"),
            "fix must name the distinct-alias remedy and the two-ref example: {}",
            info.fix
        );
    }

    #[test]
    fn jc0561_names_the_entity_xor_fields_rules_and_their_remedies() {
        // WHY: JC0561 (#122) refuses a malformed inline-DTO `request_body` —
        // `jerrycan explain JC0561` must name the entity-XOR-fields rule (both /
        // neither), the operation_id-needed-to-name-the-DTO leg, and the inline-field
        // validation, plus the two-shape remedy.
        let info = lookup("JC0561").unwrap();
        assert!(
            info.cause.contains("BOTH")
                && info.cause.contains("NEITHER")
                && info.cause.contains("operation_id"),
            "cause must name both/neither and the unnameable-DTO leg: {}",
            info.cause
        );
        assert!(
            info.fix.contains("entity") && info.fix.contains("fields"),
            "fix must name both request_body shapes: {}",
            info.fix
        );
    }

    #[test]
    fn jc0562_names_the_mixed_shape_conflict_and_both_remedies() {
        // WHY: JC0562 (#175) refuses a tenant entity reachable by BOTH a flat
        // (membership-set) and a path-scoped route — the generator emits only one
        // scoping shape, so the withheld `*_for_memberships` method the flat steer
        // references is a method-not-found behind a green `check`. `jerrycan explain
        // JC0562` must name the mixed-shape cause and BOTH single-shape remedies
        // (all path-scoped, or all flat).
        let info = lookup("JC0562").unwrap();
        assert!(
            info.cause.contains("BOTH")
                && info.cause.contains("path-scoped")
                && info.cause.contains("for_memberships"),
            "cause must name the mixed-shape conflict: {}",
            info.cause
        );
        assert!(
            info.fix.contains("PATH-SCOPED") && info.fix.contains("FLAT"),
            "fix must name both single-shape remedies: {}",
            info.fix
        );
    }

    #[test]
    fn jc0563_names_all_three_rate_limit_failure_modes() {
        // WHY: JC0563 (#83) is the agent's stop after `check` rejects a malformed
        // `rate_limit` block. `jerrycan explain JC0563` must name all three ways it
        // can be wrong — a 0 limit, a non-positive/unparseable window, and an
        // invalid api_key_header — and the fix must state the valid shapes.
        let info = lookup("JC0563").unwrap();
        assert!(
            info.cause.contains("limit` is 0")
                && info.cause.contains("window")
                && info.cause.contains("api_key_header"),
            "cause must name all three failure modes: {}",
            info.cause
        );
        assert!(
            info.fix.contains("positive") && info.fix.contains("^[A-Za-z0-9-]+$"),
            "fix must state the required shapes: {}",
            info.fix
        );
    }

    #[test]
    fn every_emitted_code_is_in_the_registry() {
        // Grep the workspace source for JC####/JL#### string literals and assert
        // each is registered. This is the "no orphan codes" guard. We walk only
        // each crate's src/ (not tests/): codes that appear solely in test
        // fixtures (e.g. a user-authored 409 ErrorCase in testgen.rs) are example
        // text, not framework-emitted diagnostics.
        use std::collections::BTreeSet;
        let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .unwrap()
            .join("crates");
        let mut found = BTreeSet::new();
        fn walk(dir: &std::path::Path, found: &mut BTreeSet<String>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    if matches!(name, "target" | ".git" | "fuzz" | "flask" | "werkzeug") {
                        continue;
                    }
                    walk(&p, found);
                } else if p.extension().is_some_and(|x| x == "rs")
                    && let Ok(s) = std::fs::read_to_string(&p)
                {
                    for cap in find_codes(&s) {
                        found.insert(cap);
                    }
                }
            }
        }
        // Only each crate's src/ tree — never its tests/.
        let Ok(entries) = std::fs::read_dir(&crates) else {
            panic!("cannot read {}", crates.display());
        };
        for e in entries.flatten() {
            let src = e.path().join("src");
            if src.is_dir() {
                walk(&src, &mut found);
            }
        }
        let registered: BTreeSet<String> = REGISTRY.iter().map(|c| c.code.to_string()).collect();
        let orphans: Vec<&String> = found.iter().filter(|c| !registered.contains(*c)).collect();
        assert!(
            orphans.is_empty(),
            "codes emitted in source but missing from the registry: {orphans:?}"
        );
    }

    /// Extract `JC####` / `JL####` tokens from a source string.
    fn find_codes(s: &str) -> Vec<String> {
        let bytes = s.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i + 6 <= bytes.len() {
            let w = &bytes[i..i + 6];
            let is_code = (w[0] == b'J')
                && (w[1] == b'C' || w[1] == b'L')
                && w[2..].iter().all(u8::is_ascii_digit);
            if is_code {
                // ensure not part of a longer alnum run
                let before_ok = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
                let after_ok = i + 6 == bytes.len() || !bytes[i + 6].is_ascii_alphanumeric();
                if before_ok && after_ok {
                    out.push(String::from_utf8_lossy(w).to_string());
                }
            }
            i += 1;
        }
        out
    }
}
