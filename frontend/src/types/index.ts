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

export interface ClosedPosition {
  address: string;
  owner: string;
  commodity: string;
  side: 'long' | 'short';
  size: number;
  collateral: number;
  entryPrice: number;
  exitPrice: number;
  leverage: number;
  realizedPnl: number;
  openedAt: number;
  closedAt: number;
  status: 'closed' | 'liquidated';
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

// Trigger orders for TP/SL
export interface TriggerOrder {
  id: string;
  positionAddress: string;
  type: 'take_profit' | 'stop_loss';
  triggerPrice: number;
  closePercent: number; // 0-100, percentage of position to close
  createdAt: number;
  status: 'active' | 'triggered' | 'cancelled';
}

// Price alert
export interface PriceAlert {
  id: string;
  commodity: string;
  condition: 'above' | 'below';
  targetPrice: number;
  createdAt: number;
  triggered: boolean;
  notified: boolean;
}

// Portfolio metrics
export interface PortfolioMetrics {
  totalValue: number;
  totalPnl: number;
  totalPnlPercent: number;
  winRate: number;
  totalTrades: number;
  avgWin: number;
  avgLoss: number;
  largestWin: number;
  largestLoss: number;
  profitFactor: number;
}
