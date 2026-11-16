use anchor_lang::prelude::*;

#[account]
#[derive(Default)]
pub struct Market {
    pub authority: Pubkey,
    pub collateral_mint: Pubkey,
    pub vault: Pubkey,
    pub pyth_price_feed: Pubkey,

    // Commodity identifier (e.g., "OIL", "GOLD", "SILVER")
    pub commodity: [u8; 8],             // Fixed-size commodity identifier

    // Market parameters
    pub max_leverage: u32,              // 20x = 20_000 (3 decimals)
    pub maintenance_margin_ratio: u32,  // 5% = 500 (2 decimals, basis points)
    pub initial_margin_ratio: u32,      // 10% = 1000 (2 decimals, basis points)
    pub taker_fee: u32,                 // 0.05% = 5 (2 decimals, basis points)
    pub maker_fee: u32,                 // 0.02% = 2 (2 decimals, basis points)
    pub liquidation_fee: u32,           // 2.5% = 250 (2 decimals, basis points)

    // Open interest tracking
    pub long_open_interest: u64,        // In base units (oil contracts)
    pub short_open_interest: u64,       // In base units (oil contracts)
    pub max_open_interest: u64,         // Cap per side

    // Funding rate
    pub funding_rate: i64,              // Current funding rate (can be negative)
    pub last_funding_time: i64,         // Unix timestamp
    pub funding_interval: i64,          // Seconds between funding (3600 = 1 hour)

    // Insurance fund
    pub insurance_fund: u64,            // Accumulated from liquidations

    // Counters
    pub total_positions: u64,
    pub total_trades: u64,

    pub bump: u8,
    pub is_paused: bool,
}

impl Market {
    pub const LEN: usize = 8 +  // discriminator
        32 +  // authority
        32 +  // collateral_mint
        32 +  // vault
        32 +  // pyth_price_feed
        8 +   // commodity identifier
        4 +   // max_leverage
        4 +   // maintenance_margin_ratio
        4 +   // initial_margin_ratio
        4 +   // taker_fee
        4 +   // maker_fee
        4 +   // liquidation_fee
        8 +   // long_open_interest
        8 +   // short_open_interest
        8 +   // max_open_interest
        8 +   // funding_rate
        8 +   // last_funding_time
        8 +   // funding_interval
        8 +   // insurance_fund
        8 +   // total_positions
        8 +   // total_trades
        1 +   // bump
        1 +   // is_paused
        56;   // padding for future use (reduced by 8 for commodity)

    /// Convert commodity bytes to string
    pub fn commodity_str(&self) -> String {
        String::from_utf8_lossy(&self.commodity)
            .trim_end_matches('\0')
            .to_string()
    }

    /// Create commodity bytes from string
    pub fn commodity_from_str(s: &str) -> [u8; 8] {
        let mut bytes = [0u8; 8];
        let src = s.as_bytes();
        let len = src.len().min(8);
        bytes[..len].copy_from_slice(&src[..len]);
        bytes
    }

    pub fn can_increase_long_oi(&self, size: u64) -> bool {
        self.long_open_interest.checked_add(size)
            .map(|new_oi| new_oi <= self.max_open_interest)
            .unwrap_or(false)
    }

    pub fn can_increase_short_oi(&self, size: u64) -> bool {
        self.short_open_interest.checked_add(size)
            .map(|new_oi| new_oi <= self.max_open_interest)
            .unwrap_or(false)
    }

    /// Credit `credit` to the insurance fund, then use the fund to absorb a
    /// position's `deficit` (losses beyond its collateral).
    /// Returns the part of the deficit the fund could not cover.
    pub fn settle_insurance(&mut self, credit: u64, deficit: u64) -> u64 {
        let (fund, uncovered) = apply_insurance(self.insurance_fund, credit, deficit);
        self.insurance_fund = fund;
        uncovered
    }
}

/// Returns `(new_fund, uncovered_deficit)`.
pub fn apply_insurance(fund: u64, credit: u64, deficit: u64) -> (u64, u64) {
    let available = fund.saturating_add(credit);
    let covered = deficit.min(available);
    (available - covered, deficit - covered)
}

/// Split a position's final equity into what is paid out and what is owed.
/// Returns `(payout, deficit)`; at most one of them is non-zero.
pub fn split_equity(equity: i64) -> (u64, u64) {
    if equity >= 0 {
        (equity as u64, 0)
    } else {
        (0, equity.unsigned_abs())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credit_with_no_deficit_grows_fund() {
        assert_eq!(apply_insurance(100, 5, 0), (105, 0));
    }

    #[test]
    fn deficit_is_drawn_from_fund() {
        assert_eq!(apply_insurance(100, 5, 30), (75, 0));
    }

    #[test]
    fn deficit_beyond_fund_is_reported_uncovered() {
        assert_eq!(apply_insurance(10, 5, 40), (0, 25));
    }

    #[test]
    fn split_equity_handles_both_signs() {
        assert_eq!(split_equity(250), (250, 0));
        assert_eq!(split_equity(0), (0, 0));
        assert_eq!(split_equity(-75), (0, 75));
        assert_eq!(split_equity(i64::MIN), (0, i64::MIN.unsigned_abs()));
    }
}

#[account]
#[derive(Default)]
pub struct Vault {
    pub market: Pubkey,
    pub collateral_mint: Pubkey,
    pub token_account: Pubkey,
    pub total_deposits: u64,
    pub bump: u8,
}

impl Vault {
    pub const LEN: usize = 8 + 32 + 32 + 32 + 8 + 1 + 32;
}
