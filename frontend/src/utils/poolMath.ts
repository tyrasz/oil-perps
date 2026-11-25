// LP pool math, mirroring programs/perps-core/src/pool_math.rs function by
// function (docs/lp-pool.md §2, §3, §7, §8, §9). The program is the source
// of truth; keep this file in step with it.
//
// Everything is bigint so the integer rounding matches the program exactly:
// prices, USDC and contract sizes are 6 dp, ratios are bps. Signed division
// floors (`divEuclid`), `ceilS` is its ceiling twin. Where the program
// returns `None` on arithmetic overflow this returns `null`; the checked
// i128 steps are mirrored with `chk`.

export const MAX_OPEN_INTEREST = 1_000_000_000_000_000n;
export const MAX_FUNDING_CATCHUP_PERIODS = 168n;
/** Open seconds (§7.4). */
export const ADL_WRITE_OFF_SECS = 86_400n;
export const ADL_FLOOR_START_BPS = 1_000_000n;
/** Open seconds (§7.2). */
export const ADL_FLOOR_DECAY_SECS = 21_600n;
/** Wall clock (§2.6). */
export const LOSS_WINDOW_SECS = 86_400n;
/** Wall clock (§2.2). */
export const DD_WINDOW_SECS = 7n * 86_400n;
export const MIN_POOL_TIMELOCK_SECS = 86_400n;
export const PARAMS_TIMELOCK_MARGIN_SECS = 5n * 86_400n;
export const LP_DEAD_SHARES = 1_000_000n;
export const LP_CLASS_COMMON = 0;

const SCALE = 1_000_000n;
const BPS = 10_000n;

const U64_MAX = (1n << 64n) - 1n;
const U128_MAX = (1n << 128n) - 1n;
const I128_MAX = (1n << 127n) - 1n;
const I128_MIN = -(1n << 127n);
const I64_MAX = (1n << 63n) - 1n;
const I64_MIN = -(1n << 63n);

class Overflow extends Error {}

/** A checked i128 step: throws (caught as `null`) where Rust's `checked_*` would give `None`. */
function chk(x: bigint): bigint {
  if (x > I128_MAX || x < I128_MIN) throw new Overflow();
  return x;
}

function orNull<T>(f: () => T): T | null {
  try {
    return f();
  } catch (e) {
    if (e instanceof Overflow) return null;
    throw e;
  }
}

const max = (a: bigint, b: bigint) => (a > b ? a : b);
const min = (a: bigint, b: bigint) => (a < b ? a : b);
const abs = (a: bigint) => (a < 0n ? -a : a);
const clamp = (x: bigint, lo: bigint, hi: bigint) => (x < lo ? lo : x > hi ? hi : x);
const satU64 = (x: bigint) => (x > U64_MAX ? U64_MAX : x);

/** Rust `div_euclid` for `b > 0`: floor division. */
export function divEuclid(a: bigint, b: bigint): bigint {
  const q = a / b;
  return a % b < 0n ? q - 1n : q;
}

/** `ceil(a / b)` for `b > 0`, on signed integers. */
export function ceilS(a: bigint, b: bigint): bigint {
  return -divEuclid(-a, b);
}

/** `ceil(a / b)` for `b > 0`, unsigned. */
export function ceilU(a: bigint, b: bigint): bigint {
  return a / b + (a % b === 0n ? 0n : 1n);
}

/** `floor(a·b / c)` exactly. `null` if `c == 0` or the quotient does not fit in u128. */
export function mulDivFloor(a: bigint, b: bigint, c: bigint): bigint | null {
  if (c === 0n) return null;
  const q = (a * b) / c;
  return q > U128_MAX ? null : q;
}

/** `a1·a2 <= b1·b2`, exact. */
export function leMul(a1: bigint, a2: bigint, b1: bigint, b2: bigint): boolean {
  return a1 * a2 <= b1 * b2;
}

// ---------------------------------------------------------------------------
// Valuation (§2.2)
// ---------------------------------------------------------------------------

/** The LpPool fields the valuation reads. */
export interface PoolState {
  cash: bigint;
  longEntrySum: bigint;
  shortEntrySum: bigint;
  longFundingSum: bigint;
  shortFundingSum: bigint;
  openPositions: bigint;
  lpShares: bigint;
  pendingWithdrawShares: bigint;
  npsPeak: bigint;
  vestAmount: bigint;
  vestStart: bigint;
  vestEnd: bigint;
}

