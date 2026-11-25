// Pure decisions of the localnet harness (no RPC, no state file), so
// selftest.ts can check them without a validator: when the price driver
// prints a reopen, how the checker decides it actually checked enough, how a
// weekend gap and the bad debt are measured, and the gapper's sizing.

import { isOpen, secondsSinceOpen } from "../../../frontend/src/utils/session";

/** Length of the SCHEDULE_TEST_HOURLY week and its weekend close (docs/localnet.md). */
export const TEST_WEEK = 3_600;
export const TEST_WEEKLY_CLOSE = 3_000;

// ---------------------------------------------------------------------------
// Price driver: the reopen print
// ---------------------------------------------------------------------------

export interface ReopenPlan {
  /** Seconds from `t` to the next open second (1..horizon). */
  inSecs: number;
  /** The reopen after the weekend (else the end of a daily break). */
  weekStart: boolean;
  /** 1, 2 or 3 for the daily breaks at c = 750, 1500, 2250; 0 for the weekend. */
  breakIndex: number;
}

/** The next reopen within `horizon` seconds of `t`, or null (open, or further away). */
export function nextReopen(schedule: number, t: number, horizon = 10): ReopenPlan | null {
  if (isOpen(schedule, t)) return null;
  for (let k = 1; k <= horizon; k++) {
    if (!isOpen(schedule, t + k)) continue;
    const at = t + k;
    const weekStart = ((at % TEST_WEEK) + TEST_WEEK) % TEST_WEEK === 0;
    const c = ((t % TEST_WEEK) + TEST_WEEK) % TEST_WEEK;
    return { inSecs: k, weekStart, breakIndex: weekStart ? 0 : Math.floor(c / 750) };
  }
  return null;
}

/** The daily break (1..3) `t` is in, 0 in the weekend or while open. */
export function breakIndexAt(schedule: number, t: number): number {
  if (isOpen(schedule, t)) return 0;
  const c = ((t % TEST_WEEK) + TEST_WEEK) % TEST_WEEK;
  return c >= TEST_WEEKLY_CLOSE ? 0 : Math.floor(c / 750);
}

/**
 * Whether the driver sends the reopen print now: in the last `lead` closed
 * seconds before the reopen, when the market cannot trade, so the first
 * open slot already carries it (no window in which an open or a fill lands
 * at the pre-gap price). Withheld for the stale-break reopen.
 */
export function printReopenNow(plan: ReopenPlan | null, lead: number, staleBreak: number): boolean {
  if (!plan || plan.inSecs > lead) return false;
  return plan.weekStart || plan.breakIndex !== staleBreak;
}

// ---------------------------------------------------------------------------
// Checker: did it check enough?
// ---------------------------------------------------------------------------

export interface CheckCoverage {
  /** Check cycles started (snapshot attempts). */
  cycles: number;
  /** Consistent snapshots actually checked. */
  consistent: number;
  /** Cycles that threw (RPC, decode, clock). */
  failures: number;
  /** Cycles with no consistent snapshot in 10 tries. */
  skippedBusy: number;
  /** Largest chain-time gap between consecutive checked snapshots (s). */
  maxGapSecs: number;
  /** Session edges (closes and reopens) seen; each needs a checked snapshot. */
  edgesMissed: number;
}

export interface CoverageLimits {
  maxGapSecs: number;
  maxUncheckedFrac: number;
}

/** Why a run that found no violation still cannot PASS (empty: it checked enough). */
export function coverageProblems(c: CheckCoverage, lim: CoverageLimits): string[] {
  const out: string[] = [];
  if (c.consistent === 0) out.push(`no consistent snapshot was checked (${c.cycles} cycles, ${c.failures} failed, ${c.skippedBusy} busy)`);
  const unchecked = c.failures + c.skippedBusy;
  if (c.cycles > 0 && unchecked / c.cycles > lim.maxUncheckedFrac) {
    out.push(`${unchecked} of ${c.cycles} check cycles checked nothing (${c.failures} failed, ${c.skippedBusy} busy), over ${(lim.maxUncheckedFrac * 100).toFixed(0)}%`);
  }
  if (c.consistent > 0 && c.maxGapSecs > lim.maxGapSecs) out.push(`${c.maxGapSecs} s between checked snapshots (limit ${lim.maxGapSecs} s)`);
  if (c.edgesMissed > 0) out.push(`${c.edgesMissed} session edges with no checked snapshot on both sides`);
  return out;
}

