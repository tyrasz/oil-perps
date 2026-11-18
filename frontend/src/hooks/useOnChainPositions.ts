import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useWallet } from '@solana/wallet-adapter-react';
import { PublicKey } from '@solana/web3.js';
import { usePerpsProgram } from './usePerpsProgram';
import { commodityForMarket, getCommodityMarket, getUserAccountPDA } from '../utils/pda';
import { accruedFunding, positionHealth, positionMarketState, type PositionMarketParams } from '../utils/positionHealth';
import { PRICE_DECIMALS, LEVERAGE_DECIMALS } from '../config/program';
import { useMarketStore } from '../stores/marketStore';
import type { MarketData, Position, ClosedPosition, UserAccount } from '../types';

interface OnChainPosition {
  publicKey: PublicKey;
  account: {
    owner: PublicKey;
    market: PublicKey;
    side: { long?: object; short?: object };
    size: { toNumber: () => number };
    collateral: { toNumber: () => number };
    entryPrice: { toNumber: () => number };
    leverage: number;
    realizedPnl: { toNumber: () => number };
    lastFundingPayment: { toNumber: () => number };
    openedAt: { toNumber: () => number };
    lastUpdatedAt: { toNumber: () => number };
    status: { open?: object; closed?: object; liquidated?: object };
    executionSource: { orderBook?: object; amm?: object };
    bump: number;
  };
}

interface OnChainMarketRisk {
  cumulativeFunding?: { toNumber: () => number };
  maintenanceMarginRatio: number;
  weekendMaintenanceMarginRatio?: number;
  sessionSchedule?: number;
  preCloseWindowSecs?: number;
  settlementPrice?: { toNumber: () => number };
}

interface OnChainUserAccount {
  owner: PublicKey;
  collateralBalance: { toNumber: () => number };
  totalPositions: number;
  totalTrades: { toNumber: () => number };
  realizedPnl: { toNumber: () => number };
  bump: number;
}

/** An open position as read from chain, before pricing. */
export interface RawOpenPosition {
  address: string;
  owner: string;
  commodity: string;
  isLong: boolean;
  size: number;
  collateral: number;
  entryPrice: number;
  leverage: number;
  openedAt: number;
  /** Funding owed to (+) or by (-) the position as of the fetch, USDC. */
  funding: number;
  market: PositionMarketParams | null;
}

const UNKNOWN_COMMODITY = 'UNKNOWN';
const toUnits = (n: number) => n / Math.pow(10, PRICE_DECIMALS);

/**
 * Price an open position: valued at its market's settlement price when
 * settled, otherwise at the latest price for its commodity (entry price if
 * none is known). Pure, so price updates never require refetching from chain.
 */
export function pricePosition(raw: RawOpenPosition, markets: Record<string, MarketData>, now: number): Position {
  const state = positionMarketState(raw.market, now);
  const price = state.settlementPrice ?? (markets[raw.commodity]?.price || raw.entryPrice);
  const { unrealizedPnl, marginRatio } = positionHealth(
    { isLong: raw.isLong, size: raw.size, collateral: raw.collateral, entryPrice: raw.entryPrice },
    price,
    raw.funding
  );
  return {
    address: raw.address,
    owner: raw.owner,
    commodity: raw.commodity,
    side: raw.isLong ? 'long' : 'short',
    size: raw.size,
    collateral: raw.collateral,
    entryPrice: raw.entryPrice,
    leverage: raw.leverage,
    unrealizedPnl,
    marginRatio,
    openedAt: raw.openedAt,
    markPrice: price,
    liquidationThreshold: state.liquidationThreshold,
    warningThreshold: state.warningThreshold,
    settled: state.settlementPrice !== null,
    closeBlockedReason: state.closeBlockedReason,
  };
}

interface Snapshot {
  open: RawOpenPosition[];
  closed: ClosedPosition[];
  userAccount: UserAccount | null;
}

