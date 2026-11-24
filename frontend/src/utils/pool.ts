// The house (LP pool) side of perps-core as the frontend sees it: account
// decoders, the open/close quotes, the caps a trade must fit, the profit-close
// quiet time and the LP-flow gates. Each function names the program code it
// mirrors (programs/perps-core/src); the pure math is in poolMath.ts.

import * as pm from './poolMath';
import {
  fundingCurrent,
  inPreCloseWindow,
  isOpen,
  openSecondsBetween,
  secondsSinceOpen,
  timeAfterOpenSeconds,
} from './session';

const SCALE = 1_000_000n;
const BPS = 10_000n;

// ---------------------------------------------------------------------------
// Decoding (Anchor returns BN for 64/128-bit fields and numbers below that)
// ---------------------------------------------------------------------------

type IntLike = { toString(): string } | number | bigint | null | undefined;

/** A BN, number or bigint as a bigint (`0n` when absent, e.g. with an older IDL). */
export function big(x: IntLike): bigint {
  if (x === null || x === undefined) return 0n;
  if (typeof x === 'bigint') return x;
  if (typeof x === 'number') return BigInt(Math.trunc(x));
  return BigInt(x.toString());
}

/** 6-dp integer to a JS number (display only). */
export const fromUnits = (x: bigint): number => Number(x) / 1e6;
/** JS number to a 6-dp integer, rounded to the nearest unit. */
export const toUnits = (x: number): bigint => BigInt(Math.round(x * 1e6));

/** `PoolParams` (state/pool.rs), Borsh order. */
export interface PoolParams {
  skewScale: bigint;
  insuranceTarget: bigint;
  insuranceClaimCapPerWindow: bigint;
  liqMinRemainingNotional: bigint;
  kNetBps: bigint;
  hardNetBps: bigint;
  perAccountCapBps: bigint;
  weekendLpLossBps: bigint;
  weekendGapBps: bigint;
  weekendCapWindowSecs: bigint;
  withdrawalDelaySecs: bigint;
  withdrawalClaimWindowSecs: bigint;
  lpReopenQuietSecs: bigint;
  adlNetTargetBps: bigint;
  adlExposureScoreFloorBps: bigint;
  ddFreezeBps: bigint;
  ddAdlBps: bigint;
  ddWithdrawBlockBps: bigint;
  redeemLossHaircutBps: bigint;
  openFeeBps: bigint;
  maxImpactBps: bigint;
  liqPenaltyBps: bigint;
  liqKeeperShareBps: bigint;
  backstopThresholdBps: bigint;
  minLiqSliceBps: bigint;
  treasuryFeeBps: bigint;
  lpLossCapWindowBps: bigint;
  profitCloseQuietSecs: bigint;
}

export const POOL_PARAM_KEYS: (keyof PoolParams)[] = [
  'skewScale',
  'insuranceTarget',
  'insuranceClaimCapPerWindow',
  'liqMinRemainingNotional',
  'kNetBps',
  'hardNetBps',
  'perAccountCapBps',
  'weekendLpLossBps',
  'weekendGapBps',
  'weekendCapWindowSecs',
  'withdrawalDelaySecs',
  'withdrawalClaimWindowSecs',
  'lpReopenQuietSecs',
  'adlNetTargetBps',
  'adlExposureScoreFloorBps',
  'ddFreezeBps',
  'ddAdlBps',
  'ddWithdrawBlockBps',
  'redeemLossHaircutBps',
  'openFeeBps',
  'maxImpactBps',
  'liqPenaltyBps',
  'liqKeeperShareBps',
  'backstopThresholdBps',
  'minLiqSliceBps',
  'treasuryFeeBps',
  'lpLossCapWindowBps',
  'profitCloseQuietSecs',
];

/** Fields that are u64 on chain (BN in Anchor); the rest are u16/u32 numbers. */
export const POOL_PARAM_U64: ReadonlySet<keyof PoolParams> = new Set([
  'skewScale',
  'insuranceTarget',
  'insuranceClaimCapPerWindow',
  'liqMinRemainingNotional',
]);

/** Upper bound of each field's integer type, for input validation. */
export function poolParamMax(key: keyof PoolParams): bigint {
  if (POOL_PARAM_U64.has(key)) return (1n << 64n) - 1n;
  const u16: (keyof PoolParams)[] = [
    'ddFreezeBps', 'ddAdlBps', 'ddWithdrawBlockBps', 'redeemLossHaircutBps', 'openFeeBps', 'maxImpactBps',
    'liqPenaltyBps', 'liqKeeperShareBps', 'backstopThresholdBps', 'minLiqSliceBps', 'treasuryFeeBps', 'lpLossCapWindowBps',
  ];
  return u16.includes(key) ? 0xffffn : 0xffff_ffffn;
}

export function decodePoolParams(raw: Record<string, IntLike> | null | undefined): PoolParams {
  const out = {} as PoolParams;
  for (const k of POOL_PARAM_KEYS) out[k] = big(raw?.[k]);
  return out;
}

/** Production defaults for OIL (§15.1, `PoolParams::production()`). */
export function productionPoolParams(): PoolParams {
  return {
    skewScale: 2_500_000_000_000n,
    insuranceTarget: 250_000_000_000n,
    insuranceClaimCapPerWindow: 0n,
    liqMinRemainingNotional: 100_000_000n,
    kNetBps: 5_000n,
    hardNetBps: 7_500n,
    perAccountCapBps: 2_500n,
    weekendLpLossBps: 1_000n,
    weekendGapBps: 0n,
    weekendCapWindowSecs: 86_400n,
    withdrawalDelaySecs: 345_600n,
    withdrawalClaimWindowSecs: 259_200n,
    lpReopenQuietSecs: 1_800n,
    adlNetTargetBps: 2_500n,
    adlExposureScoreFloorBps: 0n,
    ddFreezeBps: 1_000n,
    ddAdlBps: 1_500n,
    ddWithdrawBlockBps: 0n,
    redeemLossHaircutBps: 5_000n,
    openFeeBps: 5n,
    maxImpactBps: 100n,
    liqPenaltyBps: 200n,
    liqKeeperShareBps: 2_000n,
    backstopThresholdBps: 6_667n,
    minLiqSliceBps: 2_000n,
    treasuryFeeBps: 1_000n,
    lpLossCapWindowBps: 500n,
    profitCloseQuietSecs: 300n,
  };
}

