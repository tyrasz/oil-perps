import { useState, useMemo } from 'react';
import { usePositions } from '../hooks/usePositions';
import { useMarketStore } from '../stores/marketStore';
import { useTrading } from '../hooks/useTrading';
import { useTriggerOrderStore } from '../stores/triggerOrderStore';
import { TpSlManager } from './TpSlManager';
import type { Position } from '../types';

// Liquidation thresholds
const LIQUIDATION_THRESHOLD = 5; // Critical - will be liquidated soon
const WARNING_THRESHOLD = 10; // Warning - getting close to liquidation

export function PositionTable() {
  const { positions } = usePositions();
  const { getCurrentMarket } = useMarketStore();
  const market = getCurrentMarket();
  const { closePosition, error, clearError, getExplorerUrl } = useTrading();
  const { getTriggerOrdersForPosition } = useTriggerOrderStore();

  const [closingPositionId, setClosingPositionId] = useState<string | null>(null);
  const [successMessage, setSuccessMessage] = useState<string | null>(null);
  const [lastSignature, setLastSignature] = useState<string | null>(null);
  const [expandedPositionId, setExpandedPositionId] = useState<string | null>(null);

  // Check for positions at risk of liquidation
  const { criticalPositions, warningPositions } = useMemo(() => {
    const critical = positions.filter(p => p.marginRatio < LIQUIDATION_THRESHOLD);
    const warning = positions.filter(p => p.marginRatio >= LIQUIDATION_THRESHOLD && p.marginRatio < WARNING_THRESHOLD);
    return { criticalPositions: critical, warningPositions: warning };
  }, [positions]);

  const handleClosePosition = async (position: Position) => {
    setClosingPositionId(position.address);
    clearError();
    setSuccessMessage(null);

    try {
      const signature = await closePosition(position.address);
      setLastSignature(signature);
      setSuccessMessage('Position closed successfully!');
      setTimeout(() => setSuccessMessage(null), 10000);
    } catch (err) {
      console.error('Failed to close position:', err);
    } finally {
      setClosingPositionId(null);
    }
  };

  if (positions.length === 0) {
    return (
      <div className="position-table empty">
        <p>No open positions</p>
      </div>
    );
  }

  return (
    <div className="position-table">
      {/* Critical liquidation warning */}
      {criticalPositions.length > 0 && (
        <div className="liquidation-alert critical">
          <span className="alert-icon">⚠️</span>
          <div className="alert-content">
            <strong>LIQUIDATION IMMINENT</strong>
            <p>
              {criticalPositions.length} position{criticalPositions.length > 1 ? 's' : ''} below {LIQUIDATION_THRESHOLD}% margin ratio.
              Add collateral or close position{criticalPositions.length > 1 ? 's' : ''} immediately to avoid liquidation.
            </p>
          </div>
        </div>
      )}

      {/* Warning for positions approaching liquidation */}
      {warningPositions.length > 0 && criticalPositions.length === 0 && (
        <div className="liquidation-alert warning">
          <span className="alert-icon">⚡</span>
          <div className="alert-content">
            <strong>Margin Warning</strong>
            <p>
              {warningPositions.length} position{warningPositions.length > 1 ? 's' : ''} approaching liquidation threshold.
              Consider adding collateral or reducing position size.
            </p>
          </div>
        </div>
      )}

      <table>
        <thead>
          <tr>
            <th>Side</th>
            <th>Size</th>
            <th>Entry Price</th>
            <th>Mark Price</th>
            <th>Leverage</th>
            <th>PnL</th>
            <th>Margin Ratio</th>
            <th>TP/SL</th>
            <th>Actions</th>
          </tr>
        </thead>
        <tbody>
          {positions.map((position) => {
            const pnlClass = position.unrealizedPnl >= 0 ? 'positive' : 'negative';
            const marginClass = position.marginRatio < 10 ? 'danger' : position.marginRatio < 20 ? 'warning' : '';
            const triggerOrders = getTriggerOrdersForPosition(position.address);
            const activeTp = triggerOrders.find(o => o.type === 'take_profit' && o.status === 'active');
            const activeSl = triggerOrders.find(o => o.type === 'stop_loss' && o.status === 'active');
            const isExpanded = expandedPositionId === position.address;

            return (
              <>
                <tr key={position.address} className={isExpanded ? 'expanded' : ''}>
                  <td className={position.side === 'long' ? 'long' : 'short'}>
                    {position.side.toUpperCase()}
                  </td>
                  <td>{position.size.toFixed(4)}</td>
                  <td>${position.entryPrice.toFixed(2)}</td>
                  <td>${market?.price.toFixed(2) || '-'}</td>
                  <td>{position.leverage}x</td>
                  <td className={pnlClass}>
                    ${position.unrealizedPnl.toFixed(2)}
                    <span className="pnl-percent">
                      ({((position.unrealizedPnl / position.collateral) * 100).toFixed(2)}%)
                    </span>
                  </td>
                  <td className={marginClass}>{position.marginRatio.toFixed(2)}%</td>
                  <td className="tpsl-cell">
                    <div className="tpsl-indicators">
                      {activeTp && (
                        <span className="tpsl-indicator tp" title={`TP: $${activeTp.triggerPrice.toFixed(2)}`}>
                          TP
                        </span>
                      )}
                      {activeSl && (
                        <span className="tpsl-indicator sl" title={`SL: $${activeSl.triggerPrice.toFixed(2)}`}>
                          SL
                        </span>
                      )}
                      {!activeTp && !activeSl && (
                        <span className="tpsl-indicator none">-</span>
                      )}
                    </div>
                  </td>
                  <td className="actions-cell">
                    <button
                      className="action-btn tpsl-btn"
                      onClick={() => setExpandedPositionId(isExpanded ? null : position.address)}
                      title="Set TP/SL"
                    >
                      {isExpanded ? '▼' : '▶'} TP/SL
                    </button>
                    <button
                      className="action-btn close-btn"
                      onClick={() => handleClosePosition(position)}
                      disabled={closingPositionId === position.address}
                    >
                      {closingPositionId === position.address ? '...' : 'Close'}
                    </button>
                  </td>
                </tr>
                {isExpanded && (
                  <tr className="expanded-row">
                    <td colSpan={9}>
                      <TpSlManager
                        position={position}
                        onClose={() => setExpandedPositionId(null)}
                      />
                    </td>
                  </tr>
                )}
              </>
            );
          })}
        </tbody>
      </table>

      {/* Error message */}
      {error && (
        <div className="error-message" onClick={clearError}>
          {error}
        </div>
      )}

      {/* Success message */}
      {successMessage && lastSignature && (
        <div className="success-message">
          {successMessage}{' '}
          <a
            href={getExplorerUrl(lastSignature)}
            target="_blank"
            rel="noopener noreferrer"
          >
            View tx
          </a>
        </div>
      )}
    </div>
  );
}