export const EMPTY_POOL_STATE: PoolState = {
  cash: 0n,
  longEntrySum: 0n,
  shortEntrySum: 0n,
  longFundingSum: 0n,
  shortFundingSum: 0n,
  openPositions: 0n,
  lpShares: 0n,
  pendingWithdrawShares: 0n,
  npsPeak: 0n,
  vestAmount: 0n,
  vestStart: 0n,
  vestEnd: 0n,
};

export interface PoolView {
  /** Traders' net unrealized PnL including funding (floored). */
  u: bigint;
  /** `L − S` in contracts; the pool holds `−net`. */
  net: bigint;
  /** `ceil(|net|·mark, 1e6)`, rounded against the opener. */
  netNotional: bigint;
  /** Upper bound of LP NAV; depositors pay it. */
  navMint: bigint;
  /** Lower bound of LP NAV; withdrawers get it. */
  navRedeem: bigint;
  /** `max(NAV_redeem, 0)`: capital still in the pool, pending shares included. */
  b: bigint;
  /** `B` scaled by the non-pending share of `lp_shares`. */
  capNav: bigint;
  /** Drawdown of `nps` against `nps_peak`, bps. */
  ddBps: bigint;
  /** Per-share mark NAV, scale 1e6 (saturates at u64::MAX). */
  nps: bigint;
}

/** Value swept or donated that has not vested into `NAV_redeem` yet (§10.3). */
export function unvested(vestAmount: bigint, vestStart: bigint, vestEnd: bigint, now: bigint): bigint {
  if (vestEnd <= now || vestEnd <= vestStart) return 0n;
  const left = vestEnd - now;
  const total = vestEnd - vestStart;
  return (vestAmount * min(left, total)) / total;
}

/** `0` when there is no peak or `nps` is at/above it, else the drop in bps. */
export function drawdownBps(nps: bigint, npsPeak: bigint): bigint {
  if (npsPeak === 0n || nps >= npsPeak) return 0n;
  return ((npsPeak - nps) * BPS) / npsPeak;
}

/** Per-share NAV (scale 1e6) of a non-negative `nav` over `lpShares`. */
export function navPerShare(nav: bigint, lpShares: bigint): bigint {
  if (lpShares === 0n || nav <= 0n) return 0n;
  const v = mulDivFloor(nav, SCALE, lpShares);
  return v === null ? U64_MAX : satU64(v);
}

/**
 * The refundable skew premium (D39):
 * `min(ceil(mark·net²/(2·skew_scale·1e6)), ceil(net_notional·max_impact/1e4))`;
 * `0` with no price impact.
 */
export function unwindPremium(net: bigint, mark: bigint, skewScale: bigint, maxImpactBps: bigint): bigint | null {
  const n = abs(net);
  if (skewScale === 0n || n === 0n) return 0n;
  // The program's checked u128 products
  if (n * mark > U128_MAX || n * n > U128_MAX) return null;
  const nn = ceilU(n * mark, SCALE);
  if (nn * maxImpactBps > U128_MAX) return null;
  const cap = ceilU(nn * maxImpactBps, BPS);
  const den = 2n * skewScale * SCALE;
  const quad = ceilU(n * n * mark, den);
  return min(quad > U128_MAX ? U128_MAX : quad, cap);
}

/**
 * `premium_shortfall` (D43): what a net-raising trade of `size` at `exec`
 * can take out of `B` through the skew premium, the rise in the unwind
 * premium beyond the premium paid, `max(0, Δx − floor(size·|exec−oracle|/1e6))`.
 * 0 in the quadratic branch up to rounding; up to `o·skew_scale·max_impact²/2`
 * across the clamp. `null` on overflow.
 */
export function premiumShortfall(netBefore: bigint, netAfter: bigint, oracle: bigint, exec: bigint, size: bigint, skewScale: bigint, maxImpactBps: bigint): bigint | null {
  const x0 = unwindPremium(netBefore, oracle, skewScale, maxImpactBps);
  const x1 = unwindPremium(netAfter, oracle, skewScale, maxImpactBps);
  if (x0 === null || x1 === null) return null;
  const paid = (size * abs(exec - oracle)) / SCALE;
  if (size * abs(exec - oracle) > U128_MAX) return null;
  const d = x1 - x0 - paid;
  return d > 0n ? d : 0n;
}

/**
 * O(1) pool valuation (§2.2). `null` on arithmetic overflow (`MathOverflow`).
 * `hBps` is `redeem_loss_haircut_bps`; `skewScale` and `maxImpactBps` price
 * the refundable skew premium, which `NAV_redeem` and the mark NAV count as
 * owed to traders and `NAV_mint` does not.
 */
