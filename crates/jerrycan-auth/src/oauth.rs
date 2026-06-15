//! OAuth2 authorization-code client (spec §v2.4 Task 3), gated `#[cfg(feature =
//! "oauth")]`.
//!
//! ## Shape
//! [`Provider`] is config (production endpoints + default scopes) — there is NO
//! per-provider branching in the exchange logic, so adding a provider is a data
//! change, not a code change. [`OAuthClient`] builds the authorize URL (pure, no
//! network), runs the `authorization_code` and `refresh_token` exchanges over a
//! [`TokenTransport`], and returns a [`TokenResponse`] the app encrypts at rest
//! with `auth.tokens()` (see [`crate::Auth::tokens`]).
//!
//! ## Transport seam
//! [`TokenTransport`] is object-safe (hand-boxed `Send` future, same idiom as
//! `jerrycan_jobs`'s `JobFuture` — no `async-trait`). Production uses
//! [`HttpTransport`] (hyper + hyper-rustls, rustls/ring only, HTTPS for real
//! providers and plain HTTP only for a loopback host — the localhost mock). Tests
//! inject `MockIdp::token_transport` via [`OAuthClient::with_transport`].
//!
//! ## Secret handling (security)
//! `client_secret` is held in a [`Secret`] newtype that does NOT implement
//! `Debug`/`Display` and is never logged. The form bodies that carry it are built
//! locally and handed straight to the transport; transport/TLS errors are mapped
//! to a generic message that never echoes the request body. An OAuth error
//! response (`{"error":"…"}`) becomes a non-500 [`Error`] naming the reason — and
//! since the reason comes from the provider's `error`/`error_description` fields
//! (never from our request), it cannot contain the secret.
//!
//! ## PKCE / CSRF
//! [`OAuthClient::authorize_url_pkce`] generates an S256 verifier/challenge pair;
//! the caller stores the returned [`PkceVerifier`] (server-side, bound to the
//! session) and passes it back into [`OAuthClient::exchange_code`]. The `state`
//! parameter is the caller's CSRF token: the client percent-encodes and forwards
//! it but does NOT generate or validate it — the app must compare the `state` it
//! sent against the one the provider echoes back (documented in the threat model).

use base64::Engine;
use jerrycan_core::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// A wrapper for the OAuth `client_secret` that refuses to be printed. It has no
/// `Debug`/`Display` impl, so the secret can never reach a log line or panic
/// message by accident; the only way out is the deliberate (crate-private)
/// `expose` accessor at the exact point the form body is built.
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrow the raw secret. Named `expose` so every call site reads as a
    /// conscious decision; used only to place the secret into the outbound form.
    fn expose(&self) -> &str {
        &self.0
    }
}

impl From<&str> for Secret {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

impl From<String> for Secret {
    fn from(s: String) -> Self {
        Self::new(s)
    }
}

/// Provider configuration: the two endpoints and a set of default scopes. This is
/// data, not behavior — the exchange logic is identical for every provider, so a
/// new provider is just another constructor returning a different `Provider`.
#[derive(Clone, Debug)]
pub struct Provider {
    /// The authorization endpoint the user's browser is redirected to.
    pub auth_url: &'static str,
    /// The token endpoint the back-channel code/refresh exchange POSTs to.
    pub token_url: &'static str,
    /// Sensible default scopes, used when the caller passes an empty scope list.
    pub default_scopes: &'static [&'static str],
}

impl Provider {
    /// Google OAuth2 (offline access so a refresh token is returned).
    pub fn google() -> Self {
        Self {
            auth_url: "https://accounts.google.com/o/oauth2/v2/auth",
            token_url: "https://oauth2.googleapis.com/token",
            default_scopes: &["openid", "email", "profile"],
        }
    }

    /// GitHub OAuth2.
    pub fn github() -> Self {
        Self {
            auth_url: "https://github.com/login/oauth/authorize",
            token_url: "https://github.com/login/oauth/access_token",
            default_scopes: &["read:user", "user:email"],
        }
    }

