//! Handler abstraction (spec §4.1): a handler is a plain async fn whose
//! parameters implement [`FromRequest`] and whose return implements
//! [`IntoResponse`]. Extraction failures short-circuit into error responses.

use crate::extract::{FromRequest, RequestCtx};
use crate::response::{IntoResponse, Response};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Type-erased handler as stored by the router.
pub(crate) type BoxHandlerFn = Arc<
    dyn for<'a> Fn(&'a mut RequestCtx) -> Pin<Box<dyn Future<Output = Response> + Send + 'a>>
        + Send
        + Sync,
>;

/// Implemented for async fns of arity 0..=8 over [`FromRequest`] parameters.
pub trait Handler<Args>: Send + Sync + 'static {
    #[doc(hidden)]
    fn into_handler_fn(self) -> BoxHandlerFn;
}

macro_rules! impl_handler {
    ($($A:ident),*) => {
        impl<F, Fut, R, $($A,)*> Handler<($($A,)*)> for F
        where
            F: Fn($($A),*) -> Fut + Clone + Send + Sync + 'static,
            Fut: Future<Output = R> + Send,
            R: IntoResponse,
            $($A: FromRequest + 'static,)*
        {
            fn into_handler_fn(self) -> BoxHandlerFn {
                #[allow(unused_variables)]
                Arc::new(move |ctx: &mut RequestCtx| {
                    let f = self.clone();
                    Box::pin(async move {
                        #[allow(non_snake_case, unused_variables)]
                        {
                            $(
                                let $A = match <$A as FromRequest>::from_request(ctx).await {
                                    Ok(v) => v,
                                    Err(e) => return e.into_response(),
                                };
                            )*
                            f($($A),*).await.into_response()
                        }
                    })
                })
            }
        }
    };
}

impl_handler!();
impl_handler!(A1);
impl_handler!(A1, A2);
impl_handler!(A1, A2, A3);
impl_handler!(A1, A2, A3, A4);
impl_handler!(A1, A2, A3, A4, A5);
impl_handler!(A1, A2, A3, A4, A5, A6);
impl_handler!(A1, A2, A3, A4, A5, A6, A7);
impl_handler!(A1, A2, A3, A4, A5, A6, A7, A8);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Path;
    use crate::dep::{Dep, DepEnv, DepResolver};
    use crate::response::Json;
    use std::collections::HashMap;

    struct Greeting {
        word: &'static str,
    }

    async fn greet(g: Dep<Greeting>, Path(id): Path<u32>) -> crate::Result<Json<String>> {
        Ok(Json(format!("{} #{id}", g.word)))
    }

    #[tokio::test]
    async fn handler_extracts_runs_and_responds() {
        let mut env = DepEnv::default();
        env.insert_value(Greeting { word: "hi" });
        let req = http::Request::builder().uri("/greet/7").body(()).unwrap();
        let (parts, ()) = req.into_parts();
        let mut ctx = RequestCtx::new(
            parts,
            bytes::Bytes::new(),
            DepResolver::new(
                std::sync::Arc::new(env),
                std::sync::Arc::new(HashMap::new()),
            ),
        );
        ctx.params.push(("id".into(), "7".into()));

        let h = greet.into_handler_fn();
        let res = (*h)(&mut ctx).await; // explicit deref: Arc<dyn Fn> is not directly callable
        assert_eq!(res.status(), http::StatusCode::OK);
    }

    #[tokio::test]
    async fn extraction_failure_short_circuits_to_error_response() {
        // No Greeting provider registered → Dep extraction fails → JC1001 → 500.
        let req = http::Request::builder().uri("/greet/7").body(()).unwrap();
        let (parts, ()) = req.into_parts();
        let mut ctx = RequestCtx::new(
            parts,
            bytes::Bytes::new(),
            DepResolver::new(Default::default(), std::sync::Arc::new(HashMap::new())),
        );
        ctx.params.push(("id".into(), "7".into()));

        let h = greet.into_handler_fn();
        let res = (*h)(&mut ctx).await;
        assert_eq!(res.status(), http::StatusCode::INTERNAL_SERVER_ERROR);
    }
}
