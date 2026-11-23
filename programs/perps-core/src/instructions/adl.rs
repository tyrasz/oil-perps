//! `settle_position_pnl` (docs/lp-pool.md §6.6, D3), `adl` (§7) and
//! `write_off_bad_debt` (§7.4). All permissionless except exposure-mode ADL.

use anchor_lang::prelude::*;
use crate::errors::PerpsError;
use crate::events::{Adl, BadDebtWrittenOff, PositionPnlSettled};
use crate::instructions::liquidate::{house_sub, oi_sub, slice_funding};
use crate::instructions::settle_close::check_close_caps;
use crate::oracle::read_price;
use crate::pool_math;
use crate::session;
use crate::state::{
    drawdown_now, pay_profit, pool_remove, pool_replace, pool_view, touch_peak, LpPool, Market, PoolView, Position,
    PositionStatus, ProtocolConfig, Side, UserAccount, ADL_WRITE_OFF_SECS,
};

pub const ADL_MODE_DEFICIT: u8 = 0;
pub const ADL_MODE_EXPOSURE: u8 = 1;

/// Settled: the settlement price; otherwise in session and the oracle.
#[inline(never)]
fn mark_price(m: &Market, feed: &AccountInfo, now: i64) -> Result<u64> {
    if m.is_settled() {
        return Ok(m.settlement_price);
    }
    require!(m.is_in_session(now), PerpsError::MarketClosed);
    read_price(feed, m, now)
}

/// Seconds the pending bad debt has been on the books: open seconds since
/// the debt-weighted `pending_since`, or wall-clock seconds once settled.
pub fn pending_age(m: &Market, pool: &LpPool, now: i64) -> i64 {
    if m.is_settled() {
        now.saturating_sub(pool.pending_since)
    } else {
        session::open_seconds_between(m.session_schedule, pool.pending_since, now)
    }
}

// ---------------------------------------------------------------------------
// settle_position_pnl() (§6.6)
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct SettlePositionPnl<'info> {
    pub signer: Signer<'info>,

    #[account(
        seeds = [b"market", market.collateral_mint.as_ref(), market.commodity_seed()],
        bump = market.bump
    )]
    pub market: Box<Account<'info, Market>>,

    #[account(mut, seeds = [LpPool::SEED, market.key().as_ref()], bump = lp_pool.bump)]
    pub lp_pool: Box<Account<'info, LpPool>>,

    #[account(
        mut,
        constraint = position.market == market.key(),
        constraint = position.status == PositionStatus::Open @ PerpsError::PositionAlreadyClosed
    )]
    pub position: Box<Account<'info, Position>>,

    /// CHECK: Pyth price feed, validated by read_price (unused once settled)
    #[account(constraint = pyth_price_feed.key() == market.pyth_price_feed)]
    pub pyth_price_feed: AccountInfo<'info>,
}

/// Move a losing position's unrealized loss (incl. funding) into pool cash
/// and reset its PnL basis to `mark`. Equity and the liquidation test do not
/// change; neither does NAV (cash +loss, U +loss).
#[inline(never)]
pub fn settle_pnl(m: &Market, pool: &mut LpPool, position: &mut Position, mark: u64, now: i64) -> Result<u64> {
    let p = position.unrealized_pnl(mark) as i128 + position.accrued_funding(m.cumulative_funding) as i128;
    require!(p < 0, PerpsError::NothingToSettle);
    let loss = u64::try_from(-p).map_err(|_| error!(PerpsError::MathOverflow))?;
    require!(loss < position.collateral, PerpsError::SettleWouldBankrupt);

    position.collateral -= loss;
    pool.position_collateral = pool.position_collateral.checked_sub(loss).ok_or(PerpsError::BucketUnderflow)?;
    pool.cash = pool.cash.checked_add(loss).ok_or(PerpsError::MathOverflow)?;

    let f = m.cumulative_funding;
    pool_replace(pool, position.side, (position.size, position.entry_price, position.last_funding_payment), (position.size, mark, f))?;
    position.entry_price = mark;
    position.last_funding_payment = f;
    position.realized_pnl = position.realized_pnl.saturating_sub(loss as i64);
    position.last_updated_at = now;

    let v = pool_view(pool, m, mark, now, pool.params.redeem_loss_haircut_bps)?;
    touch_peak(pool, &v, now);
    Ok(loss)
}