/** Neutral values (§15.2, `PoolParams::neutral()`): today's numbers. */
export function neutralPoolParams(): PoolParams {
  return {
    ...productionPoolParams(),
    skewScale: 0n,
    openFeeBps: 0n,
    treasuryFeeBps: 0n,
    insuranceTarget: (1n << 64n) - 1n,
    kNetBps: 100_000n,
    hardNetBps: 0n,
    perAccountCapBps: 0n,
    weekendCapWindowSecs: 0n,
    ddFreezeBps: 0n,
    ddAdlBps: 0n,
    ddWithdrawBlockBps: 0n,
    backstopThresholdBps: 10_000n,
    liqPenaltyBps: 0n,
    lpLossCapWindowBps: 10_000n,
    withdrawalDelaySecs: 0n,
    withdrawalClaimWindowSecs: 0n,
    lpReopenQuietSecs: 0n,
    profitCloseQuietSecs: 0n,
  };
}

/** Anchor argument for `initialize_pool` / `propose_pool_params`: BN-like for u64, numbers otherwise. */
export function encodePoolParams<T>(p: PoolParams, toBn: (x: bigint) => T): Record<keyof PoolParams, T | number> {
  const out = {} as Record<keyof PoolParams, T | number>;
  for (const k of POOL_PARAM_KEYS) out[k] = POOL_PARAM_U64.has(k) ? toBn(p[k]) : Number(p[k]);
  return out;
}

/** The Market fields the frontend models. */
export interface MarketState {
  maxLeverage: bigint;
  maintenanceMarginRatio: bigint;
  initialMarginRatio: bigint;
  takerFee: bigint;
  liquidationFee: bigint;
  longOpenInterest: bigint;
  shortOpenInterest: bigint;
  maxOpenInterest: bigint;
  cumulativeFunding: bigint;
  lastFundingTime: bigint;
  fundingInterval: bigint;
  insuranceFund: bigint;
  isPaused: boolean;
  sessionSchedule: number;
  preCloseWindowSecs: number;
  weekendInitialMarginRatio: bigint;
  weekendMaintenanceMarginRatio: bigint;
  maxConfidenceBps: number;
  minOraclePrice: bigint;
  settlementPrice: bigint;
  fundingCarrySecs: number;
  settledAt: bigint;
}

export function decodeMarket(raw: Record<string, unknown>): MarketState {
  const r = raw as Record<string, IntLike> & { isPaused?: boolean };
  return {
    maxLeverage: big(r.maxLeverage),
    maintenanceMarginRatio: big(r.maintenanceMarginRatio),
    initialMarginRatio: big(r.initialMarginRatio),
    takerFee: big(r.takerFee),
    liquidationFee: big(r.liquidationFee),
    longOpenInterest: big(r.longOpenInterest),
    shortOpenInterest: big(r.shortOpenInterest),
    maxOpenInterest: big(r.maxOpenInterest),
    cumulativeFunding: big(r.cumulativeFunding),
    lastFundingTime: big(r.lastFundingTime),
    fundingInterval: big(r.fundingInterval),
    insuranceFund: big(r.insuranceFund),
    isPaused: !!r.isPaused,
    sessionSchedule: Number(big(r.sessionSchedule)),
    preCloseWindowSecs: Number(big(r.preCloseWindowSecs)),
    weekendInitialMarginRatio: big(r.weekendInitialMarginRatio),
    weekendMaintenanceMarginRatio: big(r.weekendMaintenanceMarginRatio),
    maxConfidenceBps: Number(big(r.maxConfidenceBps)),
    minOraclePrice: big(r.minOraclePrice),
    settlementPrice: big(r.settlementPrice),
    fundingCarrySecs: Number(big(r.fundingCarrySecs)),
    settledAt: big(r.settledAt),
  };
}

/** `LpPool` (§1.2). */
export interface LpPoolAccount extends pm.PoolState {
  adlKeeper: string;
  freeCollateral: bigint;
  positionCollateral: bigint;
  treasury: bigint;
  maxPoolCash: bigint;
  pendingBadDebtLong: bigint;
  pendingBadDebtShort: bigint;
  pendingSinceLong: bigint;
  pendingSinceShort: bigint;
  badDebtCum: bigint;
  badDebtRecoveredCum: bigint;
  badDebtWrittenOffCum: bigint;
  lossWindowStart: bigint;
  lossWindowNav: bigint;
  lpAbsorbedWindow: bigint;
  insPaidWindow: bigint;
  npsPeakAt: bigint;
  donatedCum: bigint;
  pendingParamsAt: bigint;
  params: PoolParams;
  pendingParams: PoolParams;
}

