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

    // Referral errors
    #[msg("Invalid referral code format")]
    InvalidReferralCode,

    #[msg("Referral code already exists")]
    ReferralCodeExists,

    #[msg("Referral code not found")]
    ReferralCodeNotFound,

    #[msg("Cannot use your own referral code")]
    SelfReferralNotAllowed,

    #[msg("User already has a referral applied")]
    ReferralAlreadyApplied,

    #[msg("User already has a referral code")]
    UserAlreadyHasCode,

    #[msg("No rewards available to claim")]
    NoRewardsToClaim,

    #[msg("Referral code is inactive")]
    ReferralCodeInactive,

    #[msg("Invalid referral parameters")]
    InvalidReferralParams,
}
