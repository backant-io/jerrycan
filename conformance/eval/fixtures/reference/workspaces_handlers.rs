//! Handlers for `workspaces` — list/create/show. Creating a workspace seeds the
//! caller as an `owner` member so subsequent tenant-scoped calls resolve a
//! Tenant for them.
use super::model::*;
use super::repo::*;
use jerrycan::db::sea_orm::{ConnectionTrait, Statement};
use jerrycan::db::{Db, db_error};
use jerrycan::prelude::*;
use shared::CurrentUser;

/// A new workspace ALWAYS starts on the free tier — the requested `plan` is
/// deliberately ignored. Letting a caller create a workspace directly as `pro`
/// would be a billing bypass (self-upgrade without paying); upgrades to `pro`
/// are billing-driven (the Stripe webhook), and `expire_trials` walks them back.
/// `trial` is also a valid value for the DB CHECK constraint (`trial`|`pro`).
const NEW_WORKSPACE_PLAN: &str = "trial";

/// GET / — list all workspaces (public discovery, per the design).
pub(crate) async fn list_workspaces(repo: Dep<WorkspaceRepo>) -> Result<Json<Vec<Workspace>>> {
    Ok(Json(repo.all().await?))
}

/// POST / — create a workspace and seed the caller as its `owner` member.
/// The membership row is what `shared::tenant` reads to authorize tenant calls.
pub(crate) async fn create_workspace(
    repo: Dep<WorkspaceRepo>,
    db: Dep<Db>,
    user: CurrentUser,
    Json(body): Json<Workspace>,
) -> Result<Created<Workspace>> {
    // Ignore any client-supplied `plan`: new workspaces can't self-grant `pro`.
    let plan = NEW_WORKSPACE_PLAN.to_string();
    let id = repo
        .insert(Workspace {
            id: body.id,
            name: body.name.clone(),
            plan: plan.clone(),
        })
        .await?;
    // Seed ownership: the membership row is what `shared::tenant` resolves to
    // authorize this caller's tenant-scoped requests.
    db.conn()
        .execute(Statement::from_sql_and_values(
            db.conn().get_database_backend(),
            db.sql(
                "INSERT INTO workspace_members (user_id, workspace_id, role) VALUES (?, ?, 'owner')",
            ),
            [user.0.id.into(), id.into()],
        ))
        .await
        .map_err(db_error)?;
    Ok(Created(Workspace {
        id,
        name: body.name,
        plan,
    }))
}

/// GET /{id} — show one workspace; unknown id → 404.
pub(crate) async fn show_workspace(
    repo: Dep<WorkspaceRepo>,
    Path(id): Path<i64>,
) -> Result<Json<Workspace>> {
    repo.get(id).await?.map(Json).ok_or_else(Error::not_found)
}