export function decodeLpPool(raw: Record<string, unknown>): LpPoolAccount {
  const r = raw as Record<string, IntLike>;
  const n = (k: string) => big(r[k]);
  return {
    adlKeeper: String(raw.adlKeeper ?? ''),
    freeCollateral: n('freeCollateral'),
    positionCollateral: n('positionCollateral'),
    cash: n('cash'),
    treasury: n('treasury'),
    longEntrySum: n('longEntrySum'),
    shortEntrySum: n('shortEntrySum'),
    longFundingSum: n('longFundingSum'),
    shortFundingSum: n('shortFundingSum'),
    openPositions: n('openPositions'),
    lpShares: n('lpShares'),
    pendingWithdrawShares: n('pendingWithdrawShares'),
    maxPoolCash: n('maxPoolCash'),
    pendingBadDebtLong: n('pendingBadDebtLong'),
    pendingBadDebtShort: n('pendingBadDebtShort'),
    pendingSinceLong: n('pendingSinceLong'),
    pendingSinceShort: n('pendingSinceShort'),
    badDebtCum: n('badDebtCum'),
    badDebtRecoveredCum: n('badDebtRecoveredCum'),
    badDebtWrittenOffCum: n('badDebtWrittenOffCum'),
    lossWindowStart: n('lossWindowStart'),
    lossWindowNav: n('lossWindowNav'),
    lpAbsorbedWindow: n('lpAbsorbedWindow'),
    insPaidWindow: n('insPaidWindow'),
    npsPeak: n('npsPeak'),
    npsPeakAt: n('npsPeakAt'),
    vestAmount: n('vestAmount'),
    vestStart: n('vestStart'),
    vestEnd: n('vestEnd'),
    donatedCum: n('donatedCum'),
    pendingParamsAt: n('pendingParamsAt'),
    params: decodePoolParams(raw.params as Record<string, IntLike>),
    pendingParams: decodePoolParams(raw.pendingParams as Record<string, IntLike>),
  };
}

/** `LpPosition` (§1.3). */
export interface LpPositionAccount {
  shares: bigint;
  pendingShares: bigint;
  requestedValue: bigint;
  requestedAt: bigint;
  unlockAt: bigint;
  depositedCum: bigint;
  withdrawnCum: bigint;
  claimWindowSecs: bigint;
}

export function decodeLpPosition(raw: Record<string, unknown>): LpPositionAccount {
  const r = raw as Record<string, IntLike>;
  return {
    shares: big(r.shares),
    pendingShares: big(r.pendingShares),
    requestedValue: big(r.requestedValue),
    requestedAt: big(r.requestedAt),
    unlockAt: big(r.unlockAt),
    depositedCum: big(r.depositedCum),
    withdrawnCum: big(r.withdrawnCum),
    claimWindowSecs: big(r.claimWindowSecs),
  };
}

// ---------------------------------------------------------------------------
// Market rules (state/market.rs, state/position.rs, state/venue.rs)
// ---------------------------------------------------------------------------

export const isSettled = (m: MarketState) => m.settlementPrice > 0n;
const max = (a: bigint, b: bigint) => (a > b ? a : b);
const min = (a: bigint, b: bigint) => (a < b ? a : b);
const ceilDiv = (a: bigint, b: bigint) => (a + b - 1n) / b;

/** `Market::margin_ratios`: `[initial, maintenance]` in force at `now`, bps. */
export function marginRatios(m: MarketState, now: number): [bigint, bigint] {
  if (inPreCloseWindow(m.sessionSchedule, now, m.preCloseWindowSecs)) {
    return [max(m.initialMarginRatio, m.weekendInitialMarginRatio), max(m.maintenanceMarginRatio, m.weekendMaintenanceMarginRatio)];
  }
  return [m.initialMarginRatio, m.maintenanceMarginRatio];
}

/** `Market::required_collateral`: `ceil(notional·1000/leverage)`, or null below the IM in force. */
export function requiredCollateral(m: MarketState, notional: bigint, leverage: bigint, now: number): bigint | null {
  if (leverage <= 0n) return null;
  const [imr] = marginRatios(m, now);
  const collateral = ceilDiv(notional * 1_000n, leverage);
  const initial = ceilDiv(notional * imr, BPS);
  return collateral < initial ? null : collateral;
}

/** Rust `/` on signed integers truncates toward zero; bigint `/` does too. */
export function unrealizedPnl(isLong: boolean, size: bigint, entryPrice: bigint, price: bigint): bigint {
  const diff = isLong ? price - entryPrice : entryPrice - price;
  return (size * diff) / SCALE;
}

/** `Position::accrued_funding`: owed to (+) or by (−) the position. */
export function accruedFunding(isLong: boolean, size: bigint, cumulativeFunding: bigint, lastFundingPayment: bigint): bigint {
  const payment = (size * (cumulativeFunding - lastFundingPayment)) / SCALE;
  return isLong ? -payment : payment;
}

/** `venue_margin`: margin to post for an open of `size` at `price` with the oracle at `oracle`. */
export function venueMargin(m: MarketState, isLong: boolean, size: bigint, price: bigint, oracle: bigint, leverage: bigint, now: number): bigint | null {
  const notionalFill = (size * price) / SCALE;
  const notionalOracle = (size * oracle) / SCALE;
  const base = requiredCollateral(m, notionalFill, leverage, now);
  if (base === null) return null;
  const [imr] = marginRatios(m, now);
  const imOracle = ceilDiv(notionalOracle * imr, BPS);
  const pnl = unrealizedPnl(isLong, size, price, oracle);
  const adverse = pnl < 0n ? -pnl : 0n;
  return max(base, imOracle) + adverse;
}

/** Skew `L − S` in contracts. */
export const skewOf = (m: MarketState) => m.longOpenInterest - m.shortOpenInterest;

/** `pool_view` (state/pool.rs): no skew premium once settled. */
export function poolView(pool: LpPoolAccount, m: MarketState, mark: bigint, now: number): pm.PoolView | null {
  return pm.poolViewRaw(
    pool,
    m.longOpenInterest,
    m.shortOpenInterest,
    m.cumulativeFunding,
    mark,
    BigInt(now),
    pool.params.redeemLossHaircutBps,
    isSettled(m) ? 0n : pool.params.skewScale,
    pool.params.maxImpactBps
  );
}

/** Drawdown against the peak as `touch_peak` leaves it (`drawdown_now`, D28). */
export function drawdownNow(pool: LpPoolAccount, v: pm.PoolView, now: number): bigint {
  const peak = pm.peakResets(v.nps, pool.npsPeak, pool.npsPeakAt, BigInt(now)) ? v.nps : pool.npsPeak;
  return pm.drawdownBps(v.nps, peak);
}

