import { describe, it, expect } from 'vitest';
import {
  acceptablePrice,
  big,
  checkCloseCaps,
  checkOpenCaps,
  closeExecPrice,
  closeFee,
  decodeLpPool,
  decodeMarket,
  decodePoolParams,
  depositShares,
  encodePoolParams,
  houseQuote,
  liquidationStage,
  lpGates,
  marketConfigParams,
  poolStatus,
  proposalTimelock,
  neutralPoolParams,
  openHeadroom,
  poolView,
  productionPoolParams,
  profitCloseQuietLeft,
  triggerAcceptablePrice,
  validatePoolParams,
  venueMargin,
  withdrawalRequest,
  withdrawPostChecks,
  withdrawRefusal,
  withdrawRoom,
  exitBufferBps,
  type LpPoolAccount,
  type MarketState,
} from './pool';
import { SCHEDULE_CME_GLOBEX, openSecondsBetween } from './session';
import { EMPTY_POOL_STATE } from './poolMath';

const C6 = 1_000_000n;
const FRIDAY_CLOSE = 1_794_607_200; // Fri 2026-11-13 17:00 ET
const WEDNESDAY = 1_794_416_400;
const SUNDAY_OPEN = FRIDAY_CLOSE + 49 * 3_600;
const SATURDAY_NOON = FRIDAY_CLOSE + 19 * 3_600;

// OIL as in programs/perps-core/src/state/venue.rs tests
const oil: MarketState = decodeMarket({
  maxLeverage: 20_000,
  initialMarginRatio: 1_000,
  maintenanceMarginRatio: 500,
  takerFee: 50,
  sessionSchedule: SCHEDULE_CME_GLOBEX,
  preCloseWindowSecs: 7_200,
  weekendInitialMarginRatio: 2_000,
  weekendMaintenanceMarginRatio: 1_000,
  maxOpenInterest: 10_000_000n * C6,
  fundingInterval: 3_600,
});

function pool(cash: bigint, params = productionPoolParams()): LpPoolAccount {
  return {
    ...decodeLpPool({}),
    ...EMPTY_POOL_STATE,
    cash,
    lpShares: cash,
    params,
  };
}

describe('decoding', () => {
  it('reads BN-like, numbers and bigints, and defaults missing fields to 0', () => {
    const bn = (s: string) => ({ toString: () => s });
    expect(big(bn('340282366920938463463374607431768211455'))).toBe((1n << 128n) - 1n);
    expect(big(7)).toBe(7n);
    expect(big(undefined)).toBe(0n);
    const m = decodeMarket({ longOpenInterest: bn('5'), isPaused: true, fundingCarrySecs: 17 });
    expect([m.longOpenInterest, m.isPaused, m.fundingCarrySecs, m.settledAt]).toEqual([5n, true, 17, 0n]);
    const p = decodeLpPool({ cash: bn('9'), shortFundingSum: bn('-12'), params: { skewScale: bn('3'), openFeeBps: 5 } });
    expect([p.cash, p.shortFundingSum, p.params.skewScale, p.params.openFeeBps, p.pendingParams.kNetBps]).toEqual([9n, -12n, 3n, 5n, 0n]);
  });

  it('round-trips pool params through the Anchor argument shape', () => {
    const prod = productionPoolParams();
    const encoded = encodePoolParams(prod, (x) => ({ toString: () => x.toString() }));
    expect(typeof encoded.kNetBps).toBe('number');
    expect(typeof encoded.skewScale).toBe('object');
    expect(decodePoolParams(encoded as never)).toEqual(prod);
  });
});

