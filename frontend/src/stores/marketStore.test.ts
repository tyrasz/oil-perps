import { describe, it, expect, beforeEach } from 'vitest';
import { useMarketStore } from './marketStore';
import type { MarketData, OrderBook, Trade, Position, Order, UserAccount } from '../types';

describe('marketStore', () => {
  beforeEach(() => {
    // Reset store state before each test
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

  describe('initial state', () => {
    it('should have correct initial values', () => {
      const state = useMarketStore.getState();

      expect(state.market).toBeNull();
      expect(state.orderBook).toBeNull();
      expect(state.recentTrades).toEqual([]);
      expect(state.userAccount).toBeNull();
      expect(state.positions).toEqual([]);
      expect(state.orders).toEqual([]);
      expect(state.selectedLeverage).toBe(10);
      expect(state.orderType).toBe('market');
    });
  });

  describe('setMarket', () => {
    it('should update market data', () => {
      const mockMarket: MarketData = {
        address: 'market123',
        price: 75.50,
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

      expect(useMarketStore.getState().market).toEqual(mockMarket);
    });
  });

  describe('setOrderBook', () => {
    it('should update order book', () => {
      const mockOrderBook: OrderBook = {
        bids: [
          { price: 75.00, size: 100, total: 100 },
          { price: 74.95, size: 150, total: 250 },
        ],
        asks: [
          { price: 75.05, size: 80, total: 80 },
          { price: 75.10, size: 120, total: 200 },
        ],
        spread: 0.05,
        midPrice: 75.025,
      };

      useMarketStore.getState().setOrderBook(mockOrderBook);

      expect(useMarketStore.getState().orderBook).toEqual(mockOrderBook);
    });
  });

  describe('addTrade', () => {
    it('should add a trade to recentTrades', () => {
      const mockTrade: Trade = {
        signature: 'sig123',
        side: 'long',
        price: 75.50,
        size: 10,
        timestamp: Date.now(),
      };

      useMarketStore.getState().addTrade(mockTrade);

      const trades = useMarketStore.getState().recentTrades;
      expect(trades).toHaveLength(1);
      expect(trades[0]).toEqual(mockTrade);
    });

    it('should prepend new trades', () => {
      const trade1: Trade = {
        signature: 'sig1',
        side: 'long',
        price: 75.00,
        size: 5,
        timestamp: Date.now(),
      };
      const trade2: Trade = {
        signature: 'sig2',
        side: 'short',
        price: 75.10,
        size: 8,
        timestamp: Date.now() + 1000,
      };

      useMarketStore.getState().addTrade(trade1);
      useMarketStore.getState().addTrade(trade2);

      const trades = useMarketStore.getState().recentTrades;
      expect(trades).toHaveLength(2);
      expect(trades[0].signature).toBe('sig2');
      expect(trades[1].signature).toBe('sig1');
    });

    it('should limit trades to 50', () => {
      // Add 55 trades
      for (let i = 0; i < 55; i++) {
        useMarketStore.getState().addTrade({
          signature: `sig${i}`,
          side: 'long',
          price: 75.00 + i * 0.01,
          size: 1,
          timestamp: Date.now() + i,
        });
      }

      const trades = useMarketStore.getState().recentTrades;
      expect(trades).toHaveLength(50);
      // Most recent trade should be first
      expect(trades[0].signature).toBe('sig54');
    });
  });

  describe('setUserAccount', () => {
    it('should update user account', () => {
      const mockAccount: UserAccount = {
        address: 'user123',
        collateralBalance: 10000,
        totalPositions: 3,
        realizedPnl: 500,
      };

      useMarketStore.getState().setUserAccount(mockAccount);

      expect(useMarketStore.getState().userAccount).toEqual(mockAccount);
    });
  });

  describe('setPositions', () => {
    it('should update positions', () => {
      const mockPositions: Position[] = [
        {
          address: 'pos1',
          owner: 'user123',
          side: 'long',
          size: 100,
          collateral: 1000,
          entryPrice: 75.00,
          leverage: 10,
          unrealizedPnl: 50,
          marginRatio: 0.15,
          openedAt: Date.now(),
        },
        {
          address: 'pos2',
          owner: 'user123',
          side: 'short',
          size: 50,
          collateral: 500,
          entryPrice: 76.00,
          leverage: 10,
          unrealizedPnl: -20,
          marginRatio: 0.12,
          openedAt: Date.now(),
        },
      ];

      useMarketStore.getState().setPositions(mockPositions);

      expect(useMarketStore.getState().positions).toEqual(mockPositions);
    });
  });

  describe('setOrders', () => {
    it('should update orders', () => {
      const mockOrders: Order[] = [
        {
          address: 'order1',
          owner: 'user123',
          side: 'bid',
          orderType: 'limit',
          price: 74.50,
          size: 100,
          filledSize: 0,
          status: 'open',
          createdAt: Date.now(),
        },
      ];

      useMarketStore.getState().setOrders(mockOrders);

      expect(useMarketStore.getState().orders).toEqual(mockOrders);
    });
  });

  describe('setSelectedLeverage', () => {
    it('should update selected leverage', () => {
      useMarketStore.getState().setSelectedLeverage(20);

      expect(useMarketStore.getState().selectedLeverage).toBe(20);
    });

    it('should allow minimum leverage of 1', () => {
      useMarketStore.getState().setSelectedLeverage(1);

      expect(useMarketStore.getState().selectedLeverage).toBe(1);
    });
  });

  describe('setOrderType', () => {
    it('should update order type to limit', () => {
      useMarketStore.getState().setOrderType('limit');

      expect(useMarketStore.getState().orderType).toBe('limit');
    });

    it('should update order type to market', () => {
      useMarketStore.getState().setOrderType('limit');
      useMarketStore.getState().setOrderType('market');

      expect(useMarketStore.getState().orderType).toBe('market');
    });
  });
});