export function useOnChainPositions() {
  const { publicKey } = useWallet();
  const { program } = usePerpsProgram();
  const markets = useMarketStore((s) => s.markets);
  const selectedCommodity = useMarketStore((s) => s.selectedCommodity);

  const [rawOpen, setRawOpen] = useState<RawOpenPosition[]>([]);
  const [closedPositions, setClosedPositions] = useState<ClosedPosition[]>([]);
  const [userAccount, setUserAccount] = useState<UserAccount | null>(null);
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [now, setNow] = useState(() => Math.floor(Date.now() / 1000));

  // Session and weekend-margin state depend on the clock
  useEffect(() => {
    const id = setInterval(() => setNow(Math.floor(Date.now() / 1000)), 30_000);
    return () => clearInterval(id);
  }, []);

  /** Read positions, trade history and the selected market's user account. */
  const load = useCallback(async (): Promise<Snapshot | null> => {
    if (!program || !publicKey) return null;
    const ownerFilter = [{ memcmp: { offset: 8, bytes: publicKey.toBase58() } }];
    const accounts = (await program.account.position.all(ownerFilter)) as unknown as OnChainPosition[];

    const open = accounts.filter((acc) => acc.account.status.open !== undefined);
    const marketKeys = [...new Set(open.map((acc) => acc.account.market.toBase58()))];
    const marketAccounts = marketKeys.length
      ? ((await program.account.market.fetchMultiple(marketKeys)) as unknown as (OnChainMarketRisk | null)[])
      : [];
    const marketInfo = new Map(
      marketKeys.map((key, i) => {
        const m = marketAccounts[i];
        return [key, {
          cumulativeFunding: toUnits(m?.cumulativeFunding?.toNumber() ?? 0),
          params: m ? {
            maintenanceMarginRatio: m.maintenanceMarginRatio,
            weekendMaintenanceMarginRatio: m.weekendMaintenanceMarginRatio ?? 0,
            sessionSchedule: m.sessionSchedule ?? 0,
            preCloseWindowSecs: m.preCloseWindowSecs ?? 0,
            settlementPrice: m.settlementPrice?.toNumber() ?? 0,
          } : null,
        }];
      })
    );

    const rawOpenPositions: RawOpenPosition[] = open.map((acc) => {
      const isLong = acc.account.side.long !== undefined;
      const size = toUnits(acc.account.size.toNumber());
      const info = marketInfo.get(acc.account.market.toBase58());
      return {
        address: acc.publicKey.toString(),
        owner: acc.account.owner.toString(),
        commodity: commodityForMarket(acc.account.market) ?? UNKNOWN_COMMODITY,
        isLong,
        size,
        collateral: toUnits(acc.account.collateral.toNumber()),
        entryPrice: toUnits(acc.account.entryPrice.toNumber()),
        leverage: acc.account.leverage / Math.pow(10, LEVERAGE_DECIMALS),
        openedAt: acc.account.openedAt.toNumber(),
        funding: accruedFunding(isLong, size, info?.cumulativeFunding ?? 0, toUnits(acc.account.lastFundingPayment.toNumber())),
        market: info?.params ?? null,
      };
    });

    const closed: ClosedPosition[] = accounts
      .filter((acc) => acc.account.status.closed !== undefined || acc.account.status.liquidated !== undefined)
      .map((acc) => {
        const isLong = acc.account.side.long !== undefined;
        const size = toUnits(acc.account.size.toNumber());
        const entryPrice = toUnits(acc.account.entryPrice.toNumber());
        const realizedPnl = toUnits(acc.account.realizedPnl.toNumber());
        // PnL = size * (exit - entry) for longs, size * (entry - exit) for shorts
        const exitPrice = size > 0 ? entryPrice + (isLong ? 1 : -1) * (realizedPnl / size) : entryPrice;
        return {
          address: acc.publicKey.toString(),
          owner: acc.account.owner.toString(),
          commodity: commodityForMarket(acc.account.market) ?? UNKNOWN_COMMODITY,
          side: isLong ? 'long' : 'short',
          size,
          collateral: toUnits(acc.account.collateral.toNumber()),
          entryPrice,
          exitPrice,
          leverage: acc.account.leverage / Math.pow(10, LEVERAGE_DECIMALS),
          realizedPnl,
          openedAt: acc.account.openedAt.toNumber(),
          closedAt: acc.account.lastUpdatedAt.toNumber(),
          status: acc.account.status.liquidated !== undefined ? 'liquidated' : 'closed',
        } as ClosedPosition;
      })
      .sort((a, b) => b.closedAt - a.closedAt);

    // Free collateral lives in the selected market's user account
    let account: UserAccount | null = null;
    const [userAccountPda] = getUserAccountPDA(publicKey, getCommodityMarket(selectedCommodity.id));
    const data = (await program.account.userAccount.fetchNullable(userAccountPda)) as unknown as OnChainUserAccount | null;
    if (data) {
      account = {
        address: userAccountPda.toString(),
        collateralBalance: toUnits(data.collateralBalance.toNumber()),
        totalPositions: data.totalPositions,
        realizedPnl: toUnits(data.realizedPnl.toNumber()),
      };
    }

    return { open: rawOpenPositions, closed, userAccount: account };
  }, [program, publicKey, selectedCommodity.id]);

  // Only the newest refresh may update state. A slow response started for a
  // previous wallet or commodity, or overtaken by a later refresh, is dropped.
  const latestRefresh = useRef(0);

  const refresh = useCallback(async () => {
    const request = ++latestRefresh.current;
    setIsLoading(true);
    try {
      const snapshot = await load();
      if (request !== latestRefresh.current || !snapshot) return;
      setRawOpen(snapshot.open);
      setClosedPositions(snapshot.closed);
      setUserAccount(snapshot.userAccount);
      setError(null);
    } catch (err) {
      if (request !== latestRefresh.current) return;
      // Keep the last good data: clearing it would make open positions look
      // closed (and the TP/SL monitor cancels orders for missing positions).
      console.error('Failed to fetch positions from chain:', err);
      setError('Failed to fetch positions');
    } finally {
      if (request === latestRefresh.current) setIsLoading(false);
    }
  }, [load]);

  // Reload on wallet, program or selected-market changes, then poll. Price
  // updates don't trigger a reload: positions are re-priced in memory below.
  useEffect(() => {
    latestRefresh.current++; // invalidate anything in flight for the old inputs
    setUserAccount(null);
    if (!publicKey || !program) {
      setRawOpen([]);
      setClosedPositions([]);
      return;
    }
    refresh();
    const interval = setInterval(refresh, 10_000);
    return () => clearInterval(interval);
  }, [publicKey, program, refresh]);

  const positions = useMemo(() => rawOpen.map((raw) => pricePosition(raw, markets, now)), [rawOpen, markets, now]);

  return {
    positions,
    closedPositions,
    userAccount,
    isLoading,
    error,
    refresh,
    refreshPositions: refresh,
    refreshClosedPositions: refresh,
    refreshUserAccount: refresh,
  };
}
