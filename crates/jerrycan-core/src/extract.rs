//! Request context and extractors (spec §4.1). Everything a handler needs is
//! visible in its signature; each parameter implements [`FromRequest`].

use crate::dep::DepResolver;
use crate::error::{Error, Result};
use crate::response::Json;
use bytes::Bytes;
use serde::de::DeserializeOwned;
use std::future::Future;

/// A live, incrementally-arriving request body: hyper's stream, pre-wrapped in
/// the route's cumulative `Limited` cap and the per-frame read deadline.
/// Unsync (hyper's body is not Sync); the lane lives inside one dispatch task.
pub(crate) type StreamLane =
    http_body_util::combinators::UnsyncBoxBody<Bytes, Box<dyn std::error::Error + Send + Sync>>;

/// How the request body reaches the context. Buffered routes collect the body
/// upfront (the v2.0b two-phase read); `.stream_body()` routes hand the live
/// hyper stream straight through as a [`BodyLane::Stream`].
pub(crate) enum BodyLane {
    Buffered(Bytes),
    /// `None` after a streaming consumer (Multipart, Task 7) took ownership.
    Stream(Option<StreamLane>),
}

/// The connection's remote socket address, threaded from the accept loop onto
/// `parts.extensions` so it survives into the handler. A newtype so the typemap
/// lookup is unambiguous. `None` for synthetic requests (tasks, some tests).
#[derive(Clone, Copy, Debug)]
pub struct ClientAddr(pub std::net::SocketAddr);

/// The mutable view of one in-flight request. Handlers receive extractors,
/// not this type; middleware and the DI resolver work through it.
pub struct RequestCtx {
    pub(crate) parts: http::request::Parts,
    pub(crate) body: BodyLane,
    /// Path parameters captured by the router, in route order.
    pub(crate) params: Vec<(String, String)>,
    pub(crate) deps: DepResolver,
    /// True only for a [`TaskContext`](crate::dep::TaskContext): resolution runs
    /// outside an HTTP request, so HTTP-coupled extractors reject with JC1003.
    pub(crate) is_task: bool,
}

impl RequestCtx {
    /// Buffered-lane constructor: the body is already fully collected. The
    /// convenience path used by the buffered dispatch route and every test
    /// helper that hands over pre-read bytes.
    pub(crate) fn new(parts: http::request::Parts, body: Bytes, deps: DepResolver) -> Self {
        Self::with_lane(parts, BodyLane::Buffered(body), deps)
    }

    /// Lane-taking constructor: the streaming dispatch route hands the live
    /// hyper stream lane straight through without buffering it upfront.
    pub(crate) fn with_lane(
        parts: http::request::Parts,
        body: BodyLane,
        deps: DepResolver,
    ) -> Self {
        Self {
            parts,
            body,
            params: Vec::new(),
            deps,
            is_task: false,
        }
    }

