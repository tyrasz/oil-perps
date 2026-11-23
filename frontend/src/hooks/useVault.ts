import { useCallback, useEffect, useMemo, useState } from 'react';
import { PublicKey, type TransactionInstruction } from '@solana/web3.js';
import { clusterUnixTime, ensureFundingCurrent } from '../utils/fundingCrank';
import { TOKEN_PROGRAM_ID, getAssociatedTokenAddress } from '@solana/spl-token';
import { BN } from '@project-serum/anchor';
import { usePerpsProgram } from './usePerpsProgram';
import { useLpPool } from './useLpPool';
import { useMarketStore } from '../stores/marketStore';
import { getLpPositionPDA, getVaultPDA, getVaultTokenAccountPDA, LP_CLASS_COMMON } from '../utils/pda';
import {
  decodeLpPosition,
  decodeMarket,
  depositShares,
  isSettled,
  lpGates,
  poolView,
  toUnits,
  withdrawalRequest,
  type LpGates,
  type LpPositionAccount,
  type WithdrawalRequest,
} from '../utils/pool';
import { navPerShare, redeemValue, type PoolView } from '../utils/poolMath';
import { parseAnchorError } from '../utils/transaction';
import { LP_DEPOSIT_SLIPPAGE_BPS, PYTH_PRICE_FEEDS } from '../config/program';
import type { ErrorContext } from '../utils/errors';

const nowSecs = () => Math.floor(Date.now() / 1000);

/**
 * The selected market's LP pool (perps-core, docs/lp-pool.md §9): the house
 * counterparty of every trade. LPs deposit at NAV_mint, request a withdrawal
 * (which starts the delay and fixes its value), then claim it at the lower of
 * that value and the value at claim time, within the claim window.
 */
