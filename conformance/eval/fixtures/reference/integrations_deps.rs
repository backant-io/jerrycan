//! Agent-owned: module-scoped dependencies and middleware.
//!
//! The integrations module owns the OAuth wiring. For a hermetic reference slice
//! the OAuth client's token transport is swapped for an in-process `MockIdp`
//! (the `mock-idp` feature), so `connect`/`callback` exercise the real
//! `OAuthClient` + token-at-rest codec with NO network. A real deployment would
//! drop the `.with_transport(mock…)` line and read client credentials from env.
//!
//! REQUIRES the generated app to enable the `oauth` + `mock-idp` facade features
//! (the design `dependencies` array can't express them — see the fixtures
//! README). Without them this module does not compile.
use jerrycan::auth::{MockIdp, OAuthClient, Provider};
use jerrycan::prelude::*;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// The fixed one-time code the slice's mock IdP exchanges. `connect` re-issues it
/// each call so a `callback?code=<this>` round-trip is always available.
pub(crate) const MOCK_CODE: &str = "reference-mock-code";

/// OAuth wiring shared by `connect` and `callback`: the client (mock transport)
/// and the IdP handle used to (re-)issue the one-time code.
#[derive(Clone)]
pub(crate) struct OAuth {
    pub client: OAuthClient,
    pub idp: MockIdp,
}

/// Encrypted provider tokens at rest, keyed by `state`. In a real app this is a
/// `linked_identity` table; in the slice it's an in-memory map of ciphertext.
#[derive(Clone, Default)]
pub(crate) struct TokenVault(pub Arc<Mutex<HashMap<String, String>>>);

/// Called by the tool-owned lib.rs — register module deps/middleware here;
/// regeneration never touches this file.
pub(crate) fn configure(module: Module) -> Module {
    let idp = MockIdp::new();
    idp.issue_code(MOCK_CODE);
    let client = OAuthClient::new(
        Provider::google(),
        "reference-client-id",
        "reference-client-secret",
        "http://127.0.0.1/integrations/auth/google/callback",
    )
    .with_transport(idp.token_transport());
    module
        .provide(OAuth { client, idp })
        .provide(TokenVault::default())
}
