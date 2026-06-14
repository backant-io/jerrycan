//! Background job engine for jerrycan (spec §v2.3): at-least-once queues with
//! retries + dead-letter, cron with skip-missed semantics, run_at delayed jobs,
//! over a Postgres (default) or Redis store. <https://jerrycan.cc>
#![forbid(unsafe_code)]

pub mod cron;
pub mod store;
pub use cron::{CronError, CronSchedule, due_fire};
pub use store::{
    DEFAULT_MAX_ATTEMPTS, EnqueueOutcome, InMemoryStore, Job, JobFuture, JobStatus, JobStore,
    NewJob,
};
