//! Conformance fixture (auth mode): guarded todos handlers.
use super::model::*;
use super::repo::*;
use jerrycan::auth::require_role;
use jerrycan::prelude::*;
use shared::CurrentUser;

pub(crate) async fn list_todos(repo: Dep<TodoRepo>) -> Result<Json<Vec<Todo>>> {
    Ok(Json(repo.all()))
}

pub(crate) async fn create_todo(
    repo: Dep<TodoRepo>,
    _user: CurrentUser,
    Json(body): Json<Todo>,
) -> Result<Created<Todo>> {
    repo.insert(body.clone());
    Ok(Created(body))
}

pub(crate) async fn show_todo(repo: Dep<TodoRepo>, Path(id): Path<i64>) -> Result<Json<Todo>> {
    repo.get(id).map(Json).ok_or_else(Error::not_found)
}

pub(crate) async fn delete_todo(
    repo: Dep<TodoRepo>,
    _user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<NoContent> {
    require_role(&_user.0.role, "admin")?;
    if repo.remove(id) {
        Ok(NoContent)
    } else {
        Err(Error::not_found())
    }
}
