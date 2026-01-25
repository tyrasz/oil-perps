import { useState, useMemo } from 'react';
import { useTriggerOrderStore } from '../stores/triggerOrderStore';
import { useMarketStore } from '../stores/marketStore';
import type { Position } from '../types';

interface TpSlManagerProps {
  position: Position;
  onClose?: () => void;
}

export function TpSlManager({ position, onClose }: TpSlManagerProps) {
  const { addTriggerOrder, removeTriggerOrder, getTriggerOrdersForPosition } = useTriggerOrderStore();
  const { getCurrentMarket } = useMarketStore();
  const market = getCurrentMarket();

  const [tpPrice, setTpPrice] = useState('');
  const [slPrice, setSlPrice] = useState('');
  const [closePercent] = useState(100); // Full close by default

  const existingOrders = useMemo(
    () => getTriggerOrdersForPosition(position.address),
    [getTriggerOrdersForPosition, position.address]
  );

  const existingTp = existingOrders.find((o) => o.type === 'take_profit' && o.status === 'active');
  const existingSl = existingOrders.find((o) => o.type === 'stop_loss' && o.status === 'active');

  const handleSetTp = () => {
    const price = parseFloat(tpPrice);
    if (!price || isNaN(price)) return;

    // Remove existing TP if any
    if (existingTp) {
      removeTriggerOrder(existingTp.id);
    }

    addTriggerOrder({
      positionAddress: position.address,
      type: 'take_profit',
      triggerPrice: price,
      closePercent,
    });

    setTpPrice('');
  };

  const handleSetSl = () => {
    const price = parseFloat(slPrice);
    if (!price || isNaN(price)) return;

    // Remove existing SL if any
    if (existingSl) {
      removeTriggerOrder(existingSl.id);
    }

    addTriggerOrder({
      positionAddress: position.address,
      type: 'stop_loss',
      triggerPrice: price,
      closePercent,
    });

    setSlPrice('');
  };

  const handleRemoveTp = () => {
    if (existingTp) {
      removeTriggerOrder(existingTp.id);
    }
  };

  const handleRemoveSl = () => {
    if (existingSl) {
      removeTriggerOrder(existingSl.id);
    }
  };

  // Calculate suggested TP/SL prices based on risk/reward
  const suggestedTp = position.side === 'long'
    ? position.entryPrice * 1.02 // 2% profit
    : position.entryPrice * 0.98;
  const suggestedSl = position.side === 'long'
    ? position.entryPrice * 0.99 // 1% loss
    : position.entryPrice * 1.01;

  // Validate TP/SL prices
  const validateTp = (price: number): boolean => {
    if (position.side === 'long') {
      return price > position.entryPrice;
    }
    return price < position.entryPrice;
  };

  const validateSl = (price: number): boolean => {
    if (position.side === 'long') {
      return price < position.entryPrice && price > 0;
    }
    return price > position.entryPrice;
  };

  const tpValid = !tpPrice || validateTp(parseFloat(tpPrice));
  const slValid = !slPrice || validateSl(parseFloat(slPrice));

  return (
    <div className="tpsl-manager">
      <div className="tpsl-header">
        <h4>TP/SL for {position.side.toUpperCase()} Position</h4>
        {onClose && (
          <button className="close-btn" onClick={onClose}>
            ×
          </button>
        )}
      </div>

      <div className="tpsl-info">
        <div className="info-row">
          <span>Entry Price</span>
          <span>${position.entryPrice.toFixed(2)}</span>
        </div>
        <div className="info-row">
          <span>Current Price</span>
          <span>${market?.price.toFixed(2) || '-'}</span>
        </div>
        <div className="info-row">
          <span>Unrealized PnL</span>
          <span className={position.unrealizedPnl >= 0 ? 'positive' : 'negative'}>
            ${position.unrealizedPnl.toFixed(2)}
          </span>
        </div>
      </div>

      <div className="tpsl-section">
        <label className="tpsl-label tp">
          Take Profit
          {existingTp && (
            <span className="existing-order">
              Active: ${existingTp.triggerPrice.toFixed(2)}
              <button onClick={handleRemoveTp} className="remove-btn">×</button>
            </span>
          )}
        </label>
        <div className="tpsl-input-row">
          <input
            type="number"
            value={tpPrice}
            onChange={(e) => setTpPrice(e.target.value)}
            placeholder={`Suggested: $${suggestedTp.toFixed(2)}`}
            className={!tpValid ? 'invalid' : ''}
          />
          <button
            onClick={handleSetTp}
            disabled={!tpPrice || !tpValid}
            className="set-btn tp"
          >
            {existingTp ? 'Update' : 'Set'}
          </button>
        </div>
        {!tpValid && (
          <span className="validation-error">
            TP must be {position.side === 'long' ? 'above' : 'below'} entry price
          </span>
        )}
      </div>

      <div className="tpsl-section">
        <label className="tpsl-label sl">
          Stop Loss
          {existingSl && (
            <span className="existing-order">
              Active: ${existingSl.triggerPrice.toFixed(2)}
              <button onClick={handleRemoveSl} className="remove-btn">×</button>
            </span>
          )}
        </label>
        <div className="tpsl-input-row">
          <input
            type="number"
            value={slPrice}
            onChange={(e) => setSlPrice(e.target.value)}
            placeholder={`Suggested: $${suggestedSl.toFixed(2)}`}
            className={!slValid ? 'invalid' : ''}
          />
          <button
            onClick={handleSetSl}
            disabled={!slPrice || !slValid}
            className="set-btn sl"
          >
            {existingSl ? 'Update' : 'Set'}
          </button>
        </div>
        {!slValid && (
          <span className="validation-error">
            SL must be {position.side === 'long' ? 'below' : 'above'} entry price
          </span>
        )}
      </div>

      <div className="tpsl-risk-reward">
        {existingTp && existingSl && (
          <div className="rr-info">
            <span>Risk/Reward Ratio:</span>
            <span className="rr-value">
              1:{(
                Math.abs(existingTp.triggerPrice - position.entryPrice) /
                Math.abs(position.entryPrice - existingSl.triggerPrice)
              ).toFixed(2)}
            </span>
          </div>
        )}
      </div>
    </div>
  );
}
