//! Validation: the `Validate` trait, the `Valid<T>` extractor (422 with
//! structured `details` on violation), and the `OpenApi` extension serving the
//! platform-generated document. `derive(Validate)` with rule attributes is a
//! contract-v1 candidate (the design schema has no field constraints yet).
#![forbid(unsafe_code)]

use jerrycan_core::{App, Error, Extension, FromRequest, IntoResponse, RequestCtx, Result};
use serde::Serialize;

/// One field-level problem, rendered into the 422 `details` array.
#[derive(Debug, Clone, Serialize)]
pub struct Violation {
    pub field: String,
    pub message: String,
}

impl Violation {
    pub fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

/// Types that can check their own invariants after extraction.
pub trait Validate {
    fn validate(&self) -> std::result::Result<(), Vec<Violation>>;
}

/// Transparent: validating a wrapper validates the payload.
impl<T: Validate> Validate for jerrycan_core::Json<T> {
    fn validate(&self) -> std::result::Result<(), Vec<Violation>> {
        self.0.validate()
    }
}

/// Extract-then-validate: `Valid(Json(todo)): Valid<Json<NewTodo>>`.
/// Violations become `422 JC0422` with a structured `details` array.
pub struct Valid<T>(pub T);

impl<T> FromRequest for Valid<T>
where
    T: FromRequest + Validate,
{
    async fn from_request(ctx: &mut RequestCtx) -> Result<Self> {
        let inner = T::from_request(ctx).await?;
        match inner.validate() {
            Ok(()) => Ok(Valid(inner)),
            Err(violations) => Err(Error::unprocessable("validation failed").with_details(
                serde_json::to_value(violations).expect("violations serialize"),
            )),
        }
    }
}

/// Serves a pre-generated OpenAPI document at `GET /openapi.json`.
/// The platform generates the document from design.json (tool-owned).
pub struct OpenApi {
    document: &'static str,
}

impl OpenApi {
    pub fn new(document: &'static str) -> Self {
        Self { document }
    }
}

impl Extension for OpenApi {
    fn register(self, app: App) -> App {
        let doc = self.document;
        app.route(
            "/openapi.json",
            jerrycan_core::get(move || async move { http_json(doc) }),
        )
    }
}

/// Build an application/json response from a static string without
/// double-encoding (Json<&str> would quote it).
fn http_json(body: &'static str) -> jerrycan_core::Response {
    let mut response = body.into_response();
    response.headers_mut().insert(
        jerrycan_core::http::header::CONTENT_TYPE,
        jerrycan_core::http::HeaderValue::from_static("application/json"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use jerrycan_core::{get, post, Json};
    use serde::Deserialize;

    #[derive(Deserialize, Serialize)]
    struct NewTodo {
        title: String,
    }

    impl Validate for NewTodo {
        fn validate(&self) -> std::result::Result<(), Vec<Violation>> {
            if self.title.trim().is_empty() {
                return Err(vec![Violation::new("title", "must not be empty")]);
            }
            Ok(())
        }
    }

    async fn create(Valid(Json(todo)): Valid<Json<NewTodo>>) -> Json<NewTodo> {
        Json(todo)
    }

    #[tokio::test]
    async fn valid_payloads_pass_through() {
        let t = App::new().route("/todos", post(create)).into_test();
        let res = t.post_json("/todos", &NewTodo { title: "ship".into() }).await;
        assert_eq!(res.status(), jerrycan_core::http::StatusCode::OK);
    }

    #[tokio::test]
    async fn violations_become_422_with_structured_details() {
        let t = App::new().route("/todos", post(create)).into_test();
        let res = t.post_json("/todos", &NewTodo { title: "   ".into() }).await;
        assert_eq!(res.status(), jerrycan_core::http::StatusCode::UNPROCESSABLE_ENTITY);
        let body: serde_json::Value = res.json();
        assert_eq!(body["code"], "JC0422");
        assert_eq!(body["details"][0]["field"], "title");
        assert_eq!(body["details"][0]["message"], "must not be empty");
    }

    #[tokio::test]
    async fn openapi_extension_serves_the_document() {
        let t = App::new()
            .extend(OpenApi::new(r#"{"openapi":"3.1.0","info":{"title":"demo"}}"#))
            .route("/x", get(|| async { "x" }))
            .into_test();
        let res = t.get("/openapi.json").await;
        assert_eq!(res.status(), jerrycan_core::http::StatusCode::OK);
        assert_eq!(res.headers()["content-type"], "application/json");
        let doc: serde_json::Value = res.json();
        assert_eq!(doc["openapi"], "3.1.0");
    }
}
