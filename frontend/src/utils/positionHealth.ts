import { marketSession, WEEKEND_MARGIN_NOTICE_SECS } from './session';

// Position health as the program computes it (state/position.rs), so the
// liquidation warnings match what keepers can actually do.

export interface PositionAmounts {
  isLong: boolean;
  /** Contracts. */
  size: number;
  /** USDC. */
  collateral: number;
  entryPrice: number;
}

/**
 * Funding owed to (+) or by (-) the position, in USDC. `cumulativeFunding` and
 * `lastFundingPayment` are the market's funding index (USDC per contract) now
 * and when the position last settled.
 */
export function accruedFunding(isLong: boolean, size: number, cumulativeFunding: number, lastFundingPayment: number): number {
  const payment = size * (cumulativeFunding - lastFundingPayment);
  return isLong ? -payment : payment;
}

export interface PositionHealth {
  unrealizedPnl: number;
  /** Collateral + PnL + funding, USDC. */
  equity: number;
  /**
   * Margin ratio in percent on **mark** notional, equity / (size·mark): what
   * liquidation compares with the maintenance margin (docs/lp-pool.md §6.1).
   */
  marginRatio: number;
}

export function positionHealth(p: PositionAmounts, currentPrice: number, funding = 0): PositionHealth {
  const unrealizedPnl = p.size * (p.isLong ? currentPrice - p.entryPrice : p.entryPrice - currentPrice);
  const equity = p.collateral + unrealizedPnl + funding;
  const notional = p.size * currentPrice;
  const marginRatio = equity > 0 && notional > 0 ? (equity / notional) * 100 : 0;
  return { unrealizedPnl, equity, marginRatio };
}

/**
 * Mark price at which equity / mark notional falls to `marginBps` (§6.1):
 * long `(size·entry − C − f) / (size·(1 − m))`, short
 * `(C + size·entry + f) / (size·(1 + m))`. 0 when the position cannot
 * reach it (a long whose collateral covers the whole entry notional).
 */
export function liquidationPrice(p: PositionAmounts, marginBps: number, funding = 0): number {
  if (p.size <= 0) return 0;
  const m = marginBps / 10_000;
  const price = p.isLong
    ? (p.size * p.entryPrice - p.collateral - funding) / (p.size * (1 - m))
    : (p.collateral + p.size * p.entryPrice + funding) / (p.size * (1 + m));
  return Math.max(0, price);
}

/** Risk-relevant fields of a position's own market account. */
export interface PositionMarketParams {
  maintenanceMarginRatio: number; // bps
  weekendMaintenanceMarginRatio: number; // bps
  sessionSchedule: number;
  preCloseWindowSecs: number;
  settlementPrice: number; // raw, 6 decimals
  /** Pool liquidation params (LpPool.params); absent = neutral (every liquidation closes in full). */
  backstopThresholdBps?: number;
  liqPenaltyBps?: number;
}

export interface PositionMarketState {
  /** Margin ratio (%) below which the program liquidates now. */
  liquidationThreshold: number;
  /**
   * Margin ratio (%) of the backstop line: between it and the liquidation
   * threshold a keeper closes only part of the position (stage 1); below it
   * the whole position is closed (stage 2). Equal to the threshold when the
   * pool has no partial stage.
   */
  backstopThreshold: number;
  /** How liquidation works for this position, for display. */
  liquidationNote: string;
  /** Margin ratio (%) to warn below: twice the threshold, or the weekend margin due within a day. */
  warningThreshold: number;
  /** Settlement price when the market is settled; positions close at this price. */
  settlementPrice: number | null;
  /** Why the position can't be closed right now, if it can't. */
  closeBlockedReason: string | null;
}

const DEFAULT_MAINTENANCE_BPS = 500;

export function positionMarketState(market: PositionMarketParams | null | undefined, ts: number): PositionMarketState {
  if (!market) {
    const pct = DEFAULT_MAINTENANCE_BPS / 100;
    return {
      liquidationThreshold: pct,
      backstopThreshold: pct,
      liquidationNote: liquidationNote(pct, pct, 0),
      warningThreshold: pct * 2,
      settlementPrice: null,
      closeBlockedReason: null,
    };
  }
  const session = marketSession(market, ts);
  const weekendBps = Math.max(market.maintenanceMarginRatio, market.weekendMaintenanceMarginRatio);
  const liquidationBps = session.state === 'pre-close' ? weekendBps : market.maintenanceMarginRatio;
  const until = session.secondsUntilWeekendMargins;
  const weekendSoon = until != null && until <= WEEKEND_MARGIN_NOTICE_SECS;
  const liquidationThreshold = liquidationBps / 100;
  // backstop = floor(mmr·backstop_threshold_bps / 10_000), as the program
  const backstopBps = Math.floor((liquidationBps * (market.backstopThresholdBps ?? 10_000)) / 10_000);
  const backstopThreshold = backstopBps / 100;
  return {
    liquidationThreshold,
    backstopThreshold,
    liquidationNote: liquidationNote(liquidationThreshold, backstopThreshold, market.liqPenaltyBps ?? 0),
    warningThreshold: Math.max(liquidationThreshold * 2, weekendSoon ? weekendBps / 100 : 0),
    settlementPrice: market.settlementPrice > 0 ? market.settlementPrice / 1_000_000 : null,
    closeBlockedReason: session.state === 'closed' ? `Market closed. Trading resumes ${session.reopens}` : null,
  };
}

function liquidationNote(threshold: number, backstop: number, penaltyBps: number): string {
  const full = `Below ${backstop.toFixed(2)}% the position is closed in full and what is left of its collateral goes to the keeper and the insurance fund.`;
  if (backstop >= threshold) return `Liquidated on mark notional below ${threshold.toFixed(2)}% margin. ${full}`;
  return (
    `Below ${threshold.toFixed(2)}% margin (on mark notional) a keeper closes part of the position, ` +
    `with a ${(penaltyBps / 100).toFixed(2)}% penalty on the closed notional, until it is back above initial margin; you keep the rest. ` +
    full
  );
}
