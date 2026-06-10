//! Reference fixture: in-memory links handlers (URL shortener).
use super::model::*;
use super::repo::*;
use jerrycan::prelude::*;

pub(crate) async fn list_links(repo: Dep<LinkRepo>) -> Result<Json<Vec<Link>>> {
    Ok(Json(repo.all()))
}

pub(crate) async fn create_link(repo: Dep<LinkRepo>, Json(body): Json<Link>) -> Result<Created<Link>> {
    repo.insert(body.clone());
    Ok(Created(body))
}

pub(crate) async fn resolve_link(repo: Dep<LinkRepo>, Path(id): Path<i64>) -> Result<Json<Link>> {
    repo.get(id).map(Json).ok_or_else(Error::not_found)
}

pub(crate) async fn delete_link(repo: Dep<LinkRepo>, Path(id): Path<i64>) -> Result<NoContent> {
    if repo.remove(id) {
        Ok(NoContent)
    } else {
        Err(Error::not_found())
    }
}
