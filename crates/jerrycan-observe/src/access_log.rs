//! Request-id + access logging middleware, and the Observe extension wiring
//! /healthz and /metrics. Logging output is structured JSON via tracing; tests
//! assert behavior (header + metrics endpoints), not log lines.

use crate::metrics::Metrics;
use jerrycan_core::{Middleware, MiddlewareFuture, Next, RequestCtx};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Monotonic request-id source (process-local; pair with the hostname/pod for
/// global uniqueness at the log-aggregation layer).
static REQUEST_SEQ: AtomicU64 = AtomicU64::new(1);

fn next_request_id() -> String {
    let n = REQUEST_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("req-{n:016x}")
}

/// Middleware: assigns a request id, times the request, records metrics, emits
/// one structured access-log line, and stamps `x-request-id` on the response.
pub struct AccessLog {
    pub(crate) metrics: Arc<Metrics>,
}

impl Middleware for AccessLog {
    fn handle<'a>(&'a self, ctx: &'a mut RequestCtx, next: Next<'a>) -> MiddlewareFuture<'a> {
        Box::pin(async move {
            let request_id = next_request_id();
            let method = ctx.method().to_string();
            let path = ctx.uri().path().to_string();
            let _guard = self.metrics.in_flight_guard();
            let started = std::time::Instant::now();

            let mut response = next.run(&mut *ctx).await;

            let elapsed = started.elapsed().as_secs_f64();
            let status = response.status().as_u16();
            self.metrics.record(status, elapsed);
            tracing::info!(
                target: "jerrycan::access",
                request_id = %request_id,
                method = %method,
                path = %path,
                status = status,
                duration_ms = elapsed * 1000.0,
                "request"
            );
            if let Ok(value) = jerrycan_core::http::HeaderValue::from_str(&request_id) {
                response.headers_mut().insert(
                    jerrycan_core::http::HeaderName::from_static("x-request-id"),
                    value,
                );
            }
            response
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::Observe;
    use jerrycan_core::{App, get};

    #[tokio::test]
    async fn responses_carry_a_request_id_header() {
        let t = App::new()
            .extend(Observe::new())
            .route("/x", get(|| async { "x" }))
            .into_test();
        let res = t.get("/x").await;
        let id = res
            .headers()
            .get("x-request-id")
            .expect("x-request-id present");
        assert!(!id.to_str().unwrap().is_empty());
    }

    #[tokio::test]
    async fn healthz_and_metrics_are_served() {
        let t = App::new()
            .extend(Observe::new())
            .route("/x", get(|| async { "x" }))
            .into_test();
        assert_eq!(t.get("/healthz").await.text(), "ok");

        // Drive one request so a counter is non-zero, then scrape.
        let _ = t.get("/x").await;
        let metrics = t.get("/metrics").await;
        assert_eq!(
            metrics.headers()["content-type"],
            "text/plain; version=0.0.4"
        );
        assert!(
            metrics.text().contains("jerrycan_requests_total"),
            "{}",
            metrics.text()
        );
    }
}
