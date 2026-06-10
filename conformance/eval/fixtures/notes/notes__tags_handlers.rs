//! Reference fixture: in-memory tags handlers (notes subroute).
use super::model::*;
use super::repo::*;
use jerrycan::prelude::*;

pub(crate) async fn list_tags(repo: Dep<TagRepo>) -> Result<Json<Vec<Tag>>> {
    Ok(Json(repo.all()))
}

pub(crate) async fn create_tag(repo: Dep<TagRepo>, Json(body): Json<Tag>) -> Result<Created<Tag>> {
    repo.insert(body.clone());
    Ok(Created(body))
}

pub(crate) async fn delete_tag(repo: Dep<TagRepo>, Path(id): Path<i64>) -> Result<NoContent> {
    if repo.remove(id) {
        Ok(NoContent)
    } else {
        Err(Error::not_found())
    }
}
