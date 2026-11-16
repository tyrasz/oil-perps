import { useState } from 'react';
import { useWallet } from '@solana/wallet-adapter-react';
import { useMarketMaker, MmStatus } from '../hooks/useMarketMaker';
import { MmQuoteManager } from './MmQuoteManager';
import { MmQuotesTable } from './MmQuotesTable';

type MmTab = 'overview' | 'quotes' | 'collateral';

export function MarketMakerPanel() {
  const { connected } = useWallet();
  const {
    registry,
    marketMaker,
    quotes,
    isLoading,
    error,
    isRegistered,
    register,
    depositCollateral,
    withdrawCollateral,
    deregister,
  } = useMarketMaker();

  const [activeTab, setActiveTab] = useState<MmTab>('overview');
  const [registerAmount, setRegisterAmount] = useState('');
  const [collateralAmount, setCollateralAmount] = useState('');
  const [collateralAction, setCollateralAction] = useState<'deposit' | 'withdraw'>('deposit');

  if (!connected) {
    return (
      <div className="mm-panel">
        <div className="mm-connect-prompt">
          Connect your wallet to become a Market Maker
        </div>
      </div>
    );
  }

  const handleRegister = async (e: React.FormEvent) => {
    e.preventDefault();
    const amount = parseFloat(registerAmount);
    if (isNaN(amount) || amount < (registry?.minCollateral || 1000)) return;

    try {
      await register(amount);
      setRegisterAmount('');
    } catch (err) {
      console.error('Registration failed:', err);
    }
  };

  const handleCollateralAction = async (e: React.FormEvent) => {
    e.preventDefault();
    const amount = parseFloat(collateralAmount);
    if (isNaN(amount) || amount <= 0) return;

    try {
      if (collateralAction === 'deposit') {
        await depositCollateral(amount);
      } else {
        await withdrawCollateral(amount);
      }
      setCollateralAmount('');
    } catch (err) {
      console.error('Collateral action failed:', err);
    }
  };

  const handleDeregister = async () => {
    if (!confirm('Are you sure you want to deregister as a Market Maker?')) return;

    try {
      await deregister();
    } catch (err) {
      console.error('Deregistration failed:', err);
    }
  };

  const totalPnl = marketMaker
    ? marketMaker.realizedPnl + marketMaker.unrealizedPnl
    : 0;

  const utilizationRate = marketMaker && marketMaker.collateralDeposited > 0
    ? (marketMaker.collateralLocked / marketMaker.collateralDeposited) * 100
    : 0;

  // Not registered - show registration form
  if (!isRegistered) {
    return (
      <div className="mm-panel">
        <div className="mm-header">
          <h3>Become a Market Maker</h3>
        </div>

        <div className="mm-registration">
          <div className="registration-info">
            <h4>Market Maker Benefits</h4>
            <ul>
              <li>Earn spread on every fill</li>
              <li>Earn {registry?.mmFee || 2} bps rebate on volume</li>
              <li>Provide liquidity to traders</li>
              <li>Full control over quote prices and sizes</li>
            </ul>

            <h4>Requirements</h4>
            <ul>
              <li>Minimum collateral: ${registry?.minCollateral.toLocaleString() || '1,000'}</li>
              <li>Maximum spread: {registry?.maxSpread || 100} bps</li>
              <li>Quote size: {registry?.minQuoteSize || 1} - {registry?.maxQuoteSize?.toLocaleString() || '1,000'} contracts</li>
            </ul>
          </div>

          <form onSubmit={handleRegister} className="registration-form">
            <div className="form-group">
              <label>Initial Collateral (USDC)</label>
              <input
                type="number"
                value={registerAmount}
                onChange={(e) => setRegisterAmount(e.target.value)}
                placeholder={`Min: ${registry?.minCollateral || 1000}`}
                min={registry?.minCollateral || 1000}
                step="100"
              />
            </div>

            <button
              type="submit"
              className="register-btn"
              disabled={isLoading || !registerAmount || parseFloat(registerAmount) < (registry?.minCollateral || 1000)}
            >
              {isLoading ? 'Registering...' : 'Register as Market Maker'}
            </button>
          </form>

          {error && <div className="mm-error">{error}</div>}
        </div>

        <div className="registry-stats">
          <h4>Registry Stats</h4>
          <div className="stats-grid">
            <div className="stat-item">
              <span className="label">Active MMs</span>
              <span className="value">{registry?.totalMms || 0}</span>
            </div>
            <div className="stat-item">
              <span className="label">Active Quotes</span>
              <span className="value">{registry?.activeQuotes || 0}</span>
            </div>
            <div className="stat-item">
              <span className="label">Total Volume</span>
              <span className="value">${(registry?.totalVolume || 0).toLocaleString()}</span>
            </div>
            <div className="stat-item">
              <span className="label">Total Fees</span>
              <span className="value">${(registry?.totalFees || 0).toLocaleString()}</span>
            </div>
          </div>
        </div>
      </div>
    );
  }

  // Registered - show MM dashboard
  return (
    <div className="mm-panel">
      <div className="mm-header">
        <h3>Market Maker Dashboard</h3>
        <span className={`mm-status ${marketMaker?.status === MmStatus.Active ? 'active' : 'inactive'}`}>
          {marketMaker?.status === MmStatus.Active ? 'Active' : 'Inactive'}
        </span>
      </div>

      <div className="mm-stats-grid">
        <div className="mm-stat">
          <span className="stat-label">Collateral</span>
          <span className="stat-value">${marketMaker?.collateralDeposited.toLocaleString()}</span>
          <span className="stat-sub">
            ${marketMaker?.collateralAvailable.toLocaleString()} available
          </span>
        </div>
        <div className="mm-stat">
          <span className="stat-label">Utilization</span>
          <span className="stat-value">{utilizationRate.toFixed(1)}%</span>
          <div className="utilization-bar">
            <div className="utilization-fill" style={{ width: `${Math.min(utilizationRate, 100)}%` }} />
          </div>
        </div>
        <div className="mm-stat">
          <span className="stat-label">Inventory</span>
          <span className={`stat-value ${(marketMaker?.inventory || 0) > 0 ? 'long' : (marketMaker?.inventory || 0) < 0 ? 'short' : ''}`}>
            {(marketMaker?.inventory || 0) > 0 ? '+' : ''}{marketMaker?.inventory || 0} contracts
          </span>
          <span className="stat-sub">
            Avg: ${marketMaker?.averageEntryPrice.toFixed(2)}
          </span>
        </div>
        <div className="mm-stat">
          <span className="stat-label">Total PnL</span>
          <span className={`stat-value ${totalPnl >= 0 ? 'positive' : 'negative'}`}>
            {totalPnl >= 0 ? '+' : ''}${totalPnl.toLocaleString()}
          </span>
          <span className="stat-sub">
            Unrealized: ${marketMaker?.unrealizedPnl.toLocaleString()}
          </span>
        </div>
      </div>

      <div className="mm-tabs">
        <button
          className={`mm-tab ${activeTab === 'overview' ? 'active' : ''}`}
          onClick={() => setActiveTab('overview')}
        >
          Overview
        </button>
        <button
          className={`mm-tab ${activeTab === 'quotes' ? 'active' : ''}`}
          onClick={() => setActiveTab('quotes')}
        >
          Quotes ({quotes.length})
        </button>
        <button
          className={`mm-tab ${activeTab === 'collateral' ? 'active' : ''}`}
          onClick={() => setActiveTab('collateral')}
        >
          Collateral
        </button>
      </div>

      <div className="mm-tab-content">
        {activeTab === 'overview' && (
          <div className="mm-overview">
            <div className="overview-section">
              <h4>Performance</h4>
              <div className="performance-stats">
                <div className="perf-stat">
                  <span className="label">Volume</span>
                  <span className="value">${marketMaker?.totalVolume.toLocaleString()}</span>
                </div>
                <div className="perf-stat">
                  <span className="label">Fees Earned</span>
                  <span className="value">${marketMaker?.totalFees.toLocaleString()}</span>
                </div>
                <div className="perf-stat">
                  <span className="label">Active Quotes</span>
                  <span className="value">{marketMaker?.activeQuotes}</span>
                </div>
                <div className="perf-stat">
                  <span className="label">Registered</span>
                  <span className="value">
                    {marketMaker?.registeredAt
                      ? new Date(marketMaker.registeredAt * 1000).toLocaleDateString()
                      : '-'}
                  </span>
                </div>
              </div>
            </div>

            <div className="overview-section">
              <h4>Quick Actions</h4>
              <MmQuoteManager />
            </div>
          </div>
        )}

        {activeTab === 'quotes' && (
          <div className="mm-quotes-tab">
            <MmQuotesTable />
          </div>
        )}

        {activeTab === 'collateral' && (
          <div className="mm-collateral">
            <div className="collateral-summary">
              <div className="collateral-stat">
                <span className="label">Total Deposited</span>
                <span className="value">${marketMaker?.collateralDeposited.toLocaleString()}</span>
              </div>
              <div className="collateral-stat">
                <span className="label">Locked in Quotes</span>
                <span className="value">${marketMaker?.collateralLocked.toLocaleString()}</span>
              </div>
              <div className="collateral-stat">
                <span className="label">Available</span>
                <span className="value highlight">${marketMaker?.collateralAvailable.toLocaleString()}</span>
              </div>
            </div>

            <div className="collateral-actions">
              <div className="action-tabs">
                <button
                  className={`action-tab ${collateralAction === 'deposit' ? 'active' : ''}`}
                  onClick={() => setCollateralAction('deposit')}
                >
                  Deposit
                </button>
                <button
                  className={`action-tab ${collateralAction === 'withdraw' ? 'active' : ''}`}
                  onClick={() => setCollateralAction('withdraw')}
                >
                  Withdraw
                </button>
              </div>

              <form onSubmit={handleCollateralAction} className="collateral-form">
                <div className="input-group">
                  <input
                    type="number"
                    value={collateralAmount}
                    onChange={(e) => setCollateralAmount(e.target.value)}
                    placeholder={collateralAction === 'deposit' ? 'Amount to deposit' : 'Amount to withdraw'}
                    min="0"
                    max={collateralAction === 'withdraw' ? marketMaker?.collateralAvailable : undefined}
                    step="100"
                  />
                  <span className="input-suffix">USDC</span>
                </div>

                {collateralAction === 'withdraw' && (
                  <button
                    type="button"
                    className="max-btn"
                    onClick={() => setCollateralAmount(marketMaker?.collateralAvailable.toString() || '0')}
                  >
                    Max: ${marketMaker?.collateralAvailable.toLocaleString()}
                  </button>
                )}

                <button
                  type="submit"
                  className={`submit-btn ${collateralAction}`}
                  disabled={isLoading || !collateralAmount}
                >
                  {isLoading ? 'Processing...' : collateralAction === 'deposit' ? 'Deposit USDC' : 'Withdraw USDC'}
                </button>
              </form>
            </div>

            <div className="deregister-section">
              <h4>Deregister</h4>
              <p>Cancel all quotes and close inventory before deregistering. All remaining collateral will be returned.</p>
              <button
                className="deregister-btn"
                onClick={handleDeregister}
                disabled={isLoading || (marketMaker?.activeQuotes || 0) > 0 || (marketMaker?.inventory || 0) !== 0}
              >
                Deregister as Market Maker
              </button>
              {((marketMaker?.activeQuotes || 0) > 0 || (marketMaker?.inventory || 0) !== 0) && (
                <span className="deregister-warning">
                  {(marketMaker?.activeQuotes || 0) > 0 && 'Cancel all quotes first. '}
                  {(marketMaker?.inventory || 0) !== 0 && 'Close all inventory first.'}
                </span>
              )}
            </div>
          </div>
        )}
      </div>

      {error && <div className="mm-error">{error}</div>}
    </div>
  );
}
