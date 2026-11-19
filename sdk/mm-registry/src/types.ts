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
  /** Reserved: no fee is charged at fill */
  mmFee: number;
  totalMms: number;
  activeQuotes: number;
  totalVolume: BN;
  totalFees: BN;
  isOpen: boolean;
  isTradingEnabled: boolean;
  bump: number;
}

/**
 * Mirrors programs/mm-registry/src/state/market_maker.rs (field order matters for memcmp).
 * inventory, avgInventoryPrice, unrealizedPnl and realizedPnl are deprecated and
 * always 0: fills open perps-core positions owned by the MM's wallet.
 */
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

/** Mirrors programs/mm-registry/src/state/quote.rs (field order matters for memcmp) */
export interface TwoSidedQuote {
  marketMaker: PublicKey;
  registry: PublicKey;
  bidPrice: BN;
  bidSize: BN;
  bidRemaining: BN;
  askPrice: BN;
  askSize: BN;
  askRemaining: BN;
  minFillSize: BN;
  /** Only legacy quotes lock collateral; new quotes lock nothing */
  collateralLocked: BN;
  createdAt: BN;
  /** 0 only on legacy quotes, which cannot be filled until update_quote sets an expiry */
  expiresAt: BN;
  updatedAt: BN;
  isActive: boolean;
  bump: number;
  /** MM's leverage for fills of this quote (3 decimals, 10x = 10_000); 0 on legacy quotes */
  leverage: number;
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
  /** Seconds until expiry, 1..=MAX_QUOTE_TTL_SECS */
  expiresIn: BN;
  /** MM's leverage for fills (3 decimals, 10x = 10_000), > 0; perps-core caps it */
  leverage: number;
}

/** null leaves a field unchanged. Any update releases a legacy quote's lock. */
export interface UpdateQuoteParams {
  bidPrice: BN | null;
  bidSize: BN | null;
  askPrice: BN | null;
  askSize: BN | null;
  /** > 0 */
  leverage: number | null;
  /** New expiry, this many seconds from now: 1..=MAX_QUOTE_TTL_SECS */
  expiresIn: BN | null;
}

/**
 * A fill opens a taker position and the opposite MM position in perps-core,
 * each margined from its owner's perps-core UserAccount.
 */
export interface FillQuoteParams {
  size: BN;
  /** true = taker goes long against the ask, false = taker goes short against the bid */
  isBuy: boolean;
  /** > 0. Buy: ask <= limit; sell: bid >= limit. Guards against a repriced quote. */
  limitPrice: BN;
  /** Taker's leverage (3 decimals) */
  takerLeverage: number;
  /** Fill what remains instead of failing when size > remaining */
  allowPartial: boolean;
  /** Seeds the taker's position PDA; pick a random u64 (see randomPositionNonce) */
  positionNonce: BN;
}

/** Mirrors perps-core's VenueApprovalParams */
export interface VenueApprovalParams {
  /** Collateral the venue may still commit for this owner; spent down by fills */
  allowance: BN;
  /** 3 decimals, 1..=market max leverage */
  maxLeverage: number;
  /** 0 = none; applies when the owner goes long */
  maxBuyPrice: BN;
  /** 0 = none; applies when the owner goes short */
  minSellPrice: BN;
  /** Adverse deviation from the oracle, 0..=10_000 bps; 0 = off */
  maxPriceDeviationBps: number;
  /** Unix seconds; at most 90 days out */
  expiresAt: BN;
  /** Which sides the owner may be put on: VenueSides.Long | VenueSides.Short */
  allowedSides: number;
}

/** VenueApproval.allowed_sides bits */
export const VenueSides = { Long: 1, Short: 2, Both: 3 } as const;

// ============================================================================
// Result Types
// ============================================================================

export interface FillQuoteResult {
  fillPrice: BN;
  fillSize: BN;
  /** Always 0: perps-core charges its taker fee on each leg at close */
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
