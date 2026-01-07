use anchor_lang::prelude::*;

#[account]
#[derive(Default)]
pub struct LpVault {
    pub authority: Pubkey,
    pub market: Pubkey,
    pub collateral_mint: Pubkey,
    pub token_account: Pubkey,
    pub pyth_price_feed: Pubkey,

    // LP accounting
    pub total_assets: u64,           // Total USDC in vault (excluding unrealized PnL)
    pub total_shares: u64,           // Total LP shares issued
    pub pending_fees: u64,           // Accumulated fees to distribute

    // Exposure tracking
    pub net_exposure: i64,           // Notional: + = protocol short, - = protocol long
    pub total_long_size: u64,        // Total long positions size
    pub total_short_size: u64,       // Total short positions size

    // PnL tracking
    pub unrealized_pnl: i64,         // Current PnL from open positions
    pub cumulative_fees: u64,        // Total fees earned historically
    pub cumulative_pnl: i64,         // Historical realized PnL

    // Risk parameters
    pub max_exposure: u64,           // Max absolute net exposure allowed
    pub max_utilization: u32,        // Max % of vault at risk (basis points, 8000 = 80%)
    pub max_position_size: u64,      // Max single position size

    // Spread parameters (basis points)
    pub base_spread: u32,            // Minimum spread (e.g., 5 = 0.05%)
    pub max_skew_spread: u32,        // Additional spread at max skew (e.g., 50 = 0.5%)

    // Fee parameters (basis points)
    pub trading_fee: u32,            // Fee per trade (e.g., 5 = 0.05%)
    pub lp_fee_share: u32,           // % of fees to LPs (e.g., 7000 = 70%)

    // Withdrawal parameters
    pub withdrawal_delay: i64,       // Seconds before withdrawal allowed (e.g., 86400 = 24h)

    pub bump: u8,
    pub is_active: bool,
}

impl LpVault {
    pub const LEN: usize = 8 +  // discriminator
        32 +  // authority
        32 +  // market
        32 +  // collateral_mint
        32 +  // token_account
        32 +  // pyth_price_feed
        8 +   // total_assets
        8 +   // total_shares
        8 +   // pending_fees
        8 +   // net_exposure
        8 +   // total_long_size
        8 +   // total_short_size
        8 +   // unrealized_pnl
        8 +   // cumulative_fees
        8 +   // cumulative_pnl
        8 +   // max_exposure
        4 +   // max_utilization
        8 +   // max_position_size
        4 +   // base_spread
        4 +   // max_skew_spread
        4 +   // trading_fee
        4 +   // lp_fee_share
        8 +   // withdrawal_delay
        1 +   // bump
        1 +   // is_active
        64;   // padding

    /// Calculate current spread based on exposure skew
    pub fn calculate_spread(&self, is_long: bool) -> u32 {
        let skew_ratio = if self.max_exposure > 0 {
            (self.net_exposure.abs() as u64 * 10000) / self.max_exposure
        } else {
            0
        };

        let skew_spread = (skew_ratio as u32 * self.max_skew_spread) / 10000;
        let total_spread = self.base_spread + skew_spread;

        // Reduce spread for trades that reduce exposure
        let exposure_reducing = (is_long && self.net_exposure < 0) ||
                                (!is_long && self.net_exposure > 0);

        if exposure_reducing {
            // 50% discount for exposure-reducing trades
            total_spread / 2
        } else {
            total_spread
        }
    }

    /// Calculate entry price for a trade
    pub fn calculate_entry_price(&self, oracle_price: u64, is_long: bool) -> u64 {
        let spread = self.calculate_spread(is_long);

        if is_long {
            // Long entry: pay oracle + spread
            oracle_price + (oracle_price * spread as u64 / 10000)
        } else {
            // Short entry: receive oracle - spread
            oracle_price - (oracle_price * spread as u64 / 10000)
        }
    }

    /// Calculate exit price for a trade
    pub fn calculate_exit_price(&self, oracle_price: u64, is_long: bool) -> u64 {
        let spread = self.calculate_spread(!is_long); // Opposite spread for exit

        if is_long {
            // Long exit: receive oracle - spread
            oracle_price - (oracle_price * spread as u64 / 10000)
        } else {
            // Short exit: pay oracle + spread
            oracle_price + (oracle_price * spread as u64 / 10000)
        }
    }

    /// Calculate LP share value (assets per share, scaled by 1e6)
    pub fn share_value(&self) -> u64 {
        if self.total_shares == 0 {
            1_000_000 // 1:1 initial ratio
        } else {
            let total_value = (self.total_assets as i64 + self.unrealized_pnl) as u64;
            (total_value * 1_000_000) / self.total_shares
        }
    }

    /// Calculate shares to mint for a deposit
    pub fn shares_for_deposit(&self, amount: u64) -> u64 {
        if self.total_shares == 0 {
            amount // 1:1 for first deposit
        } else {
            (amount * 1_000_000) / self.share_value()
        }
    }

    /// Calculate assets to return for shares
    pub fn assets_for_shares(&self, shares: u64) -> u64 {
        (shares * self.share_value()) / 1_000_000
    }

    /// Check if a trade would exceed exposure limits
    pub fn can_accept_trade(&self, size: u64, is_long: bool) -> bool {
        if !self.is_active {
            return false;
        }

        // Check position size limit
        if size > self.max_position_size {
            return false;
        }

        // Calculate new exposure
        let new_exposure = if is_long {
            self.net_exposure + size as i64
        } else {
            self.net_exposure - size as i64
        };

        // Check exposure limit
        if new_exposure.abs() as u64 > self.max_exposure {
            return false;
        }

        // Check utilization
        let total_notional = self.total_long_size + self.total_short_size + size;
        let max_notional = (self.total_assets as u128 * self.max_utilization as u128 / 10000) as u64;

        total_notional <= max_notional
    }

    /// Update exposure after opening a position
    pub fn add_position(&mut self, size: u64, is_long: bool) {
        if is_long {
            self.net_exposure += size as i64;
            self.total_long_size += size;
        } else {
            self.net_exposure -= size as i64;
            self.total_short_size += size;
        }
    }

    /// Update exposure after closing a position
    pub fn remove_position(&mut self, size: u64, is_long: bool) {
        if is_long {
            self.net_exposure -= size as i64;
            self.total_long_size = self.total_long_size.saturating_sub(size);
        } else {
            self.net_exposure += size as i64;
            self.total_short_size = self.total_short_size.saturating_sub(size);
        }
    }

    /// Settle PnL from a closed position
    pub fn settle_pnl(&mut self, trader_pnl: i64, fee: u64) {
        // Vault PnL is opposite of trader PnL
        let vault_pnl = -trader_pnl;

        if vault_pnl >= 0 {
            // Vault gained (trader lost)
            self.total_assets += vault_pnl as u64;
        } else {
            // Vault lost (trader gained)
            self.total_assets = self.total_assets.saturating_sub((-vault_pnl) as u64);
        }

        // Add fees
        let lp_fee = (fee * self.lp_fee_share as u64) / 10000;
        self.pending_fees += lp_fee;
        self.total_assets += lp_fee;
        self.cumulative_fees += fee;
        self.cumulative_pnl += vault_pnl;
    }
}
