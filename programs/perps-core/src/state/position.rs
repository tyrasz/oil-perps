use anchor_lang::prelude::*;

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Default)]
pub enum Side {
    #[default]
    Long,
    Short,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Default)]
pub enum PositionStatus {
    #[default]
    Open,
    Closed,
    Liquidated,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExecutionSource {
    #[default]
    OrderBook,
    Amm,
}

#[account]
#[derive(Default)]
pub struct Position {
    pub owner: Pubkey,
    pub market: Pubkey,

    // Position details
    pub side: Side,
    pub size: u64,                    // Position size in base units (oil contracts)
    pub collateral: u64,              // Deposited collateral in quote units (USDC)
    pub entry_price: u64,             // Average entry price (6 decimals)
    pub leverage: u32,                // Leverage used (3 decimals, 10x = 10_000)

    // PnL tracking
    pub realized_pnl: i64,            // Cumulative realized PnL
    pub last_funding_payment: i64,    // Last funding rate at settlement

    // Timestamps
    pub opened_at: i64,
    pub last_updated_at: i64,

    pub status: PositionStatus,
    pub execution_source: ExecutionSource,
    pub bump: u8,
}

impl Position {
    pub const LEN: usize = 8 +  // discriminator
        32 +  // owner
        32 +  // market
        1 +   // side
        8 +   // size
        8 +   // collateral
        8 +   // entry_price
        4 +   // leverage
        8 +   // realized_pnl
        8 +   // last_funding_payment
        8 +   // opened_at
        8 +   // last_updated_at
        1 +   // status
        1 +   // execution_source
        1 +   // bump
        32;   // padding

    pub fn notional_value(&self) -> u64 {
        // size * entry_price / 1_000_000 (adjust for decimals)
        (self.size as u128)
            .checked_mul(self.entry_price as u128)
            .and_then(|v| v.checked_div(1_000_000))
            .unwrap_or(0) as u64
    }

    pub fn unrealized_pnl(&self, current_price: u64) -> i64 {
        let entry = self.entry_price as i128;
        let current = current_price as i128;
        let size = self.size as i128;

        let price_diff = match self.side {
            Side::Long => current - entry,
            Side::Short => entry - current,
        };

        // PnL = size * price_diff / 1_000_000
        ((size * price_diff) / 1_000_000) as i64
    }

    /// Funding accrued since the position last settled, in collateral units
    /// (USDC, 6 decimals). Positive means the position is owed funding.
    ///
    /// `cumulative_funding_rate` is the market's running sum of per-period
    /// rates (6 decimals, 100 = 0.01%). The payment is that rate applied to the
    /// position's notional at `price`, so it scales with the commodity's price.
    pub fn accrued_funding(&self, cumulative_funding_rate: i64, price: u64) -> i64 {
        let rate_diff = cumulative_funding_rate as i128 - self.last_funding_payment as i128;
        let notional = (self.size as i128) * (price as i128) / 1_000_000;
        let payment = notional * rate_diff / 1_000_000;

        // Positive rate: longs pay shorts.
        let signed = match self.side {
            Side::Long => -payment,
            Side::Short => payment,
        };
        signed.clamp(i64::MIN as i128, i64::MAX as i128) as i64
    }

    /// Equity (collateral + unrealized PnL + accrued funding) as a share of
    /// notional, in basis points.
    pub fn margin_ratio(&self, current_price: u64, accrued_funding: i64) -> u32 {
        let pnl = self.unrealized_pnl(current_price);
        let equity = (self.collateral as i64)
            .saturating_add(pnl)
            .saturating_add(accrued_funding);

        if equity <= 0 {
            return 0;
        }

        let notional = self.notional_value();
        if notional == 0 {
            return 0;
        }

        // margin_ratio = equity / notional * 10000 (basis points)
        let ratio = (equity as u128) * 10_000 / notional as u128;
        ratio.min(u32::MAX as u128) as u32
    }

    pub fn is_liquidatable(
        &self,
        current_price: u64,
        accrued_funding: i64,
        maintenance_margin_ratio: u32,
    ) -> bool {
        self.margin_ratio(current_price, accrued_funding) < maintenance_margin_ratio
    }
}

#[account]
#[derive(Default)]
pub struct UserAccount {
    pub owner: Pubkey,
    pub collateral_balance: u64,      // Free collateral not in positions
    pub total_positions: u32,
    pub total_trades: u64,
    pub realized_pnl: i64,
    pub bump: u8,
}

impl UserAccount {
    pub const LEN: usize = 8 + 32 + 8 + 4 + 8 + 8 + 1 + 16;
}

#[cfg(test)]
mod tests {
    use super::*;

    const USDC: i64 = 1_000_000;

    /// 100 contracts of oil at $75 with $750 collateral (10x).
    fn oil_position(side: Side) -> Position {
        Position {
            side,
            size: 100 * 1_000_000,
            collateral: 750 * USDC as u64,
            entry_price: 75_000_000,
            last_funding_payment: 0,
            ..Default::default()
        }
    }

    #[test]
    fn funding_scales_with_notional_not_contract_count() {
        // 0.01% on $7,500 notional = $0.75
        let long = oil_position(Side::Long);
        assert_eq!(long.accrued_funding(100, 75_000_000), -750_000);
        // Same rate at $150 oil costs twice as much
        assert_eq!(long.accrued_funding(100, 150_000_000), -1_500_000);
    }

    #[test]
    fn funding_sign_depends_on_side_and_rate() {
        assert_eq!(oil_position(Side::Short).accrued_funding(100, 75_000_000), 750_000);
        assert_eq!(oil_position(Side::Long).accrued_funding(-100, 75_000_000), 750_000);
    }

    #[test]
    fn funding_only_counts_since_last_settlement() {
        let mut long = oil_position(Side::Long);
        long.last_funding_payment = 300;
        assert_eq!(long.accrued_funding(400, 75_000_000), -750_000);
        assert_eq!(long.accrued_funding(300, 75_000_000), 0);
    }

    #[test]
    fn margin_ratio_includes_accrued_funding() {
        let long = oil_position(Side::Long);
        // $750 equity on $7,500 notional = 10%
        assert_eq!(long.margin_ratio(75_000_000, 0), 1_000);
        // $400 of unpaid funding leaves $350 = 4.67%
        assert_eq!(long.margin_ratio(75_000_000, -400 * USDC), 466);
        assert!(!long.is_liquidatable(75_000_000, 0, 500));
        assert!(long.is_liquidatable(75_000_000, -400 * USDC, 500));
    }

    #[test]
    fn margin_ratio_is_zero_when_underwater() {
        // $10 drop on 100 contracts = -$1,000 > $750 collateral
        assert_eq!(oil_position(Side::Long).margin_ratio(65_000_000, 0), 0);
    }
}
