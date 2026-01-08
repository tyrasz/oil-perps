use anchor_lang::prelude::*;

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Default)]
pub enum QuoteSide {
    #[default]
    Bid,  // MM wants to buy (will go long)
    Ask,  // MM wants to sell (will go short)
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Default)]
pub enum QuoteStatus {
    #[default]
    Active,
    PartiallyFilled,
    Filled,
    Cancelled,
    Expired,
}

/// Active quote from a Market Maker
#[account]
#[derive(Default)]
pub struct Quote {
    /// Market maker who posted this quote
    pub market_maker: Pubkey,

    /// Registry this quote belongs to
    pub registry: Pubkey,

    /// Quote side (bid or ask)
    pub side: QuoteSide,

    /// Quote price (6 decimals)
    pub price: u64,

    /// Original size
    pub original_size: u64,

    /// Remaining size (decreases as fills happen)
    pub remaining_size: u64,

    /// Minimum fill size (to prevent dust attacks)
    pub min_fill_size: u64,

    /// Collateral locked for this quote
    pub collateral_locked: u64,

    /// Quote creation timestamp
    pub created_at: i64,

    /// Quote expiry timestamp (0 = no expiry)
    pub expires_at: i64,

    /// Last update timestamp
    pub updated_at: i64,

    /// Number of partial fills
    pub fill_count: u32,

    /// Current status
    pub status: QuoteStatus,

    /// Quote index (for ordering)
    pub index: u64,

    pub bump: u8,
}

impl Quote {
    pub const LEN: usize = 8 +  // discriminator
        32 +  // market_maker
        32 +  // registry
        1 +   // side
        8 +   // price
        8 +   // original_size
        8 +   // remaining_size
        8 +   // min_fill_size
        8 +   // collateral_locked
        8 +   // created_at
        8 +   // expires_at
        8 +   // updated_at
        4 +   // fill_count
        1 +   // status
        8 +   // index
        1 +   // bump
        32;   // padding

    /// Check if quote is valid for trading
    pub fn is_valid(&self, current_time: i64) -> bool {
        if self.status != QuoteStatus::Active && self.status != QuoteStatus::PartiallyFilled {
            return false;
        }

        if self.remaining_size == 0 {
            return false;
        }

        if self.expires_at > 0 && current_time > self.expires_at {
            return false;
        }

        true
    }

    /// Check if a fill size is valid
    pub fn can_fill(&self, size: u64) -> bool {
        size >= self.min_fill_size && size <= self.remaining_size
    }

    /// Process a fill
    pub fn fill(&mut self, size: u64) {
        self.remaining_size = self.remaining_size.saturating_sub(size);
        self.fill_count += 1;

        if self.remaining_size == 0 {
            self.status = QuoteStatus::Filled;
        } else {
            self.status = QuoteStatus::PartiallyFilled;
        }
    }

    /// Calculate spread from oracle price (basis points)
    pub fn spread_from_oracle(&self, oracle_price: u64) -> u32 {
        let diff = if self.price > oracle_price {
            self.price - oracle_price
        } else {
            oracle_price - self.price
        };

        ((diff as u128 * 10000) / oracle_price as u128) as u32
    }
}

/// Two-sided quote (bid and ask together)
#[account]
#[derive(Default)]
pub struct TwoSidedQuote {
    /// Market maker who posted this quote
    pub market_maker: Pubkey,

    /// Registry this quote belongs to
    pub registry: Pubkey,

    /// Bid price (6 decimals)
    pub bid_price: u64,

    /// Bid size
    pub bid_size: u64,

    /// Bid remaining size
    pub bid_remaining: u64,

    /// Ask price (6 decimals)
    pub ask_price: u64,

    /// Ask size
    pub ask_size: u64,

    /// Ask remaining size
    pub ask_remaining: u64,

    /// Minimum fill size
    pub min_fill_size: u64,

    /// Collateral locked
    pub collateral_locked: u64,

    /// Quote creation timestamp
    pub created_at: i64,

    /// Quote expiry timestamp (0 = no expiry)
    pub expires_at: i64,

    /// Last update timestamp
    pub updated_at: i64,

    /// Whether quote is active
    pub is_active: bool,

    pub bump: u8,
}

impl TwoSidedQuote {
    pub const LEN: usize = 8 +  // discriminator
        32 +  // market_maker
        32 +  // registry
        8 +   // bid_price
        8 +   // bid_size
        8 +   // bid_remaining
        8 +   // ask_price
        8 +   // ask_size
        8 +   // ask_remaining
        8 +   // min_fill_size
        8 +   // collateral_locked
        8 +   // created_at
        8 +   // expires_at
        8 +   // updated_at
        1 +   // is_active
        1 +   // bump
        32;   // padding

    /// Calculate spread (basis points)
    pub fn spread(&self) -> u32 {
        if self.bid_price == 0 {
            return 0;
        }
        ((self.ask_price - self.bid_price) as u128 * 10000 / self.bid_price as u128) as u32
    }

    /// Get mid price
    pub fn mid_price(&self) -> u64 {
        (self.bid_price + self.ask_price) / 2
    }

    /// Check if quote is valid
    pub fn is_valid(&self, current_time: i64) -> bool {
        if !self.is_active {
            return false;
        }

        if self.expires_at > 0 && current_time > self.expires_at {
            return false;
        }

        self.bid_remaining > 0 || self.ask_remaining > 0
    }
}