pub fn settle_position_pnl(ctx: Context<SettlePositionPnl>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let key = ctx.accounts.position.key();
    let a = &mut *ctx.accounts;
    let mark = mark_price(&a.market, &a.pyth_price_feed, now)?;
    let loss = settle_pnl(&a.market, &mut a.lp_pool, &mut a.position, mark, now)?;
    emit!(PositionPnlSettled { position: key, mark, loss, new_collateral: a.position.collateral });
    Ok(())
}

// ---------------------------------------------------------------------------
// adl(mode, size) (§7)
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct AdlPosition<'info> {
    pub signer: Signer<'info>,

    #[account(seeds = [ProtocolConfig::SEED], bump = config.bump)]
    pub config: Box<Account<'info, ProtocolConfig>>,

    #[account(
        mut,
        seeds = [b"market", market.collateral_mint.as_ref(), market.commodity_seed()],
        bump = market.bump
    )]
    pub market: Box<Account<'info, Market>>,

    #[account(mut, seeds = [LpPool::SEED, market.key().as_ref()], bump = lp_pool.bump)]
    pub lp_pool: Box<Account<'info, LpPool>>,

    #[account(
        mut,
        constraint = position.market == market.key(),
        constraint = position.status == PositionStatus::Open @ PerpsError::PositionAlreadyClosed
    )]
    pub position: Box<Account<'info, Position>>,

    /// CHECK: only its address is used, to derive the owner's UserAccount
    #[account(address = position.owner)]
    pub position_owner: UncheckedAccount<'info>,

    #[account(
        mut,
        seeds = [b"user", position_owner.key().as_ref(), market.key().as_ref()],
        bump = user_account.bump,
        constraint = user_account.owner == position_owner.key()
    )]
    pub user_account: Box<Account<'info, UserAccount>>,

    /// CHECK: Pyth price feed, validated by read_price (unused once settled)
    #[account(constraint = pyth_price_feed.key() == market.pyth_price_feed)]
    pub pyth_price_feed: AccountInfo<'info>,
}

/// Everything an ADL checked before it applied anything.
#[derive(Clone, Copy, Debug, Default)]
pub struct AdlPlan {
    pub x: u64,
    pub profit: u64,
    pub score: u128,
    pub floor: u64,
    pub triggers: u8,
    /// Mode 1: the smallest target among the triggers that hold.
    pub target: u128,
}