describe('PoolParams::validate', () => {
  it('accepts the production and neutral sets for OIL', () => {
    expect(validatePoolParams(productionPoolParams(), oil)).toEqual([]);
    expect(validatePoolParams(neutralPoolParams(), oil)).toEqual([]);
  });

  it('rejects what the program rejects', () => {
    const p = productionPoolParams();
    // W = 4 d with Q = 1 d shuts LP exits for weeks (D38)
    expect(validatePoolParams({ ...p, weekendCapWindowSecs: 4n * 86_400n, lpReopenQuietSecs: 86_400n }, oil).length).toBeGreaterThan(0);
    expect(validatePoolParams({ ...p, withdrawalClaimWindowSecs: 100n }, oil).length).toBeGreaterThan(0);
    expect(validatePoolParams({ ...p, profitCloseQuietSecs: 3_601n }, oil).length).toBeGreaterThan(0);
    // The stage-1 penalty must stay below the backstop equity
    expect(validatePoolParams({ ...p, liqPenaltyBps: 400n }, oil).length).toBeGreaterThan(0);
    // Self-liquidation must cost more than impact + taker fee
    expect(validatePoolParams({ ...p, liqPenaltyBps: 150n }, oil).length).toBeGreaterThan(0);
  });

  it('holds k_net to the weekend bound while W > 0 (D43)', () => {
    const p = productionPoolParams();
    // G = 20%, α = 10%: k_net ≤ 0.5×; production's 0.4× keeps a 20% exit
    // buffer below it (D45), and the edge itself still validates
    expect(validatePoolParams({ ...p, kNetBps: 5_000n }, oil)).toEqual([]);
    expect(validatePoolParams({ ...p, kNetBps: 5_001n }, oil).length).toBeGreaterThan(0);
    expect(validatePoolParams({ ...p, kNetBps: 5_001n, weekendCapWindowSecs: 0n }, oil)).toEqual([]);
    // An overridden G of 10% allows 1×
    expect(validatePoolParams({ ...p, weekendGapBps: 1_000n, kNetBps: 10_000n, hardNetBps: 15_000n }, oil)).toEqual([]);
    expect(validatePoolParams({ ...p, weekendGapBps: 1_000n, kNetBps: 10_001n, hardNetBps: 15_000n }, oil).length).toBeGreaterThan(0);
    // A higher weekend IM tightens a derived G: 25% uses up the buffer
    // (0.4× is the edge), 30% refuses production's 0.4×
    expect(validatePoolParams(p, { ...oil, weekendInitialMarginRatio: 2_500n })).toEqual([]);
    expect(validatePoolParams(p, { ...oil, weekendInitialMarginRatio: 3_000n }).length).toBeGreaterThan(0);
  });
});

describe('venue_margin (state/venue.rs tests)', () => {
  it('matches the worked example', () => {
    const [size, price, oracle] = [100n * C6, 75_030_000n, 75_000_000n];
    expect(venueMargin(oil, true, size, price, oracle, 10_000n, WEDNESDAY)).toBe(753_300_000n);
    expect(venueMargin(oil, false, size, price, oracle, 10_000n, WEDNESDAY)).toBe(750_300_000n);
  });

  it('rounds up and applies the weekend margin', () => {
    const m = { ...oil, sessionSchedule: 0, preCloseWindowSecs: 0 };
    expect(venueMargin(m, true, C6, C6, C6, 3_000n, 0)).toBe(333_334n);
    expect(venueMargin(m, true, 1n, C6, C6, 1_000n, 0)).toBe(1n);
    const args = [100n * C6, 75_000_000n, 75_000_000n] as const;
    expect(venueMargin(oil, true, ...args, 10_000n, FRIDAY_CLOSE - 60)).toBeNull();
    expect(venueMargin(oil, true, ...args, 5_000n, FRIDAY_CLOSE - 60)).toBe(1_500_000_000n);
    expect(venueMargin(oil, true, ...args, 0n, WEDNESDAY)).toBeNull();
  });
});

