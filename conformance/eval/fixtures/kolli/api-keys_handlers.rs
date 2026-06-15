//! Handlers for `api-keys` — mint/store-hash/list/revoke + a scope-gated
//! `usage` endpoint. Keys are minted server-side: only the hash + prefix +
//! scopes are persisted; the plaintext is returned to the caller exactly once.
use super::deps::SharedKeyStore;
use super::model::*;
use super::repo::*;
use jerrycan::auth::{ApiKeyRecord, ApiKeys, hash_key, mint};
use jerrycan::prelude::*;
use shared::Tenant;

/// GET / — list the caller's tenant's keys (scoped via `all_for`).
pub(crate) async fn list_api_keys(
    repo: Dep<ApiKeyRepo>,
    tenant: Dep<Tenant>,
) -> Result<Json<Vec<ApiKey>>> {
    Ok(Json(repo.all_for(tenant.id()).await?))
}

/// POST / — mint a new key for the caller's tenant. Persist the hash + prefix +
/// scopes (never the plaintext) and ALSO register the record in the in-memory
/// `ApiKeys` store the `usage` endpoint authenticates against. The plaintext is
/// returned ONCE, in a `plaintext` field alongside the echoed row.
pub(crate) async fn create_api_key(
    repo: Dep<ApiKeyRepo>,
    store: Dep<SharedKeyStore>,
    tenant: Dep<Tenant>,
    Json(body): Json<ApiKey>,
) -> Result<Created<serde_json::Value>> {
    let minted = mint("sk_live");
    let scopes = body.scopes.clone();
    // The `prefix` column is UNIQUE, so store a per-key DISPLAY prefix
    // (`sk_live_<first 8 hex of the hash>`) — non-secret, identifies the key in a
    // list, and is unique because the hash is. `mint`'s class prefix (`sk_live`)
    // alone would collide across a tenant's keys.
    let display_prefix = format!("{}_{}", minted.prefix, &minted.hash[..8]);
    let row = ApiKey {
        id: body.id,
        workspace_id: tenant.id(),
        prefix: display_prefix.clone(),
        label: body.label,
        scopes: scopes.clone(),
    };
    let id = repo.insert(row.clone()).await?;
    // Register the lookup record (hash → scopes) in the shared store the `usage`
    // authenticator reads. We persist only the hash, never the plaintext.
    let scope_list: Vec<String> = scopes
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    store.0.insert(ApiKeyRecord {
        id,
        prefix: display_prefix.clone(),
        hash: minted.hash.clone(),
        scopes: scope_list,
    });
    Ok(Created(serde_json::json!({
        "id": id,
        "workspace_id": tenant.id(),
        "prefix": display_prefix,
        "label": row.label,
        "scopes": scopes,
        "plaintext": minted.plaintext,
    })))
}

/// GET /usage — scope-gated read. With NO credential it returns a public 200
/// (the generated success probe). With a key it authenticates against the
/// `ApiKeys` store: unknown key → 401, known key lacking `leads:read` → 403,
/// known key with the scope → 200 + a usage summary.
pub(crate) async fn usage(
    keys: Dep<ApiKeys>,
    headers: Headers,
) -> Result<Json<serde_json::Value>> {
    let Some(presented) = present_key(&headers) else {
        return Ok(Json(serde_json::json!({ "authenticated": false })));
    };
    let hash = hash_key(&presented);
    let record = keys
        .0
        .lookup(&hash)
        .await?
        .ok_or_else(Error::unauthorized)?;
    record.require_scope("leads:read")?;
    Ok(Json(serde_json::json!({
        "authenticated": true,
        "prefix": record.prefix,
        "scopes": record.scopes,
        "calls": 0,
    })))
}

/// Read the presented key: `Authorization: Bearer <key>` (preferred) or
/// `X-API-Key: <key>`. Mirrors the `ApiKey` extractor's header precedence.
fn present_key(headers: &Headers) -> Option<String> {
    if let Some(auth) = headers.get("authorization")
        && let Some((scheme, token)) = auth.split_once(' ')
        && scheme.eq_ignore_ascii_case("bearer")
        && !token.is_empty()
    {
        return Some(token.to_string());
    }
    headers
        .get("x-api-key")
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

/// DELETE /{id} — owner-only, tenant-scoped revoke. Non-owner → 403;
/// cross-tenant or unknown id → 404.
pub(crate) async fn revoke_api_key(
    repo: Dep<ApiKeyRepo>,
    tenant: Dep<Tenant>,
    Path(id): Path<i64>,
) -> Result<NoContent> {
    tenant.require_role("owner")?;
    if repo.remove_for(tenant.id(), id).await? {
        Ok(NoContent)
    } else {
        Err(Error::not_found())
    }
}
