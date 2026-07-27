//! Typed model of design.json (docs/contracts/design-schema.json).
//! `deny_unknown_fields` mirrors the schema's `additionalProperties: false`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Design {
    pub name: String,
    pub contract_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// App-level mount prefix applied ONCE to every module (and bucket) mount at
    /// app assembly, e.g. `/v1` → all routes serve under `/v1`. Health (`/healthz`)
    /// and metrics (`/metrics`) stay unprefixed. Empty/`/`/absent is a no-op.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_path: Option<String>,
    /// App-level CORS policy (installed via `App::cors`). Present ⇒ the generated
    /// main.rs wires a cross-origin policy for the listed origins; absent ⇒ no
    /// CORS layer (same-origin only). The allowed origins are overridable at
    /// deploy time via `JERRYCAN_CORS_ORIGINS` (comma-separated), so a cross-origin
    /// SPA can be re-pointed without hand-editing the tool-owned main.rs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cors: Option<CorsDesign>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<Auth>,
    /// App-scoped dependency names the generator must provide on App.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenancy: Option<Tenancy>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub jobs: Vec<JobDesign>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<StorageDesign>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub realtime: Option<RealtimeDesign>,
    pub modules: Vec<ModuleDesign>,
}

/// The top-level `cors` block: a design-modeled CORS policy the generator installs
/// via `App::cors(CorsConfig::new(..))`. Maps 1:1 onto jerrycan_core's
/// `CorsConfig`/`CorsOrigins` — no options core can't honor. Serving a cross-origin
/// SPA (console on one origin, API on another) is declarative here instead of a
/// hand-edit of the tool-owned main.rs that the next `jerrycan generate` would wipe.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorsDesign {
    /// Allowed origins: exact `scheme+host[:port]` strings, or the single marker
    /// `"*"` for any origin (`CorsOrigins::any()` → `Access-Control-Allow-Origin: *`).
    /// Overridable at deploy time via `JERRYCAN_CORS_ORIGINS` (comma-separated).
    pub origins: Vec<String>,
    /// Methods allowed on preflight (`allow_methods`). Empty ⇒ core reflects the
    /// route's real methods.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub methods: Vec<HttpMethod>,
    /// Request headers allowed on preflight (`allow_headers`). Empty ⇒ core reflects
    /// the request's `Access-Control-Request-Headers`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<String>,
    /// Emit `Access-Control-Allow-Credentials: true` (`allow_credentials`). The
    /// Fetch spec forbids combining this with `"*"` origins (core's `App::build`
    /// rejects it; validation catches it at design time).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_credentials: bool,
}

impl CorsDesign {
    /// True when the origins are the single wildcard marker `"*"` (maps to
    /// `CorsOrigins::any()`). Validation guarantees `"*"` is never mixed with
    /// explicit origins, so any `"*"` present means "any origin".
    pub fn is_any(&self) -> bool {
        self.origins.iter().any(|o| o == "*")
    }
}

