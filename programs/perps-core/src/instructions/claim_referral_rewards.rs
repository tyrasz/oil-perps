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
        mut,
        seeds = [b"market", market.collateral_mint.as_ref(), market.commodity_seed()],
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
    let market = &mut ctx.accounts.market;

    // Rewards accrue across markets, but each market's vault only holds the
    // rewards earned there. Pay out what this market holds; the rest can be
    // claimed from the other markets.
    let amount = referral_claim_amount(referral_code.pending_rewards, market.referral_reserve);
    require!(amount > 0, PerpsError::NoRewardsToClaim);

    // Check vault has sufficient balance
    require!(
        ctx.accounts.vault_token_account.amount >= amount,
        PerpsError::InsufficientVaultBalance
    );

    referral_code.pending_rewards -= amount;
    market.referral_reserve -= amount;

    // Transfer rewards from vault to owner
    let market_key = market.key();
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

/// Amount a referrer can claim from one market: their pending rewards, capped
/// by the rewards that market is holding.
pub fn referral_claim_amount(pending_rewards: u64, market_reserve: u64) -> u64 {
    pending_rewards.min(market_reserve)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claim_is_capped_by_the_market_reserve() {
        assert_eq!(referral_claim_amount(100, 1_000), 100);
        assert_eq!(referral_claim_amount(100, 40), 40);
        assert_eq!(referral_claim_amount(100, 0), 0);
        assert_eq!(referral_claim_amount(0, 1_000), 0);
    }

    #[test]
    fn referrers_are_paid_in_full_across_markets() {
        // Code A earned 10 in OIL, code B earned 10 in GOLD.
        let (mut a, mut b) = (10u64, 10u64);
        let (mut oil, mut gold) = (10u64, 10u64);
        // A claims from GOLD first, then B finds GOLD empty and claims from OIL.
        let x = referral_claim_amount(a, gold); a -= x; gold -= x;
        let y = referral_claim_amount(b, gold); assert_eq!(y, 0);
        let y = referral_claim_amount(b, oil); b -= y; oil -= y;
        assert_eq!((a, b, oil, gold), (0, 0, 0, 0));
    }
}
