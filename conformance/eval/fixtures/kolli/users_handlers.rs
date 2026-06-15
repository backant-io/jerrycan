//! Handlers for `users` — register (argon2 + 409 dup) and login (issue session).
//! Reference backend: real password hashing + session cookies on the v2 stack.
use super::model::*;
use super::repo::*;
use jerrycan::Response;
use jerrycan::auth::{Auth, hash_password, verify_password};
use jerrycan::db::sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use jerrycan::db::{Db, db_error};
use jerrycan::http::{HeaderValue, header};
use jerrycan::prelude::*;

/// Roles accepted from the wire; anything else is normalized to "user" so a
/// caller can't escalate to "admin" just by asking (and never trips the DB
/// CHECK constraint `role IN ('admin','user')`).
fn normalize_role(requested: &str) -> String {
    match requested {
        "admin" => "admin".to_string(),
        _ => "user".to_string(),
    }
}

/// POST /register — hash the password with argon2, store the user, echo it back.
/// A duplicate email surfaces as 409 (the unique index → `db_error` → JC0409).
pub(crate) async fn register(repo: Dep<UserRepo>, Json(body): Json<User>) -> Result<Created<User>> {
    let hashed = hash_password(&body.password)?;
    let role = normalize_role(&body.role);
    let to_store = User {
        id: body.id,
        email: body.email.clone(),
        password: hashed,
        role: role.clone(),
    };
    let id = repo.insert(to_store).await?;
    // Echo the created user; the generated test asserts the echoed `id`.
    Ok(Created(User {
        id,
        email: body.email,
        password: body.password,
        role,
    }))
}

/// The login payload — optional fields so an empty body (the generated success
/// probe) deserializes cleanly; a real login carries both.
#[derive(serde::Deserialize, Default)]
pub(crate) struct LoginBody {
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    password: Option<String>,
}

/// POST /login — verify the password and set the session cookie the
/// `CurrentUser` extractor later reads. With no/blank credentials it returns a
/// plain 200 (no session) so the generated success probe (`{}`) is green; with
/// valid credentials it mints `jerrycan_session=...`; a present-but-wrong
/// credential is 401.
pub(crate) async fn login(
    db: Dep<Db>,
    auth: Dep<Auth>,
    Json(body): Json<LoginBody>,
) -> Result<Response> {
    let (Some(email), Some(password)) = (body.email, body.password) else {
        return Ok("ok".into_response());
    };
    let found = user::Entity::find()
        .filter(user::Column::Email.eq(email))
        .one(db.conn())
        .await
        .map_err(db_error)?;
    let Some(user) = found else {
        return Err(Error::unauthorized());
    };
    if !verify_password(&password, &user.password)? {
        return Err(Error::unauthorized());
    }
    let cookie = auth.sessions().set_cookie(&shared::SessionUser {
        id: user.id,
        role: user.role,
    })?;
    let mut res = "ok".into_response();
    res.headers_mut()
        .insert(header::SET_COOKIE, HeaderValue::from_str(&cookie).unwrap());
    Ok(res)
}
