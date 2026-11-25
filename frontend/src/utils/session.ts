// Trading-session schedule, mirroring programs/perps-core/src/session.rs.
// The browser's time-zone data supplies the US Eastern offset; the rest of the
// logic matches the program, and both are tested against sessionVectors.json.

export const SCHEDULE_ALWAYS_OPEN = 0;
export const SCHEDULE_CME_GLOBEX = 1;
/**
 * TEST ONLY (docs/localnet.md): a CME-like week compressed into one hour of
 * Unix time. Within each hour `c = ts mod 3600` the market is open for
 * [0, 3000) except 60 s daily breaks at 750, 1500 and 2250, and closed for
 * the 600 s weekend [3000, 3600). Only a `localnet` build of perps-core
 * accepts it.
 */
export const SCHEDULE_TEST_HOURLY = 2;

const HOUR = 3_600;
const DAY = 86_400;
const SESSION_CLOSE = 17 * HOUR; // 17:00 ET
const SESSION_OPEN = 18 * HOUR; // 18:00 ET

const easternFormat = new Intl.DateTimeFormat('en-US', {
  timeZone: 'America/New_York',
  year: 'numeric',
  month: 'numeric',
  day: 'numeric',
  hour: 'numeric',
  minute: 'numeric',
  second: 'numeric',
  hourCycle: 'h23',
});

/** US Eastern UTC offset at `ts` (Unix seconds), in seconds. */
export function easternOffset(ts: number): number {
  const parts: Record<string, number> = {};
  for (const p of easternFormat.formatToParts(new Date(ts * 1000))) {
    if (p.type !== 'literal') parts[p.type] = Number(p.value);
  }
  const wallClock = Date.UTC(parts.year, parts.month - 1, parts.day, parts.hour, parts.minute, parts.second) / 1000;
  return wallClock - ts;
}

const mod = (a: number, n: number) => ((a % n) + n) % n;
/** 0 = Sunday .. 6 = Saturday, for a day number since 1970-01-01 (a Thursday). */
const weekday = (days: number) => mod(days + 4, 7);

function cmeIsOpen(ts: number): boolean {
  const local = ts + easternOffset(ts);
  const secs = mod(local, DAY);
  switch (weekday(Math.floor(local / DAY))) {
    case 6:
      return false;
    case 0:
      return secs >= SESSION_OPEN;
    case 5:
      return secs < SESSION_CLOSE;
    default:
      return !(secs >= SESSION_CLOSE && secs < SESSION_OPEN);
  }
}

function cmeNextWeeklyClose(ts: number): number {
  const local = ts + easternOffset(ts);
  const day = Math.floor(local / DAY);
  let closeLocal = (day + mod(5 - weekday(day), 7)) * DAY + SESSION_CLOSE;
  if (closeLocal <= local) closeLocal += 7 * DAY;
  const est = closeLocal + 5 * HOUR;
  return easternOffset(est) === -5 * HOUR ? est : closeLocal + 4 * HOUR;
}

// SCHEDULE_TEST_HOURLY, as in session.rs
export const TEST_WEEK_SECS = 3_600;
export const TEST_WEEKLY_CLOSE = 3_000;
export const TEST_BREAK_SECS = 60;
export const TEST_BREAKS = [750, 1_500, 2_250] as const;
/** Open seconds per test week: 3000 − 3 × 60. */
export const TEST_WEEK_OPEN = 2_820;
/** Open segments of a test week: [start, end) offsets within the hour. */
const TEST_SEGMENTS: [number, number][] = [
  [0, 750],
  [810, 1_500],
  [1_560, 2_250],
  [2_310, 3_000],
];

function testIsOpen(ts: number): boolean {
  const c = mod(ts, TEST_WEEK_SECS);
  return c < TEST_WEEKLY_CLOSE && !TEST_BREAKS.some((b) => c >= b && c < b + TEST_BREAK_SECS);
}

/** Cumulative test-schedule open seconds up to `ts` (from Unix 0). */
function testOpenSecondsUntil(ts: number): number {
  const c = mod(ts, TEST_WEEK_SECS);
  let open = Math.min(c, TEST_WEEKLY_CLOSE);
  for (const b of TEST_BREAKS) open -= Math.min(Math.max(c - b, 0), TEST_BREAK_SECS);
  return Math.floor(ts / TEST_WEEK_SECS) * TEST_WEEK_OPEN + open;
}

export function isOpen(schedule: number, ts: number): boolean {
  if (schedule === SCHEDULE_CME_GLOBEX) return cmeIsOpen(ts);
  if (schedule === SCHEDULE_TEST_HOURLY) return testIsOpen(ts);
  return true;
}

