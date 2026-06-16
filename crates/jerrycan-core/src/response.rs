//! Response model. Handlers return anything implementing [`IntoResponse`];
//! `Result<T, Error>` renders errors as `{"code","message"}` JSON (spec §4.1).

use crate::error::Error;
use bytes::Bytes;
use http::{HeaderValue, StatusCode, header};
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use serde::Serialize;

/// Mid-stream body failure. Reaching hyper as a body error aborts the
/// connection, so the client sees a truncated (invalid) chunked stream rather
/// than a clean end — truncation must be detectable.
#[derive(Debug)]
pub struct BodyError(String);

impl BodyError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl std::fmt::Display for BodyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BodyError {}

impl From<std::convert::Infallible> for BodyError {
    fn from(e: std::convert::Infallible) -> Self {
        match e {}
    }
}

/// The response body: a fixed buffer for buffered handlers, or a stream when a
/// handler returns [`StreamBody`] (downloads, exports). Wraps a `BoxBody` so the
/// response type is stable whichever shape the body takes; its error channel is
/// [`BodyError`], which a mid-stream failure rides to abort the connection.
pub struct JcBody(BoxBody<Bytes, BodyError>);

impl JcBody {
    /// A complete, in-memory body.
    pub fn full(bytes: impl Into<Bytes>) -> Self {
        Self(Full::new(bytes.into()).map_err(BodyError::from).boxed())
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
        B: http_body::Body<Data = Bytes> + Send + Sync + 'static,
        B::Error: Into<BodyError>,
    {
        Self(body.map_err(Into::into).boxed())
    }
}

impl http_body::Body for JcBody {
    type Data = Bytes;
    type Error = BodyError;

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

/// An HTTP redirect: an empty body plus a `Location` header and a 3xx status.
/// Use the constructor that names the semantics you want — `to`/`see_other`/
/// `temporary`/`permanent` — rather than hand-setting a status code.
pub struct Redirect {
    status: StatusCode,
    location: String,
}

impl Redirect {
    /// 302 Found — the default redirect. The method may change to GET on follow
    /// (legacy behavior); prefer [`Redirect::see_other`] after a POST.
    pub fn to(location: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FOUND,
            location: location.into(),
        }
    }

    /// 303 See Other — redirect a POST/PUT to a GET (the POST-redirect-GET pattern).
    pub fn see_other(location: impl Into<String>) -> Self {
        Self {
            status: StatusCode::SEE_OTHER,
            location: location.into(),
        }
    }

    /// 307 Temporary Redirect — preserves the method and body on follow.
    pub fn temporary(location: impl Into<String>) -> Self {
        Self {
            status: StatusCode::TEMPORARY_REDIRECT,
            location: location.into(),
        }
    }

