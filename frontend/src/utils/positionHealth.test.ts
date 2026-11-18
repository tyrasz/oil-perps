import { describe, it, expect } from 'vitest';
import { accruedFunding, positionHealth } from './positionHealth';

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
