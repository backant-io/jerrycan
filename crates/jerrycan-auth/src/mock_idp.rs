//! A deterministic in-process OAuth2 IdP for tests (spec §v2.4 Task 4), gated
//! `#[cfg(feature = "oauth")]`.
//!
//! [`MockIdp`] mints tokens from a monotonic counter (NOT randomness), so a test
//! that issues `code "c1"` always gets the same `(access, refresh)` — runs are
//! reproducible. It exposes the SAME core through two surfaces that share one
//! implementation so they cannot diverge:
//!
//! - [`MockIdp::token_transport`] — an in-process [`TokenTransport`] (no socket)
//!   for hermetic [`OAuthClient`](crate::oauth::OAuthClient) unit tests.
//! - [`MockIdp::into_app`] — a real jerrycan [`App`] (`GET /authorize` → 302,
//!   `POST /token` → JSON) so the production [`HttpTransport`](crate::oauth::HttpTransport)
//!   can be exercised over a real localhost socket.
//!
//! Both call one shared `IdpCore`, so the transport path and the HTTP path
//! validate codes, rotate one-time codes, and mint refreshes identically.

use crate::oauth::{TokenFuture, TokenResponse, TokenTransport};
use jerrycan_core::http::{StatusCode, header};
use jerrycan_core::{App, Dep, Error, JcBody, Query, RawBody, Response, Result, get, post};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// The shared IdP core: the in-memory state plus the validate/mint logic both the
/// transport and the HTTP app delegate to. Held behind an `Arc` so the app's DI
/// and the transport can share one instance.
struct IdpCore {
    /// Pre-issued codes awaiting exchange: `code -> (access, refresh)`. Removed on
    /// first use (one-time codes).
    codes: Mutex<HashMap<String, (String, String)>>,
    /// Valid refresh tokens (a refresh stays valid for repeated use here; the
    /// access token it mints is fresh each time).
    refresh_tokens: Mutex<HashMap<String, ()>>,
    /// Monotonic counter driving deterministic token minting.
    counter: AtomicU64,
}

impl IdpCore {
    fn new() -> Self {
        Self {
            codes: Mutex::new(HashMap::new()),
            refresh_tokens: Mutex::new(HashMap::new()),
            counter: AtomicU64::new(0),
        }
    }

    /// Mint the next deterministic `(access, refresh)` pair and register the
    /// refresh as valid. Pure function of the counter, so it is reproducible.
    fn issue_code(&self, code: &str) -> (String, String) {
        let n = self.counter.fetch_add(1, Ordering::SeqCst);
        let access = format!("mock-access-{n}");
        let refresh = format!("mock-refresh-{n}");
        self.codes
            .lock()
            .expect("mock idp codes mutex poisoned")
            .insert(code.to_string(), (access.clone(), refresh.clone()));
        self.refresh_tokens
            .lock()
            .expect("mock idp refresh mutex poisoned")
            .insert(refresh.clone(), ());
        (access, refresh)
    }

    /// Validate and consume a one-time authorization code, returning its tokens.
    /// A bad/already-used code is an OAuth `invalid_grant`.
    fn exchange_code(&self, code: &str) -> std::result::Result<TokenResponse, OAuthError> {
        let removed = self
            .codes
            .lock()
            .expect("mock idp codes mutex poisoned")
            .remove(code);
        match removed {
            Some((access, refresh)) => Ok(TokenResponse {
                access_token: access,
                token_type: "Bearer".to_string(),
                refresh_token: Some(refresh),
                expires_in: Some(3600),
                scope: None,
            }),
            None => Err(OAuthError::invalid_grant(
                "unknown or already-used authorization code",
            )),
        }
    }

    /// Validate a refresh token and mint a FRESH access token (deterministic).
    /// A bad refresh token is an OAuth `invalid_grant`.
    fn refresh(&self, refresh_token: &str) -> std::result::Result<TokenResponse, OAuthError> {
        let known = self
            .refresh_tokens
            .lock()
            .expect("mock idp refresh mutex poisoned")
            .contains_key(refresh_token);
        if !known {
            return Err(OAuthError::invalid_grant("unknown refresh token"));
        }
        let n = self.counter.fetch_add(1, Ordering::SeqCst);
        Ok(TokenResponse {
            access_token: format!("mock-access-{n}"),
            token_type: "Bearer".to_string(),
            refresh_token: Some(refresh_token.to_string()),
            expires_in: Some(3600),
            scope: None,
        })
    }

