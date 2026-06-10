//! Reference fixture: in-memory items handlers.
use super::model::*;
use super::repo::*;
use jerrycan::prelude::*;

pub(crate) async fn list_items(repo: Dep<ItemRepo>) -> Result<Json<Vec<Item>>> {
    Ok(Json(repo.all()))
}

pub(crate) async fn create_item(repo: Dep<ItemRepo>, Json(body): Json<Item>) -> Result<Created<Item>> {
    repo.insert(body.clone());
    Ok(Created(body))
}

pub(crate) async fn show_item(repo: Dep<ItemRepo>, Path(id): Path<i64>) -> Result<Json<Item>> {
    repo.get(id).map(Json).ok_or_else(Error::not_found)
}

pub(crate) async fn update_item(
    repo: Dep<ItemRepo>,
    Path(id): Path<i64>,
    Json(body): Json<Item>,
) -> Result<Json<Item>> {
    if repo.get(id).is_none() {
        return Err(Error::not_found());
    }
    Ok(Json(body))
}

pub(crate) async fn delete_item(repo: Dep<ItemRepo>, Path(id): Path<i64>) -> Result<NoContent> {
    if repo.remove(id) {
        Ok(NoContent)
    } else {
        Err(Error::not_found())
    }
}
