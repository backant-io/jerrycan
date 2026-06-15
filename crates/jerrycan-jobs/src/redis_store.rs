//! The durable, multi-node [`RedisStore`] (spec §v2.3b): a [`JobStore`] over
//! Redis Streams + consumer groups + `XAUTOCLAIM`, behind the `jobs-redis`
//! feature. It is the multi-node alternative to the Postgres default store and
//! MUST match the `InMemoryStore` reference semantics in `store.rs` exactly.
//!
//! ## Why Streams
//! The trait is `i64`-keyed and a worker holds an id across
//! `lease → ack/retry/dead_letter`; Redis Streams use string entry ids. We
//! bridge the two with a per-job **hash** carrying the app `i64` id and its
//! current `stream_id`, an `INCR` **sequence** for ids, a per-queue **ready
//! Stream** (consumer group `jc`), a per-queue **scheduled ZSET** for
//! `run_at`/backoff delays (Streams have no native delay), and a per-queue
//! **dead ZSET**. Multi-key mutations are atomic Lua scripts (the BullMQ
//! pattern). Crashed-worker reclaim is `XAUTOCLAIM` over the pending-entries
//! list (PEL), keyed on Redis-server idle time = the lease.
//!
//! ## Key invariants it replicates from the reference
//! * **Lease** claims `(Pending AND run_at<=now) OR (Leased AND lease expired)`,
//!   sets `Leased`, `attempts += 1`. Due-by-`run_at` jobs are promoted from the
//!   scheduled ZSET onto the ready Stream and claimed via `XREADGROUP`; expired
//!   leases are reclaimed via `XAUTOCLAIM` (a still-running job whose PEL idle is
//!   below the lease is never stolen).
//! * **Idempotency is PERMANENT and atomic** — `idem:{key}` is `SET NX` inside
//!   the enqueue script, so a duplicate is a cross-node no-op reporting the
//!   existing id (no TTL, matching the in-memory/Postgres reference).
//! * **`list_dead` orders by `id`** — the dead ZSET is scored by the job id, so
//!   the order is deterministic without a clock.
//! * **`requeue_dead`** makes the job immediately leasable (straight onto the
//!   ready Stream) and resets `attempts` to 0.
//!
//! ## The one wall-clock dependency
//! Every other method takes the injected `now`. Crashed-worker reclaim is the
//! sole exception: `XAUTOCLAIM`'s min-idle is measured against the **Redis
//! server's** clock (the PEL idle time), not the caller's `now` — this is
//! inherent to Streams. A still-running worker (PEL idle < lease) is never
//! stolen; a crashed worker's entry (idle ≥ lease) is reassigned.

use crate::store::{
    DEFAULT_MAX_ATTEMPTS, EnqueueOutcome, Job, JobFuture, JobStatus, JobStore, NewJob,
};
use jerrycan_core::{Error, Result};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The fixed consumer group on every ready Stream.
const GROUP: &str = "jc";
/// The single fixed consumer name. The PEL tracks per-entry idle independent of
/// the consumer, and `XAUTOCLAIM` reassigns by idle time, so one consumer is
/// sufficient.
const CONSUMER: &str = "jc";

/// A `SystemTime` to epoch-millis (`i64`). Mirrors `postgres_store.rs`: a
/// pre-epoch time (never produced by the engine) floors to 0.
fn to_millis(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Epoch-millis (`i64`) back to a `SystemTime`. The inverse of [`to_millis`];
/// negative values (never stored) clamp to the epoch.
fn from_millis(ms: i64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(ms.max(0) as u64)
}

/// Map a redis error to a core `internal` error (the same shape
/// `jerrycan_ratelimit`'s `RedisStore` uses).
fn redis_err(e: redis::RedisError) -> Error {
    Error::internal(format!("jerrycan-jobs redis: {e}"))
}

/// The `jc:jobs:seq` INCR key — source of the `i64` job id.
fn seq_key() -> &'static str {
    "jc:jobs:seq"
}

/// The per-job hash key.
fn job_key(id: i64) -> String {
    format!("jc:jobs:job:{id}")
}

