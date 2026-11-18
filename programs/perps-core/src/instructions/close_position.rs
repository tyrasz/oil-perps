use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Transfer};
use crate::oracle::read_price;
use crate::state::{split_equity, Market, Position, Vault, UserAccount, Side, PositionStatus, ReferralCode, UserReferral};
use crate::errors::PerpsError;

#[derive(Accounts)]
pub struct ClosePosition<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,

    #[account(
        mut,
        seeds = [b"user", owner.key().as_ref(), market.key().as_ref()],
        bump = user_account.bump,
        constraint = user_account.owner == owner.key()
    )]
    pub user_account: Account<'info, UserAccount>,

    #[account(
        mut,
        seeds = [b"market", market.collateral_mint.as_ref(), market.commodity_seed()],
        bump = market.bump
    )]
    pub market: Account<'info, Market>,

    #[account(
        mut,
        constraint = position.owner == owner.key() @ PerpsError::Unauthorized,
        constraint = position.market == market.key(),
        constraint = position.status == PositionStatus::Open @ PerpsError::PositionAlreadyClosed
    )]
    pub position: Account<'info, Position>,

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
        constraint = user_token_account.owner == owner.key(),
        constraint = user_token_account.mint == market.collateral_mint
    )]
    pub user_token_account: Account<'info, TokenAccount>,

    /// CHECK: Pyth price feed, validated below
    #[account(constraint = pyth_price_feed.key() == market.pyth_price_feed)]
    pub pyth_price_feed: AccountInfo<'info>,

    pub token_program: Program<'info, Token>,

    // Optional referral accounts - if user has a referral, include these
    #[account(
        mut,
        seeds = [b"user_referral", owner.key().as_ref()],
        bump = user_referral.bump,
    )]
    pub user_referral: Option<Account<'info, UserReferral>>,

    #[account(
        mut,
        seeds = [b"referral_code", referral_code.code.as_ref()],
        bump = referral_code.bump,
    )]
    pub referral_code: Option<Account<'info, ReferralCode>>,

    /// The referrer's token account; the reward is paid here at close.
    /// Validated in the handler (owner = referral code owner, market's mint).
    #[account(mut)]
    pub referrer_token_account: Option<Account<'info, TokenAccount>>,
}