    /// HubSpot OAuth2.
    pub fn hubspot() -> Self {
        Self {
            auth_url: "https://app.hubspot.com/oauth/authorize",
            token_url: "https://api.hubapi.com/oauth/v1/token",
            default_scopes: &["oauth"],
        }
    }

    /// Salesforce OAuth2 (production login host; sandboxes use `test.salesforce.com`).
    pub fn salesforce() -> Self {
        Self {
            auth_url: "https://login.salesforce.com/services/oauth2/authorize",
            token_url: "https://login.salesforce.com/services/oauth2/token",
            default_scopes: &["api", "refresh_token"],
        }
    }
}

/// A successful token response. `Serialize + Deserialize` so an app can encrypt it
/// at rest via `auth.tokens().encode(&token)?` (see [`crate::Auth::tokens`]) and
/// decode it on read. Extra provider fields are ignored on deserialize.
///
/// `Debug` is hand-written to REDACT the bearer material: `access_token` prints as
/// `"***"` and `refresh_token` as `<present>`/`<absent>`, never the real values, so
/// a `{:?}` or `tracing` line can't leak a token. The derived `Debug` would have
/// printed both in cleartext.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenResponse {
    pub access_token: String,
    #[serde(default = "default_token_type")]
    pub token_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

impl std::fmt::Debug for TokenResponse {
    /// Redacting `Debug`: the secret token strings NEVER appear. `access_token` is
    /// fixed `"***"`; `refresh_token` collapses to whether one is present. The
    /// non-secret fields (`token_type`, `expires_in`, `scope`) print verbatim so
    /// the value is still useful in a log without leaking bearer material.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenResponse")
            .field("access_token", &"***")
            .field("token_type", &self.token_type)
            .field(
                "refresh_token",
                &if self.refresh_token.is_some() {
                    "<present>"
                } else {
                    "<absent>"
                },
            )
            .field("expires_in", &self.expires_in)
            .field("scope", &self.scope)
            .finish()
    }
}

fn default_token_type() -> String {
    "Bearer".to_string()
}

/// The boxed `Send` future a [`TokenTransport`] returns. Hand-boxed (not
/// `async-trait`) so the trait stays object-safe behind `dyn` — the same idiom as
/// `jerrycan_jobs`'s `JobFuture` and `jerrycan_auth`'s `ApiKeyFuture`.
pub type TokenFuture<'a> = Pin<Box<dyn Future<Output = Result<TokenResponse>> + Send + 'a>>;

/// The back-channel transport: POST an `application/x-www-form-urlencoded` body to
/// the token endpoint and return the parsed [`TokenResponse`]. Object-safe so the
/// real [`HttpTransport`] and the in-process mock both implement it and the client
/// is identical against either.
///
/// An implementation MUST map an OAuth error body (`{"error":…}`) to a non-500
/// [`Error`] naming the reason, and MUST NOT let the `client_secret` (which rides
/// in `form`) reach any error message or log.
pub trait TokenTransport: Send + Sync {
    fn post_form<'a>(&'a self, url: &'a str, form: &'a [(&'a str, &'a str)]) -> TokenFuture<'a>;
}

/// Parse a token-endpoint JSON body into a [`TokenResponse`], mapping an OAuth
/// error object to a non-500 [`Error`]. Shared by every transport so the success
/// shape and the error mapping can't diverge. `body` is the raw response bytes.
///
/// Security: the error message is built only from the provider's `error` /
/// `error_description` fields, never from our request — so it can never contain
/// the `client_secret`.
pub fn parse_token_body(body: &[u8]) -> Result<TokenResponse> {
    let value: serde_json::Value = serde_json::from_slice(body)
        .map_err(|_| Error::bad_request("oauth: token endpoint returned a non-JSON body"))?;

    if let Some(err) = value.get("error").and_then(|e| e.as_str()) {
        // RFC 6749 §5.2 error response. Surface the reason (and description, if
        // present) at 400 — a provider-rejected grant is a client problem, not a
        // 500. Only provider-supplied strings are interpolated.
        let message = match value.get("error_description").and_then(|d| d.as_str()) {
            Some(desc) => format!("oauth provider error: {err}: {desc}"),
            None => format!("oauth provider error: {err}"),
        };
        return Err(Error::bad_request(message));
    }

    serde_json::from_value(value)
        .map_err(|e| Error::bad_request(format!("oauth: malformed token response: {e}")))
}

