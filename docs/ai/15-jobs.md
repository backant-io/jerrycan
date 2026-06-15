# Background jobs

## Purpose
`jerrycan::jobs` (the `jobs` feature) runs work off the request path: cron
schedules and programmatically-enqueued queue jobs, over a durable Postgres
store (default) or an in-memory test store. You DECLARE jobs in the design; the
generator emits a typed task stub per job, a wired `Jobs` extension, and a
failing acceptance test you turn green by implementing the task. The engine
gives at-least-once delivery with retries + exponential backoff → dead-letter,
`run_at` delays, and idempotency keys.

**Jobs run AT LEAST once.** A worker that crashes mid-job has its lease expire,
and the job is re-leased and runs again — so a task handler **MUST be
idempotent**. Exactly-once is impossible across crashes; design every task so a
second run is harmless (upsert, not insert; check-then-skip; key external calls
by an idempotency token). This is the one rule you cannot skip.

## Signature
Declare jobs at the TOP LEVEL of the design (not per-module). `schedule` is the
5-field cron trigger (its presence makes a job a cron job); `queue` (default
`"default"`) is the worker pool the job runs on:
```json
{
  "jobs": [
    { "name": "expire_trials", "schedule": "0 * * * *", "queue": "billing" },
    { "name": "send_email", "queue": "email" }
  ]
}
```
A **cron** job (`schedule` present) takes no payload — the leader enqueues it
each due tick. A **queue** job (no `schedule`) is enqueued programmatically with
a typed `{Name}Payload`. The generator emits one agent-owned task module per job
(`crates/jobs/src/{name}.rs`) — fill in the body; the stub returns a `500` so
the generated acceptance test is red until you implement it:
```rust
# use jerrycan::prelude::*;
use jerrycan::TaskContext;
use serde::{Deserialize, Serialize};

// CRON job: owned ctx, no payload (the leader enqueues it each tick).
pub async fn expire_trials(mut ctx: TaskContext) -> Result<()> {
    // jobs are at-least-once — make this idempotent (it may run more than once).
    let _ = &mut ctx;
    Ok(())
}

// QUEUE job: owned ctx + the typed payload it was enqueued with.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SendEmailPayload { to: String }

pub async fn send_email(mut ctx: TaskContext, payload: SendEmailPayload) -> Result<()> {
    // idempotent: keying the send by `payload.to` makes a re-run a no-op.
    let _ = (&mut ctx, &payload.to);
    Ok(())
}
# let _ = (expire_trials, send_email);
```

## Enqueuing from a handler
App handlers resolve `Dep<JobsHandle>` and call `enqueue(NewJob, now)`. The
store is clock-free, so you pass `clock.now()` explicitly — tests then drive
time deterministically. Build the job with `NewJob::new(name, queue)` plus
optional `.payload(..)`, `.run_at(..)`, `.idempotency_key(..)`,
`.max_attempts(..)`:
```rust
# use jerrycan::prelude::*;
# #[cfg(feature = "jobs")]
# {
use jerrycan::jobs::{JobsHandle, NewJob};

async fn signup(jobs: Dep<JobsHandle>, clock: Dep<Clock>) -> Result<NoContent> {
    jobs.enqueue(
        NewJob::new("send_email", "email")
            .payload(serde_json::json!({ "to": "a@example.com" }))
            .idempotency_key("welcome:a@example.com"),  // a duplicate enqueue is a no-op
        clock.now(),
    ).await?;
    Ok(NoContent)
}
# let _ = signup;
# }
```

## Minimal example
The worker loops run under `on_serve`, which `into_test` deliberately DROPS — so
a job is never reachable through `TestApp`'s request path. Test the two halves
directly: enqueue against a store with an explicit `now`, and call the task fn
itself with `t.task_context()`. The in-memory store is the deterministic
reference (the Postgres store reuses the same contract):
```rust
# use jerrycan::prelude::*;
# #[cfg(feature = "jobs")]
# fn main() { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
use jerrycan::jobs::{EnqueueOutcome, InMemoryStore, JobStore, NewJob};
use std::time::{Duration, SystemTime};

let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
let store = InMemoryStore::new();

// Enqueue twice with the same idempotency key: the second is a no-op.
let first = store
    .enqueue(NewJob::new("send_email", "email").idempotency_key("k1"), now)
    .await
    .unwrap();
let second = store
    .enqueue(NewJob::new("send_email", "email").idempotency_key("k1"), now)
    .await
    .unwrap();
assert!(matches!(first, EnqueueOutcome::Inserted(_)));
assert!(matches!(second, EnqueueOutcome::Duplicate(_)), "same key is a no-op");

// Exactly one job is leasable on the queue.
let leased = store.lease("email", now, Duration::from_secs(30), 10).await.unwrap();
assert_eq!(leased.len(), 1);

// Test a task fn by calling it directly (the worker is invisible to into_test).
let t = App::new().into_test();
async fn send_email(_ctx: jerrycan::TaskContext) -> Result<()> { Ok(()) }
assert!(send_email(t.task_context()).await.is_ok());
# }); }
# #[cfg(not(feature = "jobs"))]
# fn main() {}
```

## Variations
- **Wiring**: the generated `crates/jobs/src/lib.rs` builds the extension —
  `Jobs::postgres(db).queue("email", 4).register("send_email", f).cron(..)` —
  and the app installs it with `app.extend(jobs(db))`. One worker pool per
  declared queue; `register` maps a job name to its typed task fn.
