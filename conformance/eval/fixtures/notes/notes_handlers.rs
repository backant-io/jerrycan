//! Reference fixture: in-memory notes handlers.
use super::model::*;
use super::repo::*;
use jerrycan::prelude::*;

pub(crate) async fn list_notes(repo: Dep<NoteRepo>) -> Result<Json<Vec<Note>>> {
    Ok(Json(repo.all()))
}

pub(crate) async fn create_note(repo: Dep<NoteRepo>, Json(body): Json<Note>) -> Result<Created<Note>> {
    repo.insert(body.clone());
    Ok(Created(body))
}

pub(crate) async fn show_note(repo: Dep<NoteRepo>, Path(id): Path<i64>) -> Result<Json<Note>> {
    repo.get(id).map(Json).ok_or_else(Error::not_found)
}

pub(crate) async fn update_note(
    repo: Dep<NoteRepo>,
    Path(id): Path<i64>,
    Json(body): Json<Note>,
) -> Result<Json<Note>> {
    if repo.update(id, body.clone()) {
        Ok(Json(body))
    } else {
        Err(Error::not_found())
    }
}

pub(crate) async fn delete_note(repo: Dep<NoteRepo>, Path(id): Path<i64>) -> Result<NoContent> {
    if repo.remove(id) {
        Ok(NoContent)
    } else {
        Err(Error::not_found())
    }
}