describe('house quote', () => {
  it('prices skew impact, the fee and the margin', () => {
    const p = pool(1_000_000n * C6);
    // 1,000 contracts long from a flat book: 75·1000/(2·2.5M) = $0.015 premium
    const q = houseQuote(oil, p, true, 1_000n * C6, 5_000n, 75n * C6, WEDNESDAY);
    expect(q.ok).toBe(true);
    if (!q.ok) return;
    expect(q.value.exec).toBe(75_015_000n);
    expect(q.value.notional).toBe(75_015_000_000n);
    expect(q.value.fee).toBe(37_507_500n); // 5 bps, ceiled
    // 5x on the fill notional, plus the $15 the fill is under water at the oracle
    expect(q.value.margin).toBe(15_003_000_000n + 15_000_000n);
    expect(q.value.impactBps).toBeCloseTo(2, 5);
    // A short from a long skew gets a better price than the oracle
    const skewed = { ...oil, longOpenInterest: 100_000n * C6 };
    const s = houseQuote(skewed, p, false, 1_000n * C6, 5_000n, 75n * C6, WEDNESDAY);
    expect(s.ok && s.value.exec > 75n * C6).toBe(true);
  });

  it('reports the program errors', () => {
    const p = pool(1_000_000n * C6);
    expect(houseQuote(oil, p, true, 1n, 5_000n, 1n, WEDNESDAY)).toEqual({ ok: false, error: 'PositionTooSmall' });
    expect(houseQuote(oil, p, true, C6, 20_000n, 75n * C6, WEDNESDAY)).toEqual({ ok: false, error: 'ExcessiveLeverage' });
  });

  it('bounds the price against the trader, never at 0', () => {
    expect(acceptablePrice(true, 75n * C6, 50)).toBe(75_375_000n);
    expect(acceptablePrice(false, 75n * C6, 50)).toBe(74_625_000n);
    expect(acceptablePrice(true, 3n, 1)).toBe(4n); // ceiled
    expect(acceptablePrice(false, 1n, 50)).toBe(1n);
    expect(acceptablePrice(false, 75n * C6, 20_000)).toBe(1n);
  });
});

