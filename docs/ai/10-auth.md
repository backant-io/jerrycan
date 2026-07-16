# Authentication

## Purpose
`jerrycan::auth` provides password hashing (argon2), encrypted session cookies,
HS256 JWTs, and role guards. Enable with the design dependency `"auth"` plus an
`auth` block (`{ "model": "session"|"jwt", "roles": [...] }`), or `jerrycan add auth`.
Guards are dependencies (spec §4.3): a `Session`/`Bearer` extractor in a handler
signature is the gate.

## Signature
```rust
# use jerrycan::prelude::*;
use jerrycan::auth::{require_role, Session};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct User { id: i64, role: String }

async fn dashboard(Session(user): Session<User>) -> Result<String> {
    Ok(format!("welcome user {}", user.id))   // unauthenticated → 401 automatically
}

async fn admin_delete(Session(user): Session<User>) -> Result<NoContent> {
    require_role(&user.role, "admin")?;        // wrong role → 403
    Ok(NoContent)
}
# let _ = (dashboard, admin_delete);
```

## Minimal example
```rust
# use jerrycan::prelude::*;
# use jerrycan::auth::{Auth, Session};
# use serde::{Deserialize, Serialize};
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
#[derive(Serialize, Deserialize)]
struct User { id: i64, role: String }
async fn me(Session(u): Session<User>) -> Json<i64> { Json(u.id) }

let auth = Auth::with_secret("a-very-long-development-secret-string!!");
let cookie = auth.sessions().set_cookie(&User { id: 42, role: "user".into() }).unwrap();
let cookie_pair = cookie.split(';').next().unwrap().to_string();

let t = App::new().extend(auth).route("/me", get(me)).into_test();
assert_eq!(t.get("/me").await.status(), jerrycan::http::StatusCode::UNAUTHORIZED);
assert_eq!(t.get_with("/me", &[("cookie", &cookie_pair)]).await.json::<i64>(), 42);
# }); }
```

## The generated guard follows `auth.model`
The design's `auth.model` decides which guard your scaffolded handlers extract —
the generated `shared::CurrentUser` alias, which every guarded REST route uses:

| `auth.model` | generated `CurrentUser` | credential the guard checks | rejects with |
|---|---|---|---|
| `"session"` | `Session<SessionUser>` | the `jerrycan_session` cookie (AEAD-encrypted) | `401` if absent/invalid |
| `"jwt"` | `Bearer<SessionUser>` | the `Authorization: Bearer <jwt>` header (HS256) | `401` if absent/invalid |

`SessionUser` is identical for both models (`{ id: String, role: String }` — `id`
is the STRINGIFIED user pk, so an integer or a uuid Supabase id round-trips). A
`jwt` design gets REAL bearer-token guards; a Supabase-migrated app (whose auth
is always JWT) is served correctly. Pick the model up front — REST routes,
realtime, and the OpenAPI `securityScheme` all follow it.

Under `"jwt"`, the agent writes the login that mints the token (there is no
cookie to set). Mint over the same `SessionUser` shape with `Auth::jwt_key()`,
and ALWAYS include an `exp` claim (unix seconds) — `decode` enforces it when
present, and `Bearer<SessionUser>` ignores the extra field on the way back in:
```rust
# use jerrycan::prelude::*;
# use jerrycan::auth::{Auth, Bearer};
# use serde::{Deserialize, Serialize};
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
// The generated shared::SessionUser; a jwt design guards on Bearer<SessionUser>.
#[derive(Serialize, Deserialize, Clone)]
struct SessionUser { id: String, role: String }
async fn me(Bearer(u): Bearer<SessionUser>) -> Json<String> { Json(u.id) }

// The agent-written login mints the bearer token (add `exp` to the payload).
#[derive(Serialize)]
struct Claims { id: String, role: String, exp: u64 }
let auth = Auth::with_secret("a-very-long-development-secret-string!!");
let token = jerrycan::auth::jwt::encode(
    &Claims { id: 42.to_string(), role: "user".into(), exp: 9_999_999_999 },
    auth.jwt_key(),
).unwrap();

let t = App::new().extend(auth).route("/me", get(me)).into_test();
assert_eq!(t.get("/me").await.status(), jerrycan::http::StatusCode::UNAUTHORIZED);
assert_eq!(
    t.get_with("/me", &[("authorization", &format!("Bearer {token}"))]).await.json::<String>(),
    "42",
);
# }); }
```

## Variations
- Passwords: `jerrycan::auth::hash_password(pw)` → `Result<String>` (a PHC
  string for storage); `verify_password(pw, &stored)` → `Result<bool>`
  (`Ok(true)` on match, `Ok(false)` on mismatch — propagate with `?`, then
  branch on the bool). Always argon2id, random salt.
- JWT: `Bearer<Claims>` extracts+verifies `Authorization: Bearer <token>`; mint
  with `jerrycan::auth::jwt::encode(&claims, auth.jwt_key())`. Include `exp`.
- Secret: `Auth::from_env()` reads `JERRYCAN_SECRET` (>= 32 bytes). The insecure
  built-in dev key is used ONLY when `JERRYCAN_ENV` is unset/empty or a dev
  marker (`dev`/`development`/`test`/`local`); any other value (incl. any
  production spelling) requires a real secret — a missing/short one is a startup
  error (fail closed).

## Verifying webhook signatures
A webhook is an unauthenticated POST from a third party; the only proof it's
genuine is an HMAC the provider computes over the EXACT bytes it sent. Take the
body with `RawBody` (see 03-extractors) — never re-parse and re-serialize it, or
the digest won't match. `jerrycan::auth::webhook` provides constant-time
verifiers; the secret is a per-provider value, modelled as a dependency you
`.provide` from the environment (`std::env::var`, the same source `JERRYCAN_SECRET`
uses).

