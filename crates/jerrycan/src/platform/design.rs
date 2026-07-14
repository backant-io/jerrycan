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
    pub buckets: Vec<BucketDesign>,
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
    pub success: Success,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<ErrorCase>,
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

impl Design {
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
        fn find<'a>(m: &'a ModuleDesign, target: &str) -> Option<&'a Entity> {
            m.entities
                .iter()
                .find(|e| e.name == target)
                .or_else(|| m.subroutes.iter().find_map(|s| find(s, target)))
        }
        self.modules
            .iter()
            .find_map(|m| find(m, target))
            .and_then(|e| e.fields.iter().find(|f| f.name == "id"))
            .map(|f| f.field_type.rust_type())
            .unwrap_or("i64")
    }
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
    }
}
