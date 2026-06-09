//! `App` (spec §4.1): assembles mounted modules + app-level routes, validates
//! the route table at build time (fail loud), and dispatches requests.

use crate::dep::{AnyArc, DepEnv, DepFactory, DepResolver};
use crate::error::{Error, Result};
use crate::extract::RequestCtx;
use crate::handler::BoxHandlerFn;
use crate::middleware::{Middleware, Next};
use crate::module::{FlatRoute, Module};
use crate::response::{IntoResponse, Response};
use crate::router::{Endpoint, MethodRouter, RouteMatch, Trie};
use bytes::Bytes;
use std::any::TypeId;
use std::collections::HashMap;
use std::sync::Arc;

/// The application builder. Generated `app/src/main.rs` is exactly this:
/// provide app-level deps, mount modules, serve.
#[derive(Default)]
pub struct App {
    routes: Vec<(String, MethodRouter)>,
    mounts: Vec<(String, Module)>,
    env: DepEnv,
    middleware: Vec<Arc<dyn Middleware>>,
}

impl App {
    pub fn new() -> Self {
        Self::default()
    }

    /// App-level route (prefer modules; this exists for tiny services and tests).
    pub fn route(mut self, path: &str, methods: MethodRouter) -> Self {
        self.routes.push((path.to_string(), methods));
        self
    }

    /// Mount a module at a prefix (spec §4.2).
    pub fn mount(mut self, prefix: &str, module: Module) -> Self {
        self.mounts.push((prefix.to_string(), module));
        self
    }

    /// App-level singleton value dependency.
    pub fn provide<T: Send + Sync + 'static>(mut self, value: T) -> Self {
        self.env.insert_value(value);
        self
    }

    /// App-level async factory dependency (request scope).
    pub fn provide_dep<F, Args, T>(mut self, factory: F) -> Self
    where
        F: DepFactory<Args, T>,
        T: Send + Sync + 'static,
    {
        self.env.insert_factory(factory);
        self
    }

    /// App-level middleware — outermost ring of every route's chain.
    pub fn middleware<M: Middleware>(mut self, mw: M) -> Self {
        self.middleware.push(Arc::new(mw));
        self
    }

    /// Flatten modules, validate the route table, freeze the dispatch trie.
    /// All conflicts surface HERE — before serving (spec §4.1 "fail loud").
    pub fn build(self) -> Result<BuiltApp> {
        let mut trie = Trie::default();
        let app_env = Arc::new(self.env.clone());
        let app_mw: Arc<[Arc<dyn Middleware>]> = Arc::from(self.middleware.clone());

        for (path, methods) in self.routes {
            insert_flat(
                &mut trie,
                FlatRoute {
                    path,
                    methods,
                    env: app_env.clone(),
                    middleware: app_mw.clone(),
                },
            )?;
        }
        for (prefix, module) in self.mounts {
            for flat in module.flatten(&prefix, &self.env, &self.middleware) {
                insert_flat(&mut trie, flat)?;
            }
        }
        Ok(BuiltApp {
            trie,
            overrides: Arc::new(HashMap::new()),
        })
    }

    /// Bind from config and serve forever. Address: `JERRYCAN_ADDR` env var,
    /// default `127.0.0.1:8000`. (Full layered config lands in Phase 1; the
    /// env-var layer is the contract that already works.)
    pub async fn serve(self) -> Result<()> {
        let addr = std::env::var("JERRYCAN_ADDR").unwrap_or_else(|_| "127.0.0.1:8000".to_string());
        let listener = tokio::net::TcpListener::bind(&addr)
            .await
            .map_err(|e| Error::internal(format!("failed to bind {addr}: {e}")))?;
        self.serve_with(listener).await
    }

    /// Serve on an existing listener (tests, socket activation, port 0).
    pub async fn serve_with(self, listener: tokio::net::TcpListener) -> Result<()> {
        const BODY_LIMIT: usize = 1024 * 1024; // 1 MiB — spec §4.4 secure default

        let built = Arc::new(self.build()?);
        loop {
            let (stream, _) = listener
                .accept()
                .await
                // TODO(phase1): tolerate transient accept() errors (EMFILE/ECONNABORTED) with backoff instead of exiting the server.
                .map_err(|e| Error::internal(format!("accept failed: {e}")))?;
            let app = built.clone();
            tokio::spawn(async move {
                let io = hyper_util::rt::TokioIo::new(stream);
                let service = hyper::service::service_fn(
                    move |req: hyper::Request<hyper::body::Incoming>| {
                        let app = app.clone();
                        async move {
                            let (parts, body) = req.into_parts();
                            use http_body_util::BodyExt;
                            let limited = http_body_util::Limited::new(body, BODY_LIMIT);
                            let response = match limited.collect().await {
                                Ok(collected) => app.dispatch(parts, collected.to_bytes()).await,
                                Err(_) => Error::payload_too_large().into_response(),
                            };
                            Ok::<_, std::convert::Infallible>(response)
                        }
                    },
                );
                // Connection errors (resets, parse failures) are per-connection
                // noise, not app failures; hyper already responded 4xx where it could.
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, service)
                    .await;
            });
        }
    }
}

