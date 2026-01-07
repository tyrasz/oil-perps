use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Transfer};
use pyth_sdk_solana::load_price_feed_from_account_info;
use crate::state::{Market, Position, Vault, UserAccount, Side, PositionStatus};
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
}

pub fn handler(ctx: Context<ClosePosition>) -> Result<()> {
    let market = &mut ctx.accounts.market;
    let position = &mut ctx.accounts.position;
    let user_account = &mut ctx.accounts.user_account;

    // Get oracle price
    let price_feed = load_price_feed_from_account_info(&ctx.accounts.pyth_price_feed)
        .map_err(|_| PerpsError::InvalidOraclePrice)?;

    let current_time = Clock::get()?.unix_timestamp;
    let price_data = price_feed
        .get_price_no_older_than(current_time, 60)
        .ok_or(PerpsError::StaleOraclePrice)?;

    require!(price_data.price > 0, PerpsError::InvalidOraclePrice);

    let oracle_price = (price_data.price as u64)
        .checked_mul(10u64.pow((6 + price_data.expo.abs() as u32) as u32))
        .and_then(|p| p.checked_div(10u64.pow(price_data.expo.abs() as u32)))
        .ok_or(PerpsError::MathOverflow)? as u64;

    // Calculate PnL
    let pnl = position.unrealized_pnl(oracle_price);

    // Calculate funding payment
    let funding_diff = market.funding_rate - position.last_funding_payment;
    let funding_payment = match position.side {
        Side::Long => -((position.size as i64 * funding_diff) / 1_000_000),
        Side::Short => (position.size as i64 * funding_diff) / 1_000_000,
    };

    // Calculate fees
    let notional = position.notional_value();
    let fee = (notional as u128 * market.taker_fee as u128 / 10000) as u64;

    // Final settlement amount
    let total_pnl = pnl + funding_payment - fee as i64;
    let settlement = (position.collateral as i64 + total_pnl).max(0) as u64;

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

    // Add fee to insurance fund
    market.insurance_fund = market.insurance_fund.saturating_add(fee);

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
