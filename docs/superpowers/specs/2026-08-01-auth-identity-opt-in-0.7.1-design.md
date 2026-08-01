# Opt-in `auth.identity` generalizes per-user owner-scoping (0.7.1) — #150

**Date:** 2026-08-01
**Status:** Approved design, pre-implementation
**Issues:** #150 (per-user owner-scoping, the #34 server-injected fk, and `public_read` (#105) all key on the LITERAL derived column `user_id` — `AUTH_IDENTITY_FK_COLUMN = "user_id"` (design.rs:647). An identity entity named `Account` (fk `account_id`) silently gets NO owner-scoping, and `endpoint_omits_identity_fk` stays false so a guarded create body keeps `account_id` client-writable → spoofable ownership behind a green gate. The design contract has no auth-identity entity to derive from — `Auth` is `{ model, roles }`, and `user_id` is BOTH the identity-fk convention AND the fixed membership-table principal column.)
**Ships as:** 0.7.1 — an ADDITIVE, opt-in contract-surface change. `Auth` is now `#[non_exhaustive]` (0.7.0/#145), so adding a field is a clean non-breaking MINOR. Default identity is `"User"` → the identity fk resolves to `user_id`, so EVERY existing design is byte-identical; the new behavior is opt-in only.

## The dual-role constant — split it
`AUTH_IDENTITY_FK_COLUMN = "user_id"` serves TWO distinct roles today (design.rs:644-647 doc):
1. The **identity-fk detection** column — "does this entity `belongs_to` the auth identity?" (`is_identity_fk`).
2. The **membership-table principal** column — `{tenant}_members.user_id` stores the SESSION PRINCIPAL, not an entity fk.
Split them:
- Role 2 (membership principal) STAYS the fixed literal `user_id` — it is the session principal column, unrelated to the identity entity name. Keep `AUTH_IDENTITY_FK_COLUMN` (rename its doc to make clear it is the membership-principal column) OR introduce `MEMBERSHIP_PRINCIPAL_COLUMN = "user_id"` for clarity.
- Role 1 (identity-fk detection) is DERIVED from `auth.identity`: `identity_fk_column = snake(auth.identity) + "_id"` (default `auth.identity = "User"` ⇒ `user_id`, byte-identical).

## The change
1. **`Auth` (design.rs:144)** — add `#[serde(default, skip_serializing_if = "Option::is_none")] pub identity: Option<String>`. A helper `Design::auth_identity() -> &str` returns `auth.identity.as_deref().unwrap_or("User")`; `Design::identity_fk_column() -> String` returns `format!("{}_id", to_snake(self.auth_identity()))`. (Both `None` and `"User"` ⇒ `user_id` — byte-identical.)
2. **`is_identity_fk`** (design.rs:1355) — change from an ASSOCIATION fn (`b.fk_column() == AUTH_IDENTITY_FK_COLUMN`) to a METHOD `Design::is_identity_fk(&self, b: &BelongsTo) -> bool` = `b.fk_column() == self.identity_fk_column()`. Thread `&self`/`&Design` through EVERY caller: `has_identity_fk` (1362, also → method), `entity_is_per_user_owned` (1376, already a method), `endpoint_omits_identity_fk` (1428), and every downstream consumer — genroute (`owner_scoped_methods` + request-DTO identity-fk omission + the server-injected-fk `#34` write), testgen (owner-scoping seeds/probes), openapi (request schema fk omission), questions (JC0540 identity collision, JC0549 owner-scope predicate), the JL0006 classifier. Grep every use of `is_identity_fk`/`has_identity_fk`/`AUTH_IDENTITY_FK_COLUMN`/the literal `"user_id"` in the platform module and thread the design-aware resolution — MISS ONE and a non-`User` identity silently loses owner-scoping (the spoofable-ownership bug) OR the membership column breaks.
3. **Validation (JC0540 + a new refusal)** — validate `auth.identity`: it must name a DECLARED entity; JC0540 (the identity-fk collision check) must collide-check the CONFIGURED identity's fk column, not the literal `user_id`. Refuse an `auth.identity` that names a non-existent entity, or whose derived fk column collides with a declared field / another fk / the membership principal column. (Reuse the existing JC0540 machinery, generalized to the configured identity.)
4. **Docs + contract:** `docs/ai/10-auth.md` (+ embedded twin) — document `auth.identity` (default `User`), the identity-fk convention (`snake(identity)_id`), and that the membership principal column stays `user_id`. Update `docs/contracts/design-schema.json` (+ embedded twin if present) to add the `auth.identity` field.

## The byte-identity + no-mirror-drift discipline (MANDATORY — this touches the #102-class classifier)
`entity_is_per_user_owned` is the SINGLE per-user classifier whose mirror-drift shipped the #102-class cross-tenant leaks. This change threads the identity resolution through it and its consumers. REQUIRE:
- **Byte-identity for the default:** every existing design (no `auth.identity`, or `auth.identity: "User"`) generates BYTE-IDENTICAL code (identity_fk_column ⇒ `user_id`). Prove with `determinism.rs` + the full conformance/reference_eval/eval battery unchanged.
- **A single source of truth:** all consumers resolve the identity fk through the ONE `Design::identity_fk_column()` — no consumer re-hardcodes `"user_id"`. Grep to confirm no residual literal `"user_id"` identity check remains (the membership-principal `user_id` in the members-table SQL is the ONLY legitimate literal).
- **The spoofable-ownership guard:** for a NON-`User` identity design, a guarded create body OMITS the identity fk (`endpoint_omits_identity_fk` true) and the write injects the session principal — prove with a genroute_compile design using `auth.identity: "Account"` (fk `account_id`) that (a) compiles, (b) omits `account_id` from the request DTO, (c) owner-scopes reads via the `account_id` accessor. This is the make-impossible half — the bug is that a non-`User` identity silently loses this.

## Tests
- Validation units: `auth.identity` names a non-entity → refused; collides → refused (JC0540 generalized); a valid `"Account"` identity → clean.
- genroute unit: `auth.identity: "Account"` → owner-scoped methods key on `account_id`; the request DTO omits `account_id`; a `User`-default design is byte-identical.
- **genroute_compile:** an `auth.identity: "Account"` per-user-owned design compiles under strict clippy (proves the non-`User` path end to end).
- testgen/openapi: the owner-scoping seeds/probes + the request schema fk omission use the configured identity.
- determinism + embedded_sync twin (docs) green.

## Gates
- `cargo test -p jerrycan` green.
- **Heavy eval gate:** `reference_eval` + `conformance` + `eval` + `genroute_compile` `--include-ignored` green (default-`User` fixtures byte-identical; the new `auth.identity: "Account"` genroute_compile design compiles). Local PG available.
- `cargo fmt`/`clippy -D warnings`; `cargo semver-checks` (additive `Auth.identity` on a `#[non_exhaustive]` struct → non-breaking MINOR); determinism + embedded_sync green.

## Version
0.7.1 (MINOR — additive opt-in on the now-`#[non_exhaustive]` `Auth`; `is_identity_fk` etc. are `pub(crate)` so their signature change is internal).

## Success criteria
- A design can set `auth.identity` (default `User`); owner-scoping, the #34 server-injected fk, `public_read`, and the JL0006/JC0540/JC0549 classifiers all key on the CONFIGURED identity's fk (`snake(identity)_id`), while the membership-table principal column stays `user_id`. A non-`User` identity owner-omits its fk from guarded bodies (no spoofable ownership) and compiles.
- Default-`User` designs BYTE-IDENTICAL; no residual hardcoded identity `"user_id"`; heavy gate + determinism green; published 0.7.1; #150 closed.

## Non-goals
- Multiple identity entities. Changing the membership-table principal column (stays `user_id`). The realtime seam (#104). Any behavior change for a `User`-identity (default) design.