describe('caps and headroom', () => {
  // $1M pool, production caps: net 0.4x capNAV (20% under α/G = 0.5x, D45),
  // per account 25% of that
  const p = pool(1_000_000n * C6);
  const oracle = 75n * C6;
  const v = poolView(p, oil, oracle, WEDNESDAY)!;

  it('agree with check_open_caps at the boundary', () => {
    const h = openHeadroom(oil, p, v, 0n, oracle, WEDNESDAY);
    expect(h.netCap).toBe('net');
    expect(h.long).toBe(h.short);
    expect(checkOpenCaps(oil, p, v, true, h.long, oracle, 0n, 0n, WEDNESDAY)).toBeNull();
    expect(checkOpenCaps(oil, p, v, true, h.long + 1n, oracle, 0n, 0n, WEDNESDAY)).toBe('PoolExposureLimit');
    expect(h.account).toBe(v.capNav / 10n);
    expect(checkOpenCaps(oil, p, v, true, C6, oracle, 0n, h.account!, WEDNESDAY)).toBeNull();
    expect(checkOpenCaps(oil, p, v, true, C6, oracle, 0n, h.account! + 1n, WEDNESDAY)).toBe('AccountExposureLimit');
  });

  it('use the weekend cap at any time while W > 0 (D43), and only for net-raising opens', () => {
    // Production k_net 0.4× is under α/G = 0.5× (D45): the net cap is the tighter
    expect(openHeadroom(oil, p, v, 0n, oracle, FRIDAY_CLOSE - 3_600).netCap).toBe('net');
    // A weekend IM raised to 30% after the params: G·net ≤ α·capNAV binds at
    // 0.333×, under the 0.4× net cap, on a Wednesday
    const raised = { ...oil, weekendInitialMarginRatio: 3_000n };
    for (const t of [WEDNESDAY, FRIDAY_CLOSE - 3_600]) {
      const h = openHeadroom(raised, p, v, 0n, oracle, t);
      expect(h.netCap).toBe('weekend');
      expect(checkOpenCaps(raised, p, v, false, h.short, oracle, 0n, 0n, t)).toBeNull();
      expect(checkOpenCaps(raised, p, v, false, h.short + 1n, oracle, 0n, 0n, t)).toBe('PoolWeekendExposureLimit');
    }
    // W = 0 turns it off: the 0.4× net cap again. A $375k net-raising close
    // fits the net cap but not the weekend cap at G 30%
    const off = pool(1_000_000n * C6, { ...productionPoolParams(), weekendCapWindowSecs: 0n });
    expect(openHeadroom(raised, off, v, 0n, oracle, WEDNESDAY).netCap).toBe('net');
    expect(checkCloseCaps(raised, p, v, true, 5_000n * C6, oracle, WEDNESDAY)).toBe('PoolWeekendExposureLimit');
    expect(checkCloseCaps(raised, off, v, true, 5_000n * C6, oracle, WEDNESDAY)).toBeNull();
  });

  it('count the skew-premium shortfall of an open across the clamp (D43)', () => {
    // +25k venue contracts long at $75, B ≈ $12.75M: a long to the closed-form
    // weekend edge crosses the 1% clamp and would leave $9,375 of B short.
    // k_net at the α/G edge (0.5×, no exit buffer; production's 0.4× would
    // answer first, D45), so only the shortfall makes the weekend cap tighter
    const n0 = 25_000n * C6;
    const big = { ...pool(12_760_000n * C6, { ...productionPoolParams(), kNetBps: 5_000n }), longEntrySum: n0 * oracle, openPositions: 1n };
    const m = { ...oil, longOpenInterest: n0 };
    const vb = poolView(big, m, oracle, WEDNESDAY)!;
    const h = openHeadroom(m, big, vb, 0n, oracle, WEDNESDAY);
    expect(checkOpenCaps(m, big, vb, true, h.long, oracle, 0n, 0n, WEDNESDAY)).toBeNull();
    expect(checkOpenCaps(m, big, vb, true, h.long + 1n, oracle, 0n, 0n, WEDNESDAY)).toBe('PoolWeekendExposureLimit');
    const flat = (vb.capNav * C6) / (2n * oracle) - n0; // α·capNAV/G in contracts, no shortfall
    expect(h.long < flat && h.long > 59_000n * C6).toBe(true);
    // A net-raising close (a short buying back) is held the same way
    expect(checkCloseCaps(m, big, vb, false, flat, oracle, WEDNESDAY)).toBe('PoolWeekendExposureLimit');
    expect(checkCloseCaps(m, big, vb, false, h.long, oracle, WEDNESDAY)).toBeNull();
  });

  it('let the reducing side through a freeze', () => {
    const debt = { ...p, pendingBadDebtLong: 1n };
    const skewed = { ...oil, longOpenInterest: 1_000n * C6 };
    const vs = poolView(debt, skewed, oracle, WEDNESDAY)!;
    const h = openHeadroom(skewed, debt, vs, 0n, oracle, WEDNESDAY);
    expect(h.frozen).toBe('PoolAdlPending');
    expect([h.long, h.short]).toEqual([0n, 2_000n * C6]);
    expect(checkOpenCaps(skewed, debt, vs, true, 1n, oracle, 0n, 0n, WEDNESDAY)).toBe('PoolAdlPending');
    expect(checkOpenCaps(skewed, debt, vs, false, 2_000n * C6, oracle, 0n, 0n, WEDNESDAY)).toBeNull();
  });

  it('checks closes that raise net against B', () => {
    // A long whose short counterparty is a venue position: closing it raises |net|
    const tiny = pool(1_000n * C6);
    const vt = poolView(tiny, oil, oracle, WEDNESDAY)!;
    expect(checkCloseCaps(oil, tiny, vt, true, 100n * C6, oracle, WEDNESDAY)).toBe('PoolExposureLimit');
    expect(checkCloseCaps(oil, tiny, vt, true, 5n * C6, oracle, WEDNESDAY)).toBeNull(); // $375 ≤ 0.4 × $1,000
    expect(checkCloseCaps(oil, tiny, vt, true, 6n * C6, oracle, WEDNESDAY)).toBe('PoolExposureLimit');
    expect(checkCloseCaps({ ...oil, settlementPrice: oracle }, tiny, vt, true, 100n * C6, oracle, WEDNESDAY)).toBeNull();
  });
});

