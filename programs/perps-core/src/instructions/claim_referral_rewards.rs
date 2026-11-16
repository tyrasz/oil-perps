use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Transfer};
use crate::state::{Market, ReferralCode, Vault};
use crate::errors::PerpsError;

#[derive(Accounts)]
pub struct ClaimReferralRewards<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,

    #[account(
        mut,
        seeds = [b"referral_code", referral_code.code.as_ref()],
        bump = referral_code.bump,
        constraint = referral_code.owner == owner.key() @ PerpsError::Unauthorized
    )]
    pub referral_code: Account<'info, ReferralCode>,

    #[account(
        seeds = [b"market", market.collateral_mint.as_ref()],
        bump = market.bump
    )]
    pub market: Account<'info, Market>,

    #[account(
        mut,
        seeds = [b"vault", market.key().as_ref()],
        bump = vault.bump
    )]
    pub vault: Account<'info, Vault>,

    #[account(
        mut,
        seeds = [b"vault_token", market.key().as_ref()],
        bump
    )]
    pub vault_token_account: Account<'info, TokenAccount>,

    #[account(
        mut,
        constraint = owner_token_account.owner == owner.key(),
        constraint = owner_token_account.mint == market.collateral_mint
    )]
    pub owner_token_account: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
}

pub fn handler(ctx: Context<ClaimReferralRewards>) -> Result<()> {
    let referral_code = &mut ctx.accounts.referral_code;

    // Check there are rewards to claim
    require!(
        referral_code.pending_rewards > 0,
        PerpsError::NoRewardsToClaim
    );

    let amount = referral_code.pending_rewards;

    // Check vault has sufficient balance
    require!(
        ctx.accounts.vault_token_account.amount >= amount,
        PerpsError::InsufficientVaultBalance
    );

    // Reset pending rewards
    referral_code.pending_rewards = 0;

    // Transfer rewards from vault to owner
    let market_key = ctx.accounts.market.key();
    let vault_bump = ctx.accounts.vault.bump;
    let vault_seeds = &[
        b"vault".as_ref(),
        market_key.as_ref(),
        &[vault_bump],
    ];
    let signer = &[&vault_seeds[..]];

    let cpi_accounts = Transfer {
        from: ctx.accounts.vault_token_account.to_account_info(),
        to: ctx.accounts.owner_token_account.to_account_info(),
        authority: ctx.accounts.vault.to_account_info(),
    };
    let cpi_ctx = CpiContext::new_with_signer(
        ctx.accounts.token_program.to_account_info(),
        cpi_accounts,
        signer,
    );
    token::transfer(cpi_ctx, amount)?;

    msg!(
        "Claimed {} referral rewards for code {}",
        amount,
        referral_code.code_str()
    );

    Ok(())
}
