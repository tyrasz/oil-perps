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

    // LP pool (docs/lp-pool.md §12). Appended in order; existing codes do not move.
    #[msg("Invalid LP pool parameters")]
    InvalidPoolParams,

    #[msg("Market or pool is not fresh")]
    MarketNotFresh,

    #[msg("Trade would take the pool's net exposure past its cap")]
    PoolExposureLimit,

    #[msg("Trade would take the pool's net exposure past its weekend cap")]
    PoolWeekendExposureLimit,

    #[msg("Trade would take this account's house notional past its cap")]
    AccountExposureLimit,

    #[msg("Pool has pending bad debt awaiting ADL")]
    PoolAdlPending,

    #[msg("Pool cash cannot cover this payout yet")]
    PoolCashInsufficient,

    #[msg("LP withdrawals are blocked by the pool drawdown")]
    PoolWithdrawalsBlocked,

    #[msg("Withdrawal would leave net exposure beyond the pool's hard line or weekend bound")]
    PoolCapWouldBreach,

    #[msg("Deposit would exceed the pool's capital cap")]
    PoolCashCapExceeded,

    #[msg("Pool NAV is not positive")]
    PoolInsolvent,

    #[msg("Pool has no LP capital yet")]
    PoolHasNoLps,

    #[msg("Price or shares are worse than the acceptable limit")]
    SlippageExceeded,

    #[msg("Invalid LP amount")]
    InvalidLpAmount,

    #[msg("Invalid LP share class")]
    InvalidLpClass,

    #[msg("Not enough free LP shares")]
    InsufficientShares,

    #[msg("A withdrawal request is already pending")]
    WithdrawalPending,

    #[msg("No withdrawal request is pending")]
    NoWithdrawalPending,

    #[msg("Withdrawal delay has not elapsed")]
    WithdrawalDelayNotElapsed,

    #[msg("No insurance excess to sweep")]
    NothingToSweep,

    #[msg("Amount exceeds the treasury")]
    InsufficientTreasury,

    #[msg("Position has no unrealized loss to settle")]
    NothingToSettle,

    #[msg("Settling would exhaust the position's collateral; liquidate instead")]
    SettleWouldBankrupt,

    #[msg("Invalid ADL mode")]
    InvalidAdlMode,

    #[msg("Only the ADL keeper or admin may run exposure ADL")]
    AdlUnauthorized,

    #[msg("ADL is not required")]
    AdlNotRequired,

    #[msg("Position is on the wrong side for this ADL")]
    AdlWrongSide,

    #[msg("ADL candidate is not profitable")]
    AdlCandidateNotProfitable,

    #[msg("ADL candidate's score is below the floor")]
    AdlScoreBelowFloor,

    #[msg("ADL would reduce net exposure past its target")]
    AdlOvershoot,

    #[msg("Pending bad debt is not old enough to write off")]
    BadDebtNotStale,

    #[msg("Funding is not due yet")]
    FundingNotDue,

    #[msg("Pool parameter change is still timelocked")]
    ParamsTimelocked,

    #[msg("No pool parameter change is pending")]
    NoPendingParams,

    #[msg("Pool bucket would go negative")]
    BucketUnderflow,

    #[msg("Vault balance does not cover the pool buckets")]
    VaultInvariantViolated,

    #[msg("Pool drawdown freezes net-raising opens")]
    PoolDrawdownFreeze,

    #[msg("LP flows are closed in this window")]
    LpFlowWindowClosed,

    #[msg("Funding must be cranked first")]
    FundingStale,

    #[msg("Withdrawal request has lapsed; cancel and request again")]
    WithdrawalExpired,

    #[msg("Market is not settled")]
    MarketNotSettled,

    #[msg("Profitable closes wait profit_close_quiet_secs after a session open")]
    ProfitCloseQuiet,

    // Pyth PriceUpdateV2 oracle (docs/lp-pool.md D47)
    #[msg("Oracle price update is not fully verified")]
    OracleNotFullyVerified,

    #[msg("Oracle price update is for another feed id")]
    OracleFeedIdMismatch,

    #[msg("No oracle feed change is pending")]
    NoPendingOracleChange,

    #[msg("Oracle feed change is still timelocked")]
    OracleChangeTimelocked,
}
