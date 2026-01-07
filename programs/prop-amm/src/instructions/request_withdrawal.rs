use anchor_lang::prelude::*;
use crate::state::{LpVault, LpPosition};
use crate::errors::PropAmmError;

#[derive(Accounts)]
pub struct RequestWithdrawal<'info> {
    pub owner: Signer<'info>,

    #[account(
        seeds = [b"lp_vault", vault.collateral_mint.as_ref()],
        bump = vault.bump
    )]
    pub vault: Account<'info, LpVault>,

    #[account(
        mut,
        seeds = [b"lp_position", owner.key().as_ref(), vault.key().as_ref()],
        bump = lp_position.bump,
        constraint = lp_position.owner == owner.key() @ PropAmmError::Unauthorized
    )]
    pub lp_position: Account<'info, LpPosition>,
}

pub fn handler(ctx: Context<RequestWithdrawal>) -> Result<()> {
    let lp_position = &mut ctx.accounts.lp_position;

    require!(lp_position.shares > 0, PropAmmError::InsufficientShares);
    require!(
        lp_position.withdrawal_requested_at == 0,
        PropAmmError::WithdrawalAlreadyRequested
    );

    let current_time = Clock::get()?.unix_timestamp;
    lp_position.withdrawal_requested_at = current_time;

    msg!(
        "Withdrawal requested for {} shares. Can withdraw after {}",
        lp_position.shares,
        current_time + ctx.accounts.vault.withdrawal_delay
    );

    Ok(())
}
