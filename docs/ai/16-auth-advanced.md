# Advanced auth: OAuth2, token-at-rest, API keys

## Purpose
`jerrycan::auth` (the `oauth` feature) adds the auth surface beyond sessions and
JWTs: an OAuth2 authorization-code client with provider presets and refresh, an
encrypted **token-at-rest** codec with key rotation, and scoped **API keys**.
Enable the OAuth client with the `oauth` feature (it pulls in `auth`); the
API-key and token-at-rest pieces are part of `auth` and need no extra feature.
This page is the recipe layer — the pieces are library types you wire yourself,
not generated code.

## OAuth2 connect + refresh
A `Provider` is **config, not code**: it carries the two endpoints and default
scopes, so adding a provider is a data change, never a new branch. Presets:
`Provider::google()`, `github()`, `hubspot()`, `salesforce()`. Build an
`OAuthClient` with your app credentials, send the user to `authorize_url` (use
the PKCE variant for public clients), then `exchange_code` on the callback:
```rust,no_run
# use jerrycan::prelude::*;
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
use jerrycan::auth::{OAuthClient, Provider};

// client_secret comes from the environment, never a literal — and lives in a
// `Secret` newtype that has no Debug/Display, so it can't reach a log line.
let client = OAuthClient::new(
    Provider::google(),
    "my-client-id",
    "my-client-secret",                       // std::env::var("GOOGLE_CLIENT_SECRET")?
    "https://app.example.com/oauth/callback",
);

// 1. Redirect the browser here. `state` is YOUR CSRF token: generate it, store
//    it in the session, and compare it when the callback returns. With PKCE,
//    also stash the returned verifier server-side keyed by `state`.
let (url, verifier) = client.authorize_url_pkce("a-random-csrf-state", &["openid", "email"]);
let _ = url;   // 302 Location: url

// 2. On the callback (?code=...&state=...), after verifying `state` matched,
//    exchange the code for tokens (network call to the provider).
let token = client.exchange_code("the-code-from-the-callback", Some(&verifier)).await?;
let _ = token.access_token;

// 3. Later, when the access token has expired, mint a new one from the refresh
//    token (provider returns one only with offline/refresh scope).
if let Some(refresh_token) = token.refresh_token.as_deref() {
    let fresh = client.refresh(refresh_token).await?;
    let _ = fresh.access_token;
}
# Ok::<(), jerrycan::Error>(())
# }).unwrap(); }
```
`authorize_url` is **pure** (no network) — every parameter is percent-encoded, so
you can build and assert it without a transport. The exchange/refresh calls hit
the provider over HTTPS (hyper + rustls), so the block above is `no_run`. For a
fully hermetic test, inject the mock IdP transport instead:
```rust
# use jerrycan::prelude::*;
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
use jerrycan::auth::{MockIdp, OAuthClient, Provider};

// authorize_url is pure: assert it without any transport.
let client = OAuthClient::new(
    Provider::github(),
    "id",
    "secret",
    "https://app.example.com/cb",
);
let url = client.authorize_url("st@te x", &["read:user"]);
assert!(url.starts_with("https://github.com/login/oauth/authorize?"));
assert!(url.contains("response_type=code"));
assert!(url.contains("state=st%40te%20x"));        // special chars are encoded

// A deterministic in-process IdP — no socket. Pre-issue a code, then exchange
// it through the SAME OAuthClient with the mock transport swapped in.
let idp = MockIdp::new();
let (access, refresh) = idp.issue_code("auth-code-1");
let client = client.with_transport(idp.token_transport());

let token = client.exchange_code("auth-code-1", None).await.unwrap();
assert_eq!(token.access_token, access);

// The code is one-time: a second exchange fails with a non-500 OAuth error.
assert_eq!(client.exchange_code("auth-code-1", None).await.unwrap_err().status().as_u16(), 400);

// refresh mints a fresh access token.
let refreshed = client.refresh(&refresh).await.unwrap();
assert_ne!(refreshed.access_token, access);
# }); }
```

