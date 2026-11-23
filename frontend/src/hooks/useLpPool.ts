import { useCallback, useEffect, useRef, useState } from 'react';
import type { PublicKey } from '@solana/web3.js';
import { usePerpsProgram } from './usePerpsProgram';
import { getCommodityMarket, getLpPoolPDA } from '../utils/pda';
import { decodeLpPool, decodeMarket, type LpPoolAccount, type MarketState } from '../utils/pool';

export interface LpPoolData {
  marketAddress: PublicKey;
  poolAddress: PublicKey;
  /** The market's collateral mint (USDC). */
  marketCollateralMint: PublicKey;
  market: MarketState;
  /** null when the market has no pool (not initialised, or a pre-pool deployment). */
  pool: LpPoolAccount | null;
}

/**
 * A commodity's market and LP pool accounts, decoded to integers, refreshed
 * every `refreshMs`. The pool is the house counterparty of every trade.
 */
export function useLpPool(commodityId: string, refreshMs = 15_000) {
  const { program } = usePerpsProgram();
  // Tagged with its commodity: another commodity's data reads as not loaded
  const [state, setData] = useState<(LpPoolData & { commodityId: string }) | null>(null);
  const data = program && state?.commodityId === commodityId ? state : null;
  const [error, setError] = useState<string | null>(null);
  const latest = useRef(0);

  const refresh = useCallback(async () => {
    const request = ++latest.current;
    if (!program) return;
    try {
      const marketAddress = getCommodityMarket(commodityId);
      const [poolAddress] = getLpPoolPDA(marketAddress);
      const [rawMarket, rawPool] = await Promise.all([
        program.account.market.fetch(marketAddress),
        program.account.lpPool.fetchNullable(poolAddress),
      ]);
      if (request !== latest.current) return;
      setData({
        commodityId,
        marketAddress,
        poolAddress,
        marketCollateralMint: (rawMarket as { collateralMint: PublicKey }).collateralMint,
        market: decodeMarket(rawMarket as Record<string, unknown>),
        pool: rawPool ? decodeLpPool(rawPool as Record<string, unknown>) : null,
      });
      setError(null);
    } catch (err) {
      if (request !== latest.current) return;
      console.error('Failed to fetch LP pool:', err);
      setError('Market or LP pool not found on-chain');
      setData(null);
    }
  }, [program, commodityId]);

  useEffect(() => {
    const first = setTimeout(refresh, 0);
    const id = setInterval(refresh, refreshMs);
    return () => {
      clearTimeout(first);
      clearInterval(id);
    };
  }, [refresh, refreshMs]);

  return { data, error, refresh };
}
