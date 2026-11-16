use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Transfer};
use crate::state::{MmRegistry, MarketMaker, MmStatus};
use crate::errors::MmRegistryError;

#[derive(Accounts)]
pub struct WithdrawCollateral<'info> {
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
        constraint = market_maker.owner == owner.key() @ MmRegistryError::Unauthorized
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

pub fn handler(ctx: Context<WithdrawCollateral>, amount: u64) -> Result<()> {
    let market_maker = &ctx.accounts.market_maker;

    require!(amount > 0, MmRegistryError::InvalidQuoteParams);
    require!(
        amount <= market_maker.collateral_available,
        MmRegistryError::CollateralLocked
    );

    // Ensure minimum collateral remains if MM is still active
    if market_maker.status == MmStatus::Active {
        let remaining = market_maker.collateral_deposited - amount;
        require!(
            remaining >= ctx.accounts.registry.min_collateral,
            MmRegistryError::InsufficientCollateral
        );
    }

    // Transfer collateral back to owner
    let market_maker_key = ctx.accounts.market_maker.key();
    let seeds = &[
        b"mm_collateral".as_ref(),
        market_maker_key.as_ref(),
        &[*ctx.bumps.get("mm_collateral_account").unwrap()],
    ];
    let signer = &[&seeds[..]];

    let cpi_accounts = Transfer {
        from: ctx.accounts.mm_collateral_account.to_account_info(),
        to: ctx.accounts.owner_token_account.to_account_info(),
        authority: ctx.accounts.mm_collateral_account.to_account_info(),
    };
    let cpi_program = ctx.accounts.token_program.to_account_info();
    let cpi_ctx = CpiContext::new_with_signer(cpi_program, cpi_accounts, signer);
    token::transfer(cpi_ctx, amount)?;

    // Update MM account
    let market_maker = &mut ctx.accounts.market_maker;
    market_maker.collateral_deposited -= amount;
    market_maker.collateral_available -= amount;
    market_maker.last_active_at = Clock::get()?.unix_timestamp;

    msg!("Withdrew {} collateral", amount);

    Ok(())
}
