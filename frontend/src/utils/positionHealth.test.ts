import { describe, it, expect } from 'vitest';
import { accruedFunding, positionHealth, positionMarketState } from './positionHealth';
import { SCHEDULE_CME_GLOBEX } from './session';

// Same cases as the program and keeper tests: 100 contracts of oil at $75, $750 collateral
const oilLong = { isLong: true, size: 100, collateral: 750, entryPrice: 75 };

describe('positionHealth', () => {
  it('matches the program margin ratio', () => {
    expect(positionHealth(oilLong, 75).marginRatio).toBeCloseTo(10);
    expect(positionHealth(oilLong, 71).marginRatio).toBeCloseTo(4.667, 3); // program: 466 bps
  });

  it('uses entry notional, like the program', () => {
    // At $80 equity is $1,250 on $7,500 entry notional (not $8,000)
    expect(positionHealth(oilLong, 80)).toEqual({ unrealizedPnl: 500, marginRatio: (1_250 / 7_500) * 100 });
  });

  it('counts unpaid funding', () => {
    // $0.3975 per contract owed on 100 contracts = $39.75; program: 947 bps
    const funding = accruedFunding(true, 100, 0.3975, 0);
    expect(funding).toBeCloseTo(-39.75);
    expect(positionHealth(oilLong, 75, funding).marginRatio).toBeCloseTo(9.47);
  });

  it('pays shorts when funding is positive', () => {
    expect(accruedFunding(false, 100, 0.0075, 0)).toBeCloseTo(0.75);
  });

  it('is zero when underwater', () => {
    expect(positionHealth(oilLong, 65).marginRatio).toBe(0);
  });
});

describe('positionMarketState', () => {
  const FRIDAY_CLOSE = 1_794_607_200; // Fri 2026-11-13 17:00 ET
  const oil = {
    maintenanceMarginRatio: 500,
    weekendMaintenanceMarginRatio: 1_000,
    sessionSchedule: SCHEDULE_CME_GLOBEX,
    preCloseWindowSecs: 7_200,
    settlementPrice: 0,
  };

  it('uses the maintenance margin midweek and the weekend margin before the close', () => {
    const wednesday = FRIDAY_CLOSE - 2 * 86_400 - 5 * 3_600;
    expect(positionMarketState(oil, wednesday)).toMatchObject({ liquidationThreshold: 5, warningThreshold: 10 });
    expect(positionMarketState(oil, FRIDAY_CLOSE - 3_600)).toMatchObject({ liquidationThreshold: 10, warningThreshold: 20 });
  });

  it('warns at the weekend margin within a day of it starting', () => {
    const daySoon = FRIDAY_CLOSE - 7_200 - 3_600;
    const strict = { ...oil, weekendMaintenanceMarginRatio: 1_500 };
    expect(positionMarketState(strict, daySoon)).toMatchObject({ liquidationThreshold: 5, warningThreshold: 15 });
  });

  it('never shows a weekend threshold below the normal one', () => {
    const lax = { ...oil, weekendMaintenanceMarginRatio: 300 };
    expect(positionMarketState(lax, FRIDAY_CLOSE - 60).liquidationThreshold).toBe(5);
  });

  it('reports settlement and closed hours', () => {
    expect(positionMarketState({ ...oil, settlementPrice: 12_340_000 }, FRIDAY_CLOSE).settlementPrice).toBeCloseTo(12.34);
    expect(positionMarketState(oil, FRIDAY_CLOSE).closeBlockedReason).toBe('Market closed. Trading resumes Sunday 6:00 PM ET');
    expect(positionMarketState({ ...oil, settlementPrice: 12_340_000 }, FRIDAY_CLOSE).closeBlockedReason).toBeNull();
  });
});