export function poolViewRaw(
  p: PoolState,
  longOi: bigint,
  shortOi: bigint,
  cumulativeFunding: bigint,
  mark: bigint,
  now: bigint,
  hBps: bigint,
  skewScale: bigint,
  maxImpactBps: bigint
): PoolView | null {
  return orNull(() => {
    const l = longOi;
    const s = shortOi;
    const m = mark;
    const f = cumulativeFunding;
    const les = chk(p.longEntrySum);
    const ses = chk(p.shortEntrySum);

    const pnlLong = chk(chk(m * l) - les);
    const pnlShort = chk(chk(m * s) - ses);
    const pnlNum = chk(pnlLong - pnlShort);
    const fundLong = chk(chk(f * l) - p.longFundingSum);
    const fundShort = chk(chk(f * s) - p.shortFundingSum);
    const fundNum = chk(fundShort - fundLong);
    const u = divEuclid(chk(pnlNum + fundNum), SCALE);

    const net = l - s;
    const slack = 2n * p.openPositions + 1n;
    const unv = unvested(p.vestAmount, p.vestStart, p.vestEnd, now);
    const cash = p.cash;
    const premium = unwindPremium(net, mark, skewScale, maxImpactBps);
    if (premium === null) throw new Overflow();
    const x = chk(premium);
    const uLp = chk(u + x);
    const gain = max(uLp, 0n);
    const loss = max(-uLp, 0n);

    const navMint = chk(chk(cash - u) + slack);
    const countedLoss = chk(loss * hBps) / BPS;
    const navRedeem = chk(chk(chk(chk(cash - unv) - gain) + countedLoss) - slack);
    const navMark = chk(chk(cash - unv) - uLp);

    const nps = navPerShare(navMark, p.lpShares);
    const ddBps = drawdownBps(nps, p.npsPeak);

    const netNotional = ceilU(abs(net) * mark, SCALE);
    const b = max(navRedeem, 0n);
    let capNav = 0n;
    if (p.lpShares !== 0n) {
      const live = p.lpShares > p.pendingWithdrawShares ? p.lpShares - p.pendingWithdrawShares : 0n;
      const c = mulDivFloor(b, live, p.lpShares);
      if (c === null) throw new Overflow();
      capNav = c;
    }
    return { u, net, netNotional, navMint, navRedeem, b, capNav, ddBps, nps };
  });
}

/** §2.2 peak tracking: whether `touch_peak` replaces the peak. */
export function peakResets(nps: bigint, npsPeak: bigint, npsPeakAt: bigint, now: bigint): boolean {
  return nps >= npsPeak || now >= npsPeakAt + DD_WINDOW_SECS;
}

// ---------------------------------------------------------------------------
// Skew price (§2.4)
// ---------------------------------------------------------------------------

/**
 * SIP-279 average premium. `delta` is `+q` for a buy (open long / close
 * short) and `−q` for a sell. Buys round up, sells down; the premium is
 * clamped to `±max_impact_bps` of the oracle.
 */
export function skewPriceRaw(oracle: bigint, skewBefore: bigint, delta: bigint, skewScale: bigint, maxImpactBps: bigint): bigint | null {
  return orNull(() => {
    const o = oracle;
    if (skewScale === 0n) return o;
    const num = chk(o * chk(chk(skewBefore * 2n) + delta));
    const den = 2n * skewScale;
    const prem = delta > 0n ? ceilS(num, den) : divEuclid(num, den);
    const cap = (o * maxImpactBps) / BPS;
    return o + clamp(prem, -cap, cap);
  });
}

// ---------------------------------------------------------------------------
// Fees and the deficit waterfall (§2.5, §2.6)
// ---------------------------------------------------------------------------

/** `[treasury, rest]` of a net fee `x`. */
export function feeSplit(x: bigint, treasuryFeeBps: bigint): [bigint, bigint] {
  const t = (x * treasuryFeeBps) / BPS;
  return [t, x - t];
}

/** `[toInsurance, toCash]`: insurance first until the target, then LPs. */
export function insuranceSplit(insurance: bigint, target: bigint, y: bigint): [bigint, bigint] {
  const room = target > insurance ? target - insurance : 0n;
  const toIns = min(y, room);
  return [toIns, y - toIns];
}

/** Debt-weighted booking time (§2.6 step 4) after adding `add > 0` to `old`. */
export function weightedSince(since: bigint, old: bigint, now: bigint, add: bigint): bigint {
  if (old === 0n) return now;
  return divEuclid(since * old + now * add, old + add);
}

