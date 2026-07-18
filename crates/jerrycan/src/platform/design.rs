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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestBody {
    pub entity: String,
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

impl Design {
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
        serde_json::from_str(&raw).map_err(|e| format!("invalid design.json: {e}"))
    }

    /// Entities owned by the tenant: any entity (in any module or subroute)
    /// with a belongs_to aimed at tenancy.entity. (module_name, entity_name) pairs.
    pub fn tenant_owned(&self) -> Vec<(&str, &str)> {
        let Some(tenancy) = self.tenancy.as_ref() else {
            return Vec::new();
        };
        let mut owned = Vec::new();
        for module in &self.modules {
            collect_tenant_owned(module, &tenancy.entity, &mut owned);
        }
        owned
    }

    /// The fk column a belongs_to derives: snake_case(target) + "_id".
    pub fn fk_column(target: &str) -> String {
        format!("{}_id", Self::to_snake(target))
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
    // Scaffolding: the membership-verifying guard codegen (issues #78/#79) is the
    // consumer, landing in a later task; remove this allow once it is wired in.
    #[allow(dead_code)]
    pub(crate) fn endpoint_tenant_shape(
        &self,
        module: &ModuleDesign,
        ep: &Endpoint,
    ) -> TenantShape {
        let Some(tenancy) = self.tenancy.as_ref() else {
            return TenantShape::None;
        };
        let is_tenant_module = module.entities.iter().any(|e| e.name == tenancy.entity);
        let owns_tenant_entity = module
            .entities
            .iter()
            .any(|e| e.belongs_to.iter().any(|b| b.entity == tenancy.entity));
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
        // (a nested mount like `/clubs/{club_id}`), else membership-scoped.
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
    /// uses.
    pub(crate) fn is_identity_fk(b: &BelongsTo) -> bool {
        Self::fk_column(&b.entity) == AUTH_IDENTITY_FK_COLUMN
    }

    /// True when the entity carries an identity FK (a belongs_to aimed at the
    /// auth identity entity). Such an entity's GUARDED request bodies omit
    /// `user_id` — the server injects the session user's id (issue #34).
    pub(crate) fn has_identity_fk(e: &Entity) -> bool {
        e.belongs_to.iter().any(Self::is_identity_fk)
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
                m.entities
                    .iter()
                    .find(|e| e.name == rb.entity)
                    .is_some_and(Self::has_identity_fk)
            })
    }

    /// The body entity of this endpoint that lives in `m`, if any (the request DTO
    /// is per-entity, so every request-shape decision resolves the entity here).
    fn request_entity<'a>(&self, m: &'a ModuleDesign, ep: &Endpoint) -> Option<&'a Entity> {
        let rb = ep.request_body.as_ref()?;
        m.entities.iter().find(|e| e.name == rb.entity)
    }

    /// The defaulted-field rule (issue #53a): this endpoint's request-body entity
    /// carries at least one `default` field, so the wire contract drops it (the
    /// server applies the declared value). Independent of auth — a public create
    /// (`POST /subscribers`) still omits `confirmed`/`status`.
    pub(crate) fn endpoint_omits_defaulted_field(&self, m: &ModuleDesign, ep: &Endpoint) -> bool {
        self.request_entity(m, ep)
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
            .map(|b| Self::fk_column(&b.entity))
            .filter(|col| self.any_body_endpoint_path_has(entity_name, col))
            .collect()
    }

    /// True when some endpoint whose request body is `entity_name` has a path
    /// segment `{col}` (the parent fk the path already carries). Walks the whole
    /// design tree (modules + subroutes).
    fn any_body_endpoint_path_has(&self, entity_name: &str, col: &str) -> bool {
        fn walk(m: &ModuleDesign, entity_name: &str, token: &str) -> bool {
            m.endpoints.iter().any(|ep| {
                ep.request_body
                    .as_ref()
                    .is_some_and(|rb| rb.entity == entity_name)
                    && ep.path.contains(token)
            }) || m.subroutes.iter().any(|s| walk(s, entity_name, token))
        }
        let token = format!("{{{col}}}");
        self.modules.iter().any(|m| walk(m, entity_name, &token))
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
                    .is_some_and(|rb| rb.entity == entity)
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

/// Walk a module and its subroutes in document order, pairing each entity
/// that belongs_to `tenant` with the owning module/subroute name.
fn collect_tenant_owned<'a>(
    module: &'a ModuleDesign,
    tenant: &str,
    out: &mut Vec<(&'a str, &'a str)>,
) {
    for entity in &module.entities {
        if entity.belongs_to.iter().any(|b| b.entity == tenant) {
            out.push((module.name.as_str(), entity.name.as_str()));
        }
    }
    for subroute in &module.subroutes {
        collect_tenant_owned(subroute, tenant, out);
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

    #[test]
    fn fk_column_is_snake_target_id() {
        assert_eq!(Design::fk_column("Workspace"), "workspace_id");
        assert_eq!(Design::fk_column("ApiKey"), "api_key_id");
        // fk_column derives from the shared to_snake (DRY); both must agree.
        assert_eq!(Design::to_snake("ApiKey"), "api_key");
        assert_eq!(Design::to_snake("Lead"), "lead");
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
                            "success": { "status": 204 } } ] },
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
}
