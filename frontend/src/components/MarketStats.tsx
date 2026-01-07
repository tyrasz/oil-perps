import { useMarket } from '../hooks/useMarket';

export function MarketStats() {
  const { market } = useMarket();

  if (!market) {
    return <div className="market-stats loading">Loading...</div>;
  }

  return (
    <div className="market-stats">
      <div className="stat">
        <span className="label">OIL-PERP</span>
        <span className="value price">${market.price.toFixed(2)}</span>
        <span className={`change ${market.priceChange24h >= 0 ? 'positive' : 'negative'}`}>
          {market.priceChange24h >= 0 ? '+' : ''}{market.priceChange24h.toFixed(2)}%
        </span>
      </div>

      <div className="stat">
        <span className="label">24h Volume</span>
        <span className="value">${(market.volume24h / 1_000_000).toFixed(2)}M</span>
      </div>

      <div className="stat">
        <span className="label">Open Interest</span>
        <span className="value">${(market.openInterest / 1_000_000).toFixed(2)}M</span>
      </div>

      <div className="stat">
        <span className="label">Funding Rate</span>
        <span className={`value ${market.fundingRate >= 0 ? 'positive' : 'negative'}`}>
          {market.fundingRate >= 0 ? '+' : ''}{(market.fundingRate * 100).toFixed(4)}%
        </span>
      </div>

      <div className="stat oi-breakdown">
        <span className="label">Long/Short</span>
        <div className="oi-bar">
          <div
            className="long-bar"
            style={{
              width: `${(market.longOpenInterest / (market.longOpenInterest + market.shortOpenInterest)) * 100}%`
            }}
          />
        </div>
        <span className="value">
          {((market.longOpenInterest / (market.longOpenInterest + market.shortOpenInterest)) * 100).toFixed(1)}%
          /
          {((market.shortOpenInterest / (market.longOpenInterest + market.shortOpenInterest)) * 100).toFixed(1)}%
        </span>
      </div>
    </div>
  );
}
