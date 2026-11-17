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

impl Market {
    /// Collateral to post for a position of `notional` at `leverage`
    /// (3 decimals, 10x = 10_000): `notional / leverage`, rounded up.
    /// Returns `None` if that is below the initial margin requirement, i.e.
    /// the leverage is higher than the market's initial margin allows.
    pub fn required_collateral(&self, notional: u64, leverage: u32) -> Option<u64> {
        if leverage == 0 {
            return None;
        }
        let collateral = ceil_div(notional as u128 * 1_000, leverage as u128);
        let initial_margin = ceil_div(notional as u128 * self.initial_margin_ratio as u128, 10_000);
        if collateral < initial_margin {
            return None;
        }
        u64::try_from(collateral).ok()
    }

    /// Liquidator reward: `liquidation_fee` applied to the position's
    /// maintenance margin. Basing it on the margin requirement rather than on
    /// whatever collateral is left keeps the reward from falling to zero on
    /// positions that are already underwater, which are the ones that most
    /// need liquidating.
    pub fn liquidation_reward_target(&self, notional: u64) -> u64 {
        let reward = (notional as u128)
            * self.maintenance_margin_ratio as u128
            * self.liquidation_fee as u128
            / 100_000_000;
        reward.min(u64::MAX as u128) as u64
    }

    /// Settle a liquidation against the insurance fund.
    /// Returns `(liquidator_reward, uncovered_bad_debt)`.
    pub fn settle_liquidation(&mut self, remaining_collateral: u64, deficit: u64, notional: u64) -> (u64, u64) {
        let target = self.liquidation_reward_target(notional);
        let (reward, fund, uncovered) =
            liquidation_payout(self.insurance_fund, remaining_collateral, deficit, target);
        self.insurance_fund = fund;
        (reward, uncovered)
    }
}

/// Split a liquidated position between the liquidator and the insurance fund.
///
/// The reward is paid from the position's remaining collateral first; any
/// shortfall is topped up from the insurance fund (as far as it can). Leftover
/// collateral goes to the fund, which then absorbs any bad debt.
/// Returns `(reward, new_fund, uncovered_bad_debt)`.
pub fn liquidation_payout(fund: u64, remaining_collateral: u64, deficit: u64, reward_target: u64) -> (u64, u64, u64) {
    let from_collateral = reward_target.min(remaining_collateral);
    let fund = fund.saturating_add(remaining_collateral - from_collateral);
    let top_up = (reward_target - from_collateral).min(fund);
    let (fund, uncovered) = apply_insurance(fund - top_up, 0, deficit);
    (from_collateral + top_up, fund, uncovered)
}

fn ceil_div(a: u128, b: u128) -> u128 {
    (a + b - 1) / b
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

    /// OIL as configured by scripts/initialize-market.ts.
    fn oil_market() -> Market {
        Market {
            max_leverage: 20_000,
            initial_margin_ratio: 1_000,
            maintenance_margin_ratio: 500,
            liquidation_fee: 100,
            ..Default::default()
        }
    }

    const NOTIONAL: u64 = 7_500_000_000; // $7,500

    #[test]
    fn collateral_follows_chosen_leverage() {
        let m = oil_market();
        assert_eq!(m.required_collateral(NOTIONAL, 2_000), Some(3_750_000_000)); // 2x
        assert_eq!(m.required_collateral(NOTIONAL, 5_000), Some(1_500_000_000)); // 5x
        assert_eq!(m.required_collateral(NOTIONAL, 10_000), Some(750_000_000)); // 10x
    }

    #[test]
    fn leverage_above_initial_margin_is_rejected() {
        // 10% initial margin caps leverage at 10x even though max_leverage is 20x
        let m = oil_market();
        assert_eq!(m.required_collateral(NOTIONAL, 10_001), None);
        assert_eq!(m.required_collateral(NOTIONAL, 20_000), None);
        assert_eq!(m.required_collateral(NOTIONAL, 0), None);
    }

    #[test]
    fn collateral_rounds_up() {
        // $1 at 3x = 0.333334
        let m = Market { initial_margin_ratio: 1_000, ..Default::default() };
        assert_eq!(m.required_collateral(1_000_000, 3_000), Some(333_334));
    }

    #[test]
    fn liquidation_reward_is_share_of_maintenance_margin() {
        // 1% of the 5% maintenance margin on $7,500 = $3.75
        assert_eq!(oil_market().liquidation_reward_target(NOTIONAL), 3_750_000);
    }

    #[test]
    fn liquidation_reward_comes_from_collateral_when_available() {
        // $40 left, $3 reward: liquidator gets $3, fund gets the other $37
        assert_eq!(liquidation_payout(100, 40, 0, 3), (3, 137, 0));
    }

    #[test]
    fn underwater_liquidation_still_pays_reward_from_fund() {
        // Nothing left and $25 of bad debt: fund pays the $3 reward and the debt
        assert_eq!(liquidation_payout(100, 0, 25, 3), (3, 72, 0));
    }

    #[test]
    fn partial_collateral_is_topped_up_from_fund() {
        assert_eq!(liquidation_payout(100, 1, 0, 3), (3, 98, 0));
    }

    #[test]
    fn empty_fund_limits_reward_and_reports_bad_debt() {
        assert_eq!(liquidation_payout(0, 0, 25, 3), (0, 0, 25));
        assert_eq!(liquidation_payout(2, 0, 25, 3), (2, 0, 25));
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
