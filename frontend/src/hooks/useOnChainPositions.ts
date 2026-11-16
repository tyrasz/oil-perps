import { useCallback, useEffect, useState } from 'react';
import { useWallet } from '@solana/wallet-adapter-react';
import { PublicKey } from '@solana/web3.js';
import { usePerpsProgram } from './usePerpsProgram';
import { getUserAccountPDA } from '../utils/pda';
import { PRICE_DECIMALS, LEVERAGE_DECIMALS } from '../config/program';
import { useMarketStore } from '../stores/marketStore';
import type { Position, ClosedPosition, UserAccount } from '../types';

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

interface OnChainUserAccount {
  owner: PublicKey;
  collateralBalance: { toNumber: () => number };
  totalPositions: number;
  totalTrades: { toNumber: () => number };
  realizedPnl: { toNumber: () => number };
  bump: number;
}

export function useOnChainPositions() {
  const { publicKey } = useWallet();
  const { program } = usePerpsProgram();
  const { markets, selectedCommodity } = useMarketStore();

  const [positions, setPositions] = useState<Position[]>([]);
  const [closedPositions, setClosedPositions] = useState<ClosedPosition[]>([]);
  const [userAccount, setUserAccount] = useState<UserAccount | null>(null);
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  /**
   * Fetch user account from chain
   */
  const fetchUserAccount = useCallback(async () => {
    if (!program || !publicKey) return null;

    try {
      const [userAccountPda] = getUserAccountPDA(publicKey);
      const account = await program.account.userAccount.fetch(userAccountPda) as unknown as OnChainUserAccount;

      return {
        address: userAccountPda.toString(),
        collateralBalance: account.collateralBalance.toNumber() / Math.pow(10, PRICE_DECIMALS),
        totalPositions: account.totalPositions,
        realizedPnl: account.realizedPnl.toNumber() / Math.pow(10, PRICE_DECIMALS),
      };
    } catch (err) {
      // User account doesn't exist yet
      console.log('User account not found (not created yet)');
      return null;
    }
  }, [program, publicKey]);

  /**
   * Fetch all positions for the connected wallet using getProgramAccounts
   */
  const fetchPositions = useCallback(async () => {
    if (!program || !publicKey) {
      setPositions([]);
      return;
    }

    setIsLoading(true);
    setError(null);

    try {
      // Fetch all position accounts owned by this user
      // The owner field is at offset 8 (after discriminator)
      const accounts = await program.account.position.all([
        {
          memcmp: {
            offset: 8, // After 8-byte discriminator
            bytes: publicKey.toBase58(),
          },
        },
      ]) as unknown as OnChainPosition[];

      // Filter for open positions and transform to frontend format
      const openPositions: Position[] = accounts
        .filter((acc) => acc.account.status.open !== undefined)
        .map((acc) => {
          const isLong = acc.account.side.long !== undefined;
          const size = acc.account.size.toNumber() / Math.pow(10, PRICE_DECIMALS);
          const collateral = acc.account.collateral.toNumber() / Math.pow(10, PRICE_DECIMALS);
          const entryPrice = acc.account.entryPrice.toNumber() / Math.pow(10, PRICE_DECIMALS);
          const leverage = acc.account.leverage / Math.pow(10, LEVERAGE_DECIMALS);

          // TODO: Map market address to commodity properly
          // For now, use selected commodity's current price
          const commodity = selectedCommodity.id;
          const currentMarket = markets[commodity];
          const currentPrice = currentMarket?.price || entryPrice;

          // Calculate unrealized PnL based on current price
          let unrealizedPnl = 0;
          if (currentPrice > 0) {
            const priceDiff = isLong
              ? currentPrice - entryPrice
              : entryPrice - currentPrice;
            unrealizedPnl = size * priceDiff;
          }

          // Calculate margin ratio: (collateral + PnL) / notional * 100
          const notional = size * currentPrice;
          const equity = collateral + unrealizedPnl;
          const marginRatio = notional > 0 ? (equity / notional) * 100 : 0;

          return {
            address: acc.publicKey.toString(),
            owner: acc.account.owner.toString(),
            commodity,
            side: isLong ? 'long' : 'short',
            size,
            collateral,
            entryPrice,
            leverage,
            unrealizedPnl,
            marginRatio,
            openedAt: acc.account.openedAt.toNumber(),
          } as Position;
        });

      setPositions(openPositions);
      console.log(`Found ${openPositions.length} open positions on-chain`);
    } catch (err) {
      console.error('Failed to fetch positions from chain:', err);
      setError('Failed to fetch positions');
      setPositions([]);
    } finally {
      setIsLoading(false);
    }
  }, [program, publicKey, markets, selectedCommodity]);

  /**
   * Fetch closed/liquidated positions for trade history
   */
  const fetchClosedPositions = useCallback(async () => {
    if (!program || !publicKey) {
      setClosedPositions([]);
      return;
    }

    try {
      // Fetch all position accounts owned by this user
      const accounts = await program.account.position.all([
        {
          memcmp: {
            offset: 8,
            bytes: publicKey.toBase58(),
          },
        },
      ]) as unknown as OnChainPosition[];

      // Filter for closed or liquidated positions
      const closed: ClosedPosition[] = accounts
        .filter((acc) => acc.account.status.closed !== undefined || acc.account.status.liquidated !== undefined)
        .map((acc) => {
          const isLong = acc.account.side.long !== undefined;
          const size = acc.account.size.toNumber() / Math.pow(10, PRICE_DECIMALS);
          const collateral = acc.account.collateral.toNumber() / Math.pow(10, PRICE_DECIMALS);
          const entryPrice = acc.account.entryPrice.toNumber() / Math.pow(10, PRICE_DECIMALS);
          const leverage = acc.account.leverage / Math.pow(10, LEVERAGE_DECIMALS);
          const realizedPnl = acc.account.realizedPnl.toNumber() / Math.pow(10, PRICE_DECIMALS);

          // Calculate exit price from realized PnL
          // PnL = size * (exitPrice - entryPrice) for long
          // PnL = size * (entryPrice - exitPrice) for short
          let exitPrice = entryPrice;
          if (size > 0) {
            if (isLong) {
              exitPrice = entryPrice + (realizedPnl / size);
            } else {
              exitPrice = entryPrice - (realizedPnl / size);
            }
          }

          const isLiquidated = acc.account.status.liquidated !== undefined;

          return {
            address: acc.publicKey.toString(),
            owner: acc.account.owner.toString(),
            commodity: selectedCommodity.id,
            side: isLong ? 'long' : 'short',
            size,
            collateral,
            entryPrice,
            exitPrice,
            leverage,
            realizedPnl,
            openedAt: acc.account.openedAt.toNumber(),
            closedAt: acc.account.lastUpdatedAt.toNumber(),
            status: isLiquidated ? 'liquidated' : 'closed',
          } as ClosedPosition;
        })
        .sort((a, b) => b.closedAt - a.closedAt); // Most recent first

      setClosedPositions(closed);
      console.log(`Found ${closed.length} closed positions on-chain`);
    } catch (err) {
      console.error('Failed to fetch closed positions:', err);
      setClosedPositions([]);
    }
  }, [program, publicKey, selectedCommodity]);

  /**
   * Refresh all data
   */
  const refresh = useCallback(async () => {
    const [account] = await Promise.all([
      fetchUserAccount(),
      fetchPositions(),
      fetchClosedPositions(),
    ]);
    setUserAccount(account);
  }, [fetchUserAccount, fetchPositions, fetchClosedPositions]);

  // Fetch on mount and when wallet changes
  useEffect(() => {
    if (publicKey && program) {
      refresh();

      // Poll every 10 seconds
      const interval = setInterval(refresh, 10000);
      return () => clearInterval(interval);
    } else {
      setPositions([]);
      setUserAccount(null);
    }
  }, [publicKey, program, refresh]);

  return {
    positions,
    closedPositions,
    userAccount,
    isLoading,
    error,
    refresh,
    refreshPositions: fetchPositions,
    refreshClosedPositions: fetchClosedPositions,
    refreshUserAccount: fetchUserAccount,
  };
}
