use anchor_lang::prelude::*;
use anchor_spl::token::{Mint, Token};
use crate::state::MmRegistry;
use crate::errors::MmRegistryError;

#[derive(AnchorSerialize, AnchorDeserialize)]
pub struct InitializeRegistryParams {
    pub min_collateral: u64,
    pub max_spread: u32,
    pub min_quote_size: u64,
    pub max_quote_size: u64,
    pub mm_fee: u32,
}

#[derive(Accounts)]
pub struct InitializeRegistry<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(
        init,
        payer = authority,
        space = MmRegistry::LEN,
        seeds = [b"mm_registry", market.key().as_ref()],
        bump
    )]
    pub registry: Account<'info, MmRegistry>,

    /// CHECK: Market account from perps-core
    pub market: AccountInfo<'info>,

    pub collateral_mint: Account<'info, Mint>,

    pub system_program: Program<'info, System>,
    pub token_program: Program<'info, Token>,
}

pub fn handler(ctx: Context<InitializeRegistry>, params: InitializeRegistryParams) -> Result<()> {
    require!(params.min_collateral > 0, MmRegistryError::InvalidQuoteParams);
    require!(params.max_spread > 0 && params.max_spread <= 1000, MmRegistryError::InvalidQuoteParams); // Max 10%
    require!(params.min_quote_size > 0, MmRegistryError::InvalidQuoteParams);
    require!(params.max_quote_size > params.min_quote_size, MmRegistryError::InvalidQuoteParams);
    require!(params.mm_fee <= 100, MmRegistryError::InvalidQuoteParams); // Max 1%

    let registry = &mut ctx.accounts.registry;

    registry.authority = ctx.accounts.authority.key();
    registry.market = ctx.accounts.market.key();
    registry.collateral_mint = ctx.accounts.collateral_mint.key();

    registry.min_collateral = params.min_collateral;
    registry.max_spread = params.max_spread;
    registry.min_quote_size = params.min_quote_size;
    registry.max_quote_size = params.max_quote_size;
    registry.mm_fee = params.mm_fee;

    registry.total_mms = 0;
    registry.active_quotes = 0;
    registry.total_volume = 0;
    registry.total_fees = 0;

    registry.is_open = true;
    registry.is_trading_enabled = true;

    registry.bump = *ctx.bumps.get("registry").unwrap();

    msg!(
        "MM Registry initialized: min_collateral={}, max_spread={}bps",
        params.min_collateral,
        params.max_spread
    );

    Ok(())
}