/** Seconds until the weekly close, or null when closed or never closing. */
export function secondsUntilWeeklyClose(schedule: number, ts: number): number | null {
  if (schedule === SCHEDULE_TEST_HOURLY) return testIsOpen(ts) ? TEST_WEEKLY_CLOSE - mod(ts, TEST_WEEK_SECS) : null;
  if (schedule !== SCHEDULE_CME_GLOBEX || !cmeIsOpen(ts)) return null;
  return cmeNextWeeklyClose(ts) - ts;
}

/** Whether `ts` falls in the `window` seconds before the weekly close. */
export function inPreCloseWindow(schedule: number, ts: number, window: number): boolean {
  if (window <= 0) return false;
  const left = secondsUntilWeeklyClose(schedule, ts);
  return left !== null && left <= window;
}

// Open-time accounting (docs/lp-pool.md §8), as in the program's session.rs.

/** Open time per CME week: Sun 6 h + Mon–Thu 4 × 23 h + Fri 17 h = 115 h. */
export const WEEK_OPEN = 414_000;
const WEEKDAY_OPEN = DAY - HOUR;
const SUNDAY_OPEN = DAY - SESSION_OPEN; // 21_600

/** Cumulative CME open seconds up to `ts`, on the US-Eastern local week grid. */
function cmeOpenSecondsUntil(ts: number): number {
  const local = ts + easternOffset(ts);
  const day = Math.floor(local / DAY);
  const s = mod(local, DAY);
  const wd = weekday(day);
  let inWeek: number;
  if (wd === 0) {
    inWeek = Math.max(0, s - SESSION_OPEN);
  } else if (wd <= 4) {
    const today = s < SESSION_CLOSE ? s : s < SESSION_OPEN ? SESSION_CLOSE : s - HOUR;
    inWeek = SUNDAY_OPEN + (wd - 1) * WEEKDAY_OPEN + today;
  } else if (wd === 5) {
    inWeek = SUNDAY_OPEN + 4 * WEEKDAY_OPEN + Math.min(s, SESSION_CLOSE);
  } else {
    inWeek = WEEK_OPEN;
  }
  return Math.floor((day - wd) / 7) * WEEK_OPEN + inWeek;
}

/** Open seconds in `[a, b)`; `0` when `b <= a`. */
export function openSecondsBetween(schedule: number, a: number, b: number): number {
  if (b <= a) return 0;
  if (schedule === SCHEDULE_TEST_HOURLY) return testOpenSecondsUntil(b) - testOpenSecondsUntil(a);
  return schedule === SCHEDULE_CME_GLOBEX ? cmeOpenSecondsUntil(b) - cmeOpenSecondsUntil(a) : b - a;
}

/**
 * Seconds since the current session opened (Sunday 18:00 or the daily 18:00
 * reopen; on the test schedule the week start or a break's end), or null if
 * the market is closed or never closes.
 */
export function secondsSinceOpen(schedule: number, ts: number): number | null {
  if (schedule === SCHEDULE_TEST_HOURLY) {
    if (!testIsOpen(ts)) return null;
    const c = mod(ts, TEST_WEEK_SECS);
    const seg = TEST_SEGMENTS.find(([a, b]) => c >= a && c < b)!;
    return c - seg[0];
  }
  if (schedule !== SCHEDULE_CME_GLOBEX || !cmeIsOpen(ts)) return null;
  const local = ts + easternOffset(ts);
  const s = mod(local, DAY);
  if (weekday(Math.floor(local / DAY)) === 0) return s - SESSION_OPEN;
  return s < SESSION_CLOSE ? s + (DAY - SESSION_OPEN) : s - SESSION_OPEN;
}

/**
 * Whether funding is current for LP flows: settled, or fewer than `interval`
 * open seconds (including the carried remainder) since the last crank.
 */
export function fundingCurrent(
  schedule: number,
  settled: boolean,
  lastFundingTime: number,
  carrySecs: number,
  interval: number,
  now: number
): boolean {
  return settled || openSecondsBetween(schedule, lastFundingTime, now) + carrySecs < interval;
}

/** Unix time of a US-Eastern local time that is not in a DST transition. */
function fromEastern(local: number): number {
  const est = local + 5 * HOUR;
  return easternOffset(est) === -5 * HOUR ? est : local + 4 * HOUR;
}

/**
 * The first in-session time `t >= from` with `openSecondsBetween(from, t) >= secs`
 * (`secs <= 0`: the first in-session time at or after `from`). On a CME
 * market this skips the daily breaks and weekends: when the count completes
 * exactly at a close, the answer is the next open, the first moment
 * anything can act on it.
 */