## Linked identities (recommended schema)
jerrycan does NOT generate an identity table — you own it. The pattern that keeps
one local user linked to many providers (and survives an email change at the
provider) is a join row keyed by `(provider, external_id)`, with the provider's
**stable subject id** as `external_id` (Google `sub`, GitHub numeric `id`) —
never the email. Store the encrypted token blob alongside it (see token-at-rest
below). A recommended shape:
```sql
CREATE TABLE linked_identity (
    id           BIGSERIAL PRIMARY KEY,
    user_id      BIGINT      NOT NULL REFERENCES users(id),
    provider     TEXT        NOT NULL,            -- 'google' | 'github' | ...
    external_id  TEXT        NOT NULL,            -- the provider's stable subject id
    token_enc    TEXT        NOT NULL,            -- auth.tokens().encode(&TokenResponse)
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (provider, external_id),               -- one local user per provider identity
    UNIQUE (user_id, provider)                    -- one identity per provider per user
);
```
On the callback: resolve the provider's subject id, `SELECT ... WHERE provider=$1
AND external_id=$2`; if present, log that `user_id` in; if absent, link to the
signed-in user (or create one). Re-write `token_enc` after every `exchange_code`
/ `refresh`.

## Encrypted token-at-rest + key rotation
Never store a provider access/refresh token in plaintext. `auth.tokens()` is a
rotation-aware AEAD codec (ChaCha20-Poly1305) keyed independently from session
cookies, so a leaked session key can't read tokens and vice-versa. Encrypt a
`TokenResponse` before persisting the ciphertext, decode it on read:
```rust
# use jerrycan::prelude::*;
use jerrycan::auth::{Auth, TokenResponse};

let auth = Auth::with_secret("a-very-long-development-secret-string!!");

let token = TokenResponse {
    access_token: "ya29.a0...".into(),
    token_type: "Bearer".into(),
    refresh_token: Some("1//refresh...".into()),
    expires_in: Some(3600),
    scope: Some("openid email".into()),
};

// Encrypt → store this opaque string in the `token_enc` column.
let ciphertext = auth.tokens().encode(&token).unwrap();
assert!(!ciphertext.contains("ya29"), "the token never appears in the ciphertext");

// On read, decode back into the typed response.
let back: TokenResponse = auth.tokens().decode(&ciphertext).unwrap();
assert_eq!(back.access_token, "ya29.a0...");
```

### Rotation runbook (rotate `JERRYCAN_SECRET` without logging users out)
The master secret seeds the session, token-at-rest, and JWT keys via distinct
labels. Rotation is **multi-key decrypt**: the *primary* secret encrypts new
data; *retired* secrets only decrypt pre-rotation data. To rotate:

1. Generate a new secret (>= 32 bytes).
2. Move the CURRENT `JERRYCAN_SECRET` value into `JERRYCAN_SECRET_OLD`
   (comma-separated; you can list several) and set `JERRYCAN_SECRET` to the new
   value. `from_env()` reads both — old entries become decrypt-only fallbacks.
3. Deploy. New sessions/tokens are encrypted under the new key; everything
   minted under the old key still decodes, so **nobody is logged out**.
4. Once every old session/token has expired or been re-encrypted, drop the
   retired secret from `JERRYCAN_SECRET_OLD`. From then on, data encrypted under
   it fails to decrypt (a real retirement) — exactly the invalidation you want.

`Auth::with_secrets(primary, retired)` is the explicit form `from_env` builds:
```rust
# use jerrycan::prelude::*;
use jerrycan::auth::{Auth, TokenResponse};

let tok = || TokenResponse {
    access_token: "at".into(), token_type: "Bearer".into(),
    refresh_token: None, expires_in: None, scope: None,
};

// Encrypted under the OLD secret, before rotation.
let before = Auth::with_secret("old-secret-of-at-least-thirty-two-bytes!!");
let ciphertext = before.tokens().encode(&tok()).unwrap();

// After rotation: NEW is primary, OLD is retired (decrypt-only).
let after = Auth::with_secrets(
    "new-secret-of-at-least-thirty-two-bytes!!",
    &["old-secret-of-at-least-thirty-two-bytes!!"],
);
// The pre-rotation ciphertext still decodes — rotation didn't log anyone out.
assert_eq!(after.tokens().decode::<TokenResponse>(&ciphertext).unwrap().access_token, "at");
```

## Scoped API keys
API keys authenticate machine clients. Mint a high-entropy key, store **only its
hash**, and show the plaintext exactly once. The `ApiKey` extractor reads the key
from `Authorization: Bearer <key>` or `X-API-Key: <key>`, looks up the hash in
the store you provide, and yields the record; `require_scope` gates the handler.

Mint → store the hash (the plaintext is never persisted):
```rust
# use jerrycan::prelude::*;
use jerrycan::auth::{mint, ApiKeyRecord, InMemoryApiKeyStore};