/// A PKCE (RFC 7636) verifier: a high-entropy random string whose S256 hash is the
/// `code_challenge` sent on the authorize URL. Hold it server-side (bound to the
/// session/state) and pass it back into [`OAuthClient::exchange_code`].
#[derive(Clone)]
pub struct PkceVerifier(String);

impl PkceVerifier {
    /// Generate a fresh verifier: 32 CSPRNG bytes, base64url-no-pad (43 chars),
    /// well within RFC 7636's 43–128 unreserved-char range.
    pub fn generate() -> Self {
        use rand::RngCore;
        let mut bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        Self(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
    }

    /// The raw verifier string (sent as `code_verifier` in the token exchange).
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The S256 `code_challenge`: `base64url_nopad(SHA256(verifier))`.
    pub fn challenge(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.0.as_bytes());
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hasher.finalize())
    }
}

/// An OAuth2 authorization-code client for one provider + app credentials.
///
/// `client_secret` lives in a non-`Debug` [`Secret`]; `OAuthClient` itself does
/// not derive `Debug`, so neither the client nor its secret can be printed.
#[derive(Clone)]
pub struct OAuthClient {
    provider: Provider,
    client_id: String,
    client_secret: Secret,
    redirect_uri: String,
    http: Arc<dyn TokenTransport>,
}

impl OAuthClient {
    /// Build a client with the production [`HttpTransport`] (hyper-rustls).
    pub fn new(
        provider: Provider,
        client_id: impl Into<String>,
        client_secret: impl Into<Secret>,
        redirect_uri: impl Into<String>,
    ) -> Self {
        Self {
            provider,
            client_id: client_id.into(),
            client_secret: client_secret.into(),
            redirect_uri: redirect_uri.into(),
            http: Arc::new(HttpTransport::new()),
        }
    }

    /// Swap the transport — inject the mock IdP's in-process transport for
    /// hermetic tests (`OAuthClient::new(...).with_transport(mock.token_transport())`).
    pub fn with_transport(mut self, transport: Arc<dyn TokenTransport>) -> Self {
        self.http = transport;
        self
    }

    /// Build the authorization-redirect URL (pure; no network). Every parameter is
    /// percent-encoded. `state` is the caller's CSRF token (forwarded verbatim,
    /// encoded). An empty `scopes` falls back to the provider's `default_scopes`.
    pub fn authorize_url(&self, state: &str, scopes: &[&str]) -> String {
        let scope = self.scope_string(scopes);
        format!(
            "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}",
            self.provider.auth_url,
            encode(&self.client_id),
            encode(&self.redirect_uri),
            encode(&scope),
            encode(state),
        )
    }

    /// Like [`authorize_url`](Self::authorize_url) but adds S256 PKCE. Returns the
    /// URL (carrying `code_challenge` + `code_challenge_method=S256`) and the
    /// [`PkceVerifier`] to store and feed back into `exchange_code`.
    pub fn authorize_url_pkce(&self, state: &str, scopes: &[&str]) -> (String, PkceVerifier) {
        let verifier = PkceVerifier::generate();
        let base = self.authorize_url(state, scopes);
        let url = format!(
            "{base}&code_challenge={}&code_challenge_method=S256",
            encode(&verifier.challenge()),
        );
        (url, verifier)
    }

