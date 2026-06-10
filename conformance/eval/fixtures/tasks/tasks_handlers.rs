//! Reference fixture: in-memory tasks handlers.
use super::model::*;
use super::repo::*;
use jerrycan::prelude::*;

pub(crate) async fn list_tasks(repo: Dep<TaskRepo>) -> Result<Json<Vec<Task>>> {
    Ok(Json(repo.all()))
}

pub(crate) async fn create_task(repo: Dep<TaskRepo>, Json(body): Json<Task>) -> Result<Created<Task>> {
    repo.insert(body.clone());
    Ok(Created(body))
}

pub(crate) async fn show_task(repo: Dep<TaskRepo>, Path(id): Path<i64>) -> Result<Json<Task>> {
    repo.get(id).map(Json).ok_or_else(Error::not_found)
}

pub(crate) async fn update_task(
    repo: Dep<TaskRepo>,
    Path(id): Path<i64>,
    Json(body): Json<Task>,
) -> Result<Json<Task>> {
    if repo.update(id, body.clone()) {
        Ok(Json(body))
    } else {
        Err(Error::not_found())
    }
}

pub(crate) async fn delete_task(repo: Dep<TaskRepo>, Path(id): Path<i64>) -> Result<NoContent> {
    if repo.remove(id) {
        Ok(NoContent)
    } else {
        Err(Error::not_found())
    }
}
