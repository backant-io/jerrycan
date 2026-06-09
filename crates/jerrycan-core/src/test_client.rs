//! In-memory test client (spec §4.1 "Test client"): no sockets, no network.
//! `override_dep` is THE testing seam — fake any dependency, run real requests.

use crate::app::{App, BuiltApp};
use crate::response::Response;
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

    async fn request(&self, method: Method, path: &str, json: Option<Vec<u8>>) -> TestResponse {
        let mut builder = http::Request::builder().method(method).uri(path);
        if json.is_some() {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
        }
        let req = builder.body(()).expect("test request build");
        let (parts, ()) = req.into_parts();
        let body = Bytes::from(json.unwrap_or_default());
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
            // Full<Bytes>'s Body::Error is Infallible — collecting cannot fail.
            .await
            .expect("collect response body (Full<Bytes> is infallible)")
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
