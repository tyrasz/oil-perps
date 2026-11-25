import { useWallet } from '@solana/wallet-adapter-react';
import { usePositions } from '../hooks/usePositions';

/** Header account summary: free balance, realized PnL and the open positions' unrealized PnL at mark. */
export function AccountPanel() {
  const { connected } = useWallet();
  const { userAccount, positions } = usePositions();

  if (!connected) {
    return null;
  }

  const realized = userAccount?.realizedPnl ?? 0;
  const unrealized = positions.reduce((s, p) => s + (p.settled ? 0 : p.unrealizedPnl), 0);
  const sign = (x: number) => (x >= 0 ? 'positive' : 'negative');

  return (
    <div className="account-panel">
      <div className="account-stat">
        <span className="label">Balance</span>
        <span className="value">${userAccount?.collateralBalance.toFixed(2) || '0.00'}</span>
      </div>
      <div className="account-stat">
        <span className="label" title="Closed and settled positions">Realized PnL</span>
        <span className={`value ${sign(realized)}`}>${realized.toFixed(2)}</span>
      </div>
      <div className="account-stat">
        <span className="label" title="Open positions at mark, funding included">Unrealized PnL</span>
        <span className={`value ${sign(unrealized)}`}>${unrealized.toFixed(2)}</span>
      </div>
    </div>
  );
}
