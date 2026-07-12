//! Guards are dependencies (spec §4.3): `Session<T>`/`Bearer<T>` are extractors
//! returning 401; `require_role` returns 403. No auth middleware.

use jerrycan_core::{Error, FromRequest, Headers, RequestCtx, Result};
use serde::de::DeserializeOwned;

/// Session extractor: decrypts the `jerrycan_session` cookie into `T`.
/// Absent/invalid cookie → 401. Requires the `Auth` extension to be registered.
pub struct Session<T>(pub T);

impl<T: DeserializeOwned + Send> FromRequest for Session<T> {
    async fn from_request(ctx: &mut RequestCtx) -> Result<Self> {
        let auth = ctx.resolve::<crate::Auth>().await?;
        let headers = Headers::from_request(ctx).await?;
        let cookie_header = headers.get("cookie").ok_or_else(Error::unauthorized)?;
        let token = auth
            .sessions()
            .read_cookie(cookie_header)
            .ok_or_else(Error::unauthorized)?;
        auth.sessions().decode::<T>(&token).map(Session)
    }
}

/// Bearer JWT extractor: verifies the `Authorization: Bearer <jwt>` token into `T`.
pub struct Bearer<T>(pub T);

impl<T: DeserializeOwned + Send> FromRequest for Bearer<T> {
    async fn from_request(ctx: &mut RequestCtx) -> Result<Self> {
        let auth = ctx.resolve::<crate::Auth>().await?;
        let headers = Headers::from_request(ctx).await?;
        let value = headers
            .get("authorization")
            .ok_or_else(Error::unauthorized)?;
        let token = value
            .strip_prefix("Bearer ")
            .ok_or_else(Error::unauthorized)?;
        crate::jwt::decode::<T>(token, auth.jwt_key()).map(Bearer)
    }
}

/// Role check helper for generated guards: `403` when the role doesn't match.
pub fn require_role(actual: &str, required: &str) -> Result<()> {
    if actual == required {
        Ok(())
    } else {
        Err(Error::forbidden())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Auth;
    use jerrycan_core::{App, Dep, Json, get, post};
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, Clone)]
    struct User {
        // Stringified pk (mirrors storage's TEXT owner_id) so a uuid user id works.
        id: String,
        role: String,
    }

    async fn login(auth: Dep<Auth>) -> Result<jerrycan_core::Response> {
        // Issue a session cookie for a fixed user (test login).
        let cookie = auth.sessions().set_cookie(&User {
            id: "1".into(),
            role: "admin".into(),
        })?;
        let mut res = jerrycan_core::IntoResponse::into_response("ok");
        res.headers_mut().insert(
            jerrycan_core::http::header::SET_COOKIE,
            jerrycan_core::http::HeaderValue::from_str(&cookie).unwrap(),
        );
        Ok(res)
    }

    async fn whoami(Session(user): Session<User>) -> Json<String> {
        Json(user.id)
    }

    fn app() -> App {
        App::new()
            .extend(Auth::with_secret("a-very-long-development-secret-string!!"))
            .route("/login", post(login))
            .route("/me", get(whoami))
    }

    #[tokio::test]
    async fn no_cookie_is_401() {
        let t = app().into_test();
        assert_eq!(
            t.get("/me").await.status(),
            jerrycan_core::http::StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn login_then_authenticated_request_succeeds() {
        let t = app().into_test();
        let login = t.post_json("/login", &()).await;
        let set_cookie = login.headers()["set-cookie"].to_str().unwrap().to_string();
        let cookie = set_cookie.split(';').next().unwrap().to_string(); // jerrycan_session=...
        let res = t.get_with("/me", &[("cookie", &cookie)]).await;
        assert_eq!(res.status(), jerrycan_core::http::StatusCode::OK);
        assert_eq!(res.json::<String>(), "1");
    }

    #[tokio::test]
    async fn require_role_rejects_wrong_role_with_403() {
        async fn admin_only(Session(user): Session<User>) -> Result<&'static str> {
            require_role(&user.role, "superadmin")?;
            Ok("secret")
        }
        let t = App::new()
            .extend(Auth::with_secret("a-very-long-development-secret-string!!"))
            .route("/login", post(login))
            .route("/admin", get(admin_only))
            .into_test();
        let login = t.post_json("/login", &()).await;
        let cookie = login.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();
        let res = t.get_with("/admin", &[("cookie", &cookie)]).await;
        assert_eq!(res.status(), jerrycan_core::http::StatusCode::FORBIDDEN);
    }
}
