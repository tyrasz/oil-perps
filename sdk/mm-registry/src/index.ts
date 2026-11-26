// Main client
export {
  MmRegistryClient,
  MM_REGISTRY_PROGRAM_ID,
  PerpsMarketInfo,
  VenueInfo,
  TakerFillOptions,
  TakerFill,
} from "./client";

// mm-registry's IDL (committed copy of target/idl/mm_registry.json, kept in
// sync by scripts/gen-idl.sh --sync) and its TS type, for
// `new Program(MM_REGISTRY_IDL as MmRegistryIdl, provider)`
export { default as MM_REGISTRY_IDL } from "./idl/mm_registry.json";
export type { MmRegistry as MmRegistryIdl } from "./idl/mm_registry";

// Types
export {
  // Enums
  MmStatus,
  QuoteSide,
  QuoteStatus,
  // Account types
  MmRegistry,
  MarketMaker,
  Quote,
  TwoSidedQuote,
  // Instruction params
  InitializeRegistryParams,
  PostQuoteParams,
  UpdateQuoteParams,
  FillQuoteParams,
  VenueApprovalParams,
  // Result types
  FillQuoteResult,
  // SDK types
  MmRegistryConfig,
  QuoteInfo,
  MarketMakerInfo,
  VenueSides,
} from "./types";

// PDA utilities
export {
  SEEDS,
  findRegistryPda,
  findMarketMakerPda,
  findMmCollateralPda,
  findQuotePda,
  getMarketMakerPdas,
  // perps-core
  PERPS_CORE_PROGRAM_ID,
  BPF_LOADER_UPGRADEABLE_PROGRAM_ID,
  PERPS_SEEDS,
  VenueRole,
  findPerpsConfigPda,
  findPerpsUserPda,
  findVenuePda,
  findVenueAuthorityPda,
  findProgramDataPda,
  findVenueApprovalPda,
  findVPositionPda,
  findVMakerPda,
  findLpPoolPda,
  findLpPositionPda,
} from "./pda";

// perps-core LP pool (docs/lp-pool.md §1.2, §1.3, §16.3)
export {
  POOL_PARAMS_LEN,
  LP_POOL_LEN,
  LP_POSITION_LEN,
  LP_DEAD_SHARES,
  LP_CLASS_COMMON,
  LP_POOL_DISCRIMINATOR,
  LP_POSITION_DISCRIMINATOR,
  INITIALIZE_POOL_DISCRIMINATOR,
  PROPOSE_POOL_PARAMS_DISCRIMINATOR,
  MARKET_FUNDING_CARRY_SECS,
  MARKET_SETTLED_AT,
  MARKET_LEN,
  MARKET_ORACLE_OFFSETS,
  MarketOracleInfo,
  decodeMarketOracleFields,
  LP_POOL_OFFSETS,
  LP_POSITION_OFFSETS,
  POOL_PARAMS_LAYOUT,
  PoolParams,
  LpPoolInfo,
  LpPositionInfo,
  productionPoolParams,
  neutralPoolParams,
  encodePoolParams,
  decodePoolParams,
  decodeLpPool,
  decodeLpPosition,
  decodeMarketPoolFields,
  buildInitializePoolIx,
  buildProposePoolParamsIx,
} from "./perpsPool";

// Utility functions
export {
  // Constants
  PRICE_DECIMALS,
  PRICE_MULTIPLIER,
  SIZE_DECIMALS,
  SIZE_MULTIPLIER,
  BPS_DECIMALS,
  BPS_MULTIPLIER,
  // Conversion functions
  toPrice,
  fromPrice,
  toSize,
  fromSize,
  bpsToPercent,
  percentToBps,
  // Calculation functions
  calculateSpreadBps,
  calculateMidPrice,
  calculateNotional,
  calculateCollateralRequired,
  calculatePnl,
  calculateFillAmount,
  // Formatting functions
  formatPrice,
  formatSize,
  // Validation functions
  isQuoteValid,
  isExpired,
  // Timestamp functions
  getExpirationTimestamp,
  // Fills
  MAX_QUOTE_TTL_SECS,
  FILL_COMPUTE_UNITS,
  ONE_SHOT_APPROVAL_SECS,
  venueMargin,
  worstCaseVenueMargin,
  randomPositionNonce,
  isFillableQuote,
} from "./utils";
