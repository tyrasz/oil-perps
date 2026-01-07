use anchor_lang::prelude::*;
use pyth_sdk_solana::load_price_feed_from_account_info;
use crate::state::Market;
use crate::errors::PerpsError;

#[derive(Accounts)]
pub struct UpdateFunding<'info> {
    #[account(mut)]
    pub keeper: Signer<'info>,

    #[account(
        mut,
        seeds = [b"market", market.collateral_mint.as_ref()],
        bump = market.bump
    )]
    pub market: Account<'info, Market>,

    /// CHECK: Pyth price feed
    #[account(constraint = pyth_price_feed.key() == market.pyth_price_feed)]
    pub pyth_price_feed: AccountInfo<'info>,
}

pub fn handler(ctx: Context<UpdateFunding>) -> Result<()> {
    let market = &mut ctx.accounts.market;
    let current_time = Clock::get()?.unix_timestamp;

    // Check if enough time has passed
    let time_elapsed = current_time - market.last_funding_time;
    require!(
        time_elapsed >= market.funding_interval,
        PerpsError::InvalidMarketConfig
    );

    // Get oracle price
    let price_feed = load_price_feed_from_account_info(&ctx.accounts.pyth_price_feed)
        .map_err(|_| PerpsError::InvalidOraclePrice)?;

    let price_data = price_feed
        .get_price_no_older_than(current_time, 60)
        .ok_or(PerpsError::StaleOraclePrice)?;

    require!(price_data.price > 0, PerpsError::InvalidOraclePrice);

    let oracle_price = (price_data.price as u64)
        .checked_mul(10u64.pow((6 + price_data.expo.abs() as u32) as u32))
        .and_then(|p| p.checked_div(10u64.pow(price_data.expo.abs() as u32)))
        .ok_or(PerpsError::MathOverflow)? as u64;

    // Calculate funding rate based on OI imbalance
    // Positive = longs pay shorts, Negative = shorts pay longs
    let long_oi = market.long_open_interest as i128;
    let short_oi = market.short_open_interest as i128;
    let total_oi = long_oi + short_oi;

    let funding_rate = if total_oi > 0 {
        // Funding rate = (long_oi - short_oi) / total_oi * base_rate
        // Using 0.01% per hour as base rate = 100 (6 decimals)
        let imbalance = ((long_oi - short_oi) * 1_000_000) / total_oi;
        let base_rate: i128 = 100; // 0.01% in 6 decimals
        ((imbalance * base_rate) / 1_000_000) as i64
    } else {
        0
    };

    // Update market
    market.funding_rate = market.funding_rate.saturating_add(funding_rate);
    market.last_funding_time = current_time;

    msg!(
        "Funding updated: rate={}, long_oi={}, short_oi={}, price={}",
        funding_rate,
        market.long_open_interest,
        market.short_open_interest,
        oracle_price
    );

    Ok(())
}