/// The per-queue ready Stream key (consumer group `jc`).
fn stream_key(queue: &str) -> String {
    format!("jc:jobs:q:{queue}:s")
}

/// The per-queue scheduled ZSET key (member id, score `run_at` epoch-ms).
fn sched_key(queue: &str) -> String {
    format!("jc:jobs:q:{queue}:z")
}

/// The per-queue dead-letter ZSET key (member id, score = id for a
/// deterministic `list_dead` order).
fn dead_key(queue: &str) -> String {
    format!("jc:jobs:q:{queue}:dead")
}

/// The idempotency key → id mapping (`SET NX`, permanent, no TTL).
fn idem_key(key: &str) -> String {
    format!("jc:jobs:idem:{key}")
}

// ---------------------------------------------------------------------------
// Lua scripts. Multi-key mutations are atomic (the BullMQ pattern): Redis runs
// each script to completion with no interleaving, so a partial mutation can
// never be observed across nodes.
// ---------------------------------------------------------------------------

/// **enqueue** — atomic insert with cross-node idempotency.
///
/// `KEYS[1]` seq, `KEYS[2]` stream, `KEYS[3]` sched zset.
/// `ARGV`: 1 name, 2 queue, 3 payload, 4 run_at_ms, 5 max_attempts,
/// 6 idem (key text, "" if none), 7 created_at_ms, 8 now_ms,
/// 9 idem_full_key ("jc:jobs:idem:{key}", "" if none).
///
/// If an idempotency key is set and its `idem` string already exists, returns
/// `{"dup", existing_id}` without inserting (a no-op). Otherwise `INCR`s the
/// seq, `HSET`s the job hash, routes it to the ready Stream (`run_at<=now`,
/// storing the `stream_id`) or the scheduled ZSET, persists the idem mapping
/// (`SET`, already-checked so unconditional), and returns `{"new", id}`.
const ENQUEUE_LUA: &str = r#"
local idem_full = ARGV[9]
if idem_full ~= '' then
  local existing = redis.call('GET', idem_full)
  if existing then
    return {'dup', existing}
  end
end
local id = redis.call('INCR', KEYS[1])
local job_key = 'jc:jobs:job:' .. id
local run_at = tonumber(ARGV[4])
local now = tonumber(ARGV[8])
local stream_id = ''
if run_at <= now then
  stream_id = redis.call('XADD', KEYS[2], '*', 'id', id)
end
redis.call('HSET', job_key,
  'name', ARGV[1],
  'queue', ARGV[2],
  'payload', ARGV[3],
  'run_at', ARGV[4],
  'attempts', '0',
  'max_attempts', ARGV[5],
  'status', 'pending',
  'idem', ARGV[6],
  'created_at', ARGV[7],
  'stream_id', stream_id)
if stream_id == '' then
  redis.call('ZADD', KEYS[3], run_at, id)
end
if idem_full ~= '' then
  redis.call('SET', idem_full, id)
end
return {'new', id}
"#;

/// **promote** — move every due scheduled job onto the ready Stream, atomically.
///
/// `KEYS[1]` sched zset, `KEYS[2]` stream. `ARGV[1]` now_ms, `ARGV[2]` max.
/// For each member with score `<= now` (up to `max`): `XADD` it, store the new
/// `stream_id` on its hash, and `ZREM` it from the scheduled set. Returns the
/// number promoted (informational).
const PROMOTE_LUA: &str = r#"
local due = redis.call('ZRANGEBYSCORE', KEYS[1], '-inf', ARGV[1], 'LIMIT', 0, tonumber(ARGV[2]))
local n = 0
for _, id in ipairs(due) do
  local job_key = 'jc:jobs:job:' .. id
  if redis.call('EXISTS', job_key) == 1 then
    local stream_id = redis.call('XADD', KEYS[2], '*', 'id', id)
    redis.call('HSET', job_key, 'stream_id', stream_id)
  end
  redis.call('ZREM', KEYS[1], id)
  n = n + 1
end
return n
"#;

