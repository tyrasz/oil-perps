import { useState } from 'react';
import { useVault } from '../hooks/useVault';
import { useMarketStore } from '../stores/marketStore';
import { adlRequired, drawdownNow, fromUnits, isSettled, toUnits, depositShares, withdrawRefusal, withdrawRoom, exitBufferBps } from '../utils/pool';
import { redeemValue, unvested } from '../utils/poolMath';
import { explainProgramError, type ErrorContext } from '../utils/errors';
import { formatDuration } from '../utils/session';

const usd = (x: bigint) =>
  `$${fromUnits(x).toLocaleString(undefined, { minimumFractionDigits: 2, maximumFractionDigits: 2 })}`;
const shares = (x: bigint) => fromUnits(x).toLocaleString(undefined, { maximumFractionDigits: 6 });
const perShare = (x: bigint) => `$${fromUnits(x).toFixed(6)}`;
const when = (t: number) => new Date(t * 1000).toLocaleString();
const bps = (x: bigint) => `${(Number(x) / 100).toFixed(2)}%`;

export function LpVault() {
  const {
    connected,
    market,
    pool,
    view,
    gates,
    lpPosition,
    request,
    summary,
    now,
    isLoading,
    error,
    deposit,
    requestWithdrawal,
    cancelWithdrawal,
    withdraw,
  } = useVault();
  const selectedCommodity = useMarketStore((s) => s.selectedCommodity);
  const [amount, setAmount] = useState('');
  const [action, setAction] = useState<'deposit' | 'withdraw'>('deposit');

  if (!connected) {
    return (
      <div className="lp-vault">
        <div className="vault-connect-prompt">Connect your wallet to provide liquidity</div>
      </div>
    );
  }

  if (!market || !pool) {
    return (
      <div className="lp-vault">
        <div className="vault-connect-prompt">
          {error ?? `Loading the ${selectedCommodity.id} LP pool...`}
        </div>
      </div>
    );
  }

  const p = pool.params;
  const settled = isSettled(market);
  const dd = view ? drawdownNow(pool, view, now) : 0n;
  const status = settled
    ? 'Settled'
    : pool.lpShares === 0n
    ? 'Not seeded'
    : adlRequired(pool)
    ? 'ADL pending'
    : p.ddFreezeBps > 0n && dd >= p.ddFreezeBps
    ? 'Drawdown freeze'
    : 'Active';
  const quietSecs = Number(p.lpReopenQuietSecs);
  const explain = (names: string[], a: ErrorContext['action']) =>
    names.map((n) => explainProgramError(n, { action: a, inWindow: gates?.inWindow, inQuiet: gates?.inQuiet, quietSecs }));

  const amountNum = parseFloat(amount);
  const amountUnits = amountNum > 0 ? toUnits(amountNum) : 0n;
  const expectedShares = action === 'deposit' && view && amountUnits > 0n ? depositShares(pool, view, amountUnits) : null;
  const requestValue =
    action === 'withdraw' && view && amountUnits > 0n ? redeemValue(amountUnits, pool.lpShares, view.navRedeem) : null;
  const hasRequest = !!request && request.state !== 'none';
  const freeShares = summary?.freeShares ?? 0n;

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!(amountNum > 0)) return;
    try {
      if (action === 'deposit') await deposit(amountNum);
      else await requestWithdrawal(amountUnits > freeShares ? freeShares : amountUnits);
      setAmount('');
    } catch (err) {
      console.error('Transaction failed:', err);
    }
  };

  const netSide = view ? (view.net > 0n ? 'SHORT' : view.net < 0n ? 'LONG' : 'FLAT') : 'FLAT';
  // lp_withdraw's post-checks on the payout (§9.4 step 8): the cash, the
  // hard-net line and, with the weekend bound on, G·net ≤ α·B' (D43)
  const refusal = request && request.state === 'claimable' && view ? withdrawRefusal(market, pool, view, request.payout) : null;
  // What can leave now: with traders at the net cap, the exit buffer (D45)
  const room = view && !isSettled(market) && view.netNotional > 0n ? withdrawRoom(market, pool, view) : null;
  // The buffer between the net cap and the weekend line at the market's
  // current G (D45): 20% at production params, 0 or less at k_net = α/G or
  // after G was raised, so the notice states it from the live params
  const buffer = exitBufferBps(market, pool);
  const delayText = formatDuration(Number(p.withdrawalDelaySecs));
  const bufferText =
    buffer === null
      ? ''
      : buffer > 0n
      ? `This market's net cap sits ${bps(buffer)} of capital below this line, so with traders at the cap, a steady price ` +
        'and nothing pending LPs keep about that much exit room; a price move, liquidations or pool losses can shrink it or use it up. '
      : `This market's net cap ${buffer === 0n ? 'reaches' : 'is past'} this line, so with traders at the cap there is no exit room. `;
  const refusalText =
    refusal?.line === 'weekend'
      ? "Claiming all of it now would leave the pool's capital below its weekend bound: a weekend gap of G on the traders' open net " +
        `must cost LPs at most ${bps(p.weekendLpLossBps)} of the capital that stays, so capital backing that net cannot leave. ` +
        bufferText +
        (room !== null && room > 0n
          ? `Up to ${usd(room)} could leave now. That room is shared by every LP's matured claim, first come first served, ` +
            `and is not reserved for you: a smaller request (cancel and request fewer shares) waits the full withdrawal delay ` +
            `(${delayText}) again, and the room may be gone by then. The rest waits until traders close and the net shrinks. `
          : 'Nothing can leave now: exits wait until traders close or the price moves back. ') +
        'Exposure ADL is not run for LP exits: it acts only when the net passes the hard line, or in a drawdown.'
      : refusal?.line === 'hard_net'
      ? explainProgramError('PoolCapWouldBreach', { action: 'lpWithdraw' })
      : refusal
      ? explainProgramError(refusal.error, { action: 'lpWithdraw' })
      : null;
  const flowGates = action === 'deposit' ? gates?.deposit ?? [] : gates?.request ?? [];

  return (
    <div className="lp-vault">
      <div className="vault-header">
        <h3>LP Pool · {selectedCommodity.id}</h3>
        <span className={`vault-status ${status === 'Active' ? 'active' : 'warning'}`}>{status}</span>
      </div>

      <div className="vault-stats-grid">
        <div className="vault-stat">
          <span className="stat-label" title="NAV_redeem: what withdrawals are valued at">Pool NAV</span>
          <span className="stat-value">{view ? usd(view.b) : '-'}</span>
        </div>
        <div className="vault-stat">
          <span className="stat-label" title="NAV_redeem per share (withdrawals); deposits pay NAV_mint per share">Share Value</span>
          <span className="stat-value">{summary ? perShare(summary.navRedeemPerShare) : '-'}</span>
          {summary && <span className="stat-sub">deposit at {perShare(summary.navMintPerShare)}</span>}
        </div>
        <div className="vault-stat">
          <span className="stat-label" title="The pool holds the opposite of the traders' net position">Pool Net Exposure</span>
          <span className={`stat-value ${netSide === 'SHORT' ? 'short' : netSide === 'LONG' ? 'long' : ''}`}>
            {netSide} {view ? usd(view.netNotional) : ''}
          </span>
        </div>
        <div className="vault-stat">
          <span className="stat-label">Cash</span>
          <span className="stat-value">{usd(pool.cash)}</span>
        </div>
        <div className="vault-stat">
          <span className="stat-label">Insurance Fund</span>
          <span className="stat-value">{usd(market.insuranceFund)}</span>
        </div>
        <div className="vault-stat">
          <span className="stat-label" title="Per-share NAV against its 7-day peak">Drawdown</span>
          <span className="stat-value">{bps(dd)}</span>
        </div>
        {adlRequired(pool) && (
          <div className="vault-stat">
            <span className="stat-label">Pending Bad Debt</span>
            <span className="stat-value negative">{usd(pool.pendingBadDebtLong + pool.pendingBadDebtShort)}</span>
          </div>
        )}
        {unvested(pool.vestAmount, pool.vestStart, pool.vestEnd, BigInt(now)) > 0n && (
          <div className="vault-stat">
            <span className="stat-label" title="Donations and insurance sweeps vest into NAV_redeem over the withdrawal delay">Vesting</span>
            <span className="stat-value">{usd(unvested(pool.vestAmount, pool.vestStart, pool.vestEnd, BigInt(now)))}</span>
          </div>
        )}
      </div>

      <div className="vault-divider" />

      <div className="user-position">
        <h4>Your Position</h4>
        <div className="position-stats">
          <div className="position-stat">
            <span className="stat-label">Shares</span>
            <span className="stat-value">{lpPosition ? shares(lpPosition.shares) : '0'}</span>
          </div>
          <div className="position-stat">
            <span className="stat-label">Value</span>
            <span className="stat-value">{summary ? usd(summary.positionValue) : '$0.00'}</span>
          </div>
          <div className="position-stat">
            <span className="stat-label">Deposited / Withdrawn</span>
            <span className="stat-value">
              {lpPosition ? `${usd(lpPosition.depositedCum)} / ${usd(lpPosition.withdrawnCum)}` : '-'}
            </span>
          </div>
        </div>

        {hasRequest && request && (
          <div className={`withdrawal-request ${request.state}`}>
            <h4>Withdrawal Request</h4>
            <div className="info-item">
              <span>Shares</span>
              <span>{shares(request.pendingShares)}</span>
            </div>
            <div className="info-item">
              <span title="Fixed at request time; you get the lower of this and the value when you claim">Requested value</span>
              <span>{usd(request.requestedValue)}</span>
            </div>
            <div className="info-item">
              <span>Payout now</span>
              <span>{usd(request.payout)}</span>
            </div>
            <div className="info-item">
              <span>Unlocks</span>
              <span>
                {when(request.unlockAt)}
                {request.state === 'waiting' && ` (in ${formatDuration(request.unlockAt - now)})`}
              </span>
            </div>
            <div className="info-item">
              <span title="Counted in open market hours after the unlock">Claim by</span>
              <span>{request.claimDeadline ? when(request.claimDeadline) : 'no deadline'}</span>
            </div>
            {request.state === 'lapsed' && (
              <div className="cooldown-notice">
                This request lapsed: its claim window passed. Cancel it and request again.
              </div>
            )}
            {request.state === 'claimable' &&
              explain(gates?.withdraw ?? [], 'lpWithdraw').map((m) => (
                <div key={m} className="cooldown-notice">
                  {m}
                </div>
              ))}
            {refusalText && <div className="cooldown-notice">{refusalText}</div>}
            <div className="request-actions">
              {request.state === 'claimable' && (
                <button className="submit-btn withdraw" onClick={() => withdraw().catch(() => {})} disabled={isLoading}>
                  Claim {usd(request.payout)}
                </button>
              )}
              <button className="request-btn" onClick={() => cancelWithdrawal().catch(() => {})} disabled={isLoading}>
                Cancel request
              </button>
            </div>
          </div>
        )}
      </div>

      <div className="vault-divider" />

      <div className="vault-actions">
        <div className="action-tabs">
          <button className={`action-tab ${action === 'deposit' ? 'active' : ''}`} onClick={() => setAction('deposit')}>
            Deposit
          </button>
          <button className={`action-tab ${action === 'withdraw' ? 'active' : ''}`} onClick={() => setAction('withdraw')}>
            Withdraw
          </button>
        </div>

        <form onSubmit={handleSubmit} className="vault-form">
          <div className="input-group">
            <input
              type="number"
              value={amount}
              onChange={(e) => setAmount(e.target.value)}
              placeholder={action === 'deposit' ? 'Amount to deposit' : 'Shares to withdraw'}
              min="0"
              step="0.000001"
            />
            <span className="input-suffix">{action === 'deposit' ? 'USDC' : 'shares'}</span>
            {action === 'withdraw' && freeShares > 0n && (
              <button type="button" className="max-btn" onClick={() => setAmount(fromUnits(freeShares).toString())}>
                Max
              </button>
            )}
          </div>

          {expectedShares !== null && expectedShares !== undefined && (
            <div className="info-item">
              <span>You receive</span>
              <span>{shares(expectedShares)} shares</span>
            </div>
          )}
          {requestValue !== null && (
            <div className="info-item">
              <span>Value at request</span>
              <span>{usd(requestValue)}</span>
            </div>
          )}

          {flowGates.length > 0 &&
            explain(flowGates, action === 'deposit' ? 'lpDeposit' : 'lpRequest').map((m) => (
              <div key={m} className="cooldown-notice">
                {m}
              </div>
            ))}
          {gates?.needsFundingCrank && (
            <div className="info-notice">Funding is due: it will be cranked first, in its own transaction (one extra signature).</div>
          )}
          {action === 'withdraw' && hasRequest && (
            <div className="info-notice">One request at a time: claim or cancel the pending one first.</div>
          )}

          <button
            type="submit"
            className={`submit-btn ${action}`}
            disabled={
              isLoading ||
              !(amountNum > 0) ||
              flowGates.length > 0 ||
              (action === 'withdraw' && (hasRequest || freeShares === 0n))
            }
          >
            {isLoading ? 'Processing...' : action === 'deposit' ? 'Deposit USDC' : 'Request Withdrawal'}
          </button>
          {error && <div className="error-message">{error}</div>}
        </form>
      </div>

      <div className="vault-info">
        <div className="info-item">
          <span>Withdrawal delay</span>
          <span>{formatDuration(Number(p.withdrawalDelaySecs))}</span>
        </div>
        <div className="info-item">
          <span title="Open market hours after the unlock to claim; then the request lapses">Claim window</span>
          <span>{p.withdrawalClaimWindowSecs > 0n ? `${formatDuration(Number(p.withdrawalClaimWindowSecs))} open` : 'none'}</span>
        </div>
        <div className="info-item">
          <span title="No LP withdrawals this long before the weekly close (W)">Pre-close exit block</span>
          <span>{p.weekendCapWindowSecs > 0n ? formatDuration(Number(p.weekendCapWindowSecs)) : 'off'}</span>
        </div>
        <div className="info-item">
          <span title="With the weekend bound on, LP capital backing the net cannot leave below α/G of the net (D43). Traders' net is capped lower (k_net), and the gap is the LPs' exit buffer at the cap (D45)">
            Weekend bound
          </span>
          <span>{p.weekendCapWindowSecs > 0n ? `gap loss ≤ ${bps(p.weekendLpLossBps)} of capital` : 'off'}</span>
        </div>
        {room !== null && (
          <div className="info-item">
            <span title="The most a claim could take out now before lp_withdraw's post-checks refuse it (cash, the hard-net line, the weekend bound). With traders at the net cap this is the exit buffer (D45)">
              LP exit room now
            </span>
            <span>{usd(room)}</span>
          </div>
        )}
        <div className="info-item">
          <span title="No LP deposits or withdrawals this long after each session open (Q)">Reopen quiet</span>
          <span>{quietSecs > 0 ? formatDuration(quietSecs) : 'off'}</span>
        </div>
        <div className="info-item">
          <span>Open fee / impact cap</span>
          <span>
            {bps(p.openFeeBps)} / {bps(p.maxImpactBps)}
          </span>
        </div>
      </div>
    </div>
  );
}