describe('closing', () => {
  const p = pool(1_000_000n * C6);

  it('prices the close against the skew, or at settlement', () => {
    const skewed = { ...oil, longOpenInterest: 1_000n * C6 };
    // Closing a long sells into a long skew: above the oracle
    expect(closeExecPrice(skewed, p, true, 1_000n * C6, 75n * C6)).toBe(75_015_000n);
    expect(closeExecPrice({ ...skewed, settlementPrice: 80n * C6 }, p, true, C6, 75n * C6)).toBe(80n * C6);
    // TP/SL bound: max impact (1%) + 1% below the oracle for a long close
    expect(triggerAcceptablePrice(skewed, p, true, 75n * C6, 100)).toBe(73_500_000n);
    expect(triggerAcceptablePrice(skewed, p, false, 75n * C6, 100)).toBe(76_500_000n);
  });

  it('times the profit-close quiet after each open and after settlement', () => {
    expect(profitCloseQuietLeft(oil, p, SUNDAY_OPEN + 100)).toBe(200);
    expect(profitCloseQuietLeft(oil, p, SUNDAY_OPEN + 300)).toBe(0);
    expect(profitCloseQuietLeft(oil, p, FRIDAY_CLOSE + 60)).toBe(0); // closed
    // Monday 18:00 ET daily reopen, 24 h after the Sunday open
    expect(profitCloseQuietLeft(oil, p, SUNDAY_OPEN + 24 * 3_600 + 10)).toBe(290);
    const settled = { ...oil, settlementPrice: 70n * C6, settledAt: BigInt(WEDNESDAY) };
    expect(profitCloseQuietLeft(settled, p, WEDNESDAY + 60)).toBe(240);
    expect(profitCloseQuietLeft({ ...settled, settledAt: 0n }, p, WEDNESDAY + 60)).toBe(0);
    expect(profitCloseQuietLeft({ ...oil, sessionSchedule: 0 }, p, SUNDAY_OPEN)).toBe(0);
  });

  it('charges the close fee on the opened notional, ceiled', () => {
    expect(closeFee(100n * C6, 75_000_001n, 50n)).toBe(37_500_001n);
  });
});

describe('liquidation stage (§6.1, §6.2)', () => {
  // 100 contracts at $75 mark: N = $7,500; MMR 5% = $375; backstop 6,667 bps of it
  const n = 100n * C6;
  const mark = 75n * C6;
  it('is none at the maintenance line, partial above the backstop, backstop below', () => {
    expect(liquidationStage(375n * C6, n, mark, 500n, 6_667n)).toBe('none');
    expect(liquidationStage(375n * C6 - 1n, n, mark, 500n, 6_667n)).toBe('partial');
    // backstop = floor(500·6667/1e4) = 333 bps → $249.75
    expect(liquidationStage(249_750_000n, n, mark, 500n, 6_667n)).toBe('partial');
    expect(liquidationStage(249_749_999n, n, mark, 500n, 6_667n)).toBe('backstop');
    // Neutral params: every liquidation is a full close
    expect(liquidationStage(300n * C6, n, mark, 500n, 10_000n)).toBe('backstop');
    expect(liquidationStage(-1n, 0n, mark, 500n, 6_667n)).toBe('backstop');
  });
});