// ---------------------------------------------------------------------------
// Report: weekend gaps and bad debt
// ---------------------------------------------------------------------------

export interface PriceRow {
  t: number;
  price: number;
  event?: string | null;
  ok?: boolean;
}

/**
 * The weekend gap of the reopen at `reopenT` as the price driver printed it:
 * its `weekend_gap` print (sent in the last closed seconds, or late after a
 * missed slot) against the last successful print before it.
 */
export function weekendGapAt(rows: PriceRow[], reopenT: number): { before: number; after: number; gapPct: number; printedAt: number } | null {
  let idx = -1;
  for (let i = 0; i < rows.length; i++) {
    const r = rows[i];
    if (r.ok === false || !r.event || !r.event.startsWith("weekend_gap")) continue;
    if (r.t >= reopenT - 30 && r.t <= reopenT + 60) {
      idx = i;
      break;
    }
  }
  if (idx < 0) return null;
  for (let j = idx - 1; j >= 0; j--) {
    if (rows[j].ok === false) continue;
    const before = rows[j].price;
    const after = rows[idx].price;
    return { before, after, gapPct: Number((((after - before) / before) * 100).toFixed(2)), printedAt: rows[idx].t };
  }
  return null;
}

export interface EventDetail {
  name: string;
  [k: string]: unknown;
}

/**
 * Every BadDebt event once, wherever it came from (keeper liquidations, bot
 * closes, ADL): `PositionLiquidated` carries the same deficit again, so it
 * is not counted.
 */
export function badDebtEvents(sources: { t: number; by: string; detail?: EventDetail[] }[]): (EventDetail & { t: number; by: string })[] {
  const out: (EventDetail & { t: number; by: string })[] = [];
  // The Anchor coder names event fields in camelCase (insurancePaid); the
  // report uses the program's snake_case names
  const pick = (d: EventDetail, snake: string) => d[snake] ?? d[snake.replace(/_([a-z])/g, (_m, c: string) => c.toUpperCase())] ?? "0";
  for (const s of sources) {
    for (const d of s.detail ?? []) {
      if (d.name !== "BadDebt") continue;
      out.push({
        ...d,
        insurance_paid: pick(d, "insurance_paid"),
        lp_absorbed: pick(d, "lp_absorbed"),
        pending_added: pick(d, "pending_added"),
        t: s.t,
        by: s.by,
      });
    }
  }
  return out;
}

// ---------------------------------------------------------------------------
// Gapper sizing
// ---------------------------------------------------------------------------

/**
 * Leverage (3 dp) at which a house open's equity right after the open, net
 * of an impact of up to `maxImpactBps`, already covers the weekend
 * maintenance margin plus `bufferBps`: the gapper then carries a thin but
 * valid margin through the pre-close window instead of being liquidated as
 * the window raises maintenance to the weekend ratio. Capped by `maxLev`.
 */
export function gapperLeverage(weekendMmrBps: number, mmrBps: number, bufferBps: number, maxImpactBps: number, maxLev: number): number {
  const need = Math.max(weekendMmrBps, mmrBps) + bufferBps + maxImpactBps;
  return Math.min(maxLev, Math.floor(10_000_000 / need));
}

/** Equity the gapper tops up to: the larger of the two maintenance ratios plus the buffer, on mark notional. */
export function gapperTopUpTarget(markNotional: bigint, weekendMmrBps: number, mmrBps: number, bufferBps: number): bigint {
  const r = BigInt(Math.max(weekendMmrBps, mmrBps) + bufferBps);
  return (markNotional * r) / 10_000n;
}

export { secondsSinceOpen };