/// §7.2 steps 3–8: the slice, the profit, the score and floor, and the mode's
/// eligibility on `v`.
#[inline(never)]
#[allow(clippy::too_many_arguments)]
pub fn adl_plan(m: &Market, pool: &LpPool, v: &PoolView, position: &Position, mode: u8, size: u64, mark: u64, now: i64) -> Result<AdlPlan> {
    let p = &pool.params;
    // 3. Size
    let q = position.size;
    let mut x = if size == 0 { q } else { size };
    require!(x <= q, PerpsError::InvalidPositionReduction);
    if pool_math::notional_floor(q - x, mark) < p.liq_min_remaining_notional {
        x = q;
    }
    // Departure from §7.2: a partial ADL whose remainder would not keep a
    // positive equity closes the whole position instead
    if x < q && remainder_equity(position, x, mark, m.cumulative_funding) <= 0 {
        x = q;
    }

    // 4. Profit on the slice. Departure from §7.2 ("all funding settled"):
    // the slice settles only its own share of the accrued funding and the
    // remainder keeps the rest, so a partial ADL cannot pay out the whole
    // position's funding and leave a bankrupt remainder behind
    let slice = Position { side: position.side, size: x, entry_price: position.entry_price, ..Default::default() };
    let profit = slice.unrealized_pnl(mark) as i128 + slice_funding(position, x, m.cumulative_funding);
    require!(profit > 0, PerpsError::AdlCandidateNotProfitable);
    let e = position.equity(mark, m.cumulative_funding);
    require!(e > 0, PerpsError::AdlCandidateNotProfitable);

    // 5. Score and floor
    let long = position.side == Side::Long;
    let score = pool_math::adl_score(long, mark, position.entry_price, position.mark_notional(mark), e);
    let floor = if mode == ADL_MODE_DEFICIT {
        pool_math::adl_floor_now(pending_age(m, pool, now))
    } else {
        p.adl_exposure_score_floor_bps as u64
    };
    require!(score >= floor as u128, PerpsError::AdlScoreBelowFloor);

    let (triggers, target) = if mode == ADL_MODE_DEFICIT {
        // 7. Opposite the bankrupt side
        require!(pool.adl_required(), PerpsError::AdlNotRequired);
        let opp = if long { pool.pending_bad_debt_short } else { pool.pending_bad_debt_long };
        require!(opp > 0, PerpsError::AdlWrongSide);
        // Departure from §3.1 (ADL exempt from the caps): a deficit ADL
        // that raises |net| leaves the closed position's counterparty
        // open against the pool, so it must fit the close-path caps (D14)
        // like any close; otherwise a self-crossed venue pair plus a
        // self-ADL puts unbounded net on the pool while house opens are
        // frozen
        if !m.is_settled() {
            check_close_caps(m, pool, v, position.side, x, mark, now)?;
        }
        (0u8, 0u128)
    } else {
        // 8. Exposure triggers, all against B
        let g = p.g_bps(m);
        let w = session::in_pre_close_window(m.session_schedule, now, p.weekend_cap_window_secs);
        let mut t = 0u8;
        let mut target = u128::MAX;
        if pool_math::n_trig(v.net_notional, p.hard_net_bps, v.b) {
            t |= pool_math::TRIGGER_NET;
            target = target.min(pool_math::bps_of(p.k_net_bps, v.b));
        }
        if pool_math::d_trig(drawdown_now(pool, v), p.dd_adl_bps) {
            t |= pool_math::TRIGGER_DRAWDOWN;
            target = target.min(pool_math::bps_of(p.adl_net_target_bps, v.b));
        }
        if pool_math::w_trig(w, v.net_notional, g, p.weekend_lp_loss_bps, v.b) {
            t |= pool_math::TRIGGER_WEEKEND;
            target = target.min(pool_math::weekend_target(p.weekend_lp_loss_bps, v.b, g));
        }
        require!(t != 0, PerpsError::AdlNotRequired);
        let side_ok = (v.net > 0 && long) || (v.net < 0 && !long);
        require!(side_ok, PerpsError::AdlWrongSide);
        (t, target)
    };
    Ok(AdlPlan { x, profit: u64::try_from(profit).map_err(|_| error!(PerpsError::MathOverflow))?, score, floor, triggers, target })
}

/// Equity of what a partial close of `x` of `q` leaves: the pro-rata
/// remaining collateral `C − floor(C·x/q)` plus the remainder's PnL and its
/// share of the accrued funding.
pub fn remainder_equity(position: &Position, x: u64, mark: u64, f: i64) -> i128 {
    let q = position.size;
    let c = position.collateral;
    let c_x = if x == q { c } else { (c as u128 * x as u128 / q as u128) as u64 };
    let rest = Position { size: q - x, ..*position };
    (c - c_x) as i128 + rest.unrealized_pnl(mark) as i128 + rest.accrued_funding(f) as i128
}

/// §7.2 step 9: pay the slice's profit (both haircuts, no shortfall) to the
/// owner's free collateral along with its share of collateral.
/// Returns `(paid, solvency_hc, bad_debt_hc)`.
#[inline(never)]
pub fn adl_apply(m: &mut Market, pool: &mut LpPool, v: &PoolView, position: &mut Position, user: &mut UserAccount, plan: &AdlPlan, now: i64) -> Result<(u64, u64, u64)> {
    let side = position.side;
    let q = position.size;
    let x = plan.x;
    let po = pay_profit(pool, v, side, plan.profit, false)?;
    let c = position.collateral;
    let c_x = if x == q { c } else { (c as u128 * x as u128 / q as u128) as u64 };
    pool.position_collateral = pool.position_collateral.checked_sub(c_x).ok_or(PerpsError::BucketUnderflow)?;
    let credit = c_x.checked_add(po.paid).ok_or(PerpsError::MathOverflow)?;
    user.collateral_balance = user.collateral_balance.checked_add(credit).ok_or(PerpsError::MathOverflow)?;
    pool.free_collateral = pool.free_collateral.checked_add(credit).ok_or(PerpsError::MathOverflow)?;

    if x == q {
        pool_remove(pool, side, q, position.entry_price, position.last_funding_payment)?;
    } else {
        // The remainder keeps its funding basis (and the funding it has not
        // settled)
        let f_old = position.last_funding_payment;
        pool_replace(pool, side, (q, position.entry_price, f_old), (q - x, position.entry_price, f_old))?;
    }
    oi_sub(m, side, x)?;
    house_sub(user, position, q, x)?;
    position.realized_pnl = position.realized_pnl.saturating_add(po.paid as i64);
    position.last_updated_at = now;
    if x == q {
        position.status = PositionStatus::Closed;
        user.realized_pnl = user.realized_pnl.saturating_add(position.realized_pnl);
    } else {
        position.size = q - x;
        position.collateral = c - c_x;
    }
    Ok((po.paid, po.solvency_hc, po.bad_debt_hc))
}

