use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Transfer};
use crate::state::{MmRegistry, MarketMaker, MmStatus};
use crate::errors::MmRegistryError;

#[derive(Accounts)]
pub struct DepositCollateral<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,

    #[account(
        seeds = [b"mm_registry", registry.market.as_ref()],
        bump = registry.bump
    )]
    pub registry: Account<'info, MmRegistry>,

    #[account(
        mut,
        seeds = [b"market_maker", registry.key().as_ref(), owner.key().as_ref()],
        bump = market_maker.bump,
        constraint = market_maker.owner == owner.key() @ MmRegistryError::Unauthorized,
        constraint = market_maker.status != MmStatus::Deregistered @ MmRegistryError::MarketMakerNotActive
    )]
    pub market_maker: Account<'info, MarketMaker>,

    #[account(
        mut,
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

    pub token_program: Program<'info, Token>,
}

pub fn handler(ctx: Context<DepositCollateral>, amount: u64) -> Result<()> {
    require!(amount > 0, MmRegistryError::InvalidQuoteParams);

    // Transfer collateral
    let cpi_accounts = Transfer {
        from: ctx.accounts.owner_token_account.to_account_info(),
        to: ctx.accounts.mm_collateral_account.to_account_info(),
        authority: ctx.accounts.owner.to_account_info(),
    };
    let cpi_program = ctx.accounts.token_program.to_account_info();
    let cpi_ctx = CpiContext::new(cpi_program, cpi_accounts);
    token::transfer(cpi_ctx, amount)?;

    // Update MM account
    let market_maker = &mut ctx.accounts.market_maker;
    market_maker.collateral_deposited += amount;
    market_maker.collateral_available += amount;
    market_maker.last_active_at = Clock::get()?.unix_timestamp;

    msg!("Deposited {} collateral", amount);

    Ok(())
}
