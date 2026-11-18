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
  /** Margin ratio in percent: (collateral + PnL + funding) / entry notional. */
  marginRatio: number;
}

export function positionHealth(p: PositionAmounts, currentPrice: number, funding = 0): PositionHealth {
  const unrealizedPnl = p.size * (p.isLong ? currentPrice - p.entryPrice : p.entryPrice - currentPrice);
  const equity = p.collateral + unrealizedPnl + funding;
  const notional = p.size * p.entryPrice;
  const marginRatio = equity > 0 && notional > 0 ? (equity / notional) * 100 : 0;
  return { unrealizedPnl, marginRatio };
}

/** Risk-relevant fields of a position's own market account. */
export interface PositionMarketParams {
  maintenanceMarginRatio: number; // bps
  weekendMaintenanceMarginRatio: number; // bps
  sessionSchedule: number;
  preCloseWindowSecs: number;
  settlementPrice: number; // raw, 6 decimals
}

export interface PositionMarketState {
  /** Margin ratio (%) below which the program liquidates now. */
  liquidationThreshold: number;
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
    return { liquidationThreshold: pct, warningThreshold: pct * 2, settlementPrice: null, closeBlockedReason: null };
  }
  const session = marketSession(market, ts);
  const weekendBps = Math.max(market.maintenanceMarginRatio, market.weekendMaintenanceMarginRatio);
  const liquidationBps = session.state === 'pre-close' ? weekendBps : market.maintenanceMarginRatio;
  const until = session.secondsUntilWeekendMargins;
  const weekendSoon = until != null && until <= WEEKEND_MARGIN_NOTICE_SECS;
  const liquidationThreshold = liquidationBps / 100;
  return {
    liquidationThreshold,
    warningThreshold: Math.max(liquidationThreshold * 2, weekendSoon ? weekendBps / 100 : 0),
    settlementPrice: market.settlementPrice > 0 ? market.settlementPrice / 1_000_000 : null,
    closeBlockedReason: session.state === 'closed' ? `Market closed. Trading resumes ${session.reopens}` : null,
  };
}
