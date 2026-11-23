import { describe, it, expect } from 'vitest';
import {
  SCHEDULE_ALWAYS_OPEN,
  SCHEDULE_CME_GLOBEX,
  easternOffset,
  isOpen,
  marketSession,
  secondsUntilWeeklyClose,
  formatDuration,
  fundingCurrent,
  inPreCloseWindow,
  nextFundingTime,
  openSecondsBetween,
  secondsSinceOpen,
  timeAfterOpenSeconds,
  WEEK_OPEN,
} from './session';
import vectors from './sessionVectors.json';

// Fri 2026-11-13 17:00 ET (22:00 UTC) weekly close
const FRIDAY_CLOSE = 1_794_607_200;
const cme = { sessionSchedule: SCHEDULE_CME_GLOBEX, preCloseWindowSecs: 7_200, settlementPrice: 0 };

describe('session schedule', () => {
  it('matches the reference vectors shared with the program', () => {
    for (const [ts, open, untilClose] of vectors.sessions as [number, boolean, number][]) {
      expect(isOpen(SCHEDULE_CME_GLOBEX, ts), `isOpen ${ts}`).toBe(open);
      expect(secondsUntilWeeklyClose(SCHEDULE_CME_GLOBEX, ts), `untilClose ${ts}`).toBe(
        untilClose >= 0 ? untilClose : null
      );
    }
  });

  it('matches reference Eastern offsets', () => {
    for (const [ts, offset] of vectors.offsets as [number, number][]) {
      expect(easternOffset(ts), `offset ${ts}`).toBe(offset);
    }
  });

  it('always-open markets never close', () => {
    expect(isOpen(SCHEDULE_ALWAYS_OPEN, FRIDAY_CLOSE)).toBe(true);
    expect(secondsUntilWeeklyClose(SCHEDULE_ALWAYS_OPEN, FRIDAY_CLOSE - 60)).toBeNull();
  });
});

describe('marketSession', () => {
  it('reports open, pre-close and closed around the weekly close', () => {
    expect(marketSession(cme, FRIDAY_CLOSE - 7_201).state).toBe('open');
    expect(marketSession(cme, FRIDAY_CLOSE - 7_200)).toEqual({
      state: 'pre-close',
      secondsUntilClose: 7_200,
      secondsUntilWeekendMargins: null,
      reopens: null,
    });
    expect(marketSession(cme, FRIDAY_CLOSE)).toEqual({ state: 'closed', secondsUntilClose: null, reopens: 'Sunday 6:00 PM ET' });
  });

  it('counts down to weekend margins before the window opens', () => {
    expect(marketSession(cme, FRIDAY_CLOSE - 7_200 - 3_600).secondsUntilWeekendMargins).toBe(3_600);
    const noWindow = { ...cme, preCloseWindowSecs: 0 };
    expect(marketSession(noWindow, FRIDAY_CLOSE - 60).secondsUntilWeekendMargins).toBeNull();
  });

  it('distinguishes the daily break from the weekend', () => {
    // Mon 2026-11-16 17:30 ET = 22:30 UTC
    expect(marketSession(cme, 1_794_868_200).reopens).toBe('6:00 PM ET');
  });

  it('reports settlement and treats unknown markets as open', () => {
    expect(marketSession({ ...cme, settlementPrice: 75_000_000 }, FRIDAY_CLOSE - 60).state).toBe('settled');
    expect(marketSession(null, FRIDAY_CLOSE).state).toBe('open');
  });

  it('formats countdowns', () => {
    expect(formatDuration(7_200)).toBe('2h 0m');
    expect(formatDuration(59 * 60)).toBe('59m');
  });
});

