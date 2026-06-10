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

## Variations
- Passwords: `jerrycan::auth::hash_password(pw)` → PHC string for storage;
  `verify_password(pw, &stored)` → bool. Always argon2id, random salt.
- JWT: `Bearer<Claims>` extracts+verifies `Authorization: Bearer <token>`; mint
  with `jerrycan::auth::jwt::encode(&claims, auth.jwt_key())`. Include `exp`.
- Secret: `Auth::from_env()` reads `JERRYCAN_SECRET` (>= 32 bytes). In
  production (`JERRYCAN_ENV=prod`) a missing/short secret is a startup error.

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
