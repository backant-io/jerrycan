//! Request context and extractors (spec §4.1). Everything a handler needs is
//! visible in its signature; each parameter implements [`FromRequest`].

use crate::dep::DepResolver;
use crate::error::{Error, Result};
use crate::response::Json;
use bytes::Bytes;
use serde::de::DeserializeOwned;
use std::future::Future;

/// The mutable view of one in-flight request. Handlers receive extractors,
/// not this type; middleware and the DI resolver work through it.
pub struct RequestCtx {
    pub(crate) parts: http::request::Parts,
    pub(crate) body: Bytes,
    /// Path parameters captured by the router, in route order.
    pub(crate) params: Vec<(String, String)>,
    pub(crate) deps: DepResolver,
}

impl RequestCtx {
    pub(crate) fn new(parts: http::request::Parts, body: Bytes, deps: DepResolver) -> Self {
        Self {
            parts,
            body,
            params: Vec::new(),
            deps,
        }
    }

    pub fn method(&self) -> &http::Method {
        &self.parts.method
    }
    pub fn uri(&self) -> &http::Uri {
        &self.parts.uri
    }
    pub fn headers(&self) -> &http::HeaderMap {
        &self.parts.headers
    }
}

/// Types that can be produced from the request. Implemented by all extractors
/// and by `Dep<T>` (see `dep` module).
pub trait FromRequest: Sized + Send {
    fn from_request(ctx: &mut RequestCtx) -> impl Future<Output = Result<Self>> + Send;
}

/// Typed path parameter: `Path<i64>` grabs the first `{param}` in the route;
/// `Path<(A, B)>` / `Path<(A, B, C)>` grab two/three `{param}`s in route order.
/// Param types are the sealed [`PathParam`] set (integers, `String`, `bool`,
/// floats, `char`); custom newtypes are a contract-v1 candidate.
pub struct Path<T>(pub T);

mod sealed {
    pub trait Sealed {}
}

/// Types extractable from one path segment. Sealed closed set in v0: integers,
/// `String`, `bool`, floats, `char`. Open/custom param types (id newtypes) are a
/// contract-v1 candidate via serde-based extraction.
pub trait PathParam: sealed::Sealed + Sized + Send {
    fn parse_param(name: &str, raw: &str) -> Result<Self>;
}

macro_rules! impl_path_param {
    ($($t:ty),* $(,)?) => {$(
        impl sealed::Sealed for $t {}
        impl PathParam for $t {
            fn parse_param(name: &str, raw: &str) -> Result<Self> {
                raw.parse::<$t>().map_err(|e| {
                    Error::bad_request(format!("invalid path parameter `{name}`: {e}"))
                })
            }
        }
    )*};
}
impl_path_param!(
    i8, i16, i32, i64, i128, isize, u8, u16, u32, u64, u128, usize, f32, f64, bool, char, String,
);

impl<T: PathParam> FromRequest for Path<T> {
    async fn from_request(ctx: &mut RequestCtx) -> Result<Self> {
        let (name, raw) = ctx
            .params
            .first()
            .ok_or_else(|| Error::internal("route has no path parameters"))?;
        T::parse_param(name, raw).map(Path)
    }
}

impl<A: PathParam, B: PathParam> FromRequest for Path<(A, B)> {
    async fn from_request(ctx: &mut RequestCtx) -> Result<Self> {
        let [a, b] = take_params::<2>(ctx)?;
        Ok(Path((
            A::parse_param(&a.0, &a.1)?,
            B::parse_param(&b.0, &b.1)?,
        )))
    }
}

impl<A: PathParam, B: PathParam, C: PathParam> FromRequest for Path<(A, B, C)> {
    async fn from_request(ctx: &mut RequestCtx) -> Result<Self> {
        let [a, b, c] = take_params::<3>(ctx)?;
        Ok(Path((
            A::parse_param(&a.0, &a.1)?,
            B::parse_param(&b.0, &b.1)?,
            C::parse_param(&c.0, &c.1)?,
        )))
    }
}

