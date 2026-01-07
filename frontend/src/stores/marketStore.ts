import { create } from 'zustand';
import type { MarketData, OrderBook, Position, Order, Trade, UserAccount } from '../types';

interface MarketState {
  // Market data
  market: MarketData | null;
  orderBook: OrderBook | null;
  recentTrades: Trade[];

  // User data
  userAccount: UserAccount | null;
  positions: Position[];
  orders: Order[];

  // UI state
  selectedLeverage: number;
  orderType: 'market' | 'limit';

  // Actions
  setMarket: (market: MarketData) => void;
  setOrderBook: (orderBook: OrderBook) => void;
  addTrade: (trade: Trade) => void;
  setUserAccount: (account: UserAccount) => void;
  setPositions: (positions: Position[]) => void;
  setOrders: (orders: Order[]) => void;
  setSelectedLeverage: (leverage: number) => void;
  setOrderType: (orderType: 'market' | 'limit') => void;
}

export const useMarketStore = create<MarketState>((set) => ({
  // Initial state
  market: null,
  orderBook: null,
  recentTrades: [],
  userAccount: null,
  positions: [],
  orders: [],
  selectedLeverage: 10,
  orderType: 'market',

  // Actions
  setMarket: (market) => set({ market }),

  setOrderBook: (orderBook) => set({ orderBook }),

  addTrade: (trade) => set((state) => ({
    recentTrades: [trade, ...state.recentTrades].slice(0, 50),
  })),

  setUserAccount: (userAccount) => set({ userAccount }),

  setPositions: (positions) => set({ positions }),

  setOrders: (orders) => set({ orders }),

  setSelectedLeverage: (selectedLeverage) => set({ selectedLeverage }),

  setOrderType: (orderType) => set({ orderType }),
}));
