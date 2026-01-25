use anchor_lang::prelude::*;
use crate::state::{ReferralCode, UserReferral};
use crate::errors::PerpsError;

#[derive(Accounts)]
pub struct ApplyReferralCode<'info> {
    #[account(mut)]
    pub user: Signer<'info>,

    #[account(
        mut,
        seeds = [b"referral_code", referral_code.code.as_ref()],
        bump = referral_code.bump,
        constraint = referral_code.is_active @ PerpsError::ReferralCodeInactive,
        // CRITICAL: Prevent self-referral - enforced on-chain
        constraint = referral_code.owner != user.key() @ PerpsError::SelfReferralNotAllowed
    )]
    pub referral_code: Account<'info, ReferralCode>,

    #[account(
        init,
        payer = user,
        space = UserReferral::LEN,
        seeds = [b"user_referral", user.key().as_ref()],
        bump
    )]
    pub user_referral: Account<'info, UserReferral>,

    pub system_program: Program<'info, System>,
}

pub fn handler(ctx: Context<ApplyReferralCode>) -> Result<()> {
    let referral_code = &mut ctx.accounts.referral_code;
    let user_referral = &mut ctx.accounts.user_referral;
    let clock = Clock::get()?;

    // Initialize user referral relationship
    user_referral.user = ctx.accounts.user.key();
    user_referral.referral_code = referral_code.key();
    user_referral.referrer = referral_code.owner;
    user_referral.discount_bps = referral_code.discount_bps;
    user_referral.total_volume = 0;
    user_referral.total_fees_paid = 0;
    user_referral.total_referrer_rewards = 0;
    user_referral.applied_at = clock.unix_timestamp;
    user_referral.bump = *ctx.bumps.get("user_referral").unwrap();

    // Increment referral count
    referral_code.total_referred = referral_code.total_referred
        .checked_add(1)
        .ok_or(PerpsError::MathOverflow)?;

    msg!(
        "Referral code {} applied by user {}. Referrer: {}",
        referral_code.code_str(),
        ctx.accounts.user.key(),
        referral_code.owner
    );

    Ok(())
}
