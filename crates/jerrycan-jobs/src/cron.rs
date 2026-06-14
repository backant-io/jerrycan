//! A minimal 5-field cron parser (minute hour day-of-month month day-of-week),
//! matching the design contract's frozen shape (digits, `*`, `,`, `/`, `-`).
//! Pure: next-fire is a function of a `SystemTime`, so cron is deterministic
//! under the injected `Clock` (no `chrono`).
//!
//! Civil-time decomposition uses Howard Hinnant's integer-only
//! `civil_from_days` algorithm (handles leap years with no lookup tables), so a
//! `SystemTime` is split into UTC (year, month, day, hour, minute) and the
//! day-of-week falls out of the epoch day number. `next_after` searches forward
//! in minute steps rather than recomposing, so only the decomposition is used.

use std::time::{Duration, SystemTime};

/// Seconds in a minute — every cron tick is minute-aligned at second 0.
const SECS_PER_MINUTE: u64 = 60;
/// Minutes in a day, for the [`CronSchedule::next_after`] search bound.
const MINUTES_PER_DAY: u64 = 24 * 60;

/// Search bound for [`CronSchedule::next_after`]: just over four calendar years
/// of minutes. Any schedule that has not matched within this window is treated
/// as "never fires again" (e.g. `0 0 30 2 *` — Feb 30 does not exist). Four
/// years guarantees a leap day has been seen, so legitimately rare schedules
/// like `0 0 29 2 *` still resolve. The loop is therefore **always bounded** —
/// it never spins on an impossible schedule.
const SEARCH_BOUND_MINUTES: u64 = 4 * 366 * MINUTES_PER_DAY;

/// A parsed 5-field cron schedule. Each field is the **set** of values it
/// permits, stored sorted and deduped. `dom_restricted` / `dow_restricted`
/// record whether day-of-month / day-of-week were narrower than `*`, which
/// selects the standard cron OR rule between them (see `CronSchedule::matches`).
#[derive(Debug, Clone)]
pub struct CronSchedule {
    minute: Vec<u8>,
    hour: Vec<u8>,
    dom: Vec<u8>,
    month: Vec<u8>,
    dow: Vec<u8>,
    dom_restricted: bool,
    dow_restricted: bool,
}

/// A cron parse failure with a human-readable reason. Carries the offending
/// detail so a misconfigured `JobDesign.schedule` fails loud, not silently.
#[derive(Debug)]
pub struct CronError(pub String);

impl std::fmt::Display for CronError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid cron schedule: {}", self.0)
    }
}

impl std::error::Error for CronError {}

/// A UTC civil date-time, decomposed from Unix seconds with Howard Hinnant's
/// integer-only algorithm. `dow` is 0=Sunday..6=Saturday. Cron has no year
/// field, so `year` is not consulted by `CronSchedule::matches`; it is kept
/// because the decomposition computes it for free and the leap-year tests read
/// it back to assert the correct year was reached.
struct Civil {
    #[cfg_attr(not(test), allow(dead_code))]
    year: i64,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    dow: u32,
}

impl Civil {
    /// Decompose Unix seconds (UTC) into a civil date-time. Negative seconds
    /// (pre-1970) floor-divide correctly, but the engine only ever feeds
    /// epoch-relative times, so the common path is non-negative.
    fn from_unix_secs(secs: i64) -> Self {
        let days = secs.div_euclid(86_400);
        let rem = secs.rem_euclid(86_400);
        let hour = (rem / 3600) as u32;
        let minute = ((rem % 3600) / 60) as u32;

        // civil_from_days (Hinnant): days since 1970-01-01 → (y, m, d).
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097; // [0, 146096]
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
        let mp = (5 * doy + 2) / 153; // [0, 11]
        let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
        let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
        let year = if month <= 2 { y + 1 } else { y };

        // Day of week from the epoch day number: 1970-01-01 was a Thursday (4),
        // so (days + 4) mod 7 gives 0=Sunday.
        let dow = (days.rem_euclid(7) + 4).rem_euclid(7) as u32;

        Self {
            year,
            month,
            day,
            hour,
            minute,
            dow,
        }
    }
}

