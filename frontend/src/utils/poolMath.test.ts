import { describe, it, expect } from 'vitest';
import {
  ADL_FLOOR_DECAY_SECS,
  ADL_FLOOR_START_BPS,
  DD_WINDOW_SECS,
  MAX_OPEN_INTEREST,
  PARAMS_TIMELOCK_MARGIN_SECS,
  EMPTY_POOL_STATE,
  adlFloorNow,
  adlScore,
  bpsOf,
  ceilS,
  ceilU,
  dTrig,
  ddFreeze,
  ddWithdrawBlocked,
  divEuclid,
  drawdownBps,
  effectiveClaimWindowSecs,
  settledWithdrawQuiet,
  feeSplit,
  fundingAccumulate,
  fundingCatchup,
  gBps,
  hardNetOk,
  insuranceSplit,
  insuranceWindowCap,
  leMul,
  lpWindowCap,
  minClaimWindowSecs,
  mulDivFloor,
  nTrig,
  netCapOk,
  notionalFloor,
  paramsTimelockSecs,
  peakResets,
  perAccountOk,
  poolViewRaw,
  quoteProfitRaw,
  redeemValue,
  sharesForDeposit,
  skewPriceRaw,
  unvested,
  unwindPremium,
  premiumShortfall,
  wTrig,
  weekendCapOk,
  weekendBoundOn,
  kNetWithinWeekendBound,
  weekendTarget,
  weightedSince,
  type PoolState,
} from './poolMath';

// Ports of the unit tests in programs/perps-core/src/pool_math.rs, with the
// values those tests assert, so the two implementations stay in step.

const U64_MAX = (1n << 64n) - 1n;
const U128_MAX = (1n << 128n) - 1n;
const I64_MAX = (1n << 63n) - 1n;
const I64_MIN = -(1n << 63n);

/** xorshift64*, the Rust tests' deterministic generator. */
class Rng {
  private x: bigint;
  constructor(seed: bigint) {
    this.x = seed;
  }
  next(): bigint {
    let x = this.x;
    x ^= x >> 12n;
    x = (x ^ (x << 25n)) & U64_MAX;
    x ^= x >> 27n;
    this.x = x;
    return (x * 0x2545_f491_4f6c_dd1dn) & U64_MAX;
  }
  below(n: bigint): bigint {
    return this.next() % n;
  }
}

const state = (cash: bigint): PoolState => ({ ...EMPTY_POOL_STATE, cash, lpShares: 1_000_000_000_000n });

describe('rounding helpers', () => {
  it('match floor and ceil', () => {
    for (let a = -50; a <= 50; a++) {
      for (let b = 1; b <= 9; b++) {
        expect(ceilS(BigInt(a), BigInt(b))).toBe(BigInt(Math.ceil(a / b)));
        expect(divEuclid(BigInt(a), BigInt(b))).toBe(BigInt(Math.floor(a / b)));
        if (a >= 0) expect(ceilU(BigInt(a), BigInt(b))).toBe(BigInt(Math.ceil(a / b)));
      }
    }
  });

  it('mulDivFloor is exact and reports u128 overflow', () => {
    const big = U128_MAX;
    expect(mulDivFloor(big, big, big)).toBe(big);
    expect(mulDivFloor(big, 3n, 3n)).toBe(big);
    expect(mulDivFloor(big, 2n, 1n)).toBeNull();
    expect(mulDivFloor(1n, 1n, 0n)).toBeNull();
    expect(mulDivFloor(1n << 100n, 1n << 100n, 1n << 90n)).toBe(1n << 110n);
    expect(mulDivFloor((1n << 100n) + 1n, 1n << 100n, 1n << 101n)).toBe(1n << 99n);
  });

  it('leMul is exact', () => {
    expect(leMul(U128_MAX, U128_MAX, U128_MAX, U128_MAX)).toBe(true);
    expect(leMul(U128_MAX, U128_MAX, U128_MAX, U128_MAX - 1n)).toBe(false);
    expect(leMul(3n, 4n, 2n, 6n)).toBe(true);
    expect(leMul(3n, 5n, 2n, 7n)).toBe(false);
  });
});