describe('LP flows', () => {
  const p = pool(1_000_000n * C6);
  const now = WEDNESDAY;
  const current = { ...oil, lastFundingTime: BigInt(now - 60) };

  it('gate deposits, requests and withdrawals as §9.5 does', () => {
    const v = poolView(p, current, 75n * C6, now);
    expect(lpGates(current, p, v, now)).toMatchObject({ deposit: [], request: [], withdraw: [], needsFundingCrank: false });
    // Inside W (production: a day before the close) only withdrawals close
    const friday = FRIDAY_CLOSE - 3_600;
    const g = lpGates({ ...current, lastFundingTime: BigInt(friday - 60) }, p, v, friday);
    expect(g).toMatchObject({ deposit: [], request: [], withdraw: ['LpFlowWindowClosed'], inWindow: true });
    // In Q after the Sunday open, deposits and withdrawals close
    const q = lpGates({ ...current, lastFundingTime: BigInt(SUNDAY_OPEN) }, p, v, SUNDAY_OPEN + 60);
    expect(q).toMatchObject({ deposit: ['LpFlowWindowClosed'], withdraw: ['LpFlowWindowClosed'], inQuiet: true });
    // Pending bad debt blocks deposits and withdrawals, never requests
    const debt = lpGates(current, { ...p, pendingBadDebtShort: 5n }, v, now);
    expect(debt).toMatchObject({ deposit: ['PoolAdlPending'], request: [], withdraw: ['PoolAdlPending'] });
    // Stale funding is cranked first (utils/fundingCrank.ts)
    expect(lpGates({ ...current, lastFundingTime: BigInt(now - 7_200) }, p, v, now).needsFundingCrank).toBe(true);
    // Weekend: closed
    expect(lpGates(current, p, v, FRIDAY_CLOSE + 60).deposit).toContain('MarketClosed');
    // Settled: withdrawals wait max(profit-close quiet, Q) = 1,800 s after settled_at (D41)
    const settled = { ...current, settlementPrice: 70n * C6, settledAt: BigInt(SATURDAY_NOON) };
    expect(lpGates(settled, p, v, SATURDAY_NOON + 1_799).withdraw).toEqual(['LpFlowWindowClosed']);
    expect(lpGates(settled, p, v, SATURDAY_NOON + 1_800).withdraw).toEqual([]);
  });

  it('values a request at the lower of request time and now, and finds its claim deadline', () => {
    const v = poolView(p, current, 75n * C6, now)!;
    const pos = {
      shares: 10_000n * C6,
      pendingShares: 10_000n * C6,
      requestedValue: 9_000n * C6,
      requestedAt: BigInt(now - 345_600),
      unlockAt: BigInt(now),
      depositedCum: 0n,
      withdrawnCum: 0n,
      claimWindowSecs: 259_200n,
    };
    const r = withdrawalRequest(current, p, pos, v, now);
    expect(r.state).toBe('claimable');
    expect(r.payout).toBe(9_000n * C6);
    expect(r.valueNow).toBe(9_999_999_999n); // NAV_redeem keeps the 1-unit slack
    // 72 open hours after the unlock: across the weekend
    expect(openSecondsBetween(SCHEDULE_CME_GLOBEX, now, r.claimDeadline!)).toBe(259_200);
    expect(withdrawalRequest(current, p, pos, v, r.claimDeadline!).state).toBe('claimable');
    expect(withdrawalRequest(current, p, pos, v, r.claimDeadline! + 1).state).toBe('lapsed');
    expect(withdrawalRequest(current, p, pos, v, now - 1).state).toBe('waiting');
    expect(withdrawalRequest(current, p, { ...pos, pendingShares: 0n }, v, now).state).toBe('none');
    expect(withdrawPostChecks(current, p, v, r.payout)).toBeNull();
    expect(withdrawPostChecks(current, { ...p, cash: 1n }, v, r.payout)).toBe('PoolCashInsufficient');
  });

  it('refuses a withdrawal that would leave G·net over α·B\' (D43), unless W = 0 or settled', () => {
    // $1M pool short 6,000 contracts to traders at $75 (U = 0): $450k of net
    const n = 6_000n * C6;
    const m = { ...current, longOpenInterest: n };
    const big = { ...pool(1_000_000n * C6), longEntrySum: n * 75n * C6, openPositions: 1n };
    const v = poolView(big, m, 75n * C6, now)!;
    // $200k out: G·$450k = $90k > α·$800k; the 0.6× hard line (B' ≥ $750k) still holds
    expect(withdrawRefusal(m, big, v, 200_000n * C6)).toEqual({ error: 'PoolCapWouldBreach', line: 'weekend' });
    expect(withdrawPostChecks(m, big, v, 200_000n * C6)).toBe('PoolCapWouldBreach');
    expect(withdrawPostChecks(m, big, v, 50_000n * C6)).toBeNull();
    expect(withdrawPostChecks(m, { ...big, params: { ...big.params, weekendCapWindowSecs: 0n } }, v, 200_000n * C6)).toBeNull();
    expect(withdrawPostChecks({ ...m, settlementPrice: 75n * C6 }, big, v, 200_000n * C6)).toBeNull();
    // Past the hard line it reports that one
    expect(withdrawRefusal(m, big, v, 500_000n * C6)?.line).toBe('hard_net');
  });

  it('leaves LPs the 20% exit buffer with traders at the 0.4× net cap (D45)', () => {
    // $1M pool, no skew premium owed (skew 0 here), net filled to the
    // production cap: 5,333.33 contracts long at $75 = $399,999.75
    const params = { ...productionPoolParams(), skewScale: 0n };
    const n = 5_333_330_000n;
    const m = { ...current, longOpenInterest: n };
    const full = { ...pool(1_000_000n * C6, params), longEntrySum: n * 75n * C6, openPositions: 1n };
    const v = poolView(full, m, 75n * C6, now)!;
    expect(checkOpenCaps(m, full, v, true, 10_000n, 75n * C6, 0n, 0n, now)).toBe('PoolExposureLimit');
    // B' ≥ G·net/α = 2·net: the room is B − $799,999.50 ≈ 20% of B, and it is exact
    const room = withdrawRoom(m, full, v);
    expect(room).toBe(v.navRedeem - 799_999_500_000n);
    expect((room * 10_000n) / v.navRedeem).toBe(2_000n);
    expect(withdrawRefusal(m, full, v, room)).toBeNull();
    expect(withdrawRefusal(m, full, v, room + 1n)).toEqual({ error: 'PoolCapWouldBreach', line: 'weekend' });
    // The hard line alone (0.6×) would leave a third of B
    const noWeekend = { ...full, params: { ...params, weekendCapWindowSecs: 0n } };
    expect((withdrawRoom(m, noWeekend, v) * 10_000n) / v.navRedeem).toBe(3_333n);
    // A 10% price rise (pool short) leaves about 8% of B, (0.2 − 1.2·0.1)·B
    // (§3.1): net_notional $440k needs B' ≥ $880k, and B falls by the
    // traders' $40k gain to ≈ $960k, so ≈ $80k of room, 60% of the buffer gone
    const up = poolView(full, m, 82_500_000n, now)!;
    expect(up.navRedeem < v.navRedeem - 39_999n * C6).toBe(true);
    expect((withdrawRoom(m, full, up) * 10_000n) / up.navRedeem).toBeLessThan(900n);
    // The buffer the vault states comes from the live params: 20% at
    // production, 0 at the α/G edge or with the weekend IM raised to 25%,
    // negative past it, none with the weekend bound off
    expect(exitBufferBps(m, full)).toBe(2_000n);
    expect(exitBufferBps(m, { ...full, params: { ...params, kNetBps: 5_000n } })).toBe(0n);
    expect(exitBufferBps({ ...m, weekendInitialMarginRatio: 2_500n }, full)).toBe(0n);
    expect(exitBufferBps({ ...m, weekendInitialMarginRatio: 3_000n }, full)).toBe(-2_000n);
    expect(exitBufferBps(m, noWeekend)).toBeNull();
    // Settled or flat: only the cash bounds it
    expect(withdrawRoom({ ...m, settlementPrice: 75n * C6 }, full, v)).toBe(full.cash);
  });

  it('shares the exit room first come first served, pre-fill requests included (D45 review)', () => {
    // B = $2.5M; LP E requested $500k before any net, so capNAV = $2M, and
    // traders filled the net to 0.4·capNAV = $800k (10,666.66 contracts at $75)
    const params = { ...productionPoolParams(), skewScale: 0n };
    const n = 10_666_660_000n;
    const m = { ...current, longOpenInterest: n };
    const base = pool(2_500_000n * C6, params);
    const filled = { ...base, pendingWithdrawShares: 500_000n * C6, longEntrySum: n * 75n * C6, openPositions: 1n };
    const v = poolView(filled, m, 75n * C6, now)!;
    expect(v.capNav > 1_999_999n * C6 && v.capNav <= 2_000_000n * C6).toBe(true);
    expect(checkOpenCaps(m, filled, v, true, 10_000n, 75n * C6, 0n, 0n, now)).toBe('PoolExposureLimit');
    // The room is (0.2 + 0.8p)·B = $900k, pool-wide: B' must stay ≥ 2·net_notional
    const room = withdrawRoom(m, filled, v);
    expect(room).toBe(v.navRedeem - 1_599_999_000_000n);
    // E's $500k fits now, and so does a $450k claim by LP L who requested
    // after the fill (more than the 20% of the $2M that stays)
    expect(withdrawRefusal(m, filled, v, 500_000n * C6)).toBeNull();
    expect(withdrawRefusal(m, filled, v, 450_000n * C6)).toBeNull();
    // L claims first: $450k and its shares leave, E's request is still pending
    const afterL = { ...filled, cash: filled.cash - 450_000n * C6, lpShares: filled.lpShares - 450_000n * C6 };
    const vl = poolView(afterL, m, 75n * C6, now)!;
    expect(withdrawRoom(m, afterL, vl)).toBe(vl.navRedeem - 1_599_999_000_000n);
    // capNAV kept traders off E's capital, but not the other LP: E's claim,
    // requested before the fill, is now refused on the weekend line
    expect(withdrawRefusal(m, afterL, vl, 500_000n * C6)).toEqual({ error: 'PoolCapWouldBreach', line: 'weekend' });
  });

  it('mints deposit shares at NAV_mint, the first deposit locking the dead shares', () => {
    const v = poolView(p, current, 75n * C6, now)!;
    // NAV_mint = cash + 1 (slack): just under one share per unit
    expect(depositShares(p, v, 1_000n * C6)).toBe(999_999_999n);
    const empty = pool(0n);
    expect(depositShares({ ...empty, lpShares: 0n }, poolView(empty, current, 75n * C6, now)!, 5n * C6)).toBe(4n * C6);
  });
});

