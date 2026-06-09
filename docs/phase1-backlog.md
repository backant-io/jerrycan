# Phase 1 backlog (carried from Phase 0 reviews)

- Accept loop: tolerate transient accept() errors (EMFILE/ECONNABORTED) with backoff (TODO marker in app.rs)
- Handler panic → 500 catch layer (today: connection drop, panic to stderr)
- Graceful shutdown, request/handler timeouts, security-header middleware, percent-decoding of path segments + router fuzzing (deferred by plan)
- jerrycan-macros: preserve spans by token-cloning re-emit instead of string round-trip (diagnostics for in-body errors currently collapse to 1:1)
- Facade Cargo.toml: internal dep version literals (0.0.0) won't track the workspace bump at 0.1.0; move to workspace deps or fix at release
- Multi-param Path<(A,B)> extractor
