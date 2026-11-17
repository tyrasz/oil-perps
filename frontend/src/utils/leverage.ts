import { LEVERAGE_DECIMALS } from '../config/program';

const BPS = 10_000;
const DEFAULT_MAINTENANCE_MARGIN_BPS = 500; // 5%

interface MarketMarginParams {
  /** On-chain max leverage, 3 decimals (20x = 20_000). */
  maxLeverage: number;
  /** On-chain initial margin ratio in basis points (10% = 1_000). */
  initialMarginRatio: number;
}

/**
 * Highest leverage the program will accept: the lowest of the commodity's UI
 * limit, the market's max leverage, and 1 / initial margin ratio.
 */
export function maxAllowedLeverage(
  commodityMaxLeverage: number,
  market?: MarketMarginParams | null
): number {
  const limits = [commodityMaxLeverage];
  if (market) {
    if (market.maxLeverage > 0) limits.push(market.maxLeverage / 10 ** LEVERAGE_DECIMALS);
    if (market.initialMarginRatio > 0) limits.push(BPS / market.initialMarginRatio);
  }
  return Math.min(...limits);
}

/**
 * Price at which a new position hits maintenance margin, ignoring fees and
 * funding: equity / entry notional = maintenance margin, with
 * collateral = notional / leverage.
 */
export function estimateLiquidationPrice(
  entryPrice: number,
  leverage: number,
  side: 'long' | 'short',
  maintenanceMarginBps: number = DEFAULT_MAINTENANCE_MARGIN_BPS
): number {
  if (entryPrice <= 0 || leverage <= 0) return 0;
  const buffer = 1 / leverage - maintenanceMarginBps / BPS;
  const price = side === 'long' ? entryPrice * (1 - buffer) : entryPrice * (1 + buffer);
  return Math.max(0, price);
}
