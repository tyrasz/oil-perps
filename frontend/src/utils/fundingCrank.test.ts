import { describe, it, expect, vi } from 'vitest';
import { decodeLpPool, decodeMarket, lpGates, type MarketState } from './pool';
import { productionPoolParams } from './pool';
import { EMPTY_POOL_STATE } from './poolMath';
import { ensureFundingCurrent, planFundingCrank, FUNDING_BOUNDARY_MARGIN_SECS } from './fundingCrank';
import { errorCode } from './errors';

// Always-open schedule (0), interval 3600, as in the review's case
const L = 1_794_416_400;
const always = (last: number, carry: number, extra: Partial<MarketState> = {}): MarketState => ({
  ...decodeMarket({ sessionSchedule: 0, fundingInterval: 3_600, lastFundingTime: last, fundingCarrySecs: carry }),
  ...extra,
});
// Wednesday 12:00 ET on the CME schedule (in session)
const WEDNESDAY = 1_794_416_400;
const cme = (last: number, carry = 0) => decodeMarket({ sessionSchedule: 1, fundingInterval: 3_600, lastFundingTime: last, fundingCarrySecs: carry });
const pool = { ...decodeLpPool({}), ...EMPTY_POOL_STATE, params: productionPoolParams() };

const anchorError = (name: string) => ({ error: { errorCode: { number: errorCode(name) } } });

describe('planFundingCrank', () => {
  it('the 15 s snapshot would crank where the chain has already cranked (review finding)', () => {
    // Snapshot at L+3598 still shows last = L; the chain cranked at L+3601 (carry 1)
    const stale = always(L, 0);
    const chain = always(L + 3_601, 1);
    // The old decision, on the snapshot: crank (update_funding then fails FundingNotDue)
    expect(lpGates(stale, pool, null, L + 3_605).needsFundingCrank).toBe(true);
    // The decision on the fresh account: funding is current, send no crank
    expect(planFundingCrank(chain, L + 3_605)).toEqual({ kind: 'none' });
  });

  it('cranks when a period is due', () => {
    expect(planFundingCrank(always(L, 0), L + 3_600)).toEqual({ kind: 'crank' });
    expect(planFundingCrank(always(L, 5), L + 3_595)).toEqual({ kind: 'crank' });
    expect(planFundingCrank(always(L, 0), L + 7 * 3_600)).toEqual({ kind: 'crank' });
  });

  it('waits out a boundary that is too close to call', () => {
    // 10 s before due: a transaction sent now may land either side
    expect(planFundingCrank(always(L, 0), L + 3_590)).toEqual({ kind: 'wait', secs: 12 });
    expect(planFundingCrank(always(L, 0), L + 3_600 - FUNDING_BOUNDARY_MARGIN_SECS)).toEqual({
      kind: 'wait',
      secs: FUNDING_BOUNDARY_MARGIN_SECS + 2,
    });
    // Clearly current
    expect(planFundingCrank(always(L, 0), L + 3_600 - FUNDING_BOUNDARY_MARGIN_SECS - 1)).toEqual({ kind: 'none' });
  });

  it('counts open seconds only on a CME market, and never cranks closed or settled', () => {
    // Last crank an hour before Friday's close; Saturday is closed: no crank
    const friClose = 1_794_607_200; // Friday 17:00 ET
    expect(planFundingCrank(cme(friClose - 3_000), friClose + 86_400)).toEqual({ kind: 'none' });
    // Sunday 18:00 open + 600 s: 3000 + 600 = 3600 open seconds: due
    const sunOpen = friClose + 49 * 3_600;
    expect(planFundingCrank(cme(friClose - 3_000), sunOpen + 600)).toEqual({ kind: 'crank' });
    // 10 open seconds before the Friday close: due only after the weekend, so current until the close
    expect(planFundingCrank(cme(friClose - 3_590), friClose - 10)).toEqual({ kind: 'none' });
    // In session, well before due
    expect(planFundingCrank(cme(WEDNESDAY - 60), WEDNESDAY)).toEqual({ kind: 'none' });
    // Settled: funding_current is true
    expect(planFundingCrank(always(L, 0, { settlementPrice: 50_000_000n }), L + 10_000)).toEqual({ kind: 'none' });
  });
});

describe('ensureFundingCurrent', () => {
  const deps = (markets: MarketState[], times: number[], crank: () => Promise<unknown> = async () => 'sig') => {
    let i = 0;
    let j = 0;
    return {
      fetchMarket: vi.fn(async () => markets[Math.min(i++, markets.length - 1)]),
      chainNow: vi.fn(async () => times[Math.min(j++, times.length - 1)]),
      fallbackNow: () => 0,
      sendCrank: vi.fn(crank),
      sleep: vi.fn(async () => undefined),
    };
  };

  it('sends no crank when the fresh account is current', async () => {
    const d = deps([always(L + 3_601, 1)], [L + 3_605]);
    const r = await ensureFundingCurrent(d);
    expect(d.sendCrank).not.toHaveBeenCalled();
    expect(r).toMatchObject({ cranked: false, now: L + 3_605 });
  });

  it('cranks in its own transaction, then re-reads', async () => {
    const d = deps([always(L, 0), always(L + 3_605, 5)], [L + 3_605, L + 3_606]);
    const r = await ensureFundingCurrent(d);
    expect(d.sendCrank).toHaveBeenCalledTimes(1);
    expect(r.cranked).toBe(true);
    expect(r.market.lastFundingTime).toBe(BigInt(L + 3_605));
  });

  it('ignores FundingNotDue (someone else cranked first)', async () => {
    const d = deps([always(L, 0), always(L + 3_601, 1)], [L + 3_605], async () => {
      throw anchorError('FundingNotDue');
    });
    const r = await ensureFundingCurrent(d);
    expect(d.sendCrank).toHaveBeenCalledTimes(1);
    expect(r.cranked).toBe(false);
  });

  it('rethrows any other crank failure', async () => {
    const d = deps([always(L, 0)], [L + 3_605], async () => {
      throw anchorError('StaleOraclePrice');
    });
    await expect(ensureFundingCurrent(d)).rejects.toEqual(anchorError('StaleOraclePrice'));
  });

  it('waits at the boundary, then cranks', async () => {
    const d = deps([always(L, 0), always(L, 0), always(L + 3_602, 2)], [L + 3_590, L + 3_602, L + 3_603]);
    const r = await ensureFundingCurrent(d);
    expect(d.sleep).toHaveBeenCalledWith(12_000);
    expect(d.sendCrank).toHaveBeenCalledTimes(1);
    expect(r.cranked).toBe(true);
  });

  it('uses the cluster clock, not the browser clock', async () => {
    // Browser 30 s behind the cluster: the browser would see 3580 s (current)
    const d = { ...deps([always(L, 0), always(L + 3_610, 10)], [L + 3_610]), fallbackNow: () => L + 3_580 };
    await ensureFundingCurrent(d);
    expect(d.sendCrank).toHaveBeenCalledTimes(1);
  });

  it('falls back to the local clock when the Clock sysvar cannot be read', async () => {
    const d = { ...deps([always(L, 0)], [null as unknown as number]), fallbackNow: () => L + 100 };
    const r = await ensureFundingCurrent(d);
    expect(r.now).toBe(L + 100);
    expect(d.sendCrank).not.toHaveBeenCalled();
  });
});
