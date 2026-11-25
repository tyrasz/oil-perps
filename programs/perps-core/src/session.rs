//! Trading-session schedule for markets whose oracle follows an exchange's
//! hours. All times are Unix seconds (UTC); the CME schedule is defined in
//! US Eastern time, so daylight saving is computed here.
//!
//! Exchange holidays are not modelled. On a holiday the Pyth feed stops
//! updating, so price-dependent instructions fail the staleness check, and the
//! authority can pause the market for longer closures.

/// Market is open around the clock (crypto-style). Default for existing markets.
pub const SCHEDULE_ALWAYS_OPEN: u8 = 0;
/// CME Globex hours used by WTI, gold, silver, copper and natural gas futures:
/// Sunday 18:00 to Friday 17:00 ET, with a daily break from 17:00 to 18:00 ET.
pub const SCHEDULE_CME_GLOBEX: u8 = 1;
/// TEST ONLY (docs/localnet.md): a CME-like week compressed into one hour of
/// Unix time, so a live localnet run of a few hours crosses several weekly
/// closes and reopens. Each "week" is `ts.div_euclid(3_600)`; within it
/// (`c = ts.rem_euclid(3_600)`) the market is open for `[0, 3_000)` except
/// three 60 s "daily breaks" at `[750, 810)`, `[1_500, 1_560)` and
/// `[2_250, 2_310)`, and closed for the 600 s "weekend" `[3_000, 3_600)`:
/// four open "days" (750 + 3 × 690 s) and 2_820 open seconds a week.
///
/// The session functions below always know it, so the keeper, the sim and
/// the frontend mirrors compile one schedule set, but a market can only be
/// set to it by a perps-core built with the `localnet` cargo feature
/// (`is_valid_schedule`), which no production build enables.
pub const SCHEDULE_TEST_HOURLY: u8 = 2;

const HOUR: i64 = 3_600;
const DAY: i64 = 86_400;
const SESSION_CLOSE: i64 = 17 * HOUR; // 17:00 ET
const SESSION_OPEN: i64 = 18 * HOUR; // 18:00 ET

/// Length of a `SCHEDULE_TEST_HOURLY` week (wall seconds).
pub const TEST_WEEK_SECS: i64 = 3_600;
/// Offset of the weekly close within a test week; closed from here to the end.
pub const TEST_WEEKLY_CLOSE: i64 = 3_000;
/// Length of each test "daily break".
pub const TEST_BREAK_SECS: i64 = 60;
/// Offsets of the test daily breaks within a week.
pub const TEST_BREAKS: [i64; 3] = [750, 1_500, 2_250];
/// Open seconds per test week: 3_000 − 3 × 60.
pub const TEST_WEEK_OPEN: i64 = 2_820;

/// Schedules `update_market_config` accepts (markets start always open). The test
/// schedule only in a `localnet` build (never a production deploy, D46).
pub fn is_valid_schedule(schedule: u8) -> bool {
    schedule == SCHEDULE_ALWAYS_OPEN
        || schedule == SCHEDULE_CME_GLOBEX
        || (cfg!(feature = "localnet") && schedule == SCHEDULE_TEST_HOURLY)
}

/// Whether the market is in session at `ts`.
pub fn is_open(schedule: u8, ts: i64) -> bool {
    match schedule {
        SCHEDULE_CME_GLOBEX => cme_is_open(ts),
        SCHEDULE_TEST_HOURLY => test_is_open(ts),
        _ => true,
    }
}

/// Seconds until the market's weekly (weekend) close, or `None` if the market
/// is currently closed or never closes. Daily breaks are not counted.
pub fn seconds_until_weekly_close(schedule: u8, ts: i64) -> Option<i64> {
    match schedule {
        SCHEDULE_CME_GLOBEX if cme_is_open(ts) => Some(cme_next_weekly_close(ts) - ts),
        SCHEDULE_TEST_HOURLY if test_is_open(ts) => Some(TEST_WEEKLY_CLOSE - ts.rem_euclid(TEST_WEEK_SECS)),
        _ => None,
    }
}

/// Whether `ts` falls in the `window` seconds before the weekly close.
#[inline(never)]
pub fn in_pre_close_window(schedule: u8, ts: i64, window: u32) -> bool {
    window > 0
        && seconds_until_weekly_close(schedule, ts).map_or(false, |left| left <= window as i64)
}

