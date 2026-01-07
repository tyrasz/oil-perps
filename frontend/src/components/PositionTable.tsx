import { usePositions } from '../hooks/usePositions';
import { useMarketStore } from '../stores/marketStore';
import type { Position } from '../types';

export function PositionTable() {
  const { positions } = usePositions();
  const { market } = useMarketStore();

  const handleClosePosition = async (position: Position) => {
    // TODO: Implement close position transaction
    console.log('Closing position:', position.address);
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
            <th>Actions</th>
          </tr>
        </thead>
        <tbody>
          {positions.map((position) => {
            const pnlClass = position.unrealizedPnl >= 0 ? 'positive' : 'negative';
            const marginClass = position.marginRatio < 10 ? 'danger' : position.marginRatio < 20 ? 'warning' : '';

            return (
              <tr key={position.address}>
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
                <td>
                  <button
                    className="close-btn"
                    onClick={() => handleClosePosition(position)}
                  >
                    Close
                  </button>
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}
