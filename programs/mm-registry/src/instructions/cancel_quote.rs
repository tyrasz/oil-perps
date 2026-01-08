use anchor_lang::prelude::*;
use crate::state::{MmRegistry, MarketMaker, TwoSidedQuote};
use crate::errors::MmRegistryError;

#[derive(Accounts)]
pub struct CancelQuote<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,

    #[account(
        mut,
        seeds = [b"mm_registry", registry.market.as_ref()],
        bump = registry.bump
    )]
    pub registry: Account<'info, MmRegistry>,

    #[account(
        mut,
        seeds = [b"market_maker", registry.key().as_ref(), owner.key().as_ref()],
        bump = market_maker.bump,
        constraint = market_maker.owner == owner.key() @ MmRegistryError::Unauthorized
    )]
    pub market_maker: Account<'info, MarketMaker>,

    #[account(
        mut,
        constraint = quote.market_maker == market_maker.key() @ MmRegistryError::Unauthorized,
        constraint = quote.is_active @ MmRegistryError::QuoteNotActive,
        close = owner
    )]
    pub quote: Account<'info, TwoSidedQuote>,
}

pub fn handler(ctx: Context<CancelQuote>) -> Result<()> {
    let quote = &ctx.accounts.quote;
    let collateral_to_unlock = quote.collateral_locked;

    // Update market maker
    let market_maker = &mut ctx.accounts.market_maker;
    market_maker.unlock_collateral(collateral_to_unlock);
    market_maker.active_quotes = market_maker.active_quotes.saturating_sub(1);
    market_maker.last_active_at = Clock::get()?.unix_timestamp;

    // Update registry
    let registry = &mut ctx.accounts.registry;
    registry.active_quotes = registry.active_quotes.saturating_sub(1);

    msg!("Quote cancelled, unlocked {} collateral", collateral_to_unlock);

    Ok(())
}
