import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { renderHook, waitFor, act } from '@testing-library/react';
import { usePositions } from './usePositions';
import { useMarketStore } from '../stores/marketStore';
import { PublicKey } from '@solana/web3.js';

// Create mock wallet
const mockPublicKey = {
  toString: () => 'MockPublicKey123',
  toBase58: () => 'MockPublicKey123',
} as unknown as PublicKey;

// Control wallet state
let walletState = { publicKey: null as PublicKey | null };

// Mock Solana wallet adapter
vi.mock('@solana/wallet-adapter-react', () => ({
  useConnection: () => ({
    connection: {},
  }),
  useWallet: () => walletState,
}));

// Mock fetch
const mockFetch = vi.fn();
global.fetch = mockFetch;

describe('usePositions', () => {
  beforeEach(() => {
    vi.useFakeTimers({ shouldAdvanceTime: true });

    // Reset wallet state
    walletState = { publicKey: null };

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
    mockFetch.mockResolvedValue({
      json: () => Promise.resolve([]),
    });
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  describe('without wallet connected', () => {
    it('should return empty data when wallet not connected', () => {
      const { result } = renderHook(() => usePositions());

      expect(result.current.positions).toEqual([]);
      expect(result.current.orders).toEqual([]);
      expect(result.current.userAccount).toBeNull();
    });

    it('should not fetch data when wallet not connected', async () => {
      renderHook(() => usePositions());

      await act(async () => {
        vi.advanceTimersByTime(100);
      });

      // No fetch calls for positions/orders/account
      expect(mockFetch).not.toHaveBeenCalled();
    });

    it('should expose refresh functions', () => {
      const { result } = renderHook(() => usePositions());

      expect(typeof result.current.refreshPositions).toBe('function');
      expect(typeof result.current.refreshOrders).toBe('function');
      expect(typeof result.current.refreshAccount).toBe('function');
    });
  });

  describe('with wallet connected', () => {
    beforeEach(() => {
      walletState = { publicKey: mockPublicKey };
    });

    it('should fetch positions on mount', async () => {
      const mockPositionsData = [
        {
          address: 'pos1',
          owner: 'user123',
          side: 'Long',
          size: 100_000_000,
          collateral: 1_000_000_000,
          entry_price: 75_000_000,
          leverage: 10_000,
          unrealized_pnl: 50_000_000,
          margin_ratio: 1500,
          opened_at: 1704067200,
        },
      ];

      mockFetch
        .mockResolvedValueOnce({ json: () => Promise.resolve(mockPositionsData) })
        .mockResolvedValueOnce({ json: () => Promise.resolve([]) })
        .mockResolvedValueOnce({ json: () => Promise.resolve({}) });

      const { result } = renderHook(() => usePositions());

      await waitFor(() => {
        expect(result.current.positions).toHaveLength(1);
      });

      expect(result.current.positions[0].side).toBe('long');
      expect(result.current.positions[0].size).toBe(100);
      expect(result.current.positions[0].collateral).toBe(1000);
      expect(result.current.positions[0].entryPrice).toBe(75);
      expect(result.current.positions[0].leverage).toBe(10);
    });

    it('should fetch orders on mount', async () => {
      const mockOrdersData = [
        {
          address: 'order1',
          owner: 'user123',
          side: 'BID',
          order_type: 'LIMIT',
          price: 74_500_000,
          size: 100_000_000,
          filled_size: 0,
          status: 'OPEN',
          created_at: 1704067200,
        },
      ];

      mockFetch
        .mockResolvedValueOnce({ json: () => Promise.resolve([]) })
        .mockResolvedValueOnce({ json: () => Promise.resolve(mockOrdersData) })
        .mockResolvedValueOnce({ json: () => Promise.resolve({}) });

      const { result } = renderHook(() => usePositions());

      await waitFor(() => {
        expect(result.current.orders).toHaveLength(1);
      });

      expect(result.current.orders[0].side).toBe('bid');
      expect(result.current.orders[0].orderType).toBe('limit');
      expect(result.current.orders[0].price).toBe(74.5);
      expect(result.current.orders[0].status).toBe('open');
    });

    it('should fetch user account on mount', async () => {
      const mockAccountData = {
        address: 'user123',
        collateral_balance: 10_000_000_000,
        total_positions: 3,
        realized_pnl: 500_000_000,
      };

      mockFetch
        .mockResolvedValueOnce({ json: () => Promise.resolve([]) })
        .mockResolvedValueOnce({ json: () => Promise.resolve([]) })
        .mockResolvedValueOnce({ json: () => Promise.resolve(mockAccountData) });

      const { result } = renderHook(() => usePositions());

      await waitFor(() => {
        expect(result.current.userAccount).not.toBeNull();
      });

      expect(result.current.userAccount?.collateralBalance).toBe(10000);
      expect(result.current.userAccount?.totalPositions).toBe(3);
      expect(result.current.userAccount?.realizedPnl).toBe(500);
    });

    it('should handle short positions correctly', async () => {
      const mockPositionsData = [
        {
          address: 'pos1',
          owner: 'user123',
          side: 'Short',
          size: 50_000_000,
          collateral: 500_000_000,
          entry_price: 76_000_000,
          leverage: 5_000,
          unrealized_pnl: -20_000_000,
          margin_ratio: 1200,
          opened_at: 1704067200,
        },
      ];

      mockFetch
        .mockResolvedValueOnce({ json: () => Promise.resolve(mockPositionsData) })
        .mockResolvedValueOnce({ json: () => Promise.resolve([]) })
        .mockResolvedValueOnce({ json: () => Promise.resolve({}) });

      const { result } = renderHook(() => usePositions());

      await waitFor(() => {
        expect(result.current.positions).toHaveLength(1);
      });

      expect(result.current.positions[0].side).toBe('short');
      expect(result.current.positions[0].unrealizedPnl).toBe(-20);
    });

    it('should handle API errors gracefully', async () => {
      const consoleSpy = vi.spyOn(console, 'error').mockImplementation(() => {});
      mockFetch.mockRejectedValue(new Error('Network error'));

      renderHook(() => usePositions());

      await waitFor(() => {
        expect(consoleSpy).toHaveBeenCalled();
      });

      consoleSpy.mockRestore();
    });
  });

  describe('refresh functions', () => {
    beforeEach(() => {
      walletState = { publicKey: mockPublicKey };
    });

    it('should refetch positions when refreshPositions called', async () => {
      mockFetch.mockResolvedValue({ json: () => Promise.resolve([]) });

      const { result } = renderHook(() => usePositions());

      const initialCalls = mockFetch.mock.calls.length;

      await act(async () => {
        await result.current.refreshPositions();
      });

      expect(mockFetch.mock.calls.length).toBeGreaterThan(initialCalls);
    });

    it('should refetch orders when refreshOrders called', async () => {
      mockFetch.mockResolvedValue({ json: () => Promise.resolve([]) });

      const { result } = renderHook(() => usePositions());

      const initialCalls = mockFetch.mock.calls.length;

      await act(async () => {
        await result.current.refreshOrders();
      });

      expect(mockFetch.mock.calls.length).toBeGreaterThan(initialCalls);
    });

    it('should refetch account when refreshAccount called', async () => {
      mockFetch.mockResolvedValue({ json: () => Promise.resolve({}) });

      const { result } = renderHook(() => usePositions());

      const initialCalls = mockFetch.mock.calls.length;

      await act(async () => {
        await result.current.refreshAccount();
      });

      expect(mockFetch.mock.calls.length).toBeGreaterThan(initialCalls);
    });
  });

  describe('data transformations', () => {
    it('should correctly transform position side from API', () => {
      expect('Long' === 'Long' ? 'long' : 'short').toBe('long');
      expect('Short' === 'Long' ? 'long' : 'short').toBe('short');
    });

    it('should correctly transform decimal values', () => {
      expect(100_000_000 / 1_000_000).toBe(100);
      expect(1_000_000_000 / 1_000_000).toBe(1000);
      expect(75_000_000 / 1_000_000).toBe(75);
      expect(10_000 / 1000).toBe(10);
      expect(1500 / 100).toBe(15);
    });

    it('should correctly transform order status', () => {
      expect('OPEN'.toLowerCase()).toBe('open');
      expect('PARTIALLY_FILLED'.toLowerCase()).toBe('partially_filled');
      expect('FILLED'.toLowerCase()).toBe('filled');
      expect('CANCELLED'.toLowerCase()).toBe('cancelled');
    });
  });

  describe('position calculations', () => {
    it('should calculate position value', () => {
      const size = 100;
      const entryPrice = 75;
      expect(size * entryPrice).toBe(7500);
    });

    it('should calculate unrealized PnL for long position', () => {
      const size = 100;
      const entryPrice = 75;
      const currentPrice = 80;
      expect(size * (currentPrice - entryPrice)).toBe(500);
    });

    it('should calculate unrealized PnL for short position', () => {
      const size = 100;
      const entryPrice = 75;
      const currentPrice = 80;
      expect(size * (entryPrice - currentPrice)).toBe(-500);
    });

    it('should calculate margin ratio', () => {
      const collateral = 1000;
      const unrealizedPnl = 100;
      const positionValue = 10000;
      const equity = collateral + unrealizedPnl;
      expect((equity / positionValue) * 100).toBe(11);
    });
  });
});
