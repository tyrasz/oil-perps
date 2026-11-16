import { useState } from 'react';
import { useTriggerOrderStore } from '../stores/triggerOrderStore';
import { useMarketStore, COMMODITIES } from '../stores/marketStore';

export function PriceAlerts() {
  const { priceAlerts, addPriceAlert, removePriceAlert } = useTriggerOrderStore();
  const { markets } = useMarketStore();

  const [selectedCommodity, setSelectedCommodity] = useState(COMMODITIES[0].id);
  const [condition, setCondition] = useState<'above' | 'below'>('above');
  const [targetPrice, setTargetPrice] = useState('');

  const handleAddAlert = () => {
    const price = parseFloat(targetPrice);
    if (!price || isNaN(price)) return;

    addPriceAlert({
      commodity: selectedCommodity,
      condition,
      targetPrice: price,
    });

    setTargetPrice('');
  };

  const activeAlerts = priceAlerts.filter((a) => !a.triggered);
  const triggeredAlerts = priceAlerts.filter((a) => a.triggered);

  const currentPrice = markets[selectedCommodity]?.price;

  return (
    <div className="price-alerts">
      <div className="alerts-header">
        <h3>Price Alerts</h3>
        <p className="alerts-description">
          Get notified when prices reach your target levels.
        </p>
      </div>

      <div className="add-alert-form">
        <div className="form-row">
          <div className="form-group">
            <label>Commodity</label>
            <select
              value={selectedCommodity}
              onChange={(e) => setSelectedCommodity(e.target.value)}
            >
              {COMMODITIES.map((c) => (
                <option key={c.id} value={c.id}>
                  {c.icon} {c.name}
                </option>
              ))}
            </select>
          </div>

          <div className="form-group">
            <label>Condition</label>
            <select
              value={condition}
              onChange={(e) => setCondition(e.target.value as 'above' | 'below')}
            >
              <option value="above">Price Above</option>
              <option value="below">Price Below</option>
            </select>
          </div>

          <div className="form-group">
            <label>Target Price</label>
            <input
              type="number"
              value={targetPrice}
              onChange={(e) => setTargetPrice(e.target.value)}
              placeholder={currentPrice ? `Current: $${currentPrice.toFixed(2)}` : 'Enter price'}
            />
          </div>

          <button
            className="add-alert-btn"
            onClick={handleAddAlert}
            disabled={!targetPrice}
          >
            Add Alert
          </button>
        </div>
      </div>

      <div className="alerts-section">
        <h4>Active Alerts ({activeAlerts.length})</h4>
        {activeAlerts.length === 0 ? (
          <p className="no-alerts">No active alerts. Create one above.</p>
        ) : (
          <div className="alerts-list">
            {activeAlerts.map((alert) => {
              const commodity = COMMODITIES.find((c) => c.id === alert.commodity);
              const marketPrice = markets[alert.commodity]?.price;
              const distance = marketPrice
                ? ((alert.targetPrice - marketPrice) / marketPrice * 100)
                : null;

              return (
                <div key={alert.id} className="alert-item">
                  <div className="alert-info">
                    <span className="alert-commodity">
                      {commodity?.icon} {commodity?.name}
                    </span>
                    <span className={`alert-condition ${alert.condition}`}>
                      {alert.condition === 'above' ? '↑' : '↓'} {alert.condition}
                    </span>
                    <span className="alert-price">${alert.targetPrice.toFixed(2)}</span>
                    {distance !== null && (
                      <span className={`alert-distance ${Math.abs(distance) < 1 ? 'close' : ''}`}>
                        ({distance >= 0 ? '+' : ''}{distance.toFixed(2)}% away)
                      </span>
                    )}
                  </div>
                  <button
                    className="remove-alert-btn"
                    onClick={() => removePriceAlert(alert.id)}
                  >
                    ×
                  </button>
                </div>
              );
            })}
          </div>
        )}
      </div>

      {triggeredAlerts.length > 0 && (
        <div className="alerts-section triggered">
          <h4>Triggered Alerts ({triggeredAlerts.length})</h4>
          <div className="alerts-list">
            {triggeredAlerts.slice(0, 10).map((alert) => {
              const commodity = COMMODITIES.find((c) => c.id === alert.commodity);

              return (
                <div key={alert.id} className="alert-item triggered">
                  <div className="alert-info">
                    <span className="alert-commodity">
                      {commodity?.icon} {commodity?.name}
                    </span>
                    <span className={`alert-condition ${alert.condition}`}>
                      {alert.condition === 'above' ? '↑' : '↓'} {alert.condition}
                    </span>
                    <span className="alert-price">${alert.targetPrice.toFixed(2)}</span>
                    <span className="alert-triggered-badge">Triggered</span>
                  </div>
                  <button
                    className="remove-alert-btn"
                    onClick={() => removePriceAlert(alert.id)}
                  >
                    ×
                  </button>
                </div>
              );
            })}
          </div>
        </div>
      )}

      <div className="alerts-info">
        <h4>How Alerts Work</h4>
        <ul>
          <li>Alerts trigger when the market price crosses your target</li>
          <li>You'll receive a browser notification when triggered</li>
          <li>Alerts are stored locally and persist across sessions</li>
          <li>Enable browser notifications for real-time alerts</li>
        </ul>
      </div>
    </div>
  );
}