/** LP share of a loss window still available: `floor(nav·bps/1e4) − used`. */
export function lpWindowCap(lossWindowNav: bigint, lpLossCapWindowBps: bigint, used: bigint): bigint {
  const cap = satU64((lossWindowNav * lpLossCapWindowBps) / BPS);
  return cap > used ? cap - used : 0n;
}

/** Insurance claim room in the window; `cap == 0` is unlimited. */
export function insuranceWindowCap(cap: bigint, used: bigint): bigint {
  if (cap === 0n) return U64_MAX;
  return cap > used ? cap - used : 0n;
}

// ---------------------------------------------------------------------------
// Paying profit (§2.8)
// ---------------------------------------------------------------------------

export interface Payout {
  /** Debited from cash. */
  paid: bigint;
  /** `P − P1`: cut because the pool is insolvent at mark. */
  solvencyHc: bigint;
  /** Repays the opposite side's pending bad debt; stays in cash. */
  badDebtHc: bigint;
  /** Liquidations only: the part cash could not pay. */
  cashHc: bigint;
}

/** Pure quote; `null` means `PoolCashInsufficient` (only when `allowShort` is false). */
export function quoteProfitRaw(u: bigint, cash: bigint, oppPending: bigint, p: bigint, allowShort: boolean): Payout | null {
  const p1 = u > 0n && u > cash ? (p * cash) / u : p;
  const hc = min(oppPending, p1);
  const p2 = p1 - hc;
  let paid = p2;
  let cashHc = 0n;
  if (p2 > cash) {
    if (!allowShort) return null;
    paid = cash;
    cashHc = p2 - cash;
  }
  return { paid, solvencyHc: p - p1, badDebtHc: hc, cashHc };
}

// ---------------------------------------------------------------------------
// Caps and triggers (§3, §7.2)
// ---------------------------------------------------------------------------

/** `G_bps`: the override, else `max(IM, weekend IM)`. */
export function gBps(weekendGapBps: bigint, initialMarginRatio: bigint, weekendInitialMarginRatio: bigint): bigint {
  return weekendGapBps !== 0n ? weekendGapBps : max(initialMarginRatio, weekendInitialMarginRatio);
}

/** Net cap: `net_notional·10_000 ≤ k_net_bps·capital`. */
export function netCapOk(netNotional: bigint, kNetBps: bigint, capital: bigint): boolean {
  return leMul(netNotional, BPS, kNetBps, capital);
}

/** Weekend net cap: `net_notional·G ≤ α·capital`; skipped when `G == 0`. */
export function weekendCapOk(netNotional: bigint, g: bigint, alphaBps: bigint, capital: bigint): boolean {
  return g === 0n || leMul(netNotional, g, alphaBps, capital);
}

/** D43: the weekend bound is on with `weekend_cap_window_secs > 0` (always-on weekend cap, LP withdraw check). */
export function weekendBoundOn(weekendCapWindowSecs: bigint): boolean {
  return weekendCapWindowSecs > 0n;
}

/** D43 validate rule with the bound on: `k_net·G ≤ α·10_000`. */
export function kNetWithinWeekendBound(kNetBps: bigint, g: bigint, alphaBps: bigint): boolean {
  return kNetBps * g <= alphaBps * BPS;
}

/** Per-account cap: `(house + opened)·1e8 ≤ per_account·k_net·capNAV`; `perAccountCapBps == 0` is off. */
export function perAccountOk(houseEntryNotional: bigint, openedNotional: bigint, perAccountCapBps: bigint, kNetBps: bigint, capNav: bigint): boolean {
  if (perAccountCapBps === 0n) return true;
  return leMul(houseEntryNotional + openedNotional, BPS * BPS, perAccountCapBps * kNetBps, capNav);
}

/** Exposure-ADL net trigger (also the LP-exit hard line): `hard_net > 0 && nn·1e4 > hard·B`. */
export function nTrig(netNotional: bigint, hardNetBps: bigint, b: bigint): boolean {
  return hardNetBps > 0n && !leMul(netNotional, BPS, hardNetBps, b);
}

/** Exposure-ADL drawdown trigger. */
export function dTrig(ddBps: bigint, ddAdlBps: bigint): boolean {
  return ddAdlBps > 0n && ddBps >= ddAdlBps;
}

