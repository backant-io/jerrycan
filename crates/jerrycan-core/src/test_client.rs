//! In-memory test client (spec §4.1 "Test client"): no sockets, no network.
//! `override_dep` is THE testing seam — fake any dependency, run real requests.
//!
//! Panics in handlers propagate in tests by design — the serve path converts them to 500 JC0500.

use crate::App;
use crate::app::{BuiltApp, Policy};
use crate::clock::Clock;
use crate::error::Error;
use crate::response::{IntoResponse, Response};
use bytes::Bytes;
use http::{Method, StatusCode, header};
use http_body_util::BodyExt;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::any::TypeId;
use std::sync::Arc;

impl App {
    /// Build for testing. Panics on build errors — a test should fail loudly.
    ///
    /// Swaps the default real [`Clock`] for a controllable [`Clock::test`] via
    /// the override seam (overrides outrank the app's `Clock::system` singleton)
    /// and keeps a handle on the same clock — [`TestApp::clock`] returns it, so
    /// `advance`/`set` move the very clock handlers resolve.
    pub fn into_test(self) -> TestApp {
        let mut built = self.build().expect("app failed to build");
        let clock = Clock::test();
        let mut overrides = (*built.overrides).clone();
        overrides.insert(
            TypeId::of::<Clock>(),
            Arc::new(clock.clone()) as crate::dep::AnyArc,
        );
        built.overrides = Arc::new(overrides);
        TestApp { built, clock }
    }
}

pub struct TestApp {
    built: BuiltApp,
    /// The test clock handed to handlers via the override above. Shares its
    /// offset with the resolved copy (`Clock` clones share one `Arc`), so
    /// `clock().advance(..)` is observable through real requests.
    clock: Clock,
}

impl TestApp {
    /// Replace the provider for `T` everywhere (values AND factories) for all
    /// subsequent requests. Chainable.
    pub fn override_dep<T: Send + Sync + 'static>(mut self, value: T) -> Self {
        let mut map = (*self.built.overrides).clone();
        map.insert(TypeId::of::<T>(), Arc::new(value) as crate::dep::AnyArc);
        self.built.overrides = Arc::new(map);
        self
    }

    /// The controllable [`Clock`] injected for this test. `advance`/`set` it to
    /// move domain time (rate windows, schedules, expiry) under the app; the
    /// change is visible to every subsequent request and task context.
    pub fn clock(&self) -> Clock {
        self.clock.clone()
    }

    /// A [`TaskContext`](crate::TaskContext) for resolving app-level dependencies
    /// outside a request, honoring any `override_dep` fakes set on this `TestApp`.
    ///
    /// Only **app-level** dependencies (those registered with `App::provide` /
    /// `App::provide_dep`) are resolvable; module-scoped providers are not in
    /// scope here.
    pub fn task_context(&self) -> crate::TaskContext {
        self.built.task_context()
    }

    pub async fn get(&self, path: &str) -> TestResponse {
        self.request(Method::GET, path, None).await
    }
    pub async fn delete(&self, path: &str) -> TestResponse {
        self.request(Method::DELETE, path, None).await
    }
    pub async fn post_json<B: Serialize>(&self, path: &str, body: &B) -> TestResponse {
        self.request(
            Method::POST,
            path,
            Some(serde_json::to_vec(body).expect("serialize")),
        )
        .await
    }
    pub async fn put_json<B: Serialize>(&self, path: &str, body: &B) -> TestResponse {
        self.request(
            Method::PUT,
            path,
            Some(serde_json::to_vec(body).expect("serialize")),
        )
        .await
    }
    pub async fn patch_json<B: Serialize>(&self, path: &str, body: &B) -> TestResponse {
        self.request(
            Method::PATCH,
            path,
            Some(serde_json::to_vec(body).expect("serialize")),
        )
        .await
    }

    /// POST a raw byte body (content-type `application/octet-stream`). Routes
    /// and per-route body limits apply exactly as they do over a socket.
    pub async fn post_bytes(&self, path: &str, bytes: &[u8]) -> TestResponse {
        self.post_bytes_with(path, bytes, &[]).await
    }

    /// POST a raw byte body with explicit request headers.
    pub async fn post_bytes_with(
        &self,
        path: &str,
        bytes: &[u8],
        headers: &[(&str, &str)],
    ) -> TestResponse {
        self.send(
            Method::POST,
            path,
            Some(Bytes::copy_from_slice(bytes)),
            Some("application/octet-stream"),
            headers,
        )
        .await
    }

    /// GET with explicit request headers (auth tests, content negotiation).
    pub async fn get_with(&self, path: &str, headers: &[(&str, &str)]) -> TestResponse {
        self.request_with(Method::GET, path, None, headers).await
    }

    /// POST JSON with explicit request headers.
    pub async fn post_json_with<B: Serialize>(
        &self,
        path: &str,
        body: &B,
        headers: &[(&str, &str)],
    ) -> TestResponse {
        self.request_with(
            Method::POST,
            path,
            Some(serde_json::to_vec(body).expect("serialize")),
            headers,
        )
        .await
    }

    /// DELETE with explicit request headers (guarded-route auth tests).
    pub async fn delete_with(&self, path: &str, headers: &[(&str, &str)]) -> TestResponse {
        self.request_with(Method::DELETE, path, None, headers).await
    }

    /// PUT JSON with explicit request headers.
    pub async fn put_json_with<B: Serialize>(
        &self,
        path: &str,
        body: &B,
        headers: &[(&str, &str)],
    ) -> TestResponse {
        self.request_with(
            Method::PUT,
            path,
            Some(serde_json::to_vec(body).expect("serialize")),
            headers,
        )
        .await
    }

    /// PATCH JSON with explicit request headers.
    pub async fn patch_json_with<B: Serialize>(
        &self,
        path: &str,
        body: &B,
        headers: &[(&str, &str)],
    ) -> TestResponse {
        self.request_with(
            Method::PATCH,
            path,
            Some(serde_json::to_vec(body).expect("serialize")),
            headers,
        )
        .await
    }

    async fn request(&self, method: Method, path: &str, json: Option<Vec<u8>>) -> TestResponse {
        self.request_with(method, path, json, &[]).await
    }

    async fn request_with(
        &self,
        method: Method,
        path: &str,
        json: Option<Vec<u8>>,
        headers: &[(&str, &str)],
    ) -> TestResponse {
        let content_type = json.as_ref().map(|_| "application/json");
        self.send(method, path, json.map(Bytes::from), content_type, headers)
            .await
    }

    /// The single test request path: build the head, run the SAME two-phase
    /// policy the live server runs (route before body, per-route limit), then
    /// dispatch. There is no streaming in tests, so the body limit is a length
    /// check on the already-buffered bytes — the equivalent of `Limited` over
    /// a socket. This keeps 404-before-read and per-route 413 honest in tests.
    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<Bytes>,
        content_type: Option<&str>,
        headers: &[(&str, &str)],
    ) -> TestResponse {
        let mut builder = http::Request::builder().method(method).uri(path);
        if let Some(ct) = content_type {
            builder = builder.header(header::CONTENT_TYPE, ct);
        }
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let req = builder.body(()).expect("test request build");
        let (parts, ()) = req.into_parts();
        let body = body.unwrap_or_default();

        // Phase 1: route on the head alone — a reject answers without reading the body.
        let (limit, stream) = match self.built.route_policy(&parts) {
            Policy::Reject(response) => return TestResponse::collect(response).await,
            Policy::Route { limit, stream } => (limit, stream),
        };
        // Phase 2: stream routes get a REAL stream lane — frames + the route's
        // `Limited` cap inside it, exactly like the live socket path, so the
        // cumulative cap (and frame straddling) are honest in tests too. The
        // buffered path keeps its upfront length check.
        let lane = if stream {
            crate::extract::BodyLane::Stream(Some(test_stream_lane(body, limit)))
        } else {
            if body.len() > limit {
                let mut response = Error::payload_too_large().into_response();
                if self.built.security_headers {
                    crate::app::apply_security_headers(&mut response);
                }
                return TestResponse::collect(response).await;
            }
            crate::extract::BodyLane::Buffered(body)
        };
        TestResponse::collect(self.built.dispatch(parts, lane).await).await
    }
}

