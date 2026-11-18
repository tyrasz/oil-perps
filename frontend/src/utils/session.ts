// Trading-session schedule, mirroring programs/perps-core/src/session.rs.
// The browser's time-zone data supplies the US Eastern offset; the rest of the
// logic matches the program, and both are tested against sessionVectors.json.

export const SCHEDULE_ALWAYS_OPEN = 0;
export const SCHEDULE_CME_GLOBEX = 1;

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

export function isOpen(schedule: number, ts: number): boolean {
  return schedule === SCHEDULE_CME_GLOBEX ? cmeIsOpen(ts) : true;
}

/** Seconds until the weekly close, or null when closed or never closing. */
export function secondsUntilWeeklyClose(schedule: number, ts: number): number | null {
  if (schedule !== SCHEDULE_CME_GLOBEX || !cmeIsOpen(ts)) return null;
  return cmeNextWeeklyClose(ts) - ts;
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
