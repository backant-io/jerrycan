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
    if d.contract_version != 0 {
        qs.push(q(
            "/contract_version",
            "contract_version must be 0 for this platform version.",
        ));
    }
    if d.modules.is_empty() {
        qs.push(q(
            "/modules",
            "No modules defined — what are the resource areas of this backend (each becomes a route crate)?",
        ));
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
            &mut qs,
        );
    }
    qs
}

fn validate_module(
    m: &ModuleDesign,
    ptr: &str,
    declared_roles: &[&str],
    auth_declared: bool,
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
        if !(200..=299).contains(&ep.success.status) {
            qs.push(q(
                format!("{eptr}/success/status"),
                format!("Success status {} is not 2xx.", ep.success.status),
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
        if let Some(ref rb) = ep.request_body
            && !entity_names.contains(&rb.entity.as_str())
        {
            qs.push(q(
                format!("{eptr}/request_body/entity"),
                format!(
                    "Entity `{}` is not defined in module `{}` — define it or fix the reference.",
                    rb.entity, m.name
                ),
            ));
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
    }

    for (i, sub) in m.subroutes.iter().enumerate() {
        validate_module(
            sub,
            &format!("{ptr}/subroutes/{i}"),
            declared_roles,
            auth_declared,
            qs,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::design::tests::MINIMAL;

    fn design(json: &str) -> Design {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn complete_design_yields_no_questions() {
        assert!(validate(&design(MINIMAL)).is_empty());
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
        let d = design(&MINIMAL.replace("\"status\": 204", "\"status\": 302"));
        assert!(validate(&d).iter().any(|q| q.question.contains("2xx")));
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
}
