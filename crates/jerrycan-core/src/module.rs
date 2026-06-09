//! `Module` (spec §4.2): the unit of routing, packaging, and ownership.
//! Bundles routes, nested subroutes, module-scoped dependencies and middleware.
//! Flattening composes URL prefixes and layers environments (inner wins).

use crate::dep::{DepEnv, DepFactory};
use crate::middleware::Middleware;
use crate::router::MethodRouter;
use std::sync::Arc;

/// Flask's Blueprint, Rust-grade. Built by route crates' `pub fn module()`.
pub struct Module {
    pub(crate) name: String,
    pub(crate) routes: Vec<(String, MethodRouter)>,
    pub(crate) mounts: Vec<(String, Module)>,
    pub(crate) env: DepEnv,
    pub(crate) middleware: Vec<Arc<dyn Middleware>>,
}

impl Module {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            routes: Vec::new(),
            mounts: Vec::new(),
            env: DepEnv::default(),
            middleware: Vec::new(),
        }
    }

    /// Register a path relative to the module's mount point.
    pub fn route(mut self, path: &str, methods: MethodRouter) -> Self {
        self.routes.push((path.to_string(), methods));
        self
    }

    /// Mount a child module (subroute) under a relative prefix. Nests arbitrarily.
    pub fn mount(mut self, prefix: &str, child: Module) -> Self {
        self.mounts.push((prefix.to_string(), child));
        self
    }

    /// Module-scoped singleton value; shadows any parent provider of the same type.
    pub fn provide<T: Send + Sync + 'static>(mut self, value: T) -> Self {
        self.env.insert_value(value);
        self
    }

    /// Module-scoped async factory (request scope); shadows parents likewise.
    pub fn provide_dep<F, Args, T>(mut self, factory: F) -> Self
    where
        F: DepFactory<Args, T>,
        T: Send + Sync + 'static,
    {
        self.env.insert_factory(factory);
        self
    }

    /// Module-scoped middleware; runs after the app's and parents' middleware.
    pub fn middleware<M: Middleware>(mut self, mw: M) -> Self {
        self.middleware.push(Arc::new(mw));
        self
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

/// One route after flattening: absolute path + effective env + middleware chain.
// Constructed by flatten and consumed by the App builder in Task 12.
#[allow(dead_code)]
pub(crate) struct FlatRoute {
    pub(crate) path: String,
    pub(crate) methods: MethodRouter,
    pub(crate) env: Arc<DepEnv>,
    pub(crate) middleware: Arc<[Arc<dyn Middleware>]>,
}

// Used by flatten; the App builder consumes flattened routes in Task 12.
#[allow(dead_code)]
pub(crate) fn join_paths(prefix: &str, rel: &str) -> String {
    let a = prefix.trim_end_matches('/');
    let b = rel.trim_start_matches('/');
    match (a.is_empty(), b.is_empty()) {
        (true, true) => "/".to_string(),
        (false, true) => a.to_string(),
        (true, false) => format!("/{b}"),
        (false, false) => format!("{a}/{b}"),
    }
}

impl Module {
    /// Resolution order baked at build time: app env ← parent modules ← this
    /// module (inner wins); middleware: app's, then parents', then this module's.
    // The App builder calls this in Task 12.
    #[allow(dead_code)]
    pub(crate) fn flatten(
        self,
        prefix: &str,
        parent_env: &DepEnv,
        parent_mw: &[Arc<dyn Middleware>],
    ) -> Vec<FlatRoute> {
        let mut merged = parent_env.clone();
        merged.merge_from(&self.env);

        let mut mw: Vec<Arc<dyn Middleware>> = parent_mw.to_vec();
        mw.extend(self.middleware);

        let env = Arc::new(merged.clone());
        let mw_arc: Arc<[Arc<dyn Middleware>]> = Arc::from(mw.clone());

        let mut out = Vec::new();
        for (path, methods) in self.routes {
            out.push(FlatRoute {
                path: join_paths(prefix, &path),
                methods,
                env: env.clone(),
                middleware: mw_arc.clone(),
            });
        }
        for (sub_prefix, child) in self.mounts {
            let child_prefix = join_paths(prefix, &sub_prefix);
            out.extend(child.flatten(&child_prefix, &merged, &mw));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::router::get;

    struct Cfg {
        tag: &'static str,
    }

    fn leaf_paths(routes: &[FlatRoute]) -> Vec<String> {
        routes.iter().map(|r| r.path.clone()).collect()
    }

    #[test]
    fn nesting_composes_prefixes() {
        let comments = Module::new("comments").route("/", get(|| async { "list" }));
        let todos = Module::new("todos")
            .route("/", get(|| async { "list" }))
            .route("/{id}", get(|| async { "one" }))
            .mount("/{id}/comments", comments);

        let flat = todos.flatten("/todos", &DepEnv::default(), &[]);
        assert_eq!(
            leaf_paths(&flat),
            vec!["/todos", "/todos/{id}", "/todos/{id}/comments"]
        );
    }

    #[test]
    fn module_env_shadows_parent_env() {
        let parent = {
            let mut e = DepEnv::default();
            e.insert_value(Cfg { tag: "app" });
            e
        };
        let child = Module::new("sub")
            .provide(Cfg { tag: "module" })
            .route("/", get(|| async { "x" }));
        let flat = child.flatten("/sub", &parent, &[]);
        let env = &flat[0].env;
        let got = env
            .singletons
            .get(&std::any::TypeId::of::<Cfg>())
            .and_then(|v| v.clone().downcast::<Cfg>().ok())
            .unwrap();
        assert_eq!(got.tag, "module");
    }

    #[test]
    fn middleware_chains_accumulate_parent_first() {
        struct Named(#[allow(dead_code)] &'static str);
        impl Middleware for Named {
            fn handle<'a>(
                &'a self,
                ctx: &'a mut crate::RequestCtx,
                next: crate::middleware::Next<'a>,
            ) -> crate::middleware::MiddlewareFuture<'a> {
                next.run(ctx)
            }
        }
        let inner = Module::new("inner")
            .middleware(Named("inner"))
            .route("/", get(|| async { "x" }));
        let outer = Module::new("outer")
            .middleware(Named("outer"))
            .mount("/inner", inner);
        let flat = outer.flatten("/outer", &DepEnv::default(), &[]);
        assert_eq!(flat[0].middleware.len(), 2, "outer then inner");
    }
}
