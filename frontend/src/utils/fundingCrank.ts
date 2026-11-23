// Deciding whether an LP flow must crank funding first (docs/lp-pool.md §8,
// §9). lp_deposit, lp_request_withdrawal and lp_withdraw fail FundingStale
// unless funding is current; update_funding fails FundingNotDue unless a
// period is due. Which one holds depends on the market's *current*
// last_funding_time and carry (any trade cranks inline, D40) and on the
// cluster clock, so the decision is made on a freshly fetched Market at the
// cluster's time, and the crank (when needed) goes in its own transaction,
// with FundingNotDue ignored: the LP instruction can then never revert
// because of the crank.

import { PublicKey, type Connection } from '@solana/web3.js';
import { isSettled, type MarketState } from './pool';
import { isOpen, openSecondsBetween, timeAfterOpenSeconds } from './session';
import { errorName, programErrorCode } from './errors';

/**
 * Seconds either side of the due time in which the outcome is too close to
 * call: transaction latency and the gap between the Clock sysvar read and
 * the slot the transaction lands in.
 */
export const FUNDING_BOUNDARY_MARGIN_SECS = 20;

export type FundingPlan =
  /** Funding is current and stays so for longer than the margin (or the flow does not need it). */
  | { kind: 'none' }
  /** A period is due (or about to be): crank first, ignoring FundingNotDue. */
  | { kind: 'crank' }
  /** Due within the margin: wait `secs`, then decide again. */
  | { kind: 'wait'; secs: number };

/** What to do about funding before an LP flow on market `m` at cluster time `now`. */
export function planFundingCrank(m: MarketState, now: number, margin = FUNDING_BOUNDARY_MARGIN_SECS): FundingPlan {
  // Settled: funding_current is true. Closed: the flow and the crank both fail MarketClosed.
  if (isSettled(m) || !isOpen(m.sessionSchedule, now)) return { kind: 'none' };
  const interval = Number(m.fundingInterval);
  const total = openSecondsBetween(m.sessionSchedule, Number(m.lastFundingTime), now) + m.fundingCarrySecs;
  const left = interval - total;
  // Due now, or due before a transaction sent now would land
  if (left <= 0) return { kind: 'crank' };
  if (left > margin) return { kind: 'none' };
  const dueAt = timeAfterOpenSeconds(m.sessionSchedule, now, left);
  // The due time falls after a break: funding stays current until the close
  if (dueAt - now > margin) return { kind: 'none' };
  // Wait until it is clearly due, then crank
  return { kind: 'wait', secs: dueAt - now + 2 };
}

/** The cluster's unix time from the Clock sysvar (`unix_timestamp`, bytes 32..40). */
export async function clusterUnixTime(connection: Connection): Promise<number | null> {
  try {
    const info = await connection.getAccountInfo(SYSVAR_CLOCK);
    if (!info || info.data.length < 40) return null;
    const view = new DataView(info.data.buffer, info.data.byteOffset, info.data.byteLength);
    return Number(view.getBigInt64(32, true));
  } catch {
    return null;
  }
}

const SYSVAR_CLOCK = new PublicKey('SysvarC1ock11111111111111111111111111111111');

export interface FundingDeps {
  /** The market as it is on chain now. */
  fetchMarket: () => Promise<MarketState>;
  /** Cluster time in unix seconds (null: fall back to `fallbackNow`). */
  chainNow: () => Promise<number | null>;
  fallbackNow: () => number;
  /** Send update_funding as its own transaction and wait for confirmation. */
  sendCrank: () => Promise<unknown>;
  sleep: (ms: number) => Promise<void>;
}

/**
 * Make funding current for an LP flow: crank in its own transaction when a
 * period is due, waiting out the boundary when it is too close to call.
 * FundingNotDue from the crank means another crank (a keeper's, or a trade's
 * inline one) got there first, which is the outcome wanted. Returns the
 * market and time the decision was made on, and whether a crank landed.
 */
export async function ensureFundingCurrent(deps: FundingDeps, maxRounds = 3): Promise<{ market: MarketState; now: number; cranked: boolean }> {
  let cranked = false;
  for (let round = 0; ; round++) {
    const [market, t] = await Promise.all([deps.fetchMarket(), deps.chainNow()]);
    const now = t ?? deps.fallbackNow();
    const plan = planFundingCrank(market, now);
    if (plan.kind === 'none' || round >= maxRounds) return { market, now, cranked };
    if (plan.kind === 'wait') {
      await deps.sleep(plan.secs * 1000);
      continue;
    }
    try {
      await deps.sendCrank();
      cranked = true;
    } catch (err) {
      if (errorName(programErrorCode(err) ?? -1) !== 'FundingNotDue') throw err;
    }
    // Re-read: the next round returns once funding is current
  }
}