describe('valuation', () => {
  it('unvested is linear and bounded', () => {
    expect(unvested(1_000n, 100n, 200n, 100n)).toBe(1_000n);
    expect(unvested(1_000n, 100n, 200n, 150n)).toBe(500n);
    expect(unvested(1_000n, 100n, 200n, 151n)).toBe(490n);
    expect(unvested(1_000n, 100n, 200n, 199n)).toBe(10n);
    expect(unvested(1_000n, 100n, 200n, 200n)).toBe(0n);
    expect(unvested(1_000n, 100n, 200n, 50n)).toBe(1_000n);
    expect(unvested(1_000n, 0n, 0n, 0n)).toBe(0n);
    expect(unvested(7n, 0n, 3n, 1n)).toBe(4n);
  });

  it('drawdown lines', () => {
    expect(drawdownBps(0n, 0n)).toBe(0n);
    expect(drawdownBps(1_100_000n, 1_000_000n)).toBe(0n);
    expect(drawdownBps(900_000n, 1_000_000n)).toBe(1_000n);
    expect(drawdownBps(899_999n, 1_000_000n)).toBe(1_000n);
    expect(drawdownBps(0n, 1_000_000n)).toBe(10_000n);
    expect(peakResets(5n, 5n, 0n, 1n)).toBe(true);
    expect(peakResets(4n, 5n, 0n, DD_WINDOW_SECS - 1n)).toBe(false);
    expect(peakResets(4n, 5n, 0n, DD_WINDOW_SECS)).toBe(true);
    expect([ddFreeze(999n, 1_000n), ddFreeze(1_000n, 1_000n), ddFreeze(10_000n, 0n)]).toEqual([false, true, false]);
    expect([dTrig(1_499n, 1_500n), dTrig(1_500n, 1_500n), dTrig(10_000n, 0n)]).toEqual([false, true, false]);
    expect([ddWithdrawBlocked(10_000n, 0n), ddWithdrawBlocked(5n, 5n)]).toEqual([false, true]);
  });

  it('values an empty pool', () => {
    const v = poolViewRaw(state(1_000_000_000_000n), 0n, 0n, 0n, 75_000_000n, 0n, 5_000n, 0n, 0n)!;
    expect(v.u).toBe(0n);
    expect(v.navMint).toBe(1_000_000_000_001n);
    expect(v.navRedeem).toBe(999_999_999_999n);
    expect(v.b).toBe(999_999_999_999n);
    expect(v.capNav).toBe(999_999_999_999n);
    expect(v.nps).toBe(1_000_000n);
    expect([v.net, v.netNotional, v.ddBps]).toEqual([0n, 0n, 0n]);
  });

  it('values a single long, and the mirrored short', () => {
    // 100 contracts long at $75, mark $76, funding moved +7_500 per contract
    const p: PoolState = {
      ...state(1_000_000_000n),
      longEntrySum: 100_000_000n * 75_000_000n,
      longFundingSum: 100_000_000n * 10n,
      openPositions: 1n,
    };
    p.pendingWithdrawShares = p.lpShares / 4n;
    const v = poolViewRaw(p, 100_000_000n, 0n, 7_510n, 76_000_000n, 0n, 5_000n, 0n, 0n)!;
    expect(v.u).toBe(99_250_000n);
    expect(v.navMint).toBe(1_000_000_000n - 99_250_000n + 3n);
    expect(v.navRedeem).toBe(1_000_000_000n - 99_250_000n - 3n);
    expect(v.net).toBe(100_000_000n);
    expect(v.netNotional).toBe(7_600_000_000n);
    expect(v.capNav).toBe((v.b * 3n) / 4n);

    const q: PoolState = {
      ...state(1_000_000_000n),
      shortEntrySum: p.longEntrySum,
      shortFundingSum: p.longFundingSum,
      openPositions: 1n,
    };
    const w = poolViewRaw(q, 0n, 100_000_000n, 7_510n, 76_000_000n, 0n, 5_000n, 0n, 0n)!;
    expect(w.u).toBe(-99_250_000n);
    expect(w.navRedeem).toBe(1_000_000_000n + 49_625_000n - 3n);
    expect(w.net).toBe(-100_000_000n);
    expect(w.netNotional).toBe(7_600_000_000n);
  });

  it('rounds net notional against the opener and U down', () => {
    expect(poolViewRaw(state(0n), 1n, 0n, 0n, 1n, 0n, 0n, 0n, 0n)!.netNotional).toBe(1n);
    const p: PoolState = { ...EMPTY_POOL_STATE, cash: 1_000n, longEntrySum: 2n, openPositions: 1n, lpShares: 1_000n };
    const v = poolViewRaw(p, 1n, 0n, 0n, 1n, 0n, 0n, 0n, 0n)!;
    expect([v.u, v.navMint]).toEqual([-1n, 1_000n + 1n + 3n]);
    expect(hardNetOk(0n, 1n, 0n)).toBe(true);
    expect(hardNetOk(1n, 1n, 0n)).toBe(false);
  });

  it('keeps net notional a ceiling and capNAV a floor', () => {
    const r = new Rng(17n);
    for (let i = 0; i < 2_000; i++) {
      const l = r.below(MAX_OPEN_INTEREST);
      const s = r.below(MAX_OPEN_INTEREST);
      const mark = 1n + r.below(1_000_000_000n);
      const p = state(r.below(U64_MAX / 2n));
      p.longEntrySum = l * mark;
      p.shortEntrySum = s * mark;
      p.lpShares = 1_000_000n + r.below(U64_MAX / 2n);
      p.pendingWithdrawShares = r.below(p.lpShares + 1n);
      const v = poolViewRaw(p, l, s, 0n, mark, 0n, 0n, 0n, 0n)!;
      expect(v.u).toBe(0n);
      const exact = (l > s ? l - s : s - l) * mark;
      expect(v.netNotional * 1_000_000n >= exact).toBe(true);
      expect(v.netNotional === 0n || (v.netNotional - 1n) * 1_000_000n < exact).toBe(true);
      const live = p.lpShares - p.pendingWithdrawShares;
      expect(v.capNav * p.lpShares <= v.b * live).toBe(true);
      expect((v.capNav + 1n) * p.lpShares > v.b * live).toBe(true);
    }
  });

  it('reports overflow, and never overflows inside the bounds', () => {
    const p = state(0n);
    p.longEntrySum = U128_MAX;
    expect(poolViewRaw(p, 1n, 0n, 0n, 1n, 0n, 0n, 0n, 0n)).toBeNull();
    const q = state(U64_MAX);
    q.longEntrySum = MAX_OPEN_INTEREST * U64_MAX;
    q.shortFundingSum = I64_MIN * MAX_OPEN_INTEREST;
    q.openPositions = U64_MAX / 4n;
    expect(poolViewRaw(q, MAX_OPEN_INTEREST, MAX_OPEN_INTEREST, I64_MAX, U64_MAX, 0n, 10_000n, 0n, 0n)).not.toBeNull();
  });

  it('widens mint minus redeem by exactly the unvested part', () => {
    const p = state(5_000_000_000n);
    const base = poolViewRaw(p, 0n, 0n, 0n, 1n, 50n, 0n, 0n, 0n)!;
    const v = poolViewRaw({ ...p, vestAmount: 1_000_000n, vestStart: 0n, vestEnd: 100n }, 0n, 0n, 0n, 1n, 50n, 0n, 0n, 0n)!;
    expect(v.navMint).toBe(base.navMint);
    expect(v.navRedeem).toBe(base.navRedeem - 500_000n);
  });
});

