use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Transfer};
use crate::state::{assert_backed, LpPool, Market, Vault, UserAccount};
use crate::errors::PerpsError;

#[derive(Accounts)]
pub struct WithdrawCollateral<'info> {
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

    // docs/lp-pool.md §4.2 (account 8). Pool state never gates a withdrawal.
    #[account(mut, seeds = [LpPool::SEED, market.key().as_ref()], bump = lp_pool.bump)]
    pub lp_pool: Box<Account<'info, LpPool>>,
}

pub fn handler(ctx: Context<WithdrawCollateral>, amount: u64) -> Result<()> {
    let vt_before = ctx.accounts.vault_token_account.amount;
    let user_account = &mut ctx.accounts.user_account;

    require!(
        user_account.collateral_balance >= amount,
        PerpsError::InsufficientCollateral
    );

    // Create vault signer seeds
    let market_key = ctx.accounts.market.key();
    let vault_bump = ctx.accounts.vault.bump;
    let vault_seeds = &[
        b"vault".as_ref(),
        market_key.as_ref(),
        &[vault_bump],
    ];
    let signer = &[&vault_seeds[..]];

    // Transfer tokens from vault to user
    let cpi_accounts = Transfer {
        from: ctx.accounts.vault_token_account.to_account_info(),
        to: ctx.accounts.user_token_account.to_account_info(),
        authority: ctx.accounts.vault.to_account_info(),
    };
    let cpi_program = ctx.accounts.token_program.key();
    let cpi_ctx = CpiContext::new_with_signer(cpi_program, cpi_accounts, signer);
    token::transfer(cpi_ctx, amount)?;

    // Update user account
    user_account.collateral_balance = user_account.collateral_balance
        .checked_sub(amount)
        .ok_or(PerpsError::InsufficientCollateral)?;
    let pool = &mut ctx.accounts.lp_pool;
    pool.free_collateral = pool.free_collateral.checked_sub(amount).ok_or(PerpsError::BucketUnderflow)?;

    // A statistic now: ADL payouts and stage-1 leftovers credit free
    // collateral from inside the pool (§4.2, red-team F18)
    let vault = &mut ctx.accounts.vault;
    vault.total_deposits = vault.total_deposits.saturating_sub(amount);

    let after = vt_before.checked_sub(amount).ok_or(PerpsError::VaultInvariantViolated)?;
    assert_backed(pool, &ctx.accounts.market, after)?;

    msg!("Withdrew {} collateral for user {}", amount, ctx.accounts.owner.key());
    Ok(())
}
