import { useWallet } from '@solana/wallet-adapter-react';
import { usePositions } from '../hooks/usePositions';

export function AccountPanel() {
  const { connected } = useWallet();
  const { userAccount } = usePositions();

  if (!connected) {
    return null;
  }

  return (
    <div className="account-panel">
      <div className="account-stat">
        <span className="label">Balance</span>
        <span className="value">${userAccount?.collateralBalance.toFixed(2) || '0.00'}</span>
      </div>
      <div className="account-stat">
        <span className="label">PnL</span>
        <span className={`value ${(userAccount?.realizedPnl || 0) >= 0 ? 'positive' : 'negative'}`}>
          ${userAccount?.realizedPnl.toFixed(2) || '0.00'}
        </span>
      </div>
    </div>
  );
}
