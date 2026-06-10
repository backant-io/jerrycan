//! Observability for jerrycan: request IDs, structured JSON access logs,
//! /healthz, and a Prometheus /metrics endpoint. #![forbid(unsafe_code)].
#![forbid(unsafe_code)]

use jerrycan_core::{App, Extension, IntoResponse, Response, get};
use std::sync::Arc;

pub mod access_log;
pub mod metrics;

pub use metrics::Metrics;

/// The observability extension: app-wide access-log middleware + health/metrics
/// routes sharing one metrics registry.
pub struct Observe {
    metrics: Arc<Metrics>,
}

impl Observe {
    pub fn new() -> Self {
        Self {
            metrics: Arc::new(Metrics::new()),
        }
    }
}

impl Default for Observe {
    fn default() -> Self {
        Self::new()
    }
}

impl Extension for Observe {
    fn register(self, app: App) -> App {
        let metrics_for_mw = self.metrics.clone();
        let metrics_for_route = self.metrics.clone();
        app.middleware(access_log::AccessLog {
            metrics: metrics_for_mw,
        })
        .route("/healthz", get(|| async { "ok" }))
        .route(
            "/metrics",
            get(move || {
                let metrics = metrics_for_route.clone();
                async move { prometheus_response(metrics.render()) }
            }),
        )
    }
}

/// Initialize JSON logging once (call from `main` before serving). Idempotent;
/// honors `RUST_LOG`. No-op if a global subscriber is already set.
pub fn init_logging() {
    use tracing_subscriber::{EnvFilter, fmt};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = fmt().json().with_env_filter(filter).try_init();
}

fn prometheus_response(body: String) -> Response {
    let mut response = body.into_response();
    response.headers_mut().insert(
        jerrycan_core::http::header::CONTENT_TYPE,
        jerrycan_core::http::HeaderValue::from_static("text/plain; version=0.0.4"),
    );
    response
}
