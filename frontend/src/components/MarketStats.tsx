import { useState, useEffect } from 'react';
import { useMarket } from '../hooks/useMarket';
import { useOnChainMarket } from '../hooks/useOnChainMarket';
import { CommoditySelector } from './CommoditySelector';

export function MarketStats() {
  const { market, selectedCommodity } = useMarket();
  const { marketData: onChainMarket } = useOnChainMarket();
  const [timeToFunding, setTimeToFunding] = useState<string>('--:--:--');

  // Update countdown to next funding
  useEffect(() => {
    if (!onChainMarket) return;

    const updateCountdown = () => {
      const now = Math.floor(Date.now() / 1000);
      const remaining = onChainMarket.nextFundingTime - now;

      if (remaining <= 0) {
        setTimeToFunding('00:00:00');
        return;
      }

      const hours = Math.floor(remaining / 3600);
      const minutes = Math.floor((remaining % 3600) / 60);
      const seconds = remaining % 60;

      setTimeToFunding(
        `${hours.toString().padStart(2, '0')}:${minutes.toString().padStart(2, '0')}:${seconds.toString().padStart(2, '0')}`
      );
    };

    updateCountdown();
    const interval = setInterval(updateCountdown, 1000);
    return () => clearInterval(interval);
  }, [onChainMarket]);

  if (!market) {
    return <div className="market-stats loading">Loading...</div>;
  }

  // Use on-chain funding rate if available, otherwise fall back to API data
  const fundingRate = onChainMarket?.fundingRate ?? market.fundingRate;
  const longOI = onChainMarket?.longOpenInterest ?? market.longOpenInterest;
  const shortOI = onChainMarket?.shortOpenInterest ?? market.shortOpenInterest;
  const totalOI = longOI + shortOI;

  return (
    <div className="market-stats">
      <div className="stat commodity-stat">
        <CommoditySelector />
        <span className="value price">${market.price.toFixed(selectedCommodity.decimals)}</span>
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

      <div className="stat funding-stat">
        <span className="label">Funding Rate</span>
        <span className={`value ${fundingRate >= 0 ? 'positive' : 'negative'}`}>
          {fundingRate >= 0 ? '+' : ''}{(fundingRate * 100).toFixed(4)}%
        </span>
        {onChainMarket && (
          <span className="funding-countdown" title="Time until next funding">
            {timeToFunding}
          </span>
        )}
      </div>

      <div className="stat oi-breakdown">
        <span className="label">Long/Short</span>
        <div className="oi-bar">
          <div
            className="long-bar"
            style={{
              width: `${totalOI > 0 ? (longOI / totalOI) * 100 : 50}%`
            }}
          />
        </div>
        <span className="value">
          {totalOI > 0 ? (longOI / totalOI * 100).toFixed(1) : '50.0'}%
          /
          {totalOI > 0 ? (shortOI / totalOI * 100).toFixed(1) : '50.0'}%
        </span>
      </div>
    </div>
  );
}