describe('admin helpers', () => {
  it('keeps every market config field and leaves max OI alone unless set', () => {
    const m = { ...oil, isPaused: true, minOraclePrice: 5n, maxConfidenceBps: 200 };
    expect(marketConfigParams(m)).toEqual({
      isPaused: true,
      sessionSchedule: SCHEDULE_CME_GLOBEX,
      preCloseWindowSecs: 7_200,
      weekendInitialMarginRatio: 2_000,
      weekendMaintenanceMarginRatio: 1_000,
      maxConfidenceBps: 200,
      minOraclePrice: 5n,
      maxOpenInterest: 0n,
    });
    expect(marketConfigParams(m, { maxOpenInterest: 7n, isPaused: false })).toMatchObject({ maxOpenInterest: 7n, isPaused: false });
  });

  it('reports pool status and the proposal timelock', () => {
    const p = pool(1_000_000n * C6);
    const v = poolView(p, oil, 75n * C6, WEDNESDAY);
    expect(poolStatus(oil, p, v, WEDNESDAY)).toMatchObject({
      hasLps: true, adlRequired: false, ddBps: 0n, nTrig: false, dTrig: false, wTrig: false, solvencyHaircut: false, paramsApplyIn: null,
    });
    // Peak 10% above: drawdown 1,000 bps, the freeze line; the ADL line is 1,500
    const dd = { ...p, npsPeak: 1_111_112n, npsPeakAt: BigInt(WEDNESDAY - 60) };
    const s = poolStatus(oil, dd, poolView(dd, oil, 75n * C6, WEDNESDAY), WEDNESDAY);
    expect([s.ddBps, s.ddFreeze, s.dTrig]).toEqual([1_000n, true, false]);
    // A peak older than 7 days resets
    const old = { ...dd, npsPeakAt: BigInt(WEDNESDAY - 8 * 86_400) };
    expect(poolStatus(oil, old, v, WEDNESDAY).ddBps).toBe(0n);
    expect(poolStatus(oil, { ...p, pendingParamsAt: BigInt(WEDNESDAY + 100) }, v, WEDNESDAY).paramsApplyIn).toBe(100);
    // Production 4 d delay + 1 d window + 5 d margin
    expect(proposalTimelock(p, productionPoolParams())).toBe(10n * 86_400n);
    expect(proposalTimelock({ ...p, lpShares: 0n }, productionPoolParams())).toBe(0n);
  });
});
