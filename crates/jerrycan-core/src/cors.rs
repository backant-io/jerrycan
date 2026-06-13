//! CORS (spec §v2.2). Lives in core because preflight must be answered BEFORE
//! routing (an `OPTIONS` to a method-mismatched route is rejected 405 before
//! any middleware runs), so CORS is a pre-routing + response-decoration concern
//! integrated into `route_policy`/dispatch in later tasks — not a `Middleware`.

use crate::response::{JcBody, Response};
use http::{HeaderValue, Method, StatusCode, header};
use std::time::Duration;

/// Which origins may make cross-origin requests.
#[derive(Clone, Debug)]
pub enum CorsOrigins {
    /// Any origin (`Access-Control-Allow-Origin: *`). Invalid with credentials —
    /// `App::build` refuses the combination.
    Any,
    /// An exact-match allowlist of origin strings (scheme + host + optional port).
    List(Vec<String>),
}

impl CorsOrigins {
    pub fn any() -> Self {
        Self::Any
    }
    pub fn list<I, S>(origins: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::List(origins.into_iter().map(Into::into).collect())
    }
}

/// CORS policy. Build with `CorsConfig::new(origins)`, chain options, install
/// with `App::cors(config)`.
#[derive(Clone, Debug)]
pub struct CorsConfig {
    origins: CorsOrigins,
    methods: Vec<http::Method>, // empty => reflect the route's real methods on preflight
    headers: Vec<String>,       // empty => reflect Access-Control-Request-Headers
    expose: Vec<String>,
    allow_credentials: bool,
    max_age: Option<Duration>,
}

impl CorsConfig {
    pub fn new(origins: CorsOrigins) -> Self {
        Self {
            origins,
            methods: Vec::new(),
            headers: Vec::new(),
            expose: Vec::new(),
            allow_credentials: false,
            max_age: None,
        }
    }
    pub fn allow_credentials(mut self, yes: bool) -> Self {
        self.allow_credentials = yes;
        self
    }
    pub fn max_age(mut self, d: Duration) -> Self {
        self.max_age = Some(d);
        self
    }
    pub fn allow_methods<I: IntoIterator<Item = http::Method>>(mut self, m: I) -> Self {
        self.methods = m.into_iter().collect();
        self
    }
    pub fn allow_headers<I: IntoIterator<Item = S>, S: Into<String>>(mut self, h: I) -> Self {
        self.headers = h.into_iter().map(Into::into).collect();
        self
    }
    pub fn expose_headers<I: IntoIterator<Item = S>, S: Into<String>>(mut self, h: I) -> Self {
        self.expose = h.into_iter().map(Into::into).collect();
        self
    }

    /// Public reader (the builder method `allow_credentials(bool)` can't share the name).
    pub fn allow_credentials_enabled(&self) -> bool {
        self.allow_credentials
    }

    /// True if `origin` is permitted. `Any` matches everything; `List` is exact.
    pub fn allows_origin(&self, origin: &str) -> bool {
        match &self.origins {
            CorsOrigins::Any => true,
            CorsOrigins::List(list) => list.iter().any(|o| o == origin),
        }
    }

    /// Configured `allow_methods` (empty => reflect the route's real methods).
    pub(crate) fn cfg_methods(&self) -> &[http::Method] {
        &self.methods
    }
    /// Configured `allow_headers` (empty => reflect `Access-Control-Request-Headers`).
    pub(crate) fn cfg_headers(&self) -> &[String] {
        &self.headers
    }
    /// Configured `Access-Control-Max-Age`, if set.
    pub(crate) fn cfg_max_age(&self) -> Option<std::time::Duration> {
        self.max_age
    }
    /// Whether `Access-Control-Allow-Credentials: true` is emitted.
    pub(crate) fn credentials(&self) -> bool {
        self.allow_credentials
    }

    /// Validate at build time: `*` + credentials is forbidden by the Fetch spec
    /// and is a footgun, so it is a build error, not a runtime surprise.
    pub(crate) fn validate(&self) -> crate::Result<()> {
        if self.allow_credentials && matches!(self.origins, CorsOrigins::Any) {
            return Err(crate::Error::internal(
                "CORS misconfiguration: allow_credentials(true) cannot be combined with CorsOrigins::any() — list explicit origins",
            ));
        }
        Ok(())
    }
}

/// Is this request a CORS preflight? (`OPTIONS` + `Origin` +
/// `Access-Control-Request-Method`). All three are required by the Fetch spec;
/// a bare `OPTIONS` (no origin/no ACRM) is a normal request, not a preflight.
pub(crate) fn is_preflight(parts: &http::request::Parts) -> bool {
    parts.method == Method::OPTIONS
        && parts.headers.contains_key(header::ORIGIN)
        && parts
            .headers
            .contains_key(header::ACCESS_CONTROL_REQUEST_METHOD)
}

/// Build the CORS preflight `204` for an ALLOWED origin. `allowed_methods` are
/// the route's real methods (used when the config doesn't pin `allow_methods`).
/// Returns a bare `204` carrying ONLY CORS headers — deliberately no security
/// headers, since `cache-control: no-store` would fight `Access-Control-Max-Age`.
pub(crate) fn preflight_response(
    config: &CorsConfig,
    origin: &str,
    request_headers: Option<&str>,
    allowed_methods: &[Method],
) -> Response {
    let mut r = http::Response::new(JcBody::empty());
    *r.status_mut() = StatusCode::NO_CONTENT;
    let h = r.headers_mut();
    if let Ok(v) = HeaderValue::from_str(origin) {
        h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, v);
        h.insert(header::VARY, HeaderValue::from_static("Origin"));
    }
    if config.credentials() {
        h.insert(
            header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
            HeaderValue::from_static("true"),
        );
    }
    let methods = if config.cfg_methods().is_empty() {
        allowed_methods
    } else {
        config.cfg_methods()
    };
    let methods_joined = methods
        .iter()
        .map(Method::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    if let Ok(v) = HeaderValue::from_str(&methods_joined) {
        h.insert(header::ACCESS_CONTROL_ALLOW_METHODS, v);
    }
    let allow_headers = if config.cfg_headers().is_empty() {
        request_headers.map(str::to_string)
    } else {
        Some(config.cfg_headers().join(", "))
    };
    if let Some(hdrs) = allow_headers
        && let Ok(v) = HeaderValue::from_str(&hdrs)
    {
        h.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, v);
    }
    if let Some(age) = config.cfg_max_age()
        && let Ok(v) = HeaderValue::from_str(&age.as_secs().to_string())
    {
        h.insert(header::ACCESS_CONTROL_MAX_AGE, v);
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_builder_shapes_origins_and_credentials() {
        let c = CorsConfig::new(CorsOrigins::list(["https://app.example"]))
            .allow_credentials(true)
            .max_age(std::time::Duration::from_secs(600));
        assert!(c.allows_origin("https://app.example"));
        assert!(!c.allows_origin("https://evil.example"));
        assert!(c.allow_credentials_enabled());
    }

    #[test]
    fn any_origin_allows_everything() {
        let c = CorsConfig::new(CorsOrigins::any());
        assert!(c.allows_origin("https://whatever.example"));
    }
}
