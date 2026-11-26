use anchor_lang::prelude::*;

#[error_code]
pub enum PropAmmError {
    #[msg("Vault is not active")]
    VaultNotActive,

    #[msg("Position size exceeds maximum")]
    PositionTooLarge,

    #[msg("Trade would exceed exposure limit")]
    ExposureLimitExceeded,

    #[msg("Trade would exceed utilization limit")]
    UtilizationLimitExceeded,

    #[msg("Insufficient vault balance")]
    InsufficientVaultBalance,

    #[msg("Insufficient LP shares")]
    InsufficientShares,

    #[msg("Withdrawal not yet requested")]
    WithdrawalNotRequested,

    #[msg("Withdrawal delay not elapsed")]
    WithdrawalDelayNotElapsed,

    #[msg("Withdrawal already requested")]
    WithdrawalAlreadyRequested,

    #[msg("Invalid oracle price")]
    InvalidOraclePrice,

    #[msg("Oracle price is stale")]
    StaleOraclePrice,

    #[msg("Math overflow")]
    MathOverflow,

    #[msg("Invalid parameters")]
    InvalidParameters,

    #[msg("Unauthorized")]
    Unauthorized,

    #[msg("Cannot withdraw with pending positions")]
    PendingPositions,

    #[msg("Oracle confidence interval is too wide")]
    OracleConfidenceTooWide,

    #[msg("Price feed is not owned by the configured oracle program")]
    InvalidOracleAccount,

    // Pyth PriceUpdateV2 oracle (docs/lp-pool.md D47)
    #[msg("Oracle price update is not fully verified")]
    OracleNotFullyVerified,

    #[msg("Oracle price update is for another feed id")]
    OracleFeedIdMismatch,
}
