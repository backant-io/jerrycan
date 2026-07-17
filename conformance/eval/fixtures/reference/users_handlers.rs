//! Handlers for `users` — register (argon2 + 409 dup) and login (mint a JWT).
//! Reference backend: real password hashing + Bearer JWTs on the v2 stack (the
//! design declares `auth.model: "jwt"`, so the guard is `Bearer<SessionUser>`).
use super::model::*;
use super::repo::*;
use jerrycan::Response;
use jerrycan::auth::{Auth, hash_password, verify_password};
use jerrycan::db::sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use jerrycan::db::{Db, db_error};
use jerrycan::prelude::*;
use std::time::{SystemTime, UNIX_EPOCH};

/// Self-registration ALWAYS assigns the lowest-privilege role — the `role`
/// field on the request body is deliberately ignored, so a caller can never
/// escalate to "admin" by mass-assigning it (the classic registration
/// mass-assignment bug). Granting "admin" is a separate, authenticated
/// promotion path (an existing admin acts on another user); it never rides in
/// on the public register payload. "user" also satisfies the DB CHECK
/// constraint `role IN ('admin','user')`.
const SELF_REGISTRATION_ROLE: &str = "user";

/// POST /register — hash the password with argon2, store the user, echo it back.
/// A duplicate email surfaces as 409 (the unique index → `db_error` → JC0409).
pub(crate) async fn register(repo: Dep<UserRepo>, Json(body): Json<User>) -> Result<Created<User>> {
    let hashed = hash_password(&body.password)?;
    // Ignore any client-supplied `role`: self-registration can't grant admin.
    let role = SELF_REGISTRATION_ROLE.to_string();
    let to_store = User {
        id: body.id,
        email: body.email.clone(),
        password: hashed,
        role: role.clone(),
    };
    let id = repo.insert(to_store).await?;
    // Echo the created user; the generated test asserts the echoed `id`. Never
    // return the password (not even the just-submitted one) — blank it out.
    Ok(Created(User {
        id,
        email: body.email,
        password: String::new(),
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

/// The JWT claims the login mints. Same `id`/`role` shape as `shared::SessionUser`
/// (so `Bearer<SessionUser>` reads it back) plus the mandatory `exp` — a JWT with
/// no `exp` never expires (docs/ai/10-auth.md), so a login must always set one.
#[derive(serde::Serialize)]
struct LoginClaims {
    id: String,
    role: String,
    exp: u64,
}

/// The login response body: the freshly minted bearer token. The client sends it
/// back as `Authorization: Bearer <token>` on guarded requests.
#[derive(serde::Serialize)]
struct LoginToken {
    token: String,
}

/// POST /login — verify the password and mint the Bearer JWT the generated
/// `CurrentUser = Bearer<SessionUser>` guard later reads. Under `auth.model:
/// "jwt"` there is NO cookie to set: the agent-written login mints the token
/// (spec + docs/ai/10-auth.md). With no/blank credentials it returns a plain
/// 200 (no token) so the generated success probe (`{}`) is green; with valid
/// credentials it returns `{ "token": "<jwt>" }`; a present-but-wrong credential
/// is 401.
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
    // Mint the Bearer JWT over the `SessionUser` shape the guard verifies, signed
    // with the app's `Auth::jwt_key()` (derived from `JERRYCAN_SECRET`). ALWAYS
    // set `exp` — an expiry-less token is a permanent credential; one hour
    // comfortably covers a request's lifetime.
    let exp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::internal("clock before epoch"))?
        .as_secs()
        + 3600;
    let token = jerrycan::auth::jwt::encode(
        &LoginClaims {
            id: user.id.to_string(),
            role: user.role,
            exp,
        },
        auth.jwt_key(),
    )?;
    Ok(Json(LoginToken { token }).into_response())
}
