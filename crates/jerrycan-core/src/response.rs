//! Response model. Handlers return anything implementing [`IntoResponse`];
//! `Result<T, Error>` renders errors as `{"code","message"}` JSON (spec §4.1).

use crate::error::Error;
use bytes::Bytes;
use http::{HeaderValue, StatusCode, header};
use http_body_util::Full;
use serde::Serialize;

/// The concrete response type of the spike. Streaming bodies arrive in Phase 1
/// behind the same `IntoResponse` seam, so handler signatures won't change.
pub type Response = http::Response<Full<Bytes>>;

/// Conversion of handler return values into HTTP responses.
pub trait IntoResponse {
    fn into_response(self) -> Response;
}

/// JSON body wrapper: `Json(value)` serializes with `application/json`.
pub struct Json<T>(pub T);

/// 201 Created with a JSON body.
pub struct Created<T>(pub T);

/// 204 No Content.
pub struct NoContent;

fn full(status: StatusCode, content_type: &'static str, body: impl Into<Bytes>) -> Response {
    let mut r = http::Response::new(Full::new(body.into()));
    *r.status_mut() = status;
    r.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    r
}

fn json_body<T: Serialize>(status: StatusCode, value: &T) -> Response {
    match serde_json::to_vec(value) {
        Ok(bytes) => full(status, "application/json", bytes),
        Err(e) => Error::internal(format!("response serialization failed: {e}")).into_response(),
    }
}

impl IntoResponse for Response {
    fn into_response(self) -> Response {
        self
    }
}

impl IntoResponse for &'static str {
    fn into_response(self) -> Response {
        full(
            StatusCode::OK,
            "text/plain; charset=utf-8",
            self.as_bytes().to_vec(),
        )
    }
}

impl IntoResponse for String {
    fn into_response(self) -> Response {
        full(
            StatusCode::OK,
            "text/plain; charset=utf-8",
            self.into_bytes(),
        )
    }
}

impl IntoResponse for StatusCode {
    fn into_response(self) -> Response {
        let mut r = http::Response::new(Full::new(Bytes::new()));
        *r.status_mut() = self;
        r
    }
}

impl<T: Serialize> IntoResponse for Json<T> {
    fn into_response(self) -> Response {
        json_body(StatusCode::OK, &self.0)
    }
}

impl<T: Serialize> IntoResponse for Created<T> {
    fn into_response(self) -> Response {
        json_body(StatusCode::CREATED, &self.0)
    }
}

impl IntoResponse for NoContent {
    fn into_response(self) -> Response {
        let mut r = http::Response::new(Full::new(Bytes::new()));
        *r.status_mut() = StatusCode::NO_CONTENT;
        r
    }
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    code: &'a str,
    message: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<&'a serde_json::Value>,
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        json_body(
            self.status(),
            &ErrorBody {
                code: self.code(),
                message: self.message(),
                details: self.details(),
            },
        )
    }
}

impl<T: IntoResponse> IntoResponse for crate::Result<T> {
    fn into_response(self) -> Response {
        match self {
            Ok(v) => v.into_response(),
            Err(e) => e.into_response(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body_of(r: &Response) -> String {
        // Full<Bytes> exposes its data without polling via a clone of the inner frame.
        let bytes = r.body().clone();
        let collected = futures_executor_lite(bytes);
        String::from_utf8(collected.to_vec()).unwrap()
    }

    /// Minimal "block on a Full body" helper so unit tests need no runtime.
    fn futures_executor_lite(full: Full<Bytes>) -> Bytes {
        use http_body_util::BodyExt;
        let fut = full.collect();
        // Full's collect future is immediately ready; poll it once by hand.
        let mut fut = Box::pin(fut);
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        match fut.as_mut().poll(&mut cx) {
            std::task::Poll::Ready(Ok(c)) => c.to_bytes(),
            _ => panic!("Full body was not immediately ready"),
        }
    }

    #[test]
    fn str_becomes_200_text() {
        let r = "hello".into_response();
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(
            r.headers()[header::CONTENT_TYPE],
            "text/plain; charset=utf-8"
        );
        assert_eq!(body_of(&r), "hello");
    }

    #[test]
    fn json_wrapper_sets_content_type() {
        #[derive(Serialize)]
        struct Todo {
            id: u32,
        }
        let r = Json(Todo { id: 7 }).into_response();
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(r.headers()[header::CONTENT_TYPE], "application/json");
        assert_eq!(body_of(&r), r#"{"id":7}"#);
    }

    #[test]
    fn created_is_201_and_no_content_is_204() {
        #[derive(Serialize)]
        struct T {
            ok: bool,
        }
        assert_eq!(
            Created(T { ok: true }).into_response().status(),
            StatusCode::CREATED
        );
        let r = NoContent.into_response();
        assert_eq!(r.status(), StatusCode::NO_CONTENT);
        assert_eq!(body_of(&r), "");
    }

    #[test]
    fn errors_render_code_and_message_json() {
        let r = Error::not_found().into_response();
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
        assert_eq!(body_of(&r), r#"{"code":"JC0404","message":"not found"}"#);
    }

    #[test]
    fn error_details_appear_in_the_body_only_when_present() {
        let r = Error::not_found().into_response();
        assert_eq!(body_of(&r), r#"{"code":"JC0404","message":"not found"}"#);
        let r = Error::unprocessable("validation failed")
            .with_details(serde_json::json!([{ "field": "t" }]))
            .into_response();
        assert_eq!(
            body_of(&r),
            r#"{"code":"JC0422","message":"validation failed","details":[{"field":"t"}]}"#
        );
    }

    #[test]
    fn result_renders_ok_or_err() {
        let ok: crate::Result<&'static str> = Ok("fine");
        assert_eq!(ok.into_response().status(), StatusCode::OK);
        let err: crate::Result<&'static str> = Err(Error::bad_request("x"));
        assert_eq!(err.into_response().status(), StatusCode::BAD_REQUEST);
    }
}
