import { useState, useCallback, useEffect } from 'react';
import { useWallet } from '@solana/wallet-adapter-react';
import { useMarketStore } from '../stores/marketStore';
import type { AdvancedOrderType, OrderSide, TriggerCondition } from '../types';
import { getTriggerCondition } from '../types';

type OrderTab = 'market' | 'limit' | 'stop' | 'oco';

export function AdvancedOrderEntry() {
  const { connected, publicKey } = useWallet();
  const {
    selectedCommodity,
    markets,
    selectedLeverage,
    setSelectedLeverage,
  } = useMarketStore();

  const market = markets[selectedCommodity.id];

  // Order state
  const [side, setSide] = useState<OrderSide>('long');
  const [orderTab, setOrderTab] = useState<OrderTab>('market');
  const [size, setSize] = useState('');
  const [limitPrice, setLimitPrice] = useState('');

  // Stop/TP order state
  const [stopLossPrice, setStopLossPrice] = useState('');
  const [takeProfitPrice, setTakeProfitPrice] = useState('');
  const [stopOrderType, setStopOrderType] = useState<'stop_loss' | 'take_profit' | 'trailing_stop'>('stop_loss');

  // Trailing stop state
  const [trailingAmount, setTrailingAmount] = useState('');
  const [trailingPercent, setTrailingPercent] = useState(true);

  // OCO state
  const [ocoTakeProfit, setOcoTakeProfit] = useState('');
  const [ocoStopLoss, setOcoStopLoss] = useState('');

  // Options
  const [reduceOnly, setReduceOnly] = useState(false);
  const [isSubmitting, setIsSubmitting] = useState(false);

  // Leverage options
  const maxLev = selectedCommodity.maxLeverage;
  const leverageOptions = [1, 2, 5, 10, 15, 20].filter(lev => lev <= maxLev);

  // Calculate margin required
  const calculateMargin = useCallback(() => {
    if (!size || !market) return 0;
    const sizeNum = parseFloat(size);
    const priceNum = orderTab === 'limit' && limitPrice
      ? parseFloat(limitPrice)
      : market.price;
    const notional = sizeNum * priceNum;
    return notional / selectedLeverage;
  }, [size, limitPrice, market, selectedLeverage, orderTab]);

  // Calculate liquidation price
  const calculateLiquidationPrice = useCallback(() => {
    if (!market) return 0;
    const entryPrice = orderTab === 'limit' && limitPrice
      ? parseFloat(limitPrice)
      : market.price;
    return side === 'long'
      ? entryPrice * (1 - 1 / selectedLeverage * 0.95)
      : entryPrice * (1 + 1 / selectedLeverage * 0.95);
  }, [market, side, selectedLeverage, orderTab, limitPrice]);

  // Get current order type
  const getOrderType = (): AdvancedOrderType => {
    switch (orderTab) {
      case 'market': return 'market';
      case 'limit': return 'limit';
      case 'stop': return stopOrderType;
      case 'oco': return 'limit'; // OCO places two orders
      default: return 'market';
    }
  };

  // Submit order
  const handleSubmit = async () => {
    if (!connected || !publicKey || !size) return;

    setIsSubmitting(true);
    try {
      const orderType = getOrderType();

      if (orderTab === 'oco') {
        // Place OCO order
        console.log('Placing OCO order:', {
          commodity: selectedCommodity.id,
          side,
          size: parseFloat(size),
          takeProfitPrice: parseFloat(ocoTakeProfit),
          stopLossPrice: parseFloat(ocoStopLoss),
          leverage: selectedLeverage,
          reduceOnly,
        });
      } else {
        // Place single order
        console.log('Placing order:', {
          commodity: selectedCommodity.id,
          side,
          orderType,
          size: parseFloat(size),
          price: orderTab === 'limit' ? parseFloat(limitPrice) : market?.price,
          triggerPrice: orderTab === 'stop' ? parseFloat(stopLossPrice || takeProfitPrice) : undefined,
          triggerCondition: orderTab === 'stop' ? getTriggerCondition(stopOrderType, side) : 'none',
          trailingAmount: stopOrderType === 'trailing_stop' ? parseFloat(trailingAmount) : undefined,
          trailingPercent: stopOrderType === 'trailing_stop' ? trailingPercent : undefined,
          leverage: selectedLeverage,
          reduceOnly,
        });
      }

      // Reset form
      setSize('');
      setLimitPrice('');
      setStopLossPrice('');
      setTakeProfitPrice('');
      setOcoTakeProfit('');
      setOcoStopLoss('');
      setTrailingAmount('');
    } catch (error) {
      console.error('Failed to submit order:', error);
    } finally {
      setIsSubmitting(false);
    }
  };

  const margin = calculateMargin();
  const liquidationPrice = calculateLiquidationPrice();
  const formatPrice = (value: number) => value.toFixed(selectedCommodity.decimals);

  return (
    <div className="order-entry advanced">
      {/* Long/Short Tabs */}
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

      {/* Order Type Tabs */}
      <div className="order-type-tabs advanced">
        <button
          className={`type-tab ${orderTab === 'market' ? 'active' : ''}`}
          onClick={() => setOrderTab('market')}
        >
          Market
        </button>
        <button
          className={`type-tab ${orderTab === 'limit' ? 'active' : ''}`}
          onClick={() => setOrderTab('limit')}
        >
          Limit
        </button>
        <button
          className={`type-tab ${orderTab === 'stop' ? 'active' : ''}`}
          onClick={() => setOrderTab('stop')}
        >
          Stop
        </button>
        <button
          className={`type-tab ${orderTab === 'oco' ? 'active' : ''}`}
          onClick={() => setOrderTab('oco')}
        >
          OCO
        </button>
      </div>

      {/* Stop Order Sub-types */}
      {orderTab === 'stop' && (
        <div className="stop-type-tabs">
          <button
            className={`stop-tab ${stopOrderType === 'stop_loss' ? 'active' : ''}`}
            onClick={() => setStopOrderType('stop_loss')}
          >
            Stop Loss
          </button>
          <button
            className={`stop-tab ${stopOrderType === 'take_profit' ? 'active' : ''}`}
            onClick={() => setStopOrderType('take_profit')}
          >
            Take Profit
          </button>
          <button
            className={`stop-tab ${stopOrderType === 'trailing_stop' ? 'active' : ''}`}
            onClick={() => setStopOrderType('trailing_stop')}
          >
            Trailing
          </button>
        </div>
      )}

      {/* Size Input */}
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

      {/* Limit Price (for limit orders) */}
      {orderTab === 'limit' && (
        <div className="form-group">
          <label>Limit Price (USD)</label>
          <input
            type="number"
            value={limitPrice}
            onChange={(e) => setLimitPrice(e.target.value)}
            placeholder={market?.price.toFixed(selectedCommodity.decimals) || '0.00'}
            min="0"
            step={selectedCommodity.tickSize}
          />
        </div>
      )}

      {/* Stop Order Fields */}
      {orderTab === 'stop' && stopOrderType !== 'trailing_stop' && (
        <div className="form-group">
          <label>
            {stopOrderType === 'stop_loss' ? 'Stop Loss' : 'Take Profit'} Price (USD)
          </label>
          <input
            type="number"
            value={stopOrderType === 'stop_loss' ? stopLossPrice : takeProfitPrice}
            onChange={(e) =>
              stopOrderType === 'stop_loss'
                ? setStopLossPrice(e.target.value)
                : setTakeProfitPrice(e.target.value)
            }
            placeholder={market?.price.toFixed(selectedCommodity.decimals) || '0.00'}
            min="0"
            step={selectedCommodity.tickSize}
          />
          <span className="input-hint">
            {stopOrderType === 'stop_loss'
              ? `Triggers when price ${side === 'long' ? 'falls below' : 'rises above'} this level`
              : `Triggers when price ${side === 'long' ? 'rises above' : 'falls below'} this level`}
          </span>
        </div>
      )}

      {/* Trailing Stop Fields */}
      {orderTab === 'stop' && stopOrderType === 'trailing_stop' && (
        <>
          <div className="form-group">
            <label>Trail Distance</label>
            <div className="input-with-toggle">
              <input
                type="number"
                value={trailingAmount}
                onChange={(e) => setTrailingAmount(e.target.value)}
                placeholder={trailingPercent ? '1.0' : '0.50'}
                min="0"
                step={trailingPercent ? '0.1' : selectedCommodity.tickSize}
              />
              <div className="toggle-group">
                <button
                  className={`toggle-btn ${trailingPercent ? 'active' : ''}`}
                  onClick={() => setTrailingPercent(true)}
                >
                  %
                </button>
                <button
                  className={`toggle-btn ${!trailingPercent ? 'active' : ''}`}
                  onClick={() => setTrailingPercent(false)}
                >
                  $
                </button>
              </div>
            </div>
            <span className="input-hint">
              Stop price trails {trailingPercent ? `${trailingAmount || '0'}%` : `$${trailingAmount || '0'}`} behind {side === 'long' ? 'highest' : 'lowest'} price
            </span>
          </div>
        </>
      )}

      {/* OCO Order Fields */}
      {orderTab === 'oco' && (
        <>
          <div className="form-group">
            <label>Take Profit Price</label>
            <input
              type="number"
              value={ocoTakeProfit}
              onChange={(e) => setOcoTakeProfit(e.target.value)}
              placeholder={
                market
                  ? formatPrice(side === 'long' ? market.price * 1.05 : market.price * 0.95)
                  : '0.00'
              }
              min="0"
              step={selectedCommodity.tickSize}
            />
          </div>
          <div className="form-group">
            <label>Stop Loss Price</label>
            <input
              type="number"
              value={ocoStopLoss}
              onChange={(e) => setOcoStopLoss(e.target.value)}
              placeholder={
                market
                  ? formatPrice(side === 'long' ? market.price * 0.95 : market.price * 1.05)
                  : '0.00'
              }
              min="0"
              step={selectedCommodity.tickSize}
            />
          </div>
          <span className="input-hint">
            One-Cancels-Other: When one order fills, the other is automatically cancelled
          </span>
        </>
      )}

      {/* Leverage Selector */}
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

      {/* Reduce Only Toggle */}
      <div className="form-group checkbox-group">
        <label className="checkbox-label">
          <input
            type="checkbox"
            checked={reduceOnly}
            onChange={(e) => setReduceOnly(e.target.checked)}
          />
          <span>Reduce Only</span>
        </label>
        <span className="input-hint">Only reduces existing position, won't open new</span>
      </div>

      {/* Order Summary */}
      <div className="order-summary">
        <div className="summary-row">
          <span>Order Type</span>
          <span className="order-type-badge">
            {orderTab === 'oco' ? 'OCO' : getOrderType().replace('_', ' ').toUpperCase()}
          </span>
        </div>
        <div className="summary-row">
          <span>Entry Price</span>
          <span>
            ${orderTab === 'limit' && limitPrice
              ? limitPrice
              : formatPrice(market?.price || 0)}
          </span>
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
        {orderTab === 'stop' && stopOrderType !== 'trailing_stop' && (
          <div className="summary-row">
            <span>{stopOrderType === 'stop_loss' ? 'Stop Loss' : 'Take Profit'}</span>
            <span className={stopOrderType === 'stop_loss' ? 'text-red' : 'text-green'}>
              ${stopOrderType === 'stop_loss' ? stopLossPrice || '-' : takeProfitPrice || '-'}
            </span>
          </div>
        )}
        {orderTab === 'oco' && (
          <>
            <div className="summary-row">
              <span>Take Profit</span>
              <span className="text-green">${ocoTakeProfit || '-'}</span>
            </div>
            <div className="summary-row">
              <span>Stop Loss</span>
              <span className="text-red">${ocoStopLoss || '-'}</span>
            </div>
          </>
        )}
        <div className="summary-row">
          <span>Trading Fee</span>
          <span>${(margin * selectedLeverage * 0.0005).toFixed(4)}</span>
        </div>
      </div>

      {/* Submit Button */}
      <button
        className={`submit-btn ${side}`}
        onClick={handleSubmit}
        disabled={!connected || !size || isSubmitting}
      >
        {!connected
          ? 'Connect Wallet'
          : isSubmitting
          ? 'Submitting...'
          : orderTab === 'oco'
          ? `Place OCO ${side === 'long' ? 'Long' : 'Short'}`
          : `${side === 'long' ? 'Long' : 'Short'} ${selectedCommodity.id}`}
      </button>
    </div>
  );
}