pub fn adl(ctx: Context<AdlPosition>, mode: u8, size: u64) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let key = ctx.accounts.position.key();
    let a = &mut *ctx.accounts;

    // 1. Mode and signer
    require!(mode == ADL_MODE_DEFICIT || mode == ADL_MODE_EXPOSURE, PerpsError::InvalidAdlMode);
    if mode == ADL_MODE_EXPOSURE {
        let s = a.signer.key();
        require!(s == a.lp_pool.adl_keeper || s == a.config.admin, PerpsError::AdlUnauthorized);
        // 2. Settled markets allow deficit mode only
        require!(!a.market.is_settled(), PerpsError::MarketSettled);
    }
    let mark = mark_price(&a.market, &a.pyth_price_feed, now)?;

    // 6. Valuation (computed before the plan, which only reads it)
    let v = pool_view(&a.lp_pool, &a.market, mark, now, a.lp_pool.params.redeem_loss_haircut_bps)?;
    touch_peak(&mut a.lp_pool, &v, now);
    let plan = adl_plan(&a.market, &a.lp_pool, &v, &a.position, mode, size, mark, now)?;

    // 9. Effects
    let (paid, solvency_hc, bad_debt_hc) = adl_apply(&mut a.market, &mut a.lp_pool, &v, &mut a.position, &mut a.user_account, &plan, now)?;

    // 10. Exposure mode stops at its target
    let after = pool_view(&a.lp_pool, &a.market, mark, now, a.lp_pool.params.redeem_loss_haircut_bps)?;
    if mode == ADL_MODE_EXPOSURE {
        let same_sign = after.net == 0 || (after.net > 0) == (v.net > 0);
        require!(same_sign && after.net_notional >= plan.target, PerpsError::AdlOvershoot);
    }

    emit!(Adl {
        mode,
        position: key,
        owner: a.position.owner,
        size_closed: plan.x,
        mark,
        profit: paid,
        solvency_hc,
        bad_debt_hc,
        score: plan.score,
        floor: plan.floor,
        triggers: plan.triggers,
        net_before: v.net,
        net_after: after.net,
        cash_after: a.lp_pool.cash,
        b: after.b,
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// write_off_bad_debt() (§7.4)
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct WriteOffBadDebt<'info> {
    pub signer: Signer<'info>,

    #[account(
        seeds = [b"market", market.collateral_mint.as_ref(), market.commodity_seed()],
        bump = market.bump
    )]
    pub market: Box<Account<'info, Market>>,

    #[account(mut, seeds = [LpPool::SEED, market.key().as_ref()], bump = lp_pool.bump)]
    pub lp_pool: Box<Account<'info, LpPool>>,
}