    /// The token-endpoint dispatch shared by the transport and the HTTP `/token`
    /// handler: route on `grant_type`, ignoring `client_id`/`client_secret`
    /// (the mock does not authenticate the app — it validates the grant only).
    ///
    /// Returns either a [`TokenResponse`] or an OAuth error — the SAME outcome
    /// for both surfaces. The HTTP handler renders the error as an RFC 6749 JSON
    /// body (`{"error":…}` + 400) and the transport feeds that same JSON through
    /// [`crate::oauth::parse_token_body`], so the wire path and the in-process path
    /// produce an identical client-facing error.
    fn handle_token_form(
        &self,
        form: &[(String, String)],
    ) -> std::result::Result<TokenResponse, OAuthError> {
        let grant_type = form
            .iter()
            .find(|(k, _)| k == "grant_type")
            .map(|(_, v)| v.as_str());
        match grant_type {
            Some("authorization_code") => {
                let code = field(form, "code")
                    .ok_or_else(|| OAuthError::invalid_request("missing code"))?;
                self.exchange_code(code)
            }
            Some("refresh_token") => {
                let token = field(form, "refresh_token")
                    .ok_or_else(|| OAuthError::invalid_request("missing refresh_token"))?;
                self.refresh(token)
            }
            Some(other) => Err(OAuthError::invalid_request(&format!(
                "unsupported grant_type: {other}"
            ))),
            None => Err(OAuthError::invalid_request("missing grant_type")),
        }
    }
}

/// An RFC 6749 §5.2 token-error: the `error` code plus a human `error_description`.
/// Rendered as the standard `{"error":…,"error_description":…}` JSON body so a real
/// OAuth client (including jerrycan's own [`HttpTransport`](crate::oauth::HttpTransport))
/// parses it exactly as it would a production provider's error.
struct OAuthError {
    error: &'static str,
    description: String,
}

impl OAuthError {
    fn invalid_grant(detail: &str) -> Self {
        Self {
            error: "invalid_grant",
            description: detail.to_string(),
        }
    }
    fn invalid_request(detail: &str) -> Self {
        Self {
            error: "invalid_request",
            description: detail.to_string(),
        }
    }
    /// The RFC 6749 error JSON body bytes.
    fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "error": self.error,
            "error_description": self.description,
        }))
        .expect("serializing a fixed json object never fails")
    }
}

/// Look up a form field by key.
fn field<'a>(form: &'a [(String, String)], key: &str) -> Option<&'a str> {
    form.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

/// A deterministic mock authorization-code + refresh IdP. Cheap to clone (shares
/// one `Arc<IdpCore>`), so `token_transport()` and `into_app()` operate on the
/// same state.
#[derive(Clone)]
pub struct MockIdp {
    core: Arc<IdpCore>,
}

impl Default for MockIdp {
    fn default() -> Self {
        Self::new()
    }
}

impl MockIdp {
    pub fn new() -> Self {
        Self {
            core: Arc::new(IdpCore::new()),
        }
    }

    /// Pre-issue a one-time `code` and return the `(access, refresh)` the IdP will
    /// mint when that code is exchanged. Deterministic (counter-driven).
    pub fn issue_code(&self, code: &str) -> (String, String) {
        self.core.issue_code(code)
    }

    /// An in-process [`TokenTransport`] over this IdP's core — feed it into
    /// `OAuthClient::with_transport` for hermetic tests with no socket.
    pub fn token_transport(&self) -> Arc<dyn TokenTransport> {
        Arc::new(MockTransport {
            core: Arc::clone(&self.core),
        })
    }

    /// Mount this IdP as a real jerrycan [`App`]: `GET /authorize` issues a code
    /// and 302-redirects to `redirect_uri?code=…&state=…`; `POST /token` runs the
    /// form exchange. Shares the core with the transport, so the live HTTP path
    /// and the in-process path can't diverge. Bind it to `127.0.0.1:0` and drive
    /// the real `HttpTransport` against it.
    pub fn into_app(self) -> App {
        App::new()
            .provide(self.core)
            .route("/authorize", get(authorize_handler))
            .route("/token", post(token_handler))
    }
}

/// The in-process transport: parse the `&[(&str,&str)]` form into owned pairs and
/// delegate to the shared core. Never touches a socket.
struct MockTransport {
    core: Arc<IdpCore>,
}

impl TokenTransport for MockTransport {
    fn post_form<'a>(&'a self, _url: &'a str, form: &'a [(&'a str, &'a str)]) -> TokenFuture<'a> {
        let owned: Vec<(String, String)> = form
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let core = Arc::clone(&self.core);
        Box::pin(async move {
            match core.handle_token_form(&owned) {
                Ok(token) => Ok(token),
                // Funnel the error through the SAME parser the wire path uses, so
                // the in-process and HTTP surfaces map an OAuth error identically.
                Err(oauth_err) => crate::oauth::parse_token_body(&oauth_err.to_json()),
            }
        })
    }
}

