//! Dependency injection (spec §4.3) — async, nested, per-request memoized,
//! override-able in tests. Resolution order: cache → overrides → singletons → factories.

use crate::error::{Error, Result};
use crate::extract::{FromRequest, RequestCtx};
use std::any::{Any, TypeId, type_name};
use std::collections::HashMap;
use std::future::Future;
use std::ops::Deref;
use std::pin::Pin;
use std::sync::Arc;

pub(crate) type AnyArc = Arc<dyn Any + Send + Sync>;
pub(crate) type ProviderFut<'a> = Pin<Box<dyn Future<Output = Result<AnyArc>> + Send + 'a>>;
pub(crate) type ProviderFn =
    Arc<dyn for<'a> Fn(&'a mut RequestCtx) -> ProviderFut<'a> + Send + Sync>;

/// The provider set effective for a route: app providers merged with the
/// route's module chain — inner module wins (spec §4.2 scoping).
#[derive(Default, Clone)]
pub struct DepEnv {
    pub(crate) singletons: HashMap<TypeId, AnyArc>,
    pub(crate) factories: HashMap<TypeId, ProviderFn>,
}

impl DepEnv {
    /// Register an already-built value; shared by every request (singleton scope).
    // Used by tests now; the app builder wires it up in a later phase task.
    #[allow(dead_code)]
    pub(crate) fn insert_value<T: Send + Sync + 'static>(&mut self, value: T) {
        let id = TypeId::of::<T>();
        self.singletons.insert(id, Arc::new(value));
        self.factories.remove(&id);
    }

    /// Later entries shadow earlier ones — used to layer module envs over the app env.
    // Consumed by the module/routing scope layer in a later phase task.
    #[allow(dead_code)]
    pub(crate) fn merge_from(&mut self, inner: &DepEnv) {
        for (k, v) in &inner.singletons {
            self.singletons.insert(*k, v.clone());
            self.factories.remove(k);
        }
        for (k, f) in &inner.factories {
            self.factories.insert(*k, f.clone());
            self.singletons.remove(k);
        }
    }
}

/// Per-request resolution state. Cheap to create; memoizes by `TypeId`.
pub struct DepResolver {
    pub(crate) env: Arc<DepEnv>,
    pub(crate) overrides: Arc<HashMap<TypeId, AnyArc>>,
    pub(crate) cache: HashMap<TypeId, AnyArc>,
    pub(crate) depth: u8,
}

impl DepResolver {
    // Used by tests now; the server constructs resolvers per request in a later task.
    #[allow(dead_code)]
    pub(crate) fn new(env: Arc<DepEnv>, overrides: Arc<HashMap<TypeId, AnyArc>>) -> Self {
        Self {
            env,
            overrides,
            cache: HashMap::new(),
            depth: 0,
        }
    }
}

const MAX_RESOLVE_DEPTH: u8 = 32;

impl RequestCtx {
    /// Resolve a dependency by type, memoized for this request (spec §4.3).
    pub async fn resolve<T: Send + Sync + 'static>(&mut self) -> Result<Arc<T>> {
        let id = TypeId::of::<T>();
        if let Some(v) = self.deps.cache.get(&id) {
            return downcast::<T>(v.clone());
        }
        if let Some(v) = self.deps.overrides.get(&id).cloned() {
            self.deps.cache.insert(id, v.clone());
            return downcast::<T>(v);
        }
        if let Some(v) = self.deps.env.singletons.get(&id).cloned() {
            self.deps.cache.insert(id, v.clone());
            return downcast::<T>(v);
        }
        let factory = match self.deps.env.factories.get(&id) {
            Some(f) => f.clone(),
            None => return Err(Error::missing_dependency(type_name::<T>())),
        };
        self.deps.depth += 1;
        if self.deps.depth > MAX_RESOLVE_DEPTH {
            self.deps.depth -= 1;
            return Err(Error::dependency_cycle());
        }
        let produced = (*factory)(self).await;
        self.deps.depth -= 1;
        let v = produced?;
        self.deps.cache.insert(id, v.clone());
        downcast::<T>(v)
    }
}

fn downcast<T: Send + Sync + 'static>(v: AnyArc) -> Result<Arc<T>> {
    v.downcast::<T>()
        .map_err(|_| Error::internal("dependency type mismatch (provider/consumer disagree)"))
}

/// A resolved dependency. Derefs to `T`; cloning is `Arc`-cheap.
pub struct Dep<T: ?Sized>(pub(crate) Arc<T>);

impl<T: ?Sized> Deref for Dep<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T: ?Sized> Clone for Dep<T> {
    fn clone(&self) -> Self {
        Dep(self.0.clone())
    }
}

impl<T: Send + Sync + 'static> FromRequest for Dep<T> {
    async fn from_request(ctx: &mut RequestCtx) -> Result<Self> {
        ctx.resolve::<T>().await.map(Dep)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    pub(crate) fn test_ctx(env: DepEnv) -> RequestCtx {
        let req = http::Request::builder().uri("/").body(()).unwrap();
        let (parts, ()) = req.into_parts();
        RequestCtx::new(
            parts,
            Bytes::new(),
            DepResolver::new(Arc::new(env), Arc::new(HashMap::new())),
        )
    }

    struct Config {
        name: &'static str,
    }

    #[tokio::test]
    async fn value_provider_resolves_and_derefs() {
        let mut env = DepEnv::default();
        env.insert_value(Config { name: "prod" });
        let mut ctx = test_ctx(env);
        let cfg: Dep<Config> = Dep::from_request(&mut ctx).await.unwrap();
        assert_eq!(cfg.name, "prod"); // Deref<Target = Config>
    }

    #[tokio::test]
    async fn missing_provider_is_jc1001() {
        let mut ctx = test_ctx(DepEnv::default());
        let err = Dep::<Config>::from_request(&mut ctx).await.err().unwrap();
        assert_eq!(err.code(), "JC1001");
        assert!(err.message().contains("Config"));
    }

    #[tokio::test]
    async fn same_request_yields_same_arc() {
        let mut env = DepEnv::default();
        env.insert_value(Config { name: "x" });
        let mut ctx = test_ctx(env);
        let a = ctx.resolve::<Config>().await.unwrap();
        let b = ctx.resolve::<Config>().await.unwrap();
        assert!(Arc::ptr_eq(&a, &b));
    }
}
