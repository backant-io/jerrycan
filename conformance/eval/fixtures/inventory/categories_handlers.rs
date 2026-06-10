//! Reference fixture: in-memory categories handlers.
use super::model::*;
use super::repo::*;
use jerrycan::prelude::*;

pub(crate) async fn list_categories(repo: Dep<CategoryRepo>) -> Result<Json<Vec<Category>>> {
    Ok(Json(repo.all()))
}

pub(crate) async fn create_category(
    repo: Dep<CategoryRepo>,
    Json(body): Json<Category>,
) -> Result<Created<Category>> {
    repo.insert(body.clone());
    Ok(Created(body))
}