/// **lease_entry** — mark one claimed Stream entry leased and read its job back.
///
/// `KEYS[1]` job hash. `ARGV[1]` entry stream_id (for the acked-then-trimmed
/// race check). Returns `nil` if the hash is gone (the caller then `XACK`+`XDEL`s
/// the orphan entry); otherwise `HINCRBY attempts 1`, sets `status=leased` and
/// the current `stream_id`, and returns the full hash as a flat field/value list.
const LEASE_ENTRY_LUA: &str = r#"
if redis.call('EXISTS', KEYS[1]) == 0 then
  return nil
end
redis.call('HINCRBY', KEYS[1], 'attempts', 1)
redis.call('HSET', KEYS[1], 'status', 'leased', 'stream_id', ARGV[1])
return redis.call('HGETALL', KEYS[1])
"#;

/// **ack** — terminal completion.
///
/// `KEYS[1]` stream, `KEYS[2]` job hash. `ARGV[1]` stream_id. `XACK` + `XDEL`
/// the entry and `DEL` the hash (no longer needed). The idempotency mapping is
/// left in place (permanent dedup).
const ACK_LUA: &str = r#"
local sid = redis.call('HGET', KEYS[2], 'stream_id')
if sid and sid ~= '' then
  redis.call('XACK', KEYS[1], 'jc', sid)
  redis.call('XDEL', KEYS[1], sid)
end
redis.call('DEL', KEYS[2])
return 1
"#;

/// **retry** — reschedule a failed job for a backoff retry.
///
/// `KEYS[1]` stream, `KEYS[2]` job hash, `KEYS[3]` sched zset. `ARGV[1]`
/// backoff_until_ms. `XACK`+`XDEL` the current entry (clears the PEL so it is
/// not reclaimed), `ZADD` to the scheduled set at `backoff_until`, status →
/// `pending`, `run_at` updated, `stream_id` cleared.
const RETRY_LUA: &str = r#"
if redis.call('EXISTS', KEYS[2]) == 0 then
  return 0
end
local sid = redis.call('HGET', KEYS[2], 'stream_id')
if sid and sid ~= '' then
  redis.call('XACK', KEYS[1], 'jc', sid)
  redis.call('XDEL', KEYS[1], sid)
end
local id = string.match(KEYS[2], 'jc:jobs:job:(.+)')
redis.call('ZADD', KEYS[3], ARGV[1], id)
redis.call('HSET', KEYS[2], 'status', 'pending', 'run_at', ARGV[1], 'stream_id', '')
return 1
"#;

/// **dead_letter** — park a job in the dead-letter set.
///
/// `KEYS[1]` stream, `KEYS[2]` job hash, `KEYS[3]` dead zset. `XACK`+`XDEL` the
/// current entry, `ZADD` to the dead set scored by **id** (deterministic
/// `list_dead` order without a clock), status → `dead`, `stream_id` cleared.
const DEAD_LETTER_LUA: &str = r#"
if redis.call('EXISTS', KEYS[2]) == 0 then
  return 0
end
local sid = redis.call('HGET', KEYS[2], 'stream_id')
if sid and sid ~= '' then
  redis.call('XACK', KEYS[1], 'jc', sid)
  redis.call('XDEL', KEYS[1], sid)
end
local id = string.match(KEYS[2], 'jc:jobs:job:(.+)')
redis.call('ZADD', KEYS[3], tonumber(id), id)
redis.call('HSET', KEYS[2], 'status', 'dead', 'stream_id', '')
return 1
"#;

/// **requeue_dead** — an admin requeue: immediately leasable, attempts reset.
///
/// `KEYS[1]` dead zset, `KEYS[2]` job hash, `KEYS[3]` stream. `ZREM` from the
/// dead set, reset `attempts` to 0, status → `pending`, `XADD` straight onto the
/// ready Stream (immediately leasable) and store the new `stream_id`. Matches
/// the in-memory/Postgres "due immediately, attempts reset" admin semantics.
const REQUEUE_DEAD_LUA: &str = r#"
if redis.call('EXISTS', KEYS[2]) == 0 then
  return 0