/// The `realtime` block (contract v2): row-change subscriptions (scope-filtered
/// by owner/tenant), ephemeral broadcast topics, and presence topics, served
/// over one WebSocket endpoint at `/realtime`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RealtimeDesign {
    /// Entity names whose row changes are subscribable (published +
    /// REPLICA IDENTITY FULL + scope-filtered delivery).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub broadcast: Vec<RealtimeTopic>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub presence: Vec<RealtimeTopic>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RealtimeTopic {
    pub name: String,
    pub scope: RealtimeScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RealtimeScope {
    None,
    Tenant,
    Auth,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Auth {
    pub model: AuthModel,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthModel {
    None,
    Session,
    Jwt,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModuleDesign {
    pub name: String,
    /// Mount prefix; defaults to "/" + name (see `effective_mount`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mount: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<Entity>,
    pub endpoints: Vec<Endpoint>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subroutes: Vec<ModuleDesign>,
    /// Module-scoped dependency names the generator must stub.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entity {
    pub name: String,
    /// Explicit SQL table name, used verbatim when present. Absent ⇒ the default
    /// `snake_case(name)` pluralized (see `Design::table_name`). Lets a frozen
    /// external schema keep its exact table name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub belongs_to: Vec<BelongsTo>,
    /// The public-read/owner-write shape (issue #105): on an identity-owned,
    /// non-tenant entity in an auth design, READS (GET list + detail) are public
    /// — unguarded and unscoped, a list returns EVERY owner's rows (the feed
    /// intent) — while WRITES (POST/PUT/PATCH/DELETE) stay owner-scoped and
    /// guarded exactly as issue #79. Valid ONLY on that per-user shape; JC0549
    /// rejects it anywhere else (tenant-owned, no identity fk, no auth model) and
    /// rejects any public/unguarded write on the entity. Serde-default false and
    /// skipped when false, so every existing design round-trips byte-identically.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub public_read: bool,
    /// Table-level composite UNIQUE constraints (issue #115): each inner vec is one
    /// `UNIQUE(col, …)` over ≥2 columns, so a "one row per (a,b)" invariant is a DB
    /// constraint (a duplicate is 409, no TOCTOU) instead of a racy SELECT-then-INSERT.
    /// Each column is a field name OR a `belongs_to` fk column. Single-column
    /// uniqueness stays `Field.unique`. Serde-default empty + skipped so every existing
    /// design round-trips byte-identically.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unique: Vec<Vec<String>>,
    pub fields: Vec<Field>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Field {
    pub name: String,
    #[serde(rename = "type")]
    pub field_type: FieldType,
    #[serde(default = "default_true")]
    pub required: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unique: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub index: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub values: Option<Vec<String>>,
    /// A server-owned default value (issue #53): a field that carries a `default`
    /// is dropped from the generated request DTO / OpenAPI request schema and the
    /// happy-path probe body — the client never sends it, the server applies the
    /// declared value. Lets a design express `confirmed (bool, default false)` /
    /// `status (enum, default "active")` without forcing the client to POST a
    /// server-controlled key (the field stays required NOT-NULL in the entity/DB).
    /// Validation type-checks the value against `field_type` (and enum `values`
    /// membership when present).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
    /// Inclusive integer lower bound (issue #80). Integer fields only; JC0552
    /// refuses misplacement, `min > max`, and any constraint on the pk `id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<i64>,
    /// Inclusive integer upper bound (issue #80). Integer fields only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<i64>,
    /// Inclusive minimum string length in Unicode code points (issue #80).
    /// String fields only, never combined with `values`; capped at 4096 so a
    /// fixture `"a".repeat(min_len)` stays bounded. `u64` makes a negative
    /// length a parse error for free.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_len: Option<u64>,
    /// Inclusive maximum string length in Unicode code points (issue #80).
    /// String fields only, never combined with `values`; `max_len: 0` on a
    /// required field is refused as unfillable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_len: Option<u64>,
    /// Response-hidden output field (issue #112): the field is emitted with
    /// `#[serde(skip_serializing)]` on the generated `Model`, so it never
    /// serializes into an API response, while staying present for DESERIALIZE
    /// (still accepted on create/update input) and for the SeaORM row
    /// round-trip. `password_hash` is auto-classified as write-only even without
    /// this flag (see `Design::field_is_write_only`, the secure-by-default). Serde-
    /// default false and skipped when false, so every existing design round-trips
    /// byte-identically. JC0554 refuses an explicit `write_only` on the pk `id`
    /// (the id must be returned).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub write_only: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FieldType {
    String,
    Integer,
    Float,
    Boolean,
    Datetime,
    Uuid,
    Json,
}

impl FieldType {
    /// The Rust type the generator emits. datetime/uuid ride as String until
    /// jerrycan-validate lands richer types in Phase 2 (documented in templates).
    pub fn rust_type(self) -> &'static str {
        match self {
            FieldType::String | FieldType::Datetime | FieldType::Uuid => "String",
            FieldType::Integer => "i64",
            FieldType::Float => "f64",
            FieldType::Boolean => "bool",
            FieldType::Json => "serde_json::Value",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BelongsTo {
    pub entity: String,
    #[serde(default)]
    pub on_delete: OnDelete,
    /// Optional fk-column alias (issue #119): the fk column becomes `{as}_id`
    /// instead of `snake(entity)_id`, so two `belongs_to` the same entity (a
    /// ledger's from/to account, a self-reference) coexist. `as` is a Rust
    /// keyword — the field is `r#as` with `#[serde(rename = "as")]`. Absent ⇒
    /// today's `snake(entity)_id`, byte-identical.
    #[serde(default, rename = "as", skip_serializing_if = "Option::is_none")]
    pub r#as: Option<String>,
}

impl BelongsTo {
    /// The fk column this belongs_to derives: `{as}_id` when aliased, else the
    /// default `snake(entity)_id` (issue #119). Every fk column derived FROM a
    /// `belongs_to` MUST come through here; a fk derived from a bare entity name
    /// (the tenancy/identity fk) keeps [`Design::fk_column`]. Falls through to
    /// `Design::fk_column(&self.entity)` when unaliased, so a `belongs_to` with
    /// no `as` is byte-identical to today.
    pub fn fk_column(&self) -> String {
        match &self.r#as {
            Some(a) => format!("{a}_id"),
            None => Design::fk_column(&self.entity),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnDelete {
    Cascade,
    SetNull,
    #[default]
    Restrict,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tenancy {
    pub entity: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub member_roles: Vec<String>,
}

/// One JOIN in a transitive tenant-ownership chain: the child table is joined to
/// its parent on `child_table.child_fk = parent_table.id`. `child_fk` is the
/// child's foreign key to that parent (`Design::fk_column(parent)`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JoinLink {
    pub child_table: String,
    pub child_fk: String,
    pub parent_table: String,
}

/// The unique `belongs_to` chain that scopes an entity to the tenant (issue #102:
/// transitive tenant ownership). Built by [`Design::tenant_path`]. `joins` walk
/// from the entity's own table up to `anchor_table` — the table that *directly*
/// `belongs_to` the tenant and so carries `tenant_fk`. An empty `joins` means the
/// entity IS the anchor (a direct child), so this subsumes the old direct
/// predicate. Gains SQL helpers in a later task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TenantPath {
    /// JOINs from the entity's own table up to `anchor_table`. Empty ⇒ direct child.
    pub joins: Vec<JoinLink>,
    /// Table carrying the tenant fk (== `entity_table` for a direct child).
    pub anchor_table: String,
    /// The tenant fk column on `anchor_table`, e.g. `org_id`.
    pub tenant_fk: String,
    /// The entity's own table (the SELECT/DELETE target).
    pub entity_table: String,
}

impl TenantPath {
    /// The JOIN clause walking from the entity's own table up to `anchor_table`,
    /// e.g. ` JOIN accounts ON contacts.account_id = accounts.id`. Empty for a
    /// direct child (no joins), so a direct child's SQL stays unchanged.
    pub(crate) fn join_sql(&self) -> String {
        self.joins
            .iter()
            .map(|j| {
                format!(
                    " JOIN {p} ON {c}.{fk} = {p}.id",
                    p = j.parent_table,
                    c = j.child_table,
                    fk = j.child_fk,
                )
            })
            .collect()
    }

    /// The qualified tenant fk column, e.g. `accounts.org_id`.
    pub(crate) fn tenant_col(&self) -> String {
        format!("{}.{}", self.anchor_table, self.tenant_fk)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobDesign {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue: Option<String>,
}

/// Contract v2: the top-level `storage` block — design-modeled object buckets.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageDesign {
    /// Mount prefix for every bucket: each serves under `{base_path}/{bucket}`
    /// (default `/storage`, see `effective_base_path`), keeping bucket routes
    /// clear of module mounts. A bucket named `media` no longer collides with a
    /// module mounted at `/media`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_path: Option<String>,
    pub buckets: Vec<BucketDesign>,
}

impl StorageDesign {
    /// The normalized bucket mount prefix: the `base_path` override (validation
    /// guarantees it starts with `/` and has no trailing slash) or `/storage`.
    pub fn effective_base_path(&self) -> String {
        self.base_path
            .clone()
            .unwrap_or_else(|| "/storage".to_string())
    }
}

/// One bucket: mounts at `/<name>` with generated guarded endpoints.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BucketDesign {
    /// `^[a-z][a-z0-9-]*$` — becomes the mount and the generated module ident.
    pub name: String,
    pub visibility: Visibility,
    /// Owning entity: the tenancy entity (tenant-owned bucket) or any other
    /// declared entity (the authenticated user id stamps owner_id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// Keys stored under `{owner_id}/…` with a prefix assertion on every
    /// access (Supabase folder-per-user parity). Requires `owner`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub owner_prefix: bool,
    /// Per-object cap, e.g. "5MB" (^[0-9]+(B|KB|MB|GB)?$). Default 50MB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_size: Option<String>,
    /// Content-type allowlist (globs like "image/*"). Empty = allow all.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_mime: Vec<String>,
    /// Tenant member roles allowed to write (upload/delete). Empty = any member
    /// may write (backward-compatible: without it a read-only-role member could
    /// upload bytes and delete others' uploads). Only meaningful on a
    /// tenant-scoped bucket; each entry must be a declared member_role. // #132
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub write_roles: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    Public,
    Private,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Endpoint {
    pub operation_id: String,
    pub method: HttpMethod,
    pub path: String,
    #[serde(default)]
    pub auth_required: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_roles: Vec<String>,
    /// Genuinely public route (credential-issuing login/register, public
    /// webhooks): exempt from JL0004 (unguarded-mutation) and from generated 401
    /// tests. Validation forbids combining it with auth_required/required_roles.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub public: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_body: Option<RequestBody>,
    /// How the generator probes this endpoint's success. Default `auto`. Set
    /// `skip` for an endpoint whose success needs a credential/signature the
    /// generator can't synthesize (login, signed webhook, api-key route): an
    /// uncredentialed 2xx probe could never pass, so `jerrycan check` could never
    /// reach `ok:true`. With `skip`, the generator emits a TODO for the author to
    /// write the credentialed success + rejection tests instead.
    #[serde(default, skip_serializing_if = "ProbePolicy::is_auto")]
    pub probe: ProbePolicy,
    pub success: Success,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<ErrorCase>,
}

/// Per-endpoint control over the generated happy-path success probe (issue #11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProbePolicy {
    /// Emit the happy-path 2xx probe (the default for every ordinary endpoint).
    #[default]
    Auto,
    /// Do NOT emit the 2xx probe — the endpoint authenticates via a credential
    /// the generator can't supply, so the author owns its success/rejection tests.
    Skip,
}

impl ProbePolicy {
    /// `true` for the default `auto` policy (so serialization can skip it).
    pub fn is_auto(&self) -> bool {
        matches!(self, ProbePolicy::Auto)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HttpMethod {
    GET,
    POST,
    PUT,
    PATCH,
    DELETE,
}

impl HttpMethod {
    /// The jerrycan-core free-fn name used in generated `module()` route tables.
    pub fn builder_fn(self) -> &'static str {
        match self {
            HttpMethod::GET => "get",
            HttpMethod::POST => "post",
            HttpMethod::PUT => "put",
            HttpMethod::PATCH => "patch",
            HttpMethod::DELETE => "delete",
        }
    }

    /// The `http::Method` associated-constant name (`GET`, `POST`, …) for emitting a
    /// CORS `allow_methods([jerrycan::http::Method::GET, …])` list into generated
    /// code (issue #21). The variant names already match the `http` constants.
    pub fn as_http_const(self) -> &'static str {
        match self {
            HttpMethod::GET => "GET",
            HttpMethod::POST => "POST",
            HttpMethod::PUT => "PUT",
            HttpMethod::PATCH => "PATCH",
            HttpMethod::DELETE => "DELETE",
        }
    }

    /// True for a REPLACE/MODIFY method (PUT/PATCH) — the "update" shape, as
    /// opposed to POST (create). Distinguishes the request DTO by write path: a
    /// `default` field is server-owned on CREATE (dropped) but client-settable on
    /// UPDATE (kept), so update keeps it in the body (issue #85 D1).
    pub fn is_update(self) -> bool {
        matches!(self, HttpMethod::PUT | HttpMethod::PATCH)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestBody {
    /// The table entity this body deserializes into (today's shape). Absent for an
    /// inline DTO body (issue #122). Exactly one of `entity` / `fields` is set
    /// (JC0561). Skipped-when-None so every entity `request_body` round-trips
    /// byte-identically.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
    /// Inline DTO body (issue #122): a custom-action body that is NOT a table row
    /// (`POST /checkout { coupon, total }`). Reuses `Field` (types + #80
    /// constraints). No pk, no belongs_to — a plain request struct named
    /// `{Pascal(operation_id)}Request`. Exactly one of `entity` / `fields` (JC0561).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<Field>,
}

impl RequestBody {
    /// True for an inline DTO body (issue #122) — no table entity, an ad-hoc
    /// `fields` struct. `false` for the entity-ref shape (today's default). The ONE
    /// predicate every `entity`/`fields` branch keys on so the sites stay readable.
    pub fn is_inline(&self) -> bool {
        self.entity.is_none()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Success {
    pub status: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
    #[serde(default)]
    pub list: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorCase {
    pub status: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub when: String,
}

impl Endpoint {
    /// This endpoint needs an authenticated user (and maybe a role).
    pub fn is_guarded(&self) -> bool {
        self.auth_required || !self.required_roles.is_empty()
    }

    /// True when this endpoint carries its OWN request authentication via a
    /// cryptographic signature (the Stripe-style webhook pattern): the design
    /// declares a 4xx error case whose `when` names a signature check. Such an
    /// endpoint is intentionally NOT JWT/session-guarded — its caller is a third
    /// party that can't hold the app's session, so it proves itself by signing the
    /// payload. JL0004 (the unguarded-mutation lint) treats this as guarded so it
    /// doesn't false-positive a deliberately signature-authenticated webhook.
    pub fn declares_signature_auth(&self) -> bool {
        self.errors
            .iter()
            .any(|e| (400..500).contains(&e.status) && e.when.to_lowercase().contains("signature"))
    }
}

impl ModuleDesign {
    /// Where this module mounts (under the app, or under its parent for subroutes).
    pub fn effective_mount(&self) -> String {
        self.mount
            .clone()
            .unwrap_or_else(|| format!("/{}", self.name))
    }
}

/// The fixed column the generated `{tenant}_members` table and the `Tenant`
/// guard use for the authenticated principal: the membership DDL emits a
/// `user_id` column (see `genroute` `write_module_migrations`) and the guard
/// factory queries `WHERE user_id = ?` (see `scaffold::shared_tenancy_types`).
/// It is a FIXED name, not a design-named identity entity, so identity checks
/// (JC0540's tenancy collision, the server-owned-FK rule) compare a derived fk
/// column against it rather than against a hardcoded `User` string.
pub(crate) const AUTH_IDENTITY_FK_COLUMN: &str = "user_id";

/// How an endpoint binds the tenant, so an ownership guard (issues #78/#79) knows
/// WHAT to verify before touching a row. Derived from the endpoint's resolved path
/// (mount + `ep.path`) relative to the design's `tenancy` block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TenantShape {
    /// The resolved URL carries the tenant fk `{fk_param}` (e.g. `/clubs/{club_id}/…`,
    /// or the tenant entity's own detail route `/{club_id}` / `/{id}`). The guard
    /// verifies the caller belongs to the tenant named IN THE PATH — the cross-tenant
    /// leak surface (#78).
    PathScoped { fk_param: String },
    /// A flat tenant-owned route whose URL carries NO tenant param (e.g. a
    /// module mounted at `/customers`, not under `/clubs/{club_id}`). The set of
    /// rows the caller may see is defined implicitly by their memberships — the
    /// per-user leak surface (#79).
    MembershipSet,
    /// The tenant entity's own collection root (`POST "/"` / `GET "/"` on the
    /// tenant module): create a new tenant, or list the caller's tenants.
    Collection,
    /// Not a tenant-scoped endpoint (no `tenancy` block, or a module that is
    /// neither the tenant module nor owns a tenant-owned entity).
    None,
}

/// One handler file the JL0006 scan must read, resolved to its REAL on-disk path
/// (issue #103). A top-level module lives at `crates/routes/{name}/src/handlers.rs`;
/// a subroute nests under its parent as `.../src/subroutes/{seg}/handlers.rs` (the
/// same layout the scaffold writes, `-`→`_` per segment). The old lint assumed the
/// flat path `crates/routes/{module}/src/handlers.rs` even for a nested/transitive
/// module, so a grandchild handler resolved to a nonexistent file and was silently
/// skipped — the hole that let the #102 transitive leak ship unseen. Carries the
/// ownership wording so the scan emits the right leak class (tenant vs per-user)
/// and the `is_flat` flag that decides whether a bare `repo.insert` is a leak (#94).
pub struct HandlerRef {
    /// Path relative to the app root, e.g. `crates/routes/accounts/src/subroutes/contacts/handlers.rs`.
    pub rel_path: String,
    /// True when the owning module is a FLAT (membership-set) tenant module, where a
    /// bare `repo.insert` reads the tenant fk from the BODY and so is a write leak (#94).
    pub is_flat: bool,
    /// "a tenant-owned" / "an identity-owned" — the ownership class in the message.
    pub owned_desc: &'static str,
    /// "another tenant's rows" / "another user's rows" — what an unscoped call leaks.
    pub leak_desc: &'static str,
    /// The registered fix text (the scoped accessors to call instead).
    pub suggestion: String,
    /// Handler fn names (operation_ids, per the JL0002 contract) whose
    /// `get`/`update`/`remove`/`insert` hits JL0006 must NOT flag (issue #124):
    /// the TENANT entity's own PathScoped detail handlers, where membership in
    /// the path tenant was already verified by the `Dep<Tenant>` guard and the
    /// tenant repo intentionally keeps its unscoped methods (per-user
    /// suppression only). `repo.all()` stays armed even in these fns —
    /// fn-level suppression cannot see which repo the `repo` binding holds,
    /// and a correct detail handler calls `get`, not `all`. Empty for every
    /// per-user ref and for tenant modules without a hosted child's handlers.
    pub exempt_fns: std::collections::BTreeSet<String>,
}

/// The JL0006 fix text for a TENANT-owned handler (both route shapes, issue #94):
/// a FLAT handler cannot call the path-scoped `*_for` accessors (there is no
/// tenant-id arg), so the membership-set methods are named too.
pub(crate) const TENANT_SCOPED_SUGGESTION: &str = "call a scoped accessor instead — path-scoped routes: all_for/get_for/remove_for with the tenant id; flat (membership-set) routes: all_for_memberships/get_for_memberships/update_for_memberships/remove_for_memberships (and create_for_memberships) with the session user's id (_user.0.id)";

impl Design {
    /// The tenant-owned handler files the JL0006 scan must read, one per module or
    /// subroute that owns ≥1 tenant-owned entity (directly OR transitively, #102),
    /// each at its REAL nested on-disk path (issue #103 — see [`HandlerRef`]). Empty
    /// when there is no tenancy. `is_flat` is OR-ed over the module's owned entities.
    pub fn tenant_owned_handlers(&self) -> Vec<HandlerRef> {
        let mut out = Vec::new();
        if self.tenancy.is_none() {
            return out;
        }
        for m in &self.modules {
            self.collect_owned_handlers(&format!("crates/routes/{}/src", m.name), m, &mut out);
        }
        out
    }

    /// Walk one module and its subroutes, emitting a [`HandlerRef`] for each node
    /// that owns a tenant-owned entity. `src_rel` is the node's on-disk src dir,
    /// extended by `/subroutes/{seg}` per nesting level exactly as the scaffold
    /// (and the JL0002/JL0007 walks) nest — so the path always points at a real file.
    fn collect_owned_handlers(&self, src_rel: &str, m: &ModuleDesign, out: &mut Vec<HandlerRef>) {
        let owned: Vec<&Entity> = m
            .entities
            .iter()
            .filter(|e| self.tenant_path(&e.name).is_some())
            .collect();
        if !owned.is_empty() {
            let is_flat = owned
                .iter()
                .any(|e| super::genroute::entity_is_flat_tenant_owned(e, self));
            // Issue #124: a tenant module that also hosts a tenant-owned child
            // drags the tenant's OWN handlers into this scan, where the
            // PathScoped detail handlers legitimately call the unscoped
            // `repo.get/update/remove` on the TENANT repo (membership in the
            // path tenant is already guard-verified). Exempt exactly those fns
            // — resolved with the STRICT repo-entity resolver on purpose: a
            // lint must UNDER-exempt (a residual false positive has the
            // line-scoped allow hatch) and never OVER-exempt (which would
            // silence a real leak in a handler the fallback mis-bound). The
            // `is_guarded()` conjunct is load-bearing: genroute emits the
            // membership-checking `Dep<Tenant>` param only for guarded
            // endpoints, so an UNGUARDED (or `public: true`) tenant detail
            // route has NO guard — its unscoped `repo.get/update/remove` is an
            // anonymous tenant read/write and must stay armed.
            let exempt_fns: std::collections::BTreeSet<String> = self
                .tenancy
                .as_ref()
                .map(|t| {
                    m.endpoints
                        .iter()
                        .filter(|ep| {
                            endpoint_repo_entity_strict(m, ep) == Some(t.entity.as_str())
                                && matches!(
                                    self.endpoint_tenant_shape(m, ep),
                                    TenantShape::PathScoped { .. }
                                )
                                && ep.is_guarded()
                        })
                        .map(|ep| ep.operation_id.clone())
                        .collect()
                })
                .unwrap_or_default();
            out.push(HandlerRef {
                rel_path: format!("{src_rel}/handlers.rs"),
                is_flat,
                owned_desc: "a tenant-owned",
                leak_desc: "another tenant's rows",
                suggestion: TENANT_SCOPED_SUGGESTION.to_string(),
                exempt_fns,
            });
        }
        for sub in &m.subroutes {
            self.collect_owned_handlers(
                &format!("{src_rel}/subroutes/{}", sub.name.replace('-', "_")),
                sub,
                out,
            );
        }
    }

    /// The normalized app-level mount prefix: the `base_path` override, or `""`
    /// when absent / `/` / empty (a no-op). Prepended to every module and bucket
    /// mount at app assembly; health/metrics are unaffected. Validation
    /// guarantees a non-empty prefix starts with `/` and has no trailing slash,
    /// so joining is a plain concatenation.
    pub fn base_prefix(&self) -> &str {
        match self.base_path.as_deref() {
            None | Some("") | Some("/") => "",
            Some(p) => p,
        }
    }

    /// Reserved dependency name `db` switches generation to SQL mode.
    pub fn wants_db(&self) -> bool {
        self.dependencies.iter().any(|d| d == "db")
    }

    /// Reserved dependency name `validate` mounts the OpenAPI document.
    pub fn wants_validate(&self) -> bool {
        self.dependencies.iter().any(|d| d == "validate")
    }

    /// Auth mode: a non-`none` auth model, or the reserved `auth` dependency.
    /// Triggers session-user types in shared, guard params, and the `Auth`
    /// extension in main.rs.
    pub fn wants_auth(&self) -> bool {
        self.auth
            .as_ref()
            .map(|a| a.model != AuthModel::None)
            .unwrap_or(false)
            || self.dependencies.iter().any(|d| d == "auth")
    }

    /// The active auth model, defaulting to `None` when no `auth` block is set.
    /// A guard/codegen decision (cookie vs Bearer) keys on this.
    pub fn auth_model(&self) -> AuthModel {
        self.auth
            .as_ref()
            .map(|a| a.model)
            .unwrap_or(AuthModel::None)
    }

    /// The HTTP header the GENERATED acceptance tests thread the test credential
    /// through: `authorization` (a `Bearer <jwt>` token) under the `jwt` model,
    /// else `cookie` (a `jerrycan_session=…` cookie). Session/none output stays
    /// byte-identical (issue #29 — jwt REST routes get real Bearer guards).
    pub(crate) fn test_auth_header(&self) -> &'static str {
        match self.auth_model() {
            AuthModel::Jwt => "authorization",
            _ => "cookie",
        }
    }

    /// The role the generated test credential (`test_cookie_for`) is minted with
    /// (issue #67). A `require_role`-guarded handler 403s a credential whose
    /// `SessionUser.role` doesn't match, so a hardcoded `"admin"` made every
    /// role-gated probe un-greenable for a design whose roles don't include it
    /// (HelpDesk: agent/customer). The rule: the first `required_roles` entry any
    /// endpoint declares (the role a correct handler will demand — first listed),
    /// else the design's first declared `auth.roles`, else `"admin"` only when the
    /// design declares no roles at all (keeps roleless output byte-identical).
    pub(crate) fn test_credential_role(&self) -> &str {
        fn first_gate(m: &ModuleDesign) -> Option<&str> {
            m.endpoints
                .iter()
                .find_map(|ep| ep.required_roles.first())
                .map(String::as_str)
                .or_else(|| m.subroutes.iter().find_map(first_gate))
        }
        self.modules
            .iter()
            .find_map(first_gate)
            .or_else(|| {
                self.auth
                    .as_ref()
                    .and_then(|a| a.roles.first())
                    .map(String::as_str)
            })
            .unwrap_or("admin")
    }

    /// Reserved dependency name `observe` wires logging + the metrics/health
    /// extension. Pure extension wiring — no per-route codegen.
    pub fn wants_observe(&self) -> bool {
        self.dependencies.iter().any(|d| d == "observe")
    }

    /// Declared background jobs switch on the generated `crates/jobs/` crate (the
    /// typed task stubs + the dispatch registry) and the `Jobs` extension wiring
    /// in main.rs. Jobs are top-level (not per-module), so this gates a single
    /// top-level crate. Jobs require a database (the engine's default store is
    /// Postgres); validation rejects jobs without a `db` dependency.
    pub fn wants_jobs(&self) -> bool {
        !self.jobs.is_empty()
    }

    /// A declared storage block (with buckets) switches on the generated
    /// `crates/storage/` crate, the Storage extension + STORAGE_MIGRATIONS
    /// wiring in main.rs, and the `storage-s3` facade feature. Storage requires
    /// `db` (metadata table) and an active auth model (mutations are always
    /// guarded); validation rejects designs missing either.
    pub fn wants_storage(&self) -> bool {
        self.storage.as_ref().is_some_and(|s| !s.buckets.is_empty())
    }

    /// The `realtime` block switches on the realtime crate wiring + the facade
    /// `realtime` feature. Like jobs, the block itself is the declaration (any
    /// of changes/broadcast/presence populated).
    pub fn wants_realtime(&self) -> bool {
        self.realtime.as_ref().is_some_and(|r| {
            !r.changes.is_empty() || !r.broadcast.is_empty() || !r.presence.is_empty()
        })
    }

    /// The first broadcast topic a SERVER handler may publish to via
    /// `RealtimeHandle::publish` (issue #50): scope `none` or `auth`, i.e.
    /// un-partitioned. A `tenant`-scoped topic is excluded — a server publish has
    /// no connection principal to derive a tenant partition from, so the runtime
    /// `publish` rejects it. `None` when the design declares no such topic, which
    /// is what keeps realtime-free AND tenant-only-broadcast designs' generated
    /// handlers byte-identical (no dep, no stub comment).
    pub fn server_publishable_broadcast(&self) -> Option<&str> {
        self.realtime
            .as_ref()?
            .broadcast
            .iter()
            .find(|t| matches!(t.scope, RealtimeScope::None | RealtimeScope::Auth))
            .map(|t| t.name.as_str())
    }

    /// "5MB" → bytes. Uppercase B/KB/MB/GB suffixes (binary multiples); a bare
    /// number is bytes. None = unparseable (a validation question).
    pub fn parse_size(s: &str) -> Option<u64> {
        let (num, mult) = if let Some(n) = s.strip_suffix("GB") {
            (n, 1024 * 1024 * 1024)
        } else if let Some(n) = s.strip_suffix("MB") {
            (n, 1024 * 1024)
        } else if let Some(n) = s.strip_suffix("KB") {
            (n, 1024)
        } else if let Some(n) = s.strip_suffix('B') {
            (n, 1)
        } else {
            (s, 1)
        };
        // checked_mul: overflow reads as unparseable (a validation question),
        // never a debug panic or a silently wrapped size.
        num.parse::<u64>().ok().and_then(|n| n.checked_mul(mult))
    }

    /// Reserved dependency name `oauth` enables the facade `oauth` feature, so a
    /// generated handler can use `jerrycan::auth::oauth::{OAuthClient, Provider}`
    /// (the OAuth2 authorization-code client). The facade `oauth` feature implies
    /// `auth`, so the auth surface is available even without a separate `auth`
    /// dependency. The client is constructed in agent-owned handler code (no
    /// app-level extension wiring), so this only gates the Cargo feature.
    pub fn wants_oauth(&self) -> bool {
        self.dependencies.iter().any(|d| d == "oauth")
    }

    /// The facade features this design's mode requires on the `jerrycan` dep,
    /// in a stable order (scaffold and mounting must agree byte-for-byte).
    pub fn facade_features(&self) -> Vec<&'static str> {
        let mut features = Vec::new();
        if self.wants_db() {
            features.push("db");
        }
        if self.wants_validate() {
            features.push("validate");
        }
        if self.wants_auth() {
            features.push("auth");
        }
        if self.wants_observe() {
            features.push("observe");
        }
        if self.wants_jobs() {
            features.push("jobs");
        }
        // Appended last so existing designs' feature order is unchanged.
        if self.wants_oauth() {
            features.push("oauth");
        }
        // Appended after oauth so existing designs' feature order is unchanged.
        // storage-s3 (implies storage): the S3 backend must be compiled into
        // every storage app so JERRYCAN_STORAGE switches backends by env alone.
        if self.wants_storage() {
            features.push("storage-s3");
        }
        // Appended last (after storage) so existing designs' feature order is
        // unchanged. Realtime channels over WebSockets; changes imply db.
        if self.wants_realtime() {
            features.push("realtime");
        }
        features
    }

    pub fn from_path(path: &std::path::Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let mut design: Self =
            serde_json::from_str(&raw).map_err(|e| format!("invalid design.json: {e}"))?;
        design.normalize_tenant_detail_routes();
        Ok(design)
    }

    /// Rewrite the tenant entity's OWN detail-route param `{id}` → `{tenant_fk}`
    /// (e.g. `GET /clubs/{id}` → `GET /clubs/{club_id}`) before any generation, so
    /// the membership-verifying `tenant()` guard reads the tenant fk BY NAME from
    /// the path and 404s a non-member — closing the #78 cross-tenant leak on the
    /// tenant's own conventional `GET`/`PUT`/`PATCH`/`DELETE /{id}` route (whose
    /// router previously captured `id`, so the guard's path branch missed and fell
    /// back to an arbitrary first membership, reading/deleting a tenant the caller
    /// isn't a member of).
    ///
    /// Scoped to the module that DECLARES the tenant entity. A nested tenant-owned
    /// child (`/clubs/{club_id}/books/{id}` — `{id}` is the BOOK) and a storage
    /// bucket `/{id}` (the object KEY, generated outside `modules`) are NOT modules
    /// of the tenant entity, so they are untouched. Idempotent: a path already
    /// carrying `{club_id}` has no `{id}` token to rewrite. Applied at every design
    /// load (`from_path`) and after in-memory design-slice merges (the MCP
    /// `jerrycan_generate` route path), so no generation path can reopen the leak.
    pub(crate) fn normalize_tenant_detail_routes(&mut self) {
        let Some(tenancy) = self.tenancy.as_ref() else {
            return;
        };
        let entity = tenancy.entity.clone();
        let fk_token = format!("{{{}}}", Self::fk_column(&entity));
        for m in &mut self.modules {
            Self::normalize_own_detail_routes(m, &entity, &fk_token);
        }
    }

    /// Rename `{id}` → `fk_token` in the OWN endpoints of the module that declares
    /// `entity` (the tenant module); recurse so a tenant entity declared in a
    /// subroute is still found. Never touches a module that merely OWNS a
    /// tenant-owned child.
    ///
    /// Scoped to endpoints whose resolved repo entity IS the tenant entity
    /// (issue #89): a sibling entity hosted in the same module keeps its own
    /// `/{id}` detail routes — there `{id}` is the SIBLING's key, and renaming
    /// it to the tenant fk would mis-scope the sibling's detail route to the
    /// tenant guard. Resolution is the lenient [`endpoint_repo_entity`]: its
    /// first-entity fallback preserves the normalization of a bodyless tenant
    /// detail route (a `DELETE /{id}` with no success entity) in a
    /// single-entity tenant module. Because that resolver reads OTHER
    /// endpoints' paths (collection-creator resolution, #56), the targets are
    /// collected in an immutable pre-pass BEFORE any path is rewritten — never
    /// resolve mid-rename against half-rewritten collection paths.
    fn normalize_own_detail_routes(m: &mut ModuleDesign, entity: &str, fk_token: &str) {
        if m.entities.iter().any(|e| e.name == entity) {
            let targets: Vec<usize> = m
                .endpoints
                .iter()
                .enumerate()
                .filter(|(_, ep)| {
                    ep.path.contains("{id}") && endpoint_repo_entity(m, ep) == Some(entity)
                })
                .map(|(i, _)| i)
                .collect();
            for i in targets {
                let path = &mut m.endpoints[i].path;
                *path = path.replace("{id}", fk_token);
            }
        }
        for sub in &mut m.subroutes {
            Self::normalize_own_detail_routes(sub, entity, fk_token);
        }
    }

    /// The unique `belongs_to` chain from `entity` to `tenancy.entity`, or `None`
    /// when the entity is not tenant-owned, the tenant itself, ambiguous (a diamond
    /// — the validator raises `JC0545`, blocking generation), or there is no
    /// tenancy. A direct child yields zero joins, so this subsumes the old direct
    /// predicate. PURE — never emits diagnostics; the validator decides on ambiguity.
    pub(crate) fn tenant_path(&self, entity: &str) -> Option<TenantPath> {
        let tenancy = self.tenancy.as_ref()?;
        if entity == tenancy.entity {
            return None; // the tenant itself is not tenant-owned
        }
        let mut chains = self.tenant_path_chains(
            entity,
            &tenancy.entity,
            &mut std::collections::BTreeSet::new(),
        );
        // 0 chains = not owned; ≥2 = ambiguous (JC0545 blocks generation). Only a
        // single, unambiguous chain scopes the entity — never guess a half-path.
        if chains.len() != 1 {
            return None;
        }
        let joins = chains.pop().expect("exactly one chain");
        Some(TenantPath {
            anchor_table: joins
                .last()
                .map(|j| j.parent_table.clone())
                .unwrap_or_else(|| self.table_name(entity)),
            tenant_fk: Self::fk_column(&tenancy.entity),
            entity_table: self.table_name(entity),
            joins,
        })
    }

    /// Every distinct `belongs_to` chain from `entity` down to the entity that
    /// *directly* `belongs_to` `tenant`. A chain of `vec![]` means `entity` itself
    /// is that anchor (a direct child). Returns 0 chains (not owned), 1 (unique),
    /// or ≥2 (ambiguous diamond). PURE — no diagnostics; the caller decides.
    /// Cycle-safe: `visited` guards against a `belongs_to` loop.
    fn tenant_path_chains(
        &self,
        entity: &str,
        tenant: &str,
        visited: &mut std::collections::BTreeSet<String>,
    ) -> Vec<Vec<JoinLink>> {
        let Some(e) = self.find_entity(entity) else {
            return Vec::new();
        };
        if e.belongs_to.iter().any(|b| b.entity == tenant) {
            return vec![Vec::new()]; // direct anchor
        }
        if !visited.insert(entity.to_string()) {
            return Vec::new(); // cycle guard
        }
        let mut found = Vec::new();
        for b in &e.belongs_to {
            for rest in self.tenant_path_chains(&b.entity, tenant, visited) {
                let mut chain = vec![JoinLink {
                    child_table: self.table_name(entity),
                    child_fk: b.fk_column(),
                    parent_table: self.table_name(&b.entity),
                }];
                chain.extend(rest);
                found.push(chain);
            }
        }
        visited.remove(entity);
        found
    }

    /// How many distinct `belongs_to` chains reach the tenant (0/1/≥2). The
    /// validator raises `JC0545` when this is ≥2 (an ambiguous diamond).
    pub(crate) fn tenant_path_branch_count(&self, entity: &str) -> usize {
        let Some(t) = self.tenancy.as_ref() else {
            return 0;
        };
        if entity == t.entity {
            return 0;
        }
        self.tenant_path_chains(entity, &t.entity, &mut std::collections::BTreeSet::new())
            .len()
    }

    /// Entities owned by the tenant — directly OR transitively (issue #102): every
    /// entity (in any module or subroute) that resolves to a unique tenant path.
    /// (module_name, entity_name) pairs, in document order.
    pub fn tenant_owned(&self) -> Vec<(&str, &str)> {
        if self.tenancy.is_none() {
            return Vec::new();
        }
        let mut owned = Vec::new();
        for module in &self.modules {
            collect_tenant_owned(self, module, &mut owned);
        }
        owned
    }

    /// The fk column a belongs_to derives: snake_case(target) + "_id".
    pub fn fk_column(target: &str) -> String {
        format!("{}_id", Self::to_snake(target))
    }

    /// Whether a field is response-hidden (issue #112): emitted with
    /// `#[serde(skip_serializing)]` on the Model so it never appears in an API
    /// response, while staying accepted on input (create/update) and stored.
    /// True for a field that declares `write_only`, AND — secure-by-default,
    /// fail-CLOSED — for the one unambiguous secret column name `password_hash`
    /// (a password hash must never be in a response). The broad
    /// `*_hash`/`token`/`secret`/`api_key` name heuristic is deliberately NOT
    /// included: too fragile, since a `share_token`/`oauth_token` may legitimately
    /// be returned. Consumed by both Model emitters and the OpenAPI `field_schema`,
    /// so the explicit flag AND a `password_hash` column are hidden identically.
    pub fn field_is_write_only(f: &Field) -> bool {
        f.write_only || f.name.eq_ignore_ascii_case("password_hash")
    }

    /// Whether a field is the dynamic server-set timestamp (issue #110): a
    /// `datetime` field whose `default` is the exact lowercase sentinel `"now"`.
    /// Such a field is dropped from BOTH request DTOs (server-owned on create,
    /// immutable on update — a client must not rewrite `created_at`) and the create
    /// handler sets it to `now_rfc3339()`, distinct from a STATIC default (which is
    /// kept-settable on update and named as a literal). `"now"` never collides with
    /// a legitimate static datetime literal — no RFC3339 timestamp is the bare word
    /// `now` — so the sentinel is unambiguous. A near-miss casing (`"NOW"`) or a
    /// `"now"` on a non-datetime field is refused at validation (JC0557), never
    /// silently read here. False for every field without this exact shape, which is
    /// what keeps designs that don't use it byte-identical.
    pub fn field_is_now_default(f: &Field) -> bool {
        f.field_type == FieldType::Datetime && f.default.as_ref() == Some(&serde_json::json!("now"))
    }

    /// Classify how `ep` (in `module`) binds the tenant, so an ownership guard
    /// knows what to verify (issues #78/#79). Resolves the endpoint's full path
    /// (`module.effective_mount()`, trailing `/` trimmed, + `ep.path`) against the
    /// design's `tenancy` block. Classes (first match wins):
    ///   - no `tenancy`, or a module that neither declares the tenant entity nor
    ///     owns a tenant-owned entity → [`TenantShape::None`];
    ///   - the tenant entity's OWN module, `ep.path == "/"` (POST or GET) →
    ///     [`TenantShape::Collection`];
    ///   - the resolved path carries the tenant fk `{fk_param}` — OR the tenant
    ///     entity's own detail route `/{id}` — → [`TenantShape::PathScoped`]
    ///     (`fk_param` is always the tenant fk, even when the route param is `id`,
    ///     so callers scope on the tenant without a route-param rename);
    ///   - a tenant-owned entity whose resolved path carries no tenant param →
    ///     [`TenantShape::MembershipSet`].
    pub(crate) fn endpoint_tenant_shape(
        &self,
        module: &ModuleDesign,
        ep: &Endpoint,
    ) -> TenantShape {
        let Some(tenancy) = self.tenancy.as_ref() else {
            return TenantShape::None;
        };
        let is_tenant_module = module.entities.iter().any(|e| e.name == tenancy.entity);
        // Transitive tenant ownership (#102): a module owns a tenant-scoped entity
        // when any of its entities resolves to a tenant path — a direct child (zero
        // joins) OR a grandchild reached through a parent chain — not merely a direct
        // `belongs_to`. The PathScoped-vs-MembershipSet decision below still keys on
        // the resolved PATH, so a flat grandchild stays MembershipSet.
        let owns_tenant_entity = module
            .entities
            .iter()
            .any(|e| self.tenant_path(&e.name).is_some());
        if !is_tenant_module && !owns_tenant_entity {
            return TenantShape::None;
        }

        let fk_param = Self::fk_column(&tenancy.entity);
        let mount = module.effective_mount();
        let mount = mount.strip_suffix('/').unwrap_or(&mount);
        let resolved = format!("{mount}{}", ep.path);
        let fk_token = format!("{{{fk_param}}}");

        // The tenant entity's own module: collection root, then its detail route
        // (`/{club_id}` matched below via fk_token, or the conventional `/{id}`).
        if is_tenant_module {
            if ep.path == "/" && matches!(ep.method, HttpMethod::POST | HttpMethod::GET) {
                return TenantShape::Collection;
            }
            if resolved.contains(&fk_token) || ep.path.contains("{id}") {
                return TenantShape::PathScoped { fk_param };
            }
        }

        // Tenant-owned routes: path-scoped when the URL carries the tenant fk
        // (a nested mount like `/clubs/{club_id}`), else membership-scoped. A flat
        // child `/{id}` (no tenant fk in the path) is MembershipSet, NOT PathScoped
        // — so the conventional `{id}` shortcut is deliberately NOT applied here.
        if resolved.contains(&fk_token) {
            return TenantShape::PathScoped { fk_param };
        }
        if owns_tenant_entity {
            return TenantShape::MembershipSet;
        }
        TenantShape::None
    }

    /// True when this belongs_to targets the AUTH IDENTITY entity: its derived
    /// fk column is the fixed `user_id` linkage the membership table and the
    /// session guard key on (see `AUTH_IDENTITY_FK_COLUMN`). Identity is a
    /// COLUMN-name fact, not an entity-name one — the same resolution JC0540
    /// uses. Keys on `b.fk_column()`, so an ALIASED `belongs_to` the identity
    /// entity (issue #119, e.g. `as: "sender"` → `sender_id`) is correctly NOT
    /// the owner fk — a message's sender/recipient is a plain reference, not the
    /// authenticated owner; only an un-aliased `belongs_to User` → `user_id` is.
    pub(crate) fn is_identity_fk(b: &BelongsTo) -> bool {
        b.fk_column() == AUTH_IDENTITY_FK_COLUMN
    }

    /// True when the entity carries an identity FK (a belongs_to aimed at the
    /// auth identity entity). Such an entity's GUARDED request bodies omit
    /// `user_id` — the server injects the session user's id (issue #34).
    pub(crate) fn has_identity_fk(e: &Entity) -> bool {
        e.belongs_to.iter().any(Self::is_identity_fk)
    }

    /// True when `e` is OWNER-scoped by the AUTHENTICATED USER (issue #79): an
    /// auth design, an identity fk (`user_id` — a belongs_to aimed at the auth
    /// identity entity, the same COLUMN-name resolution JC0540/#34 use), and NOT
    /// tenant-owned — directly OR transitively (issue #102): an entity with a
    /// tenant path is scoped by the TENANT (via `scoped_methods`), never
    /// per-user. THE single per-user classifier (#105 §F): repo emission
    /// (genroute), the JC0549 validation (questions), the isolation-test shape
    /// (testgen), the JL0006 module scan (lints), and [`Self::entity_is_public_read`]
    /// all resolve through this ONE method, so the mirror sites cannot drift
    /// apart — mirror drift is exactly how the #102-class holes shipped.
    pub(crate) fn entity_is_per_user_owned(&self, e: &Entity) -> bool {
        self.wants_auth() && Self::has_identity_fk(e) && self.tenant_path(&e.name).is_none()
    }

    /// The public-read/owner-write classifier (issue #105): entity `entity`
    /// opted in via `public_read: true` AND is per-user owned. The per-user leg
    /// IS [`Self::entity_is_per_user_owned`] — the one shared classifier — so
    /// this flag, the repo emission, the testgen shape, and the lint config
    /// agree on WHICH entities get public reads with owner-gated writes.
    /// Resolves by NAME across the tree so a caller holding only an endpoint's
    /// repo-entity name agrees with one holding the `Entity`. False for every
    /// non-opt-in entity, so existing designs are untouched.
    pub(crate) fn entity_is_public_read(&self, entity: &str) -> bool {
        self.find_entity(entity)
            .is_some_and(|e| e.public_read && self.entity_is_per_user_owned(e))
    }

    /// True when this endpoint is a PUBLIC read on a `public_read` entity (issue
    /// #105): a GET whose repo entity opted into public_read runs UNGUARDED —
    /// regardless of its declared `auth_required` — so the read is public by
    /// construction (the entity flag drives it; the design doesn't hand-set auth
    /// per GET). Writes always keep their guard, and a role-gated GET
    /// (`required_roles`) keeps its guard too: an explicit role demand outranks
    /// the entity-level read-open default (stripping it would silently drop the
    /// role check the design asked for). THE single guarding-split predicate:
    /// handler emission (genroute), the OpenAPI `security` stanza (openapi), and
    /// the generated 401 probe (testgen) all resolve through this ONE method —
    /// keyed on each site's own `is_guarded()` reading, the trio would drift
    /// (the OpenAPI doc would advertise a credential on an unguarded handler and
    /// the acceptance suite would 401-probe a handler that correctly 200s: a
    /// permanently-red test on a correct app). False for every non-`public_read`
    /// design, keeping output byte-identical. Resolves the repo entity via
    /// [`endpoint_repo_entity_strict`] — explicit signals ONLY: an entity-less
    /// GET (custom-JSON success, no body, no `{param}` collection) binds no
    /// entity, so it KEEPS its declared guard. The lenient first-entity
    /// fallback would be fail-OPEN here: an `auth_required` `GET /stats` beside
    /// a `public_read` first entity would be reclassified as a public read it
    /// never performs, and the handler/OpenAPI/401-probe trio would ship it
    /// anonymous with a green gate.
    pub(crate) fn endpoint_is_public_read_get(&self, m: &ModuleDesign, ep: &Endpoint) -> bool {
        matches!(ep.method, HttpMethod::GET)
            && ep.required_roles.is_empty()
            && endpoint_repo_entity_strict(m, ep)
                .is_some_and(|entity| self.entity_is_public_read(entity))
    }

    /// The server-owned-FK rule (issue #34), design-level: a GUARDED endpoint
    /// whose request-body entity carries an identity FK omits that FK from the
    /// wire contract — the generated probe bodies and the OpenAPI request
    /// schema drop `user_id`, and the handler injects the authenticated session
    /// user's id. Unguarded endpoints keep the field (no session to inject);
    /// every other belongs_to FK stays required client input.
    pub(crate) fn endpoint_omits_identity_fk(&self, m: &ModuleDesign, ep: &Endpoint) -> bool {
        self.wants_auth()
            && ep.is_guarded()
            && ep.request_body.as_ref().is_some_and(|rb| {
                rb.entity.as_ref().is_some_and(|ent| {
                    m.entities
                        .iter()
                        .find(|e| &e.name == ent)
                        .is_some_and(Self::has_identity_fk)
                })
            })
    }

    /// The body entity of this endpoint that lives in `m`, if any (the request DTO
    /// is per-entity, so every request-shape decision resolves the entity here).
    fn request_entity<'a>(&self, m: &'a ModuleDesign, ep: &Endpoint) -> Option<&'a Entity> {
        // An inline DTO body (issue #122) resolves to NO entity — none of the
        // entity machinery (server-owned fk, defaults, path fk) applies to it.
        let ent = ep.request_body.as_ref()?.entity.as_ref()?;
        m.entities.iter().find(|e| &e.name == ent)
    }

    /// The defaulted-field rule (issue #53a): this endpoint's request-body entity
    /// carries at least one `default` field, so the wire contract drops it (the
    /// server applies the declared value). Independent of auth — a public create
    /// (`POST /subscribers`) still omits `confirmed`/`status`.
    pub(crate) fn endpoint_omits_defaulted_field(&self, m: &ModuleDesign, ep: &Endpoint) -> bool {
        self.request_entity(m, ep)
            .is_some_and(|e| e.fields.iter().any(|f| f.default.is_some()))
    }

    /// True when entity `entity` (anywhere in the tree) declares a `default` field
    /// (issue #85 D1). Its UPDATE request DTO (`{Entity}UpdateRequest`) KEEPS those
    /// fields — a `default` is create-only, so an update must be able to set them —
    /// while the CREATE DTO drops them. Resolves by name so a caller holding only
    /// the fk/body entity name agrees with the one holding the `Entity`.
    pub(crate) fn entity_has_default(&self, entity: &str) -> bool {
        self.find_entity(entity)
            .is_some_and(|e| e.fields.iter().any(|f| f.default.is_some()))
    }

    /// The nested-route parent-FK rule (issue #53b): the fk columns this entity's
    /// belongs_to derive that ALSO appear as a `{param}` in some endpoint whose
    /// body is this entity (`Checkin belongs_to Habit` + `POST /{habit_id}/checkins`
    /// → `habit_id`). Those come from the PATH, so the request DTO omits them and
    /// the handler injects the path value. Entity-scoped (the DTO struct is
    /// per-entity): an entity created only under its parent's path always omits the
    /// parent fk. Empty for a top-level create (`POST /leads` — no matching param),
    /// which keeps every default-free non-nested design byte-identical.
    pub(crate) fn entity_path_fk_columns(&self, entity_name: &str) -> Vec<String> {
        let Some(e) = self.find_entity(entity_name) else {
            return Vec::new();
        };
        e.belongs_to
            .iter()
            .map(|b| b.fk_column())
            .filter(|col| self.any_body_endpoint_resolved_path_has(entity_name, col))
            .collect()
    }

    /// True when some endpoint whose request body is `entity_name` carries `{col}`
    /// in its RESOLVED path — the accumulated module/subroute MOUNT prefix plus
    /// `ep.path`, not `ep.path` alone. Mount-aware twin of the old ep.path-only
    /// check (issue #82): a child mounted at `/clubs/{club_id}` whose create is
    /// `POST /` still carries `club_id` in the resolved path, so the fk is
    /// path-redundant and the request DTO drops it (closing the #125 create vector —
    /// a body `club_id` can no longer relocate the row into another tenant). The
    /// mount accumulation mirrors `endpoint_tenant_shape` (mount, trailing `/`
    /// trimmed, + `ep.path`) and testgen's `base`/`sub_base`, so "resolved path"
    /// means the same thing everywhere. Walks the whole design tree (modules +
    /// subroutes). Note: a fk already spelled in `ep.path` (`POST /{col}/…`) is
    /// found by this check too, so those designs stay byte-identical.
    fn any_body_endpoint_resolved_path_has(&self, entity_name: &str, col: &str) -> bool {
        fn walk(m: &ModuleDesign, entity_name: &str, token: &str, prefix: &str) -> bool {
            let mount = m.effective_mount();
            let mount = mount.strip_suffix('/').unwrap_or(&mount);
            let base = format!("{prefix}{mount}");
            m.endpoints.iter().any(|ep| {
                ep.request_body
                    .as_ref()
                    .is_some_and(|rb| rb.entity.as_deref() == Some(entity_name))
                    && format!("{base}{}", ep.path).contains(token)
            }) || m
                .subroutes
                .iter()
                .any(|s| walk(s, entity_name, token, &base))
        }
        let token = format!("{{{col}}}");
        self.modules
            .iter()
            .any(|m| walk(m, entity_name, &token, ""))
    }

    /// True when this endpoint's request-body entity has a path-redundant parent fk
    /// (issue #53b) — the entity-scoped rule, so it fires for every endpoint that
    /// creates the nested entity.
    pub(crate) fn endpoint_omits_path_fk(&self, m: &ModuleDesign, ep: &Endpoint) -> bool {
        self.request_entity(m, ep)
            .is_some_and(|e| !self.entity_path_fk_columns(&e.name).is_empty())
    }

    /// The generalized request-DTO trigger (issue #53): a db-mode endpoint whose
    /// body entity has ANY field the wire contract drops — the server-owned
    /// identity fk (#34, guarded+auth), a `default` field (#53a), or a
    /// path-redundant parent fk (#53b) — takes `Json<{Entity}Request>` instead of
    /// the full entity. `auth` is the GENERATION mode flag (the identity-fk leg is
    /// meaningful only with the session guard wired); the default/path legs are
    /// auth-independent. Every one-shape design (no defaults, no nested fk, no
    /// identity fk) returns false, so its output is byte-identical.
    pub(crate) fn endpoint_uses_request_dto(
        &self,
        m: &ModuleDesign,
        ep: &Endpoint,
        auth: bool,
    ) -> bool {
        (auth && self.endpoint_omits_identity_fk(m, ep))
            || self.endpoint_omits_defaulted_field(m, ep)
            || self.endpoint_omits_path_fk(m, ep)
    }

    /// True when entity `entity` produces a generated `{entity}Request` DTO — it is
    /// the request body of some endpoint that omits a server-owned field, in db mode
    /// (the DTO is a db-mode construct; issue #43 gates it there). The JC0541 lint
    /// (issue #44) uses this to detect a REAL collision with an entity literally
    /// named `{entity}Request`: only a base entity that actually mints the DTO
    /// clashes, so a plain `*Request` name that shadows nothing is left alone.
    pub(crate) fn entity_generates_request_dto(&self, entity: &str) -> bool {
        if !self.wants_db() {
            return false;
        }
        let auth = self.wants_auth();
        fn walk(design: &Design, m: &ModuleDesign, entity: &str, auth: bool) -> bool {
            m.endpoints.iter().any(|ep| {
                ep.request_body
                    .as_ref()
                    .is_some_and(|rb| rb.entity.as_deref() == Some(entity))
                    && design.endpoint_uses_request_dto(m, ep, auth)
            }) || m.subroutes.iter().any(|s| walk(design, s, entity, auth))
        }
        self.modules.iter().any(|m| walk(self, m, entity, auth))
    }

    /// snake_case a validated PascalCase entity name. Entity names are validated
    /// `^[A-Z][A-Za-z0-9]*$`, so each uppercase letter (past the first char)
    /// starts a new word: "ApiKey" -> "api_key".
    pub fn to_snake(name: &str) -> String {
        let mut snake = String::with_capacity(name.len() + 2);
        for (i, ch) in name.char_indices() {
            if i > 0 && ch.is_ascii_uppercase() {
                snake.push('_');
            }
            snake.push(ch.to_ascii_lowercase());
        }
        snake
    }

    /// The Rust key type a belongs_to target keys on: the target entity's declared
    /// `id` field type, `i64` for a synthetic or integer pk. Mirrors genroute's
    /// `key_rust_type` but resolves the entity by name across the whole design tree
    /// (a fk may point at an entity in any module or subroute). Falls back to `i64`
    /// when the target is unknown (validation guarantees it exists in practice).
    pub fn target_key_rust_type(&self, target: &str) -> &'static str {
        self.find_entity(target)
            .and_then(|e| e.fields.iter().find(|f| f.name == "id"))
            .map(|f| f.field_type.rust_type())
            .unwrap_or("i64")
    }

    /// The Rust key type a PATH PARAM references (issue #85): a param named after a
    /// belongs_to fk column (`site_id`) points at that entity's pk, so it must type
    /// from the referent — a string/uuid-pk `Site` → `String`, not a hardcoded
    /// `i64`. Matches `{snake}_id` back to the entity whose `fk_column` equals the
    /// param, then resolves its pk type. Returns `i64` when the param matches no
    /// entity's fk column (a synthetic/opaque param like `code`), so every design
    /// whose non-id path params reference integer-pk entities stays byte-identical.
    pub fn path_param_key_type(&self, param: &str) -> &'static str {
        fn find_name<'a>(m: &'a ModuleDesign, param: &str) -> Option<&'a str> {
            m.entities
                .iter()
                .map(|e| e.name.as_str())
                .find(|n| Design::fk_column(n) == param)
                .or_else(|| m.subroutes.iter().find_map(|s| find_name(s, param)))
        }
        match self.modules.iter().find_map(|m| find_name(m, param)) {
            Some(name) => self.target_key_rust_type(name),
            None => "i64",
        }
    }

    /// Resolve an entity by name across the whole design tree (any module or
    /// subroute). A belongs_to/tenancy target may live anywhere.
    pub fn find_entity(&self, name: &str) -> Option<&Entity> {
        fn find<'a>(m: &'a ModuleDesign, name: &str) -> Option<&'a Entity> {
            m.entities
                .iter()
                .find(|e| e.name == name)
                .or_else(|| m.subroutes.iter().find_map(|s| find(s, name)))
        }
        self.modules.iter().find_map(|m| find(m, name))
    }

    /// The SQL table name for an entity — the single source of truth every
    /// generator shares (DDL, queries, schema.json, the realtime publication):
    /// the entity's explicit `table` override when present, else
    /// `snake_case(name)` with proper English pluralization
    /// (`EnergySummary` → `energy_summaries`, `CaptureSession` → `capture_sessions`).
    /// Resolves the override by NAME across the tree so a call site holding only
    /// a fk/tenancy target name agrees with the one holding the `Entity`.
    pub fn table_name(&self, entity: &str) -> String {
        self.find_entity(entity)
            .and_then(|e| e.table.clone())
            .unwrap_or_else(|| Self::default_table_name(entity))
    }

    /// The DEFAULT table name for an entity name, ignoring any `table` override:
    /// `snake_case(entity)` pluralized. The migration importer calls this to
    /// decide whether a source table's name round-trips through the default or
    /// needs an explicit `table` override to stay lossless.
    pub fn default_table_name(entity: &str) -> String {
        pluralize(&Self::to_snake(entity))
    }
}

/// Deterministic English pluralization for a snake_case identifier (the default
/// table-name rule): consonant + `y` → `ies` (`energy` → `energies`); ends in
/// `s`/`x`/`z`/`ch`/`sh` → `es` (`box` → `boxes`, `dish` → `dishes`); vowel + `y`
/// → `+s` (`day` → `days`); else `+s`. Not exhaustive English (no irregulars),
/// but deterministic and correct for the multi-word entity names the old
/// `lowercase + "s"` mangled (`energysummarys`).
fn pluralize(word: &str) -> String {
    if word.ends_with("ch")
        || word.ends_with("sh")
        || word.ends_with('s')
        || word.ends_with('x')
        || word.ends_with('z')
    {
        return format!("{word}es");
    }
    if let Some(stem) = word.strip_suffix('y')
        && !stem.ends_with(['a', 'e', 'i', 'o', 'u'])
    {
        return format!("{stem}ies");
    }
    format!("{word}s")
}

/// Rust keywords (2018+): reserved words that need a raw identifier (`r#name`)
/// to appear as a field/type name in generated code. Shared by the validator
/// (`questions.rs`) and the code generators (`genroute.rs`) so the "is this a
/// keyword?" decision has ONE source of truth.
const RUST_KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use",
    "where", "while",
];

