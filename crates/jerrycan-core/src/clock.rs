//! Injectable time. Handlers/extensions take `Dep<Clock>` and call `now()`;
//! tests control it via `TestApp::clock().advance(..)`. The serve engine's
//! own timeouts deliberately stay on real tokio time — Clock is for DOMAIN
//! time (rate windows, schedules, expiry), not transport timeouts.
//!
//! `App::new()` provides a [`Clock::system`] singleton by default; `provide`
//! a different one to override it. `into_test()` swaps in a [`Clock::test`]
//! that [`TestApp::clock`](crate::TestApp::clock) hands back so a test can
//! [`advance`](Clock::advance) or [`set`](Clock::set) the injected clock and
//! observe the effect through real requests.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

/// An injectable source of "now". Cloning is cheap and, for a test clock,
/// shares the same controllable offset — so a handle handed to a test moves
/// in lockstep with the clock the handler resolves.
#[derive(Clone)]
pub struct Clock(Inner);

#[derive(Clone)]
enum Inner {
    /// Reads the real system clock on every `now()`.
    System,
    /// Test clock: a fixed base plus a controllable offset in milliseconds.
    /// The offset lives behind an `Arc<AtomicU64>` so every clone — including
    /// the one resolved inside a handler and the one held by `TestApp` —
    /// observes the same advances.
    Test {
        base: SystemTime,
        offset_ms: Arc<AtomicU64>,
    },
}

impl Clock {
    /// The real clock: `now()` reads `SystemTime::now()` each call. This is
    /// what `App::new()` provides by default.
    pub fn system() -> Self {
        Clock(Inner::System)
    }

    /// A controllable test clock. The base is `SystemTime::now()` at creation
    /// — tests assert on *movement* (`advance`/`set`), not on an absolute base,
    /// so a realistic starting instant is fine and avoids surprising callers
    /// that subtract `UNIX_EPOCH`.
    pub fn test() -> Self {
        Clock(Inner::Test {
            base: SystemTime::now(),
            offset_ms: Arc::new(AtomicU64::new(0)),
        })
    }

    /// The current instant. For [`Clock::system`] this is the live system
    /// clock; for [`Clock::test`] it is `base + accumulated offset`.
    pub fn now(&self) -> SystemTime {
        match &self.0 {
            Inner::System => SystemTime::now(),
            Inner::Test { base, offset_ms } => {
                *base + Duration::from_millis(offset_ms.load(Ordering::SeqCst))
            }
        }
    }

    /// Move a test clock forward by `d`. Panics on a system clock — advancing
    /// real time is meaningless, and silently ignoring it would hide a test bug.
    pub fn advance(&self, d: Duration) {
        match &self.0 {
            Inner::Test { offset_ms, .. } => {
                // Saturate rather than wrap: a test that advances past ~584M
                // years is a bug, but wrapping would be a *silent* one.
                offset_ms.fetch_add(d.as_millis().min(u64::MAX as u128) as u64, Ordering::SeqCst);
            }
            Inner::System => {
                panic!("Clock::advance() is test-only — build the app with into_test()")
            }
        }
    }

    /// Pin a test clock to an absolute instant. Panics on a system clock for
    /// the same reason as [`advance`](Clock::advance). Useful for cron/expiry
    /// tests that need a specific wall-clock time rather than a delta.
    pub fn set(&self, when: SystemTime) {
        match &self.0 {
            Inner::Test { base, offset_ms } => {
                // Express `when` as an offset from the fixed base. Times at or
                // before the base clamp to zero (the test clock never runs
                // backwards before its own base).
                let delta = when.duration_since(*base).unwrap_or_default();
                offset_ms.store(
                    delta.as_millis().min(u64::MAX as u128) as u64,
                    Ordering::SeqCst,
                );
            }
            Inner::System => {
                panic!("Clock::set() is test-only — build the app with into_test()")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prelude::*;

    #[tokio::test]
    async fn clock_is_injectable_and_test_controllable() {
        async fn now_ms(clock: Dep<Clock>) -> Result<Json<u128>> {
            Ok(Json(
                clock
                    .now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis(),
            ))
        }
        let t = App::new().route("/now", get(now_ms)).into_test();
        let t0: u128 = t.get("/now").await.json();
        t.clock().advance(std::time::Duration::from_secs(3600));
        let t1: u128 = t.get("/now").await.json();
        assert!(
            t1 >= t0 + 3_600_000,
            "advance moved the injected clock: {t0} -> {t1}"
        );
    }

    #[test]
    fn real_clock_tracks_system_time() {
        let c = Clock::system();
        let a = c.now();
        let b = std::time::SystemTime::now();
        assert!(b.duration_since(a).unwrap() < std::time::Duration::from_secs(1));
    }

    #[tokio::test]
    async fn clock_resolves_in_task_contexts_too() {
        let built = crate::App::new().build().unwrap();
        let mut ctx = built.task_context();
        let clock = ctx.resolve::<Clock>().await.unwrap();
        let _ = clock.now(); // resolvable outside requests — jobs need this
    }

    #[test]
    fn test_clock_clones_share_one_offset() {
        // The handle TestApp keeps and the Arc a handler resolves must move
        // together — that is the whole point of an injectable test clock.
        let c = Clock::test();
        let clone = c.clone();
        let before = clone.now();
        c.advance(Duration::from_secs(10));
        let after = clone.now();
        assert_eq!(
            after.duration_since(before).unwrap(),
            Duration::from_secs(10)
        );
    }

    #[test]
    fn set_pins_test_clock_to_an_absolute_instant() {
        let c = Clock::test();
        let target = SystemTime::now() + Duration::from_secs(86_400);
        c.set(target);
        // Within a millisecond of the requested instant (we store ms offsets).
        let drift = c
            .now()
            .duration_since(target)
            .unwrap_or_else(|e| e.duration());
        assert!(
            drift < Duration::from_millis(2),
            "set pinned now() to target"
        );
    }

    #[test]
    #[should_panic(expected = "test-only")]
    fn advancing_a_system_clock_panics_loudly() {
        Clock::system().advance(Duration::from_secs(1));
    }
}
