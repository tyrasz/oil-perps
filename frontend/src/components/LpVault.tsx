import { useState } from 'react';
import { useWallet } from '@solana/wallet-adapter-react';
import { useVault } from '../hooks/useVault';

export function LpVault() {
  const { connected } = useWallet();
  const { vault, lpPosition, deposit, withdraw, requestWithdrawal, isLoading } = useVault();
  const [amount, setAmount] = useState('');
  const [action, setAction] = useState<'deposit' | 'withdraw'>('deposit');

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    const amountNum = parseFloat(amount);
    if (isNaN(amountNum) || amountNum <= 0) return;

    try {
      if (action === 'deposit') {
        await deposit(amountNum);
      } else {
        await withdraw(amountNum);
      }
      setAmount('');
    } catch (error) {
      console.error('Transaction failed:', error);
    }
  };

  if (!connected) {
    return (
      <div className="lp-vault">
        <div className="vault-connect-prompt">
          Connect your wallet to provide liquidity
        </div>
      </div>
    );
  }

  const shareValue = vault ? vault.totalShares > 0
    ? (vault.totalAssets / vault.totalShares).toFixed(4)
    : '1.0000' : '1.0000';

  const userValue = lpPosition && vault
    ? (lpPosition.shares * parseFloat(shareValue)).toFixed(2)
    : '0.00';

  const userPnl = lpPosition
    ? (parseFloat(userValue) - lpPosition.depositedAmount).toFixed(2)
    : '0.00';

  const canWithdraw = lpPosition && lpPosition.withdrawalRequestedAt > 0 &&
    Date.now() / 1000 > lpPosition.withdrawalRequestedAt + (vault?.withdrawalDelay || 0);

  return (
    <div className="lp-vault">
      <div className="vault-header">
        <h3>LP Vault</h3>
        <span className="vault-status active">Active</span>
      </div>

      <div className="vault-stats-grid">
        <div className="vault-stat">
          <span className="stat-label">Total Value Locked</span>
          <span className="stat-value">${vault?.totalAssets.toLocaleString() || '0'}</span>
        </div>
        <div className="vault-stat">
          <span className="stat-label">Share Value</span>
          <span className="stat-value">${shareValue}</span>
        </div>
        <div className="vault-stat">
          <span className="stat-label">Net Exposure</span>
          <span className={`stat-value ${(vault?.netExposure || 0) > 0 ? 'short' : 'long'}`}>
            {vault?.netExposure || 0 > 0 ? 'SHORT' : 'LONG'} ${Math.abs(vault?.netExposure || 0).toLocaleString()}
          </span>
        </div>
        <div className="vault-stat">
          <span className="stat-label">Utilization</span>
          <span className="stat-value">
            {vault ? ((vault.totalLongSize + vault.totalShortSize) / vault.totalAssets * 100).toFixed(1) : '0'}%
          </span>
        </div>
      </div>

      <div className="vault-divider" />

      <div className="user-position">
        <h4>Your Position</h4>
        <div className="position-stats">
          <div className="position-stat">
            <span className="stat-label">Shares</span>
            <span className="stat-value">{lpPosition?.shares.toLocaleString() || '0'}</span>
          </div>
          <div className="position-stat">
            <span className="stat-label">Value</span>
            <span className="stat-value">${userValue}</span>
          </div>
          <div className="position-stat">
            <span className="stat-label">PnL</span>
            <span className={`stat-value ${parseFloat(userPnl) >= 0 ? 'positive' : 'negative'}`}>
              ${userPnl}
            </span>
          </div>
        </div>
      </div>

      <div className="vault-divider" />

      <div className="vault-actions">
        <div className="action-tabs">
          <button
            className={`action-tab ${action === 'deposit' ? 'active' : ''}`}
            onClick={() => setAction('deposit')}
          >
            Deposit
          </button>
          <button
            className={`action-tab ${action === 'withdraw' ? 'active' : ''}`}
            onClick={() => setAction('withdraw')}
          >
            Withdraw
          </button>
        </div>

        <form onSubmit={handleSubmit} className="vault-form">
          <div className="input-group">
            <input
              type="number"
              value={amount}
              onChange={(e) => setAmount(e.target.value)}
              placeholder={action === 'deposit' ? 'Amount to deposit' : 'Shares to burn'}
              min="0"
              step="0.01"
            />
            <span className="input-suffix">{action === 'deposit' ? 'USDC' : 'shares'}</span>
          </div>

          {action === 'withdraw' && lpPosition && lpPosition.withdrawalRequestedAt === 0 && (
            <button
              type="button"
              className="request-btn"
              onClick={requestWithdrawal}
              disabled={isLoading || !lpPosition.shares}
            >
              Request Withdrawal (24h cooldown)
            </button>
          )}

          {action === 'withdraw' && lpPosition && lpPosition.withdrawalRequestedAt > 0 && !canWithdraw && (
            <div className="cooldown-notice">
              Withdrawal available in {Math.ceil((lpPosition.withdrawalRequestedAt + (vault?.withdrawalDelay || 86400) - Date.now() / 1000) / 3600)}h
            </div>
          )}

          <button
            type="submit"
            className={`submit-btn ${action}`}
            disabled={isLoading || !amount || (action === 'withdraw' && !canWithdraw)}
          >
            {isLoading ? 'Processing...' : action === 'deposit' ? 'Deposit USDC' : 'Withdraw'}
          </button>
        </form>
      </div>

      <div className="vault-info">
        <div className="info-item">
          <span>Base Spread</span>
          <span>{vault?.baseSpread || 5} bps</span>
        </div>
        <div className="info-item">
          <span>Trading Fee</span>
          <span>{vault?.tradingFee || 5} bps</span>
        </div>
        <div className="info-item">
          <span>LP Fee Share</span>
          <span>{vault?.lpFeeShare || 70}%</span>
        </div>
        <div className="info-item">
          <span>Withdrawal Delay</span>
          <span>24 hours</span>
        </div>
      </div>
    </div>
  );
}
