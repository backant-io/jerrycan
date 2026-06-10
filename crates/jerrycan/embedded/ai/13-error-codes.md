# Error & lint codes

Every jerrycan diagnostic carries a stable code. `jerrycan explain <code>` prints
the cause + fix for any of them.

## Runtime errors (`JC####`)
| Code | Meaning |
|---|---|
| JC0400 | Bad request — malformed path param / query / percent-encoding |
| JC0401 | Authentication required or failed |
| JC0403 | Forbidden — role check failed |
| JC0404 | Not found |
| JC0405 | Method not allowed |
| JC0408 | Request body read timeout |
| JC0409 | Conflict — the write violates a unique key (jerrycan-db) |
| JC0413 | Payload too large (default 1 MiB) |
| JC0422 | Unprocessable — bad JSON or validation violations |
| JC0500 | Internal error (or handler panic) |
| JC0503 | Handler timeout (default 30s) |
| JC0510 | Database error (jerrycan-db) |
| JC1001 | Missing dependency provider |
| JC1002 | Dependency cycle |

## Generation lints (`JL####`)
| Code | Meaning |
|---|---|
| JL0001 | Route crate exports more than `module()` |
| JL0002 | Design endpoint has no matching handler |
| JL0003 | Generated file drifted from the design |
| JL0004 | Mutating route unguarded in an auth design |
