import { create } from 'zustand';
import type { MarketData, OrderBook, Position, Order, Trade, UserAccount } from '../types';
import { DEFAULT_COMMODITY, COMMODITIES, type CommodityConfig } from '../config/commodities';

interface MarketState {
  // Selected commodity
  selectedCommodity: CommodityConfig;

  // Market data (per commodity)
  markets: Record<string, MarketData>;
  orderBooks: Record<string, OrderBook>;
  recentTrades: Record<string, Trade[]>;

  // User data (filtered by selected commodity in components)
  userAccount: UserAccount | null;
  positions: Position[];
  orders: Order[];

  // UI state
  selectedLeverage: number;
  orderType: 'market' | 'limit';

  // Actions
  setSelectedCommodity: (commodity: CommodityConfig) => void;
  setMarket: (commodity: string, market: MarketData) => void;
  setOrderBook: (commodity: string, orderBook: OrderBook) => void;
  addTrade: (commodity: string, trade: Trade) => void;
  setUserAccount: (account: UserAccount) => void;
  setPositions: (positions: Position[]) => void;
  setOrders: (orders: Order[]) => void;
  setSelectedLeverage: (leverage: number) => void;
  setOrderType: (orderType: 'market' | 'limit') => void;

  // Computed getters
  getCurrentMarket: () => MarketData | null;
  getCurrentOrderBook: () => OrderBook | null;
  getCurrentTrades: () => Trade[];
  getPositionsForCommodity: (commodity: string) => Position[];
  getOrdersForCommodity: (commodity: string) => Order[];
}

export const useMarketStore = create<MarketState>((set, get) => ({
  // Initial state
  selectedCommodity: DEFAULT_COMMODITY,
  markets: {},
  orderBooks: {},
  recentTrades: {},
  userAccount: null,
  positions: [],
  orders: [],
  selectedLeverage: 10,
  orderType: 'market',

  // Actions
  setSelectedCommodity: (commodity) => set({ selectedCommodity: commodity }),

  setMarket: (commodity, market) => set((state) => ({
    markets: { ...state.markets, [commodity]: market },
  })),

  setOrderBook: (commodity, orderBook) => set((state) => ({
    orderBooks: { ...state.orderBooks, [commodity]: orderBook },
  })),

  addTrade: (commodity, trade) => set((state) => ({
    recentTrades: {
      ...state.recentTrades,
      [commodity]: [trade, ...(state.recentTrades[commodity] || [])].slice(0, 50),
    },
  })),

  setUserAccount: (userAccount) => set({ userAccount }),

  setPositions: (positions) => set({ positions }),

  setOrders: (orders) => set({ orders }),

  setSelectedLeverage: (selectedLeverage) => set({ selectedLeverage }),

  setOrderType: (orderType) => set({ orderType }),

  // Computed getters
  getCurrentMarket: () => {
    const state = get();
    return state.markets[state.selectedCommodity.id] || null;
  },

  getCurrentOrderBook: () => {
    const state = get();
    return state.orderBooks[state.selectedCommodity.id] || null;
  },

  getCurrentTrades: () => {
    const state = get();
    return state.recentTrades[state.selectedCommodity.id] || [];
  },

  getPositionsForCommodity: (commodity) => {
    const state = get();
    return state.positions.filter(p => p.commodity === commodity);
  },

  getOrdersForCommodity: (commodity) => {
    const state = get();
    return state.orders.filter(o => o.commodity === commodity);
  },
}));

// Export available commodities for easy access
export { COMMODITIES, DEFAULT_COMMODITY };
export type { CommodityConfig };