/// Open seconds in `[a, b)` (docs/lp-pool.md §8). O(1) and exact across DST
/// changes, which happen on Sunday 02:00 local while the market is closed.
/// `0` when `b <= a`.
#[inline(never)]
pub fn open_seconds_between(schedule: u8, a: i64, b: i64) -> i64 {
    if b <= a {
        return 0;
    }
    match schedule {
        SCHEDULE_CME_GLOBEX => cme_open_seconds_until(b) - cme_open_seconds_until(a),
        SCHEDULE_TEST_HOURLY => test_open_seconds_until(b) - test_open_seconds_until(a),
        _ => b - a,
    }
}

/// Seconds since the current session opened (Sunday 18:00 or the daily
/// 18:00 reopen; on the test schedule the week start or the end of a daily
/// break), or `None` if the market is closed or never closes.
#[inline(never)]
pub fn seconds_since_open(schedule: u8, ts: i64) -> Option<i64> {
    match schedule {
        SCHEDULE_CME_GLOBEX if cme_is_open(ts) => {
            let local = ts + eastern_offset(ts);
            let s = local.rem_euclid(DAY);
            Some(match weekday(local.div_euclid(DAY)) {
                0 => s - SESSION_OPEN,
                _ if s < SESSION_CLOSE => s + (DAY - SESSION_OPEN),
                _ => s - SESSION_OPEN,
            })
        }
        SCHEDULE_TEST_HOURLY if test_is_open(ts) => {
            let c = ts.rem_euclid(TEST_WEEK_SECS);
            let start = TEST_BREAKS.iter().map(|b| b + TEST_BREAK_SECS).filter(|&o| o <= c).max().unwrap_or(0);
            Some(c - start)
        }
        _ => None,
    }
}

/// Whether funding is current for LP flows (§8): settled, or fewer than
/// `interval` open seconds (including the carried remainder) since the last
/// crank.
pub fn funding_current(schedule: u8, settled: bool, last_funding_time: i64, carry_secs: u32, interval: i64, now: i64) -> bool {
    settled || open_seconds_between(schedule, last_funding_time, now) + (carry_secs as i64) < interval
}

/// Open time per CME week: Sun 6 h + Mon–Thu 4 × 23 h + Fri 17 h = 115 h.
const WEEK_OPEN: i64 = 414_000;
/// Open seconds per Mon–Thu (23 h).
const WEEKDAY_OPEN: i64 = DAY - HOUR;

/// Cumulative CME open seconds up to `ts`, on the US-Eastern local week grid.
fn cme_open_seconds_until(ts: i64) -> i64 {
    let local = ts + eastern_offset(ts);
    let day = local.div_euclid(DAY);
    let s = local.rem_euclid(DAY);
    let wd = weekday(day);
    let sunday = DAY - SESSION_OPEN; // 21_600
    let in_week = match wd {
        0 => (s - SESSION_OPEN).max(0),
        1..=4 => {
            let today = if s < SESSION_CLOSE {
                s
            } else if s < SESSION_OPEN {
                SESSION_CLOSE
            } else {
                s - HOUR
            };
            sunday + (wd - 1) * WEEKDAY_OPEN + today
        }
        5 => sunday + 4 * WEEKDAY_OPEN + s.min(SESSION_CLOSE),
        _ => WEEK_OPEN,
    };
    (day - wd).div_euclid(7) * WEEK_OPEN + in_week
}

fn test_is_open(ts: i64) -> bool {
    let c = ts.rem_euclid(TEST_WEEK_SECS);
    c < TEST_WEEKLY_CLOSE && !TEST_BREAKS.iter().any(|&b| c >= b && c < b + TEST_BREAK_SECS)
}

/// Cumulative test-schedule open seconds up to `ts` (from Unix 0).
fn test_open_seconds_until(ts: i64) -> i64 {
    let c = ts.rem_euclid(TEST_WEEK_SECS);
    let mut open = c.min(TEST_WEEKLY_CLOSE);
    for b in TEST_BREAKS {
        open -= (c - b).clamp(0, TEST_BREAK_SECS);
    }
    ts.div_euclid(TEST_WEEK_SECS) * TEST_WEEK_OPEN + open
}