export const adlRequired = (pool: LpPoolAccount) => pool.pendingBadDebtLong > 0n || pool.pendingBadDebtShort > 0n;

// ---------------------------------------------------------------------------
// Opening against the house (instructions/open_position.rs, §4.4)
// ---------------------------------------------------------------------------

export interface HouseQuote {
  /** Execution price after skew impact (6 dp). */
  exec: bigint;
  /** `floor(size·exec, 1e6)`: the opened notional. */
  notional: bigint;
  /** Collateral moved into the position. */
  margin: bigint;
  /** Open fee, `ceil(notional·open_fee_bps, 1e4)`. */
  fee: bigint;
  /** Signed impact of the skew price against the oracle, bps. */
  impactBps: number;
}

/** Program error a quote or cap check would fail with, by name. */
export type QuoteResult<T> = { ok: true; value: T } | { ok: false; error: string };

/** `house_quote` (without the acceptable-price check). */
export function houseQuote(m: MarketState, pool: LpPoolAccount, isLong: boolean, size: bigint, leverage: bigint, oracle: bigint, now: number): QuoteResult<HouseQuote> {
  const delta = isLong ? size : -size;
  const exec = pm.skewPriceRaw(oracle, skewOf(m), delta, pool.params.skewScale, pool.params.maxImpactBps);
  if (exec === null) return { ok: false, error: 'MathOverflow' };
  if (exec <= 0n) return { ok: false, error: 'InvalidOraclePrice' };
  const notional = (size * exec) / SCALE;
  if (notional <= 0n) return { ok: false, error: 'PositionTooSmall' };
  const margin = venueMargin(m, isLong, size, exec, oracle, leverage, now);
  if (margin === null) return { ok: false, error: 'ExcessiveLeverage' };
  const fee = pm.ceilU(notional * pool.params.openFeeBps, BPS);
  const impactBps = oracle > 0n ? Number(((exec - oracle) * 1_000_000n) / oracle) / 100 : 0;
  return { ok: true, value: { exec, notional, margin, fee, impactBps } };
}

/** `check_open_caps`: the first cap a house open would break, or null. */
export function checkOpenCaps(
  m: MarketState,
  pool: LpPoolAccount,
  v: pm.PoolView,
  isLong: boolean,
  size: bigint,
  oracle: bigint,
  houseEntryNotional: bigint,
  openedNotional: bigint,
  now: number
): string | null {
  const p = pool.params;
  const after = isLong ? v.net + size : v.net - size;
  const abs = (x: bigint) => (x < 0n ? -x : x);
  if (abs(after) > abs(v.net)) {
    if (adlRequired(pool)) return 'PoolAdlPending';
    if (pm.ddFreeze(drawdownNow(pool, v, now), p.ddFreezeBps)) return 'PoolDrawdownFreeze';
    const nn = pm.ceilU(abs(after) * oracle, SCALE);
    if (!pm.netCapOk(nn, p.kNetBps, v.capNav)) return 'PoolExposureLimit';
    // Always on with the weekend bound (D43), not only inside W, against
    // capNAV net of the open's skew-premium shortfall
    if (pm.weekendBoundOn(p.weekendCapWindowSecs)) {
      const g = pm.gBps(p.weekendGapBps, m.initialMarginRatio, m.weekendInitialMarginRatio);
      const exec = pm.skewPriceRaw(oracle, skewOf(m), isLong ? size : -size, p.skewScale, p.maxImpactBps);
      const short = exec === null ? null : pm.premiumShortfall(v.net, after, oracle, exec, size, p.skewScale, p.maxImpactBps);
      if (short === null) return 'MathOverflow';
      if (!pm.weekendCapOk(nn, g, p.weekendLpLossBps, max(0n, v.capNav - short))) return 'PoolWeekendExposureLimit';
    }
  }
  if (!pm.perAccountOk(houseEntryNotional, openedNotional, p.perAccountCapBps, p.kNetBps, v.capNav)) return 'AccountExposureLimit';
  return null;
}

/** Largest `|net|` (contracts) with `ceil(|net|·oracle, 1e6) ≤ limitNotional`. */
function contractsWithin(limitNotional: bigint, oracle: bigint): bigint {
  return oracle > 0n ? (limitNotional * SCALE) / oracle : 0n;
}

export interface OpenHeadroom {
  /** Contracts a long / short house open can add before a net cap (net, or weekend while the bound is on, D43) binds. */
  long: bigint;
  short: bigint;
  /** Which net cap is the tighter one now. */
  netCap: 'net' | 'weekend';
  /** Opened notional (USDC) the per-account cap still allows; null when off. */
  account: bigint | null;
  /** Contracts the gross OI cap still allows per side. */
  oiLong: bigint;
  oiShort: bigint;
  /** Why net-raising opens are refused outright, if they are. */
  frozen: 'PoolAdlPending' | 'PoolDrawdownFreeze' | 'PoolHasNoLps' | null;
}

/**
 * How much a house open can add before a cap binds (§3), from the same
 * integer rules as `check_open_caps`. Net caps only bind opens that raise
 * `|net|`, so the side that reduces it has headroom up to `|net|` plus the cap.
 */