/** Weekend force-reduce trigger (D20): inside `W`, `nn·G > α·B`. */
export function wTrig(inWindow: boolean, netNotional: bigint, g: bigint, alphaBps: bigint, b: bigint): boolean {
  return inWindow && g > 0n && !leMul(netNotional, g, alphaBps, b);
}

/** Drawdown freeze of net-raising house opens (`ddFreezeBps == 0` is off). */
export function ddFreeze(ddBps: bigint, ddFreezeBps: bigint): boolean {
  return ddFreezeBps > 0n && ddBps >= ddFreezeBps;
}

/** LP withdraw drawdown block (`ddWithdrawBlockBps == 0` is off). */
export function ddWithdrawBlocked(ddBps: bigint, ddWithdrawBlockBps: bigint): boolean {
  return ddWithdrawBlockBps > 0n && ddBps >= ddWithdrawBlockBps;
}

/** `lp_withdraw` post-check: `hard_net == 0 || net == 0 || nn·1e4 ≤ hard·B'`. */
export function hardNetOk(netNotional: bigint, hardNetBps: bigint, bAfter: bigint): boolean {
  return hardNetBps === 0n || leMul(netNotional, BPS, hardNetBps, bAfter);
}

/** `floor(bps·B / 10_000)`: the net-mode and drawdown-mode ADL targets. */
export function bpsOf(bps: bigint, b: bigint): bigint {
  return mulDivFloor(b, bps, BPS) ?? U128_MAX;
}

/** Weekend force-reduce target `floor(α·B / G)`; `G == 0` gives u128::MAX. */
export function weekendTarget(alphaBps: bigint, b: bigint, g: bigint): bigint {
  if (g === 0n) return U128_MAX;
  return mulDivFloor(b, alphaBps, g) ?? U128_MAX;
}

export const TRIGGER_NET = 1;
export const TRIGGER_DRAWDOWN = 2;
export const TRIGGER_WEEKEND = 4;

// ---------------------------------------------------------------------------
// ADL (§7.2)
// ---------------------------------------------------------------------------

/** Hyperliquid's `(m/entry)·(notional/equity)` in bps; saturates at u128::MAX; 0 when `E ≤ 0`. */
export function adlScore(long: boolean, mark: bigint, entry: bigint, nMark: bigint, equity: bigint): bigint {
  if (equity <= 0n) return 0n;
  const [numPx, denPx] = long ? [mark, entry] : [entry, mark];
  if (denPx === 0n) return U128_MAX;
  const a = nMark * BPS;
  if (a > U128_MAX) return U128_MAX;
  const q = mulDivFloor(a, numPx, denPx);
  return q === null ? U128_MAX : q / equity;
}

/** Mode-0 floor after `dt` open seconds: decays linearly from `ADL_FLOOR_START_BPS` to 0. */
export function adlFloorNow(dt: bigint): bigint {
  const d = clamp(dt, 0n, ADL_FLOOR_DECAY_SECS);
  return (ADL_FLOOR_START_BPS * (ADL_FLOOR_DECAY_SECS - d)) / ADL_FLOOR_DECAY_SECS;
}

// ---------------------------------------------------------------------------
// Funding catch-up (§8 step 5)
// ---------------------------------------------------------------------------

export interface FundingCatchup {
  /** Periods charged now (≤ 168). */
  n: bigint;
  /** Whole periods beyond the cap, not charged. */
  dropped: bigint;
  /** Sub-period open seconds carried to the next crank. */
  carry: bigint;
}

/** `total` = open seconds since the last crank + carry. `null` when not due or `interval <= 0`. */
export function fundingCatchup(total: bigint, interval: bigint): FundingCatchup | null {
  if (interval <= 0n || total < interval) return null;
  const due = total / interval;
  const n = min(due, MAX_FUNDING_CATCHUP_PERIODS);
  const dropped = due - n;
  const carry = due > MAX_FUNDING_CATCHUP_PERIODS ? 0n : total - n * interval;
  return { n, dropped, carry: carry > 0xffff_ffffn ? 0xffff_ffffn : carry };
}

/** `clamp(cumulative + n·step)` to i64 (§8 step 7). */
export function fundingAccumulate(cumulative: bigint, n: bigint, step: bigint): bigint {
  return clamp(cumulative + n * step, I64_MIN, I64_MAX);
}

// ---------------------------------------------------------------------------
// LP share math (§9)
// ---------------------------------------------------------------------------

/** `floor(amount·lp_shares / nav_mint)` for a non-first deposit; `null` when `nav_mint ≤ 0` or it overflows u64. */
export function sharesForDeposit(amount: bigint, lpShares: bigint, navMint: bigint): bigint | null {
  if (navMint <= 0n) return null;
  const s = (amount * lpShares) / navMint;
  return s > U64_MAX ? null : s;
}

