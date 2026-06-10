# Phase 4 agent eval — results

- Date: 2026-06-10
- Agent: opus subagent (docs-only; no access to framework source or conformance fixtures)
- jerrycan @ `48f0893`
- Procedure: `conformance/eval/PROTOCOL.md`

## Per-spec results

| Spec      | Modules / shape                                   | `jerrycan check` | HTTP round-trip                          | Result |
|-----------|---------------------------------------------------|:----------------:|------------------------------------------|:------:|
| blog      | posts (CRUD) + comments subroute, authors         | green            | create→read→update→delete + 404s OK      | PASS   |
| tasks     | tasks (CRUD + PUT done-toggle), projects          | green            | create→read→toggle→delete + 404s OK      | PASS   |
| shortener | links (create/list/resolve/delete)                | green            | create→list→resolve→delete + 404s OK     | PASS   |
| inventory | items (CRUD, integer qty), categories             | green            | create→read→update→delete + 404s OK      | PASS   |
| notes     | notes (CRUD) + tags subroute                       | green            | create→read→update→delete + tags + 404s  | PASS   |

**Overall pass rate: 5/5 (100%).** Floor is 4/5; metric target is ≥ 90%.

## How it went

Every one of the five apps was scaffolded with `jerrycan new`, every generated
handler stub was implemented from scratch, `jerrycan check` came back
`all green` on the first run for all five, and each app served a real create →
read → update → delete sequence over raw HTTP (curl) with the expected status
codes (`201`/`200`/`204`) and bodies, plus correct `404 JC0404` for unknown ids.

The whole task was reconstructible from the docs:

- `jerrycan docs app` / `modules` — the generated layout (tool-owned `lib.rs`
  exposing `module()`, agent-owned `handlers`/`model`/`repo`/`deps`).
- `jerrycan docs extractors` — `Path<i64>`, `Json<T>` in handler signatures.
- `jerrycan docs dependencies` — `Dep<Repo>` injection (the repo is provided in
  the generated `lib.rs`).
- `jerrycan docs errors` — `Error::not_found()` → `404 JC0404`, `?` propagation,
  the `{"code","message"}` body shape.
- `jerrycan docs testing` — `TestApp` / `into_test`, used by the
  `jerrycan gen-tests` acceptance suite.

The response types `Json<T>`, `Created<T>`, and `NoContent` were already present
in the generated stub signatures, so the docs' coverage of them was confirmatory
rather than load-bearing. The in-memory `repo.rs` ships complete
(`all`/`get`/`insert`/`update`/`remove`), so handlers are a thin
extract → call → respond mapping exactly as the module/extractor docs describe.

No `jerrycan explain` lookups were needed — there were no diagnostics to explain,
because nothing failed `check`.

## Docs / diagnostics gaps surfaced

None.

No documented API was missing, ambiguous, or wrong for any of the five designs,
and no diagnostic required more than the docs already provide. No `docs/ai/*.md`
edits were required to reach the pass rate.

## Framework bugs

None. No failure was a framework bug; nothing was papered over.
