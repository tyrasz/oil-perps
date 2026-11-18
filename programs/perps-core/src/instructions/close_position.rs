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
        seeds = [b"user", owner.key().as_ref()],
        bump = user_account.bump,
        constraint = user_account.owner == owner.key()
    )]
    pub user_account: Account<'info, UserAccount>,

    #[account(
        mut,
        seeds = [b"market", market.collateral_mint.as_ref()],
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
    let funding_payment = position.accrued_funding(market.funding_rate, oracle_price);

    // Calculate fees
    let notional = position.notional_value();
    let base_fee = (notional as u128 * market.taker_fee as u128 / 10000) as u64;

    // Process referral if user has one
    let (fee, referral_reward) = if let (Some(user_referral), Some(referral_code)) =
        (&mut ctx.accounts.user_referral, &mut ctx.accounts.referral_code)
    {
        // Validate referral relationship
        if user_referral.referral_code == referral_code.key() && referral_code.is_active {
            // Calculate discounted fee for user
            let discount = (base_fee as u128 * user_referral.discount_bps as u128 / 10000) as u64;
            let discounted_fee = base_fee.saturating_sub(discount);

            // Calculate reward for referrer (based on discounted fee to prevent gaming)
            let reward = (discounted_fee as u128 * referral_code.reward_bps as u128 / 10000) as u64;

            // Update user referral stats (FIXES SYBIL ATTACK - tracked on actual trades)
            user_referral.total_volume = user_referral.total_volume.saturating_add(notional);
            user_referral.total_fees_paid = user_referral.total_fees_paid.saturating_add(discounted_fee);
            user_referral.total_referrer_rewards = user_referral.total_referrer_rewards.saturating_add(reward);

            // Update referral code stats
            referral_code.total_volume = referral_code.total_volume.saturating_add(notional);
            referral_code.total_fees_generated = referral_code.total_fees_generated.saturating_add(discounted_fee);
            referral_code.total_rewards_earned = referral_code.total_rewards_earned.saturating_add(reward);
            referral_code.pending_rewards = referral_code.pending_rewards.saturating_add(reward);

            msg!("Referral processed: discount={}, reward={}", discount, reward);

            (discounted_fee, reward)
        } else {
            (base_fee, 0)
        }
    } else {
        (base_fee, 0)
    };

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

    // Add fee to insurance fund (minus referral reward which stays for claiming),
    // then let the fund absorb any loss beyond the position's collateral.
    let insurance_fee = fee.saturating_sub(referral_reward);
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

    // Transfer settlement to user
    if settlement > 0 {
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
            to: ctx.accounts.user_token_account.to_account_info(),
            authority: ctx.accounts.vault.to_account_info(),
        };
        let cpi_ctx = CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            cpi_accounts,
            signer,
        );
        token::transfer(cpi_ctx, settlement)?;
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
