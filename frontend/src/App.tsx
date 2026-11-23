import { useState } from 'react';
import { WalletMultiButton } from '@solana/wallet-adapter-react-ui';
import { TradingChart } from './components/TradingChart';
import { OrderEntry } from './components/OrderEntry';
import { PositionTable } from './components/PositionTable';
import { OrderBook } from './components/OrderBook';
import { MarketStats } from './components/MarketStats';
import { RecentTrades } from './components/RecentTrades';
import { AccountPanel } from './components/AccountPanel';
import { OrdersTable } from './components/OrdersTable';
import { LpVault } from './components/LpVault';
import { MarketMakerPanel } from './components/MarketMakerPanel';
import { CollateralPanel } from './components/CollateralPanel';
import { TradeHistory } from './components/TradeHistory';
import { PortfolioAnalytics } from './components/PortfolioAnalytics';
import { PriceAlerts } from './components/PriceAlerts';
import { ReferralPanel } from './components/ReferralPanel';
import { AdminPanel } from './components/AdminPanel';
import { useTriggerOrderMonitor } from './hooks/useTriggerOrderMonitor';
import { useMarketFeed } from './hooks/useMarket';
import './App.css';

type Tab = 'positions' | 'orders' | 'trades' | 'account' | 'vault' | 'mm' | 'analytics' | 'alerts' | 'referrals' | 'admin';

function App() {
  const [activeTab, setActiveTab] = useState<Tab>('positions');

  // Single app-wide market data feed (REST polling + WebSocket)
  useMarketFeed();

  // Initialize trigger order monitoring (TP/SL execution)
  useTriggerOrderMonitor();

  return (
    <div className="app">
      <header className="header">
        <div className="logo">
          <span className="logo-icon">📈</span>
          <span className="logo-text">COMMODITY PERPS</span>
        </div>
        <MarketStats />
        <div className="header-right">
          <AccountPanel />
          <WalletMultiButton />
        </div>
      </header>

      <main className="main">
        <div className="trading-layout">
          <div className="chart-section">
            <TradingChart />
          </div>

          <div className="orderbook-section">
            <div className="section-header">Order Book</div>
            <OrderBook />
          </div>

          <div className="trades-section">
            <div className="section-header">Recent Trades</div>
            <RecentTrades />
          </div>

          <div className="order-section">
            <div className="section-header">Place Order</div>
            <OrderEntry />
          </div>
        </div>

        <div className="bottom-panel">
          <div className="tabs">
            <button
              className={`tab ${activeTab === 'positions' ? 'active' : ''}`}
              onClick={() => setActiveTab('positions')}
            >
              Positions
            </button>
            <button
              className={`tab ${activeTab === 'orders' ? 'active' : ''}`}
              onClick={() => setActiveTab('orders')}
            >
              Open Orders
            </button>
            <button
              className={`tab ${activeTab === 'trades' ? 'active' : ''}`}
              onClick={() => setActiveTab('trades')}
            >
              Trade History
            </button>
            <button
              className={`tab ${activeTab === 'account' ? 'active' : ''}`}
              onClick={() => setActiveTab('account')}
            >
              Account
            </button>
            <button
              className={`tab ${activeTab === 'vault' ? 'active' : ''}`}
              onClick={() => setActiveTab('vault')}
            >
              LP Vault
            </button>
            <button
              className={`tab ${activeTab === 'mm' ? 'active' : ''}`}
              onClick={() => setActiveTab('mm')}
            >
              Market Maker
            </button>
            <button
              className={`tab ${activeTab === 'analytics' ? 'active' : ''}`}
              onClick={() => setActiveTab('analytics')}
            >
              Analytics
            </button>
            <button
              className={`tab ${activeTab === 'alerts' ? 'active' : ''}`}
              onClick={() => setActiveTab('alerts')}
            >
              Alerts
            </button>
            <button
              className={`tab ${activeTab === 'referrals' ? 'active' : ''}`}
              onClick={() => setActiveTab('referrals')}
            >
              Referrals
            </button>
            <button
              className={`tab ${activeTab === 'admin' ? 'active' : ''}`}
              onClick={() => setActiveTab('admin')}
            >
              Admin
            </button>
          </div>

          <div className="tab-content">
            {activeTab === 'positions' && <PositionTable />}
            {activeTab === 'orders' && <OrdersTable />}
            {activeTab === 'trades' && <TradeHistory />}
            {activeTab === 'account' && <CollateralPanel />}
            {activeTab === 'vault' && <LpVault />}
            {activeTab === 'mm' && <MarketMakerPanel />}
            {activeTab === 'analytics' && <PortfolioAnalytics />}
            {activeTab === 'alerts' && <PriceAlerts />}
            {activeTab === 'referrals' && <ReferralPanel />}
            {activeTab === 'admin' && <AdminPanel />}
          </div>
        </div>
      </main>

      <footer className="footer">
        <div className="footer-links">
          <a href="https://github.com/tyrasz/oil-perps" target="_blank" rel="noopener noreferrer">GitHub</a>
          <a href="#docs">Docs</a>
          <a href="#api">API</a>
        </div>
        <div className="footer-info">
          <span>Network: Devnet</span>
          <span className="status-dot online"></span>
        </div>
      </footer>
    </div>
  );
}

export default App;
