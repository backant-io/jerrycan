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
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The current UTC instant as an RFC3339 string with seconds precision and a `Z`
/// suffix (e.g. `2026-07-26T12:34:56Z`) — the exact shape jerrycan's `datetime`
/// fields ride as (`String`) and the OpenAPI/fixture format uses.
///
/// This is the runtime companion to the design-contract `"default": "now"`
/// sentinel (issue #110): a `datetime` field defaulting to `"now"` is omitted from
/// the request DTO, and the generated create handler sets it to `now_rfc3339()`.
/// Prelude-exported so a generated handler under `use jerrycan::prelude::*;` calls
/// it bare, with no ad-hoc `chrono` dependency.
///
/// Reads the real system clock directly (NOT the injectable [`Clock`] DI): a
/// server-set create timestamp is wall-clock provenance, not domain time a test
/// rewinds. Chrono-free — the conversion is Howard Hinnant's civil-from-days
/// algorithm — so the leanest crate in the tree stays dependency-light (mirroring
/// jerrycan-db's deliberately chrono-free timestamp).
pub fn now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (year, month, day, hour, min, sec) = civil_from_unix_secs(secs);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}Z")
}

/// Convert Unix seconds (>= 0) to civil `(year, month, day, hour, minute, second)`
/// in UTC. `days_from_civil`'s exact inverse (Howard Hinnant,
/// <http://howardhinnant.github.io/date_algorithms.html>) — valid for any date in
/// the proleptic Gregorian calendar; a jerrycan `now` is always post-1970, so the
/// non-negative path is all that runs.
fn civil_from_unix_secs(secs: u64) -> (i64, u32, u32, u32, u32, u32) {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (hour, min, sec) = (
        (rem / 3600) as u32,
        ((rem % 3600) / 60) as u32,
        (rem % 60) as u32,
    );
    // days -> civil date, shifting the era so March is month 0 (leap day last).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // day of era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // month shifted [0, 11] (0 = March)
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    let year = if month <= 2 { y + 1 } else { y };
    (year, month, day, hour, min, sec)
}

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

    /// The civil conversion is exact for known epoch anchors — a wrong date here
    /// would plant a garbage `created_at` on every `default:"now"` row (#110), so
    /// pin it against timestamps whose UTC calendar date is unambiguous.
    #[test]
    fn civil_from_unix_secs_matches_known_anchors() {
        assert_eq!(super::civil_from_unix_secs(0), (1970, 1, 1, 0, 0, 0));
        // 2000-01-01T00:00:00Z = 946684800 (crosses the 1900/2000 century rule).
        assert_eq!(
            super::civil_from_unix_secs(946_684_800),
            (2000, 1, 1, 0, 0, 0)
        );
        // 2020-02-29T23:59:59Z = 1583020799 (a leap day, last second).
        assert_eq!(
            super::civil_from_unix_secs(1_583_020_799),
            (2020, 2, 29, 23, 59, 59)
        );
        // 2026-07-26T12:34:56Z = 1785069296.
        assert_eq!(
            super::civil_from_unix_secs(1_785_069_296),
            (2026, 7, 26, 12, 34, 56)
        );
    }

    /// The formatted string is RFC3339 UTC with seconds precision and a `Z` —
    /// byte-for-byte the shape jerrycan `datetime` fixtures/OpenAPI use, so a
    /// server-set `now` value round-trips through the same datetime contract.
    #[test]
    fn now_rfc3339_is_seconds_precision_utc_with_z() {
        let s = now_rfc3339();
        assert_eq!(s.len(), 20, "YYYY-MM-DDTHH:MM:SSZ is 20 chars: {s}");
        assert!(s.ends_with('Z'), "UTC `Z` suffix, not an offset: {s}");
        assert_eq!(&s[4..5], "-");
        assert_eq!(&s[10..11], "T");
        assert!(!s.contains('.'), "no fractional seconds: {s}");
        // A plausible current year (four leading digits, >= 2024).
        let year: i64 = s[..4].parse().expect("leading 4-digit year");
        assert!(year >= 2024, "reads the real clock: {s}");
    }

    /// Formatting agrees with the civil conversion for a fixed instant — the
    /// zero-padding and field order are what makes the value a valid `datetime`.
    #[test]
    fn format_zero_pads_every_field() {
        let (y, mo, d, h, mi, s) = super::civil_from_unix_secs(946_684_800);
        assert_eq!(
            format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z"),
            "2000-01-01T00:00:00Z"
        );
    }
}
