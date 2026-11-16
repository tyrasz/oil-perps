use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Transfer};
use crate::state::{LpVault, LpPosition};
use crate::errors::PropAmmError;

#[derive(Accounts)]
pub struct Withdraw<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,

    #[account(
        mut,
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

    #[account(
        mut,
        seeds = [b"vault_token", vault.key().as_ref()],
        bump
    )]
    pub vault_token_account: Account<'info, TokenAccount>,

    #[account(
        mut,
        constraint = owner_token_account.owner == owner.key(),
        constraint = owner_token_account.mint == vault.collateral_mint
    )]
    pub owner_token_account: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
}

pub fn handler(ctx: Context<Withdraw>, shares_to_burn: u64) -> Result<()> {
    let vault = &ctx.accounts.vault;
    let lp_position = &ctx.accounts.lp_position;
    let current_time = Clock::get()?.unix_timestamp;

    // Validate withdrawal
    require!(
        lp_position.can_withdraw(vault.withdrawal_delay, current_time),
        PropAmmError::WithdrawalDelayNotElapsed
    );
    require!(
        shares_to_burn <= lp_position.shares,
        PropAmmError::InsufficientShares
    );
    require!(shares_to_burn > 0, PropAmmError::InvalidParameters);

    // Calculate assets to return
    let assets_to_return = vault.assets_for_shares(shares_to_burn);
    require!(
        assets_to_return <= vault.total_assets,
        PropAmmError::InsufficientVaultBalance
    );

    // Transfer tokens from vault to owner
    let collateral_mint = vault.collateral_mint;
    let vault_bump = vault.bump;
    let vault_seeds = &[
        b"lp_vault".as_ref(),
        collateral_mint.as_ref(),
        &[vault_bump],
    ];
    let signer = &[&vault_seeds[..]];

    let cpi_accounts = Transfer {
        from: ctx.accounts.vault_token_account.to_account_info(),
        to: ctx.accounts.owner_token_account.to_account_info(),
        authority: ctx.accounts.vault.to_account_info(),
    };
    let cpi_program = ctx.accounts.token_program.to_account_info();
    let cpi_ctx = CpiContext::new_with_signer(cpi_program, cpi_accounts, signer);
    token::transfer(cpi_ctx, assets_to_return)?;

    // Update vault state
    let vault = &mut ctx.accounts.vault;
    vault.total_assets -= assets_to_return;
    vault.total_shares -= shares_to_burn;

    // Update LP position
    let lp_position = &mut ctx.accounts.lp_position;
    lp_position.shares -= shares_to_burn;
    lp_position.withdrawal_requested_at = 0;

    // Adjust deposited_amount proportionally
    let burn_ratio = (shares_to_burn as u128 * 1_000_000) / (lp_position.shares + shares_to_burn) as u128;
    let deposited_reduction = (lp_position.deposited_amount as u128 * burn_ratio / 1_000_000) as u64;
    lp_position.deposited_amount = lp_position.deposited_amount.saturating_sub(deposited_reduction);

    let pnl = assets_to_return as i64 - deposited_reduction as i64;

    msg!(
        "Withdrew {} USDC for {} shares. PnL: {}. Remaining shares: {}",
        assets_to_return,
        shares_to_burn,
        pnl,
        lp_position.shares
    );

    Ok(())
}
