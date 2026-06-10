//! Conformance fixture: the agent's implementation of the comments handlers.
use super::model::*;
use super::repo::*;
use jerrycan::prelude::*;

pub(crate) async fn list_comments(repo: Dep<CommentRepo>) -> Result<Json<Vec<Comment>>> {
    Ok(Json(repo.all()))
}

pub(crate) async fn create_comment(repo: Dep<CommentRepo>, Json(body): Json<Comment>) -> Result<Created<Comment>> {
    repo.insert(body.clone());
    Ok(Created(body))
}
