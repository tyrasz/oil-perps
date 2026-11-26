use anchor_lang::prelude::*;
use crate::errors::PerpsError;
use crate::instructions::update_funding::crank_funding;
use crate::events::PositionOpened;
use crate::oracle::read_price;
use crate::pool_math;
use crate::state::{
    distribute_fee, drawdown_now, pool_add, pool_view, side_u8, skew_price, touch_peak, venue_margin, ExecutionSource,
    LpPool, Market, Position, PoolView, Side, UserAccount,
};

#[derive(AnchorSerialize, AnchorDeserialize)]
pub struct OpenPositionParams {
    pub side: u8,           // 0 = Long, 1 = Short
    pub size: u64,          // Position size in base units
    pub leverage: u32,      // Leverage (3 decimals: 10x = 10_000)
    // docs/lp-pool.md §4.4 (appended): worst acceptable execution price; 0 = no bound
    pub acceptable_price: u64,
}

#[derive(Accounts)]
#[instruction(params: OpenPositionParams)]
pub struct OpenPosition<'info> {
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
        bump = market.bump,
        constraint = !market.is_paused @ PerpsError::MarketPaused
    )]
    pub market: Box<Account<'info, Market>>,

    #[account(
        init,
        payer = owner,
        space = Position::LEN,
        seeds = [b"position", owner.key().as_ref(), market.key().as_ref(), &market.total_positions.to_le_bytes()],
        bump
    )]
    pub position: Box<Account<'info, Position>>,

    /// CHECK: Pyth price feed, validated below
    #[account(constraint = pyth_price_feed.key() == market.pyth_price_feed)]
    pub pyth_price_feed: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,

    // docs/lp-pool.md §4.4 (account 7): the house is the pool
    #[account(mut, seeds = [LpPool::SEED, market.key().as_ref()], bump = lp_pool.bump)]
    pub lp_pool: Box<Account<'info, LpPool>>,
}

/// What the house quotes for an open: the execution price, the margin to post
/// and the open fee (§4.4 steps 4–5).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HouseQuote {
    pub exec: u64,
    pub notional: u64,
    pub margin: u64,
    pub fee: u64,
}

/// `acceptable_price` check: a buy needs `exec ≤ bound`, a sell `exec ≥ bound`;
/// 0 is no bound.
pub fn check_acceptable(buy: bool, exec: u64, acceptable_price: u64) -> Result<()> {
    if acceptable_price != 0 {
        let ok = if buy { exec <= acceptable_price } else { exec >= acceptable_price };
        require!(ok, PerpsError::SlippageExceeded);
    }
    Ok(())
}

/// §4.4 steps 4–5: skew execution price, slippage bound, margin and fee.
#[inline(never)]
#[allow(clippy::too_many_arguments)]
pub fn house_quote(m: &Market, pool: &LpPool, side: Side, size: u64, leverage: u32, oracle: u64, acceptable_price: u64, now: i64) -> Result<HouseQuote> {
    let skew = m.long_open_interest as i128 - m.short_open_interest as i128;
    let delta = match side {
        Side::Long => size as i128,
        Side::Short => -(size as i128),
    };
    let exec = skew_price(oracle, skew, delta, &pool.params)?;
    check_acceptable(side == Side::Long, exec, acceptable_price)?;
    let notional = u64::try_from(size as u128 * exec as u128 / 1_000_000).map_err(|_| error!(PerpsError::MathOverflow))?;
    require!(notional > 0, PerpsError::PositionTooSmall);
    let margin = venue_margin(m, side, size, exec, oracle, leverage, now).ok_or(PerpsError::ExcessiveLeverage)?;
    let fee = u64::try_from(pool_math::ceil_u(notional as u128 * pool.params.open_fee_bps as u128, 10_000))
        .map_err(|_| error!(PerpsError::MathOverflow))?;
    Ok(HouseQuote { exec, notional, margin, fee })
}

/// §4.4 steps 7–8: the pool's caps on a house open, at the oracle. Net-raising
/// opens must clear the bad-debt and drawdown freezes, the net cap and (with
/// the weekend bound on, D43) the weekend cap, all against `capNAV`; the
/// weekend cap's `capNAV` is net of the trade's own skew-premium shortfall
/// at `exec` (`premium_shortfall`), so `G·net_notional ≤ α·B` holds after
/// the open too. Every open must fit the per-account cap on
/// `opened_notional`.
#[inline(never)]
#[allow(clippy::too_many_arguments)]
pub fn check_open_caps(m: &Market, pool: &LpPool, v: &PoolView, side: Side, size: u64, oracle: u64, exec: u64, house_entry_notional: u64, opened_notional: u64) -> Result<()> {
    let p = &pool.params;
    let after = match side {
        Side::Long => v.net + size as i128,
        Side::Short => v.net - size as i128,
    };
    if after.unsigned_abs() > v.net.unsigned_abs() {
        require!(!pool.adl_required(), PerpsError::PoolAdlPending);
        require!(!pool_math::dd_freeze(drawdown_now(pool, v), p.dd_freeze_bps), PerpsError::PoolDrawdownFreeze);
        let nn = pool_math::ceil_u(
            after.unsigned_abs().checked_mul(oracle as u128).ok_or(PerpsError::MathOverflow)?,
            1_000_000,
        );
        require!(pool_math::net_cap_ok(nn, p.k_net_bps, v.cap_nav), PerpsError::PoolExposureLimit);
        // Always on with the weekend bound (D43), not only inside W: G can
        // rise after the params were applied, and net opened before W is
        // the net carried into the close
        if p.weekend_bound_on() {
            let short = pool_math::premium_shortfall(v.net, after, oracle, exec, size, p.skew_scale, p.max_impact_bps)
                .ok_or(PerpsError::MathOverflow)?;
            require!(
                pool_math::weekend_cap_ok(nn, p.g_bps(m), p.weekend_lp_loss_bps, v.cap_nav.saturating_sub(short)),
                PerpsError::PoolWeekendExposureLimit
            );
        }
    }
    require!(
        pool_math::per_account_ok(house_entry_notional, opened_notional, p.per_account_cap_bps, p.k_net_bps, v.cap_nav),
        PerpsError::AccountExposureLimit
    );
    Ok(())
}

