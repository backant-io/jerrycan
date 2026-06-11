//! In-memory test client (spec §4.1 "Test client"): no sockets, no network.
//! `override_dep` is THE testing seam — fake any dependency, run real requests.
//!
//! Panics in handlers propagate in tests by design — the serve path converts them to 500 JC0500.

use crate::App;
use crate::app::{BuiltApp, Policy};
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
    pub fn into_test(self) -> TestApp {
        TestApp {
            built: self.build().expect("app failed to build"),
        }
    }
}

pub struct TestApp {
    built: BuiltApp,
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

    /// A [`TaskContext`](crate::TaskContext) for resolving app-level dependencies
    /// outside a request, honoring any `override_dep` fakes set on this `TestApp`.
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
        let limit = match self.built.route_policy(&parts) {
            Policy::Reject(response) => return TestResponse::collect(response).await,
            Policy::Route { limit } => limit,
        };
        // Phase 2 (no streaming in tests): the per-route limit is a length check.
        if body.len() > limit {
            let mut response = Error::payload_too_large().into_response();
            if self.built.security_headers {
                crate::app::apply_security_headers(&mut response);
            }
            return TestResponse::collect(response).await;
        }
        TestResponse::collect(self.built.dispatch(parts, body).await).await
    }
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
            // JcBody's Body::Error is Infallible — collecting cannot fail.
            .await
            .expect("collect response body (JcBody is infallible)")
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