/// Keywords a raw identifier CANNOT escape — even `r#self` is invalid Rust — so
/// a field/entity named one of these still can't become a Rust identifier and
/// must be rejected by validation.
const UNESCAPABLE_KEYWORDS: &[&str] = &["crate", "self", "Self", "super"];

/// Whether `name` is a Rust keyword (would collide with generated struct/field
/// idents unless raw-escaped).
pub(crate) fn is_rust_keyword(name: &str) -> bool {
    RUST_KEYWORDS.contains(&name)
}

/// Whether `name` can appear as a Rust identifier — directly, or raw-escaped
/// (`r#name`). True for non-keywords and raw-escapable keywords (`type`, `match`,
/// `ref`, …); false only for `crate`/`self`/`super`/`Self`, which no `r#` rescues.
pub(crate) fn can_be_rust_ident(name: &str) -> bool {
    !UNESCAPABLE_KEYWORDS.contains(&name)
}

/// A field/type name rendered as a Rust identifier: raw-escaped (`type` →
/// `r#type`) when it is a keyword, unchanged otherwise. Every generated
/// RUST-identifier position for a field name routes through this so a frozen
/// wire contract can keep a `type`/`match`/`ref` field without a rename.
/// Precondition: `can_be_rust_ident(name)` (validation guarantees it).
pub(crate) fn rust_ident(name: &str) -> String {
    if is_rust_keyword(name) {
        format!("r#{name}")
    } else {
        name.to_string()
    }
}

