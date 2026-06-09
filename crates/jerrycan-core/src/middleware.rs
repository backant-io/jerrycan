//! Middleware (spec §4.1): `async fn handle(&self, ctx, next)`. Composable,
//! ordering explicit, no tower, no magic.

use crate::extract::RequestCtx;
use crate::handler::BoxHandlerFn;
use crate::response::Response;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Boxed future returned by middleware. The lifetime ties to the request.
pub type MiddlewareFuture<'a> = Pin<Box<dyn Future<Output = Response> + Send + 'a>>;

/// Wraps request handling. Call `next.run(&mut *ctx).await` to continue;
/// return early to short-circuit (auth rejections, rate limits, …).
pub trait Middleware: Send + Sync + 'static {
    fn handle<'a>(&'a self, ctx: &'a mut RequestCtx, next: Next<'a>) -> MiddlewareFuture<'a>;
}

/// The remainder of the middleware chain plus the endpoint handler.
pub struct Next<'a> {
    pub(crate) chain: &'a [Arc<dyn Middleware>],
    pub(crate) endpoint: &'a BoxHandlerFn,
}

impl<'a> Next<'a> {
    /// Run the rest of the chain. Takes a reborrow (`&mut *ctx`) so the caller
    /// keeps using `ctx` after awaiting.
    pub fn run<'b>(self, ctx: &'b mut RequestCtx) -> MiddlewareFuture<'b>
    where
        'a: 'b,
    {
        let Next { chain, endpoint } = self;
        match chain.split_first() {
            Some((head, rest)) => head.handle(
                ctx,
                Next {
                    chain: rest,
                    endpoint,
                },
            ),
            None => (**endpoint)(ctx), // &BoxHandlerFn → Arc<dyn Fn> → dyn Fn: explicit double deref
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dep::{DepEnv, DepResolver};
    use crate::response::IntoResponse;
    use std::collections::HashMap;
    use std::sync::Mutex;

    type Log = Arc<Mutex<Vec<&'static str>>>;

    struct Tag {
        name_in: &'static str,
        name_out: &'static str,
        log: Log,
    }

    impl Middleware for Tag {
        fn handle<'a>(&'a self, ctx: &'a mut RequestCtx, next: Next<'a>) -> MiddlewareFuture<'a> {
            Box::pin(async move {
                self.log.lock().unwrap().push(self.name_in);
                let res = next.run(&mut *ctx).await;
                self.log.lock().unwrap().push(self.name_out);
                res
            })
        }
    }

    #[tokio::test]
    async fn chain_runs_outside_in_then_inside_out() {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let l = log.clone();
        let endpoint: BoxHandlerFn = Arc::new(move |_ctx: &mut RequestCtx| {
            let l = l.clone();
            Box::pin(async move {
                l.lock().unwrap().push("handler");
                "ok".into_response()
            })
        });

        let chain: Vec<Arc<dyn Middleware>> = vec![
            Arc::new(Tag {
                name_in: "outer-in",
                name_out: "outer-out",
                log: log.clone(),
            }),
            Arc::new(Tag {
                name_in: "inner-in",
                name_out: "inner-out",
                log: log.clone(),
            }),
        ];

        let req = http::Request::builder().uri("/").body(()).unwrap();
        let (parts, ()) = req.into_parts();
        let mut ctx = RequestCtx::new(
            parts,
            bytes::Bytes::new(),
            DepResolver::new(Arc::new(DepEnv::default()), Arc::new(HashMap::new())),
        );

        let res = Next {
            chain: &chain,
            endpoint: &endpoint,
        }
        .run(&mut ctx)
        .await;
        assert_eq!(res.status(), http::StatusCode::OK);
        assert_eq!(
            *log.lock().unwrap(),
            vec!["outer-in", "inner-in", "handler", "inner-out", "outer-out"]
        );
    }
}