export function openHeadroom(m: MarketState, pool: LpPoolAccount, v: pm.PoolView, houseEntryNotional: bigint, oracle: bigint, now: number): OpenHeadroom {
  const p = pool.params;
  let limit = pm.bpsOf(p.kNetBps, v.capNav); // net cap, notional
  let netCap: OpenHeadroom['netCap'] = 'net';
  if (pm.weekendBoundOn(p.weekendCapWindowSecs)) {
    const g = pm.gBps(p.weekendGapBps, m.initialMarginRatio, m.weekendInitialMarginRatio);
    const weekend = pm.weekendTarget(p.weekendLpLossBps, v.capNav, g);
    if (weekend < limit) {
      limit = weekend;
      netCap = 'weekend';
    }
  }
  const maxNet = contractsWithin(limit, oracle);
  const absNet = v.net < 0n ? -v.net : v.net;
  const bound = max(absNet, maxNet);
  // The weekend cap counts the open's skew-premium shortfall (D43): past the
  // closed form, the largest accepted |net| by bisection (monotone)
  const weekendOk = (isLong: boolean, s: bigint): boolean => {
    if (!pm.weekendBoundOn(p.weekendCapWindowSecs) || p.skewScale === 0n || s === 0n) return true;
    const after = isLong ? v.net + s : v.net - s;
    const g = pm.gBps(p.weekendGapBps, m.initialMarginRatio, m.weekendInitialMarginRatio);
    const exec = pm.skewPriceRaw(oracle, skewOf(m), isLong ? s : -s, p.skewScale, p.maxImpactBps);
    const short = exec === null ? null : pm.premiumShortfall(v.net, after, oracle, exec, s, p.skewScale, p.maxImpactBps);
    if (short === null) return false;
    const nn = pm.ceilU((after < 0n ? -after : after) * oracle, SCALE);
    return pm.weekendCapOk(nn, g, p.weekendLpLossBps, max(0n, v.capNav - short));
  };
  const largest = (isLong: boolean, hi: bigint): bigint => {
    // Sizes that do not raise |net| (up to 2·|net| the other way) always pass
    let lo = (isLong ? v.net < 0n : v.net > 0n) ? min(hi, 2n * absNet) : 0n;
    if (weekendOk(isLong, hi)) return hi;
    while (lo < hi) {
      const mid = lo + (hi - lo + 1n) / 2n;
      if (weekendOk(isLong, mid)) lo = mid;
      else hi = mid - 1n;
    }
    return lo;
  };
  let frozen: OpenHeadroom['frozen'] = null;
  if (pool.lpShares === 0n) frozen = 'PoolHasNoLps';
  else if (adlRequired(pool)) frozen = 'PoolAdlPending';
  else if (pm.ddFreeze(drawdownNow(pool, v, now), p.ddFreezeBps)) frozen = 'PoolDrawdownFreeze';
  // Frozen: only opens that do not raise |net| (up to 2·|net| the other way)
  const cap = frozen ? absNet : bound;
  const long = frozen ? max(0n, cap - v.net) : largest(true, max(0n, cap - v.net));
  const short = frozen ? max(0n, cap + v.net) : largest(false, max(0n, cap + v.net));
  let account: bigint | null = null;
  if (p.perAccountCapBps > 0n) {
    const lim = (p.perAccountCapBps * p.kNetBps * v.capNav) / (BPS * BPS);
    account = max(0n, lim - houseEntryNotional);
  }
  const oiLong = max(0n, m.maxOpenInterest - m.longOpenInterest);
  const oiShort = max(0n, m.maxOpenInterest - m.shortOpenInterest);
  return { long, short, netCap, account, oiLong, oiShort, frozen };
}

/**
 * Worst price to accept for a trade quoted at `exec`: `slippageBps` worse,
 * rounded against the trader. Never 0, which the program reads as no bound.
 */
export function acceptablePrice(buy: boolean, exec: bigint, slippageBps: number): bigint {
  const s = BigInt(Math.max(0, Math.round(slippageBps)));
  if (buy) return pm.ceilU(exec * (BPS + s), BPS);
  const p = (exec * (s >= BPS ? 0n : BPS - s)) / BPS;
  return p > 0n ? p : 1n;
}

// ---------------------------------------------------------------------------
// Closing (instructions/close_position.rs, settle_close.rs, §4.5)
// ---------------------------------------------------------------------------

/** `close_prices`: settled → the settlement price; otherwise the skew price of the closing trade. */
export function closeExecPrice(m: MarketState, pool: LpPoolAccount, isLong: boolean, size: bigint, oracle: bigint): bigint | null {
  if (isSettled(m)) return m.settlementPrice;
  return pm.skewPriceRaw(oracle, skewOf(m), isLong ? -size : size, pool.params.skewScale, pool.params.maxImpactBps);
}

/**
 * Default bound for automated closes (TP/SL): the oracle the trigger saw,
 * `max_impact_bps` plus `extraBps` worse. The skew price never moves more
 * than `max_impact_bps` from the oracle, so this only fails when the oracle
 * itself moves `extraBps` against the trigger.
 */
export function triggerAcceptablePrice(m: MarketState, pool: LpPoolAccount, isLong: boolean, oracle: bigint, extraBps: number): bigint {
  const basis = isSettled(m) ? m.settlementPrice : oracle;
  return acceptablePrice(!isLong, basis, Number(pool.params.maxImpactBps) + extraBps);
}

/**
 * `check_close_caps`: a close that raises `|net|` must fit the net and (while
 * the weekend bound is on, D43) weekend caps against `B`, the weekend cap's
 * `B` net of the close's skew-premium shortfall at `exec` (default: the
 * close's skew price, `closeExecPrice`).
 */
// `_now` is kept for callers: the weekend cap no longer depends on the time (D43)
export function checkCloseCaps(m: MarketState, pool: LpPoolAccount, v: pm.PoolView, isLong: boolean, size: bigint, mark: bigint, _now: number, exec?: bigint): string | null {
  if (isSettled(m)) return null;
  const after = isLong ? v.net - size : v.net + size;
  const abs = (x: bigint) => (x < 0n ? -x : x);
  if (abs(after) <= abs(v.net)) return null;
  const nn = pm.ceilU(abs(after) * mark, SCALE);
  const p = pool.params;
  if (!pm.netCapOk(nn, p.kNetBps, v.b)) return 'PoolExposureLimit';
  if (pm.weekendBoundOn(p.weekendCapWindowSecs)) {
    const g = pm.gBps(p.weekendGapBps, m.initialMarginRatio, m.weekendInitialMarginRatio);
    const e = exec ?? pm.skewPriceRaw(mark, skewOf(m), isLong ? -size : size, p.skewScale, p.maxImpactBps);
    const short = e === null ? null : pm.premiumShortfall(v.net, after, mark, e, size, p.skewScale, p.maxImpactBps);
    if (short === null) return 'MathOverflow';
    if (!pm.weekendCapOk(nn, g, p.weekendLpLossBps, max(0n, v.b - short))) return 'PoolWeekendExposureLimit';
  }
  return null;
}

