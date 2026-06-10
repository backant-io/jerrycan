# Phase 1 backlog (carried from Phase 0 reviews)

- Accept loop: tolerate transient accept() errors (EMFILE/ECONNABORTED) with backoff (TODO marker in app.rs)
- Handler panic → 500 catch layer (today: connection drop, panic to stderr)
- Graceful shutdown, request/handler timeouts, security-header middleware, percent-decoding of path segments + router fuzzing (deferred by plan)
- jerrycan-macros: preserve spans by token-cloning re-emit instead of string round-trip (diagnostics for in-body errors currently collapse to 1:1)
- Facade Cargo.toml: internal dep version literals (0.0.0) won't track the workspace bump at 0.1.0; move to workspace deps or fix at release
- Multi-param Path<(A,B)> extractor

## Contract v1 candidates (deliberately deferred from v0)

- design-schema: middleware (module- and app-scoped) as first-class design objects; jerrycan_generate kind "middleware" returns then too
- design-schema: structured rate-limit config (v0: rate limits ride as opaque dependency names)
- jerrycan_check diagnostics: span (line+column ranges) instead of single line, pending macro span preservation

## Generator hygiene

- write_subroutes does not prune subroute directories removed from the design; re-adding a same-named subroute resurrects stale agent-owned handlers (create-once). Prune-or-warn decision needed.
- jerrycan_generate slice-merge REPLACES the module wholesale: a partial slice silently drops sibling endpoints/subroutes (build stays correct; stale agent files linger). Consider warn-on-route-reduction in next_step or pruning stale subroutes/.
- generate name-mismatch error (path vs design_slice.name) should hint at the divergence instead of "not in design.json".
- MCP server: directed SIGTERM to `jerrycan dev` orphans the cargo/app grandchildren (Ctrl-C is fine); consider process-group handling.
