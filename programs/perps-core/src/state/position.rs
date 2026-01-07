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

    pub fn margin_ratio(&self, current_price: u64) -> u32 {
        let pnl = self.unrealized_pnl(current_price);
        let equity = (self.collateral as i64).saturating_add(pnl);

        if equity <= 0 {
            return 0;
        }

        let notional = self.notional_value();
        if notional == 0 {
            return 0;
        }

        // margin_ratio = equity / notional * 10000 (basis points)
        ((equity as u64) * 10000 / notional) as u32
    }

    pub fn is_liquidatable(&self, current_price: u64, maintenance_margin_ratio: u32) -> bool {
        self.margin_ratio(current_price) < maintenance_margin_ratio
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