export function timeAfterOpenSeconds(schedule: number, from: number, secs: number): number {
  const need = Math.max(0, secs);
  if (schedule === SCHEDULE_TEST_HOURLY) return testTimeAfterOpenSeconds(from, need);
  if (schedule !== SCHEDULE_CME_GLOBEX) return from + need;
  const target = cmeOpenSecondsUntil(from) + need;
  const week = Math.floor(target / WEEK_OPEN);
  let within = target - week * WEEK_OPEN;
  // Local Sunday of that week: Sunday day numbers are 3 mod 7 (1970-01-04)
  const sunday = week * 7 + 3;
  let local: number;
  if (within < SUNDAY_OPEN) {
    local = sunday * DAY + SESSION_OPEN + within;
  } else {
    within -= SUNDAY_OPEN;
    const k = Math.min(Math.floor(within / WEEKDAY_OPEN), 4); // 0..3 = Mon..Thu, 4 = Fri
    const r = within - k * WEEKDAY_OPEN;
    // r == SESSION_CLOSE completes at the daily break: the next open is 18:00
    const s = r < SESSION_CLOSE ? r : r + HOUR;
    local = (sunday + 1 + k) * DAY + s;
  }
  const t = fromEastern(local);
  return Math.max(t, from);
}

/** `timeAfterOpenSeconds` on the test schedule. */
function testTimeAfterOpenSeconds(from: number, need: number): number {
  const target = testOpenSecondsUntil(from) + need;
  const week = Math.floor(target / TEST_WEEK_OPEN);
  let within = target - week * TEST_WEEK_OPEN;
  let local = 0;
  for (const [a, b] of TEST_SEGMENTS) {
    // Completing exactly at a segment's end: the next segment's open
    if (within < b - a) {
      local = a + within;
      break;
    }
    within -= b - a;
  }
  return Math.max(week * TEST_WEEK_SECS + local, from);
}

/**
 * When funding can next be cranked: the first in-session time at which open
 * seconds since `lastFundingTime`, plus the carried remainder, reach the
 * interval. Wall clock `last + interval` would show funding overdue every
 * weekend and daily break.
 */
export function nextFundingTime(schedule: number, lastFundingTime: number, carrySecs: number, interval: number): number {
  return timeAfterOpenSeconds(schedule, lastFundingTime, interval - carrySecs);
}

export type SessionState = 'open' | 'pre-close' | 'closed' | 'settled';

export interface MarketSessionParams {
  sessionSchedule: number;
  preCloseWindowSecs: number;
  settlementPrice: number;
}

/** How far ahead to warn that weekend margins are coming. */
export const WEEKEND_MARGIN_NOTICE_SECS = 24 * HOUR;

export interface MarketSession {
  state: SessionState;
  /** Seconds until the weekly close while open, else null. */
  secondsUntilClose: number | null;
  /** Seconds until weekend margins start, while open and before the window, else null. */
  secondsUntilWeekendMargins?: number | null;
  /** Human-readable reopening time while closed, else null. */
  reopens: string | null;
}

/** Where the market stands at `ts`, for display and client-side checks. */
export function marketSession(market: MarketSessionParams | null | undefined, ts: number): MarketSession {
  if (!market) return { state: 'open', secondsUntilClose: null, reopens: null };
  if (market.settlementPrice > 0) return { state: 'settled', secondsUntilClose: null, reopens: null };

  if (market.sessionSchedule === SCHEDULE_TEST_HOURLY && !isOpen(market.sessionSchedule, ts)) {
    const c = mod(ts, TEST_WEEK_SECS);
    const left = timeAfterOpenSeconds(SCHEDULE_TEST_HOURLY, ts, 0) - ts;
    const kind = c >= TEST_WEEKLY_CLOSE ? 'test weekend' : 'test break';
    return { state: 'closed', secondsUntilClose: null, reopens: `in ${Math.floor(left / 60)}m ${left % 60}s (${kind})` };
  }

  if (!isOpen(market.sessionSchedule, ts)) {
    const local = ts + easternOffset(ts);
    const wd = weekday(Math.floor(local / DAY));
    const weekend = wd === 6 || wd === 0 || (wd === 5 && mod(local, DAY) >= SESSION_CLOSE);
    return { state: 'closed', secondsUntilClose: null, reopens: weekend ? 'Sunday 6:00 PM ET' : '6:00 PM ET' };
  }

  const left = secondsUntilWeeklyClose(market.sessionSchedule, ts);
  const windowSecs = market.preCloseWindowSecs;
  const preClose = windowSecs > 0 && left !== null && left <= windowSecs;
  return {
    state: preClose ? 'pre-close' : 'open',
    secondsUntilClose: left,
    secondsUntilWeekendMargins: !preClose && windowSecs > 0 && left !== null ? left - windowSecs : null,
    reopens: null,
  };
}

export function formatDuration(seconds: number): string {
  const h = Math.floor(seconds / HOUR);
  const m = Math.floor((seconds % HOUR) / 60);
  return h > 0 ? `${h}h ${m}m` : `${m}m`;
}