describe('skew price', () => {
  it('is the oracle without a scale', () => {
    expect(skewPriceRaw(75_000_000n, 123n, 456n, 0n, 100n)).toBe(75_000_000n);
  });

  it('rounds buys up and sells down', () => {
    expect([skewPriceRaw(7n, 0n, 1n, 3n, 1_000n), skewPriceRaw(7n, 0n, -1n, 3n, 1_000n)]).toEqual([7n, 7n]);
    expect(skewPriceRaw(75_000_000n, 0n, 1n, 7n, 10_000n)).toBe(75_000_000n + 5_357_143n);
    expect(skewPriceRaw(75_000_000n, 0n, -1n, 7n, 10_000n)).toBe(75_000_000n - 5_357_143n);
    for (let skew = -20; skew <= 20; skew++) {
      for (let d = 1; d <= 15; d++) {
        for (const scale of [1, 3, 7, 50]) {
          const o = 1_000_003;
          const b = skewPriceRaw(BigInt(o), BigInt(skew), BigInt(d), BigInt(scale), 10_000n)! - BigInt(o);
          const s = skewPriceRaw(BigInt(o), BigInt(skew), BigInt(-d), BigInt(scale), 10_000n)! - BigInt(o);
          const exact = (o * (2 * skew + d)) / (2 * scale);
          const exactS = (o * (2 * skew - d)) / (2 * scale);
          if (Math.abs(exact) < o) expect(Number(b) >= exact && Number(b) < exact + 1).toBe(true);
          if (Math.abs(exactS) < o) expect(Number(s) <= exactS && Number(s) > exactS - 1).toBe(true);
        }
      }
    }
  });

  it('is clamped to max impact', () => {
    const o = 75_000_000n;
    expect(skewPriceRaw(o, 1_000_000_000_000n, 1n, 1n, 100n)).toBe(75_750_000n);
    expect(skewPriceRaw(o, -1_000_000_000_000n, -1n, 1n, 100n)).toBe(74_250_000n);
    expect(skewPriceRaw(o, 5_000_000n, 5_000_000n, 1_000_000n, 0n)).toBe(o);
  });

  it('gives a better price for reducing the skew', () => {
    const o = 75_000_000n;
    const scale = 2_500_000_000_000n;
    const skew = 10_000_000_000n;
    const sell = skewPriceRaw(o, skew, -1_000_000_000n, scale, 100n)!;
    const buy = skewPriceRaw(o, skew, 1_000_000_000n, scale, 100n)!;
    expect(sell > o).toBe(true);
    expect(buy > sell).toBe(true);
    expect(skewPriceRaw(o, -skew, 1_000_000_000n, scale, 100n)! < o).toBe(true);
  });

  it('nets an open and a close in the same skew state to at most one unit', () => {
    const r = new Rng(11n);
    for (let i = 0; i < 1_000; i++) {
      const o = 1n + r.below(200_000_000n);
      const scale = 1n + r.below(5_000_000_000_000n);
      const skew = r.below(20_000_000_000n) - 10_000_000_000n;
      const q = 1n + r.below(5_000_000_000n);
      for (const buy of [true, false]) {
        const d = buy ? q : -q;
        const open = skewPriceRaw(o, skew, d, scale, 1_000n)!;
        const close = skewPriceRaw(o, skew + d, -d, scale, 1_000n)!;
        const cost = buy ? open - close : close - open;
        expect(cost >= 0n && cost <= 1n).toBe(true);
      }
    }
  });
});