    /// Exchange an authorization `code` for tokens. Pass the same [`PkceVerifier`]
    /// returned by `authorize_url_pkce` (or `None` if PKCE was not used).
    pub async fn exchange_code(
        &self,
        code: &str,
        pkce: Option<&PkceVerifier>,
    ) -> Result<TokenResponse> {
        let mut form: Vec<(&str, &str)> = vec![
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", &self.redirect_uri),
            ("client_id", &self.client_id),
            ("client_secret", self.client_secret.expose()),
        ];
        if let Some(verifier) = pkce {
            form.push(("code_verifier", verifier.as_str()));
        }
        self.http.post_form(self.provider.token_url, &form).await
    }

    /// Exchange a `refresh_token` for a fresh access token.
    pub async fn refresh(&self, refresh_token: &str) -> Result<TokenResponse> {
        let form: Vec<(&str, &str)> = vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", &self.client_id),
            ("client_secret", self.client_secret.expose()),
        ];
        self.http.post_form(self.provider.token_url, &form).await
    }

    /// Space-join the requested scopes, falling back to the provider defaults.
    fn scope_string(&self, scopes: &[&str]) -> String {
        if scopes.is_empty() {
            self.provider.default_scopes.join(" ")
        } else {
            scopes.join(" ")
        }
    }
}

/// Percent-encode a parameter value for a query string or form body. Encodes
/// everything outside the RFC 3986 unreserved set (`A–Z a–z 0–9 - _ . ~`), which
/// is correct for both `application/x-www-form-urlencoded` and query parameters
/// (space → `%20`, not `+`; both are accepted by token endpoints).
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
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

/// The production [`TokenTransport`]: a hyper + hyper-rustls client. HTTPS (via
/// rustls/ring + bundled `webpki-roots`) for real providers; plain `http://` is
/// allowed ONLY to a loopback host (`127.0.0.1`, `::1`, `localhost`) — the mock-IdP
/// escape hatch — and is REFUSED for any other host before a byte is sent, so a
/// misconfigured `http://` token endpoint can't silently ship `client_secret` +
/// code + tokens over cleartext to a real provider.
#[derive(Clone)]
pub struct HttpTransport {
    client: hyper_util::client::legacy::Client<
        hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
        http_body_util::Full<bytes::Bytes>,
    >,
}

impl Default for HttpTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpTransport {
    /// Build the client. Uses an explicit rustls `ring` crypto provider (so it does
    /// not depend on a process-global default being installed) and the bundled
    /// Mozilla roots; `https_or_http` lets the same client reach the localhost mock
    /// over plain HTTP.
    pub fn new() -> Self {
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_provider_and_webpki_roots(rustls::crypto::ring::default_provider())
            .expect("ring provider supports rustls' safe default protocol versions")
            .https_or_http()
            .enable_http1()
            .build();
        let client =
            hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                .build(connector);
        Self { client }
    }
}

impl TokenTransport for HttpTransport {
    fn post_form<'a>(&'a self, url: &'a str, form: &'a [(&'a str, &'a str)]) -> TokenFuture<'a> {
        Box::pin(async move {
            use http_body_util::BodyExt;

            // TLS-downgrade guard: an `http://` token endpoint sends client_secret,
            // the code, and the returned tokens in cleartext. Permit plaintext only
            // to a loopback host (the mock-IdP path); refuse any other `http://`
            // host BEFORE sending a single byte. `https://` is always allowed.
            if !is_loopback_http_ok(url) {
                return Err(Error::internal(
                    "oauth: refusing plaintext http:// to a non-loopback token endpoint",
                ));
            }

            let body = encode_form(form);
            let request = hyper::Request::builder()
                .method(hyper::Method::POST)
                .uri(url)
                .header(
                    hyper::header::CONTENT_TYPE,
                    "application/x-www-form-urlencoded",
                )
                .header(hyper::header::ACCEPT, "application/json")
                .body(http_body_util::Full::new(bytes::Bytes::from(body)))
                .map_err(|e| {
                    Error::internal(format!("oauth: building token request failed: {e}"))
                })?;

            // A transport/TLS failure is mapped to a generic internal error; the
            // request body (which carries client_secret) is NEVER interpolated.
            let response = self
                .client
                .request(request)
                .await
                .map_err(|_| Error::internal("oauth: token endpoint request failed"))?;

            let bytes = response
                .into_body()
                .collect()
                .await
                .map_err(|_| Error::internal("oauth: reading token response body failed"))?
                .to_bytes();

            parse_token_body(&bytes)
        })
    }
}

