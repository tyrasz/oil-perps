use anchor_lang::prelude::*;
use crate::oracle::read_price;
use crate::state::{funding_per_contract, Market};
use crate::errors::PerpsError;
use crate::events::FundingUpdated;
use crate::pool_math;
use crate::session;

#[derive(Accounts)]
pub struct UpdateFunding<'info> {
    #[account(mut)]
    pub keeper: Signer<'info>,

    #[account(
        mut,
        seeds = [b"market", market.collateral_mint.as_ref(), market.commodity_seed()],
        bump = market.bump
    )]
    pub market: Box<Account<'info, Market>>,

    /// CHECK: Pyth price feed
    #[account(constraint = pyth_price_feed.key() == market.pyth_price_feed)]
    pub pyth_price_feed: AccountInfo<'info>,
}

/// §8 steps 3–8 at `oracle_price`: charge every open funding period missed
/// since the last crank (up to `MAX_FUNDING_CATCHUP_PERIODS`) at the rate the
/// current OI imbalance gives. `None`, and nothing changes, when no period is
/// due. `update_funding` cranks with it, and so does every instruction that
/// sets or settles a position's funding basis (open, venue fill, close,
/// liquidation, ADL; D40), so a stale `cumulative_funding` can never be
/// captured by an open or escaped by a close.
#[inline(never)]
pub fn apply_funding(market: &mut Market, oracle_price: u64, now: i64) -> Option<FundingUpdated> {
    // 3–5. Open seconds since the last crank plus the carry
    let open = session::open_seconds_between(market.session_schedule, market.last_funding_time, now);
    let total = open.saturating_add(market.funding_carry_secs as i64);
    let c = pool_math::funding_catchup(total, market.funding_interval)?;

    // 6. Calculate funding rate based on OI imbalance
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
    let step = funding_per_contract(funding_rate, oracle_price);

    // 7–8. Update market
    market.funding_rate = funding_rate;
    market.cumulative_funding = pool_math::funding_accumulate(market.cumulative_funding, c.n, step);
    market.last_funding_time = now;
    market.funding_carry_secs = c.carry;
    Some(FundingUpdated {
        periods: c.n,
        periods_dropped: c.dropped,
        rate: funding_rate,
        step,
        cumulative_funding: market.cumulative_funding,
        carry_secs: c.carry,
    })
}

/// §8 step 9: the indexer's "Funding updated:" line (D9) and the event.
#[inline(never)]
pub fn log_funding(market: &Market, oracle_price: u64, ev: FundingUpdated) {
    msg!(
        "Funding updated: rate={}, long_oi={}, short_oi={}, price={}",
        ev.rate,
        market.long_open_interest,
        market.short_open_interest,
        oracle_price
    );
    emit!(ev);
}

/// Crank funding inline if a period is due (D40). Unsettled markets only;
/// the caller has read `oracle_price` in session.
#[inline(never)]
pub fn crank_funding(market: &mut Market, oracle_price: u64, now: i64) {
    if market.is_settled() {
        return;
    }
    if let Some(ev) = apply_funding(market, oracle_price, now) {
        log_funding(market, oracle_price, ev);
    }
}

pub fn handler(ctx: Context<UpdateFunding>) -> Result<()> {
    let market = &mut ctx.accounts.market;
    let current_time = Clock::get()?.unix_timestamp;

    // 1–2. docs/lp-pool.md §8
    require!(!market.is_settled(), PerpsError::MarketSettled);
    require!(market.is_in_session(current_time), PerpsError::MarketClosed);
    let oracle_price = read_price(&ctx.accounts.pyth_price_feed, market, current_time)?;

    // 3–8. Missed periods are charged at once
    let ev = apply_funding(market, oracle_price, current_time).ok_or(PerpsError::FundingNotDue)?;

    // 9.
    log_funding(market, oracle_price, ev);
    Ok(())
}
