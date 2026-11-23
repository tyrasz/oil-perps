use anchor_lang::prelude::*;
use crate::state::{LpPool, Market, Position, UserAccount, PositionStatus};
use crate::errors::PerpsError;

#[derive(Accounts)]
pub struct AddMargin<'info> {
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
        constraint = position.owner == owner.key() @ PerpsError::Unauthorized,
        constraint = position.market == market.key(),
        constraint = position.status == PositionStatus::Open @ PerpsError::PositionAlreadyClosed
    )]
    pub position: Box<Account<'info, Position>>,

    // docs/lp-pool.md §4.3 (account 5)
    #[account(mut, seeds = [LpPool::SEED, market.key().as_ref()], bump = lp_pool.bump)]
    pub lp_pool: Box<Account<'info, LpPool>>,
}

pub fn handler(ctx: Context<AddMargin>, amount: u64) -> Result<()> {
    let user_account = &mut ctx.accounts.user_account;
    let position = &mut ctx.accounts.position;

    require!(
        user_account.collateral_balance >= amount,
        PerpsError::InsufficientCollateral
    );

    // Transfer collateral from user account to position
    user_account.collateral_balance = user_account.collateral_balance
        .checked_sub(amount)
        .ok_or(PerpsError::MathOverflow)?;

    position.collateral = position.collateral
        .checked_add(amount)
        .ok_or(PerpsError::MathOverflow)?;

    // Internal move between buckets (§4.3)
    let pool = &mut ctx.accounts.lp_pool;
    pool.free_collateral = pool.free_collateral.checked_sub(amount).ok_or(PerpsError::BucketUnderflow)?;
    pool.position_collateral = pool.position_collateral.checked_add(amount).ok_or(PerpsError::MathOverflow)?;

    position.last_updated_at = Clock::get()?.unix_timestamp;

    msg!("Added {} margin to position", amount);
    Ok(())
}
