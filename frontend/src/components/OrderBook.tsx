import { useMarket } from '../hooks/useMarket';

export function OrderBook() {
  const { orderBook, market } = useMarket();

  const maxTotal = Math.max(
    ...((orderBook?.bids || []).map((b) => b.total)),
    ...((orderBook?.asks || []).map((a) => a.total))
  ) || 1;

  return (
    <div className="orderbook">
      <div className="orderbook-header">
        <span>Price (USD)</span>
        <span>Size</span>
        <span>Total</span>
      </div>

      <div className="orderbook-asks">
        {(orderBook?.asks || []).slice(0, 8).reverse().map((ask, i) => (
          <div key={i} className="orderbook-row ask">
            <div
              className="orderbook-bar"
              style={{ width: `${(ask.total / maxTotal) * 100}%` }}
            />
            <span className="price">${ask.price.toFixed(2)}</span>
            <span className="size">{ask.size.toFixed(4)}</span>
            <span className="total">{ask.total.toFixed(4)}</span>
          </div>
        ))}
      </div>

      <div className="orderbook-spread">
        <span className="spread-label">Spread</span>
        <span className="spread-value">
          ${orderBook?.spread.toFixed(2) || '0.00'}
          {' '}
          ({orderBook && market ? ((orderBook.spread / market.price) * 100).toFixed(3) : '0.00'}%)
        </span>
      </div>

      <div className="orderbook-bids">
        {(orderBook?.bids || []).slice(0, 8).map((bid, i) => (
          <div key={i} className="orderbook-row bid">
            <div
              className="orderbook-bar"
              style={{ width: `${(bid.total / maxTotal) * 100}%` }}
            />
            <span className="price">${bid.price.toFixed(2)}</span>
            <span className="size">{bid.size.toFixed(4)}</span>
            <span className="total">{bid.total.toFixed(4)}</span>
          </div>
        ))}
      </div>
    </div>
  );
}
