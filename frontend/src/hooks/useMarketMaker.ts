import { useCallback } from 'react';

/**
 * Market making is not live.
 *
 * The mm-registry program can hold MM collateral and quotes, but quote fills
 * are disabled on-chain (FillsDisabled) until they settle into perps-core
 * positions. Until then this hook talks to no chain, shows no data, and every
 * action rejects. (It used to serve hardcoded demo numbers and simulate
 * successful transactions, which looked real.)
 */
export const MM_LIVE = false;
export const MM_NOT_LIVE_MESSAGE =
  'Market making is not live yet: quote fills do not settle into positions.';

export const MmStatus = {
  Inactive: 0,
  Active: 1,
  Suspended: 2,
  Deregistered: 3,
} as const;

export type MmStatus = typeof MmStatus[keyof typeof MmStatus];

export interface MarketMakerData {
  owner: string;
  registry: string;
  status: MmStatus;
  collateralDeposited: number;
  collateralLocked: number;
  collateralAvailable: number;
  inventory: number;
  averageEntryPrice: number;
  realizedPnl: number;
  unrealizedPnl: number;
  totalVolume: number;
  totalFees: number;
  activeQuotes: number;
  registeredAt: number;
  lastActiveAt: number;
}

export interface QuoteData {
  id: string;
  marketMaker: string;
  bidPrice: number;
  bidSize: number;
  bidRemaining: number;
  askPrice: number;
  askSize: number;
  askRemaining: number;
  collateralLocked: number;
  isActive: boolean;
  createdAt: number;
  updatedAt: number;
  expiresAt: number;
}

export interface RegistryData {
  market: string;
  collateralMint: string;
  minCollateral: number;
  maxSpread: number;
  minQuoteSize: number;
  maxQuoteSize: number;
  mmFee: number;
  totalMms: number;
  activeQuotes: number;
  totalVolume: number;
  totalFees: number;
  isOpen: boolean;
  isTradingEnabled: boolean;
}

export interface PostQuoteParams {
  bidPrice: number;
  bidSize: number;
  askPrice: number;
  askSize: number;
  expiresInSeconds: number;
}

export interface UpdateQuoteParams {
  quoteId: string;
  bidPrice?: number;
  bidSize?: number;
  askPrice?: number;
  askSize?: number;
}

export function useMarketMaker() {
  const notLive = useCallback(async (): Promise<never> => {
    throw new Error(MM_NOT_LIVE_MESSAGE);
  }, []);

  return {
    isLive: MM_LIVE,
    registry: null as RegistryData | null,
    marketMaker: null as MarketMakerData | null,
    quotes: [] as QuoteData[],
    isLoading: false,
    error: null as string | null,
    isRegistered: false,
    register: notLive as (initialCollateral: number) => Promise<void>,
    depositCollateral: notLive as (amount: number) => Promise<void>,
    withdrawCollateral: notLive as (amount: number) => Promise<void>,
    postQuote: notLive as (params: PostQuoteParams) => Promise<void>,
    updateQuote: notLive as (params: UpdateQuoteParams) => Promise<void>,
    cancelQuote: notLive as (quoteId: string) => Promise<void>,
    deregister: notLive as () => Promise<void>,
    refresh: useCallback(() => {}, []),
  };
}