/**
 * Seconds a profitable close still has to wait (`in_profit_close_quiet`,
 * D23, D35): `profit_close_quiet_secs` after each session open, or after
 * settlement. 0 when not in the quiet time.
 */
export function profitCloseQuietLeft(m: MarketState, pool: LpPoolAccount, now: number): number {
  const q = Number(pool.params.profitCloseQuietSecs);
  if (isSettled(m)) {
    const at = Number(m.settledAt);
    return at > 0 ? Math.max(0, q - (now - at)) : 0;
  }
  const since = secondsSinceOpen(m.sessionSchedule, now);
  return since === null ? 0 : Math.max(0, q - since);
}

/** `ceil(opened_notional·taker_fee, 10_000)`: the close fee (D21). */
export function closeFee(size: bigint, openedPrice: bigint, takerFee: bigint): bigint {
  return pm.ceilU(pm.notionalFloor(size, openedPrice) * takerFee, BPS);
}

// ---------------------------------------------------------------------------
// Liquidation (§6.1, §6.2)
// ---------------------------------------------------------------------------

export type LiquidationStage = 'none' | 'partial' | 'backstop';

/**
 * Which liquidation applies at `mark`: none while `E·1e4 ≥ mmr·N_mark`;
 * stage 1 (partial, owner keeps the leftover) above the backstop line
 * `mmr·backstop_threshold_bps/1e4`; stage 2 (full close) below it.
 */
export function liquidationStage(equity: bigint, size: bigint, mark: bigint, mmrBps: bigint, backstopThresholdBps: bigint): LiquidationStage {
  const n = (size * mark) / SCALE;
  const liquidatable = n === 0n ? equity < 0n : equity * BPS < mmrBps * n;
  if (!liquidatable) return 'none';
  const backstop = (mmrBps * backstopThresholdBps) / BPS;
  return equity * BPS >= backstop * n ? 'partial' : 'backstop';
}

// ---------------------------------------------------------------------------
// LP flows (instructions/lp_pool.rs, §9.5)
// ---------------------------------------------------------------------------

export interface LpGates {
  deposit: string[];
  request: string[];
  withdraw: string[];
  /** Funding must be cranked first; the vault cranks it in its own transaction (utils/fundingCrank.ts). */
  needsFundingCrank: boolean;
  /** Reopen quiet `Q` and pre-close window `W` in force now. */
  inQuiet: boolean;
  inWindow: boolean;
  /** Settled, and inside the settled quiet time (D41): withdrawals wait. */
  settledQuiet: boolean;
}

/** Why each LP flow would fail now (error names), in the program's order. */
export function lpGates(m: MarketState, pool: LpPoolAccount, v: pm.PoolView | null, now: number): LpGates {
  const settled = isSettled(m);
  const p = pool.params;
  const open = isOpen(m.sessionSchedule, now);
  const since = secondsSinceOpen(m.sessionSchedule, now);
  const inQuiet = since !== null && since < Number(p.lpReopenQuietSecs);
  const inWindow = inPreCloseWindow(m.sessionSchedule, now, Number(p.weekendCapWindowSecs));
  const current = fundingCurrent(m.sessionSchedule, settled, Number(m.lastFundingTime), m.fundingCarrySecs, Number(m.fundingInterval), now);
  const deposit: string[] = [];
  const request: string[] = [];
  const withdraw: string[] = [];
  if (m.isPaused) deposit.push('MarketPaused');
  if (!open) deposit.push('MarketClosed');
  if (inQuiet) deposit.push('LpFlowWindowClosed');
  if (v && v.navMint <= 0n && pool.lpShares > 0n) deposit.push('PoolInsolvent');
  if (adlRequired(pool)) deposit.push('PoolAdlPending');
  if (!settled) {
    if (!open) request.push('MarketClosed');
    if (!open) withdraw.push('MarketClosed');
    if (inWindow || inQuiet) withdraw.push('LpFlowWindowClosed');
  }
  // D41: a settled market's withdrawals wait out the settled quiet time
  const settledQuiet = settled && pm.settledWithdrawQuiet(m.settledAt, BigInt(now), p.profitCloseQuietSecs, p.lpReopenQuietSecs);
  if (settledQuiet) withdraw.push('LpFlowWindowClosed');
  if (adlRequired(pool)) withdraw.push('PoolAdlPending');
  if (!settled && v && pm.ddWithdrawBlocked(drawdownNow(pool, v, now), p.ddWithdrawBlockBps)) withdraw.push('PoolWithdrawalsBlocked');
  return { deposit, request, withdraw, needsFundingCrank: !current, inQuiet, inWindow, settledQuiet };
}

export interface WithdrawalRequest {
  pendingShares: bigint;
  /** Value fixed at request time; the payout is the lower of it and the value now. */
  requestedValue: bigint;
  /** Value of the pending shares at `NAV_redeem` now. */
  valueNow: bigint;
  /** What `lp_withdraw` would pay now. */
  payout: bigint;
  unlockAt: number;
  /** Last moment `lp_withdraw` can claim it (null: never lapses). */
  claimDeadline: number | null;
  state: 'none' | 'waiting' | 'claimable' | 'lapsed';
}

