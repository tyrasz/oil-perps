use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Transfer};
use crate::oracle::read_price;
use crate::events::PositionClosed;
use crate::instructions::open_position::check_acceptable;
use crate::instructions::settle_close::settle_close;
use crate::state::{assert_backed, skew_price, LpPool, Market, Position, Vault, UserAccount, Side, PositionStatus, ReferralCode, UserReferral};
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
    pub user_account: Box<Account<'info, UserAccount>>,

    #[account(
        mut,
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
    pub user_referral: Option<Box<Account<'info, UserReferral>>>,

    #[account(
        mut,
        seeds = [b"referral_code", referral_code.code.as_ref()],
        bump = referral_code.bump,
    )]
    pub referral_code: Option<Box<Account<'info, ReferralCode>>>,

    /// The referrer's token account; the reward is paid here at close.
    /// Validated in the handler (owner = referral code owner, market's mint).
    #[account(mut)]
    pub referrer_token_account: Option<Box<Account<'info, TokenAccount>>>,

    // docs/lp-pool.md §4.5: account 13, after the three optionals (a client
    // with no referral passes the program id in each optional slot)
    #[account(mut, seeds = [LpPool::SEED, market.key().as_ref()], bump = lp_pool.bump)]
    pub lp_pool: Box<Account<'info, LpPool>>,
}

pub fn handler(ctx: Context<ClosePosition>, acceptable_price: u64) -> Result<()> {
    let current_time = Clock::get()?.unix_timestamp;
    let position_key = ctx.accounts.position.key();

    // 1. Price. A settled market closes at its settlement price. Otherwise the
    // market must be in session: closing at a stale weekend price would let
    // traders pick off the reopening gap. House closes pay skew impact (§2.4).
    let (mark, exec) = close_prices(&ctx.accounts.market, &ctx.accounts.lp_pool, &ctx.accounts.position, &ctx.accounts.pyth_price_feed, current_time)?;
    check_acceptable(ctx.accounts.position.side == Side::Short, exec, acceptable_price)?;

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
                    && referrer_token_account.mint == ctx.accounts.market.collateral_mint,
                PerpsError::InvalidReferralParams
            );
            Some((user_referral.discount_bps, referral_code.reward_bps))
        }
        _ => None,
    };
    let notional = ctx.accounts.position.notional_value();

    // 2–7. Value, caps, profit or loss, fee, equity, pool (docs/lp-pool.md §4.5)
    let a = &mut *ctx.accounts;
    let o = settle_close(
        &mut a.market, &mut a.lp_pool, &mut a.position, &mut a.user_account, position_key, mark, exec, current_time, referral,
    )?;

    if let (Some(user_referral), Some(referral_code)) = (&mut a.user_referral, &mut a.referral_code) {
        if referral.is_some() {
            user_referral.total_volume = user_referral.total_volume.saturating_add(notional);
            user_referral.total_fees_paid = user_referral.total_fees_paid.saturating_add(o.fee_charged);
            user_referral.total_referrer_rewards = user_referral.total_referrer_rewards.saturating_add(o.referral_paid);
            referral_code.total_volume = referral_code.total_volume.saturating_add(notional);
            referral_code.total_fees_generated = referral_code.total_fees_generated.saturating_add(o.fee_charged);
            referral_code.total_rewards_earned = referral_code.total_rewards_earned.saturating_add(o.referral_paid);
            msg!("Referral: fee={}, reward paid={}", o.fee_charged, o.referral_paid);
        }
    }

    // 8. Pay the trader, then the referrer, from the vault
    let vt_before = a.vault_token_account.amount;
    let market_key = a.market.key();
    let vault_bump = a.vault.bump;
    let vault_seeds = &[b"vault".as_ref(), market_key.as_ref(), &[vault_bump]];
    let signer = &[&vault_seeds[..]];
    let payouts = [
        (o.payout, Some(a.user_token_account.to_account_info())),
        (o.referral_paid, a.referrer_token_account.as_ref().map(|t| t.to_account_info())),
    ];
    for (amount, to) in payouts {
        if let (true, Some(to)) = (amount > 0, to) {
            let cpi_accounts = Transfer {
                from: a.vault_token_account.to_account_info(),
                to,
                authority: a.vault.to_account_info(),
            };
            let cpi_ctx = CpiContext::new_with_signer(a.token_program.to_account_info(), cpi_accounts, signer);
            token::transfer(cpi_ctx, amount)?;
        }
    }
    let out = o.payout.checked_add(o.referral_paid).ok_or(PerpsError::MathOverflow)?;
    let after = vt_before.checked_sub(out).ok_or(PerpsError::VaultInvariantViolated)?;
    assert_backed(&a.lp_pool, &a.market, after)?;

    // 9. Log
    msg!(
        "Position closed: PnL={}, Funding={}, Fee={}, Settlement={}",
        o.pnl,
        o.funding,
        o.fee,
        o.payout
    );
    emit!(PositionClosed {
        position: position_key,
        owner: a.position.owner,
        mark,
        exec_price: exec,
        pnl: o.pnl,
        funding: o.funding,
        solvency_hc: o.solvency_hc,
        bad_debt_hc: o.bad_debt_hc,
        fee: o.fee,
        referral_paid: o.referral_paid,
        payout: o.payout,
        deficit: o.deficit,
        settled_close: false,
    });

    Ok(())
}

/// §4.5 step 1: `(mark, exec)`. Settled: both the settlement price. Otherwise
/// in session, the oracle, and the skew price of the closing trade (closing a
/// long sells, closing a short buys).
#[inline(never)]
pub fn close_prices(m: &Market, pool: &LpPool, position: &Position, feed: &AccountInfo, now: i64) -> Result<(u64, u64)> {
    if m.is_settled() {
        return Ok((m.settlement_price, m.settlement_price));
    }
    require!(m.is_in_session(now), PerpsError::MarketClosed);
    let mark = read_price(feed, m, now)?;
    let skew = m.long_open_interest as i128 - m.short_open_interest as i128;
    let delta = match position.side {
        Side::Long => -(position.size as i128),
        Side::Short => position.size as i128,
    };
    let exec = skew_price(mark, skew, delta, &pool.params)?;
    Ok((mark, exec))
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
