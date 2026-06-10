//! Reference fixture: in-memory posts handlers.
use super::model::*;
use super::repo::*;
use jerrycan::prelude::*;

pub(crate) async fn list_posts(repo: Dep<PostRepo>) -> Result<Json<Vec<Post>>> {
    Ok(Json(repo.all()))
}

pub(crate) async fn create_post(repo: Dep<PostRepo>, Json(body): Json<Post>) -> Result<Created<Post>> {
    repo.insert(body.clone());
    Ok(Created(body))
}

pub(crate) async fn show_post(repo: Dep<PostRepo>, Path(id): Path<i64>) -> Result<Json<Post>> {
    repo.get(id).map(Json).ok_or_else(Error::not_found)
}

pub(crate) async fn update_post(
    repo: Dep<PostRepo>,
    Path(id): Path<i64>,
    Json(body): Json<Post>,
) -> Result<Json<Post>> {
    if repo.update(id, body.clone()) {
        Ok(Json(body))
    } else {
        Err(Error::not_found())
    }
}

pub(crate) async fn delete_post(repo: Dep<PostRepo>, Path(id): Path<i64>) -> Result<NoContent> {
    if repo.remove(id) {
        Ok(NoContent)
    } else {
        Err(Error::not_found())
    }
}