describe('fees, waterfall and payouts', () => {
  it('splits fees and insurance', () => {
    expect(feeSplit(1_000n, 1_000n)).toEqual([100n, 900n]);
    expect(feeSplit(999n, 1_000n)).toEqual([99n, 900n]);
    expect(feeSplit(5n, 0n)).toEqual([0n, 5n]);
    expect(feeSplit(5n, 10_000n)).toEqual([5n, 0n]);
    expect(insuranceSplit(10n, 100n, 50n)).toEqual([50n, 0n]);
    expect(insuranceSplit(10n, 100n, 90n)).toEqual([90n, 0n]);
    expect(insuranceSplit(10n, 100n, 95n)).toEqual([90n, 5n]);
    expect(insuranceSplit(100n, 100n, 7n)).toEqual([0n, 7n]);
    expect(insuranceSplit(150n, 100n, 7n)).toEqual([0n, 7n]);
    expect(insuranceSplit(0n, U64_MAX, U64_MAX)).toEqual([U64_MAX, 0n]);
  });

  it('weights the booking time by debt', () => {
    expect(weightedSince(0n, 0n, 1_000n, 5n)).toBe(1_000n);
    expect(weightedSince(0n, 1n, 86_400n, 1_000_000n)).toBe(86_399n);
    expect(weightedSince(0n, 1_000_000n, 86_400n, 1n)).toBe(0n);
    expect(weightedSince(100n, 3n, 200n, 1n)).toBe(125n);
    expect(weightedSince(100n, 2n, 201n, 1n)).toBe(133n);
  });

  it('caps the loss window', () => {
    expect(lpWindowCap(1_000_000n, 500n, 0n)).toBe(50_000n);
    expect(lpWindowCap(1_000_000n, 500n, 49_999n)).toBe(1n);
    expect(lpWindowCap(1_000_000n, 500n, 60_000n)).toBe(0n);
    expect(insuranceWindowCap(0n, 123n)).toBe(U64_MAX);
    expect(insuranceWindowCap(100n, 30n)).toBe(70n);
    expect(insuranceWindowCap(100n, 130n)).toBe(0n);
  });

  it('haircuts profit payouts', () => {
    const z = { solvencyHc: 0n, badDebtHc: 0n, cashHc: 0n };
    expect(quoteProfitRaw(50n, 100n, 0n, 40n, false)).toEqual({ ...z, paid: 40n });
    expect(quoteProfitRaw(100n, 100n, 0n, 40n, false)!.solvencyHc).toBe(0n);
    expect(quoteProfitRaw(300n, 100n, 0n, 40n, false)).toEqual({ paid: 13n, solvencyHc: 27n, badDebtHc: 0n, cashHc: 0n });
    expect(quoteProfitRaw(-1_000n, 100n, 0n, 40n, false)!.paid).toBe(40n);
    expect(quoteProfitRaw(0n, 100n, 15n, 40n, false)).toEqual({ ...z, paid: 25n, badDebtHc: 15n });
    expect(quoteProfitRaw(0n, 100n, 75n, 40n, false)).toEqual({ ...z, paid: 0n, badDebtHc: 40n });
    expect(quoteProfitRaw(0n, 30n, 0n, 40n, false)).toBeNull();
    expect(quoteProfitRaw(0n, 30n, 0n, 40n, true)).toEqual({ ...z, paid: 30n, cashHc: 10n });
    expect(quoteProfitRaw(0n, 30n, 10n, 40n, false)).toEqual({ ...z, paid: 30n, badDebtHc: 10n });
  });

  it('payout parts always sum to P', () => {
    const r = new Rng(5n);
    for (let i = 0; i < 2_000; i++) {
      const u = r.below(2_000n) - 1_000n;
      const cash = r.below(1_000n);
      const opp = r.below(300n);
      const p = r.below(1_000n);
      for (const allow of [false, true]) {
        const q = quoteProfitRaw(u, cash, opp, p, allow);
        if (q) {
          expect(q.paid + q.solvencyHc + q.badDebtHc + q.cashHc).toBe(p);
          expect(q.paid <= cash && q.badDebtHc <= opp).toBe(true);
        } else {
          expect(allow).toBe(false);
        }
      }
    }
  });
});