/// PascalCase a validated snake_case name (`checkout` → `Checkout`,
/// `bulk_import` → `BulkImport`): each underscore-separated word capitalized. Used
/// to name the inline-DTO request struct `{Pascal(operation_id)}Request` (issue
/// #122) — the ONE op_id→Pascal converter shared by genroute/openapi/testgen/
/// questions so the emitted struct, the advertised schema, the probe body type,
/// and the validation message all agree on the name.
pub(crate) fn to_pascal(snake: &str) -> String {
    let mut out = String::with_capacity(snake.len());
    for word in snake.split('_') {
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
        }
    }
    out
}

/// Walk a module and its subroutes in document order, pairing each entity that
/// resolves to a tenant path (directly OR transitively — issue #102) with the
/// owning module/subroute name. Delegates ownership to `Design::tenant_path` so
/// the direct and transitive cases share one resolver (an ambiguous entity
/// resolves to `None` and is omitted — the design is rejected by `JC0545`).
/// The bare collection path a parameterized endpoint acts under: its path with the
/// trailing `/{param}` removed (`/tasks/{id}` → `/tasks`, `/{id}` → `/`). None when
/// the path carries no `{param}` (nothing to strip).
fn collection_path(ep: &Endpoint) -> Option<String> {
    let p = ep.path.as_str();
    let brace = p.rfind('{')?;
    let cut = p[..brace].rfind('/').unwrap_or(0);
    Some(if cut == 0 {
        "/".to_string()
    } else {
        p[..cut].to_string()
    })
}

/// The POST creator (with a body) mounted at a bare collection `path` in this
/// module — the route whose entity owns the rows addressable under `path/{id}`.
fn creator_at<'a>(m: &'a ModuleDesign, path: &str) -> Option<&'a Endpoint> {
    m.endpoints
        .iter()
        .find(|ep| ep.method == HttpMethod::POST && ep.path == path && ep.request_body.is_some())
}

/// The entity whose repo/model a route's handler binds. Resolution order: the
/// request body's entity, then the success entity, then — for a no-body endpoint
/// like `DELETE /{id}` that names neither (issue #56) — the entity of the
/// COLLECTION it acts under (its parent path's POST creator), so a multi-entity
/// module's `/tasks/{id}` stub binds `TaskRepo`, not the module's FIRST entity.
/// Falls back to the first entity only when path-based resolution finds nothing
/// (a bare `/import`, or a module with no matching creator) — byte-identical to
/// the pre-#56 behavior for every single-entity module (the collection creator IS
/// the sole entity there). Lives here (not genroute) so the ONE resolution serves
/// both emission and [`Design::endpoint_is_public_read_get`].
pub(crate) fn endpoint_repo_entity<'a>(m: &'a ModuleDesign, ep: &'a Endpoint) -> Option<&'a str> {
    endpoint_repo_entity_strict(m, ep).or_else(|| m.entities.first().map(|e| e.name.as_str()))
}

