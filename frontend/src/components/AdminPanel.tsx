import { useEffect, useMemo, useState } from 'react';
import { PublicKey } from '@solana/web3.js';
import { TOKEN_PROGRAM_ID, getAssociatedTokenAddress } from '@solana/spl-token';
import { BN } from '@project-serum/anchor';
import { useMarketStore, COMMODITIES } from '../stores/marketStore';
import { usePerpsProgram } from '../hooks/usePerpsProgram';
import { useLpPool } from '../hooks/useLpPool';
import { getConfigPDA, getVaultPDA, getVaultTokenAccountPDA } from '../utils/pda';
import {
  POOL_PARAM_KEYS,
  encodePoolParams,
  fromUnits,
  isSettled,
  marketConfigParams,
  poolParamMax,
  poolStatus,
  poolView,
  proposalTimelock,
  toUnits,
  validatePoolParams,
  type PoolParams,
} from '../utils/pool';
import { formatDuration } from '../utils/session';
import { parseAnchorError } from '../utils/transaction';

const usd = (x: bigint) => `$${fromUnits(x).toLocaleString(undefined, { minimumFractionDigits: 2, maximumFractionDigits: 2 })}`;
const pct = (bps: bigint) => `${(Number(bps) / 100).toFixed(2)}%`;
const when = (t: bigint) => (t > 0n ? new Date(Number(t) * 1000).toLocaleString() : '-');
const bn = (x: bigint) => new BN(x.toString());

/** snake_case label for a PoolParams field, as in the program and the spec. */
const label = (k: string) => k.replace(/[A-Z]/g, (c) => `_${c.toLowerCase()}`);

function parseParam(key: keyof PoolParams, value: string): bigint | null {
  if (!/^\d+$/.test(value.trim())) return null;
  const v = BigInt(value.trim());
  return v <= poolParamMax(key) ? v : null;
}