describe('caps and triggers', () => {
  it('match the program at the limits', () => {
    expect(netCapOk(1_000_000n, 10_000n, 1_000_000n)).toBe(true);
    expect(netCapOk(1_000_001n, 10_000n, 1_000_000n)).toBe(false);
    expect(netCapOk(0n, 0n, 0n)).toBe(true);
    expect(weekendCapOk(500_000n, 2_000n, 1_000n, 1_000_000n)).toBe(true);
    expect(weekendCapOk(500_001n, 2_000n, 1_000n, 1_000_000n)).toBe(false);
    // D43 (pool_math.rs): on with W > 0; k_net·G ≤ α·1e4 at the edge
    expect([weekendBoundOn(0n), weekendBoundOn(1n)]).toEqual([false, true]);
    expect(kNetWithinWeekendBound(5_000n, 2_000n, 1_000n)).toBe(true);
    expect(kNetWithinWeekendBound(5_001n, 2_000n, 1_000n)).toBe(false);
    expect(weekendCapOk(U128_MAX, 0n, 0n, 0n)).toBe(true);
    expect(gBps(0n, 1_000n, 2_000n)).toBe(2_000n);
    expect(gBps(0n, 1_000n, 0n)).toBe(1_000n);
    expect(gBps(1_500n, 1_000n, 2_000n)).toBe(1_500n);
    expect(perAccountOk(200_000n, 50_000n, 2_500n, 10_000n, 1_000_000n)).toBe(true);
    expect(perAccountOk(200_000n, 50_001n, 2_500n, 10_000n, 1_000_000n)).toBe(false);
    expect(perAccountOk(U64_MAX, U64_MAX, 0n, 0n, 0n)).toBe(true);
    expect(nTrig(1_500_000n, 15_000n, 1_000_000n)).toBe(false);
    expect(nTrig(1_500_001n, 15_000n, 1_000_000n)).toBe(true);
    expect(nTrig(U128_MAX, 0n, 0n)).toBe(false);
    expect(wTrig(true, 500_000n, 2_000n, 1_000n, 1_000_000n)).toBe(false);
    expect(wTrig(true, 500_001n, 2_000n, 1_000n, 1_000_000n)).toBe(true);
    expect(wTrig(false, U128_MAX, 2_000n, 1_000n, 0n)).toBe(false);
    expect(wTrig(true, U128_MAX, 0n, 1_000n, 0n)).toBe(false);
    expect(hardNetOk(1_500_000n, 15_000n, 1_000_000n)).toBe(true);
    expect(hardNetOk(1_500_001n, 15_000n, 1_000_000n)).toBe(false);
    expect(hardNetOk(U128_MAX, 0n, 0n) && hardNetOk(0n, 15_000n, 0n)).toBe(true);
    expect(bpsOf(10_000n, 1_000_001n)).toBe(1_000_001n);
    expect(bpsOf(5_000n, 1_000_001n)).toBe(500_000n);
    expect(weekendTarget(1_000n, 1_000_001n, 2_000n)).toBe(500_000n);
    expect(weekendTarget(1_000n, 1_000_001n, 0n)).toBe(U128_MAX);
  });
});

