# Observability

## Purpose
`jerrycan::observe` adds request IDs, structured JSON access logs, a `/healthz`
liveness route, and a Prometheus `/metrics` endpoint. Enable with the design
dependency `"observe"` (or `jerrycan add observe`).

## Signature
```rust
# use jerrycan::prelude::*;
use jerrycan::observe::Observe;

# fn build() -> App {
App::new().extend(Observe::new())   // adds the access-log middleware + /healthz + /metrics
# }
# let _ = build();
```

## Minimal example
```rust
# use jerrycan::prelude::*;
# use jerrycan::observe::Observe;
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
let t = App::new().extend(Observe::new()).route("/x", get(|| async { "x" })).into_test();
let res = t.get("/x").await;
assert!(res.headers().get("x-request-id").is_some());           // every response stamped
assert_eq!(t.get("/healthz").await.text(), "ok");
assert!(t.get("/metrics").await.text().contains("jerrycan_requests_total"));
# }); }
```

## Variations
- Call `jerrycan::observe::init_logging()` once in `main` for JSON logs to
  stdout (honors `RUST_LOG`). Generated apps do this automatically.
- Scrape `/metrics` with Prometheus; `/healthz` for k8s liveness/readiness
  (the generated k8s manifests already point probes at it).

## Errors you'll hit
- `/healthz` and `/metrics` are RESERVED when observe is on — defining design
  routes at those paths is a build-time conflict (fail loud).

## Anti-patterns
- Don't roll your own request-id middleware alongside Observe — it owns the
  `x-request-id` header and the access log.
