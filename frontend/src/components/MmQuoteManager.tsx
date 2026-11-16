import { useState, useCallback } from 'react';
import { useMarketMaker } from '../hooks/useMarketMaker';
import { useMarketStore } from '../stores/marketStore';

interface QuoteFormData {
  bidPrice: string;
  bidSize: string;
  askPrice: string;
  askSize: string;
  expiresIn: string;
}

export function MmQuoteManager() {
  const { registry, marketMaker, postQuote, isLoading } = useMarketMaker();
  const { selectedCommodity, markets } = useMarketStore();
  const market = markets[selectedCommodity.id];

  const [formData, setFormData] = useState<QuoteFormData>({
    bidPrice: '',
    bidSize: '100',
    askPrice: '',
    askSize: '100',
    expiresIn: '3600', // 1 hour default
  });

  const [useMarketPrice, setUseMarketPrice] = useState(true);
  const [spreadBps, setSpreadBps] = useState('50'); // 50 bps default

  // Calculate bid/ask from market price and spread
  const calculatePrices = useCallback(() => {
    if (!market) return { bid: 0, ask: 0 };

    const spread = parseFloat(spreadBps) / 10000;
    const halfSpread = spread / 2;

    return {
      bid: market.price * (1 - halfSpread),
      ask: market.price * (1 + halfSpread),
    };
  }, [market, spreadBps]);

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();

    let bidPrice: number, askPrice: number;

    if (useMarketPrice) {
      const prices = calculatePrices();
      bidPrice = prices.bid;
      askPrice = prices.ask;
    } else {
      bidPrice = parseFloat(formData.bidPrice);
      askPrice = parseFloat(formData.askPrice);
    }

    const bidSize = parseFloat(formData.bidSize);
    const askSize = parseFloat(formData.askSize);
    const expiresInSeconds = parseInt(formData.expiresIn);

    if (isNaN(bidPrice) || isNaN(askPrice) || isNaN(bidSize) || isNaN(askSize)) {
      return;
    }

    try {
      await postQuote({
        bidPrice,
        bidSize,
        askPrice,
        askSize,
        expiresInSeconds,
      });

      // Reset form
      setFormData({
        bidPrice: '',
        bidSize: '100',
        askPrice: '',
        askSize: '100',
        expiresIn: '3600',
      });
    } catch (err) {
      console.error('Failed to post quote:', err);
    }
  };

  const prices = calculatePrices();
  const currentSpread = useMarketPrice
    ? parseFloat(spreadBps)
    : formData.bidPrice && formData.askPrice
    ? ((parseFloat(formData.askPrice) - parseFloat(formData.bidPrice)) / parseFloat(formData.bidPrice)) * 10000
    : 0;

  const maxNotional = Math.max(
    parseFloat(formData.bidSize || '0') * (useMarketPrice ? prices.bid : parseFloat(formData.bidPrice) || 0),
    parseFloat(formData.askSize || '0') * (useMarketPrice ? prices.ask : parseFloat(formData.askPrice) || 0)
  );
  const collateralRequired = maxNotional / 10; // 10% collateral requirement

  const hasEnoughCollateral = marketMaker && collateralRequired <= marketMaker.collateralAvailable;
  const isSpreadValid = registry && currentSpread <= registry.maxSpread;

  return (
    <div className="quote-manager">
      <div className="quote-header">
        <h4>Post New Quote</h4>
        <div className="market-price">
          Market: ${market?.price.toFixed(2) || '0.00'}
        </div>
      </div>

      <form onSubmit={handleSubmit} className="quote-form">
        <div className="price-mode">
          <label className="checkbox-label">
            <input
              type="checkbox"
              checked={useMarketPrice}
              onChange={(e) => setUseMarketPrice(e.target.checked)}
            />
            Auto-price from spread
          </label>
        </div>

        {useMarketPrice ? (
          <div className="form-group spread-input">
            <label>Spread (bps)</label>
            <div className="spread-slider">
              <input
                type="range"
                min="10"
                max={registry?.maxSpread || 100}
                value={spreadBps}
                onChange={(e) => setSpreadBps(e.target.value)}
              />
              <span className="spread-value">{spreadBps} bps</span>
            </div>
            <div className="calculated-prices">
              <span className="bid-preview">Bid: ${prices.bid.toFixed(2)}</span>
              <span className="ask-preview">Ask: ${prices.ask.toFixed(2)}</span>
            </div>
          </div>
        ) : (
          <div className="price-inputs">
            <div className="form-group">
              <label>Bid Price</label>
              <input
                type="number"
                value={formData.bidPrice}
                onChange={(e) => setFormData({ ...formData, bidPrice: e.target.value })}
                placeholder={market?.price.toFixed(2)}
                step="0.01"
              />
            </div>
            <div className="form-group">
              <label>Ask Price</label>
              <input
                type="number"
                value={formData.askPrice}
                onChange={(e) => setFormData({ ...formData, askPrice: e.target.value })}
                placeholder={market?.price.toFixed(2)}
                step="0.01"
              />
            </div>
          </div>
        )}

        <div className="size-inputs">
          <div className="form-group">
            <label>Bid Size</label>
            <input
              type="number"
              value={formData.bidSize}
              onChange={(e) => setFormData({ ...formData, bidSize: e.target.value })}
              placeholder="100"
              min={registry?.minQuoteSize || 1}
              max={registry?.maxQuoteSize || 1000}
            />
          </div>
          <div className="form-group">
            <label>Ask Size</label>
            <input
              type="number"
              value={formData.askSize}
              onChange={(e) => setFormData({ ...formData, askSize: e.target.value })}
              placeholder="100"
              min={registry?.minQuoteSize || 1}
              max={registry?.maxQuoteSize || 1000}
            />
          </div>
        </div>

        <div className="form-group">
          <label>Expires In</label>
          <select
            value={formData.expiresIn}
            onChange={(e) => setFormData({ ...formData, expiresIn: e.target.value })}
          >
            <option value="900">15 minutes</option>
            <option value="1800">30 minutes</option>
            <option value="3600">1 hour</option>
            <option value="7200">2 hours</option>
            <option value="14400">4 hours</option>
            <option value="28800">8 hours</option>
            <option value="86400">24 hours</option>
          </select>
        </div>

        <div className="quote-summary">
          <div className="summary-row">
            <span>Spread</span>
            <span className={isSpreadValid ? '' : 'error'}>
              {currentSpread.toFixed(0)} bps
              {!isSpreadValid && ` (max: ${registry?.maxSpread})`}
            </span>
          </div>
          <div className="summary-row">
            <span>Max Notional</span>
            <span>${maxNotional.toLocaleString(undefined, { maximumFractionDigits: 0 })}</span>
          </div>
          <div className="summary-row">
            <span>Collateral Required</span>
            <span className={hasEnoughCollateral ? '' : 'error'}>
              ${collateralRequired.toLocaleString(undefined, { maximumFractionDigits: 0 })}
              {!hasEnoughCollateral && ' (insufficient)'}
            </span>
          </div>
          <div className="summary-row">
            <span>Available Collateral</span>
            <span>${marketMaker?.collateralAvailable.toLocaleString()}</span>
          </div>
        </div>

        <button
          type="submit"
          className="submit-quote-btn"
          disabled={isLoading || !hasEnoughCollateral || !isSpreadValid}
        >
          {isLoading ? 'Posting...' : 'Post Quote'}
        </button>
      </form>
    </div>
  );
}
