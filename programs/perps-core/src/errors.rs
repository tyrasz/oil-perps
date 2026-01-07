use anchor_lang::prelude::*;

#[error_code]
pub enum PerpsError {
    #[msg("Market is currently paused")]
    MarketPaused,

    #[msg("Leverage exceeds maximum allowed")]
    ExcessiveLeverage,

    #[msg("Insufficient collateral for position")]
    InsufficientCollateral,

    #[msg("Position size is too small")]
    PositionTooSmall,

    #[msg("Position size exceeds maximum")]
    PositionTooLarge,

    #[msg("Open interest cap would be exceeded")]
    OpenInterestCapExceeded,

    #[msg("Position is below maintenance margin")]
    BelowMaintenanceMargin,

    #[msg("Position is not liquidatable")]
    NotLiquidatable,

    #[msg("Invalid oracle price")]
    InvalidOraclePrice,

    #[msg("Oracle price is stale")]
    StaleOraclePrice,

    #[msg("Invalid position side")]
    InvalidPositionSide,

    #[msg("Position not found")]
    PositionNotFound,

    #[msg("Position is already closed")]
    PositionAlreadyClosed,

    #[msg("Unauthorized access")]
    Unauthorized,

    #[msg("Math overflow")]
    MathOverflow,

    #[msg("Invalid market configuration")]
    InvalidMarketConfig,

    #[msg("Cannot reduce position below zero")]
    InvalidPositionReduction,

    #[msg("Insufficient vault balance")]
    InsufficientVaultBalance,
}
