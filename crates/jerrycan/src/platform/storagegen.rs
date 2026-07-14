//! Storage-bucket crate generation: `crates/storage/` with one bucket module
//! per design bucket. EVERYTHING here is TOOL-owned and rewritten on every
//! generate — a bucket's surface is fully deterministic from the design
//! (Rule 5: modeling + access control + endpoint generation is code, not
//! agent judgment), so unlike route crates there are no agent-owned stubs.
//! The heavy lifting (metadata SQL, checksums, signing, blob IO) lives in
//! jerrycan-storage; the generated handlers only resolve the principal into a
//! `Scope` and delegate.

use super::design::{BucketDesign, Design, Entity, ModuleDesign, Visibility};
use std::fs;
use std::path::Path;

/// Default per-object cap when the bucket omits max_size: 50 MiB — Supabase's
/// default file_size_limit, so migrated buckets behave the same.
const DEFAULT_MAX_SIZE: u64 = 50 * 1024 * 1024;

/// bucket kebab name → module ident (`user-files` → `user_files`).
fn bucket_ident(name: &str) -> String {
    name.replace('-', "_")
}

/// The full path prefix buckets mount UNDER: the app base_path (issue #16) +
/// the storage base_path (issue #8), e.g. `/storage` or `/v1/storage`. This is
/// exactly what `mounting.rs` prepends to each bucket mount, so the generated
/// `Bucket.mount_prefix` (→ signed URLs) and the acceptance harness agree with
/// the served routes.
fn full_mount_prefix(design: &Design) -> String {
    let storage_base = design
        .storage
        .as_ref()
        .map(|s| s.effective_base_path())
        .unwrap_or_else(|| "/storage".to_string());
    format!("{}{storage_base}", design.base_prefix())
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
    // The full prefix this bucket mounts UNDER (app base + storage base), baked
    // into the const so app-HMAC signed URLs resolve to the real download route.
    let mount_prefix = full_mount_prefix(design);
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
        String::from(
            r#"/// GET / — list (open: public bucket), ordered by key.
pub(crate) async fn list(storage: Dep<Storage>, db: Dep<Db>) -> Result<Json<Vec<ObjectMeta>>> {
    Ok(Json(storage.list_objects(&db, &BUCKET, None).await?))
}

/// GET /{id} — download (open: public bucket). Emits `ETag` (the sha256
/// checksum) + a cache-friendly `Cache-Control`.
pub(crate) async fn download(storage: Dep<Storage>, db: Dep<Db>, Path(id): Path<String>) -> Result<Response> {
    let (meta, bytes) = storage.get_object(&db, &BUCKET, None, &id).await?;
    jerrycan::storage::object_response(&meta, bytes, true)
}
"#,
        )
    } else {
        format!(
            r#"#[derive(serde::Deserialize)]
pub(crate) struct GetQuery {{
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
    mount_prefix: "{mount_prefix}",
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
pub(crate) struct UploadQuery {{
    key: String,
}}

#[derive(serde::Deserialize)]
pub(crate) struct SignQuery {{
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

/// The module owning the design's tenancy entity (its migration carries the
/// `{tenant}_members` table the Tenant guard queries).
fn tenant_module(design: &Design) -> Option<&ModuleDesign> {
    let tenancy = design.tenancy.as_ref()?;
    design
        .modules
        .iter()
        .find(|m| m.entities.iter().any(|e| e.name == tenancy.entity))
}

/// A concrete Content-Type the bucket accepts (for generated happy paths):
/// the first allowlist entry with `*` resolved, or octet-stream.
fn concrete_mime(b: &BucketDesign) -> String {
    match b.allowed_mime.first().map(String::as_str) {
        None | Some("*/*") => "application/octet-stream".to_string(),
        Some(pat) => match pat.strip_suffix("/*") {
            Some("image") => "image/png".to_string(),
            Some(prefix) => format!("{prefix}/test"),
            None => pat.to_string(),
        },
    }
}

/// The tool-owned `crates/storage/tests/acceptance.rs`: per-bucket round
/// trips, guard checks, and the isolation NEGATIVE CONTROLS (cross-owner,
/// cross-tenant via memberships, cross-prefix). These PASS on generation
/// (handlers are real, not stubs) and turn RED if any scope is broken —
/// gen-tests does NOT count them toward expected_failing.
pub fn acceptance_rs(design: &Design) -> String {
    let buckets = sorted_buckets(design);
    // The full mount prefix the generated app uses (app base + storage base): the
    // harness mounts and requests buckets under it so it exercises the real path
    // AND the app-HMAC signed URLs (baked with the same prefix) resolve (#8, #16).
    let base = full_mount_prefix(design);
    let needs_tenant = buckets.iter().any(|b| {
        matches!(
            bucket_scope(design, b),
            BucketScope::Tenant | BucketScope::UserInTenant
        )
    });

    // Tenant plumbing (migration include + tenant 1/2 + membership seeds),
    // reusing testgen's column/value derivation so the two seeds can't drift.
    let (seed_use, tenant_setup, tenant_dep) = if needs_tenant {
        let tenancy = design
            .tenancy
            .as_ref()
            .expect("validated: tenant buckets require tenancy");
        let t = tenant_module(design).expect("validated: tenancy entity is declared");
        let entity = t
            .entities
            .iter()
            .find(|e| e.name == tenancy.entity)
            .expect("validated: tenancy entity in its module");
        let t_snake = t.name.replace('-', "_");
        let table = design.table_name(&tenancy.entity);
        let members = format!("{}_members", Design::to_snake(&tenancy.entity));
        let fk = Design::fk_column(&tenancy.entity);
        let role = tenancy
            .member_roles
            .first()
            .map(String::as_str)
            .unwrap_or("owner");
        let (cols1, vals1) = super::testgen::tenant_row_cols_vals(entity, "1", 1);
        let (cols2, vals2) = super::testgen::tenant_row_cols_vals(entity, "2", 2);
        let setup = format!(
            "    db.migrate(&[\n        jerrycan::db::Migration {{\n            name: \"{t_snake}_0001_create_tables\",\n            sqlite: include_str!(\"../../routes/{t_name}/migrations/sqlite/0001_create_tables.sql\"),\n            postgres: include_str!(\"../../routes/{t_name}/migrations/postgres/0001_create_tables.sql\"),\n        }},\n    ])\n    .await\n    .expect(\"tenant migrations\");\n    db.conn().execute_unprepared(\"INSERT INTO \\\"{table}\\\" ({cols1}) VALUES ({vals1})\").await.expect(\"seed tenant 1\");\n    db.conn().execute_unprepared(\"INSERT INTO \\\"{table}\\\" ({cols2}) VALUES ({vals2})\").await.expect(\"seed tenant 2\");\n    db.conn().execute_unprepared(\"INSERT INTO \\\"{members}\\\" (user_id, {fk}, role) VALUES (1, 1, '{role}')\").await.expect(\"seed membership 1\");\n    db.conn().execute_unprepared(\"INSERT INTO \\\"{members}\\\" (user_id, {fk}, role) VALUES (2, 2, '{role}')\").await.expect(\"seed membership 2\");\n",
            t_name = t.name,
        );
        (
            "use jerrycan::db::sea_orm::ConnectionTrait;\n\n".to_string(),
            setup,
            "        .provide_dep(shared::tenant)\n".to_string(),
        )
    } else {
        (String::new(), String::new(), String::new())
    };

    let mounts: String = buckets
        .iter()
        .map(|b| {
            format!(
                "        .mount(\"{base}/{}\", storage::{}::module())\n",
                b.name,
                bucket_ident(&b.name)
            )
        })
        .collect();

    let mut tests = String::new();
    for b in &buckets {
        bucket_tests(design, b, &base, &mut tests);
    }

    format!(
        "//! GENERATED by jerrycan — TOOL-OWNED storage acceptance + isolation tests\n\
         //! from design.json's `storage` block. These pass on generation (bucket\n\
         //! handlers are real implementations) and turn RED if any owner/tenant/\n\
         //! prefix scope is broken — the negative controls ARE the security gate.\n\
         //! Regenerated on demand; add your own tests in sibling files, not here.\n\
         use jerrycan::prelude::*;\n\n\
         {seed_use}\
         const TEST_SECRET: &str = \"a-very-long-development-secret-string!!\";\n\n\
         fn test_cookie_for(user_id: i64) -> String {{\n\
         \x20   let auth = jerrycan::auth::Auth::with_secret(TEST_SECRET);\n\
         \x20   let token = auth.sessions().encode(&shared::SessionUser {{ id: user_id.to_string(), role: \"admin\".into() }}).expect(\"encode\");\n\
         \x20   format!(\"jerrycan_session={{token}}\")\n\
         }}\n\n\
         async fn app() -> TestApp {{\n\
         \x20   let db = jerrycan::db::Db::connect(\"sqlite::memory:\").await.expect(\"test db\");\n\
         \x20   db.migrate(jerrycan::storage::STORAGE_MIGRATIONS).await.expect(\"storage migrations\");\n\
         {tenant_setup}\
         \x20   App::new()\n\
         \x20       .extend(jerrycan::auth::Auth::with_secret(TEST_SECRET))\n\
         \x20       .extend(jerrycan::storage::Storage::memory().with_sign_secret(TEST_SECRET))\n\
         \x20       .extend(db)\n\
         {tenant_dep}\
         {mounts}\
         \x20       .into_test()\n\
         }}\n\n\
         {tests}"
    )
}

/// The per-bucket test battery. Emission is conditional on the bucket's shape;
/// every emitted body is complete (no agent TODOs — the surface is generated).
/// `base` is the storage mount prefix (e.g. `/storage`); every request path and
/// the test-app mount go under `{base}/{bucket}` so the harness exercises the
/// SAME mounted path the generated app serves (issue #8).
fn bucket_tests(design: &Design, b: &BucketDesign, base: &str, out: &mut String) {
    let name = &b.name;
    let mount = format!("{base}/{name}");
    let ident = bucket_ident(name);
    let public = matches!(b.visibility, Visibility::Public);
    let owned = bucket_scope(design, b) != BucketScope::Unowned;
    let mime = concrete_mime(b);
    let cache = if public {
        "public, max-age=3600"
    } else {
        "private, no-store"
    };

    // 1. Round trip + ETag/Cache-Control (headers checked on every bucket).
    let download_1 = if public {
        format!("t.get(&format!(\"{mount}/{{id}}\")).await")
    } else {
        format!(
            "t.get_with(&format!(\"{mount}/{{id}}\"), &[(\"cookie\", &test_cookie_for(1))]).await"
        )
    };
    out.push_str(&format!(
        "#[tokio::test]\nasync fn {ident}_upload_then_download_round_trips() {{\n    let t = app().await;\n    let created = t.post_bytes_with(\"{mount}?key=probe.bin\", b\"{ident}-bytes\", &[(\"cookie\", &test_cookie_for(1)), (\"content-type\", \"{mime}\")]).await;\n    assert_eq!(created.status().as_u16(), 201, \"upload; body: {{}}\", created.text());\n    let meta: serde_json::Value = serde_json::from_str(&created.text()).expect(\"meta json\");\n    let id = meta[\"id\"].as_str().expect(\"id\").to_string();\n    let checksum = meta[\"checksum\"].as_str().expect(\"checksum\").to_string();\n    let res = {download_1};\n    assert_eq!(res.status().as_u16(), 200, \"download; body: {{}}\", res.text());\n    assert_eq!(res.bytes(), &b\"{ident}-bytes\"[..]);\n    let etag = res.headers().get(\"etag\").and_then(|v| v.to_str().ok()).expect(\"etag header\");\n    assert_eq!(etag, format!(\"\\\"{{checksum}}\\\"\"), \"ETag is the sha256 checksum\");\n    let cc = res.headers().get(\"cache-control\").and_then(|v| v.to_str().ok()).expect(\"cache-control header\");\n    assert_eq!(cc, \"{cache}\");\n}}\n\n"
    ));

    // 2. Mutations are always guarded.
    out.push_str(&format!(
        "#[tokio::test]\nasync fn {ident}_upload_without_auth_is_401() {{\n    let t = app().await;\n    let res = t.post_bytes_with(\"{mount}?key=noauth.bin\", b\"x\", &[(\"content-type\", \"{mime}\")]).await;\n    assert_eq!(res.status().as_u16(), 401, \"mutations are always guarded; body: {{}}\", res.text());\n}}\n\n"
    ));

    // 3. Private reads are guarded.
    if !public {
        out.push_str(&format!(
            "#[tokio::test]\nasync fn {ident}_download_without_auth_is_401() {{\n    let t = app().await;\n    let created = t.post_bytes_with(\"{mount}?key=guard.bin\", b\"x\", &[(\"cookie\", &test_cookie_for(1)), (\"content-type\", \"{mime}\")]).await;\n    let meta: serde_json::Value = serde_json::from_str(&created.text()).expect(\"meta json\");\n    let id = meta[\"id\"].as_str().expect(\"id\");\n    let res = t.get(&format!(\"{mount}/{{id}}\")).await;\n    assert_eq!(res.status().as_u16(), 401, \"private read without a session; body: {{}}\", res.text());\n}}\n\n"
        ));
    }

    // 4. allowed_mime → 415 JC0415.
    if !b.allowed_mime.is_empty() {
        out.push_str(&format!(
            "#[tokio::test]\nasync fn {ident}_disallowed_mime_is_415() {{\n    let t = app().await;\n    let res = t.post_bytes_with(\"{mount}?key=bad-mime.bin\", b\"x\", &[(\"cookie\", &test_cookie_for(1)), (\"content-type\", \"application/x-jerrycan-blocked\")]).await;\n    assert_eq!(res.status().as_u16(), 415, \"design: allowed_mime violation is 415 JC0415; body: {{}}\", res.text());\n    assert!(res.text().contains(\"JC0415\"), \"body: {{}}\", res.text());\n}}\n\n"
        ));
    }

    // 5. max_size → 413 (the route's body_limit fires at the transport).
    if let Some(max) = b.max_size.as_deref().and_then(Design::parse_size) {
        out.push_str(&format!(
            "#[tokio::test]\nasync fn {ident}_oversize_upload_is_413() {{\n    let t = app().await;\n    let body = vec![0u8; {over}];\n    let res = t.post_bytes_with(\"{mount}?key=huge.bin\", &body, &[(\"cookie\", &test_cookie_for(1)), (\"content-type\", \"{mime}\")]).await;\n    assert_eq!(res.status().as_u16(), 413, \"design: max_size violation is 413 JC0413; body: {{}}\", res.text());\n}}\n\n",
            over = max + 1,
        ));
    }

    // 6. Cross-owner/cross-tenant negative control (owned buckets). User 2 is
    // a different owner AND (for tenant buckets, via the seeded memberships) a
    // different tenant, so this single control covers both scopes.
    if owned {
        let read_leg = if public {
            String::new() // public read is open by design — only writes are scoped.
        } else {
            format!(
                "    let foreign = t.get_with(&format!(\"{mount}/{{id}}\"), &[(\"cookie\", &test_cookie_for(2))]).await;\n    assert_eq!(foreign.status().as_u16(), 404, \"cross-owner get must 404; body: {{}}\", foreign.text());\n    let listed = t.get_with(\"{mount}\", &[(\"cookie\", &test_cookie_for(2))]).await;\n    assert_eq!(listed.status().as_u16(), 200, \"user 2 lists their own objects; body: {{}}\", listed.text());\n    assert!(!listed.text().contains(&id), \"cross-owner list must not leak the foreign id; body: {{}}\", listed.text());\n"
            )
        };
        let survive_leg = if public {
            format!(
                "    let survives = t.get(&format!(\"{mount}/{{id}}\")).await;\n    assert_eq!(survives.status().as_u16(), 200, \"the row must survive a cross-owner delete; body: {{}}\", survives.text());\n"
            )
        } else {
            format!(
                "    let survives = t.get_with(&format!(\"{mount}/{{id}}\"), &[(\"cookie\", &test_cookie_for(1))]).await;\n    assert_eq!(survives.status().as_u16(), 200, \"the row must survive a cross-owner delete; body: {{}}\", survives.text());\n"
            )
        };
        out.push_str(&format!(
            "/// SECURITY: user/tenant 2 must not reach owner 1's `{name}` objects —\n/// this is the isolation contract; breaking any scope turns it red.\n#[tokio::test]\nasync fn {ident}_cross_owner_access_is_denied() {{\n    let t = app().await;\n    let created = t.post_bytes_with(\"{mount}?key=mine.bin\", b\"mine\", &[(\"cookie\", &test_cookie_for(1)), (\"content-type\", \"{mime}\")]).await;\n    assert_eq!(created.status().as_u16(), 201, \"setup; body: {{}}\", created.text());\n    let meta: serde_json::Value = serde_json::from_str(&created.text()).expect(\"meta json\");\n    let id = meta[\"id\"].as_str().expect(\"id\").to_string();\n{read_leg}    let del = t.delete_with(&format!(\"{mount}/{{id}}\"), &[(\"cookie\", &test_cookie_for(2))]).await;\n    assert_eq!(del.status().as_u16(), 404, \"cross-owner delete must 404; body: {{}}\", del.text());\n{survive_leg}}}\n\n"
        ));
    }

    // 7. owner_prefix negative control: same relative key, two owners, two
    // distinct prefixed objects; B never reaches A's.
    if b.owner_prefix {
        out.push_str(&format!(
            "/// SECURITY: owner_prefix isolates keys per owner (Supabase\n/// folder-per-user parity): the same relative key lands under each owner's\n/// prefix and never collides or crosses.\n#[tokio::test]\nasync fn {ident}_owner_prefix_isolates_keys() {{\n    let t = app().await;\n    let a = t.post_bytes_with(\"{mount}?key=same.bin\", b\"a\", &[(\"cookie\", &test_cookie_for(1)), (\"content-type\", \"{mime}\")]).await;\n    assert_eq!(a.status().as_u16(), 201, \"owner 1 upload; body: {{}}\", a.text());\n    let a_meta: serde_json::Value = serde_json::from_str(&a.text()).expect(\"meta json\");\n    assert_eq!(a_meta[\"key\"], serde_json::json!(\"1/same.bin\"), \"key is stored under owner 1's prefix\");\n    let b = t.post_bytes_with(\"{mount}?key=same.bin\", b\"b\", &[(\"cookie\", &test_cookie_for(2)), (\"content-type\", \"{mime}\")]).await;\n    assert_eq!(b.status().as_u16(), 201, \"same relative key, different prefix — no collision; body: {{}}\", b.text());\n    let b_meta: serde_json::Value = serde_json::from_str(&b.text()).expect(\"meta json\");\n    assert_eq!(b_meta[\"key\"], serde_json::json!(\"2/same.bin\"));\n    let a_id = a_meta[\"id\"].as_str().expect(\"id\");\n    let cross = t.delete_with(&format!(\"{mount}/{{a_id}}\"), &[(\"cookie\", &test_cookie_for(2))]).await;\n    assert_eq!(cross.status().as_u16(), 404, \"cross-prefix delete must 404; body: {{}}\", cross.text());\n}}\n\n"
        ));
    }

    // 8. Signed URL: grants without a session; (private) a tampered sig fails.
    let tamper_leg = if public {
        String::new()
    } else {
        format!(
            "    let bad = t.get(&format!(\"{{url}}0\")).await;\n    assert_eq!(bad.status().as_u16(), 401, \"{name} tampered signed URL must be rejected; body: {{}}\", bad.text());\n"
        )
    };
    out.push_str(&format!(
        "#[tokio::test]\nasync fn {ident}_signed_url_grants_and_rejects() {{\n    let t = app().await;\n    let created = t.post_bytes_with(\"{mount}?key=to-sign.bin\", b\"signed\", &[(\"cookie\", &test_cookie_for(1)), (\"content-type\", \"{mime}\")]).await;\n    assert_eq!(created.status().as_u16(), 201, \"setup; body: {{}}\", created.text());\n    let meta: serde_json::Value = serde_json::from_str(&created.text()).expect(\"meta json\");\n    let id = meta[\"id\"].as_str().expect(\"id\");\n    let signed = t.post_json_with(&format!(\"{mount}/{{id}}/sign\"), &serde_json::json!({{}}), &[(\"cookie\", &test_cookie_for(1))]).await;\n    assert_eq!(signed.status().as_u16(), 200, \"sign; body: {{}}\", signed.text());\n    let url = serde_json::from_str::<serde_json::Value>(&signed.text()).expect(\"json\")[\"url\"].as_str().expect(\"url\").to_string();\n    let ok = t.get(&url).await;\n    assert_eq!(ok.status().as_u16(), 200, \"a signed URL needs no session; body: {{}}\", ok.text());\n    assert_eq!(ok.bytes(), &b\"signed\"[..]);\n{tamper_leg}}}\n\n"
    ));
}

/// Write (or refresh) the generated `crates/storage/` crate under `target`
/// (the app root). EVERY file is TOOL-owned and rewritten each run. Returns
/// paths relative to `target`. Precondition: the design passed
/// `questions::validate` (v2, db, active auth, well-formed buckets).
pub fn write_storage(target: &Path, design: &Design) -> Result<Vec<String>, String> {
    let crate_dir = target.join("crates/storage");
    fs::create_dir_all(crate_dir.join("src")).map_err(|e| e.to_string())?;
    let mut created = Vec::new();
    let mut write_tool = |rel: &str, content: &str| -> Result<(), String> {
        let path = crate_dir.join(rel);
        fs::create_dir_all(path.parent().expect("parent")).map_err(|e| e.to_string())?;
        fs::write(&path, content).map_err(|e| format!("write {}: {e}", path.display()))?;
        created.push(format!("crates/storage/{rel}"));
        Ok(())
    };
    write_tool("Cargo.toml", &cargo_toml())?;
    write_tool("src/lib.rs", &lib_rs(design))?;
    for b in sorted_buckets(design) {
        write_tool(
            &format!("src/{}.rs", bucket_ident(&b.name)),
            &bucket_rs(design, b),
        )?;
    }
    write_tool("tests/acceptance.rs", &acceptance_rs(design))?;
    Ok(created)
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
        assert_eq!(
            lib_rs(&d),
            lib_rs(&d),
            "byte-identical across runs (JL0003)"
        );
        assert_eq!(
            bucket_rs(&d, bucket(&d, "avatars")),
            bucket_rs(&d, bucket(&d, "avatars"))
        );
        let lib = lib_rs(&d);
        let a = lib.find("pub mod avatars;").unwrap();
        let i = lib.find("pub mod invoices;").unwrap();
        assert!(a < i, "bucket modules sorted by name: {lib}");
        assert!(
            lib.contains("GENERATED by jerrycan"),
            "tool-owned banner: {lib}"
        );
    }

    /// avatars: public, owner: User (plain user scope), 5MB, image/*.
    #[test]
    fn user_owned_public_bucket_emits_the_right_guards_and_limits() {
        let d = design();
        let m = bucket_rs(&d, bucket(&d, "avatars"));
        // The design-derived const.
        assert!(
            m.contains("name: \"avatars\"") && m.contains("public: true"),
            "{m}"
        );
        assert!(
            m.contains("owner_prefix: false") && m.contains("max_size: 5242880"),
            "{m}"
        );
        assert!(m.contains("allowed_mime: &[\"image/*\"]"), "{m}");
        // Route table: body_limit = max_size; all five endpoints.
        assert!(
            m.contains(".route(\"/\", get(list).post(upload).body_limit(5242880))"),
            "{m}"
        );
        assert!(
            m.contains(".route(\"/{id}\", get(download).delete(remove))"),
            "{m}"
        );
        assert!(m.contains(".route(\"/{id}/sign\", post(sign))"), "{m}");
        // Mutations guarded by the session user; owner_id = user id.
        assert!(
            m.contains("async fn upload(storage: Dep<Storage>, db: Dep<Db>, user: CurrentUser,"),
            "{m}"
        );
        assert!(
            m.contains("Scope { owner_id: Some(user.0.id.to_string()), tenant_id: None }"),
            "{m}"
        );
        // Public reads: download/list take NO guard, and download is cacheable.
        assert!(
            m.contains(
                "async fn download(storage: Dep<Storage>, db: Dep<Db>, Path(id): Path<String>)"
            ),
            "{m}"
        );
        assert!(
            m.contains("async fn list(storage: Dep<Storage>, db: Dep<Db>)"),
            "{m}"
        );
        assert!(m.contains("object_response(&meta, bytes, true)"), "{m}");
        // No tenant machinery on a plain user bucket.
        assert!(!m.contains("Tenant"), "{m}");
    }

    /// invoices: private, owner: Org == tenancy.entity (tenant scope), prefix.
    #[test]
    fn tenant_owned_private_prefix_bucket_takes_the_tenant_guard() {
        let d = design();
        let m = bucket_rs(&d, bucket(&d, "invoices"));
        assert!(
            m.contains("public: false") && m.contains("owner_prefix: true"),
            "{m}"
        );
        assert!(m.contains("use shared::Tenant;"), "{m}");
        assert!(
            m.contains("async fn upload(storage: Dep<Storage>, db: Dep<Db>, tenant: Dep<Tenant>,"),
            "{m}"
        );
        assert!(
            m.contains("Scope { owner_id: Some(tenant.id().to_string()), tenant_id: Some(tenant.id().to_string()) }"),
            "{m}"
        );
        // Private download: optional guard OR a signed URL, both paths present.
        assert!(m.contains("tenant: Option<Dep<Tenant>>"), "{m}");
        assert!(m.contains("get_signed(&db, &BUCKET, &id, exp, sig,"), "{m}");
        assert!(m.contains("ok_or_else(Error::unauthorized)?"), "{m}");
        // Private list is scoped.
        assert!(
            m.contains("async fn list(storage: Dep<Storage>, db: Dep<Db>, tenant: Dep<Tenant>,"),
            "{m}"
        );
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
        d.modules[0].entities[1].belongs_to =
            vec![serde_json::from_str(r#"{ "entity": "Org" }"#).unwrap()];
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
        assert!(
            c.contains("name = \"storage\"") && c.contains("jerrycan.workspace = true"),
            "{c}"
        );
        assert!(c.contains("shared = { path = \"../shared\" }"), "{c}");
    }

    #[test]
    fn acceptance_covers_round_trip_guards_and_negative_controls() {
        let d = design();
        let a = acceptance_rs(&d);
        assert_eq!(a, acceptance_rs(&d), "deterministic");
        // Shared app() plumbing: memory blob store + storage migrations + auth
        // + the tenant guard (invoices is tenant-owned).
        assert!(
            a.contains("jerrycan::storage::Storage::memory().with_sign_secret(TEST_SECRET)"),
            "{a}"
        );
        assert!(
            a.contains("db.migrate(jerrycan::storage::STORAGE_MIGRATIONS)"),
            "{a}"
        );
        assert!(a.contains(".provide_dep(shared::tenant)"), "{a}");
        // Buckets mount + are requested under the /storage prefix (issue #8), so
        // the harness exercises the same path the generated app serves.
        assert!(
            a.contains(".mount(\"/storage/avatars\", storage::avatars::module())"),
            "{a}"
        );
        assert!(
            a.contains(".mount(\"/storage/invoices\", storage::invoices::module())"),
            "{a}"
        );
        assert!(
            a.contains("t.post_bytes_with(\"/storage/avatars?key=probe.bin\""),
            "requests go under the storage prefix: {a}"
        );
        // Tenant seeds: two tenants, two memberships (isolation acts as user 2).
        assert!(
            a.contains(
                "INSERT INTO \\\"org_members\\\" (user_id, org_id, role) VALUES (1, 1, 'owner')"
            ),
            "{a}"
        );
        assert!(
            a.contains(
                "INSERT INTO \\\"org_members\\\" (user_id, org_id, role) VALUES (2, 2, 'owner')"
            ),
            "{a}"
        );
        // Per-bucket surface tests.
        for needle in [
            "async fn avatars_upload_then_download_round_trips()",
            "async fn avatars_upload_without_auth_is_401()",
            "async fn avatars_disallowed_mime_is_415()",
            "async fn avatars_oversize_upload_is_413()",
            "async fn avatars_cross_owner_access_is_denied()",
            "async fn avatars_signed_url_grants_and_rejects()",
            "async fn invoices_download_without_auth_is_401()",
            "async fn invoices_cross_owner_access_is_denied()",
            "async fn invoices_owner_prefix_isolates_keys()",
            "async fn invoices_signed_url_grants_and_rejects()",
        ] {
            assert!(a.contains(needle), "missing {needle} in:\n{a}");
        }
        // The negative controls encode the SECURITY contract: cross-owner GET
        // 404s and the row survives a cross-owner DELETE.
        assert!(a.contains("cross-owner delete must 404"), "{a}");
        assert!(
            a.contains("\"2/same.bin\""),
            "prefix control asserts B's prefixed key: {a}"
        );
        // Public bucket: cache headers asserted; no download-401 test.
        assert!(a.contains("\"public, max-age=3600\""), "{a}");
        assert!(!a.contains("avatars_download_without_auth_is_401"), "{a}");
        // Private bucket: tampered signed URL is rejected.
        assert!(a.contains("invoices tampered signed URL"), "{a}");
    }

    /// A custom storage.base_path drives BOTH the acceptance mount and every
    /// request path, so the harness keeps exercising the real mounted path (#8).
    #[test]
    fn acceptance_honors_custom_storage_base_path() {
        let mut d = design();
        d.storage.as_mut().unwrap().base_path = Some("/files".into());
        let a = acceptance_rs(&d);
        assert!(
            a.contains(".mount(\"/files/avatars\", storage::avatars::module())"),
            "mount uses the custom base_path: {a}"
        );
        assert!(
            a.contains("t.post_bytes_with(\"/files/avatars?key=probe.bin\""),
            "requests use the custom base_path: {a}"
        );
        assert!(
            !a.contains("/storage/avatars"),
            "the default prefix is fully replaced: {a}"
        );
    }

    #[test]
    fn write_storage_writes_everything_tool_owned() {
        let tmp = tempfile::tempdir().unwrap();
        let d = design();
        let created = write_storage(tmp.path(), &d).unwrap();
        for rel in [
            "crates/storage/Cargo.toml",
            "crates/storage/src/lib.rs",
            "crates/storage/src/avatars.rs",
            "crates/storage/src/invoices.rs",
            "crates/storage/tests/acceptance.rs",
        ] {
            assert!(created.contains(&rel.to_string()), "{created:?}");
            assert!(tmp.path().join(rel).is_file(), "{rel} on disk");
        }
        // Ownership rule: EVERY file is tool-owned — a hand-edit is restored
        // (unlike route crates, no agent-owned files exist here).
        let handler = tmp.path().join("crates/storage/src/avatars.rs");
        fs::write(&handler, "// hand edit\n").unwrap();
        write_storage(tmp.path(), &d).unwrap();
        assert!(
            fs::read_to_string(&handler)
                .unwrap()
                .contains("const BUCKET"),
            "tool-owned bucket module restored"
        );
    }
}
