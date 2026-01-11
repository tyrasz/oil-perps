import { useState, useCallback, useEffect } from 'react';
import { useWallet } from '@solana/wallet-adapter-react';
import { useMarketStore } from '../stores/marketStore';
import { useTrading } from '../hooks/useTrading';
import { usePositions } from '../hooks/usePositions';
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
  const {
    openPosition,
    checkMarketExists,
    isLoading: isTradingLoading,
    error: tradingError,
    clearError,
    getExplorerUrl,
    lastTxSignature
  } = useTrading();
  const { userAccount } = usePositions();

  const market = markets[selectedCommodity.id];
  const availableCollateral = userAccount?.collateralBalance ?? 0;

  const [side, setSide] = useState<OrderSide>('long');
  const [size, setSize] = useState('');
  const [price, setPrice] = useState('');
  const [successMessage, setSuccessMessage] = useState<string | null>(null);
  const [marketExists, setMarketExists] = useState<boolean | null>(null);

  // Check if market is initialized on-chain
  useEffect(() => {
    if (connected) {
      checkMarketExists().then(setMarketExists);
    }
  }, [connected, checkMarketExists]);

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

    // Clear previous messages
    clearError();
    setSuccessMessage(null);

    try {
      await openPosition(side, parseFloat(size), selectedLeverage);

      // Reset form on success
      setSize('');
      setPrice('');
      setSuccessMessage(`Position opened! View on explorer`);

      // Auto-clear success message after 10 seconds
      setTimeout(() => setSuccessMessage(null), 10000);
    } catch (error) {
      console.error('Failed to submit order:', error);
      // Error is already set in useTrading hook
    }
  };

  const margin = calculateMargin();
  const insufficientMargin = margin > 0 && margin > availableCollateral;
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
          <span>Available Collateral</span>
          <span>${availableCollateral.toFixed(2)}</span>
        </div>
        <div className="summary-row">
          <span>Margin Required</span>
          <span className={insufficientMargin ? 'text-red' : ''}>
            ${margin.toFixed(2)}
          </span>
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

      {/* Market not initialized warning */}
      {connected && marketExists === false && (
        <div className="error-message">
          Market not initialized on-chain. Run the initialize-market script first.
        </div>
      )}

      {/* Insufficient margin warning */}
      {insufficientMargin && (
        <div className="warning-message">
          Insufficient collateral. Deposit ${(margin - availableCollateral).toFixed(2)} more or reduce position size.
        </div>
      )}

      {/* Error message */}
      {tradingError && (
        <div className="error-message" onClick={clearError}>
          {tradingError}
        </div>
      )}

      {/* Success message */}
      {successMessage && lastTxSignature && (
        <div className="success-message">
          {successMessage}{' '}
          <a
            href={getExplorerUrl(lastTxSignature)}
            target="_blank"
            rel="noopener noreferrer"
          >
            View tx
          </a>
        </div>
      )}

      <button
        className={`submit-btn ${side}`}
        onClick={handleSubmit}
        disabled={!connected || !size || isTradingLoading || insufficientMargin || marketExists === false}
      >
        {!connected
          ? 'Connect Wallet'
          : marketExists === false
          ? 'Market Not Initialized'
          : isTradingLoading
          ? 'Submitting...'
          : insufficientMargin
          ? 'Insufficient Collateral'
          : `${side === 'long' ? 'Long' : 'Short'} ${selectedCommodity.id}`}
      </button>
    </div>
  );
}