/// Stop pursuing pending bad debt that ADL could not recover in
/// `ADL_WRITE_OFF_SECS` open seconds. NAV already carries it (D2).
pub fn write_off_bad_debt(ctx: Context<WriteOffBadDebt>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let m = &ctx.accounts.market;
    let pool = &mut ctx.accounts.lp_pool;
    require!(pool.adl_required(), PerpsError::AdlNotRequired);
    require!(pending_age(m, pool, now) >= ADL_WRITE_OFF_SECS, PerpsError::BadDebtNotStale);
    let w = pool.pending_bad_debt_long.saturating_add(pool.pending_bad_debt_short);
    pool.pending_bad_debt_long = 0;
    pool.pending_bad_debt_short = 0;
    pool.pending_since = 0;
    pool.bad_debt_written_off_cum = pool.bad_debt_written_off_cum.saturating_add(w);
    emit!(BadDebtWrittenOff { amount: w });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instructions::open_position::{house_open_pool, house_quote};
    use crate::state::{bucket_total, pool_add, ExecutionSource, PoolParams, ADL_FLOOR_DECAY_SECS};

    const C6: u64 = 1_000_000;

    fn market() -> Market {
        Market { maintenance_margin_ratio: 500, initial_margin_ratio: 1_000, max_leverage: 20_000, taker_fee: 5, liquidation_fee: 100, max_open_interest: u64::MAX, ..Default::default() }
    }

    fn pool() -> LpPool {
        let params = PoolParams { open_fee_bps: 5, treasury_fee_bps: 1_000, insurance_target: 10 * C6, skew_scale: 2_500_000_000_000, max_impact_bps: 100, ..PoolParams::neutral() };
        LpPool { cash: 1_000_000 * C6, lp_shares: 1_000_000 * C6, params, ..Default::default() }
    }

    fn open(m: &mut Market, pool: &mut LpPool, side: Side, size: u64, entry: u64, c: u64) -> Position {
        let mut p = Position::default();
        p.init_open(Pubkey::default(), Pubkey::default(), side, size, c, entry, 5_000, m.cumulative_funding, 0, ExecutionSource::Venue, 0);
        pool_add(pool, side, size, entry, p.last_funding_payment).unwrap();
        pool.position_collateral += c;
        match side {
            Side::Long => m.long_open_interest += size,
            Side::Short => m.short_open_interest += size,
        }
        p
    }

    /// §17.1 #12: a house open is `−(margin+fee) +margin +fee`.
    #[test]
    fn house_open_is_zero_sum() {
        let (mut m, mut pool, mut user) = (market(), pool(), UserAccount { collateral_balance: 100_000 * C6, ..Default::default() });
        pool.free_collateral = user.collateral_balance;
        for (side, size) in [(Side::Long, 1_000 * C6), (Side::Short, 300 * C6), (Side::Long, 7_777_777)] {
            let q = house_quote(&m, &pool, side, size, 5_000, 75 * C6, 0, 0).unwrap();
            let before = bucket_total(&pool, &m);
            house_open_pool(&mut m, &mut pool, &mut user, side, size, 75 * C6, &q, 0).unwrap();
            assert_eq!(bucket_total(&pool, &m), before);
            assert_eq!(pool.free_collateral, user.collateral_balance);
        }
        assert!(pool.treasury > 0 && m.insurance_fund == 10 * C6 && pool.cash > 1_000_000 * C6);
        assert_eq!(pool.open_positions, 3);
    }

    /// §17.1 #12: `settle_position_pnl` is `−loss +loss`; equity unchanged.
    #[test]
    fn settle_pnl_is_zero_sum_and_keeps_equity() {
        let (mut m, mut pool) = (market(), pool());
        let mut p = open(&mut m, &mut pool, Side::Long, 10 * C6, 75 * C6, 150 * C6);
        m.cumulative_funding = 1_234;
        let e = p.equity(70 * C6, m.cumulative_funding);
        let before = bucket_total(&pool, &m);
        let loss = settle_pnl(&m, &mut pool, &mut p, 70 * C6, 1).unwrap();
        assert_eq!(bucket_total(&pool, &m), before);
        assert_eq!(loss, 50 * C6 + 12_340);
        assert_eq!(p.equity(70 * C6, m.cumulative_funding), e);
        assert_eq!((p.entry_price, p.last_funding_payment), (70 * C6, 1_234));
        assert_eq!(pool.long_entry_sum, 10 * C6 as u128 * 70 * C6 as u128);
        assert_eq!(pool.long_funding_sum, 10 * C6 as i128 * 1_234);
        assert_eq!(settle_pnl(&m, &mut pool, &mut p, 70 * C6, 1).unwrap_err(), PerpsError::NothingToSettle.into());
        assert_eq!(settle_pnl(&m, &mut pool, &mut p, 60 * C6, 1).unwrap_err(), PerpsError::SettleWouldBankrupt.into());
    }

    /// §17.1 #12: ADL is `−pay −C_x +(C_x+pay)`, partial and full.
    #[test]
    fn adl_is_zero_sum() {
        let (mut m, mut pool) = (market(), pool());
        let mut user = UserAccount::default();
        let mut short = open(&mut m, &mut pool, Side::Short, 100 * C6, 75 * C6, 1_500 * C6);
        let _long = open(&mut m, &mut pool, Side::Long, 100 * C6, 75 * C6, 1_500 * C6);
        pool.pending_bad_debt_long = 250 * C6;
        pool.pending_since = 1;
        let mark = 70 * C6;
        let now = 1 + ADL_FLOOR_DECAY_SECS; // the floor has decayed to 0
        for (size, full) in [(40 * C6, false), (0, true)] {
            let v = pool_view(&pool, &m, mark, now, 0).unwrap();
            let plan = adl_plan(&m, &pool, &v, &short, ADL_MODE_DEFICIT, size, mark, now).unwrap();
            let before = bucket_total(&pool, &m);
            adl_apply(&mut m, &mut pool, &v, &mut short, &mut user, &plan, now).unwrap();
            assert_eq!(bucket_total(&pool, &m), before);
            assert_eq!(user.collateral_balance, pool.free_collateral);
            assert_eq!(short.status == PositionStatus::Closed, full);
            if !full {
                // 40 of 100: 40% of the collateral; the slice's $200 of
                // profit all repays pending
                assert_eq!((short.size, short.collateral), (60 * C6, 900 * C6));
                assert_eq!((user.collateral_balance, pool.pending_bad_debt_long), (600 * C6, 50 * C6));
            }
        }
        // The rest: $900 + $300 − the last $50 of pending
        assert_eq!(user.collateral_balance, 600 * C6 + 900 * C6 + 250 * C6);
        assert_eq!((m.short_open_interest, pool.short_entry_sum), (0, 0));
        assert!(!pool.adl_required());
    }

    /// Review finding (partial ADL funding): the slice settles only its own
    /// share of funding, so funding that props up an under-water position
    /// cannot be paid out on a small slice. Long 100 @ $100 with $1,000 and
    /// +$800 of accrued funding, at $88: pnl −$1,200, E = $600.
    #[test]
    fn partial_adl_cannot_pay_out_the_remainders_funding() {
        let (mut m, mut pool) = (market(), pool());
        let long = open(&mut m, &mut pool, Side::Long, 100 * C6, 100 * C6, 1_000 * C6);
        m.cumulative_funding = -8 * C6 as i64; // longs receive $8 per contract
        pool.pending_bad_debt_short = C6;
        pool.pending_since = 1;
        let now = 1 + ADL_FLOOR_DECAY_SECS;
        let mark = 88 * C6;
        assert_eq!(long.equity(mark, m.cumulative_funding), 600 * C6 as i128);
        let v = pool_view(&pool, &m, mark, now, 0).unwrap();
        // 10 of 100: −$120 of PnL and $80 of the funding: not profitable
        // (before the fix the slice took all $800 and paid out $680)
        assert_eq!(adl_plan(&m, &pool, &v, &long, ADL_MODE_DEFICIT, 10 * C6, mark, now).unwrap_err(), PerpsError::AdlCandidateNotProfitable.into());
        assert_eq!(adl_plan(&m, &pool, &v, &long, ADL_MODE_DEFICIT, 0, mark, now).unwrap_err(), PerpsError::AdlCandidateNotProfitable.into());
    }

    /// A profitable partial ADL leaves the remainder its pro-rata equity and
    /// its funding basis. Same long at $96: pnl −$400, E = $1,400.
    #[test]
    fn partial_adl_keeps_the_remainders_funding() {
        let (mut m, mut pool) = (market(), pool());
        let mut user = UserAccount::default();
        let mut long = open(&mut m, &mut pool, Side::Long, 100 * C6, 100 * C6, 1_000 * C6);
        let _short = open(&mut m, &mut pool, Side::Short, 100 * C6, 100 * C6, 1_000 * C6);
        m.cumulative_funding = -8 * C6 as i64;
        pool.pending_bad_debt_short = C6;
        pool.pending_since = 1;
        let now = 1 + ADL_FLOOR_DECAY_SECS;
        let mark = 96 * C6;
        let e = long.equity(mark, m.cumulative_funding);
        assert_eq!(e, 1_400 * C6 as i128);
        let v = pool_view(&pool, &m, mark, now, 0).unwrap();
        let plan = adl_plan(&m, &pool, &v, &long, ADL_MODE_DEFICIT, 10 * C6, mark, now).unwrap();
        // −$40 of PnL + $80 of funding
        assert_eq!((plan.x, plan.profit), (10 * C6, 40 * C6));
        let before = bucket_total(&pool, &m);
        let (paid, _, bad_debt_hc) = adl_apply(&mut m, &mut pool, &v, &mut long, &mut user, &plan, now).unwrap();
        assert_eq!(bucket_total(&pool, &m), before);
        assert_eq!((paid, bad_debt_hc), (39 * C6, C6));
        // $100 of collateral + $39 to the owner; the remainder keeps its
        // $720 of funding and 90% of the equity
        assert_eq!(user.collateral_balance, 139 * C6);
        assert_eq!((long.size, long.collateral, long.last_funding_payment), (90 * C6, 900 * C6, 0));
        assert_eq!(long.equity(mark, m.cumulative_funding), 1_260 * C6 as i128);
        assert_eq!(user.collateral_balance as i128 + long.equity(mark, m.cumulative_funding), e - C6 as i128);
        assert_eq!(pool.long_funding_sum, 0);
        assert_eq!(pool.long_entry_sum, 90 * C6 as u128 * 100 * C6 as u128);
    }

    /// The funding split telescopes: slice + remainder = the position's
    /// funding, whatever the truncation.
    #[test]
    fn slice_funding_telescopes() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..20_000 {
            let size = 1 + next() % 1_000_000_000_000;
            let side = if next() % 2 == 0 { Side::Long } else { Side::Short };
            let p = Position { side, size, last_funding_payment: (next() % 2_000_000) as i64 - 1_000_000, ..Default::default() };
            let f = (next() % 20_000_000) as i64 - 10_000_000;
            let x = 1 + next() % size;
            let rest = Position { size: size - x, ..p };
            assert_eq!(slice_funding(&p, x, f) + rest.accrued_funding(f) as i128, p.accrued_funding(f) as i128);
        }
    }

    /// Review finding (deficit ADL bypassing the caps): a mode-0 ADL that
    /// raises |net| is held to the close-path caps (D14), so a self-crossed
    /// pair cannot hand the pool naked net through a self-ADL.
    #[test]
    fn deficit_adl_that_raises_net_is_capped() {
        let (mut m, mut pool) = (market(), pool());
        pool.params.k_net_bps = 10_000; // 1× B ≈ $1M of net
        // A self-crossed pair of 20,000 contracts at $75; the short is $2k up at $74.90
        let _long = open(&mut m, &mut pool, Side::Long, 20_000 * C6, 75 * C6, 150_000 * C6);
        let short = open(&mut m, &mut pool, Side::Short, 20_000 * C6, 75 * C6, 150_000 * C6);
        pool.pending_bad_debt_long = C6;
        pool.pending_since = 1;
        let now = 1 + ADL_FLOOR_DECAY_SECS;
        let mark = 74_900_000;
        let v = pool_view(&pool, &m, mark, now, 0).unwrap();
        assert_eq!(v.net, 0);
        // The whole short leg: $1.498M of net > B
        assert_eq!(adl_plan(&m, &pool, &v, &short, ADL_MODE_DEFICIT, 0, mark, now).unwrap_err(), PerpsError::PoolExposureLimit.into());
        // Half of it ($749k) fits
        let plan = adl_plan(&m, &pool, &v, &short, ADL_MODE_DEFICIT, 10_000 * C6, mark, now).unwrap();
        assert_eq!(plan.x, 10_000 * C6);
        // Settled markets are exempt (no session to carry net through)
        m.settlement_price = mark;
        adl_plan(&m, &pool, &v, &short, ADL_MODE_DEFICIT, 0, mark, now).unwrap();
    }

}
