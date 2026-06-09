//! Dependency injection (spec §4.3) — async, nested, per-request memoized,
//! override-able in tests. Resolution order: cache → overrides → singletons → factories.
//! Singletons and factories are disjoint by construction (`insert_value`/`insert_factory`
//! each remove the opposite key), so there is no singleton-vs-factory tiebreak to define.

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
    pub(crate) fn insert_value<T: Send + Sync + 'static>(&mut self, value: T) {
        let id = TypeId::of::<T>();
        self.singletons.insert(id, Arc::new(value));
        self.factories.remove(&id);
    }

    /// Register an async factory; runs at most once per request (request scope).
    pub(crate) fn insert_factory<F, Args, T>(&mut self, factory: F)
    where
        F: DepFactory<Args, T>,
        T: Send + Sync + 'static,
    {
        let id = TypeId::of::<T>();
        self.factories.insert(id, factory.into_provider());
        self.singletons.remove(&id);
    }

    /// Later entries shadow earlier ones — used to layer module envs over the app env.
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

/// Async functions registrable as providers. `Args` is the tuple of extractor
/// parameters; `T` the produced dependency. Implemented for arities 0..=8.
pub trait DepFactory<Args, T>: Send + Sync + 'static {
    fn into_provider(self) -> ProviderFn;
}

macro_rules! impl_dep_factory {
    ($($A:ident),*) => {
        impl<F, Fut, T, $($A,)*> DepFactory<($($A,)*), T> for F
        where
            F: Fn($($A),*) -> Fut + Clone + Send + Sync + 'static,
            Fut: Future<Output = Result<T>> + Send,
            T: Send + Sync + 'static,
            $($A: FromRequest + 'static,)*
        {
            fn into_provider(self) -> ProviderFn {
                #[allow(unused_variables)]
                Arc::new(move |ctx: &mut RequestCtx| {
                    let f = self.clone();
                    Box::pin(async move {
                        #[allow(non_snake_case, unused_variables)]
                        {
                            $(let $A = <$A as FromRequest>::from_request(ctx).await?;)*
                            let value = f($($A),*).await?;
                            Ok(Arc::new(value) as AnyArc)
                        }
                    })
                })
            }
        }
    };
}

impl_dep_factory!();
impl_dep_factory!(A1);
impl_dep_factory!(A1, A2);
impl_dep_factory!(A1, A2, A3);
impl_dep_factory!(A1, A2, A3, A4);
impl_dep_factory!(A1, A2, A3, A4, A5);
impl_dep_factory!(A1, A2, A3, A4, A5, A6);
impl_dep_factory!(A1, A2, A3, A4, A5, A6, A7);
impl_dep_factory!(A1, A2, A3, A4, A5, A6, A7, A8);

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

    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Clone)]
    struct Db {
        url: &'static str,
    }
    struct Session {
        token: String,
    }
    struct User {
        name: String,
    }

    async fn make_session() -> crate::Result<Session> {
        Ok(Session {
            token: "t-1".into(),
        })
    }

    async fn current_user(session: Dep<Session>, db: Dep<Db>) -> crate::Result<User> {
        Ok(User {
            name: format!("{}@{}", session.token, db.url),
        })
    }

    fn nested_env() -> DepEnv {
        let mut env = DepEnv::default();
        env.insert_value(Db { url: "pg://prod" });
        env.insert_factory(make_session);
        env.insert_factory(current_user);
        env
    }

    #[tokio::test]
    async fn factories_nest_and_resolve_async() {
        let mut ctx = test_ctx(nested_env());
        let user = ctx.resolve::<User>().await.unwrap();
        assert_eq!(user.name, "t-1@pg://prod");
    }

    #[tokio::test]
    async fn nested_deps_are_memoized_once_per_request() {
        // A test-local counter (`LOCAL_RESOLVES`) + session factory so parallel tests
        // can't pollute these exact-count assertions.
        static LOCAL_RESOLVES: AtomicUsize = AtomicUsize::new(0);
        async fn local_session() -> crate::Result<Session> {
            LOCAL_RESOLVES.fetch_add(1, Ordering::SeqCst);
            Ok(Session {
                token: "t-1".into(),
            })
        }
        fn local_env() -> DepEnv {
            let mut env = DepEnv::default();
            env.insert_value(Db { url: "pg://prod" });
            env.insert_factory(local_session);
            env.insert_factory(current_user);
            env
        }

        LOCAL_RESOLVES.store(0, Ordering::SeqCst);
        let mut ctx = test_ctx(local_env());
        // Both of these need Session (one directly, one through current_user).
        let _s = ctx.resolve::<Session>().await.unwrap();
        let _u = ctx.resolve::<User>().await.unwrap();
        assert_eq!(
            LOCAL_RESOLVES.load(Ordering::SeqCst),
            1,
            "memoized within request"
        );

        // A new request resolves afresh — request scope, not singleton.
        let mut ctx2 = test_ctx(local_env());
        let _u2 = ctx2.resolve::<User>().await.unwrap();
        assert_eq!(LOCAL_RESOLVES.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn self_referential_factory_hits_cycle_guard() {
        struct Loopy;
        async fn loopy(_again: Dep<Loopy>) -> crate::Result<Loopy> {
            Ok(Loopy)
        }
        let mut env = DepEnv::default();
        env.insert_factory(loopy);
        let mut ctx = test_ctx(env);
        let err = ctx.resolve::<Loopy>().await.err().unwrap();
        assert_eq!(err.code(), "JC1002");
    }

    #[tokio::test]
    async fn overrides_shadow_both_values_and_factories() {
        // Real env: Db value + Session factory.
        let mut env = nested_env();
        env.insert_value(Db { url: "pg://prod" });

        // Overrides replace them without touching the env.
        let mut overrides: HashMap<TypeId, AnyArc> = HashMap::new();
        overrides.insert(
            TypeId::of::<Db>(),
            Arc::new(Db {
                url: "sqlite::memory:",
            }),
        );
        overrides.insert(
            TypeId::of::<Session>(),
            Arc::new(Session {
                token: "fake".into(),
            }),
        );

        let req = http::Request::builder().uri("/").body(()).unwrap();
        let (parts, ()) = req.into_parts();
        let mut ctx = RequestCtx::new(
            parts,
            bytes::Bytes::new(),
            DepResolver::new(Arc::new(env), Arc::new(overrides)),
        );

        let user = ctx.resolve::<User>().await.unwrap();
        assert_eq!(user.name, "fake@sqlite::memory:");
    }
}
