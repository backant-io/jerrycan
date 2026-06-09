//! Method routing + segment trie with `{param}` captures (spec §4.1).
//! Conflicting routes are detected at build time — fail loud before serving.
//! NOTE: percent-decoding of path segments is deliberately Phase 1 (with fuzzing).

use crate::dep::DepEnv;
use crate::error::{Error, Result};
use crate::handler::{BoxHandlerFn, Handler};
use crate::middleware::Middleware;
use http::Method;
use std::collections::HashMap;
use std::sync::Arc;

/// Per-path method table: `get(list).post(create)` (spec §4.1).
pub struct MethodRouter {
    pub(crate) handlers: Vec<(Method, BoxHandlerFn)>,
}

pub fn get<H: Handler<A>, A>(h: H) -> MethodRouter {
    MethodRouter::new().on(Method::GET, h)
}
pub fn post<H: Handler<A>, A>(h: H) -> MethodRouter {
    MethodRouter::new().on(Method::POST, h)
}
pub fn put<H: Handler<A>, A>(h: H) -> MethodRouter {
    MethodRouter::new().on(Method::PUT, h)
}
pub fn patch<H: Handler<A>, A>(h: H) -> MethodRouter {
    MethodRouter::new().on(Method::PATCH, h)
}
pub fn delete<H: Handler<A>, A>(h: H) -> MethodRouter {
    MethodRouter::new().on(Method::DELETE, h)
}

impl MethodRouter {
    fn new() -> Self {
        Self {
            handlers: Vec::new(),
        }
    }

    pub fn on<H: Handler<A>, A>(mut self, method: Method, h: H) -> Self {
        self.handlers.push((method, h.into_handler_fn()));
        self
    }
    pub fn get<H: Handler<A>, A>(self, h: H) -> Self {
        self.on(Method::GET, h)
    }
    pub fn post<H: Handler<A>, A>(self, h: H) -> Self {
        self.on(Method::POST, h)
    }
    pub fn put<H: Handler<A>, A>(self, h: H) -> Self {
        self.on(Method::PUT, h)
    }
    pub fn patch<H: Handler<A>, A>(self, h: H) -> Self {
        self.on(Method::PATCH, h)
    }
    pub fn delete<H: Handler<A>, A>(self, h: H) -> Self {
        self.on(Method::DELETE, h)
    }
}

/// A flattened route: method table + the effective dependency environment and
/// middleware chain for this path (computed at build time, spec §4.2).
pub(crate) struct Endpoint {
    pub(crate) methods: HashMap<Method, BoxHandlerFn>,
    pub(crate) env: Arc<DepEnv>,
    pub(crate) middleware: Arc<[Arc<dyn Middleware>]>,
}

#[derive(Default)]
pub(crate) struct Trie {
    root: Node,
}

#[derive(Default)]
struct Node {
    statics: HashMap<String, Node>,
    param: Option<(String, Box<Node>)>,
    endpoint: Option<Endpoint>,
}

pub(crate) enum RouteMatch<'a> {
    Found {
        endpoint: &'a Endpoint,
        params: Vec<(String, String)>,
    },
    MethodMissing,
    NotFound,
}

fn segments(path: &str) -> impl Iterator<Item = &str> {
    path.split('/').filter(|s| !s.is_empty())
}

impl Trie {
    pub(crate) fn insert(&mut self, path: &str, endpoint: Endpoint) -> Result<()> {
        let mut node = &mut self.root;
        for seg in segments(path) {
            if let Some(name) = seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                if node.param.is_none() {
                    node.param = Some((name.to_string(), Box::default()));
                }
                let (existing, child) = node.param.as_mut().expect("just ensured");
                if existing != name {
                    return Err(Error::internal(format!(
                        "conflicting path parameters `{{{existing}}}` vs `{{{name}}}` in `{path}`"
                    )));
                }
                node = child;
            } else {
                node = node.statics.entry(seg.to_string()).or_default();
            }
        }
        if node.endpoint.is_some() {
            return Err(Error::internal(format!(
                "duplicate route registration for `{path}`"
            )));
        }
        node.endpoint = Some(endpoint);
        Ok(())
    }

    pub(crate) fn find<'a>(&'a self, path: &str, method: &Method) -> RouteMatch<'a> {
        let segs: Vec<&str> = segments(path).collect();
        let mut params: Vec<(String, String)> = Vec::new();
        match find_node(&self.root, &segs, &mut params) {
            Some(node) => {
                let ep = node
                    .endpoint
                    .as_ref()
                    .expect("find_node only returns endpoint nodes");
                if ep.methods.contains_key(method) {
                    RouteMatch::Found {
                        endpoint: ep,
                        params,
                    }
                } else {
                    RouteMatch::MethodMissing
                }
            }
            None => RouteMatch::NotFound,
        }
    }
}