/// The `/authorize` query params jerrycan needs to build the redirect. Unknown
/// params (`response_type`, `client_id`, `scope`, …) are ignored by serde.
#[derive(Deserialize)]
struct AuthorizeParams {
    redirect_uri: String,
    #[serde(default)]
    state: String,
}

/// `GET /authorize`: issue a fresh code and 302-redirect to the supplied
/// `redirect_uri` with `code` and `state` query params. The `state` is echoed
/// verbatim so the client can verify its CSRF token. Resolves the shared core
/// from DI so the issued code is exchangeable via `/token`.
async fn authorize_handler(
    core: Dep<Arc<IdpCore>>,
    Query(params): Query<AuthorizeParams>,
) -> Result<Response> {
    // Deterministic code derived from the counter, then registered for exchange.
    let code = format!("mock-code-{}", core.next_code_id());
    core.issue_code(&code);

    let sep = if params.redirect_uri.contains('?') {
        '&'
    } else {
        '?'
    };
    let location = format!(
        "{}{sep}code={}&state={}",
        params.redirect_uri,
        encode(&code),
        encode(&params.state)
    );
    let mut response: Response = jerrycan_core::http::Response::new(JcBody::empty());
    *response.status_mut() = StatusCode::FOUND;
    response.headers_mut().insert(
        header::LOCATION,
        location
            .parse()
            .map_err(|_| Error::internal("authorize: bad redirect location"))?,
    );
    Ok(response)
}

/// `POST /token`: parse the urlencoded form and run the shared exchange. Returns a
/// JSON [`TokenResponse`] on success, or the OAuth error (non-500) on a bad grant.
async fn token_handler(core: Dep<Arc<IdpCore>>, RawBody(bytes): RawBody) -> Result<Response> {
    let form: Vec<(String, String)> = jerrycan_core::serde_urlencoded::from_bytes(&bytes)
        .map_err(|e| Error::bad_request(format!("token: malformed form body: {e}")))?;

    // Both arms emit a real OAuth wire body: a `TokenResponse` (200) on success, or
    // the RFC 6749 `{"error":…}` object (400) on a bad grant — exactly what a
    // production provider returns, so jerrycan's own HttpTransport parses it.
    let (status, body) = match core.handle_token_form(&form) {
        Ok(token) => (
            StatusCode::OK,
            serde_json::to_vec(&token)
                .map_err(|e| Error::internal(format!("token: serialize failed: {e}")))?,
        ),
        Err(oauth_err) => (StatusCode::BAD_REQUEST, oauth_err.to_json()),
    };

    let mut response: Response = jerrycan_core::http::Response::new(JcBody::full(body));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json"),
    );
    Ok(response)
}

impl IdpCore {
    /// Next monotonic id for an authorize-issued code (separate read so the code
    /// string is deterministic without consuming a token counter slot).
    fn next_code_id(&self) -> u64 {
        self.counter.fetch_add(1, Ordering::SeqCst)
    }
}

