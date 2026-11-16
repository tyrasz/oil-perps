import { usePositions } from '../hooks/usePositions';

export function TradeHistory() {
  const { closedPositions } = usePositions();

  if (closedPositions.length === 0) {
    return (
      <div className="trade-history empty">
        <p>No trade history</p>
      </div>
    );
  }

  const formatDate = (timestamp: number) => {
    const date = new Date(timestamp * 1000);
    return date.toLocaleDateString() + ' ' + date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  };

  return (
    <div className="trade-history">
      <table>
        <thead>
          <tr>
            <th>Closed At</th>
            <th>Side</th>
            <th>Size</th>
            <th>Entry</th>
            <th>Exit</th>
            <th>PnL</th>
            <th>Status</th>
          </tr>
        </thead>
        <tbody>
          {closedPositions.map((position) => {
            const pnlClass = position.realizedPnl >= 0 ? 'positive' : 'negative';
            const pnlPercent = position.collateral > 0
              ? ((position.realizedPnl / position.collateral) * 100).toFixed(2)
              : '0.00';

            return (
              <tr key={position.address}>
                <td>{formatDate(position.closedAt)}</td>
                <td className={position.side === 'long' ? 'long' : 'short'}>
                  {position.side.toUpperCase()}
                </td>
                <td>{position.size.toFixed(4)}</td>
                <td>${position.entryPrice.toFixed(2)}</td>
                <td>${position.exitPrice.toFixed(2)}</td>
                <td className={pnlClass}>
                  ${position.realizedPnl.toFixed(2)}
                  <span className="pnl-percent">({pnlPercent}%)</span>
                </td>
                <td className={position.status === 'liquidated' ? 'liquidated' : ''}>
                  {position.status === 'liquidated' ? 'LIQUIDATED' : 'Closed'}
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}
