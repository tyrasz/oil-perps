import { describe, it, expect } from 'vitest';
import { estimateLiquidationPrice, maxAllowedLeverage } from './leverage';

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
  it('matches the program margin rule for longs and shorts', () => {
    // 10x with 5% maintenance: liquidated after a 5% adverse move
    expect(estimateLiquidationPrice(75, 10, 'long', 500)).toBeCloseTo(71.25);
    expect(estimateLiquidationPrice(75, 10, 'short', 500)).toBeCloseTo(78.75);
  });

  it('gives more room at lower leverage', () => {
    expect(estimateLiquidationPrice(75, 2, 'long', 500)).toBeCloseTo(41.25);
  });

  it('never goes below zero', () => {
    expect(estimateLiquidationPrice(75, 0.5, 'long', 500)).toBe(0);
  });
});
