import { Fragment, useState, useMemo } from 'react';
import { usePositions } from '../hooks/usePositions';
import { useMarketStore } from '../stores/marketStore';
import { useTrading } from '../hooks/useTrading';
import { useTriggerOrderStore } from '../stores/triggerOrderStore';
import { TpSlManager } from './TpSlManager';
import type { Position } from '../types';
import { formatLeverage } from '../utils/leverage';

const DEFAULT_MAINTENANCE_MARGIN_PCT = 5;

/** Whether settle_position_pnl has moved the PnL basis away from the open price. */
const basisMoved = (p: Position) => p.openedPrice !== undefined && Math.abs(p.openedPrice - p.entryPrice) > 1e-9;

export function PositionTable() {
  const { positions } = usePositions();
  const { markets } = useMarketStore();
  const { closePosition, addMargin, error, clearError, getExplorerUrl } = useTrading();
  const [marginInput, setMarginInput] = useState('');
  const { getTriggerOrdersForPosition } = useTriggerOrderStore();

  // Thresholds, settlement and trading hours come from each position's own
  // market (useOnChainPositions), since positions can span commodities.
  const thresholdOf = (p: Position) => p.liquidationThreshold ?? DEFAULT_MAINTENANCE_MARGIN_PCT;
  const warningOf = (p: Position) => p.warningThreshold ?? thresholdOf(p) * 2;

  const [closingPositionId, setClosingPositionId] = useState<string | null>(null);
  const [successMessage, setSuccessMessage] = useState<string | null>(null);
  const [lastSignature, setLastSignature] = useState<string | null>(null);
  const [expandedPositionId, setExpandedPositionId] = useState<string | null>(null);

  // Check for positions at risk of liquidation
  const { criticalPositions, warningPositions } = useMemo(() => {
    // Settled positions can't be liquidated; they only close at the settlement price
    const live = positions.filter(p => !p.settled);
    const critical = live.filter(p => p.marginRatio < thresholdOf(p));
    const warning = live.filter(p => p.marginRatio >= thresholdOf(p) && p.marginRatio < warningOf(p));
    return { criticalPositions: critical, warningPositions: warning };
    // thresholdOf/warningOf only read the positions passed in
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [positions]);

  const handleClosePosition = async (position: Position) => {
    setClosingPositionId(position.address);
    clearError();
    setSuccessMessage(null);

    try {
      // Quoted at the price the table shows; settled markets use the settlement price
      const signature = await closePosition(position.address, { markPrice: position.markPrice ?? markets[position.commodity]?.price });
      setLastSignature(signature);
      setSuccessMessage('Position closed successfully!');
      setTimeout(() => setSuccessMessage(null), 10000);
    } catch (err) {
      console.error('Failed to close position:', err);
    } finally {
      setClosingPositionId(null);
    }
  };

  const handleAddMargin = async (position: Position) => {
    const amount = parseFloat(marginInput);
    if (!(amount > 0)) return;
    clearError();
    setSuccessMessage(null);
    try {
      const signature = await addMargin(position.address, amount);
      setLastSignature(signature);
      setSuccessMessage(`Added $${amount.toFixed(2)} margin`);
      setMarginInput('');
      setTimeout(() => setSuccessMessage(null), 10000);
    } catch (err) {
      console.error('Failed to add margin:', err);
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
              {criticalPositions.length} position{criticalPositions.length > 1 ? 's' : ''} below the liquidation margin ({criticalPositions.map(p => `${p.commodity} ${thresholdOf(p)}%`).join(', ')}).
              Add collateral or close position{criticalPositions.length > 1 ? 's' : ''} immediately to avoid liquidation.
              {' '}Above the backstop line a keeper closes only part of a position, with a penalty; below it, all of it.
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
            <th title="Price the position opened at. The entry (PnL basis) moves when a keeper settles an unrealized loss.">Opened @</th>
            <th>Mark Price</th>
            <th title="Mark price at which the position becomes liquidatable (equity / mark notional below maintenance margin)">Liq. Price</th>
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
            const marginClass = position.settled
              ? ''
              : position.marginRatio < thresholdOf(position) ? 'danger' : position.marginRatio < warningOf(position) ? 'warning' : '';
            const markPrice = position.markPrice ?? markets[position.commodity]?.price;
            const triggerOrders = getTriggerOrdersForPosition(position.address);
            const activeTp = triggerOrders.find(o => o.type === 'take_profit' && o.status === 'active');
            const activeSl = triggerOrders.find(o => o.type === 'stop_loss' && o.status === 'active');
            const isExpanded = expandedPositionId === position.address;

            return (
              <Fragment key={position.address}>
                <tr className={isExpanded ? 'expanded' : ''}>
                  <td className={position.side === 'long' ? 'long' : 'short'}>
                    {position.side.toUpperCase()} <span className="position-commodity">{position.commodity}</span>
                  </td>
                  <td>{position.size.toFixed(4)}</td>
                  <td title={basisMoved(position) ? `PnL basis $${position.entryPrice.toFixed(2)}: a loss was settled into the pool` : undefined}>
                    ${(position.openedPrice ?? position.entryPrice).toFixed(2)}
                    {basisMoved(position) && <span className="pnl-percent"> (basis ${position.entryPrice.toFixed(2)})</span>}
                  </td>
                  <td title={position.settled ? 'Settlement price' : undefined}>
                    {markPrice !== undefined ? `$${markPrice.toFixed(2)}` : '-'}
                    {position.settled && ' (settled)'}
                  </td>
                  <td title={position.liquidationNote}>
                    {position.liquidationPrice !== undefined && position.liquidationPrice > 0 ? `$${position.liquidationPrice.toFixed(2)}` : '-'}
                  </td>
                  <td>{formatLeverage(position.leverage)}x</td>
                  <td className={pnlClass}>
                    ${position.unrealizedPnl.toFixed(2)}
                    <span className="pnl-percent">
                      ({((position.unrealizedPnl / position.collateral) * 100).toFixed(2)}%)
                    </span>
                  </td>
                  <td className={marginClass} title={`Equity / mark notional. ${position.liquidationNote ?? ''}`}>
                    {position.marginRatio.toFixed(2)}%
                  </td>
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
                      disabled={closingPositionId === position.address || !!position.closeBlockedReason}
                      title={position.closeBlockedReason ?? undefined}
                    >
                      {closingPositionId === position.address ? '...' : 'Close'}
                    </button>
                  </td>
                </tr>
                {isExpanded && (
                  <tr className="expanded-row">
                    <td colSpan={11}>
                      <TpSlManager
                        position={position}
                        onClose={() => setExpandedPositionId(null)}
                      />
                      {!position.settled && (
                        <div className="add-margin-row">
                          <span className="liquidation-note">{position.liquidationNote}</span>
                          <input
                            type="number"
                            min="0"
                            step="0.01"
                            placeholder="USDC"
                            value={marginInput}
                            onChange={(e) => setMarginInput(e.target.value)}
                          />
                          <button
                            className="action-btn"
                            onClick={() => handleAddMargin(position)}
                            disabled={!(parseFloat(marginInput) > 0)}
                          >
                            Add margin
                          </button>
                        </div>
                      )}
                    </td>
                  </tr>
                )}
              </Fragment>
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
