use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Transfer};
use crate::state::{MmRegistry, MarketMaker, MmStatus};
use crate::errors::MmRegistryError;

#[derive(Accounts)]
pub struct RegisterMm<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,

    #[account(
        mut,
        seeds = [b"mm_registry", registry.market.as_ref()],
        bump = registry.bump,
        constraint = registry.is_open @ MmRegistryError::RegistryClosed
    )]
    pub registry: Account<'info, MmRegistry>,

    #[account(
        init,
        payer = owner,
        space = MarketMaker::LEN,
        seeds = [b"market_maker", registry.key().as_ref(), owner.key().as_ref()],
        bump
    )]
    pub market_maker: Account<'info, MarketMaker>,

    #[account(
        init,
        payer = owner,
        token::mint = collateral_mint,
        token::authority = market_maker,
        seeds = [b"mm_collateral", market_maker.key().as_ref()],
        bump
    )]
    pub mm_collateral_account: Account<'info, TokenAccount>,

    #[account(
        mut,
        constraint = owner_token_account.owner == owner.key(),
        constraint = owner_token_account.mint == registry.collateral_mint
    )]
    pub owner_token_account: Account<'info, TokenAccount>,

    #[account(constraint = collateral_mint.key() == registry.collateral_mint)]
    pub collateral_mint: Account<'info, anchor_spl::token::Mint>,

    pub system_program: Program<'info, System>,
    pub token_program: Program<'info, Token>,
    pub rent: Sysvar<'info, Rent>,
}

pub fn handler(ctx: Context<RegisterMm>, initial_collateral: u64) -> Result<()> {
    let registry = &ctx.accounts.registry;

    require!(
        initial_collateral >= registry.min_collateral,
        MmRegistryError::InsufficientCollateral
    );

    // Transfer initial collateral
    let cpi_accounts = Transfer {
        from: ctx.accounts.owner_token_account.to_account_info(),
        to: ctx.accounts.mm_collateral_account.to_account_info(),
        authority: ctx.accounts.owner.to_account_info(),
    };
    let cpi_program = ctx.accounts.token_program.to_account_info();
    let cpi_ctx = CpiContext::new(cpi_program, cpi_accounts);
    token::transfer(cpi_ctx, initial_collateral)?;

    // Initialize market maker account
    let market_maker = &mut ctx.accounts.market_maker;
    let current_time = Clock::get()?.unix_timestamp;

    market_maker.owner = ctx.accounts.owner.key();
    market_maker.registry = ctx.accounts.registry.key();
    market_maker.collateral_account = ctx.accounts.mm_collateral_account.key();

    market_maker.collateral_deposited = initial_collateral;
    market_maker.collateral_locked = 0;
    market_maker.collateral_available = initial_collateral;

    market_maker.inventory = 0;
    market_maker.avg_inventory_price = 0;
    market_maker.unrealized_pnl = 0;
    market_maker.realized_pnl = 0;

    market_maker.total_volume = 0;
    market_maker.total_fills = 0;
    market_maker.total_fees_paid = 0;

    market_maker.registered_at = current_time;
    market_maker.last_active_at = current_time;

    market_maker.active_quotes = 0;
    market_maker.max_quotes = 10; // Default max quotes per MM

    market_maker.status = MmStatus::Active;
    market_maker.bump = *ctx.bumps.get("market_maker").unwrap();

    // Update registry stats
    let registry = &mut ctx.accounts.registry;
    registry.total_mms += 1;

    msg!(
        "Market Maker registered: owner={}, collateral={}",
        ctx.accounts.owner.key(),
        initial_collateral
    );

    Ok(())
}
