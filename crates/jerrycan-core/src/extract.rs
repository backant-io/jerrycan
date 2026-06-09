//! Request context and extractors (spec §4.1). Everything a handler needs is
//! visible in its signature; each parameter implements [`FromRequest`].

use crate::dep::DepResolver;
use crate::error::{Error, Result};
use crate::response::Json;
use bytes::Bytes;
use serde::de::DeserializeOwned;
use std::future::Future;
use std::str::FromStr;

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

/// Typed path parameter: `Path<i64>` grabs the first `{param}` in the route.
/// (Multi-param `Path<(A, B)>` lands in Phase 1; one param per route segment
/// covers the spike and the docs say so.)
pub struct Path<T>(pub T);

impl<T> FromRequest for Path<T>
where
    T: FromStr + Send,
    T::Err: std::fmt::Display,
{
    async fn from_request(ctx: &mut RequestCtx) -> Result<Self> {
        let (name, raw) = ctx
            .params
            .first()
            .ok_or_else(|| Error::internal("route has no path parameters"))?;
        raw.parse::<T>()
            .map(Path)
            .map_err(|e| Error::bad_request(format!("invalid path parameter `{name}`: {e}")))
    }
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
        let Path(id): Path<i64> = Path::from_request(&mut c).await.unwrap();
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
