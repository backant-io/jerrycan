//! Storage-bucket crate generation: `crates/storage/` with one bucket module
//! per design bucket. EVERYTHING here is TOOL-owned and rewritten on every
//! generate — a bucket's surface is fully deterministic from the design
//! (Rule 5: modeling + access control + endpoint generation is code, not
//! agent judgment), so unlike route crates there are no agent-owned stubs.
//! The heavy lifting (metadata SQL, checksums, signing, blob IO) lives in
//! jerrycan-storage; the generated handlers only resolve the principal into a
//! `Scope` and delegate.

use super::design::{BucketDesign, Design, Entity, ModuleDesign, Visibility};

/// Default per-object cap when the bucket omits max_size: 50 MiB — Supabase's
/// default file_size_limit, so migrated buckets behave the same.
const DEFAULT_MAX_SIZE: u64 = 50 * 1024 * 1024;

/// bucket kebab name → module ident (`user-files` → `user_files`).
fn bucket_ident(name: &str) -> String {
    name.replace('-', "_")
}

/// Buckets sorted by name — every emission site (lib.rs, mounts, tests)
/// iterates in this order so output is byte-stable.
fn sorted_buckets(design: &Design) -> Vec<&BucketDesign> {
    let mut buckets: Vec<&BucketDesign> = design
        .storage
        .as_ref()
        .map(|s| s.buckets.iter().collect())
        .unwrap_or_default();
    buckets.sort_by(|a, b| a.name.cmp(&b.name));
    buckets
}

/// How a bucket resolves its principal into a Scope.
#[derive(Clone, Copy, PartialEq)]
enum BucketScope {
    /// No owner: mutations auth-gated, rows unscoped.
    Unowned,
    /// owner = a non-tenant entity: the session user id stamps owner_id.
    User,
    /// owner = the tenancy entity: the Tenant guard's id stamps owner+tenant.
    Tenant,
    /// owner belongs_to the tenancy entity: user id + tenant id.
    UserInTenant,
}

fn find_entity<'a>(m: &'a ModuleDesign, name: &str) -> Option<&'a Entity> {
    m.entities
        .iter()
        .find(|e| e.name == name)
        .or_else(|| m.subroutes.iter().find_map(|s| find_entity(s, name)))
}

fn owner_belongs_to_tenant(design: &Design, owner: &str, tenant: &str) -> bool {
    design
        .modules
        .iter()
        .find_map(|m| find_entity(m, owner))
        .is_some_and(|e| e.belongs_to.iter().any(|b| b.entity == tenant))
}

fn bucket_scope(design: &Design, b: &BucketDesign) -> BucketScope {
    match (&b.owner, design.tenancy.as_ref()) {
        (None, _) => BucketScope::Unowned,
        (Some(o), Some(t)) if *o == t.entity => BucketScope::Tenant,
        (Some(o), Some(t)) if owner_belongs_to_tenant(design, o, &t.entity) => {
            BucketScope::UserInTenant
        }
        (Some(_), _) => BucketScope::User,
    }
}

/// The per-scope code fragments. Every string is valid Rust in its slot;
/// guard params end with ", " so they compose before other params (a trailing
/// comma in a fn signature is legal where a fragment lands last).
struct ScopeFragments {
    guard_params: &'static str,
    opt_guard_params: &'static str,
    scope_expr: &'static str,
    opt_guard_bind: &'static str,
    use_user: bool,
    use_tenant: bool,
}

fn fragments(scope: BucketScope) -> ScopeFragments {
    match scope {
        BucketScope::Unowned => ScopeFragments {
            guard_params: "_user: CurrentUser, ",
            opt_guard_params: "user: Option<CurrentUser>, ",
            scope_expr: "Scope::default()",
            opt_guard_bind: "let _user = user.ok_or_else(Error::unauthorized)?;\n    let scope = Scope::default();",
            use_user: true,
            use_tenant: false,
        },
        BucketScope::User => ScopeFragments {
            guard_params: "user: CurrentUser, ",
            opt_guard_params: "user: Option<CurrentUser>, ",
            scope_expr: "Scope { owner_id: Some(user.0.id.to_string()), tenant_id: None }",
            opt_guard_bind: "let user = user.ok_or_else(Error::unauthorized)?;\n    let scope = Scope { owner_id: Some(user.0.id.to_string()), tenant_id: None };",
            use_user: true,
            use_tenant: false,
        },
        BucketScope::Tenant => ScopeFragments {
            guard_params: "tenant: Dep<Tenant>, ",
            opt_guard_params: "tenant: Option<Dep<Tenant>>, ",
            scope_expr: "Scope { owner_id: Some(tenant.id().to_string()), tenant_id: Some(tenant.id().to_string()) }",
            opt_guard_bind: "let tenant = tenant.ok_or_else(Error::unauthorized)?;\n    let scope = Scope { owner_id: Some(tenant.id().to_string()), tenant_id: Some(tenant.id().to_string()) };",
            use_user: false,
            use_tenant: true,
        },
        BucketScope::UserInTenant => ScopeFragments {
            guard_params: "user: CurrentUser, tenant: Dep<Tenant>, ",
            opt_guard_params: "user: Option<CurrentUser>, tenant: Option<Dep<Tenant>>, ",
            scope_expr: "Scope { owner_id: Some(user.0.id.to_string()), tenant_id: Some(tenant.id().to_string()) }",
            opt_guard_bind: "let user = user.ok_or_else(Error::unauthorized)?;\n    let tenant = tenant.ok_or_else(Error::unauthorized)?;\n    let scope = Scope { owner_id: Some(user.0.id.to_string()), tenant_id: Some(tenant.id().to_string()) };",
            use_user: true,
            use_tenant: true,
        },
    }
}