// Ports of the open-time tests in programs/perps-core/src/session.rs
describe('open seconds (docs/lp-pool.md §8)', () => {
  const HOUR = 3_600;
  const DAY = 86_400;
  const CME = SCHEDULE_CME_GLOBEX;

  it('has 115 open hours a week, DST weeks included', () => {
    const close = FRIDAY_CLOSE;
    expect(openSecondsBetween(CME, close, close + 49 * HOUR)).toBe(0);
    expect(openSecondsBetween(CME, close - 7 * DAY, close)).toBe(WEEK_OPEN);
    expect(openSecondsBetween(CME, close - 1, close + 49 * HOUR + 1)).toBe(2);
    expect(secondsSinceOpen(CME, close + 49 * HOUR)).toBe(0);
    expect(secondsSinceOpen(CME, close - 1)).toBe(23 * HOUR - 1);
    // Week of the November DST change (Sun 2026-11-01)
    expect(openSecondsBetween(CME, close - 14 * DAY, close - 7 * DAY)).toBe(WEEK_OPEN);
    expect(openSecondsBetween(CME, close + 7 * DAY, close)).toBe(0);
    expect(openSecondsBetween(SCHEDULE_ALWAYS_OPEN, close, close + 49 * HOUR)).toBe(49 * HOUR);
    expect(secondsSinceOpen(SCHEDULE_ALWAYS_OPEN, close)).toBeNull();
  });

  it('keeps funding current across a weekend', () => {
    const close = FRIDAY_CLOSE;
    expect(fundingCurrent(CME, false, close - 60, 0, 3_600, close + 49 * HOUR + 3_539)).toBe(true);
    expect(fundingCurrent(CME, false, close - 60, 0, 3_600, close + 49 * HOUR + 3_540)).toBe(false);
    expect(fundingCurrent(CME, true, 0, 0, 3_600, close)).toBe(true);
    expect(fundingCurrent(SCHEDULE_ALWAYS_OPEN, false, close - 3_600, 0, 3_600, close)).toBe(false);
    // The carried remainder counts
    expect(fundingCurrent(CME, false, close + 49 * HOUR, 600, 3_600, close + 49 * HOUR + 2_999)).toBe(true);
    expect(fundingCurrent(CME, false, close + 49 * HOUR, 600, 3_600, close + 49 * HOUR + 3_000)).toBe(false);
  });

  it('agrees with the session vectors', () => {
    for (const [ts, open, untilClose] of vectors.sessions as [number, boolean, number][]) {
      const since = secondsSinceOpen(CME, ts);
      expect(since !== null, `at ${ts}`).toBe(open);
      if (since !== null) {
        // The whole time since the open was in session, the second before was not
        expect(openSecondsBetween(CME, ts - since, ts)).toBe(since);
        expect(isOpen(CME, ts - since - 1)).toBe(false);
        // Up to the weekly close only the daily breaks (<= 4 x 1 h) are closed
        const closed = untilClose - openSecondsBetween(CME, ts, ts + untilClose);
        expect(closed >= 0 && closed % HOUR === 0 && closed <= 4 * HOUR, `at ${ts}`).toBe(true);
      } else {
        expect(openSecondsBetween(CME, ts, ts + 1)).toBe(0);
      }
    }
  });

  it('matches a brute-force count across both 2026 DST changes', () => {
    for (const start of [1_772_323_200, 1_793_404_800]) {
      // 2026-03-01 and 2026-11-01 00:00 UTC, three weeks each, 5-minute steps
      let acc = 0;
      for (let t = start; t < start + 21 * DAY; t += 300) {
        expect(openSecondsBetween(CME, start, t), `at ${t}`).toBe(acc);
        const since = secondsSinceOpen(CME, t);
        expect(since !== null).toBe(isOpen(CME, t));
        if (isOpen(CME, t)) acc += 300;
      }
    }
  });

  it('is additive', () => {
    let x = 0x1234_5678;
    const next = () => {
      x ^= x << 13;
      x ^= x >>> 17;
      x ^= x << 5;
      return x >>> 0;
    };
    const base = 1_767_225_600; // 2026-01-01
    for (let i = 0; i < 2_000; i++) {
      const [a, c, b] = [0, 0, 0].map(() => base - 365 * DAY + (next() % (4 * 365)) * DAY + (next() % DAY)).sort((p, q) => p - q);
      const f = (p: number, q: number) => openSecondsBetween(CME, p, q);
      expect(f(a, c) + f(c, b)).toBe(f(a, b));
      expect(f(a, b) <= b - a).toBe(true);
    }
  });

  it('marks the pre-close window', () => {
    expect(inPreCloseWindow(CME, FRIDAY_CLOSE - 7_200, 7_200)).toBe(true);
    expect(inPreCloseWindow(CME, FRIDAY_CLOSE - 7_201, 7_200)).toBe(false);
    expect(inPreCloseWindow(CME, FRIDAY_CLOSE, 7_200)).toBe(false);
    expect(inPreCloseWindow(CME, FRIDAY_CLOSE - 1, 0)).toBe(false);
  });
});

describe('next funding time', () => {
  const HOUR = 3_600;
  const CME = SCHEDULE_CME_GLOBEX;

  it('skips the weekend', () => {
    // Cranked a minute before the Friday close: due 59 open minutes after the reopen
    const last = FRIDAY_CLOSE - 60;
    const next = nextFundingTime(CME, last, 0, 3_600);
    expect(next).toBe(FRIDAY_CLOSE + 49 * HOUR + 3_540);
    expect(fundingCurrent(CME, false, last, 0, 3_600, next - 1)).toBe(true);
    expect(fundingCurrent(CME, false, last, 0, 3_600, next)).toBe(false);
  });

  it('counts the carried remainder and skips the daily break', () => {
    // Mon 2026-11-16 16:30 ET (21:30 UTC), 1,200 s carried: 40 open minutes
    // left, 30 before the break and 10 after the 18:00 reopen
    const monday = 1_794_868_200 - 3_600;
    expect(nextFundingTime(CME, monday, 1_200, 3_600)).toBe(monday + HOUR + 40 * 60); // 18:10 ET
    // Already due: the next in-session moment
    expect(nextFundingTime(CME, monday, 3_600, 3_600)).toBe(monday);
    expect(nextFundingTime(CME, FRIDAY_CLOSE + 60, 3_600, 3_600)).toBe(FRIDAY_CLOSE + 49 * HOUR);
  });

  it('is wall clock for always-open markets', () => {
    expect(nextFundingTime(SCHEDULE_ALWAYS_OPEN, 1_000, 600, 3_600)).toBe(4_000);
  });

  it('is the first in-session time with enough open seconds', () => {
    let x = 0x9e37_79b9;
    const next = () => {
      x ^= x << 13;
      x ^= x >>> 17;
      x ^= x << 5;
      return x >>> 0;
    };
    const base = 1_767_225_600;
    for (let i = 0; i < 2_000; i++) {
      const from = base + (next() % (3 * 365 * 86_400));
      const need = next() % (3 * 86_400);
      const t = timeAfterOpenSeconds(CME, from, need);
      expect(openSecondsBetween(CME, from, t), `from ${from} need ${need}`).toBe(need);
      expect(isOpen(CME, t), `from ${from} need ${need}`).toBe(true);
      expect(t === from || !isOpen(CME, t - 1) || openSecondsBetween(CME, from, t - 1) < need).toBe(true);
    }
  });
});
