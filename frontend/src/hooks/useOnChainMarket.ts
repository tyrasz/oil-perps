import { useCallback, useEffect, useRef, useState } from 'react';
import { usePerpsProgram } from './usePerpsProgram';
import { getCommodityMarket } from '../utils/pda';
import { useMarketStore } from '../stores/marketStore';
import { PRICE_DECIMALS } from '../config/program';

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
  // Risk controls (zero when unset, or with an IDL built before they existed)
  sessionSchedule: number;
  preCloseWindowSecs: number;
  weekendInitialMarginRatio: number;
  weekendMaintenanceMarginRatio: number;
  maxConfidenceBps: number;
  minOraclePrice: number;
  settlementPrice: number;
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
  sessionSchedule?: number;
  preCloseWindowSecs?: number;
  weekendInitialMarginRatio?: number;
  weekendMaintenanceMarginRatio?: number;
  maxConfidenceBps?: number;
  minOraclePrice?: { toNumber: () => number };
  settlementPrice?: { toNumber: () => number };
}

export function useOnChainMarket() {
  const { program } = usePerpsProgram();
  const selectedCommodity = useMarketStore((s) => s.selectedCommodity);
  const [marketData, setMarketData] = useState<OnChainMarketData | null>(null);
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Only the most recent request may update state: a slow fetch for the
  // previously selected commodity must not overwrite the new one's data.
  const latestRequest = useRef(0);

  const fetchMarket = useCallback(async () => {
    const request = ++latestRequest.current;
    if (!program) {
      setMarketData(null);
      return;
    }

    setIsLoading(true);
    setError(null);

    try {
      const marketPda = getCommodityMarket(selectedCommodity.id);
      const market = await program.account.market.fetch(marketPda) as unknown as OnChainMarket;
      if (request !== latestRequest.current) return;

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
        sessionSchedule: market.sessionSchedule ?? 0,
        preCloseWindowSecs: market.preCloseWindowSecs ?? 0,
        weekendInitialMarginRatio: market.weekendInitialMarginRatio ?? 0,
        weekendMaintenanceMarginRatio: market.weekendMaintenanceMarginRatio ?? 0,
        maxConfidenceBps: market.maxConfidenceBps ?? 0,
        minOraclePrice: (market.minOraclePrice?.toNumber() ?? 0) / Math.pow(10, PRICE_DECIMALS),
        // Kept in raw 6-decimal units; only compared against zero and displayed
        settlementPrice: market.settlementPrice?.toNumber() ?? 0,
      });
    } catch (err) {
      if (request !== latestRequest.current) return;
      console.error('Failed to fetch on-chain market:', err);
      setError('Market not found on-chain');
      setMarketData(null);
    } finally {
      if (request === latestRequest.current) setIsLoading(false);
    }
  }, [program, selectedCommodity.id]);

  useEffect(() => {
    // Don't show the previous commodity's market while the new one loads
    setMarketData(null);
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