/// Depth-first with backtracking: static child first; if that subtree fails,
/// retry via the param child (capturing the segment). Only nodes WITH an
/// endpoint count as matches, so a static dead-end falls back to the param route.
fn find_node<'a>(
    node: &'a Node,
    segs: &[&str],
    params: &mut Vec<(String, String)>,
) -> Option<&'a Node> {
    let Some((head, rest)) = segs.split_first() else {
        return node.endpoint.is_some().then_some(node);
    };
    if let Some(child) = node.statics.get(*head)
        && let Some(found) = find_node(child, rest, params)
    {
        return Some(found);
    }
    if let Some((name, child)) = &node.param {
        params.push((name.clone(), (*head).to_string()));
        if let Some(found) = find_node(child, rest, params) {
            return Some(found);
        }
        params.pop();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::response::IntoResponse;

    fn dummy_handler() -> BoxHandlerFn {
        Arc::new(move |_ctx: &mut crate::RequestCtx| Box::pin(async move { "ok".into_response() }))
    }

    fn endpoint(methods: &[Method]) -> Endpoint {
        let mut map = HashMap::new();
        for m in methods {
            map.insert(m.clone(), dummy_handler());
        }
        Endpoint {
            methods: map,
            env: Arc::new(DepEnv::default()),
            middleware: Arc::from(vec![]),
        }
    }

    #[test]
    fn static_and_param_segments_match() {
        let mut t = Trie::default();
        t.insert("/todos", endpoint(&[Method::GET])).unwrap();
        t.insert("/todos/{id}", endpoint(&[Method::GET, Method::DELETE]))
            .unwrap();
        t.insert("/todos/{id}/comments", endpoint(&[Method::GET]))
            .unwrap();

        match t.find("/todos/42/comments", &Method::GET) {
            RouteMatch::Found { params, .. } => {
                assert_eq!(params, vec![("id".to_string(), "42".to_string())])
            }
            _ => panic!("expected match"),
        }
        assert!(matches!(
            t.find("/todos/42", &Method::DELETE),
            RouteMatch::Found { .. }
        ));
    }

    #[test]
    fn unknown_path_is_not_found_and_wrong_method_is_method_missing() {
        let mut t = Trie::default();
        t.insert("/todos", endpoint(&[Method::GET])).unwrap();
        assert!(matches!(
            t.find("/nope", &Method::GET),
            RouteMatch::NotFound
        ));
        assert!(matches!(
            t.find("/todos", &Method::POST),
            RouteMatch::MethodMissing
        ));
    }

    #[test]
    fn duplicate_path_registration_is_a_build_error() {
        let mut t = Trie::default();
        t.insert("/todos", endpoint(&[Method::GET])).unwrap();
        let err = t.insert("/todos", endpoint(&[Method::POST])).unwrap_err();
        assert!(err.message().contains("/todos"));
    }

    #[test]
    fn conflicting_param_names_are_a_build_error() {
        let mut t = Trie::default();
        t.insert("/todos/{id}", endpoint(&[Method::GET])).unwrap();
        let err = t
            .insert("/todos/{todo_id}", endpoint(&[Method::DELETE]))
            .unwrap_err();
        assert!(err.message().contains("id"));
    }

    #[test]
    fn static_dead_end_backtracks_to_param_branch() {
        let mut t = Trie::default();
        t.insert("/a/b/c", endpoint(&[Method::GET])).unwrap();
        t.insert("/a/{x}/d", endpoint(&[Method::GET])).unwrap();
        match t.find("/a/b/d", &Method::GET) {
            RouteMatch::Found { params, .. } => {
                assert_eq!(params, vec![("x".to_string(), "b".to_string())]);
            }
            _ => panic!("expected /a/{{x}}/d to match /a/b/d via backtracking"),
        }
        assert!(matches!(
            t.find("/a/b/c", &Method::GET),
            RouteMatch::Found { .. }
        ));
    }

    #[test]
    fn static_wins_over_param_when_both_match() {
        let mut t = Trie::default();
        t.insert("/users/me", endpoint(&[Method::GET])).unwrap();
        t.insert("/users/{id}", endpoint(&[Method::GET])).unwrap();
        match t.find("/users/me", &Method::GET) {
            RouteMatch::Found { params, .. } => {
                assert!(params.is_empty(), "static match captures nothing")
            }
            _ => panic!("expected static /users/me"),
        }
        match t.find("/users/42", &Method::GET) {
            RouteMatch::Found { params, .. } => {
                assert_eq!(params, vec![("id".to_string(), "42".to_string())])
            }
            _ => panic!("expected param /users/{{id}}"),
        }
    }

    #[test]
    fn method_router_builder_collects_methods() {
        let mr = get(|| async { "a" }).post(|| async { "b" });
        let methods: Vec<_> = mr.handlers.iter().map(|(m, _)| m.clone()).collect();
        assert_eq!(methods, vec![Method::GET, Method::POST]);
    }
}
