use anchor_lang::prelude::*;
use crate::state::ReferralCode;
use crate::errors::PerpsError;

#[derive(AnchorSerialize, AnchorDeserialize)]
pub struct CreateReferralCodeParams {
    /// The referral code (will be uppercased, max 8 chars)
    pub code: [u8; 8],
}

#[derive(Accounts)]
#[instruction(params: CreateReferralCodeParams)]
pub struct CreateReferralCode<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,

    #[account(
        init,
        payer = owner,
        space = ReferralCode::LEN,
        seeds = [b"referral_code", params.code.as_ref()],
        bump
    )]
    pub referral_code: Account<'info, ReferralCode>,

    pub system_program: Program<'info, System>,
}

pub fn handler(ctx: Context<CreateReferralCode>, params: CreateReferralCodeParams) -> Result<()> {
    // Validate code format
    require!(
        ReferralCode::is_valid_code(&params.code),
        PerpsError::InvalidReferralCode
    );

    let referral_code = &mut ctx.accounts.referral_code;
    let clock = Clock::get()?;

    referral_code.owner = ctx.accounts.owner.key();
    referral_code.code = params.code;
    referral_code.discount_bps = ReferralCode::DEFAULT_DISCOUNT_BPS;
    referral_code.reward_bps = ReferralCode::DEFAULT_REWARD_BPS;
    referral_code.total_referred = 0;
    referral_code.total_volume = 0;
    referral_code.total_fees_generated = 0;
    referral_code.total_rewards_earned = 0;
    referral_code.pending_rewards = 0;
    referral_code.created_at = clock.unix_timestamp;
    referral_code.is_active = true;
    referral_code.bump = *ctx.bumps.get("referral_code").unwrap();

    msg!(
        "Referral code created: {} by {}",
        referral_code.code_str(),
        ctx.accounts.owner.key()
    );

    Ok(())
}
