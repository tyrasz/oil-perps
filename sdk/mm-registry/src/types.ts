import { PublicKey } from "@solana/web3.js";
import BN from "bn.js";

// ============================================================================
// Enums
// ============================================================================

export enum MmStatus {
  Inactive = 0,
  Active = 1,
  Suspended = 2,
  Deregistered = 3,
}

export enum QuoteSide {
  Bid = 0,
  Ask = 1,
}

export enum QuoteStatus {
  Active = 0,
  Filled = 1,
  Cancelled = 2,
  Expired = 3,
}

// ============================================================================
// Account Types
// ============================================================================

export interface MmRegistry {
  authority: PublicKey;
  market: PublicKey;
  collateralMint: PublicKey;
  minCollateral: BN;
  maxSpread: number;
  minQuoteSize: BN;
  maxQuoteSize: BN;
  mmFee: number;
  totalMms: number;
  activeQuotes: number;
  totalVolume: BN;
  totalFees: BN;
  isOpen: boolean;
  isTradingEnabled: boolean;
  bump: number;
}

export interface MarketMaker {
  owner: PublicKey;
  registry: PublicKey;
  status: MmStatus;
  collateralDeposited: BN;
  collateralLocked: BN;
  collateralAvailable: BN;
  inventory: BN;
  averageEntryPrice: BN;
  realizedPnl: BN;
  unrealizedPnl: BN;
  totalVolume: BN;
  totalFees: BN;
  activeQuotes: number;
  registeredAt: BN;
  lastActiveAt: BN;
  bump: number;
}

export interface Quote {
  marketMaker: PublicKey;
  registry: PublicKey;
  side: QuoteSide;
  price: BN;
  size: BN;
  remaining: BN;
  collateralLocked: BN;
  status: QuoteStatus;
  createdAt: BN;
  expiresAt: BN;
  bump: number;
}

export interface TwoSidedQuote {
  marketMaker: PublicKey;
  registry: PublicKey;
  bidPrice: BN;
  bidSize: BN;
  bidRemaining: BN;
  askPrice: BN;
  askSize: BN;
  askRemaining: BN;
  collateralLocked: BN;
  isActive: boolean;
  createdAt: BN;
  updatedAt: BN;
  expiresAt: BN;
  bump: number;
}

// ============================================================================
// Instruction Parameters
// ============================================================================

export interface InitializeRegistryParams {
  minCollateral: BN;
  maxSpread: number;
  minQuoteSize: BN;
  maxQuoteSize: BN;
  mmFee: number;
}

export interface PostQuoteParams {
  bidPrice: BN;
  bidSize: BN;
  askPrice: BN;
  askSize: BN;
  expiresIn: BN;
}

export interface UpdateQuoteParams {
  bidPrice?: BN;
  bidSize?: BN;
  askPrice?: BN;
  askSize?: BN;
}

export interface FillQuoteParams {
  side: QuoteSide;
  size: BN;
  maxPrice?: BN;
  minPrice?: BN;
}

// ============================================================================
// Result Types
// ============================================================================

export interface FillQuoteResult {
  fillPrice: BN;
  fillSize: BN;
  fee: BN;
}

// ============================================================================
// SDK Types
// ============================================================================

export interface MmRegistryConfig {
  programId: PublicKey;
  market: PublicKey;
  collateralMint: PublicKey;
}

export interface QuoteInfo {
  quote: TwoSidedQuote;
  publicKey: PublicKey;
  marketMaker: MarketMaker;
  spread: number;
  midPrice: BN;
}

export interface MarketMakerInfo {
  marketMaker: MarketMaker;
  publicKey: PublicKey;
  collateralAccount: PublicKey;
  availableCollateral: BN;
  utilizationRate: number;
}
