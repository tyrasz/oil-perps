import { useMarket } from '../hooks/useMarket';

export function RecentTrades() {
  const { recentTrades } = useMarket();

  return (
    <div className="recent-trades">
      <div className="trades-header">
        <span>Price</span>
        <span>Size</span>
        <span>Time</span>
      </div>

      <div className="trades-list">
        {recentTrades.length === 0 ? (
          <div className="no-trades">No recent trades</div>
        ) : (
          recentTrades.slice(0, 20).map((trade, i) => (
            <div
              key={`${trade.timestamp}-${i}`}
              className={`trade-row ${trade.side}`}
            >
              <span className="price">${trade.price.toFixed(2)}</span>
              <span className="size">{trade.size.toFixed(4)}</span>
              <span className="time">
                {new Date(trade.timestamp * 1000).toLocaleTimeString()}
              </span>
            </div>
          ))
        )}
      </div>
    </div>
  );
}