/// [`endpoint_repo_entity`] WITHOUT the first-entity fallback: the entity an
/// EXPLICIT design signal ties the endpoint to — request body, success entity,
/// or the collection creator its `{param}` path acts under (#56) — and `None`
/// for an entity-less endpoint (custom-JSON success, no body, no `{param}`
/// collection: the documented hand-written `Json<serde_json::Value>` shape).
/// SECURITY-SENSITIVE consumers must use THIS resolver: the lenient fallback is
/// a convenience for repo-binding in stubs, but classifying an endpoint's
/// guarding by it is fail-OPEN — an `auth_required` `GET /stats` that never
/// reads the module's first entity would inherit that entity's `public_read`
/// and silently ship anonymous ([`Design::endpoint_is_public_read_get`]), and
/// JC0549(c) would falsely refuse a `public: true` custom GET in a per-user
/// module as unimplementable.
pub(crate) fn endpoint_repo_entity_strict<'a>(
    m: &'a ModuleDesign,
    ep: &'a Endpoint,
) -> Option<&'a str> {
    if m.entities.is_empty() {
        return None;
    }
    ep.request_body
        .as_ref()
        .and_then(|rb| rb.entity.as_deref())
        .or(ep.success.entity.as_deref())
        .or_else(|| {
            collection_path(ep)
                .and_then(|coll| creator_at(m, &coll))
                .and_then(|c| c.request_body.as_ref())
                .and_then(|rb| rb.entity.as_deref())
        })
}

