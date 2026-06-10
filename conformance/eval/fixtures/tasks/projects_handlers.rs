//! Reference fixture: in-memory projects handlers.
use super::model::*;
use super::repo::*;
use jerrycan::prelude::*;

pub(crate) async fn list_projects(repo: Dep<ProjectRepo>) -> Result<Json<Vec<Project>>> {
    Ok(Json(repo.all()))
}

pub(crate) async fn create_project(
    repo: Dep<ProjectRepo>,
    Json(body): Json<Project>,
) -> Result<Created<Project>> {
    repo.insert(body.clone());
    Ok(Created(body))
}

pub(crate) async fn delete_project(repo: Dep<ProjectRepo>, Path(id): Path<i64>) -> Result<NoContent> {
    if repo.remove(id) {
        Ok(NoContent)
    } else {
        Err(Error::not_found())
    }
}
