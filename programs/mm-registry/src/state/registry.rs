use anchor_lang::prelude::*;

/// Global configuration for the Market Maker Registry
#[account]
#[derive(Default)]
pub struct MmRegistry {
    /// Authority that can update registry parameters
    pub authority: Pubkey,

    /// Market this registry serves
    pub market: Pubkey,

    /// Collateral mint (USDC)
    pub collateral_mint: Pubkey,

    /// Minimum collateral required to register as MM
    pub min_collateral: u64,

    /// Maximum spread allowed (basis points)
    pub max_spread: u32,

    /// Minimum quote size
    pub min_quote_size: u64,

    /// Maximum quote size
    pub max_quote_size: u64,

    /// Fee charged to MMs per fill (basis points)
    pub mm_fee: u32,

    /// Total registered market makers
    pub total_mms: u32,

    /// Total active quotes
    pub active_quotes: u32,

    /// Total volume traded through MMs
    pub total_volume: u64,

    /// Total fees collected
    pub total_fees: u64,

    /// Whether registry is accepting new MMs
    pub is_open: bool,

    /// Whether trading is enabled
    pub is_trading_enabled: bool,

    pub bump: u8,
}

impl MmRegistry {
    pub const LEN: usize = 8 +  // discriminator
        32 +  // authority
        32 +  // market
        32 +  // collateral_mint
        8 +   // min_collateral
        4 +   // max_spread
        8 +   // min_quote_size
        8 +   // max_quote_size
        4 +   // mm_fee
        4 +   // total_mms
        4 +   // active_quotes
        8 +   // total_volume
        8 +   // total_fees
        1 +   // is_open
        1 +   // is_trading_enabled
        1 +   // bump
        64;   // padding
}
