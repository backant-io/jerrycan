//! Handlers for `api-keys` — mint/store-hash/list/revoke + a scope-gated
//! `usage` endpoint. Keys are minted server-side: only the hash + prefix +
//! scopes are persisted; the plaintext is returned to the caller exactly once.
use super::deps::SharedKeyStore;
use super::model::*;
use super::repo::*;
use jerrycan::auth::{ApiKeyRecord, ApiKeys, hash_key, mint};
use jerrycan::prelude::*;
use shared::{CurrentUser, Tenant};

/// The scopes a tenant may grant to a minted key — a server-owned allowlist.
/// A key's scope set is NEVER taken verbatim from the request: the requested
/// scopes are intersected with this list, so an unknown scope or the `"*"`
/// wildcard (which would satisfy every `require_scope` check) can never be
/// granted by a client. This is the difference between delegating access a
/// tenant already has and escalating to access it does not.
const GRANTABLE_SCOPES: &[&str] = &["leads:read", "leads:write", "billing:read"];

/// GET / — list the caller's tenant's keys (scoped via `all_for`).
pub(crate) async fn list_api_keys(
    repo: Dep<ApiKeyRepo>,
    tenant: Dep<Tenant>,
) -> Result<Json<Vec<ApiKey>>> {
    Ok(Json(repo.all_for(tenant.id()).await?))
}

/// POST / — mint a new key for the workspace named in the body (verified against
/// the caller's memberships). Persist the hash + prefix + scopes (never the
/// plaintext) and ALSO register the record in the in-memory `ApiKeys` store the
/// `usage` endpoint authenticates against. The plaintext is returned ONCE, in a
/// `plaintext` field alongside the echoed row.
pub(crate) async fn create_api_key(
    repo: Dep<ApiKeyRepo>,
    store: Dep<SharedKeyStore>,
    user: CurrentUser,
    Json(body): Json<ApiKey>,
) -> Result<Created<serde_json::Value>> {
    let minted = mint("sk_live");
    // Intersect the REQUESTED scopes with the server allowlist: drop anything
    // unknown (incl. the unsafe `"*"` wildcard) so a client can never grant a
    // key more than the app permits. The stored, registered, and echoed scopes
    // are all the GRANTED (filtered) set — never the raw request.
    let granted: Vec<String> = body
        .scopes
        .split(',')
        .map(|s| s.trim())
        .filter(|s| GRANTABLE_SCOPES.contains(s))
        .map(str::to_string)
        .collect();
    let granted_csv = granted.join(",");
    // The `prefix` column is UNIQUE, so store a per-key DISPLAY prefix
    // (`sk_live_<first 8 hex of the hash>`) — non-secret, identifies the key in a
    // list, and is unique because the hash is. `mint`'s class prefix (`sk_live`)
    // alone would collide across a tenant's keys.
    let display_prefix = format!("{}_{}", minted.prefix, &minted.hash[..8]);
    let workspace_id = body.workspace_id;
    let row = ApiKey {
        id: body.id,
        workspace_id,
        prefix: display_prefix.clone(),
        label: body.label,
        scopes: granted_csv.clone(),
    };
    // Membership-CHECKED create (#94/#97): `create_for_memberships` verifies the
    // body's `workspace_id` is in the caller's membership set before inserting — a
    // create into a non-member workspace is 403, and the bare `insert` that would skip
    // the check is not generated for a flat tenant entity.
    let id = repo.create_for_memberships(user.0.id, row.clone()).await?;
    // Register the lookup record (hash → scopes) in the shared store the `usage`
    // authenticator reads. We persist only the hash, never the plaintext.
    store.0.insert(ApiKeyRecord {
        id,
        prefix: display_prefix.clone(),
        hash: minted.hash.clone(),
        scopes: granted.clone(),
    });
    Ok(Created(serde_json::json!({
        "id": id,
        "workspace_id": workspace_id,
        "prefix": display_prefix,
        "label": row.label,
        "scopes": granted_csv,
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

/// DELETE /{id} — owner-only revoke (issue #247). The required "owner" role is the
/// caller's MEMBERSHIP role in the ROW's tenant, NOT the session role
/// (`user.0.role`, a different dimension): `require_membership_role` resolves the
/// key's workspace, verifies the caller is an `owner` MEMBER of THAT workspace, and
/// 403s a non-member / wrong-role caller — so a cross-tenant caller (a member of a
/// different workspace) is 403, never a 404 that would hide the role gate. The
/// membership-scoped `remove_for_memberships` then deletes only a key in the
/// caller's set; an unknown id in the caller's own tenant → 404.
pub(crate) async fn revoke_api_key(
    repo: Dep<ApiKeyRepo>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<NoContent> {
    repo.require_membership_role(user.0.id.clone(), id, &["owner"])
        .await?;
    if repo.remove_for_memberships(user.0.id, id).await? {
        Ok(NoContent)
    } else {
        Err(Error::not_found())
    }
}