fn cme_is_open(ts: i64) -> bool {
    let local = ts + eastern_offset(ts);
    let secs = local.rem_euclid(DAY);
    match weekday(local.div_euclid(DAY)) {
        6 => false,                  // Saturday
        0 => secs >= SESSION_OPEN,   // Sunday evening open
        5 => secs < SESSION_CLOSE,   // Friday close
        _ => !(SESSION_CLOSE..SESSION_OPEN).contains(&secs), // daily break
    }
}

/// Unix time of the next Friday 17:00 ET strictly after `ts`.
fn cme_next_weekly_close(ts: i64) -> i64 {
    let local = ts + eastern_offset(ts);
    let day = local.div_euclid(DAY);
    let mut close_local = (day + (5 - weekday(day)).rem_euclid(7)) * DAY + SESSION_CLOSE;
    if close_local <= local {
        close_local += 7 * DAY;
    }
    // DST never changes on a Friday, so the offset at the close is unambiguous.
    let est = close_local + 5 * HOUR;
    if eastern_offset(est) == -5 * HOUR {
        est
    } else {
        close_local + 4 * HOUR
    }
}

/// UTC offset of US Eastern time at `ts`, in seconds (-4h in DST, -5h otherwise).
/// DST runs from the second Sunday of March 02:00 EST to the first Sunday of
/// November 02:00 EDT.
pub fn eastern_offset(ts: i64) -> i64 {
    let (year, _, _) = civil_from_days(ts.div_euclid(DAY));
    let dst_start = nth_sunday(year, 3, 2) * DAY + 7 * HOUR; // 02:00 EST
    let dst_end = nth_sunday(year, 11, 1) * DAY + 6 * HOUR; // 02:00 EDT
    if (dst_start..dst_end).contains(&ts) {
        -4 * HOUR
    } else {
        -5 * HOUR
    }
}

/// Day number (days since 1970-01-01) of the `n`th Sunday of `month`.
fn nth_sunday(year: i64, month: u32, n: i64) -> i64 {
    let first = days_from_civil(year, month, 1);
    first + (7 - weekday(first)) % 7 + 7 * (n - 1)
}

/// 0 = Sunday .. 6 = Saturday. 1970-01-01 was a Thursday.
fn weekday(days: i64) -> i64 {
    (days + 4).rem_euclid(7)
}