/// §4.4 steps 7–9 on the pool and the user: caps, then the bucket moves
/// (`−(margin+fee) +margin +fee`), the aggregates and the house notional.
/// Returns the view the caps were checked on.
#[inline(never)]
#[allow(clippy::too_many_arguments)]
pub fn house_open_pool(m: &mut Market, pool: &mut LpPool, user: &mut UserAccount, side: Side, size: u64, oracle: u64, q: &HouseQuote, now: i64) -> Result<PoolView> {
    let v = pool_view(pool, m, oracle, now, pool.params.redeem_loss_haircut_bps)?;
    touch_peak(pool, &v, now);
    check_open_caps(m, pool, &v, side, size, oracle, q.exec, user.house_entry_notional, q.notional)?;

    let total = q.margin.checked_add(q.fee).ok_or(PerpsError::MathOverflow)?;
    user.collateral_balance = user.collateral_balance.checked_sub(total).ok_or(PerpsError::InsufficientCollateral)?;
    pool.free_collateral = pool.free_collateral.checked_sub(total).ok_or(PerpsError::BucketUnderflow)?;
    pool.position_collateral = pool.position_collateral.checked_add(q.margin).ok_or(PerpsError::MathOverflow)?;
    distribute_fee(m, pool, q.fee)?;

    pool_add(pool, side, size, q.exec, m.cumulative_funding)?;
    match side {
        Side::Long => m.long_open_interest = m.long_open_interest.checked_add(size).ok_or(PerpsError::MathOverflow)?,
        Side::Short => m.short_open_interest = m.short_open_interest.checked_add(size).ok_or(PerpsError::MathOverflow)?,
    }
    // opened_notional(size) with opened_price = exec
    user.house_entry_notional = user.house_entry_notional.checked_add(q.notional).ok_or(PerpsError::MathOverflow)?;
    Ok(v)
}