/** A position's withdrawal request at `now` (§9.4 steps 1–2 and 6). */
export function withdrawalRequest(m: MarketState, pool: LpPoolAccount, pos: LpPositionAccount, v: pm.PoolView | null, now: number): WithdrawalRequest {
  const valueNow = v ? pm.redeemValue(pos.pendingShares, pool.lpShares, v.navRedeem) : 0n;
  const payout = valueNow < pos.requestedValue ? valueNow : pos.requestedValue;
  const unlockAt = Number(pos.unlockAt);
  const base = { pendingShares: pos.pendingShares, requestedValue: pos.requestedValue, valueNow, payout, unlockAt };
  if (pos.pendingShares === 0n) return { ...base, claimDeadline: null, state: 'none' };
  let claimDeadline: number | null = null;
  let lapsed = false;
  if (!isSettled(m) && pos.claimWindowSecs > 0n) {
    const p = pool.params;
    const window = Number(pm.effectiveClaimWindowSecs(pos.claimWindowSecs, p.weekendCapWindowSecs, p.lpReopenQuietSecs));
    // The request lapses once more than `window` open seconds have passed
    // since unlock; the deadline is the last in-session second within it.
    claimDeadline = timeAfterOpenSeconds(m.sessionSchedule, unlockAt, window);
    lapsed = now >= unlockAt && openSecondsBetween(m.sessionSchedule, unlockAt, now) > window;
  }
  const state = now < unlockAt ? 'waiting' : lapsed ? 'lapsed' : 'claimable';
  return { ...base, claimDeadline, state };
}

/**
 * `lp_withdraw` steps 7–8: cash must cover the payout, and (unless settled)
 * the hard-net line and, with the weekend bound on, `G·net ≤ α·B'` must hold
 * after it (D43).
 */
export function withdrawPostChecks(m: MarketState, pool: LpPoolAccount, v: pm.PoolView, payout: bigint): string | null {
  return withdrawRefusal(m, pool, v, payout)?.error ?? null;
}

export interface WithdrawRefusal {
  error: 'PoolCashInsufficient' | 'PoolCapWouldBreach';
  /** Which line `PoolCapWouldBreach` is about. */
  line: 'cash' | 'hard_net' | 'weekend';
}

/** `withdrawPostChecks` with the line that refused it, for the LP vault's explanation. */
export function withdrawRefusal(m: MarketState, pool: LpPoolAccount, v: pm.PoolView, payout: bigint): WithdrawRefusal | null {
  if (payout > pool.cash) return { error: 'PoolCashInsufficient', line: 'cash' };
  if (!isSettled(m)) {
    const p = pool.params;
    const bAfter = v.navRedeem - payout > 0n ? v.navRedeem - payout : 0n;
    if (!pm.hardNetOk(v.netNotional, p.hardNetBps, bAfter)) return { error: 'PoolCapWouldBreach', line: 'hard_net' };
    const g = pm.gBps(p.weekendGapBps, m.initialMarginRatio, m.weekendInitialMarginRatio);
    if (pm.weekendBoundOn(p.weekendCapWindowSecs) && !pm.weekendCapOk(v.netNotional, g, p.weekendLpLossBps, bAfter)) {
      return { error: 'PoolCapWouldBreach', line: 'weekend' };
    }
  }
  return null;
}

/** Shares `lp_deposit` mints to the depositor for `amount` (the first deposit locks `LP_DEAD_SHARES`). */
export function depositShares(pool: LpPoolAccount, v: pm.PoolView, amount: bigint): bigint | null {
  if (pool.lpShares === 0n) return amount > pm.LP_DEAD_SHARES ? amount - pm.LP_DEAD_SHARES : null;
  return pm.sharesForDeposit(amount, pool.lpShares, v.navMint);
}

/** `PoolParams::validate` (state/pool.rs): false means `InvalidPoolParams`. */
export function validatePoolParams(
  p: PoolParams,
  m: Pick<MarketState, 'maintenanceMarginRatio' | 'takerFee' | 'initialMarginRatio' | 'weekendInitialMarginRatio'>
): string[] {
  const bad: string[] = [];
  const bps = (x: bigint) => x <= 10_000n;
  const need = (ok: boolean, why: string) => {
    if (!ok) bad.push(why);
  };
  need(p.kNetBps <= 100_000n, 'k_net_bps ≤ 100,000');
  need(p.hardNetBps === 0n || p.hardNetBps >= p.kNetBps, 'hard_net_bps is 0 or ≥ k_net_bps');
  need(p.adlNetTargetBps <= p.kNetBps, 'adl_net_target_bps ≤ k_net_bps');
  need(p.perAccountCapBps <= 10_000n, 'per_account_cap_bps ≤ 10,000');
  need(p.weekendLpLossBps <= 10_000n, 'weekend_lp_loss_bps ≤ 10,000');
  need(p.weekendGapBps <= 10_000n, 'weekend_gap_bps ≤ 10,000');
  need(bps(p.redeemLossHaircutBps), 'redeem_loss_haircut_bps ≤ 10,000');
  need(p.weekendCapWindowSecs <= 5n * 86_400n, 'weekend_cap_window_secs ≤ 5 days');
  need(p.withdrawalDelaySecs <= 30n * 86_400n, 'withdrawal_delay_secs ≤ 30 days');
  need(p.lpReopenQuietSecs <= 86_400n, 'lp_reopen_quiet_secs ≤ 1 day');
  need(p.profitCloseQuietSecs <= 3_600n, 'profit_close_quiet_secs ≤ 3,600');
  need(
    pm.lpGatedSecsPerWeek(p.weekendCapWindowSecs, p.lpReopenQuietSecs) + pm.LP_UNGATED_SECS <= pm.WEEK_OPEN_SECS,
    'W + 5·Q + 1 day ≤ 115 h (a day of ungated LP exits each week)'
  );
  need(
    p.withdrawalClaimWindowSecs === 0n || p.withdrawalClaimWindowSecs >= pm.minClaimWindowSecs(p.weekendCapWindowSecs, p.lpReopenQuietSecs),
    'withdrawal_claim_window_secs is 0 or ≥ W + 5·Q + 1 day'
  );
  need(
    !pm.weekendBoundOn(p.weekendCapWindowSecs) ||
      pm.kNetWithinWeekendBound(p.kNetBps, pm.gBps(p.weekendGapBps, m.initialMarginRatio, m.weekendInitialMarginRatio), p.weekendLpLossBps),
    'with the weekend bound on (W > 0): k_net_bps·G ≤ weekend_lp_loss_bps·10,000 (D43)'
  );
  need(bps(p.ddFreezeBps) && bps(p.ddAdlBps) && bps(p.ddWithdrawBlockBps), 'drawdown lines ≤ 10,000');
  need(p.ddFreezeBps === 0n || p.ddAdlBps === 0n || p.ddFreezeBps <= p.ddAdlBps, 'dd_freeze_bps ≤ dd_adl_bps');
  need(p.openFeeBps <= 1_000n && p.maxImpactBps <= 1_000n && bps(p.treasuryFeeBps), 'open_fee and max_impact ≤ 1,000, treasury_fee ≤ 10,000');
  need(p.backstopThresholdBps >= 1n && p.backstopThresholdBps <= 10_000n, 'backstop_threshold_bps in 1..=10,000');
  need(p.minLiqSliceBps >= 1n && p.minLiqSliceBps <= 10_000n, 'min_liq_slice_bps in 1..=10,000');
  need(bps(p.liqKeeperShareBps) && bps(p.lpLossCapWindowBps), 'liq_keeper_share and lp_loss_cap_window ≤ 10,000');
  need(p.liqPenaltyBps * 10_000n < m.maintenanceMarginRatio * p.backstopThresholdBps, 'liq_penalty_bps·1e4 < MMR·backstop_threshold_bps');
  const keeper = p.liqKeeperShareBps < 10_000n ? p.liqKeeperShareBps : 10_000n;
  need(
    p.backstopThresholdBps >= 10_000n || p.liqPenaltyBps * (10_000n - keeper) >= (p.maxImpactBps + m.takerFee) * 10_000n,
    'self-liquidation must cost more than impact + taker fee'
  );
  return bad;
}

