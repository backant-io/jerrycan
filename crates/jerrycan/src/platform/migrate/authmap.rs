//! Spec §5: auth.users → auth.model jwt + roles; identities → oauth dep;
//! users → seed rows preserving bcrypt hashes (verified via jerrycan-auth's
//! bcrypt dispatch). Passwords/keys are NEVER copied into config.

use crate::platform::design::{
    Auth, AuthModel, Endpoint, Entity, Field, FieldType, HttpMethod, ModuleDesign, ProbePolicy,
    RequestBody, Success,
};

pub struct AuthOutput {
    pub auth: Auth,
    pub dependencies: Vec<String>, // "auth" [+ "oauth"]
    pub users_module: ModuleDesign,
}

pub fn build_auth(member_roles: &[String], providers: &[String]) -> AuthOutput {
    let mut roles: Vec<String> = member_roles.to_vec();
    roles.sort();
    roles.dedup();
    let mut dependencies = vec!["auth".to_string()];
    let has_oauth_provider = providers
        .iter()
        .any(|p| !matches!(p.as_str(), "email" | "phone"));
    if has_oauth_provider {
        dependencies.push("oauth".to_string());
    }
    let field = |name: &str, ft: FieldType, required: bool, unique: bool| Field {
        name: name.into(),
        field_type: ft,
        required,
        unique,
        index: false,
        values: None,
        default: None,
    };
    let user = Entity {
        name: "User".into(),
        // Default table name (`users`) is exactly the target — no override.
        table: None,
        belongs_to: vec![],
        public_read: false,
        fields: vec![
            field("id", FieldType::Uuid, true, false),
            field("email", FieldType::String, true, true),
            field("password_hash", FieldType::String, false, false),
        ],
    };
    let users_module = ModuleDesign {
        name: "users".into(),
        mount: None,
        description: Some("Migrated from Supabase auth.users".into()),
        entities: vec![user],
        endpoints: vec![
            Endpoint {
                operation_id: "register".into(),
                method: HttpMethod::POST,
                path: "/register".into(),
                auth_required: false,
                required_roles: vec![],
                public: true,
                probe: ProbePolicy::Auto,
                request_body: Some(RequestBody {
                    entity: "User".into(),
                }),
                success: Success {
                    status: 201,
                    entity: Some("User".into()),
                    list: false,
                },
                errors: vec![],
            },
            Endpoint {
                operation_id: "login".into(),
                method: HttpMethod::POST,
                path: "/login".into(),
                auth_required: false,
                required_roles: vec![],
                public: true,
                // Login verifies a credential the generator can't synthesize, so
                // skip its un-greenable 2xx probe (issue #11) — keeps the migrated
                // design able to reach `jerrycan check` ok:true.
                probe: ProbePolicy::Skip,
                request_body: Some(RequestBody {
                    entity: "User".into(),
                }),
                success: Success {
                    status: 200,
                    entity: Some("User".into()),
                    list: false,
                },
                errors: vec![],
            },
        ],
        subroutes: vec![],
        dependencies: vec![],
    };
    AuthOutput {
        auth: Auth {
            model: AuthModel::Jwt,
            roles,
        },
        dependencies,
        users_module,
    }
}

/// Providers found in auth.identities data (distinct `provider` column values,
/// sorted). Streamed by the seed reader; kept separate so live mode reuses it.
pub fn providers_from_identities(
    rows: impl Iterator<Item = Vec<Option<String>>>,
    provider_idx: usize,
) -> Vec<String> {
    let mut set: std::collections::BTreeSet<String> = rows
        .filter_map(|r| r.get(provider_idx).cloned().flatten())
        .collect();
    set.remove("");
    set.into_iter().collect()
}

/// auth.users CSV → generated `users` table rows. Unmapped auth.users columns
/// are dropped (Supabase-internal). Order is stable for deterministic seeds.
pub fn user_seed_mapping() -> &'static [(&'static str, &'static str)] {
    &[
        ("id", "id"),
        ("email", "email"),
        ("encrypted_password", "password_hash"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::design::AuthModel;

    #[test]
    fn auth_users_produce_the_jwt_auth_block_and_a_users_module() {
        let out = build_auth(
            &["owner".to_string(), "member".to_string()], // member_roles from tenancy
            &["google".to_string()],                      // providers from auth.identities
        );
        assert_eq!(out.auth.model, AuthModel::Jwt, "Supabase auth is JWT");
        assert_eq!(out.auth.roles, vec!["member", "owner"], "sorted, deduped");
        assert!(out.dependencies.contains(&"auth".to_string()));
        assert!(
            out.dependencies.contains(&"oauth".to_string()),
            "google identity → oauth dep"
        );
        let users = &out.users_module;
        assert_eq!(users.name, "users");
        let user = &users.entities[0];
        assert_eq!(user.name, "User");
        let email = user.fields.iter().find(|f| f.name == "email").unwrap();
        assert!(email.unique);
        let hash = user
            .fields
            .iter()
            .find(|f| f.name == "password_hash")
            .unwrap();
        assert!(!hash.required, "oauth-only users have no password hash");
        // register + login are public (JL0004 carve-out), matching the reference slice.
        assert!(
            users
                .endpoints
                .iter()
                .any(|e| e.operation_id == "register" && e.public)
        );
        assert!(
            users
                .endpoints
                .iter()
                .any(|e| e.operation_id == "login" && e.public)
        );
    }

    #[test]
    fn no_identity_providers_means_no_oauth_dependency() {
        let out = build_auth(&[], &[]);
        assert!(!out.dependencies.contains(&"oauth".to_string()));
    }
}