/// A test-only stream lane: chop the buffered body into 13-byte frames so every
/// test on a stream route exercises frame straddling for free, then wrap in the
/// route's `Limited` cap so the cumulative cap trips in-process exactly as it
/// does over a socket.
fn test_stream_lane(body: Bytes, limit: usize) -> crate::extract::StreamLane {
    struct Frames(std::collections::VecDeque<Bytes>);
    impl http_body::Body for Frames {
        type Data = Bytes;
        type Error = Box<dyn std::error::Error + Send + Sync>;
        fn poll_frame(
            mut self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Result<http_body::Frame<Bytes>, Self::Error>>> {
            std::task::Poll::Ready(self.0.pop_front().map(|b| Ok(http_body::Frame::data(b))))
        }
    }
    let frames = body.chunks(13).map(Bytes::copy_from_slice).collect();
    // `Limited<Frames>::Error` is already `Box<dyn Error + Send + Sync>` (Frames'
    // error type), so the cap maps straight into the lane's error channel.
    let limited = http_body_util::Limited::new(Frames(frames), limit);
    http_body_util::combinators::UnsyncBoxBody::new(limited)
}

pub struct TestResponse {
    status: StatusCode,
    headers: http::HeaderMap,
    body: Bytes,
}

impl TestResponse {
    async fn collect(res: Response) -> Self {
        let (parts, body) = res.into_parts();
        let body = body
            .collect()
            // A buffered body cannot fail; a streaming one can fail mid-stream.
            .await
            .unwrap_or_else(|e| panic!("response body failed mid-stream: {e}"))
            .to_bytes();
        Self {
            status: parts.status,
            headers: parts.headers,
            body,
        }
    }

    pub fn status(&self) -> StatusCode {
        self.status
    }
    pub fn headers(&self) -> &http::HeaderMap {
        &self.headers
    }
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// Deserialize the JSON body, with a readable panic on mismatch.
    pub fn json<T: DeserializeOwned>(&self) -> T {
        serde_json::from_slice(&self.body).unwrap_or_else(|e| {
            panic!(
                "response body is not the expected JSON shape: {e}\nbody: {}",
                self.text()
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::prelude::*;

    /// `into_test` injects the SAME clock `TestApp::clock()` returns: advancing
    /// the handle must be visible to a dep resolved through a task context —
    /// the path background jobs use to read domain time.
    #[tokio::test]
    async fn test_clock_handle_drives_resolved_clock_in_task_context() {
        let t = App::new().into_test();
        let mut ctx = t.task_context();
        let resolved = ctx.resolve::<Clock>().await.unwrap();
        let before = resolved.now();
        t.clock().advance(std::time::Duration::from_secs(60));
        assert_eq!(
            resolved.now().duration_since(before).unwrap(),
            std::time::Duration::from_secs(60),
            "TestApp::clock() and the resolved Clock share one offset",
        );
    }
}
