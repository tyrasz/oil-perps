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