impl CronSchedule {
    /// Parse a 5-field cron expression (minute hour day-of-month month
    /// day-of-week), whitespace-separated. Out-of-range values, step 0,
    /// malformed syntax, and a wrong field count are all rejected — the frozen
    /// contract shape `[0-9*,/-]` is necessary but not sufficient, so this is
    /// the loud second gate.
    pub fn parse(expr: &str) -> Result<Self, CronError> {
        let fields: Vec<&str> = expr.split_whitespace().collect();
        if fields.len() != 5 {
            return Err(CronError(format!(
                "expected 5 whitespace-separated fields, got {}",
                fields.len()
            )));
        }

        let dom_restricted = fields[2] != "*";
        let dow_restricted = fields[4] != "*";

        Ok(Self {
            minute: parse_field(fields[0], 0, 59)?,
            hour: parse_field(fields[1], 0, 23)?,
            dom: parse_field(fields[2], 1, 31)?,
            month: parse_field(fields[3], 1, 12)?,
            dow: parse_field(fields[4], 0, 6)?,
            dom_restricted,
            dow_restricted,
        })
    }

    /// The first scheduled instant strictly **after** `after` (UTC,
    /// minute-aligned at second 0). The search steps minute-by-minute from the
    /// minute after `after` and is capped at `SEARCH_BOUND_MINUTES`; if no
    /// match is found within that window the schedule is impossible (e.g. Feb
    /// 30) and the far-future `never()` sentinel is returned so the worker
    /// simply never fires it. The loop is always bounded.
    pub fn next_after(&self, after: SystemTime) -> SystemTime {
        let after_secs = after
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        // Start at the next whole minute strictly after `after`: truncate to the
        // minute, then step one minute forward — so an exact fire minute yields
        // the *next* match, not itself.
        let start_minute = after_secs / SECS_PER_MINUTE + 1;

        for step in 0..SEARCH_BOUND_MINUTES {
            let candidate_minute = start_minute + step;
            let secs = (candidate_minute * SECS_PER_MINUTE) as i64;
            let civil = Civil::from_unix_secs(secs);
            if self.matches(&civil) {
                return SystemTime::UNIX_EPOCH + Duration::from_secs(secs as u64);
            }
        }
        never()
    }

    /// The most recent scheduled instant at **or before** `at` (UTC,
    /// minute-aligned at second 0), or `None` if no tick matches within the
    /// bounded search window. The mirror image of [`CronSchedule::next_after`]:
    /// it truncates `at` to its minute and steps minute-by-minute **backward**,
    /// decomposing each candidate with the same `Civil::from_unix_secs` and
    /// testing it with the same `CronSchedule::matches`, so the two directions
    /// share one field-match implementation and cannot diverge. `None` means
    /// either an impossible schedule (e.g. `0 0 30 2 *` — Feb 30) or that `at`
    /// precedes the schedule's first-ever tick; the scan stops at minute 0 so it
    /// never underflows below the epoch. The loop is capped at
    /// `SEARCH_BOUND_MINUTES`, so it is always bounded.
    pub fn prev_at_or_before(&self, at: SystemTime) -> Option<SystemTime> {
        let at_secs = at
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        // Truncate to the minute: an exact fire minute is itself a candidate, so
        // an at-or-before query on a matching minute returns that minute.
        let start_minute = at_secs / SECS_PER_MINUTE;

        for step in 0..SEARCH_BOUND_MINUTES {
            // Stop at the epoch: there is no minute before 0 to scan.
            let candidate_minute = match start_minute.checked_sub(step) {
                Some(m) => m,
                None => break,
            };
            let secs = (candidate_minute * SECS_PER_MINUTE) as i64;
            let civil = Civil::from_unix_secs(secs);
            if self.matches(&civil) {
                return Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs as u64));
            }
        }
        None
    }

    /// Whether a civil instant matches every field. Day-of-month vs
    /// day-of-week follows standard cron: when **both** are restricted the day
    /// matches if **either** does (OR); when one is `*` only the other
    /// constrains the day.
    fn matches(&self, c: &Civil) -> bool {
        if !self.minute.contains(&(c.minute as u8))
            || !self.hour.contains(&(c.hour as u8))
            || !self.month.contains(&(c.month as u8))
        {
            return false;
        }

        let dom_ok = self.dom.contains(&(c.day as u8));
        let dow_ok = self.dow.contains(&(c.dow as u8));

        match (self.dom_restricted, self.dow_restricted) {
            (true, true) => dom_ok || dow_ok, // OR when both restricted
            (true, false) => dom_ok,
            (false, true) => dow_ok,
            (false, false) => true, // both `*` — day unconstrained
        }
    }
}

