import { useCallback, useEffect, useState } from 'react';
import { usePerpsProgram } from './usePerpsProgram';
import { getMarketPDA } from '../utils/pda';
import { USDC_MINT, PRICE_DECIMALS } from '../config/program';

interface OnChainMarketData {
  address: string;
  fundingRate: number;
  lastFundingTime: number;
  fundingInterval: number;
  nextFundingTime: number;
  longOpenInterest: number;
  shortOpenInterest: number;
  maxOpenInterest: number;
  totalPositions: number;
  totalTrades: number;
  isPaused: boolean;
  maxLeverage: number;
  initialMarginRatio: number;
  maintenanceMarginRatio: number;
  takerFee: number;
  makerFee: number;
  liquidationFee: number;
  insuranceFund: number;
}

interface OnChainMarket {
  authority: { toString: () => string };
  collateralMint: { toString: () => string };
  vault: { toString: () => string };
  pythPriceFeed: { toString: () => string };
  commodity: number[];
  maxLeverage: number;
  maintenanceMarginRatio: number;
  initialMarginRatio: number;
  takerFee: number;
  makerFee: number;
  liquidationFee: number;
  longOpenInterest: { toNumber: () => number };
  shortOpenInterest: { toNumber: () => number };
  maxOpenInterest: { toNumber: () => number };
  fundingRate: { toNumber: () => number };
  lastFundingTime: { toNumber: () => number };
  fundingInterval: { toNumber: () => number };
  insuranceFund: { toNumber: () => number };
  totalPositions: { toNumber: () => number };
  totalTrades: { toNumber: () => number };
  bump: number;
  isPaused: boolean;
}

export function useOnChainMarket() {
  const { program } = usePerpsProgram();
  const [marketData, setMarketData] = useState<OnChainMarketData | null>(null);
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const fetchMarket = useCallback(async () => {
    if (!program) {
      setMarketData(null);
      return;
    }

    setIsLoading(true);
    setError(null);

    try {
      const [marketPda] = getMarketPDA(USDC_MINT);
      const market = await program.account.market.fetch(marketPda) as unknown as OnChainMarket;

      const lastFundingTime = market.lastFundingTime.toNumber();
      const fundingInterval = market.fundingInterval.toNumber();
      const nextFundingTime = lastFundingTime + fundingInterval;

      // Funding rate is stored with 6 decimals, convert to percentage
      // A funding rate of 100000 = 0.1% = 0.001
      const fundingRate = market.fundingRate.toNumber() / Math.pow(10, PRICE_DECIMALS);

      setMarketData({
        address: marketPda.toString(),
        fundingRate,
        lastFundingTime,
        fundingInterval,
        nextFundingTime,
        longOpenInterest: market.longOpenInterest.toNumber() / Math.pow(10, PRICE_DECIMALS),
        shortOpenInterest: market.shortOpenInterest.toNumber() / Math.pow(10, PRICE_DECIMALS),
        maxOpenInterest: market.maxOpenInterest.toNumber() / Math.pow(10, PRICE_DECIMALS),
        totalPositions: market.totalPositions.toNumber(),
        totalTrades: market.totalTrades.toNumber(),
        isPaused: market.isPaused,
        maxLeverage: market.maxLeverage,
        initialMarginRatio: market.initialMarginRatio,
        maintenanceMarginRatio: market.maintenanceMarginRatio,
        takerFee: market.takerFee,
        makerFee: market.makerFee,
        liquidationFee: market.liquidationFee,
        insuranceFund: market.insuranceFund.toNumber() / Math.pow(10, PRICE_DECIMALS),
      });
    } catch (err) {
      console.error('Failed to fetch on-chain market:', err);
      setError('Market not found on-chain');
      setMarketData(null);
    } finally {
      setIsLoading(false);
    }
  }, [program]);

  useEffect(() => {
    fetchMarket();

    // Refresh every 30 seconds
    const interval = setInterval(fetchMarket, 30000);
    return () => clearInterval(interval);
  }, [fetchMarket]);

  return {
    marketData,
    isLoading,
    error,
    refresh: fetchMarket,
  };
}
