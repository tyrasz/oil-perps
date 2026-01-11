import { useState } from 'react';
import { useWallet } from '@solana/wallet-adapter-react';
import { usePositions } from '../hooks/usePositions';
import { useTrading } from '../hooks/useTrading';

export function CollateralPanel() {
  const { connected } = useWallet();
  const { userAccount, refreshAccount } = usePositions();
  const {
    depositCollateral,
    isLoading,
    error,
    clearError,
    lastTxSignature,
    getExplorerUrl,
  } = useTrading();

  const [amount, setAmount] = useState('');
  const [mode, setMode] = useState<'deposit' | 'withdraw'>('deposit');
  const [successMessage, setSuccessMessage] = useState<string | null>(null);

  if (!connected) {
    return (
      <div className="collateral-panel">
        <p className="connect-message">Connect wallet to manage collateral</p>
      </div>
    );
  }

  const handleSubmit = async () => {
    if (!amount || parseFloat(amount) <= 0) return;

    clearError();
    setSuccessMessage(null);

    try {
      if (mode === 'deposit') {
        await depositCollateral(parseFloat(amount));
        setSuccessMessage('Deposit successful!');
      } else {
        // TODO: Implement withdraw
        console.log('Withdraw not yet implemented');
        return;
      }

      setAmount('');

      // Refresh account balance
      if (refreshAccount) {
        await refreshAccount();
      }

      setTimeout(() => setSuccessMessage(null), 10000);
    } catch (err) {
      console.error('Collateral operation failed:', err);
    }
  };

  const balance = userAccount?.collateralBalance ?? 0;

  return (
    <div className="collateral-panel">
      <h3>Collateral</h3>

      <div className="balance-display">
        <span className="label">Available Balance</span>
        <span className="value">${balance.toFixed(2)} USDC</span>
      </div>

      <div className="mode-tabs">
        <button
          className={`mode-tab ${mode === 'deposit' ? 'active' : ''}`}
          onClick={() => setMode('deposit')}
        >
          Deposit
        </button>
        <button
          className={`mode-tab ${mode === 'withdraw' ? 'active' : ''}`}
          onClick={() => setMode('withdraw')}
          disabled // Withdraw not implemented yet
        >
          Withdraw
        </button>
      </div>

      <div className="form-group">
        <label>Amount (USDC)</label>
        <input
          type="number"
          value={amount}
          onChange={(e) => setAmount(e.target.value)}
          placeholder="0.00"
          min="0"
          step="0.01"
        />
      </div>

      {mode === 'deposit' && (
        <p className="info-text">
          Deposit USDC to use as margin for trading positions.
        </p>
      )}

      {/* Error message */}
      {error && (
        <div className="error-message" onClick={clearError}>
          {error}
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
        className="submit-btn"
        onClick={handleSubmit}
        disabled={!amount || parseFloat(amount) <= 0 || isLoading}
      >
        {isLoading
          ? 'Processing...'
          : mode === 'deposit'
          ? 'Deposit USDC'
          : 'Withdraw USDC'}
      </button>
    </div>
  );
}
