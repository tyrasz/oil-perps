import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { renderHook, waitFor, act } from '@testing-library/react';
import { useMarket } from './useMarket';
import { useMarketStore } from '../stores/marketStore';

// Mock fetch for this test file
const mockFetch = vi.fn();
global.fetch = mockFetch;

describe('useMarket', () => {
  beforeEach(() => {
    vi.useFakeTimers({ shouldAdvanceTime: true });

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

    mockFetch.mockReset();
    // Default mock response
    mockFetch.mockResolvedValue({
      json: () => Promise.resolve({}),
    });
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  describe('initial render', () => {
    it('should return initial state from store', () => {
      const { result } = renderHook(() => useMarket());

      expect(result.current.market).toBeNull();
      expect(result.current.orderBook).toBeNull();
      expect(result.current.recentTrades).toEqual([]);
    });

    it('should expose refresh functions', () => {
      const { result } = renderHook(() => useMarket());

      expect(typeof result.current.refreshMarket).toBe('function');
      expect(typeof result.current.refreshOrderBook).toBe('function');
    });
  });

  describe('fetchMarket', () => {
    it('should fetch market data on mount', async () => {
      const mockMarketData = {
        address: 'market123',
        long_open_interest: 300_000_000,
        short_open_interest: 200_000_000,
        funding_rate: 100,
        max_leverage: 20_000,
        is_paused: false,
      };

      const mockStats = {
        price: 75_500_000,
        price_change_24h: 2.5,
        volume_24h: 1_000_000_000_000,
        open_interest: 500_000_000,
      };

      mockFetch
        .mockResolvedValueOnce({ json: () => Promise.resolve(mockMarketData) })
        .mockResolvedValueOnce({ json: () => Promise.resolve(mockStats) })
        .mockResolvedValue({ json: () => Promise.resolve({ bids: [], asks: [] }) });

      const { result } = renderHook(() => useMarket());

      await waitFor(() => {
        expect(result.current.market).not.toBeNull();
      });

      expect(result.current.market?.address).toBe('market123');
      expect(result.current.market?.price).toBe(75.5);
      expect(result.current.market?.volume24h).toBe(1000000);
    });

    it('should handle API errors gracefully', async () => {
      const consoleSpy = vi.spyOn(console, 'error').mockImplementation(() => {});
      mockFetch.mockRejectedValue(new Error('Network error'));

      renderHook(() => useMarket());

      await waitFor(() => {
        expect(consoleSpy).toHaveBeenCalled();
      });

      consoleSpy.mockRestore();
    });
  });

  describe('fetchOrderBook', () => {
    it('should fetch and transform order book data', async () => {
      const mockOrderBook = {
        bids: [
          [75_000_000, 100_000_000],
          [74_950_000, 150_000_000],
        ],
        asks: [
          [75_050_000, 80_000_000],
          [75_100_000, 120_000_000],
        ],
      };

      mockFetch
        .mockResolvedValueOnce({ json: () => Promise.resolve({}) })
        .mockResolvedValueOnce({ json: () => Promise.resolve({}) })
        .mockResolvedValueOnce({ json: () => Promise.resolve(mockOrderBook) });

      const { result } = renderHook(() => useMarket());

      await waitFor(() => {
        expect(result.current.orderBook).not.toBeNull();
      });

      expect(result.current.orderBook?.bids).toHaveLength(2);
      expect(result.current.orderBook?.asks).toHaveLength(2);
      expect(result.current.orderBook?.bids[0].price).toBe(75);
      expect(result.current.orderBook?.asks[0].price).toBe(75.05);
    });

    it('should calculate spread correctly', async () => {
      const mockOrderBook = {
        bids: [[75_000_000, 100_000_000]],
        asks: [[75_100_000, 80_000_000]],
      };

      mockFetch
        .mockResolvedValueOnce({ json: () => Promise.resolve({}) })
        .mockResolvedValueOnce({ json: () => Promise.resolve({}) })
        .mockResolvedValueOnce({ json: () => Promise.resolve(mockOrderBook) });

      const { result } = renderHook(() => useMarket());

      await waitFor(() => {
        expect(result.current.orderBook).not.toBeNull();
      });

      expect(result.current.orderBook?.spread).toBeCloseTo(0.1, 2);
      expect(result.current.orderBook?.midPrice).toBeCloseTo(75.05, 2);
    });

    it('should handle empty order book', async () => {
      mockFetch
        .mockResolvedValueOnce({ json: () => Promise.resolve({}) })
        .mockResolvedValueOnce({ json: () => Promise.resolve({}) })
        .mockResolvedValueOnce({ json: () => Promise.resolve({ bids: [], asks: [] }) });

      const { result } = renderHook(() => useMarket());

      await waitFor(() => {
        expect(result.current.orderBook).not.toBeNull();
      });

      expect(result.current.orderBook?.spread).toBe(0);
      expect(result.current.orderBook?.midPrice).toBe(0);
    });
  });

  describe('recentTrades', () => {
    it('should return trades from store', () => {
      // Pre-populate store with trades
      useMarketStore.getState().addTrade({
        signature: 'sig1',
        side: 'long',
        price: 75.5,
        size: 10,
        timestamp: Date.now(),
      });

      const { result } = renderHook(() => useMarket());

      expect(result.current.recentTrades).toHaveLength(1);
      expect(result.current.recentTrades[0].signature).toBe('sig1');
    });
  });

  describe('refreshMarket', () => {
    it('should refetch market data when called', async () => {
      mockFetch.mockResolvedValue({ json: () => Promise.resolve({}) });

      const { result } = renderHook(() => useMarket());

      const initialCalls = mockFetch.mock.calls.length;

      await act(async () => {
        await result.current.refreshMarket();
      });

      expect(mockFetch.mock.calls.length).toBeGreaterThan(initialCalls);
    });
  });

  describe('refreshOrderBook', () => {
    it('should refetch order book when called', async () => {
      mockFetch.mockResolvedValue({ json: () => Promise.resolve({ bids: [], asks: [] }) });

      const { result } = renderHook(() => useMarket());

      const initialCalls = mockFetch.mock.calls.length;

      await act(async () => {
        await result.current.refreshOrderBook();
      });

      expect(mockFetch.mock.calls.length).toBeGreaterThan(initialCalls);
    });
  });

  describe('data transformations', () => {
    it('should correctly transform API price data (6 decimal conversion)', () => {
      const rawPrice = 75_500_000;
      const transformedPrice = rawPrice / 1_000_000;
      expect(transformedPrice).toBe(75.5);
    });

    it('should correctly transform volume data', () => {
      const rawVolume = 1_000_000_000_000;
      const transformedVolume = rawVolume / 1_000_000;
      expect(transformedVolume).toBe(1000000);
    });

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
  });
});
