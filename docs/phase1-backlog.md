# Phase backlog

## Phase 4 (per roadmap)

- Router + percent-decoder fuzzing (cargo-fuzz; roadmap Phase 4 owns fuzzing)

## Contract v1 candidates (deliberately deferred from v0)

- design-schema: middleware (module- and app-scoped) as first-class design objects; jerrycan_generate kind "middleware" returns then too
- design-schema: structured rate-limit config (v0: rate limits ride as opaque dependency names)
- jerrycan_check diagnostics: span (line+column ranges) — macro spans are preserved as of Phase 1b; wiring spans through diagnostics remains
- design-schema: path parameter types (v0 generates i64; string ids need a type field on params)
- Path param types beyond the sealed PathParam set (serde-based extraction, axum-style) for custom id newtypes
- security-header granularity (per-route/per-response config) before any Phase 2 HTML serving (today: all-or-nothing app-level opt-out; handler-set values win)

## Accepted v0 limitations

- `jerrycan dev`: directed SIGTERM orphans the cargo/app child (Ctrl-C is fine). Process-group handling needs libc/unsafe — conflicts with forbid(unsafe_code); revisit if it bites.
- write_subroutes does not prune subroute directories removed from the design; generate warns on route reduction instead (stale agent-owned files are never deleted by the tool).
