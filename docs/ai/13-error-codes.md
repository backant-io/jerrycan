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
| JC0408 | Request body read timeout — on a `stream_body()` route this is the per-frame deadline (a stalled client between chunks) |
| JC0409 | Conflict — the write violates a unique key (jerrycan-db) |
| JC0413 | Payload too large — body over the route limit (default 1 MiB), or a multipart part over the per-part cap (8 MiB), >256 parts, or part headers over 8 KiB |
| JC0415 | Unsupported media type — content type is not what the endpoint consumes (e.g. `Multipart` needs `multipart/form-data` with a boundary; a storage bucket upload must match the bucket's `allowed_mime` allowlist) |
| JC0422 | Unprocessable — bad JSON, validation violations, or a foreign-key violation (a client-supplied fk referencing a nonexistent record; jerrycan-db) |
| JC0429 | Too many requests — the client exceeded its rate limit for the current window (the rate-limit extension); the response carries a `Retry-After` header |
| JC0500 | Internal error (or handler panic) |
| JC0503 | Handler timeout (default 30s) |
| JC0510 | Database error (jerrycan-db) |
| JC0520 | Schema contract is stale — schema.json drifted from the migrations |
| JC0521 | Job failed — a background job returned an error and (after its retries) was dead-lettered, or failed irrecoverably (the jobs engine) |
| JC0530 | Realtime requires Postgres — a realtime `changes` channel was joined on a sqlite deployment (broadcast/presence work without a database; changes need Postgres) |
| JC0531 | Realtime replication unavailable — `wal_level` is not `logical` (or the role lacks REPLICATION), so changes run on the trigger + LISTEN/NOTIFY fallback (identical client behavior, weaker guarantee) |
| JC1001 | Missing dependency provider |
| JC1002 | Dependency cycle |
| JC1003 | Dependency requires an HTTP request — an HTTP extractor was used in a task context (use only `Dep<T>` args, or resolve inside a request) |

## Generation lints (`JL####`)
| Code | Meaning |
|---|---|
| JL0001 | Route crate exports more than `module()` |
| JL0002 | Design endpoint has no matching handler |
| JL0003 | Generated file drifted from the design |
| JL0004 | Mutating route unguarded in an auth design |
| JL0006 | Cross-tenant data access — a tenant-owned handler used an unscoped repo method (use `all_for`/`get_for`/`remove_for`) |
| JL0007 | Request-boundary escape — agent-owned module code calls process/fs/net directly (use framework I/O; opt out per line with `// jerrycan:allow JL0007`) |
| JL0008 | A tenant-owned handler could not be scanned for scoping (missing, unreadable, or not valid Rust) — fix the file so `jerrycan check` can verify tenant scoping |
