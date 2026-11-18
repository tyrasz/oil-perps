import { describe, it, expect } from 'vitest';
import {
  SCHEDULE_ALWAYS_OPEN,
  SCHEDULE_CME_GLOBEX,
  easternOffset,
  isOpen,
  marketSession,
  secondsUntilWeeklyClose,
  formatDuration,
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
