//! A tiny global metrics registry rendered as Prometheus text. No histogram
//! crate: counters are atomics, latency is fixed-bucket counters.

use std::sync::atomic::{AtomicU64, Ordering};

/// Fixed Prometheus latency buckets (seconds), cumulative on render.
const BUCKETS: [f64; 8] = [0.005, 0.010, 0.025, 0.050, 0.100, 0.250, 0.500, 1.000];

/// Process-wide metrics. Cheap atomics; status counters keyed by a small set of
/// common codes plus an "other" sink (avoids unbounded label cardinality).
pub struct Metrics {
    status_2xx: AtomicU64,
    status_4xx: AtomicU64,
    status_5xx: AtomicU64,
    status_other: AtomicU64,
    in_flight: AtomicU64,
    duration_count: AtomicU64,
    duration_sum_micros: AtomicU64,
    buckets: [AtomicU64; 8],
}

impl Metrics {
    pub fn new() -> Self {
        Self {
            status_2xx: AtomicU64::new(0),
            status_4xx: AtomicU64::new(0),
            status_5xx: AtomicU64::new(0),
            status_other: AtomicU64::new(0),
            in_flight: AtomicU64::new(0),
            duration_count: AtomicU64::new(0),
            duration_sum_micros: AtomicU64::new(0),
            buckets: Default::default(),
        }
    }

    /// Record one finished request.
    pub fn record(&self, status: u16, seconds: f64) {
        let class = match status {
            200..=299 => &self.status_2xx,
            400..=499 => &self.status_4xx,
            500..=599 => &self.status_5xx,
            _ => &self.status_other,
        };
        class.fetch_add(1, Ordering::Relaxed);
        self.duration_count.fetch_add(1, Ordering::Relaxed);
        self.duration_sum_micros
            .fetch_add((seconds * 1_000_000.0) as u64, Ordering::Relaxed);
        for (i, edge) in BUCKETS.iter().enumerate() {
            if seconds <= *edge {
                self.buckets[i].fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// RAII guard for the in-flight gauge.
    pub fn in_flight_guard(&self) -> InFlightGuard<'_> {
        self.in_flight.fetch_add(1, Ordering::Relaxed);
        InFlightGuard { metrics: self }
    }

    /// Render the current state as Prometheus exposition text.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let total_2xx = self.status_2xx.load(Ordering::Relaxed);
        let total_4xx = self.status_4xx.load(Ordering::Relaxed);
        let total_5xx = self.status_5xx.load(Ordering::Relaxed);
        let total_other = self.status_other.load(Ordering::Relaxed);
        let total = self.duration_count.load(Ordering::Relaxed);
        out.push_str("# TYPE jerrycan_requests_total counter\n");
        out.push_str(&format!(
            "jerrycan_requests_total{{status=\"2xx\"}} {total_2xx}\n"
        ));
        out.push_str(&format!(
            "jerrycan_requests_total{{status=\"4xx\"}} {total_4xx}\n"
        ));
        out.push_str(&format!(
            "jerrycan_requests_total{{status=\"5xx\"}} {total_5xx}\n"
        ));
        out.push_str(&format!(
            "jerrycan_requests_total{{status=\"other\"}} {total_other}\n"
        ));
        out.push_str("# TYPE jerrycan_requests_in_flight gauge\n");
        out.push_str(&format!(
            "jerrycan_requests_in_flight {}\n",
            self.in_flight.load(Ordering::Relaxed)
        ));
        out.push_str("# TYPE jerrycan_request_duration_seconds histogram\n");
        for (i, edge) in BUCKETS.iter().enumerate() {
            // Each bucket is already cumulative by construction: every observation
            // increments every bucket whose edge >= the observed duration.
            let cumulative = self.buckets[i].load(Ordering::Relaxed);
            out.push_str(&format!(
                "jerrycan_request_duration_seconds_bucket{{le=\"{edge}\"}} {cumulative}\n"
            ));
        }
        out.push_str(&format!(
            "jerrycan_request_duration_seconds_bucket{{le=\"+Inf\"}} {total}\n"
        ));
        let sum = self.duration_sum_micros.load(Ordering::Relaxed) as f64 / 1_000_000.0;
        out.push_str(&format!("jerrycan_request_duration_seconds_sum {sum}\n"));
        out.push_str(&format!(
            "jerrycan_request_duration_seconds_count {total}\n"
        ));
        out
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

/// Decrements the in-flight gauge on drop.
pub struct InFlightGuard<'a> {
    metrics: &'a Metrics,
}

impl Drop for InFlightGuard<'_> {
    fn drop(&mut self) {
        self.metrics.in_flight.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_render_as_prometheus_text() {
        let m = Metrics::new();
        m.record(200, 0.003);
        m.record(200, 0.020);
        m.record(404, 0.001);
        let text = m.render();
        assert!(text.contains("# TYPE jerrycan_requests_total counter"));
        assert!(
            text.contains(r#"jerrycan_requests_total{status="2xx"} 2"#),
            "{text}"
        );
        assert!(
            text.contains(r#"jerrycan_requests_total{status="4xx"} 1"#),
            "{text}"
        );
        assert!(text.contains("# TYPE jerrycan_request_duration_seconds histogram"));
        // 0.003 and 0.020 fall in le="0.005"? no (0.020 > 0.005) — cumulative buckets:
        assert!(
            text.contains(r#"jerrycan_request_duration_seconds_bucket{le="0.005"} 2"#),
            "0.003 + 0.001 ≤ 5ms: {text}"
        );
        assert!(
            text.contains(r#"jerrycan_request_duration_seconds_bucket{le="+Inf"} 3"#),
            "{text}"
        );
        assert!(text.contains("jerrycan_request_duration_seconds_count 3"));
    }

    #[test]
    fn in_flight_tracks_concurrency() {
        let m = Metrics::new();
        let g = m.in_flight_guard();
        assert!(m.render().contains("jerrycan_requests_in_flight 1"));
        drop(g);
        assert!(m.render().contains("jerrycan_requests_in_flight 0"));
    }
}
