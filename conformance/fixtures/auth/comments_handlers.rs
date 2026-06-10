//! Conformance fixture (auth mode): guarded comments handlers.
use super::model::*;
use super::repo::*;
use jerrycan::prelude::*;
use shared::CurrentUser;

pub(crate) async fn list_comments(repo: Dep<CommentRepo>) -> Result<Json<Vec<Comment>>> {
    Ok(Json(repo.all()))
}

pub(crate) async fn create_comment(
    repo: Dep<CommentRepo>,
    _user: CurrentUser,
    Json(body): Json<Comment>,
) -> Result<Created<Comment>> {
    repo.insert(body.clone());
    Ok(Created(body))
}