end
local id = string.match(KEYS[2], 'jc:jobs:job:(.+)')
redis.call('ZREM', KEYS[1], id)
local stream_id = redis.call('XADD', KEYS[3], '*', 'id', id)
redis.call('HSET', KEYS[2], 'status', 'pending', 'attempts', '0', 'run_at', '0', 'stream_id', stream_id)
return 1
"#;

/// A durable, multi-node [`JobStore`] over Redis Streams (spec §v2.3b).
/// Construct with [`RedisStore::connect`].
///
/// Holds one auto-reconnecting [`redis::aio::ConnectionManager`] (a
/// cheap-to-clone handle over a single multiplexed connection), so no method
/// opens a fresh connection. Suitable for multi-node deployments where the
/// Postgres store is unavailable; the cron leader is the in-memory
/// single-process leader (whose duplicate cross-node ticks are collapsed by the
/// store's atomic idempotency).
#[derive(Clone)]
pub struct RedisStore {
    conn: redis::aio::ConnectionManager,
}

impl RedisStore {
    /// Connect to `url` (e.g. `redis://127.0.0.1/` or `rediss://host/`). Async
    /// connection setup happens here (mirrors `Db::connect` and
    /// `jerrycan_ratelimit`'s `RedisStore::connect`); fails fast if the server
    /// is unreachable.
    pub async fn connect(url: &str) -> Result<Self> {
        let client = redis::Client::open(url)
            .map_err(|e| Error::internal(format!("jerrycan-jobs redis open: {e}")))?;
        let conn = redis::aio::ConnectionManager::new(client)
            .await
            .map_err(|e| Error::internal(format!("jerrycan-jobs redis connect: {e}")))?;
        Ok(Self { conn })
    }

    /// Ensure the consumer group exists on `stream` (lazily, idempotently):
    /// `XGROUP CREATE … 0 MKSTREAM`, swallowing the `BUSYGROUP` "already exists"
    /// reply so concurrent leasers race harmlessly.
    ///
    /// The start id MUST be `0`, not `$`: `enqueue` `XADD`s onto the ready stream
    /// *before* any group exists (the group is created lazily here, on the first
    /// `lease`/`requeue_dead`). `$` would create the group at the stream's current
    /// tail, so every job enqueued before that first lease would be permanently
    /// undelivered (and never trimmed). `0` covers the whole stream history,
    /// including that backlog — the standard choice when producers may write
    /// before the consumer group exists. The ignored live `redis_store` tests
    /// fail 5/6 if this regresses to `$`.
    async fn ensure_group(conn: &mut redis::aio::ConnectionManager, stream: &str) -> Result<()> {
        let res: redis::RedisResult<()> = redis::cmd("XGROUP")
            .arg("CREATE")
            .arg(stream)
            .arg(GROUP)
            .arg("0")
            .arg("MKSTREAM")
            .query_async(conn)
            .await;
        match res {
            Ok(()) => Ok(()),
            // The group already exists — the only benign error here.
            Err(e) if e.code() == Some("BUSYGROUP") => Ok(()),
            Err(e) => Err(redis_err(e)),
        }
    }

