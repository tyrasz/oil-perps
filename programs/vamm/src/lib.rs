use anchor_lang::prelude::*;

declare_id!("3Vft6oJxPvHXELuqrPgCdPqJf1LxGpqDxBdqNhJxLZbm");

#[account]
pub struct Vamm {
    pub market: Pubkey,
    pub authority: Pubkey,

    // Virtual reserves (x * y = k)
    pub base_reserve: u128,     // Virtual oil amount
    pub quote_reserve: u128,    // Virtual USDC amount
    pub k: u128,                // Constant product

    // Configuration
    pub base_spread: u32,       // Spread in basis points (e.g., 10 = 0.1%)
    pub max_spread: u32,        // Maximum spread during high utilization
    pub utilization_threshold: u32,  // Threshold for spread increase

    // Stats
    pub total_long: u64,
    pub total_short: u64,
    pub total_volume: u128,

    pub bump: u8,
    pub is_active: bool,
}

impl Vamm {
    pub const LEN: usize = 8 + 32 + 32 + 16 + 16 + 16 + 4 + 4 + 4 + 8 + 8 + 16 + 1 + 1 + 32;

    pub fn get_price(&self) -> u64 {
        // price = quote_reserve / base_reserve
        ((self.quote_reserve * 1_000_000) / self.base_reserve) as u64
    }

    pub fn get_spread(&self) -> u32 {
        let total_oi = self.total_long + self.total_short;
        let capacity = (self.base_reserve / 10) as u64; // 10% of base reserve

        if total_oi > capacity {
            let utilization = (total_oi * 10000) / capacity;
            let extra_spread = ((utilization - 10000) * (self.max_spread - self.base_spread) as u64 / 10000) as u32;
            (self.base_spread + extra_spread).min(self.max_spread)
        } else {
            self.base_spread
        }
    }

    pub fn get_bid_price(&self) -> u64 {
        let price = self.get_price();
        let spread = self.get_spread();
        price - (price * spread as u64 / 10000)
    }

    pub fn get_ask_price(&self) -> u64 {
        let price = self.get_price();
        let spread = self.get_spread();
        price + (price * spread as u64 / 10000)
    }

    pub fn swap_base_to_quote(&mut self, base_amount: u64) -> u64 {
        // Selling base (closing long / opening short)
        let new_base_reserve = self.base_reserve + base_amount as u128;
        let new_quote_reserve = self.k / new_base_reserve;
        let quote_out = self.quote_reserve - new_quote_reserve;

        self.base_reserve = new_base_reserve;
        self.quote_reserve = new_quote_reserve;

        quote_out as u64
    }

    pub fn swap_quote_to_base(&mut self, quote_amount: u64) -> u64 {
        // Buying base (opening long / closing short)
        let new_quote_reserve = self.quote_reserve + quote_amount as u128;
        let new_base_reserve = self.k / new_quote_reserve;
        let base_out = self.base_reserve - new_base_reserve;

        self.quote_reserve = new_quote_reserve;
        self.base_reserve = new_base_reserve;

        base_out as u64
    }
}

#[program]
pub mod vamm {
    use super::*;

    pub fn initialize_vamm(
        ctx: Context<InitializeVamm>,
        base_reserve: u128,
        quote_reserve: u128,
        base_spread: u32,
        max_spread: u32,
    ) -> Result<()> {
        let vamm = &mut ctx.accounts.vamm;

        vamm.market = ctx.accounts.market.key();
        vamm.authority = ctx.accounts.authority.key();
        vamm.base_reserve = base_reserve;
        vamm.quote_reserve = quote_reserve;
        vamm.k = base_reserve * quote_reserve;
        vamm.base_spread = base_spread;
        vamm.max_spread = max_spread;
        vamm.utilization_threshold = 8000; // 80%
        vamm.total_long = 0;
        vamm.total_short = 0;
        vamm.total_volume = 0;
        vamm.is_active = true;
        vamm.bump = *ctx.bumps.get("vamm").unwrap();

        msg!("vAMM initialized: k={}, price={}", vamm.k, vamm.get_price());
        Ok(())
    }

