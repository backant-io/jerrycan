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
    pub modules: Vec<ModuleDesign>,
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
    #[serde(default)]
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
    /// Entity names are validated `^[A-Z][A-Za-z0-9]*$`, so each uppercase
    /// letter (past the first char) starts a new word: "ApiKey" -> "api_key".
    pub fn fk_column(target: &str) -> String {
        let mut snake = String::with_capacity(target.len() + 4);
        for (i, ch) in target.char_indices() {
            if i > 0 && ch.is_ascii_uppercase() {
                snake.push('_');
            }
            snake.push(ch.to_ascii_lowercase());
        }
        snake.push_str("_id");
        snake
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
        "name": "kolli-mini", "contract_version": 1,
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
    fn v0_designs_still_parse_unchanged() {
        let d: Design = serde_json::from_str(MINIMAL).unwrap();
        assert_eq!(d.contract_version, 0);
        assert!(d.tenancy.is_none() && d.jobs.is_empty());
        assert!(d.modules[0].entities[0].belongs_to.is_empty());
    }

    #[test]
    fn tenant_owned_walks_modules_and_subroutes() {
        let d: Design = serde_json::from_str(V1_FULL).unwrap();
        assert_eq!(d.tenant_owned(), vec![("leads", "Lead")]);
    }

    #[test]
    fn fk_column_is_snake_target_id() {
        assert_eq!(Design::fk_column("Workspace"), "workspace_id");
        assert_eq!(Design::fk_column("ApiKey"), "api_key_id");
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
}
