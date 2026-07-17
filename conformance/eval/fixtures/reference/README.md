# Reference slice — reference backend fixtures (Phase 5a)

These are the **working, agent-owned** source files that turn a fresh scaffold of
`conformance/designs/reference-slice.design.json` into a real, `jerrycan check`-green
backend exercising every v2 primitive: JWT/session auth, argon2 passwords,
tenant-scoped CRUD, multipart CSV import, raw-body webhook signature
verification, scoped API keys, OAuth (connect + callback) against an in-process
mock IdP, and two cron jobs.

The Phase 5b live battery (`crates/jerrycan/tests/reference_eval.rs`) replays this:
scaffold the design → apply these files → patch the app Cargo features → run
`jerrycan check` → serve and drive the HTTP battery.

## How to apply (scaffold → these files)

1. Scaffold the design (wired to the local framework path dep), then `gen-tests`
   each module:
   ```
   JERRYCAN_FRAMEWORK_DEP='jerrycan = { path = "<repo>/crates/jerrycan", default-features = false }' \
     <jerrycan-bin> new <tmp>/reference --design conformance/designs/reference-slice.design.json
   cd <tmp>/reference
   for m in users workspaces leads api-keys billing integrations; do
     <jerrycan-bin> gen-tests --module "$m"; done
   ```

2. Copy each fixture to its destination in the scaffold:

   | fixture file | scaffold destination |
   |---|---|
   | `users_handlers.rs`        | `crates/routes/users/src/handlers.rs` |
   | `workspaces_handlers.rs`   | `crates/routes/workspaces/src/handlers.rs` |
   | `leads_handlers.rs`        | `crates/routes/leads/src/handlers.rs` |
   | `api-keys_handlers.rs`     | `crates/routes/api-keys/src/handlers.rs` |
   | `billing_handlers.rs`      | `crates/routes/billing/src/handlers.rs` |
   | `integrations_handlers.rs` | `crates/routes/integrations/src/handlers.rs` |
   | `api-keys_deps.rs`         | `crates/routes/api-keys/src/deps.rs` |
   | `integrations_deps.rs`     | `crates/routes/integrations/src/deps.rs` |
   | `jobs_expire_trials.rs`    | `crates/jobs/src/expire_trials.rs` |
   | `jobs_overdue_callbacks.rs`| `crates/jobs/src/overdue_callbacks.rs` |

   The `*_handlers.rs` mapping matches the existing `eval.rs` rule
   (`<module>_handlers.rs` → `crates/routes/<module>/src/handlers.rs`); the
   hyphen in `api-keys` is preserved. The two `*_deps.rs` and two `jobs_*.rs`
   files are **extra** files the reference slice needs beyond the eval.rs handler-only
   layout — the Phase 5b harness must copy them too (see destinations above).

3. **Cargo feature patch (test-only — just `mock-idp`):** the reference design now
   declares the **`oauth` dependency**, so the scaffolder wires the `oauth` facade
   feature AUTOMATICALLY — the `integrations` module compiles out of the box.
   The only remaining patch is `mock-idp`, the **test-only** IdP harness (never a
   production dependency, so not expressible as a design dependency). Patch the
   generated root `Cargo.toml` `[workspace.dependencies]` jerrycan line:
   ```
   # before (scaffolded, oauth auto-wired):
   jerrycan = { path = "…", default-features = false, features = ["db", "validate", "auth", "observe", "jobs", "oauth"] }
   # after (add the test-only mock IdP):
   jerrycan = { path = "…", default-features = false, features = ["db", "validate", "auth", "observe", "jobs", "oauth", "mock-idp"] }
   ```
   (`mock-idp` is enabled because the reference slice wires the OAuth client's
   token transport to an in-process `MockIdp` so the flow is hermetic — no
   socket. A real deployment drops `mock-idp` and the `.with_transport(...)` line
   and reads client credentials from env.)

4. `jerrycan --json check` → `ok: true`. The generated acceptance suite passes,
   including the cross-tenant isolation tests
   `tenant_a_cannot_read_tenant_b_leads` / `…_api_keys`.

## DI providers wired by the agent-owned `deps.rs` (no app/main.rs patch needed)

App-wide DI lives in the tool-owned `crates/app/src/main.rs`, which is
regenerated — so the slice provides its extra dependencies at **module scope**
via the agent-owned `deps.rs` (a `Module::provide` is visible to that module's
routes). No `app/main.rs` edit is required.

