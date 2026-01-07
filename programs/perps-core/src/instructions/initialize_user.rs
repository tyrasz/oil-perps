use anchor_lang::prelude::*;
use crate::state::UserAccount;

#[derive(Accounts)]
pub struct InitializeUser<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,

    #[account(
        init,
        payer = owner,
        space = UserAccount::LEN,
        seeds = [b"user", owner.key().as_ref()],
        bump
    )]
    pub user_account: Account<'info, UserAccount>,

    pub system_program: Program<'info, System>,
}

pub fn handler(ctx: Context<InitializeUser>) -> Result<()> {
    let user_account = &mut ctx.accounts.user_account;
    user_account.owner = ctx.accounts.owner.key();
    user_account.collateral_balance = 0;
    user_account.total_positions = 0;
    user_account.total_trades = 0;
    user_account.realized_pnl = 0;
    user_account.bump = *ctx.bumps.get("user_account").unwrap();

    msg!("User account initialized: {}", user_account.key());
    Ok(())
}
