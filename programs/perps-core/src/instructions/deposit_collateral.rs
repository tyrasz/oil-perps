use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Transfer};
use crate::errors::PerpsError;
use crate::state::{assert_backed, LpPool, Market, Vault, UserAccount};

#[derive(Accounts)]
pub struct DepositCollateral<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,

    #[account(
        mut,
        seeds = [b"user", owner.key().as_ref(), market.key().as_ref()],
        bump = user_account.bump,
        constraint = user_account.owner == owner.key()
    )]
    pub user_account: Box<Account<'info, UserAccount>>,

    #[account(
        seeds = [b"market", market.collateral_mint.as_ref(), market.commodity_seed()],
        bump = market.bump
    )]
    pub market: Box<Account<'info, Market>>,

    #[account(
        mut,
        seeds = [b"vault", market.key().as_ref()],
        bump = vault.bump
    )]
    pub vault: Box<Account<'info, Vault>>,

    #[account(
        mut,
        seeds = [b"vault_token", market.key().as_ref()],
        bump
    )]
    pub vault_token_account: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        constraint = user_token_account.owner == owner.key(),
        constraint = user_token_account.mint == market.collateral_mint
    )]
    pub user_token_account: Box<Account<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,

    // docs/lp-pool.md §4.1 (account 8)
    #[account(mut, seeds = [LpPool::SEED, market.key().as_ref()], bump = lp_pool.bump)]
    pub lp_pool: Box<Account<'info, LpPool>>,
}

pub fn handler(ctx: Context<DepositCollateral>, amount: u64) -> Result<()> {
    let vt_before = ctx.accounts.vault_token_account.amount;
    // Transfer tokens from user to vault
    let cpi_accounts = Transfer {
        from: ctx.accounts.user_token_account.to_account_info(),
        to: ctx.accounts.vault_token_account.to_account_info(),
        authority: ctx.accounts.owner.to_account_info(),
    };
    let cpi_program = ctx.accounts.token_program.key();
    let cpi_ctx = CpiContext::new(cpi_program, cpi_accounts);
    token::transfer(cpi_ctx, amount)?;

    // Free collateral: the user's balance and the pool's bucket (§4.1)
    let user_account = &mut ctx.accounts.user_account;
    user_account.collateral_balance = user_account.collateral_balance
        .checked_add(amount)
        .ok_or(PerpsError::MathOverflow)?;
    let pool = &mut ctx.accounts.lp_pool;
    pool.free_collateral = pool.free_collateral.checked_add(amount).ok_or(PerpsError::MathOverflow)?;

    // Update vault
    let vault = &mut ctx.accounts.vault;
    vault.total_deposits = vault.total_deposits.checked_add(amount).ok_or(PerpsError::MathOverflow)?;

    let after = vt_before.checked_add(amount).ok_or(PerpsError::MathOverflow)?;
    assert_backed(pool, &ctx.accounts.market, after)?;

    msg!("Deposited {} collateral for user {}", amount, ctx.accounts.owner.key());
    Ok(())
}