pub fn handler(ctx: Context<ClosePosition>) -> Result<()> {
    let market = &mut ctx.accounts.market;
    let position = &mut ctx.accounts.position;
    let user_account = &mut ctx.accounts.user_account;

    let current_time = Clock::get()?.unix_timestamp;

    // A settled market closes at its settlement price. Otherwise the market must
    // be in session: closing at a stale weekend price would let traders pick
    // off the reopening gap.
    let oracle_price = if market.is_settled() {
        market.settlement_price
    } else {
        require!(market.is_in_session(current_time), PerpsError::MarketClosed);
        read_price(&ctx.accounts.pyth_price_feed, market, current_time)?
    };

    // Calculate PnL
    let pnl = position.unrealized_pnl(oracle_price);

    // Funding accrued since open, in collateral units
    let funding_payment = position.accrued_funding(market.cumulative_funding);

    // Calculate fees
    let notional = position.notional_value();
    let base_fee = (notional as u128 * market.taker_fee as u128 / 10000) as u64;

    // Referral: the reward is paid straight to the referrer from this market's
    // vault, so it is always backed by a fee collected here. (Accruing rewards
    // in one balance and claiming them from any market let rewards earned in
    // one market, or minted in an attacker's market, drain another's vault.)
    let referral = match (
        &ctx.accounts.user_referral,
        &ctx.accounts.referral_code,
        &ctx.accounts.referrer_token_account,
    ) {
        (Some(user_referral), Some(referral_code), Some(referrer_token_account))
            if user_referral.referral_code == referral_code.key() && referral_code.is_active =>
        {
            require!(
                referrer_token_account.owner == referral_code.owner
                    && referrer_token_account.mint == market.collateral_mint,
                PerpsError::InvalidReferralParams
            );
            Some((user_referral.discount_bps, referral_code.reward_bps))
        }
        _ => None,
    };
    let (fee, referral_reward) = referral_fees(base_fee, referral);

    // Only pay the referrer out of fee the position could actually cover
    let equity_before_fee = (position.collateral as i64)
        .saturating_add(pnl)
        .saturating_add(funding_payment);
    let referral_paid = referral_reward_paid(referral_reward, fee, equity_before_fee);

    if let (Some(user_referral), Some(referral_code)) =
        (&mut ctx.accounts.user_referral, &mut ctx.accounts.referral_code)
    {
        if referral.is_some() {
            user_referral.total_volume = user_referral.total_volume.saturating_add(notional);
            user_referral.total_fees_paid = user_referral.total_fees_paid.saturating_add(fee);
            user_referral.total_referrer_rewards = user_referral.total_referrer_rewards.saturating_add(referral_paid);
            referral_code.total_volume = referral_code.total_volume.saturating_add(notional);
            referral_code.total_fees_generated = referral_code.total_fees_generated.saturating_add(fee);
            referral_code.total_rewards_earned = referral_code.total_rewards_earned.saturating_add(referral_paid);
            msg!("Referral: fee={} (base {}), reward paid={}", fee, base_fee, referral_paid);
        }
    }

    // Final settlement amount
    let total_pnl = pnl + funding_payment - fee as i64;
    let (settlement, deficit) = split_equity(position.collateral as i64 + total_pnl);

    // Update market OI
    match position.side {
        Side::Long => {
            market.long_open_interest = market.long_open_interest
                .saturating_sub(position.size);
        }
        Side::Short => {
            market.short_open_interest = market.short_open_interest
                .saturating_sub(position.size);
        }
    }

    // Fee (minus the referral reward paid out) goes to the insurance fund, which
    // then absorbs any loss beyond the position's collateral.
    let insurance_fee = fee.saturating_sub(referral_paid);
    let uncovered = market.settle_insurance(insurance_fee, deficit);
    if deficit > 0 {
        msg!("Bad debt: deficit={}, uncovered={}", deficit, uncovered);
    }

    // Mark position as closed
    position.status = PositionStatus::Closed;
    position.realized_pnl = total_pnl;
    position.last_updated_at = current_time;

    // Update user stats
    user_account.realized_pnl = user_account.realized_pnl
        .saturating_add(total_pnl);
    user_account.total_trades = user_account.total_trades
        .saturating_add(1);

    // Pay the trader, then the referrer, from the vault
    let market_key = market.key();
    let vault_bump = ctx.accounts.vault.bump;
    let vault_seeds = &[b"vault".as_ref(), market_key.as_ref(), &[vault_bump]];
    let signer = &[&vault_seeds[..]];
    let payouts = [
        (settlement, Some(ctx.accounts.user_token_account.to_account_info())),
        (referral_paid, ctx.accounts.referrer_token_account.as_ref().map(|a| a.to_account_info())),
    ];
    for (amount, to) in payouts {
        if let (true, Some(to)) = (amount > 0, to) {
            let cpi_accounts = Transfer {
                from: ctx.accounts.vault_token_account.to_account_info(),
                to,
                authority: ctx.accounts.vault.to_account_info(),
            };
            let cpi_ctx = CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                cpi_accounts,
                signer,
            );
            token::transfer(cpi_ctx, amount)?;
        }
    }

    msg!(
        "Position closed: PnL={}, Funding={}, Fee={}, Settlement={}",
        pnl,
        funding_payment,
        fee,
        settlement
    );

    Ok(())
}

/// `(fee, referral_reward)` for a close: with a referral, the trader's fee is
/// discounted and the referrer gets `reward_bps` of the discounted fee.
pub fn referral_fees(base_fee: u64, referral: Option<(u16, u16)>) -> (u64, u64) {
    match referral {
        Some((discount_bps, reward_bps)) => {
            let discount = (base_fee as u128 * discount_bps as u128 / 10_000) as u64;
            let fee = base_fee.saturating_sub(discount);
            (fee, (fee as u128 * reward_bps as u128 / 10_000) as u64)
        }
        None => (base_fee, 0),
    }
}

/// Referral reward actually paid: scaled down to the part of the fee the
/// position could cover (`equity_before_fee` = collateral + PnL + funding).
pub fn referral_reward_paid(reward: u64, fee: u64, equity_before_fee: i64) -> u64 {
    if fee == 0 || reward == 0 {
        return 0;
    }
    let collected = equity_before_fee.clamp(0, fee as i64) as u128;
    (reward as u128 * collected / fee as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn referral_discounts_fee_and_rewards_referrer() {
        // $10 base fee, 10% discount, 20% of the discounted fee to the referrer
        assert_eq!(referral_fees(10_000_000, Some((1_000, 2_000))), (9_000_000, 1_800_000));
        assert_eq!(referral_fees(10_000_000, None), (10_000_000, 0));
    }

    #[test]
    fn reward_is_only_paid_from_collected_fee() {
        assert_eq!(referral_reward_paid(1_800_000, 9_000_000, 100_000_000), 1_800_000);
        // Position could cover only half the fee
        assert_eq!(referral_reward_paid(1_800_000, 9_000_000, 4_500_000), 900_000);
        // Underwater: nothing collected, nothing paid
        assert_eq!(referral_reward_paid(1_800_000, 9_000_000, -50_000_000), 0);
        assert_eq!(referral_reward_paid(0, 0, 100), 0);
    }
}