describe('ADL', () => {
  it('scores like the formula', () => {
    expect(adlScore(true, 90_000_000n, 75_000_000n, 9_000_000_000n, 2_250_000_000n)).toBe(48_000n);
    expect(adlScore(false, 75_000_000n, 90_000_000n, 7_500_000_000n, 2_250_000_000n)).toBe(40_000n);
    expect(adlScore(true, 1n, 1n, 1n, 0n)).toBe(0n);
    expect(adlScore(true, 1n, 0n, 1n, 1n)).toBe(U128_MAX);
    const n = (MAX_OPEN_INTEREST * U64_MAX) / 1_000_000n;
    expect(adlScore(true, U64_MAX, U64_MAX, n, 1n)).toBe(n * 10_000n);
    const half = U64_MAX / 2n;
    const expected = mulDivFloor(n * 10_000n, U64_MAX, half)! / 7n;
    expect(adlScore(true, U64_MAX, half, n, 7n)).toBe(expected);
    expect(adlScore(false, half, U64_MAX, n, 7n)).toBe(expected);
    expect(adlScore(true, U64_MAX, 1n, n, 1n)).toBe(U128_MAX);
  });

  it('decays the floor linearly', () => {
    expect(adlFloorNow(0n)).toBe(ADL_FLOOR_START_BPS);
    expect(adlFloorNow(-5n)).toBe(ADL_FLOOR_START_BPS);
    expect(adlFloorNow(ADL_FLOOR_DECAY_SECS / 2n)).toBe(ADL_FLOOR_START_BPS / 2n);
    expect(adlFloorNow(1n)).toBe(999_953n);
    expect(adlFloorNow(ADL_FLOOR_DECAY_SECS)).toBe(0n);
    expect(adlFloorNow(10n * ADL_FLOOR_DECAY_SECS)).toBe(0n);
    expect(adlFloorNow(3n * 3_600n)).toBe(500_000n);
  });
});

describe('funding catch-up', () => {
  it('charges whole periods and carries the rest', () => {
    expect(fundingCatchup(3_599n, 3_600n)).toBeNull();
    expect(fundingCatchup(1n, 0n)).toBeNull();
    expect(fundingCatchup(3_600n, 3_600n)).toEqual({ n: 1n, dropped: 0n, carry: 0n });
    expect(fundingCatchup(3n * 3_600n + 17n, 3_600n)).toEqual({ n: 3n, dropped: 0n, carry: 17n });
    expect(fundingCatchup(168n * 3_600n + 59n, 3_600n)).toEqual({ n: 168n, dropped: 0n, carry: 59n });
    expect(fundingCatchup(169n * 3_600n + 59n, 3_600n)).toEqual({ n: 168n, dropped: 1n, carry: 0n });
    expect(fundingCatchup(I64_MAX, 60n)!.n).toBe(168n);
    expect(fundingAccumulate(I64_MAX - 5n, 168n, 1_000n)).toBe(I64_MAX);
    expect(fundingAccumulate(I64_MIN + 5n, 168n, -1_000n)).toBe(I64_MIN);
    expect(fundingAccumulate(100n, 3n, -7n)).toBe(79n);
  });
});