/// The far-future "never fires" sentinel returned by [`CronSchedule::next_after`]
/// when no match exists within the search bound. Far enough out that no real
/// `Clock::now()` reaches it, so the worker leaves the job unfired forever.
fn never() -> SystemTime {
    // ~year 5000+ — comfortably beyond any production clock.
    SystemTime::UNIX_EPOCH + Duration::from_secs(100_000 * 365 * 86_400)
}

/// Parse one cron field into the sorted, deduped set of values it permits.
/// Accepts `*`, `N`, `A-B`, `A-B/S`, `*/S`, and comma-separated lists of those.
/// Rejects out-of-range values, descending ranges, step 0, and any malformed
/// token — only `[0-9*,/-]` is admissible, and even within that, nonsense fails.
fn parse_field(spec: &str, min: u8, max: u8) -> Result<Vec<u8>, CronError> {
    if spec.is_empty() {
        return Err(CronError("empty field".into()));
    }
    // Reject any char outside the frozen shape up front (defence in depth).
    if let Some(bad) = spec
        .chars()
        .find(|ch| !matches!(ch, '0'..='9' | '*' | ',' | '/' | '-'))
    {
        return Err(CronError(format!(
            "illegal character {bad:?} in field {spec:?}"
        )));
    }

    let mut values: Vec<u8> = Vec::new();
    for part in spec.split(',') {
        if part.is_empty() {
            return Err(CronError(format!("empty list element in field {spec:?}")));
        }
        parse_element(part, min, max, &mut values)?;
    }

    values.sort_unstable();
    values.dedup();
    Ok(values)
}

/// Parse a single comma-separated element (`*`, `N`, `A-B`, `*/S`, `A-B/S`)
/// and push its expanded values onto `out`.
fn parse_element(part: &str, min: u8, max: u8, out: &mut Vec<u8>) -> Result<(), CronError> {
    // Split an optional `/step` suffix.
    let (range_spec, step) = match part.split_once('/') {
        Some((r, s)) => {
            if s.is_empty() || s.contains('/') {
                return Err(CronError(format!("malformed step in {part:?}")));
            }
            let step: u8 = s
                .parse()
                .map_err(|_| CronError(format!("malformed step in {part:?}")))?;
            if step == 0 {
                return Err(CronError(format!("step 0 in {part:?}")));
            }
            (r, step)
        }
        None => (part, 1),
    };

    // Resolve the range the step iterates over.
    let (lo, hi) = if range_spec == "*" {
        (min, max)
    } else if let Some((a, b)) = range_spec.split_once('-') {
        let a = parse_num(a, part)?;
        let b = parse_num(b, part)?;
        if a > b {
            return Err(CronError(format!("descending range in {part:?}")));
        }
        check_range(a, min, max, part)?;
        check_range(b, min, max, part)?;
        (a, b)
    } else {
        // A single number. A step on a bare number (`5/2`) is nonstandard;
        // reject it as malformed rather than silently treating `5` as `5-max`.
        if step != 1 {
            return Err(CronError(format!("step on a single value in {part:?}")));
        }
        let n = parse_num(range_spec, part)?;
        check_range(n, min, max, part)?;
        out.push(n);
        return Ok(());
    };

    let mut v = lo;
    while v <= hi {
        out.push(v);
        v = v.saturating_add(step);
        if v < lo {
            break; // overflow guard (cannot happen for u8 ranges, but explicit)
        }
    }
    Ok(())
}