/// Percent-encode a redirect query value (RFC 3986 unreserved passthrough).
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => {
                out.push('%');
                out.push(
                    char::from_digit((b >> 4) as u32, 16)
                        .expect("nibble < 16")
                        .to_ascii_uppercase(),
                );
                out.push(
                    char::from_digit((b & 0x0f) as u32, 16)
                        .expect("nibble < 16")
                        .to_ascii_uppercase(),
                );
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oauth::{OAuthClient, Provider};

    #[tokio::test]
    async fn exchange_code_returns_promised_tokens_and_is_one_time() {
        let idp = MockIdp::new();
        let (access, refresh) = idp.issue_code("c1");

        let client = OAuthClient::new(Provider::google(), "id", "sec", "https://x/cb")
            .with_transport(idp.token_transport());

        let token = client.exchange_code("c1", None).await.unwrap();
        assert_eq!(token.access_token, access);
        assert_eq!(token.refresh_token.as_deref(), Some(refresh.as_str()));

        // The code is one-time: a second exchange fails (non-500 invalid_grant).
        let err = client.exchange_code("c1", None).await.unwrap_err();
        assert_eq!(err.status().as_u16(), 400);
        assert!(err.message().contains("invalid_grant"), "got: {err}");
    }

    #[tokio::test]
    async fn refresh_mints_new_access_and_rejects_a_bogus_token() {
        let idp = MockIdp::new();
        idp.issue_code("c2");
        let client = OAuthClient::new(Provider::google(), "id", "sec", "https://x/cb")
            .with_transport(idp.token_transport());

        let token = client.exchange_code("c2", None).await.unwrap();
        let refresh = token.refresh_token.clone().unwrap();

        let refreshed = client.refresh(&refresh).await.unwrap();
        assert_ne!(
            refreshed.access_token, token.access_token,
            "refresh must mint a fresh access token"
        );

        // A made-up refresh token is rejected.
        let err = client.refresh("not-a-real-refresh").await.unwrap_err();
        assert_eq!(err.status().as_u16(), 400);
        assert!(err.message().contains("invalid_grant"), "got: {err}");
    }

    #[tokio::test]
    async fn mounted_app_authorize_redirects_and_token_exchanges() {
        let app = MockIdp::new().into_app();
        let t = app.into_test();

        // GET /authorize → 302 with a `code` param in the Location redirect.
        let res = t
            .get("/authorize?response_type=code&client_id=id&redirect_uri=https%3A%2F%2Fapp%2Fcb&state=xyz&scope=openid")
            .await;
        assert_eq!(res.status(), StatusCode::FOUND);
        let location = res
            .headers()
            .get(header::LOCATION)
            .expect("Location header present")
            .to_str()
            .unwrap()
            .to_string();
        assert!(
            location.starts_with("https://app/cb?"),
            "redirect target: {location}"
        );
        assert!(location.contains("state=xyz"), "state echoed: {location}");

        // Pull the issued code out of the redirect query.
        let code = location
            .split(['?', '&'])
            .find_map(|kv| kv.strip_prefix("code="))
            .expect("code param present")
            .to_string();

        // POST /token with that code → 200 + JSON tokens.
        let body = format!(
            "grant_type=authorization_code&code={code}&redirect_uri=https%3A%2F%2Fapp%2Fcb&client_id=id&client_secret=sec"
        );
        let res = t
            .post_bytes_with(
                "/token",
                body.as_bytes(),
                &[("content-type", "application/x-www-form-urlencoded")],
            )
            .await;
        assert_eq!(res.status(), StatusCode::OK, "body: {}", res.text());
        let token: TokenResponse = res.json();
        assert!(token.access_token.starts_with("mock-access-"));
        assert!(token.refresh_token.is_some());
    }

    /// The highest-value test: drive the REAL production `HttpTransport` (hyper +
    /// hyper-rustls over a real TCP socket, plain HTTP path) against the mock IdP
    /// `into_app()` served on an ephemeral localhost port. The in-process mock
    /// transport does NOT exercise the hyper client — this proves the production
    /// HTTP path (request building, body collect, JSON parse) actually works.
    #[tokio::test]
    async fn real_http_transport_against_a_real_localhost_server() {
        use crate::oauth::HttpTransport;

        // Pre-issue a code, then serve the SAME IdP over a real socket.
        let idp = MockIdp::new();
        let (access, refresh) = idp.issue_code("live-code");
        let app = idp.into_app();

        // Bind to 127.0.0.1:0, capture the port, then serve in the background.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local_addr");
        let server = tokio::spawn(async move {
            // Serves until the task is dropped at test end.
            let _ = app.serve_with(listener).await;
        });

        // A REAL OAuthClient with the REAL HttpTransport, token_url over plain HTTP.
        let token_url: &'static str = Box::leak(format!("http://{addr}/token").into_boxed_str());
        let provider = Provider {
            auth_url: "http://unused/authorize",
            token_url,
            default_scopes: &["openid"],
        };
        let client = OAuthClient::new(provider, "id", "sec", "http://app/cb")
            .with_transport(Arc::new(HttpTransport::new()));

        // exchange_code over the wire returns the promised tokens.
        let token = client
            .exchange_code("live-code", None)
            .await
            .expect("real http exchange_code");
        assert_eq!(token.access_token, access);
        assert_eq!(token.refresh_token.as_deref(), Some(refresh.as_str()));

        // refresh over the wire mints a fresh access token.
        let refreshed = client.refresh(&refresh).await.expect("real http refresh");
        assert_ne!(refreshed.access_token, access);
        assert!(refreshed.access_token.starts_with("mock-access-"));

        // A bad code over the wire surfaces the non-500 OAuth error.
        let err = client
            .exchange_code("never-issued", None)
            .await
            .unwrap_err();
        assert_eq!(err.status().as_u16(), 400);
        assert!(err.message().contains("invalid_grant"), "got: {err}");

        server.abort();
    }
}
