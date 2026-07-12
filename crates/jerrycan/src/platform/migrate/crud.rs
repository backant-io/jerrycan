//! Resolved ambiguity #5: PostgREST exposes CRUD per table, so the translated
//! design exposes the CRUD five per entity, guards derived from the RLS
//! translation. MIGRATION.md maps the old PostgREST paths onto these.

use super::pgmodel::PolicyCommand;
use super::tenancy::TableAccess;
use crate::platform::design::{Design, Endpoint, ErrorCase, HttpMethod, RequestBody, Success};

fn plural_snake(entity: &str) -> String {
    let snake = Design::to_snake(entity);
    if snake.ends_with('s') {
        format!("{snake}es")
    } else if snake.ends_with('y') {
        format!("{}ies", &snake[..snake.len() - 1])
    } else {
        format!("{snake}s")
    }
}

/// The CRUD five, filtered to `covered` commands (see
/// `tenancy::covered_commands`): Postgres default-denies a command with no
/// policy, so an uncovered command gets NO endpoint — emitting one would grant
/// what the source denies.
pub fn endpoints_for(
    entity: &str,
    access: &TableAccess,
    covered: &std::collections::BTreeSet<PolicyCommand>,
) -> Vec<Endpoint> {
    let (read_public, guarded) = match access {
        TableAccess::PublicRead { .. } => (true, true),
        TableAccess::NoRls
        | TableAccess::Gap { .. }
        | TableAccess::Tenant { .. }
        | TableAccess::OwnerAsUserTenant { .. }
        | TableAccess::AuthOnly => (false, true),
    };
    let roles_for = |cmd: PolicyCommand| -> Vec<String> {
        match access {
            TableAccess::Tenant {
                required_roles_by_command,
            } => required_roles_by_command
                .get(&cmd)
                .cloned()
                .or_else(|| required_roles_by_command.get(&PolicyCommand::All).cloned())
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    };
    let not_found = || {
        vec![ErrorCase {
            status: 404,
            code: Some("JC0404".into()),
            when: "unknown id".into(),
        }]
    };
    let plural = plural_snake(entity);
    let single = Design::to_snake(entity);
    let ep = |op: String,
              method: HttpMethod,
              path: &str,
              body: bool,
              status: u16,
              list: bool,
              errors: Vec<ErrorCase>,
              cmd: PolicyCommand,
              is_read: bool,
              has_entity: bool|
     -> Endpoint {
        let public = read_public && is_read;
        Endpoint {
            operation_id: op,
            method,
            path: path.into(),
            auth_required: guarded && !public,
            required_roles: if public { Vec::new() } else { roles_for(cmd) },
            public,
            request_body: body.then(|| RequestBody {
                entity: entity.into(),
            }),
            success: Success {
                status,
                entity: has_entity.then(|| entity.to_string()),
                list,
            },
            errors,
        }
    };
    let five = vec![
        (
            PolicyCommand::Select,
            ep(
                format!("list_{plural}"),
                HttpMethod::GET,
                "/",
                false,
                200,
                true,
                vec![],
                PolicyCommand::Select,
                true,
                true,
            ),
        ),
        (
            PolicyCommand::Insert,
            ep(
                format!("create_{single}"),
                HttpMethod::POST,
                "/",
                true,
                201,
                false,
                vec![],
                PolicyCommand::Insert,
                false,
                true,
            ),
        ),
        (
            PolicyCommand::Select,
            ep(
                format!("get_{single}"),
                HttpMethod::GET,
                "/{id}",
                false,
                200,
                false,
                not_found(),
                PolicyCommand::Select,
                true,
                true,
            ),
        ),
        (
            PolicyCommand::Update,
            ep(
                format!("update_{single}"),
                HttpMethod::PATCH,
                "/{id}",
                true,
                200,
                false,
                not_found(),
                PolicyCommand::Update,
                false,
                true,
            ),
        ),
        // delete: 204, no entity body (matches how MINIMAL expresses 204).
        (
            PolicyCommand::Delete,
            ep(
                format!("delete_{single}"),
                HttpMethod::DELETE,
                "/{id}",
                false,
                204,
                false,
                not_found(),
                PolicyCommand::Delete,
                false,
                false,
            ),
        ),
    ];
    five.into_iter()
        .filter(|(cmd, _)| covered.contains(cmd))
        .map(|(_, e)| e)
        .collect()
}

/// Full command coverage — for callers (tests, NoRls/Gap tables) that want the
/// unfiltered CRUD five.
pub fn all_commands() -> std::collections::BTreeSet<PolicyCommand> {
    [
        PolicyCommand::Select,
        PolicyCommand::Insert,
        PolicyCommand::Update,
        PolicyCommand::Delete,
    ]
    .into_iter()
    .collect()
}

/// Prefix every endpoint's path so a second+ entity in a module doesn't collide
/// with the primary entity's `/` and `/{id}` routes. Empty prefix = no change.
pub fn prefix_paths(eps: &mut [Endpoint], prefix: &str) {
    if prefix.is_empty() {
        return;
    }
    for ep in eps {
        ep.path = if ep.path == "/" {
            prefix.to_string()
        } else {
            format!("{prefix}{}", ep.path)
        };
    }
}

/// Fully guard a set of endpoints: public reads are downgraded to auth-required.
/// The orchestrator calls this when the entity is tenant-owned (questions.rs
/// forbids public on tenant-owned) + emits an advisory gap.
pub fn strip_public(eps: &mut [Endpoint]) {
    for ep in eps {
        if ep.public {
            ep.public = false;
            ep.auth_required = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::design::HttpMethod;
    use crate::platform::migrate::tenancy::TableAccess;
    use std::collections::BTreeMap;

    #[test]
    fn a_tenant_scoped_entity_gets_the_guarded_crud_five() {
        let eps = endpoints_for(
            "Customer",
            &TableAccess::Tenant {
                required_roles_by_command: BTreeMap::new(),
            },
            &all_commands(),
        );
        let ops: Vec<(&str, HttpMethod, &str)> = eps
            .iter()
            .map(|e| (e.operation_id.as_str(), e.method, e.path.as_str()))
            .collect();
        assert_eq!(
            ops,
            vec![
                ("list_customers", HttpMethod::GET, "/"),
                ("create_customer", HttpMethod::POST, "/"),
                ("get_customer", HttpMethod::GET, "/{id}"),
                ("update_customer", HttpMethod::PATCH, "/{id}"),
                ("delete_customer", HttpMethod::DELETE, "/{id}"),
            ]
        );
        assert!(
            eps.iter().all(|e| e.auth_required),
            "every tenant endpoint is guarded"
        );
        let get = &eps[2];
        assert!(
            get.errors
                .iter()
                .any(|er| er.status == 404 && er.code.as_deref() == Some("JC0404"))
        );
    }

    #[test]
    fn public_read_marks_only_the_reads_public() {
        let access = TableAccess::PublicRead {
            write: Box::new(TableAccess::AuthOnly),
        };
        let eps = endpoints_for("Plan", &access, &all_commands());
        assert!(
            eps.iter()
                .find(|e| e.operation_id == "list_plans")
                .unwrap()
                .public
        );
        assert!(
            eps.iter()
                .find(|e| e.operation_id == "get_plan")
                .unwrap()
                .public
        );
        let create = eps
            .iter()
            .find(|e| e.operation_id == "create_plan")
            .unwrap();
        assert!(create.auth_required && !create.public);
    }

    #[test]
    fn per_command_roles_flow_into_required_roles() {
        let mut roles = BTreeMap::new();
        roles.insert(
            crate::platform::migrate::pgmodel::PolicyCommand::Delete,
            vec!["owner".to_string()],
        );
        let eps = endpoints_for(
            "Customer",
            &TableAccess::Tenant {
                required_roles_by_command: roles,
            },
            &all_commands(),
        );
        let del = eps
            .iter()
            .find(|e| e.operation_id == "delete_customer")
            .unwrap();
        assert_eq!(del.required_roles, vec!["owner"]);
    }

    #[test]
    fn uncovered_commands_get_no_endpoint() {
        // A SELECT-only policy set: Postgres denies writes for everyone, so the
        // translation must not invent create/update/delete endpoints.
        let covered = [crate::platform::migrate::pgmodel::PolicyCommand::Select]
            .into_iter()
            .collect();
        let eps = endpoints_for(
            "Plan",
            &TableAccess::PublicRead {
                write: Box::new(TableAccess::AuthOnly),
            },
            &covered,
        );
        let ops: Vec<&str> = eps.iter().map(|e| e.operation_id.as_str()).collect();
        assert_eq!(ops, vec!["list_plans", "get_plan"], "reads only");
    }
}
