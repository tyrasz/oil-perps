use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Transfer, CloseAccount};
use crate::state::{MmRegistry, MarketMaker};
use crate::errors::MmRegistryError;

#[derive(Accounts)]
pub struct DeregisterMm<'info> {
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
        constraint = market_maker.owner == owner.key() @ MmRegistryError::Unauthorized,
        close = owner
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

pub fn handler(ctx: Context<DeregisterMm>) -> Result<()> {
    let market_maker = &ctx.accounts.market_maker;

    // Can't deregister with active quotes
    require!(market_maker.active_quotes == 0, MmRegistryError::HasActiveQuotes);

    // Can't deregister with open inventory
    require!(market_maker.inventory == 0, MmRegistryError::HasOpenInventory);

    // Transfer all remaining collateral back to owner
    let balance = ctx.accounts.mm_collateral_account.amount;

    if balance > 0 {
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
        let cpi_ctx = CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            cpi_accounts,
            signer
        );
        token::transfer(cpi_ctx, balance)?;

        // Close the token account
        let close_accounts = CloseAccount {
            account: ctx.accounts.mm_collateral_account.to_account_info(),
            destination: ctx.accounts.owner.to_account_info(),
            authority: ctx.accounts.mm_collateral_account.to_account_info(),
        };
        let close_ctx = CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            close_accounts,
            signer
        );
        token::close_account(close_ctx)?;
    }

    // Update registry
    let registry = &mut ctx.accounts.registry;
    registry.total_mms = registry.total_mms.saturating_sub(1);

    msg!(
        "Market Maker deregistered: owner={}, final_balance={}",
        ctx.accounts.owner.key(),
        balance
    );

    Ok(())
}
