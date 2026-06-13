//! The rate-limit middleware: identity partitioning, OPTIONS exemption,
//! per-request Clock resolution, and a 429 + Retry-After on over-limit. Fails
//! OPEN when there is no identity or no clock — a misconfigured limiter must
//! never break all traffic.

use crate::RateLimit;
use jerrycan_core::{Clock, Error, IntoResponse, Middleware, MiddlewareFuture, Next, RequestCtx};
use std::time::SystemTime;

pub(crate) struct RateLimitMw {
    cfg: RateLimit,
}

impl RateLimitMw {
    pub(crate) fn new(cfg: RateLimit) -> Self {
        Self { cfg }
    }

    /// api-key header (opt-in) → user-key closure → IP. First that yields a key
    /// wins; the tier prefix prevents cross-tier collisions. None ⇒ no identity ⇒
    /// fail open. The api-key tier is gated on opt-in because an unauthenticated,
    /// client-rotatable header would let a caller mint a fresh budget per request.
    fn partition_key(&self, ctx: &RequestCtx) -> Option<String> {
        if let Some(header) = self.cfg.api_key_header_ref()
            && let Some(v) = ctx
                .headers()
                .get(header)
                .and_then(|v| v.to_str().ok())
                .filter(|s| !s.is_empty())
        {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            v.hash(&mut h);
            // Hash so the raw secret isn't held in the window map (this is
            // partitioning, not authentication — the hash is non-cryptographic
            // and rare collisions just share a counter, never bypass the limit).
            return Some(format!("apikey:{:016x}", h.finish()));
        }
        if let Some(uk) = self.cfg.user_key_ref()
            && let Some(u) = uk(ctx)
        {
            return Some(format!("user:{u}"));
        }
        let ip = if self.cfg.trusts_forwarded_for() {
            // An empty / whitespace XFF entry must fall back to the socket peer,
            // not collapse every such request into one shared `ip:` bucket.
            ctx.headers()
                .get("x-forwarded-for")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.split(',').next())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        } else {
            None
        }
        .or_else(|| ctx.peer_addr().map(|a| a.ip().to_string()));
        ip.map(|ip| format!("ip:{ip}"))
    }
}

impl Middleware for RateLimitMw {
    fn handle<'a>(&'a self, ctx: &'a mut RequestCtx, next: Next<'a>) -> MiddlewareFuture<'a> {
        Box::pin(async move {
            if ctx.method() == http::Method::OPTIONS {
                return next.run(&mut *ctx).await; // OPTIONS exempt (CORS preflight etc.)
            }
            let Some(key) = self.partition_key(ctx) else {
                return next.run(&mut *ctx).await; // no identity → fail open
            };
            // Resolve the Clock PER REQUEST — `into_test` swaps the clock AFTER
            // build, so a cached Arc<Clock> would never see the test's advance.
            let now: SystemTime = match ctx.resolve::<Clock>().await {
                Ok(clock) => clock.now(),
                Err(_) => return next.run(&mut *ctx).await, // no clock → fail open
            };
            match self
                .cfg
                .store_ref()
                .hit(&key, self.cfg.window(), self.cfg.limit(), now)
                .await
            {
                Ok(out) if out.allowed => next.run(&mut *ctx).await,
                Ok(out) => {
                    let mut resp = Error::too_many_requests().into_response();
                    let retry = out.retry_after.as_secs().max(1);
                    if let Ok(v) = http::HeaderValue::from_str(&retry.to_string()) {
                        resp.headers_mut().insert(http::header::RETRY_AFTER, v);
                    }
                    resp
                }
                Err(e) => e.into_response(), // store failure → surface (e.g. JC0500)
            }
        })
    }
}