/// The tool-owned `crates/storage/Cargo.toml`. `shared` carries CurrentUser/
/// Tenant (storage designs always have an active auth model — validated).
pub fn cargo_toml() -> String {
    "[package]\nname = \"storage\"\nversion.workspace = true\nedition.workspace = true\npublish = false\n\n[dependencies]\njerrycan.workspace = true\nserde.workspace = true\nserde_json.workspace = true\nshared = { path = \"../shared\" }\n\n[dev-dependencies]\ntokio.workspace = true\n".to_string()
}

/// The tool-owned `src/lib.rs`: one `pub mod` per bucket, sorted.
pub fn lib_rs(design: &Design) -> String {
    let mods: String = sorted_buckets(design)
        .iter()
        .map(|b| format!("pub mod {};\n", bucket_ident(&b.name)))
        .collect();
    format!(
        "//! GENERATED by jerrycan — storage bucket modules from design.json's `storage`\n//! block. TOOL-OWNED: `jerrycan generate` rewrites every file in this crate\n//! (bucket behavior is fully deterministic from the design; there is no agent\n//! judgment in a bucket's surface — custom logic belongs in route modules).\n#![forbid(unsafe_code)]\n\n{mods}"
    )
}

/// One TOOL-OWNED bucket module: the design-derived `BUCKET` const, `module()`,
/// and the five handlers (upload/list/download/remove/sign).
pub(crate) fn bucket_rs(design: &Design, b: &BucketDesign) -> String {
    let name = &b.name;
    let public = matches!(b.visibility, Visibility::Public);
    let max_size = b
        .max_size
        .as_deref()
        .and_then(Design::parse_size)
        .unwrap_or(DEFAULT_MAX_SIZE);
    let mime_list = b
        .allowed_mime
        .iter()
        .map(|m| format!("\"{m}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let f = fragments(bucket_scope(design, b));

    let mut uses = String::from(
        "use jerrycan::Response;\nuse jerrycan::db::Db;\nuse jerrycan::prelude::*;\nuse jerrycan::storage::{Bucket, ObjectMeta, Scope, SignedUrl, Storage};\n",
    );
    if f.use_user {
        uses.push_str("use shared::CurrentUser;\n");
    }
    if f.use_tenant {
        uses.push_str("use shared::Tenant;\n");
    }

    let guard = f.guard_params;
    let scope_expr = f.scope_expr;

    // Public reads: open list + a plain cacheable download. Private reads:
    // scoped list + a download that accepts a session OR a signed URL.
    let read_handlers = if public {
        format!(
            r#"/// GET / — list (open: public bucket), ordered by key.
pub(crate) async fn list(storage: Dep<Storage>, db: Dep<Db>) -> Result<Json<Vec<ObjectMeta>>> {{
    Ok(Json(storage.list_objects(&db, &BUCKET, None).await?))
}}

/// GET /{{id}} — download (open: public bucket). Emits `ETag` (the sha256
/// checksum) + a cache-friendly `Cache-Control`.
pub(crate) async fn download(storage: Dep<Storage>, db: Dep<Db>, Path(id): Path<String>) -> Result<Response> {{
    let (meta, bytes) = storage.get_object(&db, &BUCKET, None, &id).await?;
    jerrycan::storage::object_response(&meta, bytes, true)
}}
"#
        )
    } else {
        format!(
            r#"#[derive(serde::Deserialize)]
struct GetQuery {{
    #[serde(default)]
    exp: Option<u64>,
    #[serde(default)]
    sig: Option<String>,
}}

/// GET / — list, scoped to the caller (a foreign row never appears).
pub(crate) async fn list(storage: Dep<Storage>, db: Dep<Db>, {guard}) -> Result<Json<Vec<ObjectMeta>>> {{
    let scope = {scope_expr};
    Ok(Json(storage.list_objects(&db, &BUCKET, Some(&scope)).await?))
}}

/// GET /{{id}} — download: a scoped session OR a valid `exp`/`sig` pair (the
/// app-HMAC signed URL). A missing/failed guard on the session path reads as
/// 401 (this route's credential can also be the URL itself).
pub(crate) async fn download(storage: Dep<Storage>, db: Dep<Db>, {opt_guard}Path(id): Path<String>, Query(q): Query<GetQuery>) -> Result<Response> {{
    if let (Some(exp), Some(sig)) = (q.exp, q.sig.as_deref()) {{
        let (meta, bytes) = storage.get_signed(&db, &BUCKET, &id, exp, sig, std::time::SystemTime::now()).await?;
        return jerrycan::storage::object_response(&meta, bytes, false);
    }}
    {opt_guard_bind}
    let (meta, bytes) = storage.get_object(&db, &BUCKET, Some(&scope), &id).await?;
    jerrycan::storage::object_response(&meta, bytes, false)
}}
"#,
            opt_guard = f.opt_guard_params,
            opt_guard_bind = f.opt_guard_bind,
        )
    };

    format!(
        r#"//! GENERATED by jerrycan — storage bucket `{name}`. TOOL-OWNED: regenerated
//! by `jerrycan generate`; do not hand-edit (custom logic belongs in route
//! modules, not here).
{uses}
/// Design-derived rules for bucket `{name}` (contract v2 `storage` block).
const BUCKET: Bucket = Bucket {{
    name: "{name}",
    public: {public},
    owner_prefix: {owner_prefix},
    max_size: {max_size},
    allowed_mime: &[{mime_list}],
}};

/// This bucket's routes. `body_limit` = the bucket's max_size, so an
/// over-limit upload is the framework's 413 JC0413 before the handler runs.
pub fn module() -> Module {{
    Module::new("storage-{name}")
        .route("/", get(list).post(upload).body_limit({max_size}))
        .route("/{{id}}", get(download).delete(remove))
        .route("/{{id}}/sign", post(sign))
}}

#[derive(serde::Deserialize)]
struct UploadQuery {{
    key: String,
}}

#[derive(serde::Deserialize)]
struct SignQuery {{
    #[serde(default = "default_ttl")]
    ttl: u64,
}}

fn default_ttl() -> u64 {{
    300
}}

/// POST /?key=<path> — upload a raw body; the request `Content-Type` is the
/// stored mime (415 JC0415 outside `allowed_mime`); duplicate key = 409.
pub(crate) async fn upload(storage: Dep<Storage>, db: Dep<Db>, {guard}headers: Headers, Query(q): Query<UploadQuery>, RawBody(body): RawBody) -> Result<Created<ObjectMeta>> {{
    let mime = headers.get("content-type").unwrap_or("application/octet-stream").to_string();
    let scope = {scope_expr};
    Ok(Created(storage.put_object(&db, &BUCKET, &scope, &q.key, &mime, body).await?))
}}

{read_handlers}
/// DELETE /{{id}} — delete row + bytes, scoped (a foreign object is a 404).
pub(crate) async fn remove(storage: Dep<Storage>, db: Dep<Db>, {guard}Path(id): Path<String>) -> Result<NoContent> {{
    let scope = {scope_expr};
    storage.delete_object(&db, &BUCKET, &scope, &id).await?;
    Ok(NoContent)
}}

/// POST /{{id}}/sign?ttl=<secs> — a time-limited URL: native S3 presign when
/// the backend supports it, else app-HMAC. TTL clamps to 24h in the service.
pub(crate) async fn sign(storage: Dep<Storage>, db: Dep<Db>, {guard}Path(id): Path<String>, Query(q): Query<SignQuery>) -> Result<Json<SignedUrl>> {{
    let scope = {scope_expr};
    Ok(Json(storage.sign_object(&db, &BUCKET, &scope, &id, q.ttl, std::time::SystemTime::now()).await?))
}}
"#,
        owner_prefix = b.owner_prefix,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::design::tests::V2_STORAGE;

    fn design() -> Design {
        serde_json::from_str(V2_STORAGE).unwrap()
    }

    fn bucket<'a>(d: &'a Design, name: &str) -> &'a BucketDesign {
        d.storage
            .as_ref()
            .unwrap()
            .buckets
            .iter()
            .find(|b| b.name == name)
            .unwrap()
    }

    #[test]
    fn generation_is_deterministic_and_lib_declares_sorted_buckets() {
        let d = design();
        assert_eq!(lib_rs(&d), lib_rs(&d), "byte-identical across runs (JL0003)");
        assert_eq!(
            bucket_rs(&d, bucket(&d, "avatars")),
            bucket_rs(&d, bucket(&d, "avatars"))
        );
        let lib = lib_rs(&d);
        let a = lib.find("pub mod avatars;").unwrap();
        let i = lib.find("pub mod invoices;").unwrap();
        assert!(a < i, "bucket modules sorted by name: {lib}");
        assert!(lib.contains("GENERATED by jerrycan"), "tool-owned banner: {lib}");
    }

    /// avatars: public, owner: User (plain user scope), 5MB, image/*.
    #[test]
    fn user_owned_public_bucket_emits_the_right_guards_and_limits() {
        let d = design();
        let m = bucket_rs(&d, bucket(&d, "avatars"));
        // The design-derived const.
        assert!(m.contains("name: \"avatars\"") && m.contains("public: true"), "{m}");
        assert!(m.contains("owner_prefix: false") && m.contains("max_size: 5242880"), "{m}");
        assert!(m.contains("allowed_mime: &[\"image/*\"]"), "{m}");
        // Route table: body_limit = max_size; all five endpoints.
        assert!(m.contains(".route(\"/\", get(list).post(upload).body_limit(5242880))"), "{m}");
        assert!(m.contains(".route(\"/{id}\", get(download).delete(remove))"), "{m}");
        assert!(m.contains(".route(\"/{id}/sign\", post(sign))"), "{m}");
        // Mutations guarded by the session user; owner_id = user id.
        assert!(m.contains("async fn upload(storage: Dep<Storage>, db: Dep<Db>, user: CurrentUser,"), "{m}");
        assert!(m.contains("Scope { owner_id: Some(user.0.id.to_string()), tenant_id: None }"), "{m}");
        // Public reads: download/list take NO guard, and download is cacheable.
        assert!(m.contains("async fn download(storage: Dep<Storage>, db: Dep<Db>, Path(id): Path<String>)"), "{m}");
        assert!(m.contains("async fn list(storage: Dep<Storage>, db: Dep<Db>)"), "{m}");
        assert!(m.contains("object_response(&meta, bytes, true)"), "{m}");
        // No tenant machinery on a plain user bucket.
        assert!(!m.contains("Tenant"), "{m}");
    }

    /// invoices: private, owner: Org == tenancy.entity (tenant scope), prefix.
    #[test]
    fn tenant_owned_private_prefix_bucket_takes_the_tenant_guard() {
        let d = design();
        let m = bucket_rs(&d, bucket(&d, "invoices"));
        assert!(m.contains("public: false") && m.contains("owner_prefix: true"), "{m}");
        assert!(m.contains("use shared::Tenant;"), "{m}");
        assert!(m.contains("async fn upload(storage: Dep<Storage>, db: Dep<Db>, tenant: Dep<Tenant>,"), "{m}");
        assert!(
            m.contains("Scope { owner_id: Some(tenant.id().to_string()), tenant_id: Some(tenant.id().to_string()) }"),
            "{m}"
        );
        // Private download: optional guard OR a signed URL, both paths present.
        assert!(m.contains("tenant: Option<Dep<Tenant>>"), "{m}");
        assert!(m.contains("get_signed(&db, &BUCKET, &id, exp, sig,"), "{m}");
        assert!(m.contains("ok_or_else(Error::unauthorized)?"), "{m}");
        // Private list is scoped.
        assert!(m.contains("async fn list(storage: Dep<Storage>, db: Dep<Db>, tenant: Dep<Tenant>,"), "{m}");
        // Default max on the missing allowed_mime: allow-all.
        assert!(m.contains("allowed_mime: &[]"), "{m}");
        assert!(m.contains("max_size: 20971520"), "{m}");
    }

    /// A user-owned bucket whose owner entity belongs_to the tenant takes BOTH
    /// guards: owner_id = user id, tenant_id = tenant id.
    #[test]
    fn user_in_tenant_bucket_takes_both_guards() {
        let mut d = design();
        // Graft: User belongs_to Org, and repoint avatars' semantics.
        d.modules[0].entities[1].belongs_to = vec![serde_json::from_str(r#"{ "entity": "Org" }"#).unwrap()];
        let m = bucket_rs(&d, &d.storage.as_ref().unwrap().buckets[0].clone());
        assert!(m.contains("user: CurrentUser, tenant: Dep<Tenant>,"), "{m}");
        assert!(
            m.contains("Scope { owner_id: Some(user.0.id.to_string()), tenant_id: Some(tenant.id().to_string()) }"),
            "{m}"
        );
    }

    #[test]
    fn cargo_toml_depends_on_the_facade_and_shared() {
        let c = cargo_toml();
        assert!(c.contains("name = \"storage\"") && c.contains("jerrycan.workspace = true"), "{c}");
        assert!(c.contains("shared = { path = \"../shared\" }"), "{c}");
    }
}