/// Parse a bare numeric token, mapping any non-digit / overflow to a parse error.
fn parse_num(s: &str, part: &str) -> Result<u8, CronError> {
    s.parse::<u8>()
        .map_err(|_| CronError(format!("malformed number {s:?} in {part:?}")))
}

/// Reject a value outside the field's inclusive `[min, max]` range.
fn check_range(n: u8, min: u8, max: u8, part: &str) -> Result<(), CronError> {
    if n < min || n > max {
        return Err(CronError(format!(
            "value {n} out of range {min}-{max} in {part:?}"
        )));
    }
    Ok(())
}

/// The single tick to fire now, with **skip-missed** semantics: the **most
/// recent** scheduled tick at-or-before `now` that has not already fired (i.e.
/// is strictly after `last_fired`). A backlog after downtime collapses to one
/// fire — each later run re-evaluates from the advanced `last_fired`, so no tick
/// is lost or double-fired. Returns `None` when nothing new is due.
///
/// This computes the answer directly via [`CronSchedule::prev_at_or_before`] (a
/// bounded backward scan) rather than walking scheduled ticks forward from
/// `last_fired`. It is therefore O(minutes back to the most recent match) —
/// independent of how old `last_fired` is, and crucially it does **not** walk
/// ~30M ticks from the epoch when `last_fired == None`.
///
/// **First-run / leader policy (see the cron leader, Task 5):** a `None`
/// `last_fired` — e.g. a freshly inserted row whose `last_fired` is still NULL —
/// makes this fire the most-recent tick immediately. That is the intended pure
/// semantics and is deliberately left unchanged. Whether to *seed*
/// `last_fired = now` on a first NULL row so a deploy does not immediately fire
/// every cron job is a deploy-policy decision that belongs to the leader, not
/// this pure function.
pub fn due_fire(
    schedule: &CronSchedule,
    last_fired: Option<SystemTime>,
    now: SystemTime,
) -> Option<SystemTime> {
    let prev = schedule.prev_at_or_before(now)?;
    match last_fired {
        Some(lf) if prev <= lf => None, // the most-recent tick already fired
        _ => Some(prev),                // due: first run, or newer than last_fired
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn every_fifteen_fires_on_the_quarter_hours_only() {
        let s = CronSchedule::parse("*/15 * * * *").unwrap();
        let base = at(0); // 1970-01-01 00:00:00 UTC (Thursday)
        let n1 = s.next_after(base);
        assert_eq!(n1, at(15 * 60));
        assert_eq!(s.next_after(n1), at(30 * 60));
        assert_eq!(s.next_after(at(7 * 60)), at(15 * 60), "never fires at :07");
    }

    #[test]
    fn hourly_top_of_hour() {
        let s = CronSchedule::parse("0 * * * *").unwrap();
        assert_eq!(
            s.next_after(at(90)),
            at(3600),
            "next top-of-hour after 00:01:30"
        );
    }

    #[test]
    fn invalid_fields_are_rejected() {
        assert!(
            CronSchedule::parse("60 * * * *").is_err(),
            "minute 60 out of range (0-59)"
        );
        assert!(CronSchedule::parse("* * * *").is_err(), "4 fields not 5");
        assert!(CronSchedule::parse("a * * * *").is_err(), "non-numeric");
        assert!(CronSchedule::parse("*/0 * * * *").is_err(), "step 0");
        assert!(CronSchedule::parse("").is_err());
        assert!(CronSchedule::parse("* * * * * *").is_err(), "6 fields");
    }

    #[test]
    fn due_fire_skips_missed_ticks_after_downtime() {
        let s = CronSchedule::parse("0 * * * *").unwrap();
        let last_fired = Some(at(0)); // fired at 00:00 before downtime
        let now = at(5 * 3600 + 1800); // 05:30
        assert_eq!(
            due_fire(&s, last_fired, now),
            Some(at(5 * 3600)),
            "fire the 05:00 tick once, skip 01:00..04:00"
        );
        assert_eq!(
            due_fire(&s, Some(at(5 * 3600)), now),
            None,
            "nothing due until 06:00"
        );
        assert_eq!(
            due_fire(&s, Some(at(5 * 3600)), at(6 * 3600)),
            Some(at(6 * 3600))
        );
    }

    // ----- Step 3: exhaustive parser/semantics tests (the parser is fuzzed) -----

    #[test]
    fn comma_lists_fire_at_each_listed_value() {
        let s = CronSchedule::parse("0,30 * * * *").unwrap();
        // From 00:00:00, the next fires are :30, then the next hour :00, :30, ...
        let a = s.next_after(at(0));
        assert_eq!(a, at(30 * 60), "first after 00:00 is 00:30");
        let b = s.next_after(a);
        assert_eq!(b, at(3600), "then 01:00");
        let c = s.next_after(b);
        assert_eq!(c, at(3600 + 30 * 60), "then 01:30");
    }

    #[test]
    fn dash_ranges_fire_only_within_the_hour_range() {
        // "0 9-17 * * *" — top of hour, 09:00 through 17:00 inclusive.
        let s = CronSchedule::parse("0 9-17 * * *").unwrap();
        let nine = 9 * 3600;
        // Just before 09:00 → 09:00.
        assert_eq!(s.next_after(at(nine - 60)), at(nine));
        // 17:00 is the last; after it the next is 09:00 the following day.
        let seventeen = 17 * 3600;
        assert_eq!(
            s.next_after(at(seventeen)),
            at(86400 + nine),
            "after 17:00 jumps to next day 09:00"
        );
        // 18:xx never fires that day.
        assert_eq!(s.next_after(at(18 * 3600)), at(86400 + nine));
    }

    #[test]
    fn step_over_an_explicit_range() {
        // "0-30/10 * * * *" → :00, :10, :20, :30 each hour.
        let s = CronSchedule::parse("0-30/10 * * * *").unwrap();
        let mut t = at(0);
        // From 00:00, strictly-after gives :10, :20, :30, then next hour :00.
        for expect in [10, 20, 30, 60, 70] {
            t = s.next_after(t);
            assert_eq!(
                t,
                at(expect * 60),
                "tick at :{:02} of the step series",
                expect % 60
            );
        }
    }

    #[test]
    fn midnight_on_the_first_crosses_month_and_year_boundaries() {
        // "0 0 1 * *" — 00:00 on the 1st of each month.
        let s = CronSchedule::parse("0 0 1 * *").unwrap();
        // 1970-01-01 00:00:00 is epoch 0. next strictly-after is 1970-02-01.
        // Feb 1 1970 = 31 days after Jan 1 = 31 * 86400.
        assert_eq!(s.next_after(at(0)), at(31 * 86400), "Jan 1 → Feb 1");
        // Dec 1 1970 → Jan 1 1971 (year rollover).
        // Days Jan1..Dec1 1970: 31+28+31+30+31+30+31+31+30+31+30 = 334.
        let dec1_1970 = 334 * 86400;
        // Jan 1 1971 = 365 days after Jan 1 1970.
        assert_eq!(
            s.next_after(at(dec1_1970)),
            at(365 * 86400),
            "Dec 1 1970 → Jan 1 1971"
        );
    }

    #[test]
    fn day_of_week_monday_only() {
        // "0 0 * * 1" — midnight on Mondays. 1970-01-05 was a Monday.
        let s = CronSchedule::parse("0 0 * * 1").unwrap();
        let mon_jan5 = 4 * 86400; // 1970-01-05 00:00 UTC
        // From epoch (Thursday Jan 1), next Monday midnight is Jan 5.
        assert_eq!(s.next_after(at(0)), at(mon_jan5));
        // Strictly after that Monday → the following Monday (Jan 12).
        assert_eq!(s.next_after(at(mon_jan5)), at(mon_jan5 + 7 * 86400));
    }

    #[test]
    fn dom_and_dow_both_restricted_is_an_or() {
        // "0 0 13 * 5" — midnight on the 13th OR any Friday (standard cron OR).
        let s = CronSchedule::parse("0 0 13 * 5").unwrap();
        // 1970-01-02 was a Friday → fires (Friday branch), epoch 86400.
        let fri_jan2 = 86400;
        assert_eq!(
            s.next_after(at(0)),
            at(fri_jan2),
            "Friday Jan 2 matches via dow"
        );
        // 1970-01-13 (a Tuesday) → fires via the dom branch even though not Friday.
        // From Jan 9 (Fri) strictly-after should reach Jan 13 (the 13th) before
        // the next Friday (Jan 16): assert the 13th is hit.
        let jan12 = 11 * 86400; // 1970-01-12 00:00 (Monday)
        let jan13 = 12 * 86400; // 1970-01-13 00:00 (Tuesday, the 13th)
        assert_eq!(
            s.next_after(at(jan12)),
            at(jan13),
            "the 13th matches via dom though it is a Tuesday"
        );
    }

    #[test]
    fn leap_day_fires_only_in_leap_years() {
        // "0 0 29 2 *" — Feb 29, which exists only in leap years.
        let s = CronSchedule::parse("0 0 29 2 *").unwrap();
        // First Feb 29 at/after epoch is 1972-02-29 (1972 is a leap year).
        // Days from 1970-01-01 to 1972-02-29:
        //   1970 full year = 365, 1971 full year = 365 → 730 to 1972-01-01.
        //   Jan 1972 = 31 days → 761 to 1972-02-01. +28 → 789 to 1972-02-29.
        let feb29_1972 = 789 * 86400;
        assert_eq!(s.next_after(at(0)), at(feb29_1972));
        // Strictly after 1972-02-29 → the next leap Feb 29 is 1976-02-29, NOT
        // 1973/74/75. Just assert it lands on a Feb-29 instant 4 years later-ish
        // by decoding it back.
        let next = s.next_after(at(feb29_1972));
        let (y, mo, d, h, mi) = decompose_for_test(next);
        assert_eq!((mo, d, h, mi), (2, 29, 0, 0), "still a Feb 29 midnight");
        assert_eq!(y, 1976, "skips the three non-leap years to 1976");
    }

    #[test]
    fn next_after_is_strictly_after_an_exact_fire_minute() {
        // On an exact matching minute, the NEXT match is returned, not `after`.
        let s = CronSchedule::parse("0 * * * *").unwrap();
        // at(3600) is exactly 01:00:00, a fire minute. next must be 02:00.
        assert_eq!(
            s.next_after(at(3600)),
            at(7200),
            "exact fire minute returns the next, not itself"
        );
        // A sub-minute offset into a fire minute still returns the next hour.
        assert_eq!(s.next_after(at(3600 + 30)), at(7200));
    }

    #[test]
    fn whitespace_is_tolerated_between_fields() {
        // Multiple spaces collapse — fields are whitespace-separated.
        let s = CronSchedule::parse("0   *  * * *").unwrap();
        assert_eq!(s.next_after(at(90)), at(3600));
    }

    #[test]
    fn out_of_range_in_every_field_is_rejected() {
        assert!(CronSchedule::parse("* 24 * * *").is_err(), "hour 24 (0-23)");
        assert!(CronSchedule::parse("* * 0 * *").is_err(), "dom 0 (1-31)");
        assert!(CronSchedule::parse("* * 32 * *").is_err(), "dom 32 (1-31)");
        assert!(CronSchedule::parse("* * * 0 *").is_err(), "month 0 (1-12)");
        assert!(
            CronSchedule::parse("* * * 13 *").is_err(),
            "month 13 (1-12)"
        );
        assert!(CronSchedule::parse("* * * * 7").is_err(), "dow 7 (0-6)");
    }

    #[test]
    fn malformed_specs_are_rejected() {
        assert!(
            CronSchedule::parse("1- * * * *").is_err(),
            "open-ended range"
        );
        assert!(CronSchedule::parse("-5 * * * *").is_err(), "leading dash");
        assert!(
            CronSchedule::parse("5-3 * * * *").is_err(),
            "descending range"
        );
        assert!(
            CronSchedule::parse("1,,2 * * * *").is_err(),
            "empty list element"
        );
        assert!(CronSchedule::parse("*/ * * * *").is_err(), "missing step");
        assert!(CronSchedule::parse("1/2/3 * * * *").is_err(), "double step");
        assert!(
            CronSchedule::parse("1.5 * * * *").is_err(),
            "non-[0-9*,/-] char"
        );
        assert!(
            CronSchedule::parse("99999999999 * * * *").is_err(),
            "overflowing number"
        );
    }

    #[test]
    fn impossible_schedule_never_fires_within_the_bound() {
        // Feb 30 never exists; next_after returns the far-future never sentinel.
        let s = CronSchedule::parse("0 0 30 2 *").unwrap();
        assert_eq!(s.next_after(at(0)), never(), "Feb 30 never fires");
    }

    #[test]
    fn due_fire_first_run_returns_the_most_recent_tick_without_walking_from_epoch() {
        // last_fired=None must NOT walk ~30M ticks from 1970; it returns the
        // most recent tick <= now via the bounded backward scan.
        let s = CronSchedule::parse("0 * * * *").unwrap();
        let now = at(1_780_000_000); // a 2026-ish instant with leftover seconds
        let fire = due_fire(&s, None, now).unwrap();
        // The fire is at-or-before now and is itself the most-recent real tick.
        assert!(fire <= now);
        assert_eq!(s.prev_at_or_before(now), Some(fire));
        // And it is a genuine schedule tick: the next match strictly after one
        // second before it lands exactly on it.
        assert_eq!(s.next_after(fire - Duration::from_secs(1)), fire);
    }

    #[test]
    fn prev_at_or_before_is_the_mirror_of_next_after() {
        let s = CronSchedule::parse("*/15 * * * *").unwrap();
        // Most recent quarter-hour at/before 12:07 is 12:00.
        assert_eq!(
            s.prev_at_or_before(at(12 * 3600 + 7 * 60)),
            Some(at(12 * 3600))
        );
        // At exactly 12:15 the at-or-before is 12:15 itself (inclusive).
        assert_eq!(
            s.prev_at_or_before(at(12 * 3600 + 15 * 60)),
            Some(at(12 * 3600 + 15 * 60))
        );
    }

    #[test]
    fn prev_at_or_before_impossible_schedule_is_none() {
        // Feb 30 never exists, so no tick is found within the backward bound.
        let s = CronSchedule::parse("0 0 30 2 *").unwrap();
        assert_eq!(s.prev_at_or_before(at(2_000_000_000)), None);
    }

    #[test]
    fn prev_at_or_before_before_first_tick_is_none() {
        // A schedule whose first-ever tick is after `at` has no prior tick;
        // the scan stops at minute 0 without underflowing below the epoch.
        let s = CronSchedule::parse("0 0 29 2 *").unwrap(); // Feb 29, first is 1972
        // 1971-06-01 is before the first leap day; nothing at-or-before it.
        assert_eq!(s.prev_at_or_before(at(517 * 86400)), None);
    }

    /// Test-only re-decompose so leap-year assertions can read back the result.
    fn decompose_for_test(t: SystemTime) -> (i64, u32, u32, u32, u32) {
        let secs = t.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs() as i64;
        let c = Civil::from_unix_secs(secs);
        (c.year, c.month, c.day, c.hour, c.minute)
    }
}