export function AdminPanel() {
  const { program, publicKey, connected } = usePerpsProgram();
  const { selectedCommodity } = useMarketStore();
  const [selectedMarket, setSelectedMarket] = useState(selectedCommodity.id);
  const feedPrice = useMarketStore((s) => s.markets[selectedMarket]?.price);
  const { data, refresh } = useLpPool(selectedMarket);
  const [admin, setAdmin] = useState<string | null>(null);
  const [now, setNow] = useState(() => Math.floor(Date.now() / 1000));
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<{ ok: boolean; text: string } | null>(null);

  // Form state
  const [maxOi, setMaxOi] = useState('');
  const [draft, setDraft] = useState<Record<string, string>>({});
  const [maxPoolCash, setMaxPoolCash] = useState('');
  const [adlKeeper, setAdlKeeper] = useState('');
  const [donation, setDonation] = useState('');
  const [treasuryAmount, setTreasuryAmount] = useState('');
  const [treasuryDest, setTreasuryDest] = useState('');

  useEffect(() => {
    const id = setInterval(() => setNow(Math.floor(Date.now() / 1000)), 5_000);
    return () => clearInterval(id);
  }, []);

  // The protocol admin signs every admin instruction (ProtocolConfig.admin)
  useEffect(() => {
    if (!program) return;
    program.account.protocolConfig
      .fetch(getConfigPDA()[0])
      .then((c) => setAdmin(String((c as { admin: PublicKey }).admin)))
      .catch(() => setAdmin(null));
  }, [program]);

  // Start the params editor from the pool's current params
  const pool = data?.pool ?? null;
  const market = data?.market ?? null;
  useEffect(() => {
    if (!pool) return;
    setDraft(Object.fromEntries(POOL_PARAM_KEYS.map((k) => [k, pool.params[k].toString()])));
    // Reset only when the market or the applied params change, not on every refresh
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectedMarket, pool && POOL_PARAM_KEYS.map((k) => pool.params[k].toString()).join(',')]);

  const parsed = useMemo(() => {
    const out = {} as PoolParams;
    const bad: string[] = [];
    for (const k of POOL_PARAM_KEYS) {
      const v = parseParam(k, draft[k] ?? '');
      if (v === null) bad.push(`${label(k)}: not an integer in range`);
      else out[k] = v;
    }
    return { params: bad.length ? null : out, bad };
  }, [draft]);
  const invalid = parsed.params && market ? validatePoolParams(parsed.params, market) : parsed.bad;

  const mark = market && isSettled(market) ? market.settlementPrice : feedPrice ? toUnits(feedPrice) : null;
  const view = market && pool && mark ? poolView(pool, market, mark, now) : null;
  const status = market && pool ? poolStatus(market, pool, view, now) : null;

  const isAdmin = connected && !!publicKey && admin === publicKey.toBase58();

  const send = async (what: string, build: () => Promise<string>) => {
    setBusy(true);
    setMessage(null);
    try {
      const sig = await build();
      setMessage({ ok: true, text: `${what}: ${sig.slice(0, 16)}…` });
      await refresh();
    } catch (err) {
      console.error(`${what} failed:`, err);
      setMessage({ ok: false, text: `${what} failed: ${parseAnchorError(err, { action: 'admin' })}` });
    } finally {
      setBusy(false);
    }
  };

  if (!connected) {
    return (
      <div className="admin-panel">
        <div className="admin-header">
          <h3>Admin Panel</h3>
        </div>
        <div className="admin-notice">Connect wallet to access admin features.</div>
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
          Connected wallet is not the protocol admin.
          <br />
          <span className="wallet-address">{publicKey?.toBase58()}</span>
        </div>
      </div>
    );
  }

  const accounts = data && program && publicKey
    ? { program, admin: publicKey, config: getConfigPDA()[0], market: data.marketAddress, lpPool: data.poolAddress }
    : null;

  const updateMaxOi = () =>
    accounts && market &&
    send('Update max open interest', () => {
      const p = marketConfigParams(market, { maxOpenInterest: toUnits(parseFloat(maxOi)) });
      return accounts.program.methods
        .updateMarketConfig({ ...p, minOraclePrice: bn(p.minOraclePrice), maxOpenInterest: bn(p.maxOpenInterest) })
        .accounts({ authority: accounts.admin, config: accounts.config, market: accounts.market })
        .rpc();
    });

  const togglePause = () =>
    accounts && market &&
    send(market.isPaused ? 'Unpause market' : 'Pause market', () => {
      const p = marketConfigParams(market, { isPaused: !market.isPaused });
      return accounts.program.methods
        .updateMarketConfig({ ...p, minOraclePrice: bn(p.minOraclePrice), maxOpenInterest: bn(p.maxOpenInterest) })
        .accounts({ authority: accounts.admin, config: accounts.config, market: accounts.market })
        .rpc();
    });

  const propose = () =>
    accounts && parsed.params &&
    send('Propose pool params', () =>
      accounts.program.methods
        .proposePoolParams(encodePoolParams(parsed.params!, bn))
        .accounts({ admin: accounts.admin, config: accounts.config, market: accounts.market, lpPool: accounts.lpPool })
        .rpc()
    );

  const apply = () =>
    accounts &&
    send('Apply pool params', () =>
      accounts.program.methods.applyPoolParams().accounts({ market: accounts.market, lpPool: accounts.lpPool }).rpc()
    );

  const cancel = () =>
    accounts &&
    send('Cancel pool params', () =>
      accounts.program.methods
        .cancelPoolParams()
        .accounts({ admin: accounts.admin, config: accounts.config, lpPool: accounts.lpPool })
        .rpc()
    );

  const setCashCap = () =>
    accounts &&
    send('Set max pool cash', () =>
      accounts.program.methods
        .setMaxPoolCash(bn(toUnits(parseFloat(maxPoolCash) || 0)))
        .accounts({ admin: accounts.admin, config: accounts.config, lpPool: accounts.lpPool })
        .rpc()
    );

  const setKeeper = () =>
    accounts &&
    send('Set ADL keeper', () =>
      accounts.program.methods
        .setAdlKeeper(new PublicKey(adlKeeper.trim()))
        .accounts({ admin: accounts.admin, config: accounts.config, lpPool: accounts.lpPool })
        .rpc()
    );

  const donate = () =>
    accounts && data &&
    send('Donate to pool', async () =>
      accounts.program.methods
        .donateToPool(bn(toUnits(parseFloat(donation))))
        .accounts({
          donor: accounts.admin,
          market: accounts.market,
          lpPool: accounts.lpPool,
          vaultTokenAccount: getVaultTokenAccountPDA(accounts.market)[0],
          donorTokenAccount: await getAssociatedTokenAddress(data.marketCollateralMint, accounts.admin),
          tokenProgram: TOKEN_PROGRAM_ID,
        })
        .rpc()
    );

  const withdrawTreasury = () =>
    accounts && data &&
    send('Withdraw treasury', async () =>
      accounts.program.methods
        .withdrawTreasury(bn(toUnits(parseFloat(treasuryAmount))))
        .accounts({
          admin: accounts.admin,
          config: accounts.config,
          market: accounts.market,
          lpPool: accounts.lpPool,
          vault: getVaultPDA(accounts.market)[0],
          vaultTokenAccount: getVaultTokenAccountPDA(accounts.market)[0],
          destination: treasuryDest.trim()
            ? new PublicKey(treasuryDest.trim())
            : await getAssociatedTokenAddress(data.marketCollateralMint, accounts.admin),
          tokenProgram: TOKEN_PROGRAM_ID,
        })
        .rpc()
    );

  const changed = pool && parsed.params ? POOL_PARAM_KEYS.filter((k) => parsed.params![k] !== pool.params[k]) : [];
  const pending = pool && pool.pendingParamsAt !== 0n ? POOL_PARAM_KEYS.filter((k) => pool.pendingParams[k] !== pool.params[k]) : [];

  return (
    <div className="admin-panel">
      <div className="admin-header">
        <h3>Market Administration</h3>
        <span className="admin-badge">Admin</span>
      </div>

      <div className="admin-section">
        <label>Select Market</label>
        <select value={selectedMarket} onChange={(e) => setSelectedMarket(e.target.value)}>
          {COMMODITIES.map((c) => (
            <option key={c.id} value={c.id}>
              {c.icon} {c.name} ({c.symbol})
            </option>
          ))}
        </select>
      </div>

      {message && <div className={message.ok ? 'success-message' : 'error-message'}>{message.text}</div>}

      {!market ? (
        <p>Loading on-chain data...</p>
      ) : (
        <>
          <div className="admin-section">
            <h4>Market</h4>
            <div className="onchain-data">
              <div className="data-row"><span>Max leverage / IM / MM:</span><span>{Number(market.maxLeverage) / 1000}x / {pct(market.initialMarginRatio)} / {pct(market.maintenanceMarginRatio)}</span></div>
              <div className="data-row"><span>Weekend IM / MM:</span><span>{pct(market.weekendInitialMarginRatio)} / {pct(market.weekendMaintenanceMarginRatio)}</span></div>
              <div className="data-row"><span>Taker fee / liquidation fee:</span><span>{pct(market.takerFee)} / {pct(market.liquidationFee)}</span></div>
              <div className="data-row"><span>Open interest (long / short):</span><span>{fromUnits(market.longOpenInterest).toLocaleString()} / {fromUnits(market.shortOpenInterest).toLocaleString()} contracts</span></div>
              <div className="data-row"><span>Max open interest:</span><span>{fromUnits(market.maxOpenInterest).toLocaleString()} contracts per side</span></div>
            </div>
            <div className="param-grid">
              <div className="param-item wide">
                <label>Max open interest (contracts per side; 0 keeps)</label>
                <input type="number" min={0} value={maxOi} onChange={(e) => setMaxOi(e.target.value)} placeholder={fromUnits(market.maxOpenInterest).toString()} />
                <button className="update-btn" disabled={busy || !(parseFloat(maxOi) >= 0) || isSettled(market)} onClick={updateMaxOi}>Update</button>
              </div>
            </div>
            <div className={`status-indicator ${market.isPaused ? 'paused' : 'active'}`}>
              <span className="status-dot" />
              {isSettled(market) ? 'SETTLED' : market.isPaused ? 'PAUSED' : 'ACTIVE'}
            </div>
            <button className={`toggle-pause-btn ${market.isPaused ? 'unpause' : 'pause'}`} disabled={busy || isSettled(market)} onClick={togglePause}>
              {market.isPaused ? 'Unpause Market' : 'Pause Market'}
            </button>
          </div>

          {!pool ? (
            <div className="admin-notice warning">This market has no LP pool (initialize_pool has not run).</div>
          ) : (
            <>
              <div className="admin-section">
                <h4>Pool Status</h4>
                <div className="onchain-data">
                  <div className="data-row"><span>Status:</span><span>
                    {[
                      !status?.hasLps && 'not seeded',
                      status?.adlRequired && 'ADL pending',
                      status?.ddFreeze && 'drawdown freeze',
                      status?.nTrig && 'net over hard line',
                      status?.dTrig && 'drawdown ADL',
                      status?.wTrig && 'weekend force-reduce',
                      status?.solvencyHaircut && 'U > cash: profits haircut',
                    ].filter(Boolean).join(', ') || 'active'}
                  </span></div>
                  <div className="data-row"><span>NAV redeem / mint:</span><span>{view ? `${usd(view.navRedeem)} / ${usd(view.navMint)}` : '-'}</span></div>
                  <div className="data-row"><span>Cash:</span><span>{usd(pool.cash)}</span></div>
                  <div className="data-row"><span>Insurance fund:</span><span>{usd(market.insuranceFund)} (target {usd(pool.params.insuranceTarget)})</span></div>
                  <div className="data-row"><span>Treasury:</span><span>{usd(pool.treasury)}</span></div>
                  <div className="data-row"><span>Free / position collateral:</span><span>{usd(pool.freeCollateral)} / {usd(pool.positionCollateral)}</span></div>
                  <div className="data-row"><span>LP shares (pending):</span><span>{fromUnits(pool.lpShares).toLocaleString()} ({fromUnits(pool.pendingWithdrawShares).toLocaleString()})</span></div>
                  <div className="data-row"><span>Net (traders), notional:</span><span>{view ? `${fromUnits(view.net).toLocaleString()} contracts, ${usd(view.netNotional)}` : '-'}</span></div>
                  <div className="data-row"><span>Vesting:</span><span>{status ? `${usd(status.unvested)} unvested, ends ${when(pool.vestEnd)}` : '-'}</span></div>
                  <div className="data-row"><span>Drawdown:</span><span>{status ? pct(status.ddBps) : '-'} (peak set {when(pool.npsPeakAt)})</span></div>
                  <div className="data-row"><span>Pending bad debt long / short:</span><span>{usd(pool.pendingBadDebtLong)} (since {when(pool.pendingSinceLong)}) / {usd(pool.pendingBadDebtShort)} (since {when(pool.pendingSinceShort)})</span></div>
                  <div className="data-row"><span>Bad debt total / recovered / written off:</span><span>{usd(pool.badDebtCum)} / {usd(pool.badDebtRecoveredCum)} / {usd(pool.badDebtWrittenOffCum)}</span></div>
                  <div className="data-row"><span>Loss window (LP / insurance used):</span><span>{when(pool.lossWindowStart)}: {usd(pool.lpAbsorbedWindow)} / {usd(pool.insPaidWindow)}</span></div>
                  <div className="data-row"><span>Max pool cash:</span><span>{pool.maxPoolCash === 0n ? 'unlimited' : usd(pool.maxPoolCash)}</span></div>
                  <div className="data-row"><span>ADL keeper:</span><span className="wallet-address">{pool.adlKeeper}</span></div>
                </div>
              </div>

              <div className="admin-section">
                <h4>Pool Parameters</h4>
                {pool.pendingParamsAt !== 0n && (
                  <div className="admin-notice">
                    Pending change ({pending.length ? pending.map(label).join(', ') : 'no field differs'}), applies{' '}
                    {status?.paramsApplyIn ? `in ${formatDuration(status.paramsApplyIn)} (${when(pool.pendingParamsAt)})` : 'now'}.
                    <div className="admin-actions">
                      <button className="update-btn" disabled={busy || (status?.paramsApplyIn ?? 1) > 0} onClick={apply}>Apply</button>
                      <button className="toggle-pause-btn pause" disabled={busy} onClick={cancel}>Cancel</button>
                    </div>
                  </div>
                )}
                <div className="param-grid">
                  {POOL_PARAM_KEYS.map((k) => (
                    <div className="param-item" key={k}>
                      <label title={k}>{label(k)}</label>
                      <input
                        value={draft[k] ?? ''}
                        className={changed.includes(k) ? 'changed' : ''}
                        onChange={(e) => setDraft((d) => ({ ...d, [k]: e.target.value }))}
                      />
                      {pool.pendingParamsAt !== 0n && pool.pendingParams[k] !== pool.params[k] && (
                        <span className="param-unit">pending: {pool.pendingParams[k].toString()}</span>
                      )}
                    </div>
                  ))}
                </div>
                {invalid.length > 0 && (
                  <div className="error-message">
                    Invalid: {invalid.join('; ')}
                  </div>
                )}
                {parsed.params && changed.length > 0 && (
                  <div className="admin-notice">
                    Changes: {changed.map(label).join(', ')}. Timelock:{' '}
                    {formatDuration(Number(proposalTimelock(pool, parsed.params)))}
                    {pool.lpShares === 0n && ' (unseeded pool: applies at once)'}.
                  </div>
                )}
                <div className="admin-actions">
                  <button className="update-btn" disabled={busy || invalid.length > 0 || changed.length === 0} onClick={propose}>
                    Propose Pool Parameters
                  </button>
                </div>
              </div>

              <div className="admin-section">
                <h4>Pool Controls</h4>
                <div className="param-grid">
                  <div className="param-item wide">
                    <label>Max pool cash (USDC; 0 = unlimited)</label>
                    <input type="number" min={0} value={maxPoolCash} onChange={(e) => setMaxPoolCash(e.target.value)} />
                    <button className="update-btn" disabled={busy || maxPoolCash === ''} onClick={setCashCap}>Set</button>
                  </div>
                  <div className="param-item wide">
                    <label>ADL keeper</label>
                    <input value={adlKeeper} onChange={(e) => setAdlKeeper(e.target.value)} placeholder={pool.adlKeeper} />
                    <button className="update-btn" disabled={busy || !adlKeeper.trim()} onClick={setKeeper}>Set</button>
                  </div>
                  <div className="param-item wide">
                    <label>Donate to pool (USDC; repays pending bad debt first, then vests)</label>
                    <input type="number" min={0} value={donation} onChange={(e) => setDonation(e.target.value)} />
                    <button className="update-btn" disabled={busy || !(parseFloat(donation) > 0)} onClick={donate}>Donate</button>
                  </div>
                  <div className="param-item wide">
                    <label>Withdraw treasury (USDC; max {usd(pool.treasury)})</label>
                    <input type="number" min={0} value={treasuryAmount} onChange={(e) => setTreasuryAmount(e.target.value)} />
                    <input value={treasuryDest} onChange={(e) => setTreasuryDest(e.target.value)} placeholder="Destination token account (default: your USDC account)" />
                    <button
                      className="update-btn"
                      disabled={busy || !(parseFloat(treasuryAmount) > 0) || toUnits(parseFloat(treasuryAmount) || 0) > pool.treasury}
                      onClick={withdrawTreasury}
                    >
                      Withdraw
                    </button>
                  </div>
                </div>
              </div>
            </>
          )}
        </>
      )}
    </div>
  );
}
