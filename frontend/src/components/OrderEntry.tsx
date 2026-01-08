import { useState, useCallback } from 'react';
import { useWallet } from '@solana/wallet-adapter-react';
import { useMarketStore } from '../stores/marketStore';
import type { OrderSide } from '../types';

export function OrderEntry() {
  const { connected, publicKey } = useWallet();
  const {
    selectedCommodity,
    markets,
    selectedLeverage,
    orderType,
    setSelectedLeverage,
    setOrderType
  } = useMarketStore();

  const market = markets[selectedCommodity.id];

  const [side, setSide] = useState<OrderSide>('long');
  const [size, setSize] = useState('');
  const [price, setPrice] = useState('');
  const [isSubmitting, setIsSubmitting] = useState(false);

  // Generate leverage options based on commodity max leverage
  const maxLev = selectedCommodity.maxLeverage;
  const leverageOptions = [1, 2, 5, 10, 15, 20].filter(lev => lev <= maxLev);

  const calculateMargin = useCallback(() => {
    if (!size || !market) return 0;
    const sizeNum = parseFloat(size);
    const priceNum = orderType === 'market' ? market.price : parseFloat(price) || market.price;
    const notional = sizeNum * priceNum;
    return notional / selectedLeverage;
  }, [size, price, market, selectedLeverage, orderType]);

  const handleSubmit = async () => {
    if (!connected || !publicKey || !size) return;

    setIsSubmitting(true);
    try {
      // TODO: Implement actual transaction submission
      console.log('Submitting order:', {
        commodity: selectedCommodity.id,
        side,
        size: parseFloat(size),
        price: orderType === 'market' ? market?.price : parseFloat(price),
        leverage: selectedLeverage,
        orderType,
      });

      // Reset form
      setSize('');
      setPrice('');
    } catch (error) {
      console.error('Failed to submit order:', error);
    } finally {
      setIsSubmitting(false);
    }
  };

  const margin = calculateMargin();
  const liquidationPrice = market
    ? side === 'long'
      ? market.price * (1 - 1 / selectedLeverage * 0.95)
      : market.price * (1 + 1 / selectedLeverage * 0.95)
    : 0;

  const formatPrice = (value: number) => value.toFixed(selectedCommodity.decimals);

  return (
    <div className="order-entry">
      <div className="order-tabs">
        <button
          className={`tab ${side === 'long' ? 'active long' : ''}`}
          onClick={() => setSide('long')}
        >
          Long
        </button>
        <button
          className={`tab ${side === 'short' ? 'active short' : ''}`}
          onClick={() => setSide('short')}
        >
          Short
        </button>
      </div>

      <div className="order-type-tabs">
        <button
          className={`type-tab ${orderType === 'market' ? 'active' : ''}`}
          onClick={() => setOrderType('market')}
        >
          Market
        </button>
        <button
          className={`type-tab ${orderType === 'limit' ? 'active' : ''}`}
          onClick={() => setOrderType('limit')}
        >
          Limit
        </button>
      </div>

      <div className="form-group">
        <label>Size ({selectedCommodity.contractUnit}s)</label>
        <input
          type="number"
          value={size}
          onChange={(e) => setSize(e.target.value)}
          placeholder="0.00"
          min={selectedCommodity.minTradeSize}
          step={selectedCommodity.minTradeSize}
        />
      </div>

      {orderType === 'limit' && (
        <div className="form-group">
          <label>Price (USD)</label>
          <input
            type="number"
            value={price}
            onChange={(e) => setPrice(e.target.value)}
            placeholder={market?.price.toFixed(selectedCommodity.decimals) || '0.00'}
            min="0"
            step={selectedCommodity.tickSize}
          />
        </div>
      )}

      <div className="form-group">
        <label>Leverage</label>
        <div className="leverage-slider">
          {leverageOptions.map((lev) => (
            <button
              key={lev}
              className={`leverage-btn ${selectedLeverage === lev ? 'active' : ''}`}
              onClick={() => setSelectedLeverage(lev)}
            >
              {lev}x
            </button>
          ))}
        </div>
      </div>

      <div className="order-summary">
        <div className="summary-row">
          <span>Entry Price</span>
          <span>${orderType === 'market' ? formatPrice(market?.price || 0) : price || formatPrice(market?.price || 0)}</span>
        </div>
        <div className="summary-row">
          <span>Margin Required</span>
          <span>${margin.toFixed(2)}</span>
        </div>
        <div className="summary-row">
          <span>Est. Liq. Price</span>
          <span className={side === 'long' ? 'text-red' : 'text-green'}>
            ${formatPrice(liquidationPrice)}
          </span>
        </div>
        <div className="summary-row">
          <span>Trading Fee</span>
          <span>${(margin * selectedLeverage * 0.0005).toFixed(4)}</span>
        </div>
      </div>

      <button
        className={`submit-btn ${side}`}
        onClick={handleSubmit}
        disabled={!connected || !size || isSubmitting}
      >
        {!connected
          ? 'Connect Wallet'
          : isSubmitting
          ? 'Submitting...'
          : `${side === 'long' ? 'Long' : 'Short'} ${selectedCommodity.id}`}
      </button>
    </div>
  );
}