fn collect_tenant_owned<'a>(
    design: &Design,
    module: &'a ModuleDesign,
    out: &mut Vec<(&'a str, &'a str)>,
) {
    for entity in &module.entities {
        if design.tenant_path(&entity.name).is_some() {
            out.push((module.name.as_str(), entity.name.as_str()));
        }
    }
    for subroute in &module.subroutes {
        collect_tenant_owned(design, subroute, out);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) const MINIMAL: &str = r#"{
        "name": "demo-api",
        "contract_version": 0,
        "auth": { "model": "session", "roles": ["admin"] },
        "dependencies": ["db"],
        "modules": [{
            "name": "todos",
            "entities": [{ "name": "Todo", "fields": [
                { "name": "title", "type": "string" },
                { "name": "done", "type": "boolean", "required": false }
            ]}],
            "endpoints": [
                { "operation_id": "list_todos", "method": "GET", "path": "/",
                  "success": { "status": 200, "entity": "Todo", "list": true } },
                { "operation_id": "create_todo", "method": "POST", "path": "/",
                  "request_body": { "entity": "Todo" },
                  "success": { "status": 201, "entity": "Todo" } },
                { "operation_id": "delete_todo", "method": "DELETE", "path": "/{id}",
                  "required_roles": ["admin"],
                  "success": { "status": 204 },
                  "errors": [{ "status": 404, "code": "JC0404", "when": "unknown id" }] }
            ],
            "subroutes": [{
                "name": "comments",
                "endpoints": [{ "operation_id": "list_comments", "method": "GET", "path": "/",
                                "success": { "status": 200 } }]
            }]
        }]
    }"#;

    pub(crate) const V1_FULL: &str = r#"{
        "name": "reference-mini", "contract_version": 1,
        "auth": { "model": "jwt", "roles": ["owner", "member"] },
        "dependencies": ["db", "auth"],
        "tenancy": { "entity": "Workspace", "member_roles": ["owner", "member"] },
        "jobs": [{ "name": "expire_trials", "schedule": "0 * * * *" }],
        "modules": [
            { "name": "workspaces",
              "entities": [{ "name": "Workspace", "fields": [
                  { "name": "id", "type": "integer" },
                  { "name": "plan", "type": "string", "values": ["trial", "pro"] }
              ]}],
              "endpoints": [{ "operation_id": "list_workspaces", "method": "GET",
                  "path": "/", "success": { "status": 200, "entity": "Workspace", "list": true } }] },
            { "name": "leads",
              "entities": [{ "name": "Lead",
                  "belongs_to": [{ "entity": "Workspace", "on_delete": "cascade" }],
                  "fields": [
                      { "name": "id", "type": "integer" },
                      { "name": "phone", "type": "string", "unique": true, "index": true },
                      { "name": "custom", "type": "json", "required": false }
                  ]}],
              "endpoints": [{ "operation_id": "list_leads", "method": "GET",
                  "path": "/", "success": { "status": 200, "entity": "Lead", "list": true } }] }
        ]
    }"#;

    pub(crate) const V2_STORAGE: &str = r#"{
        "name": "files-app", "contract_version": 2,
        "auth": { "model": "session", "roles": ["owner", "member"] },
        "dependencies": ["db", "auth"],
        "tenancy": { "entity": "Org", "member_roles": ["owner", "member"] },
        "storage": { "buckets": [
            { "name": "avatars", "visibility": "public", "owner": "User",
              "max_size": "5MB", "allowed_mime": ["image/*"] },
            { "name": "invoices", "visibility": "private", "owner": "Org",
              "owner_prefix": true, "max_size": "20MB" }
        ]},
        "modules": [
            { "name": "orgs",
              "entities": [
                  { "name": "Org", "fields": [
                      { "name": "id", "type": "integer" },
                      { "name": "plan", "type": "string" } ] },
                  { "name": "User", "fields": [
                      { "name": "id", "type": "integer" },
                      { "name": "email", "type": "string" } ] }
              ],
              "endpoints": [{ "operation_id": "list_orgs", "method": "GET", "path": "/",
                  "success": { "status": 200, "entity": "Org", "list": true } }] }
        ]
    }"#;

    pub(crate) const V2_REALTIME: &str = r#"{
        "name": "rt-app", "contract_version": 2,
        "auth": { "model": "jwt", "roles": ["owner", "member"] },
        "dependencies": ["db", "auth"],
        "tenancy": { "entity": "Workspace", "member_roles": ["owner", "member"] },
        "realtime": {
            "changes": ["Lead"],
            "broadcast": [{ "name": "deal_room", "scope": "tenant" }],
            "presence": [{ "name": "editors", "scope": "tenant" }]
        },
        "modules": [
            { "name": "workspaces",
              "entities": [{ "name": "Workspace", "fields": [
                  { "name": "id", "type": "integer" }, { "name": "name", "type": "string" } ]}],
              "endpoints": [{ "operation_id": "list_workspaces", "method": "GET", "path": "/",
                  "success": { "status": 200, "entity": "Workspace", "list": true } }] },
            { "name": "leads",
              "entities": [{ "name": "Lead",
                  "belongs_to": [{ "entity": "Workspace", "on_delete": "cascade" }],
                  "fields": [{ "name": "id", "type": "integer" },
                             { "name": "phone", "type": "string" }] }],
              "endpoints": [{ "operation_id": "list_leads", "method": "GET", "path": "/",
                  "success": { "status": 200, "entity": "Lead", "list": true } }] }
        ]
    }"#;

    #[test]
    fn realtime_block_round_trips_and_gates_the_facade_feature() {
        let d: Design = serde_json::from_str(V2_REALTIME).unwrap();
        assert!(d.wants_realtime());
        let rt = d.realtime.as_ref().unwrap();
        assert_eq!(rt.changes, vec!["Lead"]);
        assert_eq!(rt.broadcast[0].name, "deal_room");
        assert_eq!(rt.broadcast[0].scope, RealtimeScope::Tenant);
        let feats = d.facade_features();
        assert!(feats.contains(&"realtime"), "{feats:?}");
        assert_eq!(
            feats.last(),
            Some(&"realtime"),
            "realtime is appended last (after storage): {feats:?}"
        );
        // Round trip.
        let back = serde_json::to_string(&d).unwrap();
        let re: Design = serde_json::from_str(&back).unwrap();
        assert!(re.wants_realtime());
        // Absent block ⇒ no feature (v0/v1 designs untouched).
        let plain: Design = serde_json::from_str(MINIMAL).unwrap();
        assert!(!plain.wants_realtime());
        assert!(!plain.facade_features().contains(&"realtime"));
    }

    /// #112: the write-only classifier hides a field from responses when it
    /// declares `write_only` OR is the one unambiguous secret column name
    /// `password_hash` (secure-by-default, fail-CLOSED). A normal field is never
    /// hidden — that is what guarantees byte-identity for designs with neither.
    #[test]
    fn field_is_write_only_flags_explicit_flag_and_password_hash_by_name() {
        let f = |json: &str| -> Field { serde_json::from_str(json).unwrap() };
        assert!(
            Design::field_is_write_only(&f(
                r#"{ "name": "api_token", "type": "string", "write_only": true }"#
            )),
            "an explicit write_only flag hides the field"
        );
        assert!(
            Design::field_is_write_only(&f(r#"{ "name": "password_hash", "type": "string" }"#)),
            "password_hash is auto-classified WITHOUT the flag (secure-by-default)"
        );
        assert!(
            Design::field_is_write_only(&f(r#"{ "name": "Password_Hash", "type": "string" }"#)),
            "the password_hash name match is case-insensitive"
        );
        assert!(
            !Design::field_is_write_only(&f(r#"{ "name": "share_token", "type": "string" }"#)),
            "the broad *_token/*_hash heuristic is deliberately EXCLUDED (may be public)"
        );
        assert!(
            !Design::field_is_write_only(&f(r#"{ "name": "email", "type": "string" }"#)),
            "a normal field is never hidden"
        );
    }

    /// #110: the now-default classifier is TRUE only for a `datetime` field whose
    /// `default` is the exact lowercase `"now"`. A static datetime default, a
    /// bad-casing near-miss, `"now"` on another type, and no-default all read FALSE
    /// — that precision is what makes the immutable-timestamp treatment (drop from
    /// both DTOs, `now_rfc3339()` steer) fire ONLY on the intended sentinel and
    /// leaves every other datetime field byte-identical.
    #[test]
    fn field_is_now_default_flags_only_datetime_with_exact_lowercase_now() {
        let f = |json: &str| -> Field { serde_json::from_str(json).unwrap() };
        assert!(
            Design::field_is_now_default(&f(
                r#"{ "name": "created_at", "type": "datetime", "default": "now" }"#
            )),
            "datetime + exact \"now\" is the sentinel"
        );
        assert!(
            !Design::field_is_now_default(&f(
                r#"{ "name": "created_at", "type": "datetime", "default": "2026-01-01T00:00:00Z" }"#
            )),
            "a STATIC datetime default is not the now sentinel"
        );
        assert!(
            !Design::field_is_now_default(&f(
                r#"{ "name": "created_at", "type": "datetime", "default": "NOW" }"#
            )),
            "a bad-casing near-miss is not the sentinel (validation refuses it)"
        );
        assert!(
            !Design::field_is_now_default(&f(
                r#"{ "name": "label", "type": "string", "default": "now" }"#
            )),
            "\"now\" on a non-datetime field is not the sentinel (validation refuses it)"
        );
        assert!(
            !Design::field_is_now_default(&f(r#"{ "name": "created_at", "type": "datetime" }"#)),
            "a datetime field WITHOUT a default is never the sentinel"
        );
    }

    #[test]
    fn published_schema_accepts_the_realtime_block() {
        let s = include_str!("../../../../docs/contracts/design-schema.json");
        assert!(
            s.contains("\"realtime\"") && s.contains("\"broadcast\"") && s.contains("\"presence\"")
        );
    }

    #[test]
    fn cors_block_round_trips_and_maps_onto_core_config() {
        // WHY: a cross-origin SPA (console on one origin, API on another) must be
        // declarable in design.json — not hand-patched into the tool-owned main.rs.
        // The block maps 1:1 onto CorsConfig/CorsOrigins (issue #21).
        let d: Design = serde_json::from_str(
            r#"{ "name": "api", "contract_version": 0, "dependencies": [],
                "cors": {
                    "origins": ["https://app.example", "https://admin.example"],
                    "methods": ["GET", "POST", "PUT", "PATCH", "DELETE"],
                    "headers": ["content-type", "authorization"],
                    "allow_credentials": true
                },
                "modules": [{ "name": "m", "endpoints": [
                    { "operation_id": "list_m", "method": "GET", "path": "/",
                      "success": { "status": 200 } }] }] }"#,
        )
        .unwrap();
        let cors = d.cors.as_ref().expect("cors block parses");
        assert_eq!(
            cors.origins,
            ["https://app.example", "https://admin.example"]
        );
        assert_eq!(
            cors.methods,
            [
                HttpMethod::GET,
                HttpMethod::POST,
                HttpMethod::PUT,
                HttpMethod::PATCH,
                HttpMethod::DELETE
            ]
        );
        assert_eq!(cors.headers, ["content-type", "authorization"]);
        assert!(cors.allow_credentials);
        assert!(!cors.is_any(), "an explicit allowlist is not `any`");
        // Round trip preserves the block.
        let back = serde_json::to_string(&d).unwrap();
        let re: Design = serde_json::from_str(&back).unwrap();
        assert_eq!(re.cors.as_ref().unwrap().origins, cors.origins);

        // The `*` marker maps to CorsOrigins::any(); false-y defaults are skipped
        // on serialize (no `methods`/`headers`/`allow_credentials` keys emitted).
        let any: Design = serde_json::from_str(
            r#"{ "name": "api", "contract_version": 0, "dependencies": [],
                "cors": { "origins": ["*"] },
                "modules": [{ "name": "m", "endpoints": [
                    { "operation_id": "list_m", "method": "GET", "path": "/",
                      "success": { "status": 200 } }] }] }"#,
        )
        .unwrap();
        assert!(any.cors.as_ref().unwrap().is_any());
        let val = serde_json::to_value(any.cors.as_ref().unwrap()).unwrap();
        assert!(val.get("methods").is_none() && val.get("headers").is_none());
        assert!(
            val.get("allow_credentials").is_none(),
            "false allow_credentials is not serialized: {val}"
        );

        // Absent block ⇒ no cors (v0/v1/v2 designs untouched, no facade feature).
        let plain: Design = serde_json::from_str(MINIMAL).unwrap();
        assert!(plain.cors.is_none());
        assert!(
            !plain.facade_features().iter().any(|f| f == &"cors"),
            "cors is unconditional in core — it adds no facade feature"
        );
    }

    #[test]
    fn published_schema_accepts_the_cors_block() {
        let s = include_str!("../../../../docs/contracts/design-schema.json");
        assert!(
            s.contains("\"cors\"")
                && s.contains("\"origins\"")
                && s.contains("\"allow_credentials\"")
        );
    }

    #[test]
    fn field_constraint_keys_round_trip_and_stay_absent_when_unset() {
        // #80: the four constraint keys (min/max integer range, min_len/max_len
        // string length) parse and survive a round trip; being serde-default None
        // + skip_serializing_if, an unconstrained design stays byte-identical
        // through canonical_design_json — the byte-identity gate for every
        // existing design.
        let constrained: Design = serde_json::from_str(
            r#"{ "name": "shop", "contract_version": 0, "dependencies": ["db"],
                "modules": [{ "name": "items",
                    "entities": [{ "name": "Item", "fields": [
                        { "name": "quantity", "type": "integer", "min": 1, "max": 600 },
                        { "name": "bio", "type": "string", "min_len": 1, "max_len": 280 }
                    ]}],
                    "endpoints": [{ "operation_id": "list_items", "method": "GET", "path": "/",
                        "success": { "status": 200, "entity": "Item", "list": true } }] }] }"#,
        )
        .unwrap();
        let f = &constrained.modules[0].entities[0].fields[0];
        assert_eq!((f.min, f.max), (Some(1), Some(600)));
        assert_eq!((f.min_len, f.max_len), (None, None));
        let g = &constrained.modules[0].entities[0].fields[1];
        assert_eq!((g.min_len, g.max_len), (Some(1), Some(280)));
        let back: Design =
            serde_json::from_str(&serde_json::to_string(&constrained).unwrap()).unwrap();
        assert_eq!(back.modules[0].entities[0].fields[0].max, Some(600));
        assert_eq!(back.modules[0].entities[0].fields[1].max_len, Some(280));

        // An unconstrained design serializes NO constraint keys, and the
        // canonical writer round-trips it byte-identically.
        let plain: Design = serde_json::from_str(MINIMAL).unwrap();
        let canon = crate::platform::scaffold::canonical_design_json(&plain);
        for key in ["\"min\"", "\"max\"", "\"min_len\"", "\"max_len\""] {
            assert!(
                !canon.contains(key),
                "unconstrained design must not serialize {key}: {canon}"
            );
        }
        let re: Design = serde_json::from_str(&canon).unwrap();
        assert_eq!(
            canon,
            crate::platform::scaffold::canonical_design_json(&re),
            "canonical round trip is byte-identical"
        );
    }

    #[test]
    fn published_schema_accepts_the_field_constraint_keys() {
        let s = include_str!("../../../../docs/contracts/design-schema.json");
        for key in ["\"min\"", "\"max\"", "\"min_len\"", "\"max_len\""] {
            assert!(
                s.contains(key),
                "design-schema.json must define {key} (#80)"
            );
        }
    }

    #[test]
    fn v2_storage_block_round_trips_and_gates_wants_storage() {
        let d: Design = serde_json::from_str(V2_STORAGE).unwrap();
        assert_eq!(d.contract_version, 2);
        let s = d.storage.as_ref().unwrap();
        assert_eq!(s.buckets.len(), 2);
        assert_eq!(s.buckets[0].name, "avatars");
        assert_eq!(s.buckets[0].visibility, Visibility::Public);
        assert_eq!(s.buckets[0].owner.as_deref(), Some("User"));
        assert!(!s.buckets[0].owner_prefix, "owner_prefix defaults false");
        assert!(s.buckets[1].owner_prefix);
        assert!(d.wants_storage());
        let back = serde_json::to_string(&d).unwrap();
        let re: Design = serde_json::from_str(&back).unwrap();
        assert!(re.wants_storage(), "storage survives a round trip");
        // v0/v1 designs stay valid and storage-free.
        let v0: Design = serde_json::from_str(MINIMAL).unwrap();
        assert!(v0.storage.is_none() && !v0.wants_storage());
    }

    #[test]
    fn storage_base_path_defaults_to_storage_and_round_trips_an_override() {
        // Buckets mount under the base path (default `/storage`), keeping them
        // clear of module mounts (issue #8). Absent ⇒ `/storage`; an override is
        // preserved across a round trip.
        let d: Design = serde_json::from_str(V2_STORAGE).unwrap();
        assert_eq!(
            d.storage.as_ref().unwrap().effective_base_path(),
            "/storage",
            "absent base_path defaults to /storage"
        );
        // false-y default is skipped on serialize (no `base_path` key emitted).
        let back = serde_json::to_value(d.storage.as_ref().unwrap()).unwrap();
        assert!(
            back.get("base_path").is_none(),
            "absent base_path is not serialized: {back}"
        );
        // An override survives a round trip and drives effective_base_path.
        let mut d2 = d;
        d2.storage.as_mut().unwrap().base_path = Some("/files".into());
        assert_eq!(d2.storage.as_ref().unwrap().effective_base_path(), "/files");
        let s = serde_json::to_string(&d2).unwrap();
        let re: Design = serde_json::from_str(&s).unwrap();
        assert_eq!(
            re.storage.as_ref().unwrap().base_path.as_deref(),
            Some("/files")
        );
    }

    #[test]
    fn wants_storage_appends_the_storage_s3_facade_feature_last() {
        // Generated apps get storage-s3 (S3 compiled in) so JERRYCAN_STORAGE
        // can switch backends by env WITHOUT recompiling (zero-touch config).
        let d: Design = serde_json::from_str(V2_STORAGE).unwrap();
        let feats = d.facade_features();
        assert_eq!(
            feats.last(),
            Some(&"storage-s3"),
            "storage-s3 appended last: {feats:?}"
        );
        assert!(feats.contains(&"db") && feats.contains(&"auth"));
        // No storage block → no storage feature (order of the rest unchanged).
        let no: Design = serde_json::from_str(V1_FULL).unwrap();
        assert!(!no.facade_features().contains(&"storage-s3"));
    }

    #[test]
    fn parse_size_handles_the_documented_suffixes() {
        assert_eq!(Design::parse_size("5MB"), Some(5 * 1024 * 1024));
        assert_eq!(Design::parse_size("20MB"), Some(20 * 1024 * 1024));
        assert_eq!(Design::parse_size("512KB"), Some(512 * 1024));
        assert_eq!(Design::parse_size("1GB"), Some(1024 * 1024 * 1024));
        assert_eq!(Design::parse_size("123B"), Some(123));
        assert_eq!(Design::parse_size("123"), Some(123), "bare number = bytes");
        assert_eq!(
            Design::parse_size("5mb"),
            None,
            "suffixes are uppercase (schema-validated)"
        );
        assert_eq!(Design::parse_size("lots"), None);
    }

    #[test]
    fn parse_size_refuses_overflow_instead_of_panicking_or_wrapping() {
        // WHY: parse_size runs on agent-authored design.json during validation.
        // An unchecked `n * mult` panics in debug and silently WRAPS in release
        // (a huge max_size becoming a small one) — overflow must read as
        // unparseable, which validation turns into a question.
        assert_eq!(Design::parse_size("99999999999999GB"), None, "overflow");
        assert_eq!(Design::parse_size("18446744073709551615B"), Some(u64::MAX));
        assert_eq!(Design::parse_size("18446744073709551616B"), None);
    }

    #[test]
    fn v1_design_round_trips_with_new_constructs() {
        let d: Design = serde_json::from_str(V1_FULL).unwrap();
        assert_eq!(d.contract_version, 1);
        assert_eq!(d.tenancy.as_ref().unwrap().entity, "Workspace");
        assert_eq!(d.jobs[0].name, "expire_trials");
        let lead = &d.modules[1].entities[0];
        assert_eq!(lead.belongs_to[0].entity, "Workspace");
        assert_eq!(lead.belongs_to[0].on_delete, OnDelete::Cascade);
        assert!(lead.fields[1].unique && lead.fields[1].index);
        assert_eq!(
            d.modules[0].entities[0].fields[1]
                .values
                .as_ref()
                .unwrap()
                .len(),
            2
        );
        let back = serde_json::to_string(&d).unwrap();
        let _re: Design = serde_json::from_str(&back).unwrap();
    }

    #[test]
    fn wants_jobs_gates_on_declared_jobs_and_adds_the_facade_feature() {
        // A design that declares a job switches on the jobs crate + the `jobs`
        // facade feature; the reference eval slice (V1_FULL) carries one.
        let with_jobs: Design = serde_json::from_str(V1_FULL).unwrap();
        assert!(with_jobs.wants_jobs(), "a declared job must set wants_jobs");
        assert!(
            with_jobs.facade_features().contains(&"jobs"),
            "wants_jobs must surface the `jobs` facade feature so the app enables it: {:?}",
            with_jobs.facade_features()
        );
        // No declared jobs → no jobs crate, no `jobs` feature.
        let no_jobs: Design = serde_json::from_str(MINIMAL).unwrap();
        assert!(!no_jobs.wants_jobs());
        assert!(!no_jobs.facade_features().contains(&"jobs"));
    }

    #[test]
    fn wants_oauth_gates_on_the_dependency_and_appends_the_facade_feature() {
        // A design declaring the `oauth` dependency enables the `oauth` facade
        // feature so a generated handler can use the OAuth2 client without a
        // manual Cargo patch. `oauth` is appended LAST, after `jobs`.
        let s = r#"{ "name": "x", "contract_version": 1,
            "dependencies": ["db", "auth", "oauth"],
            "modules": [{ "name": "m", "endpoints": [
                { "operation_id": "go", "method": "GET", "path": "/go",
                  "success": { "status": 302 } }] }] }"#;
        let d: Design = serde_json::from_str(s).unwrap();
        assert!(
            d.wants_oauth(),
            "the `oauth` dependency must set wants_oauth"
        );
        let feats = d.facade_features();
        assert!(
            feats.contains(&"oauth"),
            "wants_oauth must surface the `oauth` facade feature: {feats:?}"
        );
        assert_eq!(
            feats.last(),
            Some(&"oauth"),
            "oauth is appended last: {feats:?}"
        );
        // No `oauth` dependency → no `oauth` feature.
        let no_oauth: Design = serde_json::from_str(MINIMAL).unwrap();
        assert!(!no_oauth.wants_oauth());
        assert!(!no_oauth.facade_features().contains(&"oauth"));
    }

    #[test]
    fn v0_designs_still_parse_unchanged() {
        let d: Design = serde_json::from_str(MINIMAL).unwrap();
        assert_eq!(d.contract_version, 0);
        assert!(d.tenancy.is_none() && d.jobs.is_empty());
        assert!(d.modules[0].entities[0].belongs_to.is_empty());
    }

    #[test]
    fn tenant_owned_walks_modules_and_subroutes() {
        let mut d: Design = serde_json::from_str(V1_FULL).unwrap();
        // V1_FULL has no subroutes, so graft one carrying a tenant-owned
        // entity onto modules[1] to exercise the recursion in
        // collect_tenant_owned (deleting that recursion must fail this test).
        let sub: ModuleDesign = serde_json::from_str(
            r#"{
                "name": "notes",
                "entities": [{ "name": "Note",
                    "belongs_to": [{ "entity": "Workspace" }],
                    "fields": [{ "name": "body", "type": "string" }] }],
                "endpoints": [{ "operation_id": "list_notes", "method": "GET", "path": "/",
                    "success": { "status": 200 } }]
            }"#,
        )
        .unwrap();
        d.modules[1].subroutes.push(sub);
        assert_eq!(d.tenant_owned(), vec![("leads", "Lead"), ("notes", "Note")]);
    }

    /// Org (tenant) ; Account belongs_to Org ; Contact belongs_to Account —
    /// a two-hop tenant-ownership chain (the transitive case #102 re-opened).
    fn org_account_contact() -> Design {
        serde_json::from_str(
            r#"{ "name": "org-api", "contract_version": 1,
                "auth": { "model": "session", "roles": ["owner", "member"] },
                "dependencies": ["db", "auth"],
                "tenancy": { "entity": "Org", "member_roles": ["owner", "member"] },
                "modules": [
                    { "name": "orgs",
                      "entities": [{ "name": "Org", "fields": [
                          { "name": "id", "type": "integer" },
                          { "name": "name", "type": "string" } ]}],
                      "endpoints": [{ "operation_id": "list_orgs", "method": "GET", "path": "/",
                          "success": { "status": 200, "entity": "Org", "list": true } }] },
                    { "name": "accounts",
                      "entities": [{ "name": "Account",
                          "belongs_to": [{ "entity": "Org" }],
                          "fields": [{ "name": "id", "type": "integer" },
                                     { "name": "name", "type": "string" }] }],
                      "endpoints": [{ "operation_id": "list_accounts", "method": "GET", "path": "/",
                          "success": { "status": 200, "entity": "Account", "list": true } }] },
                    { "name": "contacts",
                      "entities": [{ "name": "Contact",
                          "belongs_to": [{ "entity": "Account" }],
                          "fields": [{ "name": "id", "type": "integer" },
                                     { "name": "email", "type": "string" }] }],
                      "endpoints": [{ "operation_id": "list_contacts", "method": "GET", "path": "/",
                          "success": { "status": 200, "entity": "Contact", "list": true } }] }
                ]
            }"#,
        )
        .unwrap()
    }

    /// Contact belongs_to [Account, Region]; Account and Region each belongs_to
    /// Org (the tenant) — two distinct chains reach the tenant (a diamond).
    fn diamond_design() -> Design {
        serde_json::from_str(
            r#"{ "name": "diamond-api", "contract_version": 1,
                "auth": { "model": "session", "roles": ["owner", "member"] },
                "dependencies": ["db", "auth"],
                "tenancy": { "entity": "Org", "member_roles": ["owner", "member"] },
                "modules": [
                    { "name": "orgs",
                      "entities": [{ "name": "Org", "fields": [
                          { "name": "id", "type": "integer" },
                          { "name": "name", "type": "string" } ]}],
                      "endpoints": [{ "operation_id": "list_orgs", "method": "GET", "path": "/",
                          "success": { "status": 200, "entity": "Org", "list": true } }] },
                    { "name": "accounts",
                      "entities": [{ "name": "Account",
                          "belongs_to": [{ "entity": "Org" }],
                          "fields": [{ "name": "id", "type": "integer" },
                                     { "name": "name", "type": "string" }] }],
                      "endpoints": [{ "operation_id": "list_accounts", "method": "GET", "path": "/",
                          "success": { "status": 200, "entity": "Account", "list": true } }] },
                    { "name": "regions",
                      "entities": [{ "name": "Region",
                          "belongs_to": [{ "entity": "Org" }],
                          "fields": [{ "name": "id", "type": "integer" },
                                     { "name": "name", "type": "string" }] }],
                      "endpoints": [{ "operation_id": "list_regions", "method": "GET", "path": "/",
                          "success": { "status": 200, "entity": "Region", "list": true } }] },
                    { "name": "contacts",
                      "entities": [{ "name": "Contact",
                          "belongs_to": [{ "entity": "Account" }, { "entity": "Region" }],
                          "fields": [{ "name": "id", "type": "integer" },
                                     { "name": "email", "type": "string" }] }],
                      "endpoints": [{ "operation_id": "list_contacts", "method": "GET", "path": "/",
                          "success": { "status": 200, "entity": "Contact", "list": true } }] }
                ]
            }"#,
        )
        .unwrap()
    }

    /// A→B, B→A (a belongs_to cycle) with the tenant (Org) off to the side —
    /// the resolver must terminate on the cycle, not recurse forever.
    fn cyclic_belongs_to_design() -> Design {
        serde_json::from_str(
            r#"{ "name": "cycle-api", "contract_version": 1,
                "auth": { "model": "session", "roles": ["owner", "member"] },
                "dependencies": ["db", "auth"],
                "tenancy": { "entity": "Org", "member_roles": ["owner", "member"] },
                "modules": [
                    { "name": "orgs",
                      "entities": [{ "name": "Org", "fields": [
                          { "name": "id", "type": "integer" } ]}],
                      "endpoints": [{ "operation_id": "list_orgs", "method": "GET", "path": "/",
                          "success": { "status": 200, "entity": "Org", "list": true } }] },
                    { "name": "as",
                      "entities": [{ "name": "A",
                          "belongs_to": [{ "entity": "B" }],
                          "fields": [{ "name": "id", "type": "integer" }] }],
                      "endpoints": [{ "operation_id": "list_as", "method": "GET", "path": "/",
                          "success": { "status": 200, "entity": "A", "list": true } }] },
                    { "name": "bs",
                      "entities": [{ "name": "B",
                          "belongs_to": [{ "entity": "A" }],
                          "fields": [{ "name": "id", "type": "integer" }] }],
                      "endpoints": [{ "operation_id": "list_bs", "method": "GET", "path": "/",
                          "success": { "status": 200, "entity": "B", "list": true } }] }
                ]
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn tenant_path_direct_child_has_no_joins() {
        let d = org_account_contact();
        let p = d.tenant_path("Account").expect("Account is tenant-owned");
        assert!(p.joins.is_empty(), "direct child = zero joins");
        assert_eq!(p.tenant_fk, "org_id");
        assert_eq!(p.anchor_table, d.table_name("Account"));
        assert_eq!(p.entity_table, d.table_name("Account"));
    }

    #[test]
    fn tenant_path_grandchild_joins_through_parent() {
        let d = org_account_contact();
        let p = d
            .tenant_path("Contact")
            .expect("Contact is transitively tenant-owned");
        assert_eq!(p.joins.len(), 1);
        assert_eq!(p.joins[0].child_table, d.table_name("Contact"));
        assert_eq!(p.joins[0].child_fk, "account_id");
        assert_eq!(p.joins[0].parent_table, d.table_name("Account"));
        assert_eq!(p.anchor_table, d.table_name("Account"));
        assert_eq!(p.entity_table, d.table_name("Contact"));
        assert_eq!(p.tenant_fk, "org_id");
    }

    #[test]
    fn tenant_path_none_for_unowned_entity() {
        let d = org_account_contact();
        assert!(
            d.tenant_path("Org").is_none(),
            "the tenant itself is not tenant-owned"
        );
    }

    #[test]
    fn tenant_path_ambiguous_diamond_raises_jc0545() {
        // A diamond graph reaches the tenant through TWO chains. `tenant_path`
        // resolves it to None (unscoped) — so the validator MUST reject the
        // design with JC0545, or the ambiguity silently re-opens the leak.
        let d = diamond_design();
        let diags = crate::platform::questions::validate(&d);
        assert!(
            diags.iter().any(|x| x.question.contains("JC0545")),
            "diamond → JC0545"
        );
        assert!(
            d.tenant_path("Contact").is_none(),
            "ambiguous resolves to None"
        );
        assert_eq!(
            d.tenant_path_branch_count("Contact"),
            2,
            "two distinct chains"
        );
    }

    #[test]
    fn tenant_path_cycle_does_not_hang() {
        let d = cyclic_belongs_to_design();
        let _ = d.tenant_path("A"); // must return, not loop
    }

    #[test]
    fn grandchild_flat_route_is_membership_set_not_none() {
        // Recognition is now transitive (#102): a GRANDCHILD flat route — `/contacts`,
        // mounted flat, carrying NO tenant fk (Contact belongs_to Account belongs_to
        // the tenant Org) — is tenant-owned and classified `MembershipSet`, the flat
        // membership-scoped shape. Pre-#102 the direct-only `owns_tenant_entity` gate
        // saw Contact belongs_to Account (not the tenant) and returned `None`: an
        // UNSCOPED grandchild — the transitive leak this fix closes.
        let d = org_account_contact();
        let contacts = &d.modules[2];
        assert_eq!(
            d.endpoint_tenant_shape(contacts, &contacts.endpoints[0]),
            TenantShape::MembershipSet,
        );
    }

    #[test]
    fn direct_child_shape_unchanged() {
        // Byte-identity guard: switching `owns_tenant_entity` onto the transitive
        // `tenant_path` must NOT change a DIRECT child's classification. Account is a
        // direct child of the tenant Org; its own flat route stays `MembershipSet`
        // (passes pre- AND post-fix). The direct-child PATH-SCOPED case (a nested
        // `/clubs/{club_id}/…`) is locked by `tenant_shape_classifies_by_route`.
        let d = org_account_contact();
        let accounts = &d.modules[1];
        assert_eq!(
            d.endpoint_tenant_shape(accounts, &accounts.endpoints[0]),
            TenantShape::MembershipSet,
        );
    }

    #[test]
    fn fk_column_is_snake_target_id() {
        assert_eq!(Design::fk_column("Workspace"), "workspace_id");
        assert_eq!(Design::fk_column("ApiKey"), "api_key_id");
        // fk_column derives from the shared to_snake (DRY); both must agree.
        assert_eq!(Design::to_snake("ApiKey"), "api_key");
        assert_eq!(Design::to_snake("Lead"), "lead");
    }

    #[test]
    fn belongs_to_fk_column_falls_through_or_aliases() {
        // WHY (issue #119): the fk-column derivation must be byte-identical to
        // `Design::fk_column(&entity)` when there is no `as` (every existing design
        // stays byte-for-byte), and become `{as}_id` when aliased — the SINGLE
        // mechanic that lets two refs to one entity coexist. If this diverged from
        // `Design::fk_column` on the unaliased path, determinism.rs would break.
        let plain = BelongsTo {
            entity: "Account".to_string(),
            on_delete: OnDelete::Restrict,
            r#as: None,
        };
        assert_eq!(plain.fk_column(), Design::fk_column("Account"));
        assert_eq!(plain.fk_column(), "account_id");
        let aliased = BelongsTo {
            entity: "Account".to_string(),
            on_delete: OnDelete::Restrict,
            r#as: Some("from_account".to_string()),
        };
        assert_eq!(aliased.fk_column(), "from_account_id");
        // A self-reference aliases just the same — the derivation never consults the
        // target, only the alias.
        let self_ref = BelongsTo {
            entity: "Comment".to_string(),
            on_delete: OnDelete::Cascade,
            r#as: Some("parent".to_string()),
        };
        assert_eq!(self_ref.fk_column(), "parent_id");
    }

    #[test]
    fn belongs_to_as_serde_roundtrips_and_omits_when_absent() {
        // `as` is a Rust keyword → `r#as` with `#[serde(rename = "as")]`; it
        // deserializes from the wire key `as`, and an absent alias is skipped on
        // serialize so an unaliased belongs_to stays byte-identical on the wire.
        let aliased: BelongsTo =
            serde_json::from_str(r#"{ "entity": "Account", "as": "from_account" }"#).unwrap();
        assert_eq!(aliased.r#as.as_deref(), Some("from_account"));
        assert_eq!(aliased.fk_column(), "from_account_id");
        let plain: BelongsTo = serde_json::from_str(r#"{ "entity": "Account" }"#).unwrap();
        assert_eq!(plain.r#as, None);
        let back = serde_json::to_string(&plain).unwrap();
        assert!(
            !back.contains("\"as\""),
            "absent alias must not serialize: {back}"
        );
    }

    #[test]
    fn tenant_shape_classifies_by_route() {
        // WHY: ownership scoping (issues #78/#79) needs, per endpoint, HOW the
        // tenant is bound so a guard can verify it. PathScoped: the URL carries
        // the tenant fk `{club_id}` — verify the caller belongs to THAT tenant.
        // Collection: create/list at the tenant module root — scope to the caller's
        // memberships. MembershipSet: a flat tenant-owned route with NO tenant
        // param in the URL (the #79 per-user leak surface). None: non-tenant routes.
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
                          { "operation_id": "create_club", "method": "POST", "path": "/",
                            "request_body": { "entity": "Club" },
                            "success": { "status": 201, "entity": "Club" } },
                          { "operation_id": "list_clubs", "method": "GET", "path": "/",
                            "success": { "status": 200, "entity": "Club", "list": true } },
                          { "operation_id": "get_club", "method": "GET", "path": "/{club_id}",
                            "success": { "status": 200, "entity": "Club" } },
                          { "operation_id": "delete_club", "method": "DELETE", "path": "/{club_id}",
                            "success": { "status": 204 } },
                          { "operation_id": "get_club_conventional", "method": "GET", "path": "/{id}",
                            "success": { "status": 200, "entity": "Club" } } ] },
                    { "name": "books", "mount": "/clubs/{club_id}",
                      "entities": [{ "name": "Book",
                          "belongs_to": [{ "entity": "Club" }],
                          "fields": [{ "name": "id", "type": "integer" },
                                     { "name": "title", "type": "string" }] }],
                      "endpoints": [
                          { "operation_id": "create_book", "method": "POST", "path": "/",
                            "request_body": { "entity": "Book" },
                            "success": { "status": 201, "entity": "Book" } },
                          { "operation_id": "list_books", "method": "GET", "path": "/",
                            "success": { "status": 200, "entity": "Book", "list": true } },
                          { "operation_id": "get_book", "method": "GET", "path": "/{id}",
                            "success": { "status": 200, "entity": "Book" } } ] },
                    { "name": "customers",
                      "entities": [{ "name": "Customer",
                          "belongs_to": [{ "entity": "Club" }],
                          "fields": [{ "name": "id", "type": "integer" },
                                     { "name": "email", "type": "string" }] }],
                      "endpoints": [
                          { "operation_id": "list_customers", "method": "GET", "path": "/",
                            "success": { "status": 200, "entity": "Customer", "list": true } },
                          { "operation_id": "get_customer", "method": "GET", "path": "/{id}",
                            "success": { "status": 200, "entity": "Customer" } } ] }
                ] }"#,
        )
        .unwrap();
        let clubs = &d.modules[0];
        let books = &d.modules[1];
        let customers = &d.modules[2];

        // Collection: the tenant entity's own POST "/" and GET "/".
        assert!(matches!(
            d.endpoint_tenant_shape(clubs, &clubs.endpoints[0]),
            TenantShape::Collection
        ));
        assert!(matches!(
            d.endpoint_tenant_shape(clubs, &clubs.endpoints[1]),
            TenantShape::Collection
        ));
        // PathScoped: the tenant's own detail routes + a nested tenant-owned
        // detail route, all carrying the tenant fk `club_id` in the resolved path.
        assert!(matches!(
            d.endpoint_tenant_shape(clubs, &clubs.endpoints[2]),
            TenantShape::PathScoped { fk_param } if fk_param == "club_id"
        ));
        assert!(matches!(
            d.endpoint_tenant_shape(clubs, &clubs.endpoints[3]),
            TenantShape::PathScoped { .. }
        ));
        // The tenant's OWN detail route using the conventional `/{id}` (not
        // `/{club_id}`) must ALSO be PathScoped with the tenant fk — this is the
        // load-bearing clause that lets a guard verify the tenant's own resource.
        assert!(matches!(
            d.endpoint_tenant_shape(clubs, &clubs.endpoints[4]),
            TenantShape::PathScoped { fk_param } if fk_param == "club_id"
        ));
        assert!(matches!(
            d.endpoint_tenant_shape(books, &books.endpoints[2]),
            TenantShape::PathScoped { fk_param } if fk_param == "club_id"
        ));
        // MembershipSet: a flat tenant-owned route with NO tenant param in the URL.
        assert!(matches!(
            d.endpoint_tenant_shape(customers, &customers.endpoints[1]),
            TenantShape::MembershipSet
        ));

        // A design with NO tenancy → every endpoint is None.
        let plain: Design = serde_json::from_str(MINIMAL).unwrap();
        let m = &plain.modules[0];
        assert!(matches!(
            plain.endpoint_tenant_shape(m, &m.endpoints[0]),
            TenantShape::None
        ));
    }

    #[test]
    fn normalize_renames_only_the_tenant_module_own_detail_route() {
        // Issue #78: the tenant entity's OWN `/{id}` detail route is normalized to
        // `/{club_id}` so the guard reads the tenant fk by name and 404s a
        // non-member. A nested tenant-owned child (`books/{id}` — the BOOK) and a
        // flat tenant-owned child (`customers/{id}`) MUST stay `{id}`; the tenant
        // collection root (`POST`/`GET "/"`) is unaffected. This is the security
        // fix: without the rename the guard's path branch misses and leaks another
        // tenant's row.
        //
        // Issue #89: a SIBLING entity hosted in the SAME (tenant-declaring) module
        // keeps its own `/trophies/{id}` detail routes — `{id}` there is the
        // TROPHY's key, and renaming it to `{club_id}` would mis-scope the
        // sibling's detail route to the tenant guard.
        let mut d: Design = serde_json::from_str(
            r#"{ "name": "clubs-api", "contract_version": 1,
                "auth": { "model": "session", "roles": ["owner", "member"] },
                "dependencies": ["db", "auth"],
                "tenancy": { "entity": "Club", "member_roles": ["owner", "member"] },
                "modules": [
                    { "name": "clubs",
                      "entities": [
                          { "name": "Club", "fields": [
                              { "name": "id", "type": "integer" },
                              { "name": "name", "type": "string" } ]},
                          { "name": "Trophy",
                            "belongs_to": [{ "entity": "Club" }],
                            "fields": [
                              { "name": "id", "type": "integer" },
                              { "name": "title", "type": "string" } ]}],
                      "endpoints": [
                          { "operation_id": "create_club", "method": "POST", "path": "/",
                            "request_body": { "entity": "Club" },
                            "success": { "status": 201, "entity": "Club" } },
                          { "operation_id": "get_club", "method": "GET", "path": "/{id}",
                            "success": { "status": 200, "entity": "Club" } },
                          { "operation_id": "delete_club", "method": "DELETE", "path": "/{id}",
                            "success": { "status": 204 } },
                          { "operation_id": "create_trophy", "method": "POST", "path": "/trophies",
                            "request_body": { "entity": "Trophy" },
                            "success": { "status": 201, "entity": "Trophy" } },
                          { "operation_id": "get_trophy", "method": "GET", "path": "/trophies/{id}",
                            "success": { "status": 200, "entity": "Trophy" } },
                          { "operation_id": "update_trophy", "method": "PUT", "path": "/trophies/{id}",
                            "request_body": { "entity": "Trophy" },
                            "success": { "status": 200, "entity": "Trophy" } },
                          { "operation_id": "delete_trophy", "method": "DELETE", "path": "/trophies/{id}",
                            "success": { "status": 204 } } ] },
                    { "name": "books", "mount": "/clubs/{club_id}",
                      "entities": [{ "name": "Book",
                          "belongs_to": [{ "entity": "Club" }],
                          "fields": [{ "name": "id", "type": "integer" },
                                     { "name": "title", "type": "string" }] }],
                      "endpoints": [
                          { "operation_id": "get_book", "method": "GET", "path": "/{id}",
                            "success": { "status": 200, "entity": "Book" } } ] },
                    { "name": "customers",
                      "entities": [{ "name": "Customer",
                          "belongs_to": [{ "entity": "Club" }],
                          "fields": [{ "name": "id", "type": "integer" },
                                     { "name": "email", "type": "string" }] }],
                      "endpoints": [
                          { "operation_id": "get_customer", "method": "GET", "path": "/{id}",
                            "success": { "status": 200, "entity": "Customer" } } ] }
                ] }"#,
        )
        .unwrap();
        d.normalize_tenant_detail_routes();
        // The tenant module's OWN detail routes are renamed to the tenant fk.
        assert_eq!(
            d.modules[0].endpoints[1].path, "/{club_id}",
            "GET tenant detail"
        );
        assert_eq!(
            d.modules[0].endpoints[2].path, "/{club_id}",
            "DELETE tenant detail"
        );
        // The collection root is untouched.
        assert_eq!(d.modules[0].endpoints[0].path, "/");
        // #89: the sibling entity's OWN detail routes in the SAME module keep
        // `{id}` — it is the trophy's key, not the tenant's. `get_trophy` and
        // `update_trophy` resolve to Trophy via their explicit entity;
        // `delete_trophy` (bodyless, no success entity) resolves via its
        // collection creator `POST /trophies` — which the immutable pre-pass
        // must consult BEFORE any path is rewritten.
        assert_eq!(
            d.modules[0].endpoints[4].path, "/trophies/{id}",
            "sibling GET detail untouched"
        );
        assert_eq!(
            d.modules[0].endpoints[5].path, "/trophies/{id}",
            "sibling PUT detail untouched"
        );
        assert_eq!(
            d.modules[0].endpoints[6].path, "/trophies/{id}",
            "sibling DELETE detail untouched"
        );
        // The nested child (`books/{id}` — the BOOK) and the flat child
        // (`customers/{id}`) keep `{id}` — normalizing them would misname a
        // non-tenant key.
        assert_eq!(
            d.modules[1].endpoints[0].path, "/{id}",
            "nested child untouched"
        );
        assert_eq!(
            d.modules[2].endpoints[0].path, "/{id}",
            "flat child untouched"
        );
        // Idempotent: a second pass is a no-op.
        let before = d.clone();
        d.normalize_tenant_detail_routes();
        assert_eq!(
            d.modules[0].endpoints[1].path,
            before.modules[0].endpoints[1].path
        );

        // A non-tenancy design is entirely untouched.
        let mut plain: Design = serde_json::from_str(MINIMAL).unwrap();
        let snapshot = plain.clone();
        plain.normalize_tenant_detail_routes();
        assert_eq!(
            plain.modules[0].endpoints[0].path,
            snapshot.modules[0].endpoints[0].path
        );
    }

    /// The immutable pre-pass in `normalize_own_detail_routes` is load-bearing,
    /// not a style choice: resolution reads OTHER endpoints' paths (the
    /// collection-creator arm), so a mutate-as-you-go rename would mis-resolve
    /// later endpoints against half-rewritten collection paths. Fixture: the
    /// creator's OWN path contains `{id}` (a replace-style `POST /{id}` whose
    /// body is the tenant) and the tenant is NOT the module's first entity. An
    /// interleaved implementation renames the creator first (→ `/{club_id}`),
    /// so the dependent bodyless `DELETE /{id}/{version}`'s creator lookup at
    /// `/{id}` misses, falls back to the FIRST entity (the sibling), and skips
    /// the rename — leaving a dead `{id}` the guard cannot bind. The pre-pass
    /// resolves both against pristine paths and renames both.
    #[test]
    fn normalize_pre_pass_resolves_against_pristine_paths() {
        let mut d: Design = serde_json::from_str(
            r#"{ "name": "clubs-api", "contract_version": 1,
                "auth": { "model": "session", "roles": ["owner", "member"] },
                "dependencies": ["db", "auth"],
                "tenancy": { "entity": "Club", "member_roles": ["owner", "member"] },
                "modules": [{
                    "name": "clubs",
                    "entities": [
                        { "name": "Trophy",
                          "belongs_to": [{ "entity": "Club" }],
                          "fields": [{ "name": "id", "type": "integer" },
                                     { "name": "title", "type": "string" }] },
                        { "name": "Club", "fields": [
                            { "name": "id", "type": "integer" },
                            { "name": "name", "type": "string" }] }],
                    "endpoints": [
                        { "operation_id": "replace_club", "method": "POST", "path": "/{id}",
                          "request_body": { "entity": "Club" },
                          "success": { "status": 200, "entity": "Club" } },
                        { "operation_id": "purge_club_version", "method": "DELETE",
                          "path": "/{id}/{version}",
                          "success": { "status": 204 } }
                    ]
                }]
            }"#,
        )
        .unwrap();
        d.normalize_tenant_detail_routes();
        assert_eq!(
            d.modules[0].endpoints[0].path, "/{club_id}",
            "the tenant-bodied creator itself is renamed"
        );
        assert_eq!(
            d.modules[0].endpoints[1].path, "/{club_id}/{version}",
            "the creator-resolved dependent is renamed too — an interleaved \
             implementation loses the creator at `/{{id}}` mid-rename and \
             leaves this `{{id}}` behind"
        );
    }

    #[test]
    fn field_default_round_trips_and_defaults_to_none() {
        // Issue #53a: a field may carry a server-owned `default`; it survives a
        // round trip, an absent `default` is None and is not serialized (so every
        // existing design's bytes are unchanged).
        let f: Field =
            serde_json::from_str(r#"{ "name": "confirmed", "type": "boolean", "default": false }"#)
                .unwrap();
        assert_eq!(f.default, Some(serde_json::json!(false)));
        let back = serde_json::to_value(&f).unwrap();
        assert_eq!(back["default"], serde_json::json!(false));

        let plain: Field =
            serde_json::from_str(r#"{ "name": "title", "type": "string" }"#).unwrap();
        assert!(plain.default.is_none());
        let back = serde_json::to_value(&plain).unwrap();
        assert!(
            back.get("default").is_none(),
            "absent default is not serialized: {back}"
        );
    }

    #[test]
    fn endpoint_omits_defaulted_field_detects_a_default() {
        // A create whose body entity has a `default` field takes the request DTO
        // (issue #53a); the same entity with no default field does not.
        let d: Design = serde_json::from_str(
            r#"{ "name": "news", "contract_version": 0, "dependencies": ["db"],
                "modules": [{ "name": "subs",
                    "entities": [{ "name": "Subscriber", "fields": [
                        { "name": "email", "type": "string" },
                        { "name": "confirmed", "type": "boolean", "default": false } ] }],
                    "endpoints": [{ "operation_id": "create_subscriber", "method": "POST", "path": "/",
                        "request_body": { "entity": "Subscriber" },
                        "success": { "status": 201, "entity": "Subscriber" } }] }] }"#,
        )
        .unwrap();
        let m = &d.modules[0];
        let ep = &m.endpoints[0];
        assert!(d.endpoint_omits_defaulted_field(m, ep));
        assert!(
            d.endpoint_uses_request_dto(m, ep, false),
            "auth-independent"
        );
    }

    #[test]
    fn entity_path_fk_columns_finds_the_path_redundant_parent() {
        // Issue #53b: `Checkin belongs_to Habit` created under `POST /{habit_id}/checkins`
        // → `habit_id` comes from the path, so it is a path-redundant fk. A
        // top-level `POST /leads` on a belongs_to entity yields no such column.
        let d: Design = serde_json::from_str(
            r#"{ "name": "habits", "contract_version": 0, "dependencies": ["db"],
                "modules": [{ "name": "habits",
                    "entities": [
                        { "name": "Habit", "fields": [{ "name": "name", "type": "string" }] },
                        { "name": "Checkin", "belongs_to": [{ "entity": "Habit" }],
                          "fields": [{ "name": "note", "type": "string" }] } ],
                    "endpoints": [
                        { "operation_id": "create_habit", "method": "POST", "path": "/",
                          "request_body": { "entity": "Habit" },
                          "success": { "status": 201, "entity": "Habit" } },
                        { "operation_id": "create_checkin", "method": "POST", "path": "/{habit_id}/checkins",
                          "request_body": { "entity": "Checkin" },
                          "success": { "status": 201, "entity": "Checkin" } }] }] }"#,
        )
        .unwrap();
        assert_eq!(d.entity_path_fk_columns("Checkin"), vec!["habit_id"]);
        assert!(d.entity_path_fk_columns("Habit").is_empty());
        let m = &d.modules[0];
        let create_checkin = &m.endpoints[1];
        assert!(d.endpoint_omits_path_fk(m, create_checkin));
        assert!(d.endpoint_uses_request_dto(m, create_checkin, false));
    }

    #[test]
    fn entity_path_fk_columns_is_mount_aware() {
        // Issue #82 (and the #125 create vector): the path-redundancy check resolves
        // the FULL path (module MOUNT + `ep.path`), not `ep.path` alone. `Book
        // belongs_to Club` created by `POST /` under a module mounted at
        // `/clubs/{club_id}` carries `club_id` in the MOUNT — so it is path-redundant
        // and must be dropped from the request DTO. A mount-BLIND check saw only
        // `ep.path == "/"` and left `club_id` client-controllable (the #82 friction /
        // the #125 cross-tenant-create hole).
        let mount: Design = serde_json::from_str(
            r#"{ "name": "clubs", "contract_version": 0, "dependencies": ["db"],
                "modules": [
                    { "name": "clubs",
                      "entities": [{ "name": "Club", "fields": [{ "name": "name", "type": "string" }] }],
                      "endpoints": [] },
                    { "name": "books", "mount": "/clubs/{club_id}",
                      "entities": [{ "name": "Book", "belongs_to": [{ "entity": "Club" }],
                        "fields": [{ "name": "title", "type": "string" }] }],
                      "endpoints": [
                        { "operation_id": "create_book", "method": "POST", "path": "/",
                          "request_body": { "entity": "Book" },
                          "success": { "status": 201, "entity": "Book" } }] } ] }"#,
        )
        .unwrap();
        assert_eq!(mount.entity_path_fk_columns("Book"), vec!["club_id"]);

        // The fk already in the endpoint's OWN `ep.path` (`POST /{club_id}/books`) was
        // detected by the old ep.path-only check too, so the mount-aware switch leaves
        // it UNCHANGED — no regression for designs that spelled the param on the route.
        let ep_path: Design = serde_json::from_str(
            r#"{ "name": "lib", "contract_version": 0, "dependencies": ["db"],
                "modules": [
                    { "name": "library",
                      "entities": [
                        { "name": "Club", "fields": [{ "name": "name", "type": "string" }] },
                        { "name": "Book", "belongs_to": [{ "entity": "Club" }],
                          "fields": [{ "name": "title", "type": "string" }] }],
                      "endpoints": [
                        { "operation_id": "create_book", "method": "POST", "path": "/{club_id}/books",
                          "request_body": { "entity": "Book" },
                          "success": { "status": 201, "entity": "Book" } }] } ] }"#,
        )
        .unwrap();
        assert_eq!(ep_path.entity_path_fk_columns("Book"), vec!["club_id"]);
    }

    #[test]
    fn table_name_snake_cases_and_pluralizes_by_default() {
        // The default table name is snake_case(entity) + proper English
        // pluralization — the old `lowercase + "s"` mangled multi-word names
        // (`EnergySummary` → `energysummarys`). `table_name` falls back to the
        // default for any name not carrying a `table` override, so a minimal
        // design exercises pluralization directly.
        let d: Design = serde_json::from_str(MINIMAL).unwrap();
        assert_eq!(d.table_name("EnergySummary"), "energy_summaries");
        assert_eq!(d.table_name("CaptureSession"), "capture_sessions");
        assert_eq!(d.table_name("MediaItem"), "media_items");
        assert_eq!(d.table_name("ApiKey"), "api_keys");
        // Pluralization rules: consonant+y→ies, s/x/z/ch/sh→es, vowel+y→s, else +s.
        assert_eq!(d.table_name("Todo"), "todos");
        assert_eq!(d.table_name("Class"), "classes");
        assert_eq!(d.table_name("Box"), "boxes");
        assert_eq!(d.table_name("Dish"), "dishes");
        assert_eq!(d.table_name("Batch"), "batches");
        assert_eq!(d.table_name("Gateway"), "gateways", "vowel+y → +s");
        assert_eq!(d.table_name("Company"), "companies", "consonant+y → ies");
    }

    #[test]
    fn table_override_is_used_verbatim() {
        // A frozen external schema can pin an exact table name via `table`; the
        // override wins over the pluralized default, resolved by NAME across the
        // tree so a fk/tenancy target agrees with the entity's own emission.
        let d: Design = serde_json::from_str(
            r#"{ "name": "x", "contract_version": 1, "dependencies": ["db"],
                "modules": [{ "name": "m",
                    "entities": [{ "name": "EnergySummary", "table": "legacy_energy",
                        "fields": [{ "name": "kwh", "type": "float" }] }],
                    "endpoints": [{ "operation_id": "list_it", "method": "GET", "path": "/",
                        "success": { "status": 200 } }] }] }"#,
        )
        .unwrap();
        assert_eq!(d.table_name("EnergySummary"), "legacy_energy");
        // An entity WITHOUT an override still gets the pluralized default.
        assert_eq!(d.table_name("MediaItem"), "media_items");
    }

    #[test]
    fn target_key_rust_type_resolves_pk_across_the_tree() {
        let d: Design = serde_json::from_str(V1_FULL).unwrap();
        // Workspace declares an integer id → i64 key (the fk column type a
        // belongs_to: Workspace must use). An unknown target falls back to i64.
        assert_eq!(d.target_key_rust_type("Workspace"), "i64");
        assert_eq!(d.target_key_rust_type("Nonexistent"), "i64");
    }

    #[test]
    fn minimal_design_round_trips() {
        let d: Design = serde_json::from_str(MINIMAL).unwrap();
        assert_eq!(d.name, "demo-api");
        assert_eq!(d.modules[0].endpoints.len(), 3);
        assert_eq!(d.modules[0].subroutes[0].name, "comments");
        assert!(d.modules[0].entities[0].fields[0].required); // default true
        assert!(!d.modules[0].entities[0].fields[1].required);
        let back = serde_json::to_string(&d).unwrap();
        let _re: Design = serde_json::from_str(&back).unwrap(); // serializable both ways
    }

    #[test]
    fn unknown_fields_are_rejected_like_additional_properties_false() {
        let bad = MINIMAL.replacen(
            "\"name\": \"demo-api\",",
            "\"name\": \"demo-api\", \"surprise\": 1,",
            1,
        );
        assert!(serde_json::from_str::<Design>(&bad).is_err());
    }

    #[test]
    fn method_enum_rejects_options() {
        let bad = MINIMAL.replace("\"GET\"", "\"OPTIONS\"");
        assert!(serde_json::from_str::<Design>(&bad).is_err());
    }

    #[test]
    fn public_endpoint_flag_round_trips_defaults_false_and_skips_when_false() {
        // A credential-issuing route declares itself public; the flag must
        // survive a round trip (Task 9: JL0004 carve-out for login/register).
        let pub_ep: Endpoint = serde_json::from_str(
            r#"{ "operation_id": "register", "method": "POST", "path": "/register",
                 "public": true, "success": { "status": 201 } }"#,
        )
        .unwrap();
        assert!(pub_ep.public, "public: true must deserialize");
        let back = serde_json::to_value(&pub_ep).unwrap();
        assert_eq!(back["public"], serde_json::json!(true), "round trips");

        // Default false when absent.
        let plain: Endpoint = serde_json::from_str(
            r#"{ "operation_id": "list", "method": "GET", "path": "/",
                 "success": { "status": 200 } }"#,
        )
        .unwrap();
        assert!(!plain.public, "absent public defaults to false");
        // false is skipped on serialize (mirrors unique/index), so a non-public
        // endpoint emits no `public` key.
        let back = serde_json::to_value(&plain).unwrap();
        assert!(
            back.get("public").is_none(),
            "public: false must be skipped on serialize: {back}"
        );
    }

    #[test]
    fn probe_policy_defaults_to_auto_and_round_trips_skip() {
        // Absent ⇒ auto (the ordinary happy-path probe); auto is skipped on
        // serialize so ordinary endpoints emit no `probe` key. `skip` (issue #11)
        // survives a round trip.
        let plain: Endpoint = serde_json::from_str(
            r#"{ "operation_id": "list", "method": "GET", "path": "/",
                 "success": { "status": 200 } }"#,
        )
        .unwrap();
        assert_eq!(plain.probe, ProbePolicy::Auto);
        assert!(plain.probe.is_auto());
        let back = serde_json::to_value(&plain).unwrap();
        assert!(
            back.get("probe").is_none(),
            "auto is not serialized: {back}"
        );
        let skip: Endpoint = serde_json::from_str(
            r#"{ "operation_id": "login", "method": "POST", "path": "/login",
                 "public": true, "probe": "skip", "success": { "status": 200 } }"#,
        )
        .unwrap();
        assert_eq!(skip.probe, ProbePolicy::Skip);
        assert_eq!(
            serde_json::to_value(&skip).unwrap()["probe"],
            serde_json::json!("skip")
        );
    }

    #[test]
    fn published_schema_accepts_v1_constructs() {
        // Structural spot-checks keep the published contract honest (we don't
        // run a full JSON-Schema validator).
        let s = include_str!("../../../../docs/contracts/design-schema.json");
        let v: serde_json::Value = serde_json::from_str(s).unwrap();
        assert_eq!(
            v["properties"]["contract_version"]["enum"],
            serde_json::json!([0, 1, 2])
        );
        assert!(
            s.contains("\"belongs_to\"")
                && s.contains("\"tenancy\"")
                && s.contains("\"jobs\"")
                && s.contains("\"on_delete\"")
                && s.contains("\"unique\"")
                && s.contains("\"values\"")
        );
        assert!(
            s.contains("\"storage\"")
                && s.contains("\"buckets\"")
                && s.contains("\"owner_prefix\"")
        );
        // Issue #53a: the field schema advertises the server-owned `default` key
        // (distinct from JSON-Schema's own `default` on required/unique/index).
        assert!(
            s.contains("A server-owned default value"),
            "published schema must document the `default` field key"
        );
    }

    #[test]
    fn public_read_defaults_false_and_round_trips() {
        // WHY (#105): serde-default false + skip-when-false means an existing
        // design.json without the key parses unchanged AND serializes WITHOUT
        // `public_read`, so every non-opt-in design round-trips byte-identically.
        let e: Entity = serde_json::from_str(
            r#"{ "name": "Post", "fields": [ { "name": "title", "type": "string" } ] }"#,
        )
        .unwrap();
        assert!(!e.public_read, "absent key defaults to false");
        let back = serde_json::to_value(&e).unwrap();
        assert!(
            back.get("public_read").is_none(),
            "false is not serialized: {back}"
        );
        let on: Entity = serde_json::from_str(
            r#"{ "name": "Post", "public_read": true,
                 "fields": [ { "name": "title", "type": "string" } ] }"#,
        )
        .unwrap();
        assert!(on.public_read);
        assert_eq!(
            serde_json::to_value(&on).unwrap()["public_read"],
            serde_json::json!(true),
            "an opted-in entity keeps the flag across a round trip"
        );
    }

    #[test]
    fn published_schema_accepts_public_read() {
        let s = include_str!("../../../../docs/contracts/design-schema.json");
        assert!(
            s.contains("\"public_read\""),
            "published schema must admit the entity public_read key (#105)"
        );
    }

    #[test]
    fn entity_is_public_read_requires_the_per_user_shape() {
        // WHY (#105): this classifier is the single predicate the generator,
        // testgen, and lints key public reads on — it must be true ONLY for a
        // `public_read` entity that is per-user owned (auth design + identity
        // fk + not tenant-owned), mirroring genroute's
        // `entity_is_per_user_owned` so validation and emission agree.
        let src = r#"{
            "name": "feed", "contract_version": 1,
            "auth": { "model": "session", "roles": ["admin"] },
            "dependencies": ["db", "auth"],
            "modules": [{
                "name": "posts",
                "entities": [
                    { "name": "Post", "public_read": true,
                      "belongs_to": [{ "entity": "User" }],
                      "fields": [{ "name": "title", "type": "string" }] },
                    { "name": "Draft",
                      "belongs_to": [{ "entity": "User" }],
                      "fields": [{ "name": "title", "type": "string" }] },
                    { "name": "Tag", "public_read": true,
                      "fields": [{ "name": "label", "type": "string" }] },
                    { "name": "User", "fields": [{ "name": "email", "type": "string" }] }
                ],
                "endpoints": [
                    { "operation_id": "list_posts", "method": "GET", "path": "/",
                      "success": { "status": 200, "entity": "Post", "list": true } }
                ]
            }]
        }"#;
        let d: Design = serde_json::from_str(src).unwrap();
        assert!(
            d.entity_is_public_read("Post"),
            "public_read + identity fk + auth + no tenancy → public-read"
        );
        assert!(
            !d.entity_is_public_read("Draft"),
            "per-user owned but NOT opted in → owner-scoped as before"
        );
        assert!(
            !d.entity_is_public_read("Tag"),
            "opted in but no identity fk → not public-read"
        );
        assert!(!d.entity_is_public_read("Nope"), "unknown entity → false");

        // No active auth model → the per-user shape doesn't exist → false.
        let mut no_auth = d.clone();
        no_auth.auth = None;
        no_auth.dependencies.retain(|dep| dep != "auth");
        assert!(!no_auth.entity_is_public_read("Post"));

        // Tenant-owned (Post belongs_to the tenancy root) → false: the tenant
        // guard scopes it, public_read is identity-owned-only.
        let mut tenant: Design = serde_json::from_str(src).unwrap();
        tenant.tenancy = Some(
            serde_json::from_str(r#"{ "entity": "Org", "member_roles": ["owner"] }"#).unwrap(),
        );
        tenant.modules[0].entities.push(
            serde_json::from_str(
                r#"{ "name": "Org", "fields": [{ "name": "label", "type": "string" }] }"#,
            )
            .unwrap(),
        );
        tenant.modules[0].entities[0]
            .belongs_to
            .push(serde_json::from_str(r#"{ "entity": "Org" }"#).unwrap());
        assert!(!tenant.entity_is_public_read("Post"));
    }
}