    /// 308 Permanent Redirect — preserves the method and body, and is cacheable.
    pub fn permanent(location: impl Into<String>) -> Self {
        Self {
            status: StatusCode::PERMANENT_REDIRECT,
            location: location.into(),
        }
    }
}

impl IntoResponse for Redirect {
    fn into_response(self) -> Response {
        // A control char (or other non-token byte) in the location can't go into
        // a header value. That's a programming error in the handler, not a client
        // fault, so surface it as a 500 rather than panicking the request task.
        let value = match HeaderValue::from_str(&self.location) {
            Ok(v) => v,
            Err(_) => {
                return Error::internal("redirect location is not a valid header value")
                    .into_response();
            }
        };
        let mut r = http::Response::new(JcBody::empty());
        *r.status_mut() = self.status;
        r.headers_mut().insert(header::LOCATION, value);
        r
    }
}

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

/// Render the inner value, then overwrite the status. This is what makes
/// `(StatusCode::ACCEPTED, Json(body))` a 202-with-JSON and
/// `(StatusCode::ACCEPTED, "queued")` a 202 text response — the body's own
/// content type and bytes are kept, only the status line changes.
impl<T: IntoResponse> IntoResponse for (StatusCode, T) {
    fn into_response(self) -> Response {
        let (status, inner) = self;
        let mut r = inner.into_response();
        *r.status_mut() = status;
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

/// A streaming response body: downloads, CSV exports, anything produced
/// incrementally. Defaults: `application/octet-stream`, 200 OK, 30s frame
/// timeout (a producer that stalls longer aborts the connection).
pub struct StreamBody {
    stream: std::pin::Pin<
        Box<dyn futures_core::Stream<Item = Result<Bytes, Error>> + Send + Sync + 'static>,
    >,
    content_type: HeaderValue,
    attachment: Option<HeaderValue>,
    frame_timeout: std::time::Duration,
}

impl StreamBody {
    /// Default per-frame producer deadline (see [`StreamBody::frame_timeout`]).
    pub const DEFAULT_FRAME_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

    /// Stream chunks from anything implementing `Stream` (SeaORM streaming
    /// queries, hand-rolled producers). An `Err` item aborts the connection.
    pub fn new<S>(stream: S) -> Self
    where
        S: futures_core::Stream<Item = Result<Bytes, Error>> + Send + Sync + 'static,
    {
        Self {
            stream: Box::pin(stream),
            content_type: HeaderValue::from_static("application/octet-stream"),
            attachment: None,
            frame_timeout: Self::DEFAULT_FRAME_TIMEOUT,
        }
    }

    /// A channel-fed body for producers that push: returns the body and a
    /// sender. Dropping the sender ends the stream cleanly; `fail` aborts it.
    /// The channel is bounded, so `send` awaits while a slow client is behind.
    pub fn channel() -> (Self, BodySender) {
        // 16: bounded buffer — a slow client backpressures the producer instead of buffering unboundedly.
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, Error>>(16);
        (Self::new(ReceiverStream(rx)), BodySender(tx))
    }

    /// Sets the `content-type` header. Panics on a value that is not a valid
    /// header value — that is a programming error, not request-dependent.
    pub fn content_type(mut self, value: &str) -> Self {
        self.content_type =
            HeaderValue::from_str(value).expect("content_type must be a valid header value");
        self
    }

    /// Marks the response as a download: `content-disposition: attachment` with
    /// the given filename. Quotes/backslashes are stripped (header-injection
    /// break-out) and control chars too — stripping the latter is what makes the
    /// following `HeaderValue::from_str` infallible for any UTF-8 input. Non-ASCII
    /// filenames pass through verbatim (no RFC 5987 `filename*` encoding).
    pub fn attachment(mut self, filename: &str) -> Self {
        let safe: String = filename
            .chars()
            .filter(|c| *c != '"' && *c != '\\' && !c.is_control())
            .collect();
        self.attachment = Some(
            HeaderValue::from_str(&format!("attachment; filename=\"{safe}\""))
                .expect("sanitized filename is a valid header value"),
        );
        self
    }

    /// Maximum time the producer may take between chunks before the
    /// connection is aborted.
    pub fn frame_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.frame_timeout = timeout;
        self
    }
}

/// Push side of [`StreamBody::channel`].
pub struct BodySender(tokio::sync::mpsc::Sender<Result<Bytes, Error>>);

impl BodySender {
    /// Sends one chunk. Returns false when the client is gone (stop producing).
    pub async fn send(&self, chunk: impl Into<Bytes>) -> bool {
        self.0.send(Ok(chunk.into())).await.is_ok()
    }
    /// Aborts the response: the connection is reset so the client sees
    /// truncation instead of a falsely-complete body.
    pub async fn fail(self, error: Error) -> bool {
        self.0.send(Err(error)).await.is_ok()
    }
}

/// mpsc receiver as a Stream (hand-rolled: futures-util is not a dependency).
struct ReceiverStream(tokio::sync::mpsc::Receiver<Result<Bytes, Error>>);

impl futures_core::Stream for ReceiverStream {
    type Item = Result<Bytes, Error>;
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.0.poll_recv(cx)
    }
}

/// Stream → Body adapter with a per-frame producer deadline. The deadline arms
/// when a poll returns Pending and RESETS on every yielded frame, so steady
/// producers of any total duration are unaffected; only stalls trip it.
struct TimedFrames {
    stream: std::pin::Pin<
        Box<dyn futures_core::Stream<Item = Result<Bytes, Error>> + Send + Sync + 'static>,
    >,
    timeout: std::time::Duration,
    sleep: Option<std::pin::Pin<Box<tokio::time::Sleep>>>,
}

impl http_body::Body for TimedFrames {
    type Data = Bytes;
    type Error = BodyError;

    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Bytes>, BodyError>>> {
        use std::future::Future;
        use std::task::Poll;
        match self.stream.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(chunk))) => {
                self.sleep = None;
                Poll::Ready(Some(Ok(http_body::Frame::data(chunk))))
            }
            Poll::Ready(Some(Err(e))) => {
                self.sleep = None;
                Poll::Ready(Some(Err(BodyError::new(format!(
                    "response stream failed: {e}"
                )))))
            }
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => {
                let timeout = self.timeout;
                let sleep = self
                    .sleep
                    .get_or_insert_with(|| Box::pin(tokio::time::sleep(timeout)));
                match sleep.as_mut().poll(cx) {
                    Poll::Ready(()) => {
                        self.sleep = None;
                        Poll::Ready(Some(Err(BodyError::new(
                            "response stream timed out producing the next chunk",
                        ))))
                    }
                    Poll::Pending => Poll::Pending,
                }
            }
        }
    }
}

