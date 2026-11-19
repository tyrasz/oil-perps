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
  PartiallyFilled = 1,
  Filled = 2,
  Cancelled = 3,
  Expired = 4,
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

/** Mirrors programs/mm-registry/src/state/market_maker.rs (field order matters for memcmp) */
export interface MarketMaker {
  owner: PublicKey;
  registry: PublicKey;
  collateralAccount: PublicKey;
  collateralDeposited: BN;
  collateralLocked: BN;
  collateralAvailable: BN;
  inventory: BN;
  avgInventoryPrice: BN;
  unrealizedPnl: BN;
  realizedPnl: BN;
  totalVolume: BN;
  totalFills: BN;
  totalFeesPaid: BN;
  registeredAt: BN;
  lastActiveAt: BN;
  activeQuotes: number;
  maxQuotes: number;
  status: MmStatus;
  bump: number;
  /** Quotes ever posted; the next quote's PDA index */
  quotesPosted: BN;
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
  minFillSize: BN;
  /** Seconds until expiry; 0 = no expiry */
  expiresIn: BN;
}

export interface UpdateQuoteParams {
  bidPrice: BN | null;
  bidSize: BN | null;
  askPrice: BN | null;
  askSize: BN | null;
}

/**
 * Note: fills are disabled on-chain (FillsDisabled) until they settle into
 * perps-core positions.
 */
export interface FillQuoteParams {
  size: BN;
  /** true = taker buys (fills the MM's ask), false = taker sells (fills the bid) */
  isBuy: boolean;
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
