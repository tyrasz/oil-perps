import { useMemo } from 'react';
import { usePositions } from '../hooks/usePositions';
import type { PortfolioMetrics, ClosedPosition } from '../types';

export function PortfolioAnalytics() {
  const { positions, closedPositions, userAccount } = usePositions();

  const metrics: PortfolioMetrics = useMemo(() => {
    const wins = closedPositions.filter((p: ClosedPosition) => p.realizedPnl > 0);
    const losses = closedPositions.filter((p: ClosedPosition) => p.realizedPnl < 0);

    const totalWins = wins.reduce((sum: number, p: ClosedPosition) => sum + p.realizedPnl, 0);
    const totalLosses = Math.abs(losses.reduce((sum: number, p: ClosedPosition) => sum + p.realizedPnl, 0));

    const unrealizedPnl = positions.reduce((sum, p) => sum + p.unrealizedPnl, 0);
    const totalCollateral = positions.reduce((sum, p) => sum + p.collateral, 0);
    const availableCollateral = userAccount?.collateralBalance ?? 0;

    return {
      totalValue: availableCollateral + totalCollateral + unrealizedPnl,
      totalPnl: (userAccount?.realizedPnl ?? 0) + unrealizedPnl,
      totalPnlPercent: availableCollateral > 0
        ? ((userAccount?.realizedPnl ?? 0) + unrealizedPnl) / availableCollateral * 100
        : 0,
      winRate: closedPositions.length > 0 ? (wins.length / closedPositions.length) * 100 : 0,
      totalTrades: closedPositions.length,
      avgWin: wins.length > 0 ? totalWins / wins.length : 0,
      avgLoss: losses.length > 0 ? totalLosses / losses.length : 0,
      largestWin: wins.length > 0 ? Math.max(...wins.map((p: ClosedPosition) => p.realizedPnl)) : 0,
      largestLoss: losses.length > 0 ? Math.min(...losses.map((p: ClosedPosition) => p.realizedPnl)) : 0,
      profitFactor: totalLosses > 0 ? totalWins / totalLosses : totalWins > 0 ? Infinity : 0,
    };
  }, [positions, closedPositions, userAccount]);

  const unrealizedPnl = positions.reduce((sum, p) => sum + p.unrealizedPnl, 0);

  return (
    <div className="portfolio-analytics">
      <div className="analytics-header">
        <h3>Portfolio Analytics</h3>
      </div>

      <div className="analytics-grid">
        <div className="metric-card primary">
          <span className="metric-label">Total Portfolio Value</span>
          <span className="metric-value">${metrics.totalValue.toFixed(2)}</span>
        </div>

        <div className="metric-card">
          <span className="metric-label">Total PnL</span>
          <span className={`metric-value ${metrics.totalPnl >= 0 ? 'positive' : 'negative'}`}>
            ${metrics.totalPnl.toFixed(2)}
            <span className="metric-percent">
              ({metrics.totalPnlPercent >= 0 ? '+' : ''}{metrics.totalPnlPercent.toFixed(2)}%)
            </span>
          </span>
        </div>

        <div className="metric-card">
          <span className="metric-label">Unrealized PnL</span>
          <span className={`metric-value ${unrealizedPnl >= 0 ? 'positive' : 'negative'}`}>
            ${unrealizedPnl.toFixed(2)}
          </span>
        </div>

        <div className="metric-card">
          <span className="metric-label">Realized PnL</span>
          <span className={`metric-value ${(userAccount?.realizedPnl ?? 0) >= 0 ? 'positive' : 'negative'}`}>
            ${(userAccount?.realizedPnl ?? 0).toFixed(2)}
          </span>
        </div>
      </div>

      <div className="analytics-section">
        <h4>Trading Performance</h4>
        <div className="stats-grid">
          <div className="stat-item">
            <span className="stat-label">Win Rate</span>
            <span className={`stat-value ${metrics.winRate >= 50 ? 'positive' : 'negative'}`}>
              {metrics.winRate.toFixed(1)}%
            </span>
          </div>

          <div className="stat-item">
            <span className="stat-label">Total Trades</span>
            <span className="stat-value">{metrics.totalTrades}</span>
          </div>

          <div className="stat-item">
            <span className="stat-label">Profit Factor</span>
            <span className={`stat-value ${metrics.profitFactor >= 1 ? 'positive' : 'negative'}`}>
              {metrics.profitFactor === Infinity ? '∞' : metrics.profitFactor.toFixed(2)}
            </span>
          </div>

          <div className="stat-item">
            <span className="stat-label">Avg Win</span>
            <span className="stat-value positive">${metrics.avgWin.toFixed(2)}</span>
          </div>

          <div className="stat-item">
            <span className="stat-label">Avg Loss</span>
            <span className="stat-value negative">-${metrics.avgLoss.toFixed(2)}</span>
          </div>

          <div className="stat-item">
            <span className="stat-label">Largest Win</span>
            <span className="stat-value positive">${metrics.largestWin.toFixed(2)}</span>
          </div>

          <div className="stat-item">
            <span className="stat-label">Largest Loss</span>
            <span className="stat-value negative">${Math.abs(metrics.largestLoss).toFixed(2)}</span>
          </div>

          <div className="stat-item">
            <span className="stat-label">Open Positions</span>
            <span className="stat-value">{positions.length}</span>
          </div>
        </div>
      </div>

      <div className="analytics-section">
        <h4>Position Breakdown</h4>
        <div className="position-breakdown">
          <div className="breakdown-item">
            <span className="breakdown-label">Long Positions</span>
            <span className="breakdown-value long">
              {positions.filter((p) => p.side === 'long').length}
            </span>
          </div>
          <div className="breakdown-item">
            <span className="breakdown-label">Short Positions</span>
            <span className="breakdown-value short">
              {positions.filter((p) => p.side === 'short').length}
            </span>
          </div>
          <div className="breakdown-item">
            <span className="breakdown-label">Total Collateral Locked</span>
            <span className="breakdown-value">
              ${positions.reduce((sum, p) => sum + p.collateral, 0).toFixed(2)}
            </span>
          </div>
          <div className="breakdown-item">
            <span className="breakdown-label">Available Collateral</span>
            <span className="breakdown-value">
              ${(userAccount?.collateralBalance ?? 0).toFixed(2)}
            </span>
          </div>
        </div>
      </div>

      {closedPositions.length === 0 && (
        <div className="no-data">
          <p>No closed trades yet. Analytics will populate as you trade.</p>
        </div>
      )}
    </div>
  );
}