/// Whether a token-endpoint URL is allowed to be sent over plaintext transport.
///
/// `https://` is always allowed (TLS protects the secret). `http://` is allowed
/// ONLY when the host is loopback (`127.0.0.1`, `::1`, `localhost`) — the in-repo
/// mock-IdP escape hatch — so a real provider can never receive `client_secret`,
/// the code, or tokens in cleartext. Any other scheme, or `http://` to a
/// non-loopback host (or an unparseable host), is refused.
fn is_loopback_http_ok(url: &str) -> bool {
    // Split scheme off `scheme://rest`. Without `://` we can't reason about it, so
    // refuse (the real transport only ever receives http/https token URLs).
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    if scheme.eq_ignore_ascii_case("https") {
        return true;
    }
    if !scheme.eq_ignore_ascii_case("http") {
        return false;
    }

    // Plaintext http://: isolate the host from `host[:port]/path?query#frag`. The
    // authority ends at the first `/`, `?`, or `#`. (No userinfo `@` handling —
    // token endpoints don't carry credentials in the URL.)
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .expect("split always yields at least one element");

    // An IPv6 literal is bracketed: `[::1]:8080`. Strip the brackets to compare the
    // inner address; otherwise the host is everything before the LAST `:` (port).
    let host = if let Some(after) = authority.strip_prefix('[') {
        match after.split_once(']') {
            Some((inner, _port)) => inner,
            None => return false, // malformed bracketed authority
        }
    } else {
        authority.rsplit_once(':').map_or(authority, |(h, _)| h)
    };

    host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1"
}