fn insert_flat(trie: &mut Trie, flat: FlatRoute) -> Result<()> {
    let mut methods = HashMap::new();
    for (m, h) in flat.methods.handlers {
        if methods.insert(m.clone(), h).is_some() {
            return Err(Error::internal(format!(
                "duplicate method {m} for `{}`",
                flat.path
            )));
        }
    }
    trie.insert(
        &flat.path,
        Endpoint {
            methods,
            env: flat.env,
            middleware: flat.middleware,
        },
    )
}

/// The frozen, immutable runtime form. Cheap to share across connections.
pub struct BuiltApp {
    pub(crate) trie: Trie,
    pub(crate) overrides: Arc<HashMap<TypeId, AnyArc>>,
}

// The trie holds type-erased handler fns and overrides are `dyn Any`, so the
// internals can't be formatted. A marker impl lets `build().unwrap()` work.
impl std::fmt::Debug for BuiltApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BuiltApp").finish_non_exhaustive()
    }
}

impl BuiltApp {
    /// Route + run middleware chain + handler for one request.
    pub(crate) async fn dispatch(&self, parts: http::request::Parts, body: Bytes) -> Response {
        let method = parts.method.clone();
        let path = parts.uri.path().to_string();
        match self.trie.find(&path, &method) {
            RouteMatch::NotFound => Error::not_found().into_response(),
            RouteMatch::MethodMissing => Error::method_not_allowed().into_response(),
            RouteMatch::Found { endpoint, params } => {
                let mut ctx = RequestCtx::new(
                    parts,
                    body,
                    DepResolver::new(endpoint.env.clone(), self.overrides.clone()),
                );
                ctx.params = params;
                let handler: &BoxHandlerFn = endpoint
                    .methods
                    .get(&method)
                    .expect("find() checked the method");
                Next {
                    chain: &endpoint.middleware,
                    endpoint: handler,
                }
                .run(&mut ctx)
                .await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::response::Json;
    use crate::router::get;
    use crate::{Dep, Path};
    use std::sync::Mutex;

    #[derive(Default)]
    struct Store {
        items: Mutex<Vec<String>>,
    }

    async fn list(store: Dep<Store>) -> Json<Vec<String>> {
        Json(store.items.lock().unwrap().clone())
    }

    async fn create(store: Dep<Store>, Json(item): Json<String>) -> crate::Result<Json<usize>> {
        let mut items = store.items.lock().unwrap();
        items.push(item);
        Ok(Json(items.len()))
    }

    async fn show(store: Dep<Store>, Path(ix): Path<usize>) -> crate::Result<Json<String>> {
        store
            .items
            .lock()
            .unwrap()
            .get(ix)
            .cloned()
            .map(Json)
            .ok_or_else(Error::not_found)
    }

    fn crud_app() -> App {
        App::new().provide(Store::default()).mount(
            "/todos",
            Module::new("todos")
                .route("/", get(list).post(create))
                .route("/{ix}", get(show)),
        )
    }

    async fn dispatch(built: &BuiltApp, method: http::Method, path: &str, body: &str) -> Response {
        let req = http::Request::builder()
            .method(method)
            .uri(path)
            .body(())
            .unwrap();
        let (parts, ()) = req.into_parts();
        built.dispatch(parts, Bytes::from(body.to_string())).await
    }

    #[tokio::test]
    async fn crud_round_trip_in_process() {
        let built = crud_app().build().unwrap();
        let r = dispatch(&built, http::Method::POST, "/todos/", r#""write spike""#).await;
        assert_eq!(r.status(), http::StatusCode::OK);
        let r = dispatch(&built, http::Method::GET, "/todos/0", "").await;
        assert_eq!(r.status(), http::StatusCode::OK);
        let r = dispatch(&built, http::Method::GET, "/todos/9", "").await;
        assert_eq!(r.status(), http::StatusCode::NOT_FOUND);
        let r = dispatch(&built, http::Method::PATCH, "/todos/", "").await;
        assert_eq!(r.status(), http::StatusCode::METHOD_NOT_ALLOWED);
        let r = dispatch(&built, http::Method::GET, "/nope", "").await;
        assert_eq!(r.status(), http::StatusCode::NOT_FOUND);
    }

    #[test]
    fn conflicting_routes_fail_at_build_not_at_request_time() {
        let app = App::new()
            .route("/x", get(|| async { "a" }))
            .route("/x", get(|| async { "b" }));
        let err = app.build().unwrap_err();
        assert!(err.message().contains("/x"));
    }
}
