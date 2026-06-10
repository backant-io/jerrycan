//! Reference fixture: in-memory authors handlers.
use super::model::*;
use super::repo::*;
use jerrycan::prelude::*;

pub(crate) async fn list_authors(repo: Dep<AuthorRepo>) -> Result<Json<Vec<Author>>> {
    Ok(Json(repo.all()))
}

pub(crate) async fn create_author(
    repo: Dep<AuthorRepo>,
    Json(body): Json<Author>,
) -> Result<Created<Author>> {
    repo.insert(body.clone());
    Ok(Created(body))
}

pub(crate) async fn show_author(repo: Dep<AuthorRepo>, Path(id): Path<i64>) -> Result<Json<Author>> {
    repo.get(id).map(Json).ok_or_else(Error::not_found)
}