pub fn handler(ctx: Context<OpenPosition>, params: OpenPositionParams) -> Result<()> {
    let bump = ctx.bumps.position;
    let a = ctx.accounts;

    // 1. Validate leverage, session, size and side
    require!(
        params.leverage > 0 && params.leverage <= a.market.max_leverage,
        PerpsError::ExcessiveLeverage
    );
    let current_time = Clock::get()?.unix_timestamp;
    require!(!a.market.is_settled(), PerpsError::MarketSettled);
    require!(a.market.is_in_session(current_time), PerpsError::MarketClosed);
    require!(params.size > 0, PerpsError::PositionTooSmall);
    let side = match params.side {
        0 => Side::Long,
        1 => Side::Short,
        _ => return Err(PerpsError::InvalidPositionSide.into()),
    };

    // 2. Validated oracle price (staleness, confidence, floor), 6 decimals
    let oracle_price = read_price(&a.pyth_price_feed, &a.market, current_time)?;
    // Funding due is charged before the position takes its basis (D40)
    crank_funding(&mut a.market, oracle_price, current_time);

    // 3. Gross OI cap; no trading before the pool has LPs (D11)
    let oi_ok = match side {
        Side::Long => a.market.can_increase_long_oi(params.size),
        Side::Short => a.market.can_increase_short_oi(params.size),
    };
    require!(oi_ok, PerpsError::OpenInterestCapExceeded);
    require!(a.lp_pool.lp_shares > 0, PerpsError::PoolHasNoLps);

    // 4–6. Execution price, margin, fee
    let q = house_quote(&a.market, &a.lp_pool, side, params.size, params.leverage, oracle_price, params.acceptable_price, current_time)?;
    require!(
        a.user_account.collateral_balance >= q.margin.saturating_add(q.fee),
        PerpsError::InsufficientCollateral
    );

    // 7–9. Pool caps and effects
    let v = house_open_pool(&mut a.market, &mut a.lp_pool, &mut a.user_account, side, params.size, oracle_price, &q, current_time)?;

    // Initialize position (init_open also sets opened_price = exec, docs/lp-pool.md §1.1)
    let market_key = a.market.key();
    a.position.init_open(
        a.owner.key(),
        market_key,
        side,
        params.size,
        q.margin,
        q.exec,
        params.leverage,
        a.market.cumulative_funding,
        current_time,
        ExecutionSource::OrderBook,
        bump,
    );

    let market = &mut a.market;
    market.total_positions = market.total_positions.checked_add(1).ok_or(PerpsError::MathOverflow)?;
    market.total_trades = market.total_trades.checked_add(1).ok_or(PerpsError::MathOverflow)?;
    let user_account = &mut a.user_account;
    user_account.total_positions = user_account.total_positions.checked_add(1).ok_or(PerpsError::MathOverflow)?;
    user_account.total_trades = user_account.total_trades.checked_add(1).ok_or(PerpsError::MathOverflow)?;

    msg!(
        "Position opened: {} {} @ {} with {}x leverage",
        if side == Side::Long { "LONG" } else { "SHORT" },
        params.size,
        q.exec,
        params.leverage / 1000
    );
    let net_after = market.long_open_interest as i128 - market.short_open_interest as i128;
    emit_event!(PositionOpened {
        position: a.position.key(),
        owner: a.owner.key(),
        side: side_u8(side),
        size: params.size,
        oracle: oracle_price,
        exec_price: q.exec,
        collateral: q.margin,
        open_fee: q.fee,
        net_after,
        nav_redeem: v.nav_redeem,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{pool_add, PoolParams};

    const C6: u64 = 1_000_000;

    /// Review finding (D43, skew clamp): a net-raising house open that
    /// crosses into the `max_impact` clamp pays `s·o·1%` but raises the
    /// unwind premium `NAV_redeem` owes by more, so `B` falls in the same
    /// transaction. The weekend cap counts that shortfall, so an open at the
    /// cap still leaves `G·net_notional ≤ α·B` after it.
    #[test]
    fn weekend_cap_holds_after_a_clamped_open() {
        let m = Market {
            maintenance_margin_ratio: 500, initial_margin_ratio: 1_000, weekend_initial_margin_ratio: 2_000,
            max_leverage: 20_000, taker_fee: 5, max_open_interest: u64::MAX, ..Default::default()
        };
        let params = PoolParams {
            skew_scale: 2_500_000_000_000, max_impact_bps: 100, redeem_loss_haircut_bps: 5_000,
            weekend_cap_window_secs: 86_400, weekend_lp_loss_bps: 1_000, ..PoolParams::neutral()
        };
        let o = 75 * C6;
        // +25k contracts of venue net at $75 (quadratic branch), B ≈ $12.75M
        let base = |cash: u64| {
            let (mut m, mut pool) = (m, LpPool { cash, lp_shares: cash, params, ..Default::default() });
            let n0 = 25_000 * C6;
            pool_add(&mut pool, Side::Long, n0, o, 0).unwrap();
            m.long_open_interest = n0;
            (m, pool)
        };
        let (m0, pool0) = base(12_760_000 * C6);
        let v = pool_view(&pool0, &m0, o, 0, params.redeem_loss_haircut_bps).unwrap();
        let g = params.g_bps(&m0);
        assert_eq!(g, 2_000);
        // The largest long the pre-trade view alone would allow: G·nn ≤ α·capNAV
        let nn_max = pool_math::weekend_target(params.weekend_lp_loss_bps, v.cap_nav, g);
        let n_max = (nn_max * 1_000_000 / o as u128) as u64;
        let s = n_max - 25_000 * C6;
        assert!(s > 50_000 * C6, "the trade crosses into the clamp: {}", s);
        let try_open = |s: u64| -> Result<PoolView> {
            let (mut m, mut pool) = (m0.clone(), pool0.clone());
            let mut user = UserAccount { collateral_balance: 10_000_000 * C6, ..Default::default() };
            pool.free_collateral = user.collateral_balance;
            let q = house_quote(&m, &pool, Side::Long, s, 1_000, o, 0, 0)?;
            house_open_pool(&mut m, &mut pool, &mut user, Side::Long, s, o, &q, 0)?;
            pool_view(&pool, &m, o, 0, params.redeem_loss_haircut_bps)
        };
        let post_ok = |w: &PoolView| pool_math::weekend_cap_ok(w.net_notional, g, params.weekend_lp_loss_bps, w.b);
        // At the pre-trade edge the open would end above the bound: refused
        match try_open(s) {
            Ok(w) => assert!(post_ok(&w), "open at the pre-trade edge left G·nn {} over α·B {}", w.net_notional * g as u128, w.b * 1_000),
            Err(e) => assert_eq!(e, PerpsError::PoolWeekendExposureLimit.into()),
        }
        // The largest open the cap accepts keeps the bound after it
        let (mut lo, mut hi) = (0u64, s);
        while lo < hi {
            let mid = lo + (hi - lo + 1) / 2;
            if try_open(mid).is_ok() { lo = mid } else { hi = mid - 1 }
        }
        assert!(lo > 50_000 * C6 && lo < s);
        let w = try_open(lo).unwrap();
        assert!(post_ok(&w), "G·nn {} over α·B {}", w.net_notional * g as u128, w.b * 1_000);
    }
}
