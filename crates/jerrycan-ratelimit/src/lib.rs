//! Fixed-window, identity-aware rate limiting as a jerrycan extension. The
//! store layer (this + redis); the RateLimit extension + middleware land in the
//! sibling modules. <https://jerrycan.cc>
#![forbid(unsafe_code)]

pub mod store;
pub use store::{InMemoryStore, Outcome, RateLimitStore};