/// `application/x-www-form-urlencoded` body from key/value pairs (both encoded).
fn encode_form(form: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (i, (k, v)) in form.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        out.push_str(&encode(k));
        out.push('=');
        out.push_str(&encode(v));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock_idp::MockIdp;

    #[test]
    fn authorize_url_contains_required_params_and_encodes_them() {
        let client = OAuthClient::new(
            Provider::google(),
            "client-123",
            "topsecret",
            "https://app.example.com/callback",
        );
        // A state with characters that MUST be percent-encoded.
        let url = client.authorize_url("st@te/with spaces&x", &["email", "profile"]);

        assert!(url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("client_id=client-123"));
        // redirect_uri must be percent-encoded (`://` and `/` encoded).
        assert!(
            url.contains("redirect_uri=https%3A%2F%2Fapp.example.com%2Fcallback"),
            "redirect_uri not encoded: {url}"
        );
        // scope is space-joined then encoded (space → %20).
        assert!(url.contains("scope=email%20profile"), "scope wrong: {url}");
        // state special chars encoded; the raw form must NOT appear.
        assert!(
            url.contains("state=st%40te%2Fwith%20spaces%26x"),
            "state not encoded: {url}"
        );
        assert!(!url.contains("st@te"), "raw state leaked: {url}");
    }

    #[test]
    fn authorize_url_empty_scopes_uses_provider_defaults() {
        let client = OAuthClient::new(Provider::github(), "id", "sec", "https://x/cb");
        let url = client.authorize_url("s", &[]);
        // GitHub defaults: read:user user:email → "read%3Auser%20user%3Aemail".
        assert!(
            url.contains("scope=read%3Auser%20user%3Aemail"),
            "default scopes not applied: {url}"
        );
    }

    #[test]
    fn pkce_url_carries_s256_challenge_matching_the_verifier() {
        let client = OAuthClient::new(Provider::google(), "id", "sec", "https://x/cb");
        let (url, verifier) = client.authorize_url_pkce("state-1", &["openid"]);

        assert!(
            url.contains("code_challenge_method=S256"),
            "missing method: {url}"
        );
        // The challenge in the URL must equal S256(verifier), percent-encoded.
        let expected = encode(&verifier.challenge());
        assert!(
            url.contains(&format!("code_challenge={expected}")),
            "challenge does not match verifier: {url}"
        );
        // Sanity: the challenge is NOT the raw verifier (it is hashed).
        assert_ne!(verifier.challenge(), verifier.as_str());
    }

    #[tokio::test]
    async fn exchange_code_and_refresh_happy_path_via_mock_transport() {
        let idp = MockIdp::new();
        let (access, refresh) = idp.issue_code("auth-code-1");

        let client = OAuthClient::new(
            Provider::google(),
            "id",
            "sec",
            "https://app.example.com/cb",
        )
        .with_transport(idp.token_transport());

        let token = client.exchange_code("auth-code-1", None).await.unwrap();
        assert_eq!(token.access_token, access);
        assert_eq!(token.refresh_token.as_deref(), Some(refresh.as_str()));

        // Refresh returns a *new* access token (deterministic mint, so it differs).
        let refreshed = client.refresh(&refresh).await.unwrap();
        assert_ne!(refreshed.access_token, access);
        assert!(!refreshed.access_token.is_empty());
    }

    #[tokio::test]
    async fn oauth_error_response_is_non_500_and_never_leaks_the_secret() {
        // A transport that always returns an RFC 6749 error body.
        struct ErrorTransport;
        impl TokenTransport for ErrorTransport {
            fn post_form<'a>(
                &'a self,
                _url: &'a str,
                _form: &'a [(&'a str, &'a str)],
            ) -> TokenFuture<'a> {
                Box::pin(async {
                    parse_token_body(
                        br#"{"error":"invalid_grant","error_description":"code expired"}"#,
                    )
                })
            }
        }

        let client = OAuthClient::new(
            Provider::google(),
            "id",
            "super-secret-value-xyz",
            "https://x/cb",
        )
        .with_transport(Arc::new(ErrorTransport));

        let err = client.exchange_code("dead", None).await.unwrap_err();
        // Non-500: a rejected grant is a client error.
        assert_eq!(err.status().as_u16(), 400, "must not be a 500");
        assert_eq!(err.code(), "JC0400");
        // The reason is surfaced...
        assert!(
            err.message().contains("invalid_grant"),
            "reason missing: {err}"
        );
        assert!(err.message().contains("code expired"));
        // ...but the client_secret never appears in the rendered error.
        let rendered = err.to_string();
        assert!(
            !rendered.contains("super-secret-value-xyz"),
            "client_secret leaked into error: {rendered}"
        );
    }

    #[test]
    fn token_response_round_trips_through_serde_for_at_rest_encryption() {
        let token = TokenResponse {
            access_token: "at".into(),
            token_type: "Bearer".into(),
            refresh_token: Some("rt".into()),
            expires_in: Some(3600),
            scope: Some("email".into()),
        };
        let json = serde_json::to_string(&token).unwrap();
        let back: TokenResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(token, back);

        // A minimal provider body (only access_token) still parses, defaulting
        // token_type and leaving the options None.
        let minimal: TokenResponse = serde_json::from_str(r#"{"access_token":"only"}"#).unwrap();
        assert_eq!(minimal.access_token, "only");
        assert_eq!(minimal.token_type, "Bearer");
        assert!(minimal.refresh_token.is_none());
    }

    #[test]
    fn token_response_debug_redacts_access_and_refresh_tokens() {
        // The redacting Debug must NEVER print the bearer material — a `{:?}` or a
        // tracing line on a TokenResponse would otherwise leak the live tokens.
        let token = TokenResponse {
            access_token: "ACCESS-SECRET-abc123".into(),
            token_type: "Bearer".into(),
            refresh_token: Some("REFRESH-SECRET-xyz789".into()),
            expires_in: Some(3600),
            scope: Some("email".into()),
        };
        let rendered = format!("{token:?}");
        assert!(
            !rendered.contains("ACCESS-SECRET-abc123"),
            "access_token leaked through Debug: {rendered}"
        );
        assert!(
            !rendered.contains("REFRESH-SECRET-xyz789"),
            "refresh_token leaked through Debug: {rendered}"
        );
        // It still carries the redaction markers + the non-secret fields.
        assert!(
            rendered.contains("access_token: \"***\""),
            "got: {rendered}"
        );
        assert!(rendered.contains("<present>"), "got: {rendered}");
        assert!(rendered.contains("Bearer"), "got: {rendered}");

        // With no refresh token, the marker flips to <absent> (still no secret).
        let no_refresh = TokenResponse {
            access_token: "ANOTHER-ACCESS-SECRET".into(),
            token_type: "Bearer".into(),
            refresh_token: None,
            expires_in: None,
            scope: None,
        };
        let rendered = format!("{no_refresh:?}");
        assert!(
            !rendered.contains("ANOTHER-ACCESS-SECRET"),
            "got: {rendered}"
        );
        assert!(rendered.contains("<absent>"), "got: {rendered}");
    }

    #[test]
    fn loopback_http_guard_allows_only_loopback_plaintext_and_all_https() {
        // https:// is always fine, regardless of host.
        assert!(is_loopback_http_ok("https://oauth2.googleapis.com/token"));
        assert!(is_loopback_http_ok("HTTPS://EVIL.example.com/token"));

        // http:// is allowed ONLY to a loopback host (the mock-IdP escape hatch).
        assert!(is_loopback_http_ok("http://127.0.0.1:8080/token"));
        assert!(is_loopback_http_ok("http://localhost/token"));
        assert!(is_loopback_http_ok("http://LocalHost:3000/token"));
        assert!(is_loopback_http_ok("http://[::1]:9000/token"));

        // http:// to anything else is refused (TLS downgrade).
        assert!(!is_loopback_http_ok("http://evil.example.com/token"));
        assert!(!is_loopback_http_ok("http://oauth2.googleapis.com/token"));
        // A host that merely starts with "localhost" must not pass.
        assert!(!is_loopback_http_ok("http://localhost.evil.com/token"));
        assert!(!is_loopback_http_ok("http://127.0.0.1.evil.com/token"));
        // Unparseable / non-http(s) schemes are refused.
        assert!(!is_loopback_http_ok("ftp://127.0.0.1/token"));
        assert!(!is_loopback_http_ok("not-a-url"));
    }

    #[tokio::test]
    async fn real_http_transport_rejects_non_loopback_plaintext_without_a_network_call() {
        // A real HttpTransport must REFUSE an http:// exchange to a non-loopback
        // host BEFORE any socket is opened — the client_secret never leaves the
        // process. (No server is listening; the rejection is the guard, not a
        // connection error.)
        let provider = Provider {
            auth_url: "http://evil.example.com/authorize",
            token_url: "http://evil.example.com/token",
            default_scopes: &["openid"],
        };
        let client = OAuthClient::new(provider, "id", "super-secret-value", "http://app/cb")
            .with_transport(Arc::new(HttpTransport::new()));

        let err = client.exchange_code("any-code", None).await.unwrap_err();
        assert!(
            err.message().contains("refusing plaintext http"),
            "expected the loopback guard, got: {err}"
        );
        // The secret never appears in the rendered error.
        assert!(
            !err.to_string().contains("super-secret-value"),
            "secret leaked into error: {err}"
        );
    }

    #[test]
    fn secret_does_not_implement_debug_or_display() {
        // Compile-time-ish guard: this test documents the contract. `Secret` has
        // no Debug/Display, so the following would NOT compile:
        //   let _ = format!("{:?}", Secret::new("x"));
        // We assert the one supported access path works and nothing else.
        let s = Secret::new("abc");
        assert_eq!(s.expose(), "abc");
    }
}