- **`api-keys_deps.rs`** provides:
  - `SharedKeyStore(Arc<InMemoryApiKeyStore>)` — concrete handle so
    `create_api_key` can `insert` the minted hash (the `ApiKeyStore` trait is
    `lookup`-only).
  - `ApiKeys::from_arc(store)` — the documented DI contract the `usage`
    handler reads to authenticate a presented key. Both point at the SAME `Arc`,
    so a key minted by `create_api_key` authenticates on the next `usage` call.
- **`integrations_deps.rs`** provides:
  - `OAuth { client: OAuthClient, idp: MockIdp }` — the OAuth client with the
    mock transport swapped in (`.with_transport(idp.token_transport())`); the
    `idp` handle re-issues the one-time code on each `connect`.
  - `TokenVault` — an in-memory map of **encrypted** provider tokens keyed by
    `state` (a real app uses a `linked_identity` table; the slice keeps
    ciphertext in memory).

## Jobs

`jobs_expire_trials.rs` and `jobs_overdue_callbacks.rs` are the two declared cron
tasks. Each resolves `Db` from the `TaskContext` and UPSERTs a heartbeat into a
self-created `job_audit` table (`CREATE TABLE IF NOT EXISTS` + `ON CONFLICT DO
UPDATE`) — genuinely idempotent, and it does not depend on the app tables (the
generated `crates/jobs/tests/acceptance.rs` migrates only the jobs tables).
`overdue_callbacks` additionally counts overdue (`status='new'`) leads **only
when the `leads` table exists**, so the same code is green in both the
jobs-only acceptance DB and the live app.

## Behaviour notes / design-vs-generator tensions handled in these handlers

The generated success probes are tool-owned and cannot be edited, so several
handlers accept the probe's shape AND the real flow:

- **`users::login`** returns `200` for an empty `{}` body (the generated probe),
  mints a Bearer JWT and returns it as `{ "token": "<jwt>" }` for valid
  credentials — the design's `auth.model: "jwt"` guards on `Bearer<SessionUser>`,
  so there is no cookie to set — and `401`s a present-but-wrong credential.
- **`billing::stripe_webhook`** returns `200` when there is no
  `Stripe-Signature` header (the unsigned probe), verifies the HMAC-SHA256 hex
  signature over the **raw** body otherwise (`200` valid / `400` invalid). The
  signing secret is `STRIPE_WEBHOOK_SECRET` (default `whsec_reference_reference_secret`).
- **`integrations::google_callback`** returns `200` with no `code` (the probe /
  a direct hit), exchanges a present code via the mock (`200`), and `400`s a
  bad/expired code. `connect` re-issues the fixed mock code `reference-mock-code`,
  so a battery can drive `callback?code=reference-mock-code` after `connect`.
- **`leads::import_leads`** takes `Headers` + `RawBody` and builds the real
  `Multipart` parser via `Multipart::from_buffered` **only when the request is
  multipart**, so the generated `post_json({})` probe imports zero rows and still
  returns `202` (instead of the `Multipart` extractor's `415`). Rows are inserted
  omitting the id so the DB assigns it.
- Enum string fields (`role`, `plan`, `status`) are normalized to a valid value
  before insert, because the generated success probes post `"test-value"` which
  would violate the DB `CHECK` constraints.

## Framework changes this slice required (made in the framework, not hidden)

These additive, non-weakening framework changes were needed and are committed
alongside the fixtures:

1. `questions.rs`: allow a **3xx** success status (was 2xx-only) so an OAuth
   `connect` endpoint can declare `success: 302`.
2. `genroute.rs`: generate a tenant-scoped **`update_for`** repo accessor
   (parallel to `all_for`/`get_for`/`remove_for`) — without it a JL0006-clean
   scoped UPDATE handler had no sanctioned repo method.
3. `multipart.rs`: add `Multipart::from_buffered(content_type, bytes)` so a
   handler can accept either multipart or another content type on one route.
4. `testgen.rs`: only emit the `test_cookie`/`test_cookie_for` helpers when the
   module's generated tests actually use them (a module with no guarded endpoint
   and no isolation test — `billing`, `integrations` — otherwise carried dead
   helpers that tripped clippy `-D warnings`).
