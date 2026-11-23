import { LEVERAGE_DECIMALS } from '../config/program';

const BPS = 10_000;
const DEFAULT_MAINTENANCE_MARGIN_BPS = 500; // 5%

interface MarketMarginParams {
  /** On-chain max leverage, 3 decimals (20x = 20_000). */
  maxLeverage: number;
  /** On-chain initial margin ratio in basis points (10% = 1_000). */
  initialMarginRatio: number;
  /** Initial margin before the weekly close, basis points; 0 = none. */
  weekendInitialMarginRatio?: number;
}

/**
 * Highest leverage the program will accept: the lowest of the commodity's UI
 * limit, the market's max leverage, and 1 / initial margin ratio (the weekend
 * initial margin when `preClose` is set).
 */
export function maxAllowedLeverage(
  commodityMaxLeverage: number,
  market?: MarketMarginParams | null,
  preClose = false
): number {
  const limits = [commodityMaxLeverage];
  if (market) {
    if (market.maxLeverage > 0) limits.push(market.maxLeverage / 10 ** LEVERAGE_DECIMALS);
    const initial = preClose
      ? Math.max(market.initialMarginRatio, market.weekendInitialMarginRatio ?? 0)
      : market.initialMarginRatio;
    if (initial > 0) limits.push(BPS / initial);
  }
  return Math.min(...limits);
}

/**
 * Price at which a new position becomes liquidatable, ignoring fees and
 * funding: equity / **mark** notional = maintenance margin (docs/lp-pool.md
 * §6.1), with collateral = entry notional / leverage. Long:
 * `e·(1 − 1/L) / (1 − m)`; short: `e·(1 + 1/L) / (1 + m)`.
 */
export function estimateLiquidationPrice(
  entryPrice: number,
  leverage: number,
  side: 'long' | 'short',
  maintenanceMarginBps: number = DEFAULT_MAINTENANCE_MARGIN_BPS
): number {
  if (entryPrice <= 0 || leverage <= 0) return 0;
  const m = maintenanceMarginBps / BPS;
  const price = side === 'long' ? (entryPrice * (1 - 1 / leverage)) / (1 - m) : (entryPrice * (1 + 1 / leverage)) / (1 + m);
  return Math.max(0, price);
}

/** Leverage for display: whole numbers as-is, otherwise rounded down to 2 decimals. */
export function formatLeverage(leverage: number): string {
  return Number.isInteger(leverage) ? String(leverage) : (Math.floor(leverage * 100) / 100).toFixed(2);
}
