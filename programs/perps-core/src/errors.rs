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

    #[msg("Market is outside trading hours")]
    MarketClosed,

    #[msg("Market has been settled; positions can only be closed")]
    MarketSettled,

    #[msg("Oracle confidence interval is too wide")]
    OracleConfidenceTooWide,

    #[msg("Oracle price is below the market's minimum price")]
    OraclePriceBelowFloor,

    #[msg("Price feed is not owned by the configured oracle program")]
    InvalidOracleAccount,

    // Venue fills. Appended so existing codes stay put.
    #[msg("Signer is not the venue's authority")]
    VenueUnauthorized,

    #[msg("Venue is disabled")]
    VenueDisabled,

    #[msg("Venue program was upgraded since it was registered; the admin must re-attest")]
    VenueProgramChanged,

    #[msg("Venue fills are paused protocol-wide")]
    VenueFillsPaused,

    #[msg("Invalid venue parameters")]
    InvalidVenueParams,

    #[msg("Invalid fill parameters")]
    InvalidFillParams,

    #[msg("Fill price is outside the venue's band around the oracle")]
    FillPriceOutOfBand,

    #[msg("Fill notional exceeds the venue's per-fill cap")]
    FillTooLarge,

    #[msg("Taker and maker are the same wallet")]
    SelfTrade,

    #[msg("Not enough of taker and maker signed")]
    MissingSigner,

    #[msg("Venue approval has expired")]
    ApprovalExpired,

    #[msg("Fill would commit more collateral than the approval allows")]
    ApprovalAllowanceExceeded,

    #[msg("Leverage exceeds the approval's maximum")]
    ApprovalLeverageExceeded,

    #[msg("Fill price is beyond the approval's limit price")]
    ApprovalLimitPrice,

    #[msg("Fill price deviates too far from the oracle for the approval")]
    ApprovalPriceDeviation,

    #[msg("Invalid venue approval parameters")]
    InvalidApprovalParams,

    #[msg("Venue fill would open a position below initial margin")]
    VenueMarginInvariant,

    #[msg("The approval does not allow this side")]
    ApprovalSideNotAllowed,

    #[msg("Venue approvals can only be written by a top-level instruction")]
    ApprovalViaCpi,
}
