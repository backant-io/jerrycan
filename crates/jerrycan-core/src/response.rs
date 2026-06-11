//! Response model. Handlers return anything implementing [`IntoResponse`];
//! `Result<T, Error>` renders errors as `{"code","message"}` JSON (spec §4.1).

use crate::error::Error;
use bytes::Bytes;
use http::{HeaderValue, StatusCode, header};
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use serde::Serialize;

/// The response body: a fixed buffer today, a stream when a handler opts in
/// (`StreamBody` arrives with the protocol-surface phase; the TYPE lands now so
/// it is a feature, not a core change). Wraps a `BoxBody` so the response type
/// is stable whether the body is a full buffer or a stream.
pub struct JcBody(BoxBody<Bytes, std::convert::Infallible>);

impl JcBody {
    /// A complete, in-memory body.
    pub fn full(bytes: impl Into<Bytes>) -> Self {
        Self(Full::new(bytes.into()).boxed())
    }

    /// An empty body (zero frames).
    pub fn empty() -> Self {
        Self::full(Bytes::new())
    }

    /// A streaming body. The handler drives `body`'s frames as the response is
    /// written. `BoxBody` requires `Send + Sync` so the response stays usable
    /// across hyper's `Send` service future.
    pub fn stream<B>(body: B) -> Self
    where
        B: http_body::Body<Data = Bytes, Error = std::convert::Infallible> + Send + Sync + 'static,
    {
        Self(BoxBody::new(body))
    }
}

impl http_body::Body for JcBody {
    type Data = Bytes;
    type Error = std::convert::Infallible;

    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        std::pin::Pin::new(&mut self.0).poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.0.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.0.size_hint()
    }
}

/// The concrete response type. Streaming bodies ride the same `IntoResponse`
/// seam as buffered ones, so handler signatures won't change.
pub type Response = http::Response<JcBody>;

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
    let mut r = http::Response::new(JcBody::full(body));
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
        let mut r = http::Response::new(JcBody::empty());
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
        let mut r = http::Response::new(JcBody::empty());
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

    fn body_of(r: Response) -> String {
        let collected = futures_executor_lite(r.into_body());
        String::from_utf8(collected.to_vec()).unwrap()
    }

    /// Minimal "block on a buffered body" helper so unit tests need no runtime.
    /// The bodies built by `IntoResponse` are full buffers whose collect future
    /// is immediately ready, so we poll it once by hand.
    fn futures_executor_lite(body: JcBody) -> Bytes {
        let fut = body.collect();
        let mut fut = Box::pin(fut);
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        match fut.as_mut().poll(&mut cx) {
            std::task::Poll::Ready(Ok(c)) => c.to_bytes(),
            _ => panic!("buffered body was not immediately ready"),
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
        assert_eq!(body_of(r), "hello");
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
        assert_eq!(body_of(r), r#"{"id":7}"#);
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
        assert_eq!(body_of(r), "");
    }

    #[test]
    fn errors_render_code_and_message_json() {
        let r = Error::not_found().into_response();
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
        assert_eq!(body_of(r), r#"{"code":"JC0404","message":"not found"}"#);
    }

    #[test]
    fn error_details_appear_in_the_body_only_when_present() {
        let r = Error::not_found().into_response();
        assert_eq!(body_of(r), r#"{"code":"JC0404","message":"not found"}"#);
        let r = Error::unprocessable("validation failed")
            .with_details(serde_json::json!([{ "field": "t" }]))
            .into_response();
        assert_eq!(
            body_of(r),
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

    #[tokio::test]
    async fn boxed_bodies_stream_and_collect() {
        // hand-rolled chunked Body over a VecDeque — no new deps
        struct Chunks(std::collections::VecDeque<Bytes>);
        impl http_body::Body for Chunks {
            type Data = Bytes;
            type Error = std::convert::Infallible;
            fn poll_frame(
                mut self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Option<Result<http_body::Frame<Bytes>, Self::Error>>> {
                std::task::Poll::Ready(self.0.pop_front().map(|b| Ok(http_body::Frame::data(b))))
            }
        }
        let body = JcBody::stream(Chunks(
            [Bytes::from("ab"), Bytes::from("cd")].into_iter().collect(),
        ));
        use http_body_util::BodyExt;
        let collected = body.collect().await.unwrap().to_bytes();
        assert_eq!(collected, Bytes::from("abcd"));
    }
}