export function useVault() {
  const { program, publicKey } = usePerpsProgram();
  const selectedCommodity = useMarketStore((s) => s.selectedCommodity);
  const feedPrice = useMarketStore((s) => s.markets[s.selectedCommodity.id]?.price);
  const { data, error: poolError, refresh: refreshPool } = useLpPool(selectedCommodity.id);
  const [lpPosition, setLpPosition] = useState<LpPositionAccount | null>(null);
  const [lpPositionExists, setLpPositionExists] = useState(false);
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [now, setNow] = useState(nowSecs);

  useEffect(() => {
    const id = setInterval(() => setNow(nowSecs()), 15_000);
    return () => clearInterval(id);
  }, []);

  const lpPositionPda = useMemo(
    () => (data && publicKey ? getLpPositionPDA(data.poolAddress, publicKey, LP_CLASS_COMMON)[0] : null),
    [data, publicKey]
  );

  const fetchLpPosition = useCallback(async () => {
    if (!program || !lpPositionPda) {
      setLpPosition(null);
      setLpPositionExists(false);
      return;
    }
    try {
      const raw = await program.account.lpPosition.fetchNullable(lpPositionPda);
      setLpPositionExists(!!raw);
      setLpPosition(raw ? decodeLpPosition(raw as Record<string, unknown>) : null);
    } catch (err) {
      console.error('Error fetching LP position:', err);
    }
  }, [program, lpPositionPda]);

  useEffect(() => {
    fetchLpPosition();
    const id = setInterval(fetchLpPosition, 15_000);
    return () => clearInterval(id);
  }, [fetchLpPosition]);

  const refresh = useCallback(async () => {
    await Promise.all([refreshPool(), fetchLpPosition()]);
  }, [refreshPool, fetchLpPosition]);

  // Valuation at the settlement price once settled, else the latest price
  const market = data?.market ?? null;
  const pool = data?.pool ?? null;
  const mark = market && isSettled(market) ? market.settlementPrice : feedPrice ? toUnits(feedPrice) : null;
  const view: PoolView | null = useMemo(
    () => (market && pool && mark ? poolView(pool, market, mark, now) : null),
    [market, pool, mark, now]
  );
  const gates: LpGates | null = useMemo(() => (market && pool ? lpGates(market, pool, view, now) : null), [market, pool, view, now]);
  const request: WithdrawalRequest | null = useMemo(
    () => (market && pool && lpPosition ? withdrawalRequest(market, pool, lpPosition, view, now) : null),
    [market, pool, lpPosition, view, now]
  );

  /**
   * Accounts every LP instruction shares, after making funding current. LP
   * flows fail FundingStale unless funding is current, and update_funding
   * fails FundingNotDue unless a period is due; any trade cranks funding
   * inline (D40), so the 15 s snapshot and the browser clock cannot decide
   * between the two. ensureFundingCurrent re-reads the Market and the
   * cluster clock, waits out a boundary that is too close to call, and
   * cranks in its own transaction (ignoring FundingNotDue) so the LP
   * instruction never reverts because of the crank.
   */
  const context = useCallback(async () => {
    if (!program || !publicKey || !data || !lpPositionPda) throw new Error('Wallet not connected');
    if (!data.pool) throw new Error('This market has no LP pool');
    const pythPriceFeed = PYTH_PRICE_FEEDS[selectedCommodity.id] ?? new PublicKey(selectedCommodity.pythPriceFeed);
    const connection = program.provider.connection;
    const fresh = await ensureFundingCurrent({
      fetchMarket: async () => decodeMarket((await program.account.market.fetch(data.marketAddress)) as Record<string, unknown>),
      chainNow: () => clusterUnixTime(connection),
      fallbackNow: nowSecs,
      sendCrank: () =>
        program.methods.updateFunding().accounts({ keeper: publicKey, market: data.marketAddress, pythPriceFeed }).rpc(),
      sleep: (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
    });
    const g = lpGates(fresh.market, data.pool, null, fresh.now);
    return { program, publicKey, data, lpPositionPda, pythPriceFeed, gates: g };
  }, [program, publicKey, data, lpPositionPda, selectedCommodity]);

  const run = useCallback(
    async (action: ErrorContext['action'], send: () => Promise<string>, gates?: LpGates) => {
      setIsLoading(true);
      setError(null);
      try {
        const sig = await send();
        await refresh();
        return sig;
      } catch (err) {
        console.error(`${action} failed:`, err);
        const message = parseAnchorError(err, {
          action,
          inWindow: gates?.inWindow,
          inQuiet: gates?.inQuiet,
          settledQuiet: gates?.settledQuiet,
          quietSecs: data?.pool ? Number(data.pool.params.lpReopenQuietSecs) : undefined,
        });
        setError(message);
        throw new Error(message);
      } finally {
        setIsLoading(false);
      }
    },
    [refresh, data]
  );

  /** Deposit `amount` USDC; creates the LP position first if needed. */
  const deposit = useCallback(
    async (amount: number) => {
      const c = await context();
      return run('lpDeposit', async () => {
        const amountUnits = toUnits(amount);
        // Minimum shares: the quote at NAV_mint, LP_DEPOSIT_SLIPPAGE_BPS lower
        const expected = view && c.data.pool ? depositShares(c.data.pool, view, amountUnits) : null;
        const minShares = expected ? (expected * BigInt(10_000 - LP_DEPOSIT_SLIPPAGE_BPS)) / 10_000n : 1n;
        const pre: TransactionInstruction[] = [];
        if (!lpPositionExists) {
          pre.unshift(
            await c.program.methods
              .initLpPosition(LP_CLASS_COMMON)
              .accounts({ owner: c.publicKey, lpPool: c.data.poolAddress, lpPosition: c.lpPositionPda })
              .instruction()
          );
        }
        const ownerTokenAccount = await getAssociatedTokenAddress(c.data.marketCollateralMint, c.publicKey);
        return c.program.methods
          .lpDeposit(new BN(amountUnits.toString()), new BN((minShares > 0n ? minShares : 1n).toString()))
          .accounts({
            owner: c.publicKey,
            market: c.data.marketAddress,
            lpPool: c.data.poolAddress,
            lpPosition: c.lpPositionPda,
            vaultTokenAccount: getVaultTokenAccountPDA(c.data.marketAddress)[0],
            ownerTokenAccount,
            pythPriceFeed: c.pythPriceFeed,
            tokenProgram: TOKEN_PROGRAM_ID,
          })
          .preInstructions(pre)
          .rpc();
      }, c.gates);
    },
    [context, run, view, lpPositionExists]
  );

  /** Request a withdrawal of `shares` (6 dp); starts the delay and fixes the value. */
  const requestWithdrawal = useCallback(
    async (shares: bigint) => {
      const c = await context();
      return run('lpRequest', () =>
        c.program.methods
          .lpRequestWithdrawal(new BN(shares.toString()))
          .accounts({
            owner: c.publicKey,
            market: c.data.marketAddress,
            lpPool: c.data.poolAddress,
            lpPosition: c.lpPositionPda,
            pythPriceFeed: c.pythPriceFeed,
          })
          .rpc(), c.gates);
    },
    [context, run]
  );

  /** Cancel the pending request (also how a lapsed request is cleared). */
  const cancelWithdrawal = useCallback(async () => {
    const c = await context();
    return run('lpCancel', () =>
      c.program.methods
        .lpCancelWithdrawal()
        .accounts({ owner: c.publicKey, lpPool: c.data.poolAddress, lpPosition: c.lpPositionPda })
        .rpc(), c.gates);
  }, [context, run]);

  /** Claim the pending request after `unlock_at`, within the claim window. */
  const withdraw = useCallback(async () => {
    const c = await context();
    return run('lpWithdraw', async () => {
      const ownerTokenAccount = await getAssociatedTokenAddress(c.data.marketCollateralMint, c.publicKey);
      return c.program.methods
        .lpWithdraw()
        .accounts({
          owner: c.publicKey,
          market: c.data.marketAddress,
          lpPool: c.data.poolAddress,
          lpPosition: c.lpPositionPda,
          vault: getVaultPDA(c.data.marketAddress)[0],
          vaultTokenAccount: getVaultTokenAccountPDA(c.data.marketAddress)[0],
          ownerTokenAccount,
          pythPriceFeed: c.pythPriceFeed,
          tokenProgram: TOKEN_PROGRAM_ID,
        })
        .rpc();
    }, c.gates);
  }, [context, run]);

  // Derived numbers for display (6-dp integers)
  const summary = useMemo(() => {
    if (!pool || !view) return null;
    const free = lpPosition ? lpPosition.shares - lpPosition.pendingShares : 0n;
    return {
      navRedeemPerShare: navPerShare(view.navRedeem, pool.lpShares),
      navMintPerShare: navPerShare(view.navMint, pool.lpShares),
      positionValue: lpPosition ? redeemValue(lpPosition.shares, pool.lpShares, view.navRedeem) : 0n,
      freeShares: free > 0n ? free : 0n,
    };
  }, [pool, view, lpPosition]);

  return {
    market,
    pool,
    view,
    gates,
    lpPosition,
    request,
    summary,
    now,
    isLoading,
    error: error ?? poolError,
    deposit,
    requestWithdrawal,
    cancelWithdrawal,
    withdraw,
    refresh,
    connected: !!publicKey,
  };
}
