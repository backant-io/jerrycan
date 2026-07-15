# Agent-eval fixes (issues #27–#35) — Implementation Plan

> **For agentic workers:** execute package-by-package (fresh implementer per package, task review after each). Packages are sequential: each lands as its own PR and merges before the next dependent package starts.

**Goal:** Fix the 9 defects found by the 2026-07-15 multi-model agent eval (issues #27–#35), per the maintainer's decisions: #27 design-time lint (not self-tenancy), #29 full Bearer-JWT guard implementation, #32 schema→399, #35 checklist-only.

**Architecture:** Five packages, each an independent branch/PR off `main`: P1 docs-truth (pure docs), P2 CLI fail-loud (lint + JSON envelope), P3 schema 3xx, P4 server-owned FKs in request bodies, P5 Bearer JWT guards. Ordered so docs land first (P2/P4 reference their wording), and the riskiest (P5) goes last with a scouted plan.

**Decisions already made (do not relitigate):**
- #27 → validation-time lint rejecting `tenancy.entity == auth identity entity`; self-tenancy is NOT in scope.
- #29 → implement real Bearer JWT REST guards (not reject-and-defer).
- #32 → schema moves to `maximum: 399`; docs stay.
- #35 → SKILL Phase-1 checklist only; 00-designing.md is NOT split.

## Global Constraints

- Commits authored by the repo's git user (Pavel Hegler); NO Co-Authored-By/Claude/AI mentions; plain "what changed" messages. Commit when a package is complete and verified (repo Rule 13); PR per package.
- **embedded_sync**: any edit to `docs/ai/*.md` MUST be copied byte-identical to `crates/jerrycan/embedded/ai/<same-name>.md`; `docs/SKILL.md` MUST be copied to `.claude/skills/jerrycan-backend/SKILL.md` (no test enforces the SKILL pair — do it manually). `cargo test -p jerrycan --test embedded_sync` must pass.
- Toolchain pinned 1.97 (`rust-toolchain.toml`) — do not touch.
- The per-PR CI gate is hermetic (fmt, clippy -D warnings, audit, deny, semver-checks, workspace tests, benches, facade, docs build with `RUSTDOCFLAGS=-D warnings` — backtick any `[foo]` in doc comments). Heavy conformance runs via the manual `heavy.yml` workflow.
- A local pre-commit hook runs fmt+clippy on every commit.
- Reference the GitHub issue number in each commit message body (e.g. "Fixes #30").

---

## P1 — Docs truth fixes (#30, #31, #33, #35) — pure docs, no code

**Branch:** `fix/agent-eval-docs` · **Files:** `docs/SKILL.md`, `docs/ai/00-designing.md`, `docs/ai/14-tenancy.md`, plus sync copies (`crates/jerrycan/embedded/ai/00-designing.md`, `crates/jerrycan/embedded/ai/14-tenancy.md`, `.claude/skills/jerrycan-backend/SKILL.md`).

1. **#30 — contract_version bullet.** In `docs/ai/00-designing.md` (~line 30) replace the bullet
   "`contract_version` (REQUIRED) — an integer, `0` or `1`. Use `1`: v1 unlocks … only `> 1` is rejected."
   with one stating the truth: an integer `0`, `1`, or `2`; use `1` for standard apps (`belongs_to` relations, enum `values`, real `json` columns in db mode); `2` additionally unlocks storage buckets and realtime channels (required when the design uses either); `0` is the legacy in-memory contract; only `> 2` is rejected. Keep the existing bullet style. Check the page for other "0 or 1" contract_version claims and fix them too.
2. **#31 — stale 405-probe text, two places.**
   a. `docs/SKILL.md` golden rule 2: delete "; the `404`-probe sent as `GET` to a POST-only route" from the un-greenable list (the parenthetical keeps only the no-credential happy-path probe).
   b. `docs/ai/00-designing.md` (~lines 228–233): delete the sentence "(Same for the `404`-probe-as-`GET` on a POST-only `/{id}` action, which the framework correctly answers `405`.)" — the probe now uses the real method (`testgen.rs:248-250`).
3. **#33 — ownership truth.** First scaffold a throwaway app into a temp dir (binary: `target/debug/jerrycan`, env `JERRYCAN_FRAMEWORK_DEP` path dep, own `CARGO_TARGET_DIR`) and read the ACTUAL generated header comments of `model.rs` and `repo.rs`. Then fix `docs/SKILL.md` (~line 160) — currently "`model.rs`, `repo.rs` (TOOL-owned — regenerated, don't edit)" — to match the headers (known: repo.rs header says "agent-owned; edit freely"). Mention that `repo.rs` is where per-user query scoping lives.
4. **#35 — Phase-1 decision checklist.** In `docs/SKILL.md` Phase 1, add a short "Decide before you design" checklist (~12 lines): auth model (session/none — and which endpoints are public); is there a real org/team tenant entity? (if data is just per-user, do NOT use `tenancy` — the tenant entity must be a separate entity from the auth identity; use `belongs_to` User + guard scoping, see `jerrycan docs tenancy`); which fields are server-owned (owner ids, timestamps) vs client-supplied.
5. **#33/#27 support — tenancy page.** In `docs/ai/14-tenancy.md` add a short section "Per-user data without tenancy": tenancy is for org/team entities with memberships; for user-owned rows use a `belongs_to` relation to the identity entity and scope every query by the session user's id in `repo.rs`/handlers; state plainly that the tenant entity must not be the auth identity entity (a user cannot be their own tenant org). Do NOT claim a validator lint exists (P2 adds it; P2 updates this sentence).
6. **Sync + verify:** copy the three sync pairs; `cargo test -p jerrycan --test embedded_sync`; commit ("docs: … Fixes #30, #31, #33, #35 (checklist)"), push, PR, merge on green gate.

**Acceptance:** grep shows no "0` or `1`" contract_version claim; no 404-probe-as-GET text anywhere in docs/SKILL/embedded copies; SKILL ownership matches generated headers verbatim; embedded_sync green.

---

## P2 — CLI fail-loud (#27 lint + #28 JSON envelope) — code

**Branch:** `fix/agent-eval-failloud` · **Files:** `crates/jerrycan/src/platform/questions.rs` (or `lints.rs` — match where kindred design-shape rules live), `crates/jerrycan/src/platform/codes.rs`, `crates/jerrycan/src/main.rs` (~195–205 result sink), tests in the matching unit-test modules + `crates/jerrycan/tests/cli.rs`; one-sentence update to `docs/ai/14-tenancy.md` (+ embedded copy) turning P1's "must not" into "the validator rejects (JC code …)".

1. **#27 lint.** Add a validation-time rule: a design whose `tenancy.entity` names the auth identity entity is rejected before any scaffolding. First pin how the identity entity is determined (see `scaffold.rs:18` and `:164` comments re tenancy+auth interplay; the collision manifests as `duplicate column name: user_id` in `auth_0001_create_tables`). Allocate the next free JC code in `codes.rs` with explain text of the form: what's wrong (a user cannot be their own tenant org) + the two fixes (per-user data → `belongs_to` identity + guard scoping, `jerrycan docs tenancy`; orgs/teams → separate tenant entity).
   **Regression fixture:** commit the eval's failing design (copy from `/tmp/jc-eval-opus-K1jN/design-v1-tenancy.json`, rename fields to a neutral fixture) under the existing test-fixture convention and assert: validation fails with the new code; NO files are written; `jerrycan explain <CODE>` returns the guidance.
2. **#28 JSON envelope.** In the `main.rs` result sink: when `--json` is active and the command fails, stdout must carry exactly one JSON document `{"ok": false, "code": <JC-code-or-null>, "error": <message>, "hint": <recovery-or-null>}` (match the existing success-envelope field style — read one `--json` success path first); human text may remain on stderr; exit code unchanged (nonzero). Cover every `Failure` variant, not just the lint.
   **Test:** `tests/cli.rs` integration test: run the binary `--json new` against the P2 fixture design; assert stdout parses as a single JSON doc with `ok:false` + the new code, and exit ≠ 0. Add a second case for a non-lint failure (e.g. missing design file) to prove the envelope is universal.
3. **Verify:** `cargo test -p jerrycan` (unit + cli + contracts + embedded_sync); commit ("cli: … Fixes #27, #28"), push, PR, merge on green.

**Acceptance:** the exact eval design that produced a raw SQLite error now fails pre-scaffold with an explained JC code; every `--json` failure emits machine-readable JSON.

---

## P3 — Schema allows 3xx success (#32) — contract + proof

**Branch:** `fix/agent-eval-3xx` · **Files:** `docs/contracts/design-schema.json` (hand-authored canonical; `include_str!`'d by `design.rs:868,944`), `crates/jerrycan/tests/contracts.rs`, possibly `testgen.rs`/`genroute.rs` if 3xx probes need it.

1. Change `success.status` `"maximum": 299` → `399` in the schema (both the `$defs/endpoint` success block and any duplicate).
2. Pin it: extend the schema contract test in `contracts.rs` to assert the maximum is 399.
3. **Prove the promise end-to-end:** author a minimal design with one endpoint whose success is `303` (e.g. a shortener-style redirect), scaffold into a temp dir (path dep + own target), and drive `jerrycan check`/the generated tests: the generated probe must expect 303 and the emitted stub/`Redirect` path must compile. If the generator or testgen cannot actually produce a green 3xx success, STOP — do not ship the schema change — and report exactly what breaks (the fix then includes making testgen/genroute handle 3xx, which is in scope for this package if small; escalate if large).
4. Verify (`cargo test -p jerrycan`), commit ("contract: … Fixes #32"), push, PR, merge on green.

---

## P4 — Server-owned FKs excluded from request bodies (#34) — generator

**Branch:** `fix/agent-eval-serverfk` · **Files:** `crates/jerrycan/src/platform/genroute.rs` (request-struct emission), `testgen.rs` (generated probe bodies), `openapi.rs` (request schemas), `crates/jerrycan/tests/genroute_compile.rs` + `testgen.rs` tests; small doc note in `docs/ai/00-designing.md` request_body semantics (+ embedded copy).

**The rule (decided):** a `belongs_to` FK field is omitted from the generated request body (and its OpenAPI request schema) **iff** it references the auth identity entity AND the endpoint is guarded — the server injects the session user's id (the eval proved handlers already force this). All other `belongs_to` FKs (e.g. bookmark→collection) remain required client input. Unguarded endpoints keep the field (no session to inject).

1. Implement in genroute request-struct emission; reuse P2's identity-entity resolution helper.
2. Update testgen probe bodies to omit the field in the same condition (probes currently 422 or send it explicitly).
3. Update openapi.rs so the request schema omits it too — contract, docs, and wire behavior must agree.
4. Update the generated handler stub comment (and the 00-designing request_body note) to say the server injects the session user id.
5. **Tests:** genroute_compile-style fixture with a guarded identity-FK module + an unguarded one + a non-identity FK: assert emitted struct fields, openapi schemas, and that the guarded scaffold compiles; e2e-lite: scaffolded app's POST without the FK produces 201 (not 422) in the generated acceptance run.
6. Verify (`cargo test -p jerrycan` + `genroute_compile` non-ignored parts), commit ("generator: … Fixes #34"), push, PR, merge on green.

---

## P5 — Bearer JWT guards for REST (#29) — feature

**Branch:** `feat/jwt-bearer-guards` · **Scouted 2026-07-15.** The runtime already ships everything (`Bearer<T>` extractor `jerrycan-auth/src/guard.rs:24-39`, HS256 `jwt::encode/decode` `jwt.rs:31-64`, derived `jwt_key` `lib.rs:126`, `Auth::from_env` carries all keys) — the gap is pure codegen: `scaffold.rs:13-15` hardcodes `pub type CurrentUser = jerrycan::auth::Session<SessionUser>` regardless of `auth.model`; only `realtimegen.rs` branches on the model. `crates/jerrycan-auth` and `mounting.rs`/main.rs need NO changes.

**Real-world regression target:** Supabase migration (`migrate/authmap.rs:139`) already forces `AuthModel::Jwt` — migrated apps today declare jwt and silently get cookie guards.

1. **Alias flip (core, small).** In `scaffold.rs:13-15` (`shared_auth_types()`): when `auth.model == Jwt`, emit `pub type CurrentUser = jerrycan::auth::Bearer<SessionUser>;` (Session for `Session` as today). `SessionUser` struct unchanged — claims carry the same `id: String` (stringified pk; tenant `user_id TEXT` + storage `owner_id` depend on it). Update the alias comment in `templates.rs:121-123`.
2. **Realtime type coupling (must move in lockstep or realtime apps won't compile).** `realtimegen.rs:75-90` jwt arm: the `?token=` fallback wraps claims in `jerrycan::auth::Session(claims)` — under the flipped alias it must wrap `Bearer(claims)`. Update its wording test at `realtimegen.rs:330-339`.
3. **testgen cookie→Bearer (the biggest lift).** When model is Jwt: `auth_preamble_login()` (`testgen.rs:308-312`) mints via `jwt::encode(&SessionUser{..}, auth.jwt_key())` instead of `auth.sessions().encode(..)`; `request_expr` (`testgen.rs:111`) and the 401 test (`:285-288`) send `("authorization", "Bearer <token>")` instead of the `jerrycan_session` cookie; same for the seed/isolation helpers (`test_cookie`/`test_cookie_for(1/2)` — `:174,559,578,585,590,594`) and storagegen's twin helper (`storagegen.rs:405-411`). Session-model output stays byte-identical (regression).
4. **openapi securitySchemes (net-new, keep minimal).** `openapi.rs` emits no security today: add `components.securitySchemes` (`bearerAuth` http/bearer/JWT when jwt; `cookieAuth` apiKey/cookie `jerrycan_session` when session) and per-operation `security` on guarded endpoints.
5. **Docs (+ embedded copies + SKILL twin).** `10-auth.md`: document the model→guard mapping (session → cookie `Session` guard; jwt → `Authorization: Bearer` guard; the agent-written login returns `jwt::encode(&SessionUser{ id: user.id.to_string(), role }, auth.jwt_key())`, include `exp` guidance — decode enforces it when present). Update the SKILL Phase-1 checklist "session/none" → "session / jwt / none" and its one-line consequence. Update `14-tenancy.md`/others only if they hardcode cookie assumptions.
6. **Tests.** `authgen.rs` (asserts alias at `:38-40,51,81`): add a jwt-model fixture asserting the Bearer alias; `tests/testgen.rs` jwt fixtures (`:147,399,549,625`) flip cookie assertions → Bearer; add a genroute_compile-style jwt variant proving a guarded jwt app compiles; extend a migrate test asserting Supabase-migrated scaffolds emit the Bearer alias. All session-model suites must pass UNCHANGED.
7. **Verify:** `cargo test -p jerrycan` full non-ignored suite green; scaffold one jwt-model design in a temp dir and run its generated acceptance tests (mint→Bearer→green); commit ("auth: … Fixes #29"), push, PR. The heavy conformance suite (manual `heavy.yml`) covers the e2e serve path post-merge.

**Out of scope (do not touch):** RS256/JWKS (`idtoken` feature), OAuth flows, token refresh/revocation, mock-idp, session-store internals.