    /// Build a [`Job`] from a flat `HGETALL` field/value list plus its `id`.
    /// A missing/garbled field is schema corruption, so it fails loud.
    fn job_from_hash(id: i64, fields: &[(String, String)]) -> Result<Job> {
        let get = |k: &str| -> Result<&str> {
            fields
                .iter()
                .find(|(f, _)| f == k)
                .map(|(_, v)| v.as_str())
                .ok_or_else(|| {
                    Error::internal(format!("jerrycan-jobs redis: job hash missing field `{k}`"))
                })
        };
        let parse_i64 = |k: &str| -> Result<i64> {
            get(k)?.parse::<i64>().map_err(|e| {
                Error::internal(format!("jerrycan-jobs redis: field `{k}` not an int: {e}"))
            })
        };
        let payload_str = get("payload")?;
        let payload = serde_json::from_str(payload_str).map_err(|e| {
            Error::internal(format!(
                "jerrycan-jobs redis: payload is not valid JSON: {e}"
            ))
        })?;
        let status = match get("status")? {
            "pending" => JobStatus::Pending,
            "leased" => JobStatus::Leased,
            "done" => JobStatus::Done,
            "dead" => JobStatus::Dead,
            other => {
                return Err(Error::internal(format!(
                    "jerrycan-jobs redis: unknown job status {other:?}"
                )));
            }
        };
        let idem = get("idem")?;
        Ok(Job {
            id,
            name: get("name")?.to_string(),
            queue: get("queue")?.to_string(),
            payload,
            run_at: from_millis(parse_i64("run_at")?),
            attempts: parse_i64("attempts")? as u32,
            max_attempts: parse_i64("max_attempts")? as u32,
            status,
            idempotency_key: if idem.is_empty() {
                None
            } else {
                Some(idem.to_string())
            },
            // The store does not carry an explicit lease-expiry timestamp: the
            // lease lives in the Stream PEL (Redis-server idle time), so the
            // reconstructed Job leaves it None. Reclaim is decided by
            // XAUTOCLAIM's min-idle, not this field.
            lease_expires_at: None,
            created_at: from_millis(parse_i64("created_at")?),
        })
    }

    /// Read a job hash by id (the shared read path for `list_dead`). Returns
    /// `None` if the hash is gone (acked-then-trimmed race).
    async fn load_job(conn: &mut redis::aio::ConnectionManager, id: i64) -> Result<Option<Job>> {
        let fields: Vec<(String, String)> = redis::cmd("HGETALL")
            .arg(job_key(id))
            .query_async(conn)
            .await
            .map_err(redis_err)?;
        if fields.is_empty() {
            return Ok(None);
        }
        Ok(Some(Self::job_from_hash(id, &fields)?))
    }
}