// Gregorian calendar conversions (Howard Hinnant's algorithms).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = month as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + if month <= 2 { 1 } else { 0 };
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    include!("session_vectors.rs");

    #[test]
    fn matches_reference_schedule() {
        for &(ts, open, until_close) in SESSION_VECTORS {
            assert_eq!(is_open(SCHEDULE_CME_GLOBEX, ts), open, "is_open at {}", ts);
            let expected = if until_close >= 0 { Some(until_close) } else { None };
            assert_eq!(
                seconds_until_weekly_close(SCHEDULE_CME_GLOBEX, ts),
                expected,
                "seconds_until_weekly_close at {}",
                ts
            );
        }
    }

    #[test]
    fn eastern_offset_matches_reference() {
        for &(ts, offset) in OFFSET_VECTORS {
            assert_eq!(eastern_offset(ts), offset, "offset at {}", ts);
        }
    }

    #[test]
    fn always_open_schedule_never_closes() {
        for &(ts, _, _) in SESSION_VECTORS {
            assert!(is_open(SCHEDULE_ALWAYS_OPEN, ts));
            assert!(!in_pre_close_window(SCHEDULE_ALWAYS_OPEN, ts, 86_400));
        }
    }

    #[test]
    fn pre_close_window_covers_friday_afternoon_only() {
        // Fri 2026-11-13 17:00 ET = 22:00 UTC
        let close = 1_794_607_200;
        assert!(in_pre_close_window(SCHEDULE_CME_GLOBEX, close - 1, 7_200));
        assert!(in_pre_close_window(SCHEDULE_CME_GLOBEX, close - 7_200, 7_200));
        assert!(!in_pre_close_window(SCHEDULE_CME_GLOBEX, close - 7_201, 7_200));
        assert!(!in_pre_close_window(SCHEDULE_CME_GLOBEX, close, 7_200)); // closed
        assert!(!in_pre_close_window(SCHEDULE_CME_GLOBEX, close - 1, 0)); // disabled
    }

    const YEAR_2026: i64 = 1_767_225_600; // 2026-01-01 00:00 UTC
    const YEAR_2027: i64 = 1_798_761_600;

    /// §17.1 #9: minute-by-minute across 2026 (both DST changes) against
    /// `is_open`. Session edges are on the hour, so minutes are exact.
    #[test]
    fn open_seconds_match_brute_force_across_2026() {
        let mut acc = 0i64;
        let mut t = YEAR_2026;
        while t < YEAR_2027 {
            assert_eq!(open_seconds_between(SCHEDULE_CME_GLOBEX, YEAR_2026, t), acc, "at {}", t);
            if is_open(SCHEDULE_CME_GLOBEX, t) {
                acc += 60;
            }
            t += 60;
        }
        // 52 weeks of 115 h, plus the partial weeks at both ends
        assert_eq!(open_seconds_between(SCHEDULE_CME_GLOBEX, YEAR_2026, YEAR_2027), acc);
        assert_eq!(open_seconds_between(SCHEDULE_ALWAYS_OPEN, YEAR_2026, YEAR_2027), YEAR_2027 - YEAR_2026);
        assert_eq!(open_seconds_between(SCHEDULE_CME_GLOBEX, YEAR_2027, YEAR_2026), 0);
    }

    #[test]
    fn seconds_since_open_matches_brute_force_across_2026() {
        let mut open_at: Option<i64> = None;
        let mut was_open = true; // 2026-01-01 00:00 UTC is in session; skip to the first reopen
        let mut seen = 0;
        let mut t = YEAR_2026;
        while t < YEAR_2027 {
            let open = is_open(SCHEDULE_CME_GLOBEX, t);
            if open && !was_open {
                open_at = Some(t);
            }
            match (open, open_at) {
                (true, Some(o)) => {
                    assert_eq!(seconds_since_open(SCHEDULE_CME_GLOBEX, t), Some(t - o), "at {}", t);
                    assert_eq!(seconds_since_open(SCHEDULE_CME_GLOBEX, t + 37), Some(t + 37 - o));
                    seen += 1;
                }
                (false, _) => assert_eq!(seconds_since_open(SCHEDULE_CME_GLOBEX, t), None),
                _ => {}
            }
            was_open = open;
            t += 60;
        }
        assert!(seen > 350_000);
        assert_eq!(seconds_since_open(SCHEDULE_ALWAYS_OPEN, YEAR_2026), None);
    }

    /// The weekly close found by brute force (open, then closed for over an
    /// hour) matches `seconds_until_weekly_close`, so `in_pre_close_window`
    /// is exact across DST.
    #[test]
    fn pre_close_window_matches_brute_force_across_2026() {
        let mut next_close: Option<i64> = None;
        let mut t = YEAR_2027;
        while t > YEAR_2026 {
            t -= 60;
            let open = is_open(SCHEDULE_CME_GLOBEX, t);
            if open && !is_open(SCHEDULE_CME_GLOBEX, t + 60) && !is_open(SCHEDULE_CME_GLOBEX, t + 60 + HOUR) {
                next_close = Some(t + 60);
            }
            if let (true, Some(c)) = (open, next_close) {
                assert_eq!(seconds_until_weekly_close(SCHEDULE_CME_GLOBEX, t), Some(c - t), "at {}", t);
                let left = c - t;
                assert!(in_pre_close_window(SCHEDULE_CME_GLOBEX, t, left as u32));
                assert!(!in_pre_close_window(SCHEDULE_CME_GLOBEX, t, (left - 1) as u32));
            }
        }
    }

    #[test]
    fn open_seconds_are_additive() {
        let mut x = 0x1234_5678_9abc_def0u64;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for _ in 0..20_000 {
            let span = 4 * (YEAR_2027 - YEAR_2026) as u64;
            let mut v = [0i64; 3];
            for e in v.iter_mut() {
                *e = YEAR_2026 - 365 * DAY + (next() % span) as i64;
            }
            v.sort();
            let [a, c, b] = v;
            let f = |a, b| open_seconds_between(SCHEDULE_CME_GLOBEX, a, b);
            assert_eq!(f(a, c) + f(c, b), f(a, b), "{} {} {}", a, c, b);
            assert!(f(a, b) <= b - a);
        }
    }

    #[test]
    fn open_seconds_and_session_start_agree_with_vectors() {
        for &(ts, open, until_close) in SESSION_VECTORS {
            let since = seconds_since_open(SCHEDULE_CME_GLOBEX, ts);
            assert_eq!(since.is_some(), open, "at {}", ts);
            if let Some(since) = since {
                // The whole time since the open was in session, the second before was not
                assert_eq!(open_seconds_between(SCHEDULE_CME_GLOBEX, ts - since, ts), since);
                assert!(!is_open(SCHEDULE_CME_GLOBEX, ts - since - 1));
                // Up to the weekly close only the daily breaks (≤ 4 × 1 h) are closed
                let open_left = open_seconds_between(SCHEDULE_CME_GLOBEX, ts, ts + until_close);
                let closed = until_close - open_left;
                assert!(closed >= 0 && closed % HOUR == 0 && closed <= 4 * HOUR, "at {}", ts);
            } else {
                assert_eq!(open_seconds_between(SCHEDULE_CME_GLOBEX, ts, ts + 1), 0);
            }
        }
    }

    #[test]
    fn a_week_has_115_open_hours() {
        // Fri 2026-11-13 17:00 ET close; Sun 15 18:00 ET reopen
        let close = 1_794_607_200;
        assert_eq!(open_seconds_between(SCHEDULE_CME_GLOBEX, close, close + 49 * HOUR), 0);
        assert_eq!(open_seconds_between(SCHEDULE_CME_GLOBEX, close - 7 * DAY, close), WEEK_OPEN);
        assert_eq!(open_seconds_between(SCHEDULE_CME_GLOBEX, close - 1, close + 49 * HOUR + 1), 2);
        assert_eq!(seconds_since_open(SCHEDULE_CME_GLOBEX, close + 49 * HOUR), Some(0));
        assert_eq!(seconds_since_open(SCHEDULE_CME_GLOBEX, close - 1), Some(23 * HOUR - 1));
        // Week of the November DST change (Sun 2026-11-01): still 115 h
        assert_eq!(open_seconds_between(SCHEDULE_CME_GLOBEX, close - 14 * DAY, close - 7 * DAY), WEEK_OPEN);
        // Funding is current across a weekend
        assert!(funding_current(SCHEDULE_CME_GLOBEX, false, close - 60, 0, 3_600, close + 49 * HOUR + 3_539));
        assert!(!funding_current(SCHEDULE_CME_GLOBEX, false, close - 60, 0, 3_600, close + 49 * HOUR + 3_540));
        assert!(funding_current(SCHEDULE_CME_GLOBEX, true, 0, 0, 3_600, close));
        assert!(!funding_current(SCHEDULE_ALWAYS_OPEN, false, close - 3_600, 0, 3_600, close));
    }

    // SCHEDULE_TEST_HOURLY (localnet only, docs/localnet.md)

    const TEST: u8 = SCHEDULE_TEST_HOURLY;
    /// A test week start (ts ≡ 0 mod 3_600) in November 2026.
    const TEST_WEEK0: i64 = 1_794_607_200;

    #[test]
    fn test_schedule_matches_reference_vectors() {
        assert!(TEST_SESSION_VECTORS.len() > 400);
        for &(ts, open, until_close, since_open, open_until) in TEST_SESSION_VECTORS {
            assert_eq!(is_open(TEST, ts), open, "is_open at {}", ts);
            let opt = |v: i64| if v >= 0 { Some(v) } else { None };
            assert_eq!(seconds_until_weekly_close(TEST, ts), opt(until_close), "until close at {}", ts);
            assert_eq!(seconds_since_open(TEST, ts), opt(since_open), "since open at {}", ts);
            // Open seconds since Unix 0 (negative before it)
            let since_epoch = open_seconds_between(TEST, 0, ts) - open_seconds_between(TEST, ts, 0);
            assert_eq!(since_epoch, open_until, "open seconds at {}", ts);
        }
    }

    /// Second by second over three test weeks: open seconds, the session
    /// start and the weekly close (open, then closed for over the longest
    /// daily break) all agree with `is_open`.
    #[test]
    fn test_schedule_matches_brute_force() {
        let (a, b) = (TEST_WEEK0 - 1, TEST_WEEK0 + 3 * TEST_WEEK_SECS);
        let mut acc = 0;
        let mut open_at: Option<i64> = None;
        let mut opens = 0;
        for t in a..b {
            assert_eq!(open_seconds_between(TEST, a, t), acc, "at {}", t);
            let open = is_open(TEST, t);
            if open && !is_open(TEST, t - 1) {
                open_at = Some(t);
                opens += 1;
            }
            if open {
                acc += 1;
                assert_eq!(seconds_since_open(TEST, t), Some(t - open_at.unwrap()), "at {}", t);
                let close = (t..).find(|&c| (c..c + TEST_BREAK_SECS + 1).all(|x| !is_open(TEST, x))).unwrap();
                assert_eq!(seconds_until_weekly_close(TEST, t), Some(close - t), "at {}", t);
                assert!(in_pre_close_window(TEST, t, (close - t) as u32));
                assert!(!in_pre_close_window(TEST, t, (close - t - 1) as u32));
            } else {
                assert_eq!(seconds_since_open(TEST, t), None);
                assert_eq!(seconds_until_weekly_close(TEST, t), None);
                assert!(!in_pre_close_window(TEST, t, 3_600));
            }
        }
        // Four session opens a week (the week start and three break ends)
        assert_eq!(opens, 3 * 4);
        assert_eq!(acc, 3 * TEST_WEEK_OPEN);
    }

    #[test]
    fn test_week_shape() {
        let w = TEST_WEEK0;
        assert_eq!(open_seconds_between(TEST, w, w + TEST_WEEK_SECS), TEST_WEEK_OPEN);
        assert_eq!(open_seconds_between(TEST, w - 7 * TEST_WEEK_SECS, w), 7 * TEST_WEEK_OPEN);
        // Days: 750 s, then three of 690 s
        assert!(is_open(TEST, w) && is_open(TEST, w + 749) && !is_open(TEST, w + 750));
        assert!(!is_open(TEST, w + 809) && is_open(TEST, w + 810));
        assert_eq!(seconds_since_open(TEST, w + 810), Some(0));
        assert_eq!(seconds_since_open(TEST, w + 2_999), Some(689));
        assert!(!is_open(TEST, w + 3_000) && !is_open(TEST, w + 3_599) && is_open(TEST, w + 3_600));
        assert_eq!(seconds_until_weekly_close(TEST, w), Some(3_000));
        assert_eq!(seconds_until_weekly_close(TEST, w + 2_999), Some(1));
        // A 300 s pre-close window: [2_700, 3_000), across no break
        assert!(!in_pre_close_window(TEST, w + 2_699, 300));
        assert!(in_pre_close_window(TEST, w + 2_700, 300));
        assert!(in_pre_close_window(TEST, w + 2_999, 300));
        assert!(!in_pre_close_window(TEST, w + 3_000, 300));
        // Funding is current across the weekend: 60 s interval, crank 30 s before the close
        assert!(funding_current(TEST, false, w + 2_970, 0, 60, w + 3_600 + 29));
        assert!(!funding_current(TEST, false, w + 2_970, 0, 60, w + 3_600 + 30));
        assert!(funding_current(TEST, false, w + 2_970, 20, 60, w + 3_600 + 9));
        assert!(!funding_current(TEST, false, w + 2_970, 20, 60, w + 3_600 + 10));
        assert!(funding_current(TEST, true, 0, 0, 60, w + 3_000));
    }

    #[test]
    fn test_open_seconds_are_additive() {
        let mut x = 0x0bad_cafe_dead_beefu64;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for _ in 0..20_000 {
            let mut v = [0i64; 3];
            for e in v.iter_mut() {
                *e = TEST_WEEK0 - 50 * TEST_WEEK_SECS + (next() % (100 * TEST_WEEK_SECS as u64)) as i64;
            }
            v.sort();
            let [a, c, b] = v;
            let f = |a, b| open_seconds_between(TEST, a, b);
            assert_eq!(f(a, c) + f(c, b), f(a, b), "{} {} {}", a, c, b);
            assert!(f(a, b) <= b - a && f(a, b) >= 0);
        }
    }

    /// The test schedule is only accepted by a `localnet` build of perps-core;
    /// the default (production) build rejects it like any unknown schedule.
    #[test]
    fn test_schedule_is_only_valid_in_localnet_builds() {
        assert!(is_valid_schedule(SCHEDULE_ALWAYS_OPEN));
        assert!(is_valid_schedule(SCHEDULE_CME_GLOBEX));
        assert_eq!(is_valid_schedule(SCHEDULE_TEST_HOURLY), cfg!(feature = "localnet"));
        assert!(!is_valid_schedule(3));
        assert!(!is_valid_schedule(u8::MAX));
    }

    #[test]
    fn calendar_round_trips() {
        for days in (-800_000..800_000).step_by(997) {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
    }
}
