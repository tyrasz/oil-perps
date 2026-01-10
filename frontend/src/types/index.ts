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

// Advanced order types
export type AdvancedOrderType =
  | 'market'
  | 'limit'
  | 'stop_loss'
  | 'take_profit'
  | 'stop_limit'
  | 'trailing_stop';

export type TriggerCondition = 'none' | 'price_above' | 'price_below';

export type OrderStatus =
  | 'open'
  | 'partially_filled'
  | 'filled'
  | 'cancelled'
  | 'triggered'
  | 'expired';

export interface Order {
  address: string;
  owner: string;
  commodity: string;
  side: 'bid' | 'ask';
  orderType: AdvancedOrderType;
  price: number;
  size: number;
  filledSize: number;
  status: OrderStatus;
  createdAt: number;
  expiresAt: number;

  // Trigger order fields
  triggerPrice: number;
  triggerCondition: TriggerCondition;

  // Trailing stop fields
  trailingAmount: number;
  trailingPercent: boolean;

  // OCO fields
  linkedOrder: string | null;
  isOco: boolean;

  // Position reference
  position: string | null;
  reduceOnly: boolean;
}

export interface Trade {
  signature: string;
  commodity: string;
  side: 'long' | 'short';
  price: number;
  size: number;
  timestamp: number;
}

export interface MarketData {
  address: string;
  commodity: string;
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

// Legacy types for backward compatibility
export type OrderSide = 'long' | 'short';
export type OrderType = 'market' | 'limit';

// Order entry params
export interface PlaceOrderParams {
  commodity: string;
  side: 'long' | 'short';
  orderType: AdvancedOrderType;
  size: number;
  price?: number;              // For limit orders
  triggerPrice?: number;       // For stop/take-profit orders
  triggerCondition?: TriggerCondition;
  trailingAmount?: number;     // For trailing stop
  trailingPercent?: boolean;   // If true, trailingAmount is in basis points
  reduceOnly?: boolean;
  expiresAt?: number;
}

// OCO order params
export interface PlaceOcoParams {
  commodity: string;
  size: number;

  // Take profit order
  takeProfitPrice: number;
  takeProfitTriggerPrice: number;

  // Stop loss order
  stopLossPrice: number;
  stopLossTriggerPrice: number;

  reduceOnly?: boolean;
  expiresAt?: number;
}

// Helper function to determine trigger condition based on order type and position
export function getTriggerCondition(
  orderType: AdvancedOrderType,
  positionSide: 'long' | 'short'
): TriggerCondition {
  if (orderType === 'stop_loss') {
    // Stop loss triggers when price moves against position
    return positionSide === 'long' ? 'price_below' : 'price_above';
  }
  if (orderType === 'take_profit') {
    // Take profit triggers when price moves in favor of position
    return positionSide === 'long' ? 'price_above' : 'price_below';
  }
  return 'none';
}
