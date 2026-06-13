//! CORS (spec §v2.2). Lives in core because preflight must be answered BEFORE
//! routing (an `OPTIONS` to a method-mismatched route is rejected 405 before
//! any middleware runs), so CORS is a pre-routing + response-decoration concern
//! integrated into `route_policy`/dispatch in later tasks — not a `Middleware`.

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
