import { useState, useEffect, useCallback } from 'react';
import { useConnection, useWallet } from '@solana/wallet-adapter-react';
import { PublicKey } from '@solana/web3.js';

// Mock program ID - replace with actual deployed program ID
const MM_REGISTRY_PROGRAM_ID = new PublicKey('MMReg11111111111111111111111111111111111111');
const USDC_MINT = new PublicKey('EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v');

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
  useConnection(); // Keep for future use
  const { publicKey, signTransaction } = useWallet();
  const [registry, setRegistry] = useState<RegistryData | null>(null);
  const [marketMaker, setMarketMaker] = useState<MarketMakerData | null>(null);
  const [quotes, setQuotes] = useState<QuoteData[]>([]);
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // PDA derivation helpers (kept for production use)
  const _getRegistryPDA = (market: PublicKey) => {
    const [registryPda] = PublicKey.findProgramAddressSync(
      [Buffer.from('mm_registry'), market.toBuffer()],
      MM_REGISTRY_PROGRAM_ID
    );
    return registryPda;
  };

  const _getMarketMakerPDA = (registry: PublicKey, owner: PublicKey) => {
    const [mmPda] = PublicKey.findProgramAddressSync(
      [Buffer.from('market_maker'), registry.toBuffer(), owner.toBuffer()],
      MM_REGISTRY_PROGRAM_ID
    );
    return mmPda;
  };

  // Suppress unused warnings - these will be used in production
  void _getRegistryPDA;
  void _getMarketMakerPDA;

  // Fetch registry data
  const fetchRegistry = useCallback(async () => {
    try {
      // For demo, use mock data
      setRegistry({
        market: 'WTI-PERP',
        collateralMint: USDC_MINT.toBase58(),
        minCollateral: 1000,
        maxSpread: 100, // 100 bps = 1%
        minQuoteSize: 1,
        maxQuoteSize: 1000,
        mmFee: 2, // 2 bps
        totalMms: 12,
        activeQuotes: 45,
        totalVolume: 15_000_000,
        totalFees: 7_500,
        isOpen: true,
        isTradingEnabled: true,
      });
    } catch (err) {
      console.error('Error fetching registry:', err);
      setError('Failed to fetch registry');
    }
  }, []);

  // Fetch market maker data
  const fetchMarketMaker = useCallback(async () => {
    if (!publicKey) {
      setMarketMaker(null);
      return;
    }

    try {
      // For demo, simulate registered MM
      // In production, fetch from chain
      const mockMm: MarketMakerData = {
        owner: publicKey.toBase58(),
        registry: 'mock-registry',
        status: MmStatus.Active,
        collateralDeposited: 50_000,
        collateralLocked: 15_000,
        collateralAvailable: 35_000,
        inventory: 25, // 25 barrels net long
        averageEntryPrice: 74.50,
        realizedPnl: 2_500,
        unrealizedPnl: 375, // 25 * ($75 - $74.50) = $12.50
        totalVolume: 250_000,
        totalFees: 125,
        activeQuotes: 3,
        registeredAt: Date.now() / 1000 - 86400 * 30, // 30 days ago
        lastActiveAt: Date.now() / 1000 - 60, // 1 minute ago
      };

      setMarketMaker(mockMm);
    } catch (err) {
      console.error('Error fetching market maker:', err);
    }
  }, [publicKey]);

  // Fetch active quotes
  const fetchQuotes = useCallback(async () => {
    if (!publicKey || !marketMaker) {
      setQuotes([]);
      return;
    }

    try {
      // Mock quotes for demo
      const mockQuotes: QuoteData[] = [
        {
          id: 'quote-1',
          marketMaker: publicKey.toBase58(),
          bidPrice: 74.50,
          bidSize: 100,
          bidRemaining: 75,
          askPrice: 75.00,
          askSize: 100,
          askRemaining: 80,
          collateralLocked: 750,
          isActive: true,
          createdAt: Date.now() / 1000 - 3600,
          updatedAt: Date.now() / 1000 - 300,
          expiresAt: Date.now() / 1000 + 3600,
        },
        {
          id: 'quote-2',
          marketMaker: publicKey.toBase58(),
          bidPrice: 74.25,
          bidSize: 50,
          bidRemaining: 50,
          askPrice: 75.25,
          askSize: 50,
          askRemaining: 50,
          collateralLocked: 375,
          isActive: true,
          createdAt: Date.now() / 1000 - 1800,
          updatedAt: Date.now() / 1000 - 1800,
          expiresAt: Date.now() / 1000 + 7200,
        },
        {
          id: 'quote-3',
          marketMaker: publicKey.toBase58(),
          bidPrice: 74.00,
          bidSize: 200,
          bidRemaining: 200,
          askPrice: 75.50,
          askSize: 200,
          askRemaining: 150,
          collateralLocked: 1500,
          isActive: true,
          createdAt: Date.now() / 1000 - 7200,
          updatedAt: Date.now() / 1000 - 600,
          expiresAt: Date.now() / 1000 + 14400,
        },
      ];

      setQuotes(mockQuotes);
    } catch (err) {
      console.error('Error fetching quotes:', err);
    }
  }, [publicKey, marketMaker]);

  // Register as market maker
  const register = useCallback(async (initialCollateral: number) => {
    if (!publicKey || !signTransaction) {
      throw new Error('Wallet not connected');
    }

    setIsLoading(true);
    setError(null);

    try {
      console.log(`Registering as MM with ${initialCollateral} USDC`);

      // Simulate transaction
      await new Promise(resolve => setTimeout(resolve, 2000));

      // Refresh data
      await fetchMarketMaker();
    } catch (err) {
      console.error('Registration failed:', err);
      setError('Registration failed');
      throw err;
    } finally {
      setIsLoading(false);
    }
  }, [publicKey, signTransaction, fetchMarketMaker]);

  // Deposit collateral
  const depositCollateral = useCallback(async (amount: number) => {
    if (!publicKey || !signTransaction) {
      throw new Error('Wallet not connected');
    }

    setIsLoading(true);
    setError(null);

    try {
      console.log(`Depositing ${amount} USDC collateral`);

      await new Promise(resolve => setTimeout(resolve, 2000));
      await fetchMarketMaker();
    } catch (err) {
      console.error('Deposit failed:', err);
      setError('Deposit failed');
      throw err;
    } finally {
      setIsLoading(false);
    }
  }, [publicKey, signTransaction, fetchMarketMaker]);

  // Withdraw collateral
  const withdrawCollateral = useCallback(async (amount: number) => {
    if (!publicKey || !signTransaction) {
      throw new Error('Wallet not connected');
    }

    setIsLoading(true);
    setError(null);

    try {
      console.log(`Withdrawing ${amount} USDC collateral`);

      await new Promise(resolve => setTimeout(resolve, 2000));
      await fetchMarketMaker();
    } catch (err) {
      console.error('Withdrawal failed:', err);
      setError('Withdrawal failed');
      throw err;
    } finally {
      setIsLoading(false);
    }
  }, [publicKey, signTransaction, fetchMarketMaker]);

  // Post a new quote
  const postQuote = useCallback(async (params: PostQuoteParams) => {
    if (!publicKey || !signTransaction) {
      throw new Error('Wallet not connected');
    }

    setIsLoading(true);
    setError(null);

    try {
      console.log('Posting quote:', params);

      // Validate spread
      const spread = ((params.askPrice - params.bidPrice) / params.bidPrice) * 10000;
      if (registry && spread > registry.maxSpread) {
        throw new Error(`Spread ${spread.toFixed(0)}bps exceeds max ${registry.maxSpread}bps`);
      }

      await new Promise(resolve => setTimeout(resolve, 2000));

      await fetchMarketMaker();
      await fetchQuotes();
    } catch (err) {
      console.error('Post quote failed:', err);
      setError(err instanceof Error ? err.message : 'Post quote failed');
      throw err;
    } finally {
      setIsLoading(false);
    }
  }, [publicKey, signTransaction, registry, fetchMarketMaker, fetchQuotes]);

  // Update an existing quote
  const updateQuote = useCallback(async (params: UpdateQuoteParams) => {
    if (!publicKey || !signTransaction) {
      throw new Error('Wallet not connected');
    }

    setIsLoading(true);
    setError(null);

    try {
      console.log('Updating quote:', params);

      await new Promise(resolve => setTimeout(resolve, 2000));

      await fetchQuotes();
    } catch (err) {
      console.error('Update quote failed:', err);
      setError('Update quote failed');
      throw err;
    } finally {
      setIsLoading(false);
    }
  }, [publicKey, signTransaction, fetchQuotes]);

  // Cancel a quote
  const cancelQuote = useCallback(async (quoteId: string) => {
    if (!publicKey || !signTransaction) {
      throw new Error('Wallet not connected');
    }

    setIsLoading(true);
    setError(null);

    try {
      console.log('Cancelling quote:', quoteId);

      await new Promise(resolve => setTimeout(resolve, 2000));

      // Remove from local state
      setQuotes(prev => prev.filter(q => q.id !== quoteId));
      await fetchMarketMaker();
    } catch (err) {
      console.error('Cancel quote failed:', err);
      setError('Cancel quote failed');
      throw err;
    } finally {
      setIsLoading(false);
    }
  }, [publicKey, signTransaction, fetchMarketMaker]);

  // Deregister as market maker
  const deregister = useCallback(async () => {
    if (!publicKey || !signTransaction) {
      throw new Error('Wallet not connected');
    }

    if (marketMaker && marketMaker.activeQuotes > 0) {
      throw new Error('Cancel all quotes before deregistering');
    }

    if (marketMaker && marketMaker.inventory !== 0) {
      throw new Error('Close all inventory before deregistering');
    }

    setIsLoading(true);
    setError(null);

    try {
      console.log('Deregistering as MM');

      await new Promise(resolve => setTimeout(resolve, 2000));

      setMarketMaker(null);
      setQuotes([]);
    } catch (err) {
      console.error('Deregistration failed:', err);
      setError('Deregistration failed');
      throw err;
    } finally {
      setIsLoading(false);
    }
  }, [publicKey, signTransaction, marketMaker]);

  // Initial fetch and polling
  useEffect(() => {
    fetchRegistry();
    fetchMarketMaker();
  }, [fetchRegistry, fetchMarketMaker]);

  useEffect(() => {
    if (marketMaker) {
      fetchQuotes();
    }
  }, [marketMaker, fetchQuotes]);

  // Poll for updates
  useEffect(() => {
    const interval = setInterval(() => {
      fetchRegistry();
      fetchMarketMaker();
      if (marketMaker) {
        fetchQuotes();
      }
    }, 15000);

    return () => clearInterval(interval);
  }, [fetchRegistry, fetchMarketMaker, fetchQuotes, marketMaker]);

  return {
    registry,
    marketMaker,
    quotes,
    isLoading,
    error,
    isRegistered: marketMaker !== null && marketMaker.status !== MmStatus.Deregistered,
    register,
    depositCollateral,
    withdrawCollateral,
    postQuote,
    updateQuote,
    cancelQuote,
    deregister,
    refresh: useCallback(() => {
      fetchRegistry();
      fetchMarketMaker();
      fetchQuotes();
    }, [fetchRegistry, fetchMarketMaker, fetchQuotes]),
  };
}