- **Store**: `Jobs::postgres(db)` is the production default (durable, multi-node;
  the table is created by `JOBS_MIGRATIONS` / `PostgresStore::migrate`).
  `Jobs::in_memory()` is the single-process / test store. `Jobs::redis(store)`
  (the `jobs-redis` feature) is the durable multi-node Redis store — see below.
- **`run_at` delays**: `NewJob::new(..).run_at(at)` holds a job until `at` — it
  is not leased before then.
- **Retries → dead-letter**: a task that returns `Err` (or times out) is retried
  with exponential backoff; after `max_attempts` (default 5) it moves to the
  dead-letter set. Dead jobs are inspectable (`list_dead`) and requeueable
  (`requeue_dead`), never silently dropped. A job that is dead-lettered or fails
  irrecoverably surfaces as `JC0521`.
- **Idempotency keys**: a second `enqueue` with a key already seen is a no-op
  reporting the existing id (`EnqueueOutcome::Duplicate`) — use it to make an
  at-most-one-enqueue contract from a handler that may retry.

## Cron semantics
- **Skip-missed**: after downtime, a cron job fires the **most recent** missed
  tick exactly once — not every tick in the backlog. An hourly job down from
  01:00 to 05:30 fires the 05:00 tick once; 01:00–04:00 are skipped. No
  thundering-herd backfill.
- **First run**: a freshly-deployed cron job (no recorded last-fired) fires the
  most-recent tick immediately on deploy. Seed a baseline if you don't want a
  fire on first boot.
- **Leader**: only ONE instance fires each tick. On Postgres this is a
  `pg_advisory_xact_lock` leader — the lock, the enqueue, and the last-fired
  advance all happen in one transaction, so two nodes can't double-fire.
  Single-process / in-memory deploys are the trivial leader (one process). Cron
  leadership is **Postgres-only** (the in-memory leader is single-process).

## Redis store (multi-node)
`Jobs::redis(RedisStore::connect(url).await?)` (the `jobs-redis` feature) is a
durable, multi-node store over Redis Streams + consumer groups, an alternative
to Postgres when you don't run a database. It satisfies the same `JobStore`
contract as the Postgres and in-memory stores, so every guarantee above —
at-least-once, retries → dead-letter, `run_at` delays, idempotency keys —
holds unchanged:
```rust
# #[cfg(feature = "jobs-redis")]
# async fn wire() -> jerrycan::Result<()> {
use jerrycan::jobs::{Jobs, RedisStore};

let store = RedisStore::connect("redis://127.0.0.1/").await?;
let jobs = Jobs::redis(store).queue("email", 4);
// app.extend(jobs) — same as Jobs::postgres / Jobs::in_memory.
# let _ = jobs;
# Ok(())
# }
```
Caveats specific to the Redis store:
- **Still at-least-once.** A crashed worker's lease is reclaimed and the job
  re-runs, so the idempotency rule above is unchanged — make every task handler
  idempotent.
- **Idempotency dedup is atomic and cross-node.** The enqueue idempotency key is
  a `SET NX` inside the store's Lua script, so two nodes enqueuing the same key
  (e.g. duplicate cron ticks) collapse to one job — the in-memory cron leader
  relies on this.
- **Reclaim uses the Redis-server clock, not your `now`.** Every other method
  takes the injected `now`; crashed-worker reclaim is the one exception —
  `XAUTOCLAIM`'s min-idle is measured against the Redis server's idle time for
  the lease, inherent to Streams. A still-running worker is never stolen.
- **No Redis cron leader.** A Redis-only deploy uses the in-memory
  single-process cron leader; its duplicate cross-node ticks are deduped by the
  atomic idempotency above (a Postgres-style distributed cron leader is out of
  scope).

## Testing
The worker is invisible to `into_test` (the `on_serve` loops are dropped), so
drive jobs deterministically with the injected `Clock`:
- **A task**: call its fn directly — `job_name(t.task_context())` (cron) or
  `job_name(t.task_context(), payload)` (queue) — exactly what the generated
  acceptance test does. Build the `TestApp` from `App::new().extend(db)` so the
  task can resolve the app-level `Db`.
- **Cron / backoff / `run_at`**: these all key off `now`. Advance the test clock
  with `t.clock().advance(dur)` (or pass a hand-chosen `SystemTime` into a store
  method) to step time past a backoff window, a `run_at`, or a cron tick — no
  sleeping, fully deterministic.

## Errors you'll hit
- A job that returns `Err` and exhausts its retries → dead-lettered, surfaced as
  `JC0521`. Inspect it with `list_dead` and requeue with `requeue_dead`.
- A job whose `name` has no registered handler is dead-lettered immediately
  (the dispatch table has no entry) — register every declared job.
- HTTP extractors (`Json`/`Path`/`Query`) in a task fn fail with `JC1003`: a
  task runs in a `TaskContext`, not a request, so only app-level `Dep<T>`
  resolve. Pass request data as the job's typed payload, not as an extractor.

## Anti-patterns
- **Don't assume a job runs exactly once.** It runs at LEAST once; a non-idempotent
  task (charge a card, send an email, increment a counter) double-executes when a
  lease is reclaimed. Key every side effect by an idempotency token, or upsert.
- Don't put long-running work behind the request: enqueue a job and return. A
  background task has NO `handler_timeout` — its only wall-clock bound is the
  per-job exec timeout — so it must not block a response.
- Don't read `clock.now()` inside the store call expecting it to re-read time —
  the store is clock-free by design; pass the `now` you want once, so tests
  control it.
- Don't rely on cron firing every missed tick after an outage — it fires the
  most recent one. If a job must process each interval, make it enumerate the
  range itself, not depend on one fire per tick.