    pub fn open_long(ctx: Context<Trade>, size: u64) -> Result<()> {
        let vamm = &mut ctx.accounts.vamm;
        require!(vamm.is_active, ErrorCode::VammPaused);

        let quote_amount = vamm.swap_quote_to_base(size);
        vamm.total_long += size;
        vamm.total_volume += size as u128;

        msg!("Opened long: size={}, quote_in={}, new_price={}", size, quote_amount, vamm.get_price());
        Ok(())
    }

    pub fn open_short(ctx: Context<Trade>, size: u64) -> Result<()> {
        let vamm = &mut ctx.accounts.vamm;
        require!(vamm.is_active, ErrorCode::VammPaused);

        let quote_amount = vamm.swap_base_to_quote(size);
        vamm.total_short += size;
        vamm.total_volume += size as u128;

        msg!("Opened short: size={}, quote_out={}, new_price={}", size, quote_amount, vamm.get_price());
        Ok(())
    }

    pub fn close_long(ctx: Context<Trade>, size: u64) -> Result<()> {
        let vamm = &mut ctx.accounts.vamm;

        let quote_amount = vamm.swap_base_to_quote(size);
        vamm.total_long = vamm.total_long.saturating_sub(size);

        msg!("Closed long: size={}, quote_out={}, new_price={}", size, quote_amount, vamm.get_price());
        Ok(())
    }

    pub fn close_short(ctx: Context<Trade>, size: u64) -> Result<()> {
        let vamm = &mut ctx.accounts.vamm;

        let quote_amount = vamm.swap_quote_to_base(size);
        vamm.total_short = vamm.total_short.saturating_sub(size);

        msg!("Closed short: size={}, quote_in={}, new_price={}", size, quote_amount, vamm.get_price());
        Ok(())
    }

    pub fn add_liquidity(ctx: Context<ManageLiquidity>, base_amount: u128, quote_amount: u128) -> Result<()> {
        let vamm = &mut ctx.accounts.vamm;

        // Maintain price ratio
        let current_price = vamm.get_price();
        let expected_quote = (base_amount * current_price as u128) / 1_000_000;

        require!(
            quote_amount >= expected_quote * 99 / 100 && quote_amount <= expected_quote * 101 / 100,
            ErrorCode::InvalidLiquidityRatio
        );

        vamm.base_reserve += base_amount;
        vamm.quote_reserve += quote_amount;
        vamm.k = vamm.base_reserve * vamm.quote_reserve;

        msg!("Liquidity added: new_k={}", vamm.k);
        Ok(())
    }

    pub fn pause(ctx: Context<ManageVamm>) -> Result<()> {
        ctx.accounts.vamm.is_active = false;
        msg!("vAMM paused");
        Ok(())
    }

    pub fn unpause(ctx: Context<ManageVamm>) -> Result<()> {
        ctx.accounts.vamm.is_active = true;
        msg!("vAMM unpaused");
        Ok(())
    }
}

#[derive(Accounts)]
pub struct InitializeVamm<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    /// CHECK: Market account from perps-core
    pub market: AccountInfo<'info>,

    #[account(
        init,
        payer = authority,
        space = Vamm::LEN,
        seeds = [b"vamm", market.key().as_ref()],
        bump
    )]
    pub vamm: Account<'info, Vamm>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Trade<'info> {
    pub trader: Signer<'info>,

    #[account(
        mut,
        seeds = [b"vamm", vamm.market.as_ref()],
        bump = vamm.bump
    )]
    pub vamm: Account<'info, Vamm>,
}

#[derive(Accounts)]
pub struct ManageLiquidity<'info> {
    #[account(constraint = authority.key() == vamm.authority)]
    pub authority: Signer<'info>,

    #[account(
        mut,
        seeds = [b"vamm", vamm.market.as_ref()],
        bump = vamm.bump
    )]
    pub vamm: Account<'info, Vamm>,
}

#[derive(Accounts)]
pub struct ManageVamm<'info> {
    #[account(constraint = authority.key() == vamm.authority)]
    pub authority: Signer<'info>,

    #[account(
        mut,
        seeds = [b"vamm", vamm.market.as_ref()],
        bump = vamm.bump
    )]
    pub vamm: Account<'info, Vamm>,
}

#[error_code]
pub enum ErrorCode {
    #[msg("vAMM is paused")]
    VammPaused,
    #[msg("Invalid liquidity ratio")]
    InvalidLiquidityRatio,
}