    /// The complete request body. Buffered lane: a cheap clone. Stream lane:
    /// drains the stream (the route's `Limited` cap and per-frame deadline are
    /// inside it) and CACHES the bytes, so repeated extractors keep working.
    pub(crate) async fn drain_body(&mut self) -> Result<Bytes> {
        match &mut self.body {
            BodyLane::Buffered(bytes) => Ok(bytes.clone()),
            BodyLane::Stream(slot) => {
                // A `None` slot means a streaming consumer (Multipart, Task 7) took the
                // lane and left it empty; a later drain on the same request lands here.
                // This 500 is the intended post-Multipart contract, not dead code.
                let stream = slot
                    .take()
                    .ok_or_else(|| Error::internal("request body was already consumed"))?;
                use http_body_util::BodyExt;
                let collected = stream.collect().await.map_err(map_stream_error)?;
                let bytes = collected.to_bytes();
                self.body = BodyLane::Buffered(bytes.clone());
                Ok(bytes)
            }
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

    /// The named path parameter captured by the router, if present. Unlike
    /// `Path<T>` (which binds the leaf-most param), a guard can address a specific
    /// mount param by name — e.g. the tenant fk `club_id` under `/clubs/{club_id}`.
    pub fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// The remote peer's socket address, if the transport provided one. Set by the
    /// serve loop from `accept()`; absent for task contexts and synthetic requests.
    /// Rate limiting uses the IP here as its last-resort partition key; treat it as
    /// the raw TCP peer (a proxy's address behind a load balancer).
    pub fn peer_addr(&self) -> Option<std::net::SocketAddr> {
        self.parts.extensions.get::<ClientAddr>().map(|c| c.0)
    }

    /// Remove a typed extension from the request parts. jerrycan-realtime takes
    /// hyper's `OnUpgrade` handle this way to run a WebSocket after replying 101.
    /// Remove-not-get: the handle is single-use and `!Clone`.
    pub fn take_extension<T: Send + Sync + 'static>(&mut self) -> Option<T> {
        self.parts.extensions.remove::<T>()
    }
}

/// Map a stream-lane read failure onto the stable codes: the route's
/// cumulative cap → 413, a frame that never arrived → 408 (same code the
/// buffered read path uses), anything else (client vanished mid-upload) → 400.
pub(crate) fn map_stream_error(e: Box<dyn std::error::Error + Send + Sync>) -> Error {
    if e.downcast_ref::<http_body_util::LengthLimitError>()
        .is_some()
    {
        return Error::payload_too_large();
    }
    if e.downcast_ref::<crate::serve::RecvTimeout>().is_some() {
        return Error::new(
            http::StatusCode::REQUEST_TIMEOUT,
            "JC0408",
            "timed out reading the request body",
        );
    }
    Error::bad_request("request body failed mid-read")
}

/// Types that can be produced from the request. Implemented by all extractors
/// and by `Dep<T>` (see `dep` module).
pub trait FromRequest: Sized + Send {
    fn from_request(ctx: &mut RequestCtx) -> impl Future<Output = Result<Self>> + Send;
}

/// Typed path parameter: `Path<i64>` binds the LEAF-MOST (last) captured
/// parameter; use a tuple to address all parameters root→leaf — `Path<(A, B)>` /
/// `Path<(A, B, C)>` grab two/three `{param}`s in route order. Param types are
/// the sealed [`PathParam`] set (integers, `String`, `bool`, floats, `char`);
/// custom newtypes opt in through the [`path_param!`](crate::path_param) macro.
pub struct Path<T>(pub T);

/// Crate-internal seal for [`PathParam`]. Hidden from docs, but `pub` so the
/// [`path_param!`](crate::path_param) macro can name it from outside this module
/// — the trait below stays the real gate, and `path_param!` is its sanctioned door.
#[doc(hidden)]
pub mod sealed {
    pub trait Sealed {}
}

/// Types extractable from one path segment. The built-in set (integers,
/// `String`, `bool`, floats, `char`) is sealed; custom param types (id newtypes)
/// join it through the [`path_param!`](crate::path_param) macro, which is the
/// only sanctioned way to implement this trait outside the crate.
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

/// Admit a custom newtype as a [`Path`] parameter. The type must implement
/// [`FromStr`](std::str::FromStr) with a `Display` error; a parse failure maps
/// to the same `JC0400` invalid-path-parameter error the built-in impls produce.
///
/// ```
/// # use jerrycan_core as jerrycan;
/// #[derive(Debug)]
/// struct LeadId(i64);
/// impl std::str::FromStr for LeadId {
///     type Err = std::num::ParseIntError;
///     fn from_str(s: &str) -> Result<Self, Self::Err> { Ok(LeadId(s.parse()?)) }
/// }
/// jerrycan::path_param!(LeadId);
/// ```
#[macro_export]
macro_rules! path_param {
    ($($t:ty),* $(,)?) => {$(
        impl $crate::extract::sealed::Sealed for $t {}
        impl $crate::extract::PathParam for $t {
            fn parse_param(name: &str, raw: &str) -> $crate::Result<Self> {
                raw.parse::<$t>().map_err(|e| {
                    $crate::Error::bad_request(format!("invalid path parameter `{name}`: {e}"))
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
        if ctx.is_task {
            return Err(Error::task_context());
        }
        // Binds the leaf-most (last) captured parameter, so a route mounted under
        // a param-carrying prefix (e.g. `/ws/{ws}` + `/leads/{id}`) addresses its
        // own `{id}` rather than the mount's `{ws}`. Tuples address all of them.
        let (name, raw) = ctx
            .params
            .last()
            .ok_or_else(|| Error::internal("route has no path parameters"))?;
        T::parse_param(name, raw).map(Path)
    }
}

impl<A: PathParam, B: PathParam> FromRequest for Path<(A, B)> {
    async fn from_request(ctx: &mut RequestCtx) -> Result<Self> {
        if ctx.is_task {
            return Err(Error::task_context());
        }
        let [a, b] = take_params::<2>(ctx)?;
        Ok(Path((
            A::parse_param(&a.0, &a.1)?,
            B::parse_param(&b.0, &b.1)?,
        )))
    }
}

impl<A: PathParam, B: PathParam, C: PathParam> FromRequest for Path<(A, B, C)> {
    async fn from_request(ctx: &mut RequestCtx) -> Result<Self> {
        if ctx.is_task {
            return Err(Error::task_context());
        }
        let [a, b, c] = take_params::<3>(ctx)?;
        Ok(Path((
            A::parse_param(&a.0, &a.1)?,
            B::parse_param(&b.0, &b.1)?,
            C::parse_param(&c.0, &c.1)?,
        )))
    }
}

impl<A: PathParam, B: PathParam, C: PathParam, D: PathParam> FromRequest for Path<(A, B, C, D)> {
    async fn from_request(ctx: &mut RequestCtx) -> Result<Self> {
        if ctx.is_task {
            return Err(Error::task_context());
        }
        let [a, b, c, d] = take_params::<4>(ctx)?;
        Ok(Path((
            A::parse_param(&a.0, &a.1)?,
            B::parse_param(&b.0, &b.1)?,
            C::parse_param(&c.0, &c.1)?,
            D::parse_param(&d.0, &d.1)?,
        )))
    }
}

impl<A: PathParam, B: PathParam, C: PathParam, D: PathParam, E: PathParam> FromRequest
    for Path<(A, B, C, D, E)>
{
    async fn from_request(ctx: &mut RequestCtx) -> Result<Self> {
        if ctx.is_task {
            return Err(Error::task_context());
        }
        let [a, b, c, d, e] = take_params::<5>(ctx)?;
        Ok(Path((
            A::parse_param(&a.0, &a.1)?,
            B::parse_param(&b.0, &b.1)?,
            C::parse_param(&c.0, &c.1)?,
            D::parse_param(&d.0, &d.1)?,
            E::parse_param(&e.0, &e.1)?,
        )))
    }
}

impl<A: PathParam, B: PathParam, C: PathParam, D: PathParam, E: PathParam, F: PathParam> FromRequest
    for Path<(A, B, C, D, E, F)>
{
    async fn from_request(ctx: &mut RequestCtx) -> Result<Self> {
        if ctx.is_task {
            return Err(Error::task_context());
        }
        let [a, b, c, d, e, f] = take_params::<6>(ctx)?;
        Ok(Path((
            A::parse_param(&a.0, &a.1)?,
            B::parse_param(&b.0, &b.1)?,
            C::parse_param(&c.0, &c.1)?,
            D::parse_param(&d.0, &d.1)?,
            E::parse_param(&e.0, &e.1)?,
            F::parse_param(&f.0, &f.1)?,
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

/// A by-name view of the request's captured path parameters. Where [`Path<T>`]
/// binds positionally (the leaf-most segment, or a root→leaf tuple), `PathParams`
/// reads a SPECIFIC mount param BY NAME — the accessor a DI factory needs, since
/// a factory resolves each argument through [`FromRequest`] and cannot borrow
/// `&RequestCtx` to call [`RequestCtx::param`]. The membership-verifying tenancy
/// guard uses it to read the tenant fk `club_id` under `/clubs/{club_id}` even
/// when a leaf `{id}` follows (issues #78/#79). Rejects a task context (JC1003),
/// like every other HTTP-coupled extractor.
pub struct PathParams(Vec<(String, String)>);

impl PathParams {
    /// The value of the named path parameter captured by the router, or `None`
    /// if this route captured no param by that name.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

impl FromRequest for PathParams {
    async fn from_request(ctx: &mut RequestCtx) -> Result<Self> {
        if ctx.is_task {
            return Err(Error::task_context());
        }
        Ok(PathParams(ctx.params.clone()))
    }
}

/// Typed query string: `Query<MyParams>` via serde.
pub struct Query<T>(pub T);

impl<T: DeserializeOwned + Send> FromRequest for Query<T> {
    async fn from_request(ctx: &mut RequestCtx) -> Result<Self> {
        if ctx.is_task {
            return Err(Error::task_context());
        }
        let q = ctx.parts.uri.query().unwrap_or("");
        serde_urlencoded::from_str::<T>(q)
            .map(Query)
            .map_err(|e| Error::bad_request(format!("invalid query string: {e}")))
    }
}

impl<T: DeserializeOwned + Send> FromRequest for Json<T> {
    async fn from_request(ctx: &mut RequestCtx) -> Result<Self> {
        if ctx.is_task {
            return Err(Error::task_context());
        }
        let body = ctx.drain_body().await?;
        serde_json::from_slice::<T>(&body)
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
        if ctx.is_task {
            return Err(Error::task_context());
        }
        Ok(Headers(ctx.headers().clone()))
    }
}

/// The request body as EXACT bytes — the extractor for webhook signature
/// verification, where the digest must cover the wire bytes, not a re-serialized
/// value. Works on buffered routes (cheap clone) and `stream_body()` routes
/// (drains and caches). See the auth docs for the Stripe/Twilio recipes.
pub struct RawBody(pub Bytes);

impl FromRequest for RawBody {
    async fn from_request(ctx: &mut RequestCtx) -> Result<Self> {
        if ctx.is_task {
            return Err(Error::task_context());
        }
        Ok(RawBody(ctx.drain_body().await?))
    }
}

/// Optional extraction: `Some` when the inner extractor succeeds, `None` on
/// ANY extraction failure. For genuinely optional inputs — the canonical use
/// is optional auth (`Option<CurrentUser>` on a route that also accepts a
/// signed URL). Do NOT use it to paper over malformed required input: the
/// failure reason is discarded by design — reach for `Result<T, Error>` when
/// the failure must stay observable.
impl<T: FromRequest> FromRequest for Option<T> {
    async fn from_request(ctx: &mut RequestCtx) -> Result<Self> {
        Ok(T::from_request(ctx).await.ok())
    }
}

/// Error-PRESERVING optional extraction: `Ok(v)` when the inner extractor
/// succeeds, `Err(e)` carrying ITS error when it fails — the extraction itself
/// never fails the request, so the handler decides. For routes that accept
/// more than one credential and must keep the guard's real status when the
/// fallback also misses (#109): a private tenant bucket's download takes
/// `Result<Dep<Tenant>, Error>` and, past the signed-URL branch, propagates
/// with `let tenant = tenant?;` — a missing session stays 401 and a
/// non-member's guard failure stays 403, instead of `Option<T>` collapsing
/// both into a rebound 401.
impl<T: FromRequest> FromRequest for Result<T, Error> {
    async fn from_request(ctx: &mut RequestCtx) -> Result<Self> {
        Ok(T::from_request(ctx).await)
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
    async fn peer_addr_is_none_without_a_socket_and_readable_when_set() {
        let mut c = ctx("/x", "");
        assert!(c.peer_addr().is_none());
        let addr: std::net::SocketAddr = "203.0.113.7:5000".parse().unwrap();
        c.parts.extensions.insert(crate::extract::ClientAddr(addr));
        assert_eq!(c.peer_addr(), Some(addr));
    }

    #[tokio::test]
    async fn path_extracts_typed_param() {
        let mut c = ctx("/todos/42", "");
        c.params.push(("id".into(), "42".into()));
        let Path(id): Path<i64> = Path::<i64>::from_request(&mut c).await.unwrap();
        assert_eq!(id, 42);
    }

    #[test]
    fn ctx_param_reads_a_named_captured_param() {
        // WHY: a tenancy/ownership guard must address a SPECIFIC mount param by
        // name — the tenant fk `club_id` under `/clubs/{club_id}` — not just the
        // leaf-most param that `Path<T>` binds. `param(name)` is that named read;
        // it underpins membership verification (issues #78/#79). Absent → None.
        let mut c = ctx("/x", "");
        c.params.push(("club_id".into(), "42".into()));
        c.params.push(("id".into(), "7".into()));
        assert_eq!(c.param("club_id"), Some("42"));
        assert_eq!(c.param("id"), Some("7"));
        assert_eq!(c.param("missing"), None);
    }

    #[tokio::test]
    async fn path_params_reads_named_and_rejects_task() {
        // WHY: a DI factory resolves each arg via `FromRequest` (it cannot borrow
        // `&RequestCtx`), and `Path<T>` binds only the leaf-most segment — so the
        // membership-verifying tenancy guard (issues #78/#79) needs a by-NAME read
        // of a specific mount param (the tenant fk `club_id` under `/clubs/{club_id}`,
        // even when a leaf `{id}` follows). `PathParams` is that FromRequest accessor.
        let mut c = ctx("/x", "");
        c.params.push(("club_id".into(), "42".into()));
        c.params.push(("id".into(), "7".into()));
        let p = PathParams::from_request(&mut c).await.unwrap();
        assert_eq!(p.get("club_id"), Some("42"));
        assert_eq!(p.get("id"), Some("7"));
        assert_eq!(p.get("missing"), None);

        // A task context has no request path — reject like every HTTP extractor (JC1003).
        let mut task = ctx("/x", "");
        task.is_task = true;
        let err = PathParams::from_request(&mut task).await.err().unwrap();
        assert_eq!(err.code(), "JC1003");
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
    async fn option_extractor_yields_none_on_failure_and_some_on_success() {
        // WHY: a private bucket's GET must accept EITHER a session OR a signed
        // URL — the handler needs optional extraction instead of a hard 401
        // from the extractor. Option<T> is None on ANY extraction failure.
        #[derive(serde::Deserialize)]
        struct P {
            n: i64,
        }
        async fn probe(q: Option<Query<P>>) -> Result<Json<Option<i64>>> {
            Ok(Json(q.map(|Query(p)| p.n)))
        }
        let t = crate::App::new()
            .route("/probe", crate::get(probe))
            .into_test();
        assert_eq!(t.get("/probe?n=7").await.text(), "7");
        // Missing/malformed query → None, not a 400.
        assert_eq!(t.get("/probe").await.text(), "null");
        assert_eq!(t.get("/probe?n=not-a-number").await.text(), "null");
    }

    #[tokio::test]
    async fn result_extractor_preserves_the_inner_error() {
        // WHY (#109): Option<T> discards WHY the inner extractor failed, so a
        // route that accepts a session OR a signed URL collapses a guard's 403
        // into a blanket 401. Result<T, Error> keeps the inner error for the
        // handler to propagate (`let v = v?;`) — extraction itself never fails
        // the request, so the fallback credential path still runs first.
        struct Gate;
        impl FromRequest for Gate {
            async fn from_request(_ctx: &mut RequestCtx) -> Result<Self> {
                Err(Error::forbidden())
            }
        }
        async fn probe(gate: Result<Gate, Error>) -> Result<Json<&'static str>> {
            let _gate = gate?;
            Ok(Json("open"))
        }
        let t = crate::App::new()
            .route("/probe", crate::get(probe))
            .into_test();
        let res = t.get("/probe").await;
        assert_eq!(
            res.status().as_u16(),
            403,
            "the inner error's status survives; body: {}",
            res.text()
        );
        assert!(res.text().contains("JC0403"), "body: {}", res.text());
        // The success side passes the value through as Ok(Ok(v)).
        let mut c = ctx("/x", "");
        let ok = <Result<Headers, Error> as FromRequest>::from_request(&mut c)
            .await
            .expect("outer extraction never fails");
        assert!(ok.is_ok(), "success is Ok(Ok(_))");
    }

    #[tokio::test]
    async fn single_path_param_binds_the_leaf_segment() {
        use crate::prelude::*;
        async fn show(Path(id): Path<i64>) -> Result<Json<i64>> {
            Ok(Json(id))
        }
        let t = App::new()
            .mount(
                "/ws/{ws}",
                Module::new("leads").route("/leads/{id}", get(show)),
            )
            .into_test();
        assert_eq!(
            t.get("/ws/7/leads/42").await.json::<i64>(),
            42,
            "leaf param, not mount param"
        );
    }

    #[tokio::test]
    async fn tuples_still_read_root_to_leaf() {
        use crate::prelude::*;
        async fn pair(Path((ws, id)): Path<(i64, i64)>) -> Result<Json<(i64, i64)>> {
            Ok(Json((ws, id)))
        }
        let t = App::new()
            .mount(
                "/ws/{ws}",
                Module::new("leads").route("/leads/{id}", get(pair)),
            )
            .into_test();
        assert_eq!(t.get("/ws/7/leads/42").await.json::<(i64, i64)>(), (7, 42));
    }

    /// Issue #283: a route nested THREE param-levels deep must bind a 3-tuple that
    /// reads root→leaf. This is the runtime counterpart of the generator fix: a
    /// handler under `/accounts/{account_id}/contacts/{contact_id}/notes/{id}` binds
    /// `Path<(i64,i64,i64)>`, and `take_params::<3>` reads (account_id, contact_id,
    /// id) POSITIONALLY. Before the generator fix the handler bound only the local
    /// 2-tuple `(contact_id, id)`, which core resolves to (account_id, contact_id) —
    /// the leaf id `999` was never read and the route was dead.
    #[tokio::test]
    async fn deep_nested_triple_reads_root_to_leaf() {
        use crate::prelude::*;
        async fn triple(
            Path((account_id, contact_id, id)): Path<(i64, i64, i64)>,
        ) -> Result<Json<(i64, i64, i64)>> {
            Ok(Json((account_id, contact_id, id)))
        }
        let t = App::new()
            .mount(
                "/accounts/{account_id}",
                Module::new("contacts").mount(
                    "/contacts/{contact_id}",
                    Module::new("notes").route("/notes/{id}", get(triple)),
                ),
            )
            .into_test();
        assert_eq!(
            t.get("/accounts/111/contacts/222/notes/999")
                .await
                .json::<(i64, i64, i64)>(),
            (111, 222, 999),
            "the 3-tuple must read account_id=111, contact_id=222, id=999 positionally"
        );
    }

    #[tokio::test]
    async fn path_param_macro_admits_custom_newtypes() {
        use crate::prelude::*;
        #[derive(Debug)]
        struct LeadId(i64);
        impl std::str::FromStr for LeadId {
            type Err = std::num::ParseIntError;
            fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
                Ok(LeadId(s.parse()?))
            }
        }
        crate::path_param!(LeadId);
        async fn show(Path(id): Path<LeadId>) -> Result<Json<i64>> {
            Ok(Json(id.0))
        }
        let t = App::new().route("/leads/{id}", get(show)).into_test();
        assert_eq!(t.get("/leads/42").await.json::<i64>(), 42);
    }

    #[tokio::test]
    async fn raw_body_yields_exact_bytes_and_coexists_with_headers() {
        use crate::prelude::*;
        async fn verify(headers: Headers, body: RawBody) -> Result<Json<(usize, bool)>> {
            let signed = headers.get("x-signature").is_some();
            Ok(Json((body.0.len(), signed)))
        }
        let t = App::new().route("/hook", post(verify)).into_test();
        let res = t
            .post_bytes_with("/hook", b"{\"raw\": 1}", &[("x-signature", "abc")])
            .await;
        assert_eq!(res.status().as_u16(), 200);
        assert_eq!(res.json::<(usize, bool)>(), (10, true));
    }

    #[tokio::test]
    async fn raw_body_drains_a_stream_route_transparently() {
        use crate::prelude::*;
        async fn len(body: RawBody) -> Result<Json<usize>> {
            Ok(Json(body.0.len()))
        }
        let t = App::new().route("/up", post(len).stream_body()).into_test();
        let payload = vec![b'x'; 100]; // > one 13-byte test frame
        let res = t.post_bytes("/up", &payload).await;
        assert_eq!(res.json::<usize>(), 100);
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

    /// Build a stream-lane RequestCtx directly, optionally capping it with
    /// `Limited` — the in-process analogue of the serve-time stream lane,
    /// without a socket. Frames the body in one chunk; that is enough for the
    /// caching/limit unit tests (frame straddling is exercised by TestApp).
    fn stream_ctx(body: &[u8], limit: Option<usize>) -> RequestCtx {
        use http_body_util::BodyExt;
        use http_body_util::combinators::UnsyncBoxBody;
        let req = http::Request::builder().uri("/up").body(()).unwrap();
        let (parts, ()) = req.into_parts();
        let bytes = Bytes::copy_from_slice(body);
        let lane: StreamLane = match limit {
            Some(limit) => {
                let limited = http_body_util::Limited::new(
                    http_body_util::Full::<Bytes>::new(bytes).map_err(
                        |never| -> Box<dyn std::error::Error + Send + Sync> { match never {} },
                    ),
                    limit,
                );
                UnsyncBoxBody::new(limited.map_err(Into::into))
            }
            None => {
                let full = http_body_util::Full::<Bytes>::new(bytes);
                UnsyncBoxBody::new(full.map_err(
                    |never| -> Box<dyn std::error::Error + Send + Sync> { match never {} },
                ))
            }
        };
        RequestCtx::with_lane(
            parts,
            BodyLane::Stream(Some(lane)),
            DepResolver::new(Arc::new(DepEnv::default()), Default::default()),
        )
    }

    #[tokio::test]
    async fn stream_routes_deliver_the_body_and_enforce_the_limit() {
        use crate::prelude::*;
        async fn echo(Json(v): Json<serde_json::Value>) -> Result<Json<serde_json::Value>> {
            Ok(Json(v))
        }
        let t = App::new()
            .route("/up", post(echo).stream_body().body_limit(64))
            .into_test();
        // Json over a STREAM lane drains transparently.
        let res = t.post_json("/up", &serde_json::json!({"k": "v"})).await;
        assert_eq!(res.status().as_u16(), 200);
        // Cumulative limit still applies on the stream lane: oversize → 413.
        let big = serde_json::json!({"k": "x".repeat(200)});
        let res = t.post_json("/up", &big).await;
        assert_eq!(res.status().as_u16(), 413, "body: {}", res.text());
    }

    #[tokio::test]
    async fn drain_body_twice_caches_the_stream_bytes() {
        // The caching contract: a stream lane is drained once and cached back
        // into Buffered, so a SECOND extractor on the same request keeps working
        // instead of seeing an already-consumed stream.
        use bytes::Bytes;
        let mut c = stream_ctx(br#"{"k":"v"}"#, None);
        let first = c.drain_body().await.unwrap();
        assert_eq!(first, Bytes::from_static(br#"{"k":"v"}"#));
        let second = c.drain_body().await.unwrap();
        assert_eq!(second, first, "second drain returns the cached bytes");
    }

    #[tokio::test]
    async fn stream_lane_over_limit_maps_to_413() {
        // A stream lane whose Limited cap trips mid-drain surfaces as 413,
        // exactly like the buffered read path.
        let mut c = stream_ctx(&[b'x'; 200], Some(64));
        let err = c.drain_body().await.err().unwrap();
        assert_eq!(err.code(), "JC0413");
    }

    #[tokio::test]
    async fn limit_trips_through_the_timed_recv_wrapper_still_map_to_413() {
        // The serve-time lane wraps `Limited` in `TimedRecvBody` (the per-frame
        // read-deadline guard); only the unwrapped `Limited` is covered above.
        // If `TimedRecvBody`'s `map_err(Into::into)` ever double-boxed the
        // error, `downcast_ref::<LengthLimitError>()` in `map_stream_error`
        // would miss it and 413s would silently degrade to 400s. Build the
        // exact serve.rs lane shape and assert the cap still maps to 413.
        use crate::serve::TimedRecvBody;
        use http_body_util::BodyExt;
        use http_body_util::combinators::UnsyncBoxBody;
        use std::time::Duration;

        let req = http::Request::builder().uri("/up").body(()).unwrap();
        let (parts, ()) = req.into_parts();
        let over_limit_body = http_body_util::Full::<Bytes>::new(Bytes::from_static(&[b'x'; 200]))
            .map_err(|never| -> Box<dyn std::error::Error + Send + Sync> { match never {} });
        let lane: StreamLane = UnsyncBoxBody::new(TimedRecvBody::new(
            http_body_util::Limited::new(over_limit_body, 64),
            Duration::from_secs(5),
        ));
        let mut c = RequestCtx::with_lane(
            parts,
            BodyLane::Stream(Some(lane)),
            DepResolver::new(Arc::new(DepEnv::default()), Default::default()),
        );
        let err = c.drain_body().await.err().unwrap();
        assert_eq!(err.code(), "JC0413");
        assert_eq!(err.status().as_u16(), 413);
    }
}
