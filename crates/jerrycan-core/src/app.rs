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
use crate::serve;
#[cfg(test)]
use crate::serve::is_transient_accept_error;
use bytes::Bytes;
use std::any::TypeId;
use std::collections::HashMap;
use std::sync::Arc;

/// The application builder. Generated `app/src/main.rs` is exactly this:
/// provide app-level deps, mount modules, serve.
pub struct App {
    routes: Vec<(String, MethodRouter)>,
    mounts: Vec<(String, Module)>,
    env: DepEnv,
    middleware: Vec<Arc<dyn Middleware>>,
    security_headers: bool,
    handler_timeout: std::time::Duration,
    body_read_timeout: std::time::Duration,
}

impl Default for App {
    fn default() -> Self {
        Self {
            routes: Vec::new(),
            mounts: Vec::new(),
            env: DepEnv::default(),
            middleware: Vec::new(),
            security_headers: true,
            handler_timeout: std::time::Duration::from_secs(30),
            body_read_timeout: std::time::Duration::from_secs(30),
        }
    }
}

/// Spec §6: capabilities register through one seam. An extension receives the
/// builder and returns it — providers, routes, middleware, anything.
pub trait Extension {
    fn register(self, app: App) -> App;
}

impl App {
    pub fn new() -> Self {
        Self::default()
    }

    /// Attach an extension: `App::new().extend(Db::from_env().await?)`.
    pub fn extend<E: Extension>(self, extension: E) -> App {
        extension.register(self)
    }

    /// Secure-by-default headers on every response (spec §4.4). Opting out
    /// must be explicit — that is the contract.
    pub fn security_headers(mut self, on: bool) -> Self {
        self.security_headers = on;
        self
    }

    /// Per-request handler time budget (default 30s — spec §4.4). Exceeding it
    /// returns 503 JC0503 without killing the connection or the server.
    pub fn handler_timeout(mut self, budget: std::time::Duration) -> Self {
        self.handler_timeout = budget;
        self
    }

    /// Time budget for reading a request body (default 30s — spec §4.4).
    pub fn body_read_timeout(mut self, budget: std::time::Duration) -> Self {
        self.body_read_timeout = budget;
        self
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
            security_headers: self.security_headers,
            handler_timeout: self.handler_timeout,
            body_read_timeout: self.body_read_timeout,
        })
    }

    /// Bind from config and serve until Ctrl-C, then drain gracefully.
    /// Address: `JERRYCAN_ADDR` env var, default `127.0.0.1:8000`. (Full layered
    /// config lands in Phase 1; the env-var layer is the contract that already works.)
    pub async fn serve(self) -> Result<()> {
        let addr = std::env::var("JERRYCAN_ADDR").unwrap_or_else(|_| "127.0.0.1:8000".to_string());
        let listener = tokio::net::TcpListener::bind(&addr)
            .await
            .map_err(|e| Error::internal(format!("failed to bind {addr}: {e}")))?;
        self.serve_with_shutdown(listener, serve::shutdown_signal())
            .await
    }

    /// Serve on an existing listener forever (tests, port 0, socket activation).
    pub async fn serve_with(self, listener: tokio::net::TcpListener) -> Result<()> {
        self.serve_with_shutdown(listener, std::future::pending())
            .await
    }

    /// The serve engine: accept until `shutdown` resolves, then stop accepting,
    /// drain in-flight connections (10s cap), and return.
    pub async fn serve_with_shutdown(
        self,
        listener: tokio::net::TcpListener,
        shutdown: impl std::future::Future<Output = ()> + Send,
    ) -> Result<()> {
        serve::run_with_shutdown(self, listener, shutdown).await
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
    pub(crate) security_headers: bool,
    pub(crate) handler_timeout: std::time::Duration,
    pub(crate) body_read_timeout: std::time::Duration,
}

// The trie holds type-erased handler fns and overrides are `dyn Any`, so the
// internals can't be formatted. A marker impl lets `build().unwrap()` work.
impl std::fmt::Debug for BuiltApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BuiltApp").finish_non_exhaustive()
    }
}

/// Defaults chosen for API-only services; handler-set values always win.
pub(crate) fn apply_security_headers(res: &mut Response) {
    const DEFAULTS: [(&str, &str); 5] = [
        ("x-content-type-options", "nosniff"),
        ("x-frame-options", "DENY"),
        ("referrer-policy", "no-referrer"),
        ("content-security-policy", "default-src 'none'"),
        ("cache-control", "no-store"),
    ];
    for (name, value) in DEFAULTS {
        let header_name = http::HeaderName::from_static(name);
        if !res.headers().contains_key(&header_name) {
            res.headers_mut()
                .insert(header_name, http::HeaderValue::from_static(value));
        }
    }
}

impl BuiltApp {
    /// Route + run middleware chain + handler for one request, then apply
    /// secure-by-default headers at the single dispatch exit (spec §4.4).
    pub(crate) async fn dispatch(&self, parts: http::request::Parts, body: Bytes) -> Response {
        let mut response = self.dispatch_inner(parts, body).await;
        if self.security_headers {
            apply_security_headers(&mut response);
        }
        response
    }

    async fn dispatch_inner(&self, parts: http::request::Parts, body: Bytes) -> Response {
        let method = parts.method.clone();
        let path = parts.uri.path().to_string();
        match self.trie.find(&path, &method) {
            RouteMatch::NotFound => Error::not_found().into_response(),
            RouteMatch::MethodMissing => Error::method_not_allowed().into_response(),
            RouteMatch::Malformed => {
                Error::bad_request("malformed percent-encoding in path").into_response()
            }
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
                let run = Next {
                    chain: &endpoint.middleware,
                    endpoint: handler,
                }
                .run(&mut ctx);
                match tokio::time::timeout(self.handler_timeout, run).await {
                    Ok(response) => response,
                    Err(_) => Error::handler_timeout().into_response(),
                }
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

    #[tokio::test]
    async fn extensions_register_through_extend() {
        struct Greeting(&'static str);
        struct GreetingExt;
        impl Extension for GreetingExt {
            fn register(self, app: App) -> App {
                app.provide(Greeting("from-extension"))
            }
        }
        async fn read(g: crate::Dep<Greeting>) -> String {
            // `Dep`'s own `.0` (the inner `Arc`) is `pub(crate)`, so inside this
            // crate it shadows the field access; deref explicitly to the value.
            (*g).0.to_string()
        }
        let t = App::new()
            .extend(GreetingExt)
            .route("/", crate::router::get(read))
            .into_test();
        assert_eq!(t.get("/").await.text(), "from-extension");
    }

    #[test]
    fn accept_error_classification_matches_unix_reality() {
        use std::io::{Error as IoError, ErrorKind};
        for transient in [
            IoError::from(ErrorKind::ConnectionAborted),
            IoError::from(ErrorKind::ConnectionReset),
            IoError::from(ErrorKind::Interrupted),
            IoError::from_raw_os_error(24), // EMFILE
            IoError::from_raw_os_error(23), // ENFILE
        ] {
            assert!(is_transient_accept_error(&transient), "{transient:?}");
        }
        assert!(!is_transient_accept_error(&IoError::from(
            ErrorKind::InvalidInput
        )));
        assert!(!is_transient_accept_error(&IoError::from(
            ErrorKind::PermissionDenied
        )));
    }
}