impl IntoResponse for StreamBody {
    fn into_response(self) -> Response {
        let body = JcBody::stream(TimedFrames {
            stream: self.stream,
            timeout: self.frame_timeout,
            sleep: None,
        });
        let mut r = http::Response::new(body);
        r.headers_mut()
            .insert(header::CONTENT_TYPE, self.content_type);
        if let Some(disposition) = self.attachment {
            r.headers_mut()
                .insert(header::CONTENT_DISPOSITION, disposition);
        }
        r
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

    #[test]
    fn redirect_to_is_302_with_location_and_empty_body() {
        let r = Redirect::to("/x").into_response();
        assert_eq!(r.status(), StatusCode::FOUND);
        assert_eq!(r.headers()[header::LOCATION], "/x");
        assert_eq!(body_of(r), "");
    }

    #[test]
    fn redirect_constructors_set_their_status_and_location() {
        // Each named constructor encodes a distinct redirect semantic; a regression
        // that collapses them to one status would change browser follow behavior.
        for (build, status) in [
            (Redirect::see_other("/a") as Redirect, StatusCode::SEE_OTHER),
            (Redirect::temporary("/b"), StatusCode::TEMPORARY_REDIRECT),
            (Redirect::permanent("/c"), StatusCode::PERMANENT_REDIRECT),
        ] {
            let r = build.into_response();
            assert_eq!(r.status(), status);
            assert!(r.headers().contains_key(header::LOCATION));
        }
    }

    #[test]
    fn redirect_with_invalid_location_is_a_non_panicking_500() {
        // A control char can't be a header value; the handler shouldn't panic the
        // request task, it should surface a 500 the connection can report.
        let r = Redirect::to("/bad\nlocation").into_response();
        assert_eq!(r.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(r.headers().get(header::LOCATION).is_none());
    }

    #[test]
    fn status_tuple_overrides_status_keeping_the_json_body() {
        // (StatusCode, Json) must render the JSON body (content type + bytes) and
        // only swap the status — that's what lets a 202 carry a payload.
        #[derive(Serialize)]
        struct Summary {
            queued: u32,
        }
        let r = (StatusCode::ACCEPTED, Json(Summary { queued: 3 })).into_response();
        assert_eq!(r.status(), StatusCode::ACCEPTED);
        assert_eq!(r.headers()[header::CONTENT_TYPE], "application/json");
        assert_eq!(body_of(r), r#"{"queued":3}"#);
    }

    #[test]
    fn status_tuple_overrides_status_keeping_the_text_body() {
        let r = (StatusCode::ACCEPTED, "queued").into_response();
        assert_eq!(r.status(), StatusCode::ACCEPTED);
        assert_eq!(
            r.headers()[header::CONTENT_TYPE],
            "text/plain; charset=utf-8"
        );
        assert_eq!(body_of(r), "queued");
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

    #[tokio::test]
    async fn stream_body_streams_with_content_type_and_disposition() {
        let (body, tx) = StreamBody::channel();
        let send = async move {
            assert!(tx.send("a,b\n").await);
            assert!(tx.send("1,2\n").await);
        };
        let r = body
            .content_type("text/csv")
            .attachment("export.csv")
            .into_response();
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(r.headers()[header::CONTENT_TYPE], "text/csv");
        assert_eq!(
            r.headers()[header::CONTENT_DISPOSITION],
            "attachment; filename=\"export.csv\""
        );
        let (_, collected) = tokio::join!(send, r.into_body().collect());
        assert_eq!(collected.unwrap().to_bytes(), Bytes::from("a,b\n1,2\n"));
    }

    #[tokio::test(start_paused = true)]
    async fn stream_body_frame_timeout_errors_the_body() {
        struct Never;
        impl futures_core::Stream for Never {
            type Item = Result<Bytes, Error>;
            fn poll_next(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Option<Self::Item>> {
                std::task::Poll::Pending
            }
        }
        let body = StreamBody::new(Never)
            .frame_timeout(std::time::Duration::from_millis(100))
            .into_response()
            .into_body();
        use http_body_util::BodyExt;
        let err = body
            .collect()
            .await
            .expect_err("stall must error, not end cleanly");
        assert!(err.to_string().contains("timed out"), "{err}");
    }

    #[tokio::test]
    async fn channel_fail_surfaces_as_a_body_error_carrying_the_message() {
        // The headline guarantee of the error channel: a producer that fails
        // after some output must reach the client as a body ERROR (truncation),
        // never a clean end. This is the test that fails if the `Err` branch of
        // `TimedFrames::poll_frame` regresses to swallowing the error.
        let (body, tx) = StreamBody::channel();
        let produce = async move {
            assert!(tx.send("first chunk").await, "client present");
            assert!(tx.fail(Error::internal("boom")).await, "fail delivered");
        };
        let response = body.into_response();
        use http_body_util::BodyExt;
        let (_, collected) = tokio::join!(produce, response.into_body().collect());
        let err = collected.expect_err("a failed producer must error the body, not end cleanly");
        assert!(
            err.to_string().contains("boom"),
            "the propagated message must survive to the body error: {err}"
        );
    }

    #[tokio::test]
    async fn stream_body_composes_through_a_real_handler_dispatch() {
        use crate::prelude::*;
        async fn export() -> Result<StreamBody> {
            let (body, tx) = StreamBody::channel();
            tokio::spawn(async move {
                tx.send("id,name\n").await;
                tx.send("1,ada\n").await;
            });
            Ok(body.content_type("text/csv"))
        }
        let t = App::new().route("/export", get(export)).into_test();
        let r = t.get("/export").await;
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(r.headers()[header::CONTENT_TYPE], "text/csv");
        assert_eq!(r.text(), "id,name\n1,ada\n");
    }
}
