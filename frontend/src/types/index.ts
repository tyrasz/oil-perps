export interface Position {
  address: string;
  owner: string;
  commodity: string;          // Commodity ID (e.g., 'OIL', 'GOLD')
  side: 'long' | 'short';
  size: number;
  collateral: number;
  entryPrice: number;
  leverage: number;
  unrealizedPnl: number;
  marginRatio: number;
  openedAt: number;
}

export interface Order {
  address: string;
  owner: string;
  commodity: string;          // Commodity ID (e.g., 'OIL', 'GOLD')
  side: 'bid' | 'ask';
  orderType: 'limit' | 'market' | 'stop_loss' | 'take_profit';
  price: number;
  size: number;
  filledSize: number;
  status: 'open' | 'partially_filled' | 'filled' | 'cancelled';
  createdAt: number;
}

export interface Trade {
  signature: string;
  commodity: string;          // Commodity ID (e.g., 'OIL', 'GOLD')
  side: 'long' | 'short';
  price: number;
  size: number;
  timestamp: number;
}

export interface MarketData {
  address: string;
  commodity: string;          // Commodity ID (e.g., 'OIL', 'GOLD')
  price: number;
  priceChange24h: number;
  volume24h: number;
  openInterest: number;
  longOpenInterest: number;
  shortOpenInterest: number;
  fundingRate: number;
  maxLeverage: number;
  isPaused: boolean;
}

export interface OrderBookEntry {
  price: number;
  size: number;
  total: number;
}

export interface OrderBook {
  bids: OrderBookEntry[];
  asks: OrderBookEntry[];
  spread: number;
  midPrice: number;
}

export interface UserAccount {
  address: string;
  collateralBalance: number;
  totalPositions: number;
  realizedPnl: number;
}

export type OrderSide = 'long' | 'short';
export type OrderType = 'market' | 'limit';
