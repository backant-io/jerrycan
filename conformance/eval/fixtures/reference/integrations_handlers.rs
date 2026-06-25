//! Handlers for `integrations` — Google OAuth connect + callback. Hermetic: the
//! `OAuthClient` is wired (in deps.rs) to an in-process `MockIdp`, so the
//! exchange runs with no network. Tokens are encrypted at rest before storage.
use super::deps::{MOCK_CODE, OAuth, TokenVault};
use jerrycan::Response;
use jerrycan::auth::Auth;
use jerrycan::http::{HeaderValue, StatusCode, header};
use jerrycan::prelude::*;

/// A CSRF `state` value. Deterministic-enough for the slice; a real app uses a
/// CSPRNG and stores it in the session to compare on callback.
fn new_state() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("st_{nanos}")
}

/// GET /auth/google/connect — 302 the browser to Google's consent screen.
/// `authorize_url` is pure (no network); we also re-issue the mock's one-time
/// code so a subsequent `callback?code=<MOCK_CODE>` round-trip is exchangeable.
pub(crate) async fn google_connect(oauth: Dep<OAuth>) -> Result<Response> {
    let state = new_state();
    oauth.idp.issue_code(MOCK_CODE);
    let url = oauth
        .client
        .authorize_url(&state, &["openid", "email", "https://www.googleapis.com/auth/calendar.readonly"]);
    let mut res = jerrycan::http::Response::new(jerrycan::JcBody::empty());
    *res.status_mut() = StatusCode::FOUND; // 302
    res.headers_mut()
        .insert(header::LOCATION, HeaderValue::from_str(&url).unwrap());
    Ok(res)
}

/// The callback query: optional so the generated success probe (no params)
/// deserializes; a real callback carries both.
#[derive(serde::Deserialize, Default)]
pub(crate) struct CallbackQuery {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    state: Option<String>,
}

/// GET /auth/google/callback — exchange the code for tokens, encrypt them at
/// rest, and store the ciphertext. No code (a direct hit / the generated probe)
/// → 200 landing. A present-but-bad/expired code → 400.
pub(crate) async fn google_callback(
    oauth: Dep<OAuth>,
    auth: Dep<Auth>,
    vault: Dep<TokenVault>,
    Query(q): Query<CallbackQuery>,
) -> Result<Json<serde_json::Value>> {
    let Some(code) = q.code.filter(|c| !c.is_empty()) else {
        return Ok(Json(serde_json::json!({ "connected": false })));
    };
    // Exchange via the (mock-backed) OAuth client. A bad/expired code is a
    // non-500 OAuth error (400) — propagate it as 400, never a 500.
    let token = oauth
        .client
        .exchange_code(&code, None)
        .await
        .map_err(|_| Error::bad_request("authorization code is missing, bad, or expired"))?;
    // Encrypt the token before persisting — the column holds ciphertext only.
    let ciphertext = auth.tokens().encode(&token)?;
    let state = q.state.unwrap_or_else(|| "default".to_string());
    vault
        .0
        .lock()
        .expect("token vault mutex poisoned")
        .insert(state, ciphertext);
    Ok(Json(serde_json::json!({
        "connected": true,
        "provider": "google",
        "token_type": token.token_type,
    })))
}
