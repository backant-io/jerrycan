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

impl ModuleDesign {
    /// Where this module mounts (under the app, or under its parent for subroutes).
    pub fn effective_mount(&self) -> String {
        self.mount
            .clone()
            .unwrap_or_else(|| format!("/{}", self.name))
    }
}

impl Design {
    pub fn from_path(path: &std::path::Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        serde_json::from_str(&raw).map_err(|e| format!("invalid design.json: {e}"))
    }
}

#[cfg(test)]
mod tests {
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
