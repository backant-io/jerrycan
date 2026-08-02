# testgen: isolation-test probe base pins all mount params (0.7.13) — #245

**Date:** 2026-08-02
**Status:** Approved design, pre-implementation (AUDIT round 5 finding, MEDIUM; #240 sibling)
**Issue:** #245 — `tenant_owned_isolation_test` builds its probe base `cbase` (`crates/jerrycan/src/platform/testgen.rs:1819-1822`) by substituting ONLY the canonical tenancy fk token (`Design::fk_column(tenancy.entity)`, `:1812`) + the `path.joins[].child_fk` tokens (`1813-1817`) — NOT arbitrary mount params. The per-endpoint tests use `concrete_mount_base` (`:669`), which pins EVERY `{param}`→`1`. So a tenant-owned entity mounted on a param whose name differs from the canonical fk column (e.g. `Organization`/`organization_id` mounted `/happenings/{org_id}`) leaves the literal `{org_id}` in the generated isolation-test URL → the cross-tenant negative control fails at setup (`400 "invalid path parameter org_id"`, `Path<i64>` can't parse `{org_id}`) → it can NEVER go green → a tool-owned security test that wedges the agent (or is deleted to get green). Scaffold is GREEN (a valid string literal), runtime is safe (`MembershipSet` + `create_for_memberships` body-fk check) → a TEST defect, not a leak. The same latent gap exists in `per_user_isolation_test` (`2067-2068`) and `public_read_isolation_test` (`2184-2185`).
**Ships as:** 0.7.13 — a testgen fix. Patch bump 0.7.12 → 0.7.13. Isolation tests for normal mounts (where the mount param IS the canonical fk / a join child_fk) are byte-identical.

## The fix
Replace the hand-rolled targeted-substitution `cbase` in `tenant_owned_isolation_test` (`~1812-1822`) — and the analogous bases in `per_user_isolation_test` (`~2067-2068`) and `public_read_isolation_test` (`~2184-2185`) — with `concrete_mount_base(base)` (`testgen.rs:669`), which pins EVERY `{param}`→`1`. The seeded tenant/parent/owner id is `1` in every isolation probe, so pinning all params to `1`:
- fixes the literal-token bug (a non-canonical mount param like `{org_id}` becomes `1`),
- stays correct for the existing nested/grandchild shapes (the canonical fk + join child_fks were already being replaced with `1` — `concrete_mount_base` produces the same result for those), and
- is byte-identical for a mount whose only params ARE the canonical fk / join child_fks.

Confirm `concrete_mount_base` is in scope / callable from those three fns (it's used by the per-endpoint tests in the same module). If any isolation-test code path needs the base WITHOUT the trailing collection segment, mirror exactly what `concrete_mount_base` returns for the per-endpoint tests (the probe URLs must match the real mounted paths with all params = 1).

## Tests
- **Unit:** a tenant-owned entity mounted on a NON-canonical param (`/happenings/{org_id}` with tenancy `Organization`/`organization_id`) → the generated isolation test's URLs contain `/happenings/1/` (no literal `{org_id}`), and it compiles.
- **Compile fixture (add to `crates/jerrycan/tests/genroute_compile.rs`):** a cross-prefix-mounted tenant-owned design (the #245 repro) whose generated `acceptance.rs` compiles under `-D warnings` AND whose isolation test URL is concrete — RED before (the URL fails to parse at runtime, but at minimum assert the emitted string contains no `{` param) / GREEN after. (A compile fixture proves it compiles; add a testgen-unit assertion that the emitted isolation-test body contains no unresolved `{param}` for this shape — that is the real regression guard.)
- **Byte-identity:** the existing #240 nested-list + read-less fixtures and all `tests/testgen.rs` isolation tests (canonical-fk mounts) are byte-identical (or updated only where `concrete_mount_base` legitimately produces the same string).

## Gates
- `cargo test -p jerrycan` (testgen + genroute_compile `--include-ignored` + conformance) green; the new fixture + unit test green.
- `cargo fmt`/`clippy -D warnings`; `cargo doc -D warnings`; determinism + embedded_sync.

## Version + success criteria
0.7.13. A tenant-owned (or per-user/public-read) entity mounted on a non-canonical param generates an isolation test whose URLs are concrete (all `{param}`→`1`), so the cross-tenant/cross-user negative control is greenable and honest — no literal `{param}` survives. Canonical-fk mounts byte-identical; a compile+unit fixture locks the cross-prefix shape; published 0.7.13; #245 closed.

## Non-goals
- Changing the runtime guard/scoping (safe). The grandchild parent-fk mount list-coverage limit (#240-noted). Any non-isolation-test output.
