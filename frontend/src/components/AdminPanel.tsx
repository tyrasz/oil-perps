import { useState } from 'react';
import { useWallet } from '@solana/wallet-adapter-react';
import { useMarketStore, COMMODITIES } from '../stores/marketStore';
import { useOnChainMarket } from '../hooks/useOnChainMarket';

// Admin wallet addresses (in production, these would be loaded from config)
const ADMIN_WALLETS = [
  // Add admin wallet addresses here
  'AdminPubkey1111111111111111111111111111111111',
];

interface MarketParams {
  maxLeverage: number;
  initialMarginRatio: number;
  maintenanceMarginRatio: number;
  takerFee: number;
  makerFee: number;
  liquidationFee: number;
  maxOpenInterest: number;
  isPaused: boolean;
}

export function AdminPanel() {
  const { publicKey, connected } = useWallet();
  const { selectedCommodity } = useMarketStore();
  const { marketData } = useOnChainMarket();

  const [selectedMarket, setSelectedMarket] = useState(selectedCommodity.id);
  const [params, setParams] = useState<MarketParams>({
    maxLeverage: 20,
    initialMarginRatio: 100, // 1% = 100 basis points
    maintenanceMarginRatio: 50, // 0.5% = 50 basis points
    takerFee: 50, // 0.05% = 50 basis points / 100
    makerFee: 20, // 0.02% = 20 basis points / 100
    liquidationFee: 100, // 0.1% = 100 basis points / 100
    maxOpenInterest: 10_000_000,
    isPaused: false,
  });

  const isAdmin = connected && publicKey && ADMIN_WALLETS.includes(publicKey.toBase58());

  // Update params when market data changes
  useState(() => {
    if (marketData) {
      setParams({
        maxLeverage: marketData.maxLeverage,
        initialMarginRatio: marketData.initialMarginRatio || 100,
        maintenanceMarginRatio: marketData.maintenanceMarginRatio || 50,
        takerFee: marketData.takerFee || 50,
        makerFee: marketData.makerFee || 20,
        liquidationFee: marketData.liquidationFee || 100,
        maxOpenInterest: marketData.maxOpenInterest,
        isPaused: marketData.isPaused,
      });
    }
  });

  const handleParamChange = (key: keyof MarketParams, value: number | boolean) => {
    setParams((prev) => ({ ...prev, [key]: value }));
  };

  const handleUpdateParams = async () => {
    // In production, this would call the smart contract
    console.log('Updating market params:', params);
    alert('Market parameter update would be submitted here. This requires admin privileges and contract integration.');
  };

  const handleTogglePause = async () => {
    // In production, this would pause/unpause the market
    const newPausedState = !params.isPaused;
    console.log('Toggling market pause:', newPausedState);
    handleParamChange('isPaused', newPausedState);
    alert(`Market would be ${newPausedState ? 'PAUSED' : 'UNPAUSED'}. This requires admin privileges.`);
  };

  if (!connected) {
    return (
      <div className="admin-panel">
        <div className="admin-header">
          <h3>Admin Panel</h3>
        </div>
        <div className="admin-notice">
          Connect wallet to access admin features.
        </div>
      </div>
    );
  }

  if (!isAdmin) {
    return (
      <div className="admin-panel">
        <div className="admin-header">
          <h3>Admin Panel</h3>
        </div>
        <div className="admin-notice warning">
          Connected wallet is not authorized as admin.
          <br />
          <span className="wallet-address">{publicKey?.toBase58()}</span>
        </div>
      </div>
    );
  }

  return (
    <div className="admin-panel">
      <div className="admin-header">
        <h3>Market Administration</h3>
        <span className="admin-badge">Admin</span>
      </div>

      <div className="admin-section">
        <label>Select Market</label>
        <select
          value={selectedMarket}
          onChange={(e) => setSelectedMarket(e.target.value)}
        >
          {COMMODITIES.map((c) => (
            <option key={c.id} value={c.id}>
              {c.icon} {c.name} ({c.symbol})
            </option>
          ))}
        </select>
      </div>

      <div className="admin-section">
        <h4>Leverage & Margin</h4>
        <div className="param-grid">
          <div className="param-item">
            <label>Max Leverage</label>
            <input
              type="number"
              value={params.maxLeverage}
              onChange={(e) => handleParamChange('maxLeverage', parseInt(e.target.value))}
              min={1}
              max={100}
            />
            <span className="param-unit">x</span>
          </div>
          <div className="param-item">
            <label>Initial Margin</label>
            <input
              type="number"
              value={params.initialMarginRatio}
              onChange={(e) => handleParamChange('initialMarginRatio', parseInt(e.target.value))}
              min={1}
              step={10}
            />
            <span className="param-unit">bp ({(params.initialMarginRatio / 100).toFixed(2)}%)</span>
          </div>
          <div className="param-item">
            <label>Maintenance Margin</label>
            <input
              type="number"
              value={params.maintenanceMarginRatio}
              onChange={(e) => handleParamChange('maintenanceMarginRatio', parseInt(e.target.value))}
              min={1}
              step={10}
            />
            <span className="param-unit">bp ({(params.maintenanceMarginRatio / 100).toFixed(2)}%)</span>
          </div>
        </div>
      </div>

      <div className="admin-section">
        <h4>Fee Structure</h4>
        <div className="param-grid">
          <div className="param-item">
            <label>Taker Fee</label>
            <input
              type="number"
              value={params.takerFee}
              onChange={(e) => handleParamChange('takerFee', parseInt(e.target.value))}
              min={0}
              step={10}
            />
            <span className="param-unit">bp ({(params.takerFee / 1000).toFixed(3)}%)</span>
          </div>
          <div className="param-item">
            <label>Maker Fee</label>
            <input
              type="number"
              value={params.makerFee}
              onChange={(e) => handleParamChange('makerFee', parseInt(e.target.value))}
              min={0}
              step={10}
            />
            <span className="param-unit">bp ({(params.makerFee / 1000).toFixed(3)}%)</span>
          </div>
          <div className="param-item">
            <label>Liquidation Fee</label>
            <input
              type="number"
              value={params.liquidationFee}
              onChange={(e) => handleParamChange('liquidationFee', parseInt(e.target.value))}
              min={0}
              step={10}
            />
            <span className="param-unit">bp ({(params.liquidationFee / 1000).toFixed(3)}%)</span>
          </div>
        </div>
      </div>

      <div className="admin-section">
        <h4>Risk Limits</h4>
        <div className="param-grid">
          <div className="param-item wide">
            <label>Max Open Interest</label>
            <input
              type="number"
              value={params.maxOpenInterest}
              onChange={(e) => handleParamChange('maxOpenInterest', parseInt(e.target.value))}
              min={0}
              step={1000000}
            />
            <span className="param-unit">${(params.maxOpenInterest / 1_000_000).toFixed(1)}M</span>
          </div>
        </div>
      </div>

      <div className="admin-section market-status">
        <h4>Market Status</h4>
        <div className={`status-indicator ${params.isPaused ? 'paused' : 'active'}`}>
          <span className="status-dot" />
          {params.isPaused ? 'PAUSED' : 'ACTIVE'}
        </div>
        <button
          className={`toggle-pause-btn ${params.isPaused ? 'unpause' : 'pause'}`}
          onClick={handleTogglePause}
        >
          {params.isPaused ? 'Unpause Market' : 'Pause Market'}
        </button>
      </div>

      <div className="admin-actions">
        <button className="update-btn" onClick={handleUpdateParams}>
          Update Market Parameters
        </button>
      </div>

      <div className="admin-info">
        <h4>Current On-Chain Data</h4>
        {marketData ? (
          <div className="onchain-data">
            <div className="data-row">
              <span>Long OI:</span>
              <span>${(marketData.longOpenInterest / 1_000_000).toFixed(2)}M</span>
            </div>
            <div className="data-row">
              <span>Short OI:</span>
              <span>${(marketData.shortOpenInterest / 1_000_000).toFixed(2)}M</span>
            </div>
            <div className="data-row">
              <span>Insurance Fund:</span>
              <span>${(marketData.insuranceFund / 1_000_000).toFixed(2)}</span>
            </div>
            <div className="data-row">
              <span>Funding Rate:</span>
              <span>{(marketData.fundingRate * 100).toFixed(4)}%</span>
            </div>
          </div>
        ) : (
          <p>Loading on-chain data...</p>
        )}
      </div>
    </div>
  );
}
