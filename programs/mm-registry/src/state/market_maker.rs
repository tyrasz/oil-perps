use anchor_lang::prelude::*;

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Default)]
pub enum MmStatus {
    #[default]
    Inactive,
    Active,
    Suspended,
    Deregistered,
}

/// Individual Market Maker account
#[account]
#[derive(Default)]
pub struct MarketMaker {
    /// Owner/operator of this MM account
    pub owner: Pubkey,

    /// Registry this MM belongs to
    pub registry: Pubkey,

    /// MM's collateral token account
    pub collateral_account: Pubkey,

    /// Total collateral deposited
    pub collateral_deposited: u64,

    /// Collateral locked in active quotes
    pub collateral_locked: u64,

    /// Available collateral (deposited - locked)
    pub collateral_available: u64,

    /// Current inventory position (positive = long, negative = short)
    pub inventory: i64,

    /// Average entry price of inventory
    pub avg_inventory_price: u64,

    /// Unrealized PnL from inventory
    pub unrealized_pnl: i64,

    /// Realized PnL (historical)
    pub realized_pnl: i64,

    /// Total volume traded
    pub total_volume: u64,

    /// Total number of fills
    pub total_fills: u64,

    /// Total fees paid
    pub total_fees_paid: u64,

    /// Registration timestamp
    pub registered_at: i64,

    /// Last activity timestamp
    pub last_active_at: i64,

    /// Number of active quotes
    pub active_quotes: u8,

    /// Maximum allowed active quotes
    pub max_quotes: u8,

    /// Current status
    pub status: MmStatus,

    pub bump: u8,
}

impl MarketMaker {
    pub const LEN: usize = 8 +  // discriminator
        32 +  // owner
        32 +  // registry
        32 +  // collateral_account
        8 +   // collateral_deposited
        8 +   // collateral_locked
        8 +   // collateral_available
        8 +   // inventory
        8 +   // avg_inventory_price
        8 +   // unrealized_pnl
        8 +   // realized_pnl
        8 +   // total_volume
        8 +   // total_fills
        8 +   // total_fees_paid
        8 +   // registered_at
        8 +   // last_active_at
        1 +   // active_quotes
        1 +   // max_quotes
        1 +   // status
        1 +   // bump
        64;   // padding

    /// Check if MM has enough available collateral
    pub fn has_available_collateral(&self, amount: u64) -> bool {
        self.collateral_available >= amount
    }

    /// Calculate margin requirement for a quote
    pub fn margin_for_quote(&self, size: u64, price: u64) -> u64 {
        // 10% margin requirement for quotes
        let notional = (size as u128 * price as u128 / 1_000_000) as u64;
        notional / 10
    }

    /// Lock collateral for a quote
    pub fn lock_collateral(&mut self, amount: u64) {
        self.collateral_locked += amount;
        self.collateral_available = self.collateral_deposited.saturating_sub(self.collateral_locked);
    }

    /// Unlock collateral when quote is cancelled/filled
    pub fn unlock_collateral(&mut self, amount: u64) {
        self.collateral_locked = self.collateral_locked.saturating_sub(amount);
        self.collateral_available = self.collateral_deposited.saturating_sub(self.collateral_locked);
    }

    /// Update inventory after a fill
    pub fn update_inventory(&mut self, size: i64, price: u64, is_buy: bool) {
        let signed_size = if is_buy { size } else { -size };

        if self.inventory == 0 {
            // Fresh position
            self.inventory = signed_size;
            self.avg_inventory_price = price;
        } else if (self.inventory > 0 && signed_size > 0) || (self.inventory < 0 && signed_size < 0) {
            // Adding to position - calculate new average price
            let old_value = (self.inventory.abs() as u128) * (self.avg_inventory_price as u128);
            let new_value = (signed_size.abs() as u128) * (price as u128);
            let total_size = self.inventory.abs() + signed_size.abs();
            self.avg_inventory_price = ((old_value + new_value) / total_size as u128) as u64;
            self.inventory += signed_size;
        } else {
            // Reducing position - realize PnL
            let close_size = signed_size.abs().min(self.inventory.abs());
            let pnl = if self.inventory > 0 {
                // Was long, selling
                ((price as i128 - self.avg_inventory_price as i128) * close_size as i128 / 1_000_000) as i64
            } else {
                // Was short, buying
                ((self.avg_inventory_price as i128 - price as i128) * close_size as i128 / 1_000_000) as i64
            };

            self.realized_pnl += pnl;
            self.inventory += signed_size;

            // If flipped sides, set new avg price
            if (self.inventory > 0 && signed_size > 0) || (self.inventory < 0 && signed_size < 0) {
                self.avg_inventory_price = price;
            }
        }
    }

    /// Calculate unrealized PnL at current price
    pub fn calculate_unrealized_pnl(&self, current_price: u64) -> i64 {
        if self.inventory == 0 {
            return 0;
        }

        let pnl = if self.inventory > 0 {
            // Long position
            ((current_price as i128 - self.avg_inventory_price as i128) * self.inventory as i128 / 1_000_000) as i64
        } else {
            // Short position
            ((self.avg_inventory_price as i128 - current_price as i128) * (-self.inventory) as i128 / 1_000_000) as i64
        };

        pnl
    }
}
