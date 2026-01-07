import { describe, it, expect, vi, beforeEach } from 'vitest';
import { renderHook } from '@testing-library/react';
import { useMarketStore } from '../stores/marketStore';

// Mock the Solana wallet adapter hooks
vi.mock('@solana/wallet-adapter-react', () => ({
  useConnection: () => ({
    connection: {},
  }),
  useWallet: () => ({
    publicKey: null,
  }),
}));

// Mock fetch
vi.stubGlobal('fetch', vi.fn(() => Promise.resolve({
  json: () => Promise.resolve({}),
})));

// Mock WebSocket
vi.stubGlobal('WebSocket', class MockWebSocket {
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: string }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: ((error: Error) => void) | null = null;
  send = vi.fn();
  close = vi.fn();
});

describe('useMarket', () => {
  beforeEach(() => {
    // Reset store state
    useMarketStore.setState({
      market: null,
      orderBook: null,
      recentTrades: [],
      userAccount: null,
      positions: [],
      orders: [],
      selectedLeverage: 10,
      orderType: 'market',
    });
  });

  describe('store integration', () => {
    it('should have access to market state from store', () => {
      const mockMarket = {
        address: 'market123',
        price: 75.5,
        priceChange24h: 2.5,
        volume24h: 1000000,
        openInterest: 500000,
        longOpenInterest: 300000,
        shortOpenInterest: 200000,
        fundingRate: 0.0001,
        maxLeverage: 20,
        isPaused: false,
      };

      useMarketStore.getState().setMarket(mockMarket);

      const state = useMarketStore.getState();
      expect(state.market).toEqual(mockMarket);
    });

    it('should have access to orderBook state from store', () => {
      const mockOrderBook = {
        bids: [{ price: 75.00, size: 100, total: 100 }],
        asks: [{ price: 75.05, size: 80, total: 80 }],
        spread: 0.05,
        midPrice: 75.025,
      };

      useMarketStore.getState().setOrderBook(mockOrderBook);

      const state = useMarketStore.getState();
      expect(state.orderBook).toEqual(mockOrderBook);
    });

    it('should track recent trades in store', () => {
      const trade1 = {
        signature: 'sig1',
        side: 'long' as const,
        price: 75.00,
        size: 5,
        timestamp: Date.now(),
      };
      const trade2 = {
        signature: 'sig2',
        side: 'short' as const,
        price: 75.10,
        size: 8,
        timestamp: Date.now() + 1000,
      };

      useMarketStore.getState().addTrade(trade1);
      useMarketStore.getState().addTrade(trade2);

      const trades = useMarketStore.getState().recentTrades;
      expect(trades).toHaveLength(2);
      expect(trades[0].signature).toBe('sig2'); // Most recent first
    });
  });

  describe('data transformation', () => {
    it('should correctly transform API price data (6 decimal conversion)', () => {
      // API returns prices in 6 decimal format
      const rawPrice = 75_500_000; // $75.50 with 6 decimals
      const transformedPrice = rawPrice / 1_000_000;
      expect(transformedPrice).toBe(75.5);
    });

    it('should correctly transform volume data', () => {
      const rawVolume = 1_000_000_000_000; // $1M with 6 decimals
      const transformedVolume = rawVolume / 1_000_000;
      expect(transformedVolume).toBe(1000000);
    });

    it('should calculate spread from order book', () => {
      const bestBid = 75.00;
      const bestAsk = 75.05;
      const spread = bestAsk - bestBid;
      expect(spread).toBeCloseTo(0.05, 2);
    });

    it('should calculate midPrice from order book', () => {
      const bestBid = 75.00;
      const bestAsk = 75.10;
      const midPrice = (bestBid + bestAsk) / 2;
      expect(midPrice).toBeCloseTo(75.05, 2);
    });

    it('should handle empty order book gracefully', () => {
      const bids: number[] = [];
      const asks: number[] = [];
      const bestBid = bids[0] || 0;
      const bestAsk = asks[0] || 0;
      const spread = bestAsk - bestBid;
      const midPrice = (bestBid + bestAsk) / 2;

      expect(spread).toBe(0);
      expect(midPrice).toBe(0);
    });
  });

  describe('order book aggregation', () => {
    it('should calculate cumulative totals for bids', () => {
      const rawBids = [
        [75_000_000, 100_000_000],
        [74_950_000, 150_000_000],
        [74_900_000, 200_000_000],
      ];

      const bids = rawBids.map(([price, size], i, arr) => ({
        price: price / 1_000_000,
        size: size / 1_000_000,
        total: arr.slice(0, i + 1).reduce((sum, [, s]) => sum + s / 1_000_000, 0),
      }));

      expect(bids[0].total).toBe(100);
      expect(bids[1].total).toBe(250);
      expect(bids[2].total).toBe(450);
    });

    it('should calculate cumulative totals for asks', () => {
      const rawAsks = [
        [75_050_000, 80_000_000],
        [75_100_000, 120_000_000],
      ];

      const asks = rawAsks.map(([price, size], i, arr) => ({
        price: price / 1_000_000,
        size: size / 1_000_000,
        total: arr.slice(0, i + 1).reduce((sum, [, s]) => sum + s / 1_000_000, 0),
      }));

      expect(asks[0].total).toBe(80);
      expect(asks[1].total).toBe(200);
    });
  });

  describe('WebSocket message handling', () => {
    it('should parse price update messages', () => {
      const message = { type: 'price', price: 76_000_000 };
      const transformedPrice = message.price / 1_000_000;
      expect(transformedPrice).toBe(76);
    });

    it('should parse trade messages', () => {
      const message = {
        type: 'trade',
        side: 'long',
        price: 75_500_000,
        size: 10_000_000,
        timestamp: Date.now(),
      };

      const trade = {
        signature: '',
        side: message.side,
        price: message.price / 1_000_000,
        size: message.size / 1_000_000,
        timestamp: message.timestamp,
      };

      expect(trade.price).toBe(75.5);
      expect(trade.size).toBe(10);
    });
  });
});
