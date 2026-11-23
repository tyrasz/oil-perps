import { describe, it, expect } from 'vitest';
import { estimateLiquidationPrice, formatLeverage, maxAllowedLeverage } from './leverage';

describe('maxAllowedLeverage', () => {
  it('uses the commodity limit when no on-chain market is loaded', () => {
    expect(maxAllowedLeverage(20)).toBe(20);
    expect(maxAllowedLeverage(20, null)).toBe(20);
  });

  it('is capped by the initial margin ratio', () => {
    // OIL: 20x max leverage but 10% initial margin -> 10x
    expect(maxAllowedLeverage(20, { maxLeverage: 20_000, initialMarginRatio: 1_000 })).toBe(10);
  });

  it('is capped by the on-chain max leverage', () => {
    expect(maxAllowedLeverage(20, { maxLeverage: 5_000, initialMarginRatio: 500 })).toBe(5);
  });

  it('uses the weekend initial margin before the weekly close', () => {
    const market = { maxLeverage: 20_000, initialMarginRatio: 1_000, weekendInitialMarginRatio: 2_000 };
    expect(maxAllowedLeverage(20, market)).toBe(10);
    expect(maxAllowedLeverage(20, market, true)).toBe(5);
    expect(maxAllowedLeverage(20, { ...market, weekendInitialMarginRatio: 0 }, true)).toBe(10);
  });

  it('ignores unset on-chain values', () => {
    expect(maxAllowedLeverage(15, { maxLeverage: 0, initialMarginRatio: 0 })).toBe(15);
  });
});

describe('estimateLiquidationPrice', () => {
  it('matches the program margin rule (mark notional) for longs and shorts', () => {
    // 10x with 5% maintenance: $750 collateral on 100 contracts at $75.
    // Long at $71.0526: equity $355.26 = 5% of $7,105.26 mark notional
    expect(estimateLiquidationPrice(75, 10, 'long', 500)).toBeCloseTo(67.5 / 0.95);
    expect(estimateLiquidationPrice(75, 10, 'short', 500)).toBeCloseTo(82.5 / 1.05);
    const p = estimateLiquidationPrice(75, 10, 'long', 500);
    expect(750 + 100 * (p - 75)).toBeCloseTo(0.05 * 100 * p);
  });

  it('gives more room at lower leverage', () => {
    expect(estimateLiquidationPrice(75, 2, 'long', 500)).toBeCloseTo(37.5 / 0.95);
  });

  it('never goes below zero', () => {
    expect(estimateLiquidationPrice(75, 0.5, 'long', 500)).toBe(0);
  });
});

describe('formatLeverage', () => {
  it('rounds caps that 1/initial margin leaves fractional', () => {
    expect(formatLeverage(10)).toBe('10');
    expect(formatLeverage(10_000 / 1_500)).toBe('6.66'); // NATGAS, 15% initial margin
    expect(formatLeverage(10_000 / 2_400)).toBe('4.16'); // SILVER weekend
  });
});