/// First N captured params, cloned in route order. Fewer than N is a routing
/// bug (the route declared fewer `{params}` than the handler expects) — 500.
fn take_params<const N: usize>(ctx: &RequestCtx) -> Result<[(String, String); N]> {
    if ctx.params.len() < N {
        return Err(Error::internal(format!(
            "route captures {} path parameter(s) but the handler expects {N}",
            ctx.params.len()
        )));
    }
    Ok(std::array::from_fn(|i| ctx.params[i].clone()))
}

/// Typed query string: `Query<MyParams>` via serde.
pub struct Query<T>(pub T);

impl<T: DeserializeOwned + Send> FromRequest for Query<T> {
    async fn from_request(ctx: &mut RequestCtx) -> Result<Self> {
        let q = ctx.parts.uri.query().unwrap_or("");
        serde_urlencoded::from_str::<T>(q)
            .map(Query)
            .map_err(|e| Error::bad_request(format!("invalid query string: {e}")))
    }
}

impl<T: DeserializeOwned + Send> FromRequest for Json<T> {
    async fn from_request(ctx: &mut RequestCtx) -> Result<Self> {
        serde_json::from_slice::<T>(&ctx.body)
            .map(Json)
            .map_err(|e| Error::unprocessable(format!("invalid JSON body: {e}")))
    }
}

/// Read-only access to request headers in a handler signature.
pub struct Headers(pub(crate) http::HeaderMap);

impl Headers {
    /// Header value as a &str, or None if absent or non-ASCII.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).and_then(|v| v.to_str().ok())
    }
}

impl FromRequest for Headers {
    async fn from_request(ctx: &mut RequestCtx) -> Result<Self> {
        Ok(Headers(ctx.headers().clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dep::DepEnv;
    use std::sync::Arc;

    fn ctx(uri: &str, body: &str) -> RequestCtx {
        let req = http::Request::builder()
            .method(http::Method::GET)
            .uri(uri)
            .body(())
            .unwrap();
        let (parts, ()) = req.into_parts();
        RequestCtx::new(
            parts,
            Bytes::from(body.to_string()),
            DepResolver::new(Arc::new(DepEnv::default()), Default::default()),
        )
    }

    #[tokio::test]
    async fn path_extracts_typed_param() {
        let mut c = ctx("/todos/42", "");
        c.params.push(("id".into(), "42".into()));
        let Path(id): Path<i64> = Path::<i64>::from_request(&mut c).await.unwrap();
        assert_eq!(id, 42);
    }

    #[tokio::test]
    async fn path_with_wrong_type_is_400() {
        let mut c = ctx("/todos/abc", "");
        c.params.push(("id".into(), "abc".into()));
        let err = Path::<i64>::from_request(&mut c).await.err().unwrap();
        assert_eq!(err.code(), "JC0400");
    }

    #[tokio::test]
    async fn path_missing_param_is_500() {
        // No params captured by the router → internal error (route declared a param
        // the trie never filled), surfaced as JC0500.
        let mut c = ctx("/todos", "");
        let err = Path::<i64>::from_request(&mut c).await.err().unwrap();
        assert_eq!(err.code(), "JC0500");
    }

    #[tokio::test]
    async fn query_deserializes_struct() {
        #[derive(serde::Deserialize)]
        struct Page {
            limit: u32,
            offset: u32,
        }
        let mut c = ctx("/todos?limit=10&offset=20", "");
        let Query(p): Query<Page> = Query::from_request(&mut c).await.unwrap();
        assert_eq!((p.limit, p.offset), (10, 20));
    }

    #[tokio::test]
    async fn json_body_deserializes_and_bad_json_is_422() {
        #[derive(serde::Deserialize)]
        struct NewTodo {
            title: String,
        }
        let mut c = ctx("/todos", r#"{"title":"x"}"#);
        let Json(t): Json<NewTodo> = Json::from_request(&mut c).await.unwrap();
        assert_eq!(t.title, "x");

        let mut bad = ctx("/todos", r#"{"title":"#);
        let err = Json::<NewTodo>::from_request(&mut bad).await.err().unwrap();
        assert_eq!(err.code(), "JC0422");
    }
}
