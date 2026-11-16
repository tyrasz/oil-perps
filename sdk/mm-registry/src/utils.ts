import { BN } from "@coral-xyz/anchor";

// Price decimals (6 for USDC-style)
export const PRICE_DECIMALS = 6;
export const PRICE_MULTIPLIER = 10 ** PRICE_DECIMALS;

// Size decimals (for position sizing)
export const SIZE_DECIMALS = 6;
export const SIZE_MULTIPLIER = 10 ** SIZE_DECIMALS;

// Basis points
export const BPS_DECIMALS = 4;
export const BPS_MULTIPLIER = 10000;

/**
 * Convert a human-readable price to on-chain format
 */
export function toPrice(price: number): BN {
  return new BN(Math.round(price * PRICE_MULTIPLIER));
}

/**
 * Convert on-chain price to human-readable format
 */
export function fromPrice(price: BN): number {
  return price.toNumber() / PRICE_MULTIPLIER;
}

/**
 * Convert a human-readable size to on-chain format
 */
export function toSize(size: number): BN {
  return new BN(Math.round(size * SIZE_MULTIPLIER));
}

/**
 * Convert on-chain size to human-readable format
 */
export function fromSize(size: BN): number {
  return size.toNumber() / SIZE_MULTIPLIER;
}

/**
 * Convert basis points to percentage
 */
export function bpsToPercent(bps: number): number {
  return bps / 100;
}

/**
 * Convert percentage to basis points
 */
export function percentToBps(percent: number): number {
  return Math.round(percent * 100);
}

/**
 * Calculate spread in basis points
 */
export function calculateSpreadBps(bidPrice: BN, askPrice: BN): number {
  if (bidPrice.isZero()) return 0;
  return askPrice
    .sub(bidPrice)
    .muln(BPS_MULTIPLIER)
    .div(bidPrice)
    .toNumber();
}

/**
 * Calculate mid price
 */
export function calculateMidPrice(bidPrice: BN, askPrice: BN): BN {
  return bidPrice.add(askPrice).divn(2);
}

/**
 * Calculate notional value
 */
export function calculateNotional(size: BN, price: BN): BN {
  return size.mul(price).divn(PRICE_MULTIPLIER);
}

/**
 * Calculate collateral required (10% of notional)
 */
export function calculateCollateralRequired(notional: BN): BN {
  return notional.divn(10);
}

/**
 * Format price for display
 */
export function formatPrice(price: BN, decimals: number = 2): string {
  return fromPrice(price).toFixed(decimals);
}

/**
 * Format size for display
 */
export function formatSize(size: BN, decimals: number = 4): string {
  return fromSize(size).toFixed(decimals);
}

/**
 * Calculate PnL from entry to current price
 */
export function calculatePnl(
  size: BN,
  entryPrice: BN,
  currentPrice: BN,
  isLong: boolean
): BN {
  const priceDiff = isLong
    ? currentPrice.sub(entryPrice)
    : entryPrice.sub(currentPrice);

  return size.mul(priceDiff).divn(PRICE_MULTIPLIER);
}

/**
 * Check if quote is valid (not expired, has remaining size)
 */
export function isQuoteValid(
  bidRemaining: BN,
  askRemaining: BN,
  expiresAt: BN,
  currentTimestamp: number
): boolean {
  const hasRemaining = bidRemaining.gtn(0) || askRemaining.gtn(0);
  const notExpired = expiresAt.gtn(currentTimestamp);
  return hasRemaining && notExpired;
}

/**
 * Calculate fill amount respecting remaining and max size
 */
export function calculateFillAmount(
  requestedSize: BN,
  remaining: BN,
  maxFillSize?: BN
): BN {
  let fillAmount = BN.min(requestedSize, remaining);
  if (maxFillSize) {
    fillAmount = BN.min(fillAmount, maxFillSize);
  }
  return fillAmount;
}

/**
 * Generate expiration timestamp
 */
export function getExpirationTimestamp(secondsFromNow: number): BN {
  return new BN(Math.floor(Date.now() / 1000) + secondsFromNow);
}

/**
 * Check if timestamp is expired
 */
export function isExpired(expiresAt: BN): boolean {
  return expiresAt.ltn(Math.floor(Date.now() / 1000));
}
