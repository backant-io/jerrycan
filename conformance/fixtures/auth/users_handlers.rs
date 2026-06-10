//! Conformance fixture (auth mode): guarded users handlers.
use super::model::*;
use super::repo::*;
use jerrycan::prelude::*;
use shared::CurrentUser;

pub(crate) async fn list_users(repo: Dep<UserRepo>) -> Result<Json<Vec<User>>> {
    Ok(Json(repo.all()))
}

pub(crate) async fn create_user(
    repo: Dep<UserRepo>,
    _user: CurrentUser,
    Json(body): Json<User>,
) -> Result<Created<User>> {
    repo.insert(body.clone());
    Ok(Created(body))
}
