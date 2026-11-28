// The price pusher's decisions (docs/lp-pool.md §18 #21, D54), free of I/O
// so the self-test can cover them: when a market's push-feed account needs a
// newer Hermes update, and how far the stored price lags Hermes.

export interface PusherPolicy {
  /** Push when Hermes has moved at least this far from the stored price (bps). */
  deviationBps: number;
  /** Push when the stored update is at least this old (s), whatever the price. */
  heartbeatSecs: number;
  /** Report the feed as lagging when the stored price is this far from Hermes (bps). */
  lagAlertBps: number;
}

/**
 * Defaults from §18 #21: a deviation threshold well under the round-trip fee
 * (5 + 50 bps with initialize-market.ts's values; at most 10 bps) and a
 * heartbeat well under `MAX_PRICE_AGE_SECS` (60 s; at most 10 s).
 */
export const DEFAULT_POLICY: PusherPolicy = { deviationBps: 10, heartbeatSecs: 10, lagAlertBps: 10 };

/** The policy's bounds: §18 #21's maxima, and an alert line at or above the push line. */
export function checkPolicy(p: PusherPolicy): void {
  if (!Number.isFinite(p.deviationBps) || p.deviationBps <= 0 || p.deviationBps > 10) {
    throw new Error(`deviation threshold ${p.deviationBps} bps: must be in (0, 10] (docs/lp-pool.md §18 #21)`);
  }
  if (!Number.isFinite(p.heartbeatSecs) || p.heartbeatSecs <= 0 || p.heartbeatSecs > 10) {
    throw new Error(`heartbeat ${p.heartbeatSecs} s: must be in (0, 10] (docs/lp-pool.md §18 #21)`);
  }
  if (!Number.isFinite(p.lagAlertBps) || p.lagAlertBps < p.deviationBps) {
    throw new Error(`lag alert ${p.lagAlertBps} bps: must be at least the deviation threshold`);
  }
}

/** A price and its publish time, as Hermes or the account has it (integer price at `exponent`). */
export interface Quote {
  price: bigint;
  exponent: number;
  publishTime: number;
}

export type Decision =
  /** No account yet: the first push creates it (before initialize_market can run). */
  | { push: true; reason: 'missing' }
  | { push: true; reason: 'deviation'; bps: number }
  | { push: true; reason: 'heartbeat'; ageSecs: number }
  /** Hermes has nothing newer than the account: the market is closed, or Hermes is behind. */
  | { push: false; reason: 'nothingNewer' }
  | { push: false; reason: 'withinPolicy'; bps: number; ageSecs: number };

/** |a − b| / b in bps, with `a` and `b` brought to the same exponent first. */
export function deviationBps(a: Quote, b: Quote): number {
  const e = Math.min(a.exponent, b.exponent);
  const scale = (q: Quote) => q.price * 10n ** BigInt(q.exponent - e);
  const x = scale(a);
  const y = scale(b);
  if (y <= 0n) return Number.POSITIVE_INFINITY;
  const diff = x > y ? x - y : y - x;
  // Integer bps, rounded up so a threshold is never undercut by truncation
  return Number((diff * 10_000n + y - 1n) / y);
}

/**
 * Whether to push `hermes` over `stored` (the account's update, or null if
 * the account does not exist) at clock `now`. A newer publish time is
 * required for any push: the push oracle refuses an update that is not
 * newer, and off hours Hermes repeats the last one.
 */
export function decide(stored: Quote | null, hermes: Quote, now: number, p: PusherPolicy): Decision {
  if (!stored) return { push: true, reason: 'missing' };
  if (hermes.publishTime <= stored.publishTime) return { push: false, reason: 'nothingNewer' };
  const bps = deviationBps(hermes, stored);
  if (bps >= p.deviationBps) return { push: true, reason: 'deviation', bps };
  const ageSecs = now - stored.publishTime;
  if (ageSecs >= p.heartbeatSecs) return { push: true, reason: 'heartbeat', ageSecs };
  return { push: false, reason: 'withinPolicy', bps, ageSecs };
}

/**
 * Whether the stored price lags Hermes past the alert line, which is the
 * signal to pause the market (§18 #21): only while Hermes is newer, since
 * an equal publish time is the same price.
 */
export function lagging(stored: Quote | null, hermes: Quote, p: PusherPolicy): boolean {
  if (!stored) return true;
  if (hermes.publishTime <= stored.publishTime) return false;
  return deviationBps(hermes, stored) >= p.lagAlertBps;
}