describe('LP share math', () => {
  it('floors shares and redemptions', () => {
    expect(sharesForDeposit(100n, 1_000n, 1_000n)).toBe(100n);
    expect(sharesForDeposit(100n, 1_000n, 1_001n)).toBe(99n);
    expect(sharesForDeposit(100n, 1_000n, 0n)).toBeNull();
    expect(sharesForDeposit(100n, 1_000n, -5n)).toBeNull();
    expect(redeemValue(100n, 1_000n, 1_001n)).toBe(100n);
    expect(redeemValue(100n, 1_000n, 999n)).toBe(99n);
    expect(redeemValue(100n, 1_000n, -1n)).toBe(0n);
    expect(redeemValue(100n, 0n, 1_000n)).toBe(0n);
  });

  it('never lowers per-share NAV on deposits and withdrawals', () => {
    const r = new Rng(3n);
    for (let i = 0; i < 2_000; i++) {
      const p: PoolState = {
        ...EMPTY_POOL_STATE,
        cash: r.below(10_000_000_000_000n),
        lpShares: 1_000_000n + r.below(10_000_000_000_000n),
        openPositions: r.below(50n),
      };
      const l = r.below(1_000_000_000n);
      const s = r.below(1_000_000_000n);
      p.longEntrySum = l * (1n + r.below(100_000_000n));
      p.shortEntrySum = s * (1n + r.below(100_000_000n));
      const mark = 1n + r.below(100_000_000n);
      const h = r.below(10_001n);
      const [k, mi] = r.below(2n) === 0n ? [0n, 0n] : [1n + r.below(10_000_000_000_000n), r.below(1_001n)];
      const v = poolViewRaw(p, l, s, 0n, mark, 0n, h, k, mi)!;
      expect(v.navRedeem < v.navMint).toBe(true);
      if (v.navMint <= 0n) continue;
      const before = v.nps;
      const amount = 1n + r.below(1_000_000_000_000n);
      const minted = sharesForDeposit(amount, p.lpShares, v.navMint)!;
      const d = { ...p, cash: p.cash + amount, lpShares: p.lpShares + minted };
      expect(poolViewRaw(d, l, s, 0n, mark, 0n, h, k, mi)!.nps >= before).toBe(true);
      const shares = 1n + r.below(p.lpShares - 1n);
      const value = redeemValue(shares, p.lpShares, v.navRedeem);
      if (value > p.cash) continue;
      const w = { ...p, cash: p.cash - value, lpShares: p.lpShares - shares };
      expect(poolViewRaw(w, l, s, 0n, mark, 0n, h, k, mi)!.nps >= before).toBe(true);
    }
  });

  it('does not count the refundable skew premium as LP value', () => {
    const [k, mi, h] = [2_500_000_000_000n, 100n, 5_000n];
    const o = 75_000_000n;
    const cases: [bigint, bigint][] = [
      [10_000_000_000_000n, 133_333_000_000n],
      [1_000_000_000_000n, 13_333_000_000n],
      [1_000_000_000_000n, 1_000_000_000n],
      [1_000_000_000_000n, 1n],
    ];
    for (const [cash, q] of cases) {
      const p0: PoolState = { ...EMPTY_POOL_STATE, cash, lpShares: cash };
      const before = poolViewRaw(p0, 0n, 0n, 0n, o, 0n, h, k, mi)!;
      const open = skewPriceRaw(o, 0n, q, k, mi)!;
      const p1 = { ...p0, longEntrySum: q * open, openPositions: 1n };
      const v = poolViewRaw(p1, q, 0n, 0n, o, 0n, h, k, mi)!;
      const close = skewPriceRaw(o, q, -q, k, mi)!;
      const finalCash = cash - divEuclid(q * (close - open), 1_000_000n);
      expect(v.navRedeem <= finalCash).toBe(true);
      expect(finalCash - v.navRedeem <= 4n).toBe(true);
      const dn = v.nps - before.nps;
      expect(dn >= -1n && dn <= 1n).toBe(true);
    }
    const q = 133_333_000_000n;
    expect(unwindPremium(q, o, k, mi)).toBe(ceilU(ceilU(q * o, 1_000_000n) * 100n, 10_000n));
    expect(unwindPremium(-q, o, k, mi)).toBe(unwindPremium(q, o, k, mi));
    expect(unwindPremium(1_000_000_000n, o, k, mi)).toBe(15_000_000n);
    expect(unwindPremium(1_000_000_001n, o, k, mi)).toBe(15_000_001n);
    expect(unwindPremium(1_000_000_000n, o, 0n, mi)).toBe(0n);
    expect(unwindPremium(0n, o, k, mi)).toBe(0n);
  });

  it('times the params lock and claim windows', () => {
    expect(paramsTimelockSecs(345_600n, 345_600n, 86_400n, 86_400n)).toBe(10n * 86_400n);
    expect(paramsTimelockSecs(345_600n, 0n, 86_400n, 0n)).toBe(10n * 86_400n);
    expect(paramsTimelockSecs(0n, 345_600n, 0n, 86_400n)).toBe(10n * 86_400n);
    expect(paramsTimelockSecs(0n, 0n, 0n, 0n)).toBe(PARAMS_TIMELOCK_MARGIN_SECS);
    // Production W = 1 d, Q = 30 min: W + 5·Q + 1 d
    expect(minClaimWindowSecs(86_400n, 1_800n)).toBe(181_800n);
    expect(effectiveClaimWindowSecs(259_200n, 86_400n, 1_800n)).toBe(259_200n);
    expect(effectiveClaimWindowSecs(100n, 86_400n, 1_800n)).toBe(181_800n);
    expect(effectiveClaimWindowSecs(0n, 86_400n, 1_800n)).toBe(0n);
  });

  it('holds settled withdrawals for the longer quiet time (pool_math settled_withdraw_quiet)', () => {
    expect(settledWithdrawQuiet(1_000n, 1_000n, 300n, 1_800n)).toBe(true);
    expect(settledWithdrawQuiet(1_000n, 2_799n, 300n, 1_800n)).toBe(true);
    expect(settledWithdrawQuiet(1_000n, 2_800n, 300n, 1_800n)).toBe(false);
    expect(settledWithdrawQuiet(1_000n, 1_299n, 300n, 0n)).toBe(true);
    expect(settledWithdrawQuiet(1_000n, 1_300n, 300n, 0n)).toBe(false);
    expect(settledWithdrawQuiet(1_000n, 1_000n, 0n, 0n)).toBe(false);
    expect(settledWithdrawQuiet(0n, 1n, 300n, 1_800n)).toBe(false);
  });

  it('saturates notional', () => {
    expect(notionalFloor(1_500_000n, 75_000_001n)).toBe(112_500_001n);
    expect(notionalFloor(U64_MAX, U64_MAX)).toBe(U64_MAX);
  });
});

describe('premium_shortfall (D43)', () => {
  it('matches the program: rounding in the quadratic branch, o·K·1%²/2 across the clamp', () => {
    const [k, imp, o, c] = [2_500_000_000_000n, 100n, 75_000_000n, 1_000_000n];
    for (const [n, s] of [[0n, 1_000n], [10_000n, 5_000n], [-20_000n, -10_000n], [0n, 1n]]) {
      const exec = skewPriceRaw(o, n * c, s * c, k, imp)!;
      const size = s < 0n ? -s * c : s * c;
      expect(premiumShortfall(n * c, (n + s) * c, o, exec, size, k, imp)! <= 2n).toBe(true);
    }
    const [n, s] = [25_000n * c, 60_000n * c];
    const exec = skewPriceRaw(o, n, s, k, imp)!;
    expect(exec).toBe(o + o / 100n);
    expect(premiumShortfall(n, n + s, o, exec, s, k, imp)).toBe(9_375n * c);
    // An ADL slice at the mark: all of Δx
    expect(premiumShortfall(n, n + s, o, o, s, k, imp)).toBe(63_750n * c - 9_375n * c);
    expect(premiumShortfall(n + s, n, o, o, s, k, imp)).toBe(0n);
  });
});
