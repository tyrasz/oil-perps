import { describe, it, expect } from 'vitest';
import { accruedFunding, liquidationPrice, positionHealth, positionMarketState } from './positionHealth';
import { SCHEDULE_CME_GLOBEX } from './session';

// Same cases as the program and keeper tests: 100 contracts of oil at $75, $750 collateral
const oilLong = { isLong: true, size: 100, collateral: 750, entryPrice: 75 };

describe('positionHealth', () => {
  it('matches the program margin ratio on mark notional', () => {
    expect(positionHealth(oilLong, 75).marginRatio).toBeCloseTo(10);
    // $350 equity on $7,100 mark notional; program margin_ratio_mark: 492 bps
    expect(positionHealth(oilLong, 71).marginRatio).toBeCloseTo(4.93, 2);
  });

  it('uses mark notional, like liquidation (docs/lp-pool.md §6.1)', () => {
    // At $80 equity is $1,250 on $8,000 mark notional (not $7,500 entry)
    expect(positionHealth(oilLong, 80)).toEqual({ unrealizedPnl: 500, equity: 1_250, marginRatio: (1_250 / 8_000) * 100 });
  });

  it('finds the liquidation price on mark notional', () => {
    const p = liquidationPrice(oilLong, 500);
    expect(p).toBeCloseTo(6_750 / 95);
    expect(positionHealth(oilLong, p).marginRatio).toBeCloseTo(5);
    const short = { ...oilLong, isLong: false };
    const q = liquidationPrice(short, 500);
    expect(q).toBeCloseTo(8_250 / 105);
    expect(positionHealth(short, q).marginRatio).toBeCloseTo(5);
    // Funding owed brings it closer
    expect(liquidationPrice(oilLong, 500, -39.75)).toBeGreaterThan(p);
    expect(liquidationPrice({ ...oilLong, collateral: 8_000 }, 500)).toBe(0);
  });

  it('counts unpaid funding', () => {
    // $0.3975 per contract owed on 100 contracts = $39.75; program: 947 bps
    const funding = accruedFunding(true, 100, 0.3975, 0);
    expect(funding).toBeCloseTo(-39.75);
    expect(positionHealth(oilLong, 75, funding).marginRatio).toBeCloseTo(9.47);
    expect(positionHealth(oilLong, 75, funding).equity).toBeCloseTo(710.25);
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

  it('puts the backstop line below the liquidation threshold', () => {
    const wednesday = FRIDAY_CLOSE - 2 * 86_400 - 5 * 3_600;
    const pooled = { ...oil, backstopThresholdBps: 6_667, liqPenaltyBps: 200 };
    // floor(500·6667/1e4) = 333 bps
    const s = positionMarketState(pooled, wednesday);
    expect(s).toMatchObject({ liquidationThreshold: 5, backstopThreshold: 3.33 });
    expect(s.liquidationNote).toMatch(/closes part of the position, with a 2.00% penalty/);
    // Weekend MMR 10%: floor(1000·6667/1e4) = 666 bps
    expect(positionMarketState(pooled, FRIDAY_CLOSE - 3_600).backstopThreshold).toBe(6.66);
    // Neutral params (or none): every liquidation is a full close
    const neutral = positionMarketState(oil, wednesday);
    expect(neutral.backstopThreshold).toBe(5);
    expect(neutral.liquidationNote).not.toMatch(/part of the position/);
  });

  it('reports settlement and closed hours', () => {
    expect(positionMarketState({ ...oil, settlementPrice: 12_340_000 }, FRIDAY_CLOSE).settlementPrice).toBeCloseTo(12.34);
    expect(positionMarketState(oil, FRIDAY_CLOSE).closeBlockedReason).toBe('Market closed. Trading resumes Sunday 6:00 PM ET');
    expect(positionMarketState({ ...oil, settlementPrice: 12_340_000 }, FRIDAY_CLOSE).closeBlockedReason).toBeNull();
  });
});