// ---------------------------------------------------------------------------
// Admin (instructions/pool_admin.rs, update_market_config.rs)
// ---------------------------------------------------------------------------

/** `MarketConfigParams`: every field is written, so start from the market as it is. */
export interface MarketConfigParams {
  isPaused: boolean;
  sessionSchedule: number;
  preCloseWindowSecs: number;
  weekendInitialMarginRatio: number;
  weekendMaintenanceMarginRatio: number;
  maxConfidenceBps: number;
  minOraclePrice: bigint;
  /** Contracts per side (6 dp); `0` keeps the current value (D6). */
  maxOpenInterest: bigint;
}

export function marketConfigParams(m: MarketState, changes: Partial<MarketConfigParams> = {}): MarketConfigParams {
  return {
    isPaused: m.isPaused,
    sessionSchedule: m.sessionSchedule,
    preCloseWindowSecs: m.preCloseWindowSecs,
    weekendInitialMarginRatio: Number(m.weekendInitialMarginRatio),
    weekendMaintenanceMarginRatio: Number(m.weekendMaintenanceMarginRatio),
    maxConfidenceBps: m.maxConfidenceBps,
    minOraclePrice: m.minOraclePrice,
    maxOpenInterest: 0n,
    ...changes,
  };
}

export interface PoolStatus {
  hasLps: boolean;
  adlRequired: boolean;
  /** Drawdown now, bps. */
  ddBps: bigint;
  /** Net-raising house opens frozen by the drawdown line. */
  ddFreeze: boolean;
  /** Exposure-ADL triggers (§7.2): net over the hard line, drawdown, weekend force-reduce. */
  nTrig: boolean;
  dTrig: boolean;
  wTrig: boolean;
  /** `U > cash`: profit payouts are haircut pro rata (§2.8). */
  solvencyHaircut: boolean;
  unvested: bigint;
  /** Seconds until a pending params change can be applied; null when none is pending. */
  paramsApplyIn: number | null;
}

export function poolStatus(m: MarketState, pool: LpPoolAccount, v: pm.PoolView | null, now: number): PoolStatus {
  const p = pool.params;
  const dd = v ? drawdownNow(pool, v, now) : 0n;
  const inW = inPreCloseWindow(m.sessionSchedule, now, Number(p.weekendCapWindowSecs));
  const g = pm.gBps(p.weekendGapBps, m.initialMarginRatio, m.weekendInitialMarginRatio);
  return {
    hasLps: pool.lpShares > 0n,
    adlRequired: adlRequired(pool),
    ddBps: dd,
    ddFreeze: pm.ddFreeze(dd, p.ddFreezeBps),
    nTrig: !!v && !isSettled(m) && pm.nTrig(v.netNotional, p.hardNetBps, v.b),
    dTrig: !!v && !isSettled(m) && pm.dTrig(dd, p.ddAdlBps),
    wTrig: !!v && !isSettled(m) && pm.wTrig(inW, v.netNotional, g, p.weekendLpLossBps, v.b),
    solvencyHaircut: !!v && v.u > 0n && v.u > pool.cash,
    unvested: pm.unvested(pool.vestAmount, pool.vestStart, pool.vestEnd, BigInt(now)),
    paramsApplyIn: pool.pendingParamsAt === 0n ? null : Math.max(0, Number(pool.pendingParamsAt) - now),
  };
}

/** Timelock a proposal of `next` would get now (§5); 0 before the pool is seeded (applies at once). */
export function proposalTimelock(pool: LpPoolAccount, next: PoolParams): bigint {
  if (pool.lpShares === 0n) return 0n;
  return pm.paramsTimelockSecs(
    pool.params.withdrawalDelaySecs,
    next.withdrawalDelaySecs,
    pool.params.weekendCapWindowSecs,
    next.weekendCapWindowSecs
  );
}