impl JobStore for RedisStore {
    fn enqueue<'a>(&'a self, job: NewJob, now: SystemTime) -> JobFuture<'a, EnqueueOutcome> {
        Box::pin(async move {
            let mut conn = self.conn.clone();
            let run_at = to_millis(job.run_at.unwrap_or(now));
            let max_attempts = job.max_attempts.unwrap_or(DEFAULT_MAX_ATTEMPTS);
            let payload = job.payload.to_string();
            let created_at = to_millis(now);
            let idem = job.idempotency_key.clone().unwrap_or_default();
            let idem_full = job
                .idempotency_key
                .as_deref()
                .map(idem_key)
                .unwrap_or_default();

            // The script returns {tag, id} where tag is "new" or "dup".
            let (tag, id): (String, i64) = redis::Script::new(ENQUEUE_LUA)
                .key(seq_key())
                .key(stream_key(&job.queue))
                .key(sched_key(&job.queue))
                .arg(&job.name)
                .arg(&job.queue)
                .arg(payload)
                .arg(run_at)
                .arg(max_attempts)
                .arg(idem)
                .arg(created_at)
                .arg(to_millis(now))
                .arg(idem_full)
                .invoke_async(&mut conn)
                .await
                .map_err(redis_err)?;

            match tag.as_str() {
                "new" => Ok(EnqueueOutcome::Inserted(id)),
                "dup" => Ok(EnqueueOutcome::Duplicate(id)),
                other => Err(Error::internal(format!(
                    "jerrycan-jobs redis: enqueue returned unexpected tag {other:?}"
                ))),
            }
        })
    }

    fn lease<'a>(
        &'a self,
        queue: &'a str,
        now: SystemTime,
        lease: Duration,
        max: u32,
    ) -> JobFuture<'a, Vec<Job>> {
        Box::pin(async move {
            let mut conn = self.conn.clone();
            let stream = stream_key(queue);
            let now_ms = to_millis(now);
            let lease_ms = lease.as_millis() as u64;

            Self::ensure_group(&mut conn, &stream).await?;

            // 1) Promote due scheduled jobs onto the ready Stream (atomic).
            let _: i64 = redis::Script::new(PROMOTE_LUA)
                .key(sched_key(queue))
                .key(&stream)
                .arg(now_ms)
                .arg(max as i64)
                .invoke_async(&mut conn)
                .await
                .map_err(redis_err)?;

            // The Stream entry ids we claimed, in claim order. We mark each
            // leased + build its Job afterwards.
            let mut claimed: Vec<(String, i64)> = Vec::new();

            // 2) Reclaim crashed workers: XAUTOCLAIM with min-idle = the lease.
            // A still-running job (PEL idle < lease) is NOT stolen; a crashed
            // worker's entry (idle >= lease) is reassigned to us. Idle time is
            // Redis-server time, not the injected `now` (inherent to Streams).
            if (claimed.len() as u32) < max {
                let remaining = max - claimed.len() as u32;
                // XAUTOCLAIM key group consumer min-idle start COUNT n
                // reply: [cursor, [[entry_id, [field, value, ...]], ...], deleted]
                type AutoclaimReply = (String, Vec<(String, Vec<String>)>, Vec<String>);
                let reply: AutoclaimReply = redis::cmd("XAUTOCLAIM")
                    .arg(&stream)
                    .arg(GROUP)
                    .arg(CONSUMER)
                    .arg(lease_ms)
                    .arg("0-0")
                    .arg("COUNT")
                    .arg(remaining)
                    .query_async(&mut conn)
                    .await
                    .map_err(redis_err)?;
                for (entry_id, kv) in reply.1 {
                    if let Some(id) = entry_field_id(&kv) {
                        claimed.push((entry_id, id));
                    }
                }
            }

            // 3) Claim new (never-delivered) entries via the consumer group.
            if (claimed.len() as u32) < max {
                let remaining = max - claimed.len() as u32;
                // XREADGROUP GROUP jc jc COUNT n STREAMS stream >
                // reply: [[stream_name, [[entry_id, [field, value, ...]], ...]]]
                type ReadReply = Vec<(String, Vec<(String, Vec<String>)>)>;
                let reply: Option<ReadReply> = redis::cmd("XREADGROUP")
                    .arg("GROUP")
                    .arg(GROUP)
                    .arg(CONSUMER)
                    .arg("COUNT")
                    .arg(remaining)
                    .arg("STREAMS")
                    .arg(&stream)
                    .arg(">")
                    .query_async(&mut conn)
                    .await
                    .map_err(redis_err)?;
                if let Some(streams) = reply {
                    for (_name, entries) in streams {
                        for (entry_id, kv) in entries {
                            if let Some(id) = entry_field_id(&kv) {
                                claimed.push((entry_id, id));
                            }
                        }
                    }
                }
            }

            // For every claimed entry: bump attempts, set leased, build the Job.
            // An entry whose hash is gone (acked-then-trimmed race) is skipped
            // and its orphan Stream entry is XACK+XDEL'd so it is not reclaimed.
            let mut leased: Vec<Job> = Vec::with_capacity(claimed.len());
            for (entry_id, id) in claimed {
                let hash: Option<Vec<(String, String)>> = redis::Script::new(LEASE_ENTRY_LUA)
                    .key(job_key(id))
                    .arg(&entry_id)
                    .invoke_async(&mut conn)
                    .await
                    .map_err(redis_err)?;
                match hash {
                    Some(fields) if !fields.is_empty() => {
                        leased.push(Self::job_from_hash(id, &fields)?);
                    }
                    _ => {
                        // Orphan entry (hash gone): drop it from the Stream/PEL.
                        let _: () = redis::cmd("XACK")
                            .arg(&stream)
                            .arg(GROUP)
                            .arg(&entry_id)
                            .query_async(&mut conn)
                            .await
                            .map_err(redis_err)?;
                        let _: () = redis::cmd("XDEL")
                            .arg(&stream)
                            .arg(&entry_id)
                            .query_async(&mut conn)
                            .await
                            .map_err(redis_err)?;
                    }
                }
            }

            // Ordered by id to match the reference's deterministic claim order.
            leased.sort_by_key(|j| j.id);
            Ok(leased)
        })
    }

    fn ack<'a>(&'a self, id: i64) -> JobFuture<'a, ()> {
        Box::pin(async move {
            let mut conn = self.conn.clone();
            let queue = job_queue(&mut conn, id).await?;
            let Some(queue) = queue else { return Ok(()) };
            let _: i64 = redis::Script::new(ACK_LUA)
                .key(stream_key(&queue))
                .key(job_key(id))
                .invoke_async(&mut conn)
                .await
                .map_err(redis_err)?;
            Ok(())
        })
    }

    fn retry<'a>(&'a self, id: i64, backoff_until: SystemTime) -> JobFuture<'a, ()> {
        Box::pin(async move {
            let mut conn = self.conn.clone();
            let Some(queue) = job_queue(&mut conn, id).await? else {
                return Ok(());
            };
            let _: i64 = redis::Script::new(RETRY_LUA)
                .key(stream_key(&queue))
                .key(job_key(id))
                .key(sched_key(&queue))
                .arg(to_millis(backoff_until))
                .invoke_async(&mut conn)
                .await
                .map_err(redis_err)?;
            Ok(())
        })
    }

    fn dead_letter<'a>(&'a self, id: i64) -> JobFuture<'a, ()> {
        Box::pin(async move {
            let mut conn = self.conn.clone();
            let Some(queue) = job_queue(&mut conn, id).await? else {
                return Ok(());
            };
            let _: i64 = redis::Script::new(DEAD_LETTER_LUA)
                .key(stream_key(&queue))
                .key(job_key(id))
                .key(dead_key(&queue))
                .invoke_async(&mut conn)
                .await
                .map_err(redis_err)?;
            Ok(())
        })
    }

    fn list_dead<'a>(&'a self, queue: &'a str, limit: u32) -> JobFuture<'a, Vec<Job>> {
        Box::pin(async move {
            let mut conn = self.conn.clone();
            if limit == 0 {
                return Ok(Vec::new());
            }
            // Scored by id ⇒ ZRANGE by score is insertion order.
            let ids: Vec<i64> = redis::cmd("ZRANGE")
                .arg(dead_key(queue))
                .arg(0)
                .arg((limit as i64) - 1)
                .query_async(&mut conn)
                .await
                .map_err(redis_err)?;
            let mut out = Vec::with_capacity(ids.len());
            for id in ids {
                if let Some(job) = Self::load_job(&mut conn, id).await? {
                    out.push(job);
                }
            }
            Ok(out)
        })
    }

    fn requeue_dead<'a>(&'a self, id: i64) -> JobFuture<'a, ()> {
        Box::pin(async move {
            let mut conn = self.conn.clone();
            let Some(queue) = job_queue(&mut conn, id).await? else {
                return Ok(());
            };
            // The ready Stream must have its group before XADD-then-XREADGROUP.
            Self::ensure_group(&mut conn, &stream_key(&queue)).await?;
            let _: i64 = redis::Script::new(REQUEUE_DEAD_LUA)
                .key(dead_key(&queue))
                .key(job_key(id))
                .key(stream_key(&queue))
                .invoke_async(&mut conn)
                .await
                .map_err(redis_err)?;
            Ok(())
        })
    }
}

/// Pull the `id` field value out of a flat Stream-entry field list
/// (`[field, value, field, value, ...]`).
fn entry_field_id(kv: &[String]) -> Option<i64> {
    kv.chunks_exact(2)
        .find(|c| c[0] == "id")
        .and_then(|c| c[1].parse::<i64>().ok())
}

/// Read a job's `queue` from its hash, for the id-only methods (`ack`/`retry`/
/// `dead_letter`/`requeue_dead`) that must locate the per-queue keys. Returns
/// `None` if the hash is gone (the method then no-ops, matching the reference's
/// "missing id is a silent no-op").
async fn job_queue(conn: &mut redis::aio::ConnectionManager, id: i64) -> Result<Option<String>> {
    let queue: Option<String> = redis::cmd("HGET")
        .arg(job_key(id))
        .arg("queue")
        .query_async(conn)
        .await
        .map_err(redis_err)?;
    Ok(queue)
}
