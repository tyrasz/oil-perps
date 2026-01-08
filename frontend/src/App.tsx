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
import './App.css';

type Tab = 'positions' | 'orders' | 'trades' | 'vault';

function App() {
  const [activeTab, setActiveTab] = useState<Tab>('positions');

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
              className={`tab ${activeTab === 'vault' ? 'active' : ''}`}
              onClick={() => setActiveTab('vault')}
            >
              LP Vault
            </button>
          </div>

          <div className="tab-content">
            {activeTab === 'positions' && <PositionTable />}
            {activeTab === 'orders' && <OrdersTable />}
            {activeTab === 'trades' && <TradeHistory />}
            {activeTab === 'vault' && <LpVault />}
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

function TradeHistory() {
  return (
    <div className="trade-history">
      <table>
        <thead>
          <tr>
            <th>Time</th>
            <th>Side</th>
            <th>Size</th>
            <th>Price</th>
            <th>PnL</th>
            <th>Fee</th>
          </tr>
        </thead>
        <tbody>
          <tr className="empty-row">
            <td colSpan={6}>No trade history</td>
          </tr>
        </tbody>
      </table>
    </div>
  );
}

export default App;