/** `floor(shares·max(nav_redeem, 0) / lp_shares)`, saturating at u64::MAX. */
export function redeemValue(shares: bigint, lpShares: bigint, navRedeem: bigint): bigint {
  if (lpShares === 0n || navRedeem <= 0n) return 0n;
  const v = mulDivFloor(navRedeem, shares, lpShares);
  return v === null ? U64_MAX : satU64(v);
}

/** Params timelock (§5): `max(MIN, max(delays) + max(windows) + MARGIN)`. */
export function paramsTimelockSecs(curDelay: bigint, newDelay: bigint, curWindow: bigint, newWindow: bigint): bigint {
  return max(max(curDelay, newDelay) + max(curWindow, newWindow) + PARAMS_TIMELOCK_MARGIN_SECS, MIN_POOL_TIMELOCK_SECS);
}

/** Open seconds in one CME week (115 h): every schedule but the test one (D38, D46). */
export const WEEK_OPEN_SECS = 414_000n;
/** One day of open time every week and claim window must leave ungated for `lp_withdraw` (D38). */
export const LP_UNGATED_SECS = 86_400n;
/** Session opens per CME week the reopen quiet gates. */
export const SESSION_OPENS_PER_WEEK = 5n;
/** `SCHEDULE_TEST_HOURLY` (localnet only, D46): its week's open time, ungated exit time and opens. */
const SCHEDULE_TEST_HOURLY = 2;
export const TEST_WEEK_OPEN_SECS = 2_820n;
export const TEST_LP_UNGATED_SECS = 600n;
export const TEST_SESSION_OPENS_PER_WEEK = 4n;

export function weekOpenSecs(schedule: number): bigint {
  return schedule === SCHEDULE_TEST_HOURLY ? TEST_WEEK_OPEN_SECS : WEEK_OPEN_SECS;
}

export function lpUngatedSecs(schedule: number): bigint {
  return schedule === SCHEDULE_TEST_HOURLY ? TEST_LP_UNGATED_SECS : LP_UNGATED_SECS;
}

export function sessionOpensPerWeek(schedule: number): bigint {
  return schedule === SCHEDULE_TEST_HOURLY ? TEST_SESSION_OPENS_PER_WEEK : SESSION_OPENS_PER_WEEK;
}

/** Upper bound on the open seconds per week the LP gates close `lp_withdraw`: `W + 5·Q` on CME (D38). */
export function lpGatedSecsPerWeek(schedule: number, window: bigint, quiet: bigint): bigint {
  return window + sessionOpensPerWeek(schedule) * quiet;
}

/** `validate`'s D38 rule: `W + 5·Q + 1 day ≤ 115 h` on CME, the compressed week on the test schedule. */
export function lpGatesLeaveExitTime(schedule: number, window: bigint, quiet: bigint): boolean {
  return lpGatedSecsPerWeek(schedule, window, quiet) + lpUngatedSecs(schedule) <= weekOpenSecs(schedule);
}

/** Shortest claim window that always holds the ungated `lp_withdraw` time: `W + 5·Q + 1 day` on CME. */
export function minClaimWindowSecs(schedule: number, window: bigint, quiet: bigint): bigint {
  return lpGatedSecsPerWeek(schedule, window, quiet) + lpUngatedSecs(schedule);
}

/** The claim window a request is held to (D38); `0` (never lapses) stays 0. */
export function effectiveClaimWindowSecs(schedule: number, stored: bigint, window: bigint, quiet: bigint): bigint {
  return stored === 0n ? 0n : max(stored, minClaimWindowSecs(schedule, window, quiet));
}

/**
 * A settled market's LP exit quiet time (D41): `lp_withdraw` waits
 * `max(profitCloseQuietSecs, lpReopenQuietSecs)` of wall time after
 * `settledAt`. `settledAt == 0`: none.
 */
export function settledWithdrawQuiet(settledAt: bigint, now: bigint, profitCloseQuietSecs: bigint, lpReopenQuietSecs: bigint): boolean {
  const q = max(profitCloseQuietSecs, lpReopenQuietSecs);
  return settledAt > 0n && now - settledAt < q;
}

/** `floor(size·price / 1e6)`, saturating at u64::MAX. */
export function notionalFloor(size: bigint, price: bigint): bigint {
  return satU64((size * price) / SCALE);
}