let minted = mint("sk_live");                       // sk_live_<base64url-random>
// Show `minted.plaintext` to the operator ONCE; persist only the hash + scopes.
let store = InMemoryApiKeyStore::new();             // a DB-backed ApiKeyStore in prod
store.insert(ApiKeyRecord {
    id: 1,
    prefix: minted.prefix.clone(),
    hash: minted.hash.clone(),                      // hex SHA-256, the lookup column
    scopes: vec!["reports:read".into()],
});
assert!(!minted.hash.contains(&minted.plaintext));  // the plaintext is not in the hash
```

Wire the store with `ApiKeys`, guard the handler with `ApiKey`, gate with
`require_scope`:
```rust
# use jerrycan::prelude::*;
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
use jerrycan::auth::{mint, ApiKey, ApiKeyRecord, ApiKeys, InMemoryApiKeyStore};

// A scope-gated handler: the `ApiKey` guard authenticates; require_scope authorizes.
async fn reports(ApiKey(key): ApiKey) -> Result<Json<String>> {
    key.require_scope("reports:read")?;             // missing scope → 403 JC0403
    Ok(Json(key.prefix))
}

let store = InMemoryApiKeyStore::new();
let scoped = mint("sk_live");
store.insert(ApiKeyRecord {
    id: 1, prefix: scoped.prefix.clone(), hash: scoped.hash.clone(),
    scopes: vec!["reports:read".into()],
});

// Provide the store as `ApiKeys` (the documented DI contract for the extractor).
let app = App::new()
    .provide(ApiKeys::new(store))
    .route("/reports", get(reports));
let t = app.into_test();

// A valid key with the scope → 200.
let ok = t.get_with("/reports", &[("x-api-key", &scoped.plaintext)]).await;
assert_eq!(ok.status(), jerrycan::http::StatusCode::OK);

// No key, or an unknown key → 401 (authentication fails before any scope check).
assert_eq!(t.get("/reports").await.status(), jerrycan::http::StatusCode::UNAUTHORIZED);
let unknown = t.get_with("/reports", &[("x-api-key", "sk_live_not-a-real-key")]).await;
assert_eq!(unknown.status(), jerrycan::http::StatusCode::UNAUTHORIZED);

// A key that authenticates but carries the wrong scope → 403 (require_scope).
let weak = mint("sk_weak");
// re-insert into a fresh store wired into a fresh app to show the 403 path
let store2 = InMemoryApiKeyStore::new();
store2.insert(ApiKeyRecord {
    id: 2, prefix: weak.prefix.clone(), hash: weak.hash.clone(),
    scopes: vec!["other:read".into()],              // NOT reports:read
});
let t2 = App::new().provide(ApiKeys::new(store2)).route("/reports", get(reports)).into_test();
let forbidden = t2.get_with("/reports", &[("x-api-key", &weak.plaintext)]).await;
assert_eq!(forbidden.status(), jerrycan::http::StatusCode::FORBIDDEN);
# }); }
```
A wildcard scope `"*"` is an admin grant that passes every `require_scope` check.
In production back the store with your database: implement `ApiKeyStore::lookup`
to `SELECT` by the hex hash and return the `ApiKeyRecord` (or `None`).

## Errors you'll hit
- An OAuth provider rejecting a grant (`{"error":"invalid_grant"}`) surfaces as
  `400 JC0400` naming the reason — never a 500, and never echoing your
  `client_secret`. Catch it at the callback and restart the flow.
- A transport/TLS failure on `exchange_code`/`refresh` is `500 JC0500` with a
  generic message (the request body, which carries the secret, is never logged).
- A token encrypted under a secret that is neither the primary nor a retired key
  fails to decode with `401 JC0401` — that's a fully-retired key (intended
  invalidation), not a bug.
- A missing/unknown/malformed API key → `401 JC0401`; a known key lacking the
  required scope → `403 JC0403`.

## Anti-patterns
- **Don't skip the `state` check.** The client forwards `state` but never
  validates it — comparing the returned `state` to the one you sent is YOUR CSRF
  defense. Drop it and you accept forged callbacks.
- **Don't store provider tokens in plaintext.** Always `auth.tokens().encode`
  before persisting; the column holds opaque ciphertext, never the bearer token.
- **Don't store an API key's plaintext.** Persist only `hash_key(plaintext)`;
  show the plaintext once. A stolen database then yields no usable keys.
- **Don't compare key hashes with `==`.** Use `verify` (constant-time digest
  compare); a `String ==` short-circuits and leaks a partial-match prefix length
  through timing.
- **Don't drop a retired secret too early.** Keep it in `JERRYCAN_SECRET_OLD`
  until all data it encrypted has aged out, or you log those users out.
