//! Handlers for `leads` — tenant-SCOPED CRUD + multipart CSV import.
//!
//! Reads and the path-scoped writes are tenant-guarded via `Dep<Tenant>` and the
//! scoped accessors (`all_for`/`get_for`/`update_for`/`remove_for`); the FLAT create
//! takes `CurrentUser` and calls the membership-CHECKED `create_for_memberships`,
//! whose RLS `WITH CHECK` verifies the body's `workspace_id` is in the caller's
//! membership set before inserting (#94/#97). A caller can never see or mutate
//! another tenant's leads (JL0006).
use super::model::*;
use super::repo::*;
use jerrycan::prelude::*;
use shared::{CurrentUser, Tenant};

/// Coerce a requested status onto the design's enum (`new`|`called`|`dnc`);
/// anything else defaults to `new` so a free-text value can't violate the DB
/// CHECK constraint.
fn normalize_status(requested: &str) -> String {
    match requested {
        "called" => "called".to_string(),
        "dnc" => "dnc".to_string(),
        _ => "new".to_string(),
    }
}

/// GET / — list the caller's tenant's leads (scoped via `all_for`).
pub(crate) async fn list_leads(
    repo: Dep<LeadRepo>,
    tenant: Dep<Tenant>,
) -> Result<Json<Vec<Lead>>> {
    Ok(Json(repo.all_for(tenant.id()).await?))
}

/// POST / — create a lead. The `workspace_id` comes from the request BODY and is
/// verified against the caller's membership set by `create_for_memberships` (the
/// flat-tenant RLS `WITH CHECK`, #94): a create into a workspace the caller does
/// NOT belong to is 403, and the bare `insert` that would skip that check is not
/// generated for a flat tenant entity (#97). A duplicate phone surfaces as 409 (the
/// unique index → `db_error` → JC0409).
pub(crate) async fn create_lead(
    repo: Dep<LeadRepo>,
    user: CurrentUser,
    Json(body): Json<Lead>,
) -> Result<Created<Lead>> {
    let status = normalize_status(&body.status);
    let workspace_id = body.workspace_id;
    let to_store = Lead {
        id: body.id,
        workspace_id,
        phone: body.phone.clone(),
        name: body.name.clone(),
        status: status.clone(),
        custom: body.custom.clone(),
    };
    // Membership-CHECKED create (#94/#97): `create_for_memberships` verifies the
    // body's `workspace_id` is in the caller's membership set before inserting — a
    // create aimed at a non-member workspace is 403, never a silent cross-tenant write.
    let id = repo.create_for_memberships(user.0.id, to_store).await?;
    Ok(Created(Lead {
        id,
        workspace_id,
        phone: body.phone,
        name: body.name,
        status,
        custom: body.custom,
    }))
}

/// GET /{id} — scoped read; cross-tenant or unknown id → 404.
pub(crate) async fn show_lead(
    repo: Dep<LeadRepo>,
    tenant: Dep<Tenant>,
    Path(id): Path<i64>,
) -> Result<Json<Lead>> {
    repo.get_for(tenant.id(), id)
        .await?
        .map(Json)
        .ok_or_else(Error::not_found)
}

/// PUT /{id} — update a lead in the caller's tenant; cross-tenant or unknown → 404.
/// Uses the tenant-scoped `update_for` so a foreign tenant can never write.
pub(crate) async fn update_lead(
    repo: Dep<LeadRepo>,
    tenant: Dep<Tenant>,
    Path(id): Path<i64>,
    Json(body): Json<Lead>,
) -> Result<Json<Lead>> {
    let updated = Lead {
        id,
        workspace_id: tenant.id(),
        phone: body.phone,
        name: body.name,
        status: normalize_status(&body.status),
        custom: body.custom,
    };
    if repo.update_for(tenant.id(), id, updated.clone()).await? {
        Ok(Json(updated))
    } else {
        Err(Error::not_found())
    }
}

/// DELETE /{id} — owner-only, tenant-scoped. Non-owner → 403; cross-tenant or
/// unknown id → 404. Uses `remove_for` so a foreign tenant can never delete.
pub(crate) async fn delete_lead(
    repo: Dep<LeadRepo>,
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

/// POST /import — parse a `multipart/form-data` CSV (field `file`), insert each
/// row into the caller's tenant, return 202 with the inserted count. CSV shape:
/// `phone,name,status` per line (a leading `phone,...` header row is skipped).
///
/// We take `Headers` + `RawBody` and build the real `Multipart` parser via
/// `Multipart::from_buffered` only when the request is multipart — a
/// non-multipart body (e.g. the generated success probe's `{}`) imports zero
/// rows and still returns 202, instead of the extractor's 415.
pub(crate) async fn import_leads(
    db: Dep<jerrycan::db::Db>,
    tenant: Dep<Tenant>,
    headers: Headers,
    body: RawBody,
) -> Result<Accepted> {
    use jerrycan::db::sea_orm::{ConnectionTrait, Statement};

    let mut csv = String::new();
    let content_type = headers.get("content-type").unwrap_or("");
    if let Some(mut form) = Multipart::from_buffered(content_type, body.0) {
        while let Some(part) = form.next_part().await? {
            if part.name() == "file" {
                csv = part.text().await?;
                break;
            }
        }
    }
    let mut inserted = 0usize;
    for line in csv.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut cols = line.split(',');
        let phone = cols.next().unwrap_or("").trim();
        // Skip an optional header row.
        if phone.eq_ignore_ascii_case("phone") {
            continue;
        }
        if phone.is_empty() {
            continue;
        }
        let name = cols.next().unwrap_or("").trim().to_string();
        let status = normalize_status(cols.next().unwrap_or("new").trim());
        // Insert OMITTING the id so the DB assigns it (the generated `insert`
        // always `Set`s the id, which a bulk import has no value for). The
        // workspace_id is the authenticated tenant's — never client-supplied.
        db.conn()
            .execute(Statement::from_sql_and_values(
                db.conn().get_database_backend(),
                db.sql(
                    "INSERT INTO leads (workspace_id, phone, name, status) VALUES (?, ?, ?, ?)",
                ),
                [
                    tenant.id().into(),
                    phone.into(),
                    name.into(),
                    status.into(),
                ],
            ))
            .await
            .map_err(jerrycan::db::db_error)?;
        inserted += 1;
    }
    Accepted::json(serde_json::json!({ "imported": inserted }))
}

/// A 202 Accepted response carrying a JSON body. The framework ships `Created`
/// (201) and `NoContent` (204) but no `Accepted`, so we build the response.
pub(crate) struct Accepted(jerrycan::Response);

impl Accepted {
    fn json(value: serde_json::Value) -> Result<Self> {
        use jerrycan::http::{HeaderValue, StatusCode, header};
        let body = serde_json::to_vec(&value).map_err(|e| Error::internal(e.to_string()))?;
        let mut res = jerrycan::http::Response::new(jerrycan::JcBody::full(body));
        *res.status_mut() = StatusCode::ACCEPTED;
        res.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        Ok(Self(res))
    }
}

impl IntoResponse for Accepted {
    fn into_response(self) -> jerrycan::Response {
        self.0
    }
}
