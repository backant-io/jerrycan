//! Conformance fixture (db mode): the agent's users handlers.
use super::model::*;
use super::repo::*;
use jerrycan::prelude::*;

pub(crate) async fn list_users(repo: Dep<UserRepo>) -> Result<Json<Vec<User>>> {
    Ok(Json(repo.all().await?))
}

pub(crate) async fn create_user(repo: Dep<UserRepo>, Json(body): Json<User>) -> Result<Created<User>> {
    repo.insert(body.clone()).await?;
    Ok(Created(body))
}
