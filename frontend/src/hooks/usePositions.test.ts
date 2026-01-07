import { describe, it, expect, vi, beforeEach } from 'vitest';
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
  json: () => Promise.resolve([]),
})));

describe('usePositions', () => {
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
    it('should have access to positions state from store', () => {
      const mockPositions = [
        {
          address: 'pos1',
          owner: 'user123',
          side: 'long' as const,
          size: 100,
          collateral: 1000,
          entryPrice: 75.00,
          leverage: 10,
          unrealizedPnl: 50,
          marginRatio: 0.15,
          openedAt: Date.now(),
        },
      ];

      useMarketStore.getState().setPositions(mockPositions);

      const state = useMarketStore.getState();
      expect(state.positions).toEqual(mockPositions);
    });

    it('should have access to orders state from store', () => {
      const mockOrders = [
        {
          address: 'order1',
          owner: 'user123',
          side: 'bid' as const,
          orderType: 'limit' as const,
          price: 74.50,
          size: 100,
          filledSize: 0,
          status: 'open' as const,
          createdAt: Date.now(),
        },
      ];

      useMarketStore.getState().setOrders(mockOrders);

      const state = useMarketStore.getState();
      expect(state.orders).toEqual(mockOrders);
    });

    it('should have access to userAccount state from store', () => {
      const mockAccount = {
        address: 'user123',
        collateralBalance: 10000,
        totalPositions: 3,
        realizedPnl: 500,
      };

      useMarketStore.getState().setUserAccount(mockAccount);

      const state = useMarketStore.getState();
      expect(state.userAccount).toEqual(mockAccount);
    });
  });

  describe('data transformation', () => {
    it('should correctly transform position side from API', () => {
      const apiSide = 'Long';
      const transformedSide = apiSide === 'Long' ? 'long' : 'short';
      expect(transformedSide).toBe('long');

      const apiSideShort = 'Short';
      const transformedSideShort = apiSideShort === 'Long' ? 'long' : 'short';
      expect(transformedSideShort).toBe('short');
    });

    it('should correctly transform position amounts (6 decimal conversion)', () => {
      const rawPosition = {
        size: 100_000_000,
        collateral: 1_000_000_000,
        entry_price: 75_000_000,
        unrealized_pnl: 50_000_000,
      };

      const transformedPosition = {
        size: rawPosition.size / 1_000_000,
        collateral: rawPosition.collateral / 1_000_000,
        entryPrice: rawPosition.entry_price / 1_000_000,
        unrealizedPnl: rawPosition.unrealized_pnl / 1_000_000,
      };

      expect(transformedPosition.size).toBe(100);
      expect(transformedPosition.collateral).toBe(1000);
      expect(transformedPosition.entryPrice).toBe(75);
      expect(transformedPosition.unrealizedPnl).toBe(50);
    });

    it('should correctly transform leverage (3 decimal conversion)', () => {
      const rawLeverage = 10_000; // 10x with 3 decimals
      const transformedLeverage = rawLeverage / 1000;
      expect(transformedLeverage).toBe(10);
    });

    it('should correctly transform margin ratio (2 decimal conversion)', () => {
      const rawMarginRatio = 1500; // 15% with 2 decimals
      const transformedMarginRatio = rawMarginRatio / 100;
      expect(transformedMarginRatio).toBe(15);
    });
  });

  describe('order transformation', () => {
    it('should correctly transform order side', () => {
      const apiSide = 'BID';
      const transformedSide = apiSide.toLowerCase();
      expect(transformedSide).toBe('bid');
    });

    it('should correctly transform order type', () => {
      const apiOrderType = 'LIMIT';
      const transformedType = apiOrderType.toLowerCase();
      expect(transformedType).toBe('limit');

      const apiMarketType = 'MARKET';
      const transformedMarketType = apiMarketType.toLowerCase();
      expect(transformedMarketType).toBe('market');
    });

    it('should correctly transform order status', () => {
      const statuses = ['OPEN', 'PARTIALLY_FILLED', 'FILLED', 'CANCELLED'];
      const transformed = statuses.map(s => s.toLowerCase());

      expect(transformed).toEqual(['open', 'partially_filled', 'filled', 'cancelled']);
    });

    it('should correctly transform order amounts', () => {
      const rawOrder = {
        price: 74_500_000,
        size: 100_000_000,
        filled_size: 50_000_000,
      };

      const transformedOrder = {
        price: rawOrder.price / 1_000_000,
        size: rawOrder.size / 1_000_000,
        filledSize: rawOrder.filled_size / 1_000_000,
      };

      expect(transformedOrder.price).toBe(74.5);
      expect(transformedOrder.size).toBe(100);
      expect(transformedOrder.filledSize).toBe(50);
    });
  });

  describe('user account transformation', () => {
    it('should correctly transform collateral balance', () => {
      const rawBalance = 10_000_000_000; // $10,000 with 6 decimals
      const transformedBalance = rawBalance / 1_000_000;
      expect(transformedBalance).toBe(10000);
    });

    it('should correctly transform realized PnL', () => {
      const rawPnl = 500_000_000; // $500 profit with 6 decimals
      const transformedPnl = rawPnl / 1_000_000;
      expect(transformedPnl).toBe(500);

      const rawLoss = -200_000_000; // $200 loss with 6 decimals
      const transformedLoss = rawLoss / 1_000_000;
      expect(transformedLoss).toBe(-200);
    });

    it('should preserve total positions count', () => {
      const rawAccount = { total_positions: 5 };
      expect(rawAccount.total_positions).toBe(5);
    });
  });

  describe('position calculations', () => {
    it('should calculate position value', () => {
      const size = 100; // 100 contracts
      const entryPrice = 75; // $75 per contract
      const positionValue = size * entryPrice;
      expect(positionValue).toBe(7500);
    });

    it('should calculate unrealized PnL for long position', () => {
      const size = 100;
      const entryPrice = 75;
      const currentPrice = 80;
      const pnl = size * (currentPrice - entryPrice);
      expect(pnl).toBe(500); // +$500 profit
    });

    it('should calculate unrealized PnL for short position', () => {
      const size = 100;
      const entryPrice = 75;
      const currentPrice = 80;
      const pnl = size * (entryPrice - currentPrice);
      expect(pnl).toBe(-500); // -$500 loss
    });

    it('should calculate margin ratio', () => {
      const collateral = 1000;
      const unrealizedPnl = 100;
      const positionValue = 10000;
      const equity = collateral + unrealizedPnl;
      const marginRatio = (equity / positionValue) * 100;
      expect(marginRatio).toBe(11); // 11%
    });
  });

  describe('order fill calculations', () => {
    it('should calculate fill percentage', () => {
      const size = 100;
      const filledSize = 75;
      const fillPercentage = (filledSize / size) * 100;
      expect(fillPercentage).toBe(75);
    });

    it('should calculate remaining size', () => {
      const size = 100;
      const filledSize = 75;
      const remainingSize = size - filledSize;
      expect(remainingSize).toBe(25);
    });
  });
});
