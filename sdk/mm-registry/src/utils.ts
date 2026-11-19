import { BN } from "@coral-xyz/anchor";
import { Keypair } from "@solana/web3.js";

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
 * Calculate collateral required (10% of notional).
 * @deprecated Quotes lock nothing; fills margin in perps-core (see venueMargin).
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

// ============================================================================
// Fills (perps-core venue_fill)
// ============================================================================

/** Longest a quote may live (mm-registry MAX_QUOTE_TTL_SECS) */
export const MAX_QUOTE_TTL_SECS = 86_400;

/** Compute budget for a taker's fill transaction */
export const FILL_COMPUTE_UNITS = 300_000;

/** Lifetime of the taker's one-shot approval */
export const ONE_SHOT_APPROVAL_SECS = 120;

const BPS = new BN(10_000);
const ONE = new BN(PRICE_MULTIPLIER);

function ceilDiv(a: BN, b: BN): BN {
  return a.add(b).subn(1).div(b);
}

/**
 * Collateral a venue fill posts for one side: mirrors perps-core's
 * `venue_margin`, `max(notional_fill / lev, IM x notional_oracle) + adverse`,
 * where adverse is the side's immediate loss at the oracle.
 *
 * `initialMarginBps` is the initial margin in force at fill time (the weekend
 * value inside the pre-close window). Returns null when `leverage` is above
 * what that margin allows.
 */
export function venueMargin(
  isLong: boolean,
  size: BN,
  price: BN,
  oracle: BN,
  leverage: number,
  initialMarginBps: number
): BN | null {
  if (leverage <= 0) return null;
  const im = new BN(initialMarginBps);
  const notionalFill = size.mul(price).div(ONE);
  const notionalOracle = size.mul(oracle).div(ONE);
  const base = ceilDiv(notionalFill.mul(new BN(1_000)), new BN(leverage));
  if (base.lt(ceilDiv(notionalFill.mul(im), BPS))) return null;
  const imOracle = ceilDiv(notionalOracle.mul(im), BPS);
  // bn.js div truncates toward zero, like perps-core's i128 PnL
  const diff = isLong ? oracle.sub(price) : price.sub(oracle);
  const pnl = size.mul(diff).div(ONE);
  const adverse = pnl.isNeg() ? pnl.neg() : new BN(0);
  return BN.max(base, imOracle).add(adverse);
}

/**
 * The most collateral a fill at `price` can post at any oracle the venue's
 * band (`bandBps`) accepts: the allowance for a one-shot taker approval.
 * venueMargin is convex in the oracle, so the band's two ends bound it.
 */
export function worstCaseVenueMargin(
  isLong: boolean,
  size: BN,
  price: BN,
  leverage: number,
  initialMarginBps: number,
  bandBps: number
): BN | null {
  const b = new BN(bandBps);
  // within_band: |price - oracle| * 10_000 <= oracle * band
  const lo = ceilDiv(price.mul(BPS), BPS.add(b));
  const hi = price.mul(BPS).div(BPS.sub(b));
  let worst: BN | null = null;
  for (const oracle of [lo, price, hi]) {
    const margin = venueMargin(isLong, size, price, oracle, leverage, initialMarginBps);
    if (margin === null) return null;
    if (worst === null || margin.gt(worst)) worst = margin;
  }
  return worst;
}

/**
 * A random u64 for FillQuoteParams.positionNonce. A reused nonce only makes
 * that fill fail, but a counter would let any other fill landing first
 * invalidate a transaction already built.
 */
export function randomPositionNonce(): BN {
  const bytes = new Uint8Array(8);
  const webCrypto = (globalThis as { crypto?: { getRandomValues?: (a: Uint8Array) => Uint8Array } }).crypto;
  if (webCrypto && webCrypto.getRandomValues) {
    webCrypto.getRandomValues(bytes);
  } else {
    bytes.set(Keypair.generate().publicKey.toBytes().subarray(0, 8));
  }
  return new BN(Buffer.from(bytes), "le");
}
