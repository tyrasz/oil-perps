use anchor_lang::prelude::*;

#[error_code]
pub enum MmRegistryError {
    #[msg("Registry is not open for new registrations")]
    RegistryClosed,

    #[msg("Trading is not enabled")]
    TradingDisabled,

    #[msg("Insufficient collateral")]
    InsufficientCollateral,

    #[msg("Market maker is not active")]
    MarketMakerNotActive,

    #[msg("Market maker is suspended")]
    MarketMakerSuspended,

    #[msg("Maximum quotes reached")]
    MaxQuotesReached,

    #[msg("Quote size below minimum")]
    QuoteSizeTooSmall,

    #[msg("Quote size above maximum")]
    QuoteSizeTooLarge,

    #[msg("Quote spread exceeds maximum")]
    SpreadTooWide,

    #[msg("Quote has expired")]
    QuoteExpired,

    #[msg("Quote is not active")]
    QuoteNotActive,

    #[msg("Fill size too small")]
    FillSizeTooSmall,

    #[msg("Fill size exceeds remaining")]
    FillSizeExceedsRemaining,

    #[msg("Invalid price")]
    InvalidPrice,

    #[msg("Invalid quote parameters")]
    InvalidQuoteParams,

    #[msg("Unauthorized")]
    Unauthorized,

    #[msg("Math overflow")]
    MathOverflow,

    #[msg("Cannot withdraw locked collateral")]
    CollateralLocked,

    #[msg("Has active quotes - cancel them first")]
    HasActiveQuotes,

    #[msg("Has open inventory - close it first")]
    HasOpenInventory,

    /// Unused since fills settle into perps-core positions; kept so later codes don't shift
    #[msg("Quote fills are disabled until they settle into perps-core positions")]
    FillsDisabled,

    #[msg("Collateral mint must be the market's collateral mint")]
    InvalidCollateralMint,

    #[msg("Quote price is worse than the taker's limit")]
    PriceWorseThanLimit,

    #[msg("Legacy quote: set a leverage and expiry with update_quote")]
    QuoteNotFillable,

    #[msg("Taker cannot fill their own quote")]
    SelfTrade,

    #[msg("Market is not this registry's market")]
    WrongMarket,
}