**Stripe** signs `"{timestamp}.{body}"` with HMAC-SHA256 and sends
`Stripe-Signature: t=<unix-seconds>,v1=<hex>`. Verify the digest, and bound the
timestamp's age (via `Dep<Clock>`) so a captured request can't be replayed:
```rust
# use jerrycan::prelude::*;
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
use jerrycan::auth::webhook::verify_sha256_hex;
use std::time::{Duration, UNIX_EPOCH};

struct StripeSecret(String);   // .provide(StripeSecret(std::env::var("STRIPE_WEBHOOK_SECRET")?))

async fn stripe(
    headers: Headers,
    clock: Dep<Clock>,
    secret: Dep<StripeSecret>,
    RawBody(body): RawBody,
) -> Result<NoContent> {
    let unauthorized = || Error::new(jerrycan::http::StatusCode::UNAUTHORIZED, "JC0401", "bad signature");
    let header = headers.get("stripe-signature").ok_or_else(unauthorized)?;
    let mut ts = None;
    let mut v1 = None;
    for part in header.split(',') {
        match part.split_once('=') {
            Some(("t", v)) => ts = v.parse::<u64>().ok(),
            Some(("v1", v)) => v1 = Some(v),
            _ => {}
        }
    }
    let (ts, v1) = (ts.ok_or_else(unauthorized)?, v1.ok_or_else(unauthorized)?);

    // Reject stale timestamps (replay) — Clock is injectable, so tests can move it.
    let now = clock.now().duration_since(UNIX_EPOCH).map_err(|_| Error::internal("clock"))?;
    if now.saturating_sub(Duration::from_secs(ts)) > Duration::from_secs(300) {
        return Err(unauthorized());
    }

    let signed = format!("{ts}.{}", String::from_utf8_lossy(&body));
    if !verify_sha256_hex(secret.0.as_bytes(), signed.as_bytes(), v1) {
        return Err(unauthorized());
    }
    Ok(NoContent)   // genuine, fresh event
}
# let _ = stripe;
# }); }
```

**Signing (for tests, or producing your own webhook).** The same module exposes
the producer side: `sign_sha256_hex(secret, message) -> String` (hex HMAC-SHA256,
what Stripe's `v1=` carries) and `sign_sha1_base64(secret, message) -> String`
(base64 HMAC-SHA1, what Twilio sends). Use them to forge a valid signature when
testing a webhook handler — `verify_*` is the exact inverse:
```rust
# use jerrycan::prelude::*;
use jerrycan::auth::webhook::{sign_sha256_hex, verify_sha256_hex};

let secret = b"whsec_test";
let body = b"{\"event\":\"invoice.paid\"}";
let sig = sign_sha256_hex(secret, body);              // hex digest, like Stripe's v1=
assert!(verify_sha256_hex(secret, body, &sig));       // round-trips
assert!(!verify_sha256_hex(secret, body, "deadbeef")); // a wrong sig is rejected
```

**Twilio** sends `X-Twilio-Signature`: base64 HMAC-SHA1 over the full request
URL with the POST form params appended, sorted by key (`key+value`, no
separators). The body is `application/x-www-form-urlencoded`, parsed with the
re-exported `serde_urlencoded`:
```rust
# use jerrycan::prelude::*;
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
use jerrycan::auth::webhook::verify_sha1_base64;

struct TwilioToken(String);   // .provide(TwilioToken(std::env::var("TWILIO_AUTH_TOKEN")?))
struct WebhookUrl(String);    // the publicly-reachable URL Twilio is configured to call

async fn twilio(
    headers: Headers,
    token: Dep<TwilioToken>,
    url: Dep<WebhookUrl>,
    RawBody(body): RawBody,
) -> Result<NoContent> {
    let unauthorized = || Error::new(jerrycan::http::StatusCode::UNAUTHORIZED, "JC0401", "bad signature");
    let signature = headers.get("x-twilio-signature").ok_or_else(unauthorized)?;

    let mut params: Vec<(String, String)> = jerrycan::serde_urlencoded::from_bytes(&body)
        .map_err(|_| Error::bad_request("malformed form body"))?;
    params.sort_by(|a, b| a.0.cmp(&b.0));               // sort by key, then append key+value
    let mut message = url.0.clone();
    for (k, v) in &params {
        message.push_str(k);
        message.push_str(v);
    }

    if !verify_sha1_base64(token.0.as_bytes(), message.as_bytes(), signature) {
        return Err(unauthorized());
    }
    Ok(NoContent)
}
# let _ = twilio;
# }); }
```

## Errors you'll hit
- Missing/invalid session cookie or bearer token → `401 JC0401`.
- Authenticated but wrong role (`require_role`) → `403 JC0403`.
- Tampered cookie/JWT → `401` (AEAD/signature rejects it; never a panic).

## Anti-patterns
- Don't put secrets in a JWT — it's signed, not encrypted (readable by anyone).
  Server-private state goes in the AEAD session cookie.
- Don't check auth inside handler bodies with ad-hoc logic — declare a
  `Session`/`Bearer` guard in the signature so the contract is visible.
- **A JWT with no `exp` claim never expires.** `decode` only enforces expiry
  when the payload actually carries an `exp` (unix seconds); a token minted
  without one is accepted forever as long as the signature verifies. Always set
  `exp` when minting — an omitted expiry is a silent, permanent credential.
