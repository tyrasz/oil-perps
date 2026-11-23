//! `settle_position_pnl` (docs/lp-pool.md §6.6, D3), `adl` (§7) and
//! `write_off_bad_debt` (§7.4). All permissionless except exposure-mode ADL.

use anchor_lang::prelude::*;
use crate::errors::PerpsError;
use crate::instructions::update_funding::crank_funding;
use crate::events::{Adl, BadDebtWrittenOff, PositionPnlSettled};
use crate::instructions::liquidate::{house_sub, oi_sub, slice_funding};
use crate::instructions::settle_close::{check_close_caps, in_profit_close_quiet, slice_close_fee};
use crate::oracle::read_price;
use crate::pool_math;
use crate::session;
use crate::state::{
    distribute_fee, drawdown_now, pay_profit, pool_remove, pool_replace, pool_view, touch_peak, LpPool, Market, PoolView, Position,
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

/// Seconds pending bad debt booked at the debt-weighted `since` (one side's
/// `pending_since_*`, D36) has been on the books: open seconds; once settled,
/// the open seconds up to `settled_at` plus the wall-clock seconds after it
/// (D35), so settling does not turn the closed time before it into age.
/// A market settled before `settled_at` existed (0) counts wall clock.
#[inline(never)]
pub fn pending_age(m: &Market, since: i64, now: i64) -> i64 {
    if m.is_settled() {
        let s = since.max(m.settled_at);
        session::open_seconds_between(m.session_schedule, since, s).saturating_add(now.saturating_sub(s))
    } else {
        session::open_seconds_between(m.session_schedule, since, now)
    }
}

/// Mode 0 slice bound (D37): the smallest `x` whose bad-debt haircut covers
/// `opp`, the opposite side's pending, or `q` when even the whole position
/// does not. `u` and `cash` are the view's: when `u > cash` the solvency
/// haircut first scales the profit by `cash/u`, so the slice must earn
/// `ceil(opp·u/cash)`. The 4 units of slack cover the truncation of the
/// slice's PnL and funding; the result is checked, never assumed.
#[inline(never)]
pub fn deficit_slice(position: &Position, opp: u64, u: i128, cash: u64, mark: u64, f: i64) -> u64 {
    let q = position.size;
    let profit = |x: u64| -> i128 {
        let slice = Position { side: position.side, size: x, entry_price: position.entry_price, ..Default::default() };
        slice.unrealized_pnl(mark) as i128 + slice_funding(position, x, f)
    };
    let p_q = profit(q);
    if opp == 0 || p_q <= 0 {
        return q;
    }
    let need: i128 = if u > cash as i128 {
        if cash == 0 {
            return q;
        }
        pool_math::ceil_s(opp as i128 * u, cash as i128)
    } else {
        opp as i128
    };
    if need >= p_q {
        return q;
    }
    // need < p_q < 2^64 and q < 2^64, so the product fits
    let x = pool_math::ceil_s((need + 4) * q as i128, p_q).clamp(1, q as i128) as u64;
    if profit(x) < need {
        q
    } else {
        x
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
    /// Mode 0: the slice's close fee owed, `ceil(opened_notional(x)·taker_fee,
    /// 10_000)` (D22). Mode 1 is fee-free.
    pub fee: u64,
}

/// What an ADL realized.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AdlOutcome {
    /// The slice's profit paid out of cash, after both haircuts.
    pub paid: u64,
    pub solvency_hc: u64,
    pub bad_debt_hc: u64,
    /// The close fee collected (mode 0): `min(plan.fee, C_x + paid)`.
    pub fee: u64,
    /// Credited to the owner's free collateral: `C_x + paid − fee`.
    pub credit: u64,
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
    let long = position.side == Side::Long;
    // The bankrupt side this candidate repays, and its own clock (D36)
    let (opp, opp_since) = pool.pending_of(if long { Side::Short } else { Side::Long });
    if mode == ADL_MODE_DEFICIT {
        // No more than the pending needs (D37); a permissionless caller
        // cannot close a whole winner, and charge it the fee, for dust
        x = x.min(deficit_slice(position, opp, v.u, pool.cash, mark, m.cumulative_funding));
    }
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
    let score = pool_math::adl_score(long, mark, position.entry_price, position.mark_notional(mark), e);
    let floor = if mode == ADL_MODE_DEFICIT {
        pool_math::adl_floor_now(pending_age(m, opp_since, now))
    } else {
        p.adl_exposure_score_floor_bps as u64
    };
    require!(score >= floor as u128, PerpsError::AdlScoreBelowFloor);

    let (triggers, target) = if mode == ADL_MODE_DEFICIT {
        // 7. Opposite the bankrupt side
        require!(pool.adl_required(), PerpsError::AdlNotRequired);
        require!(opp > 0, PerpsError::AdlWrongSide);
        // Mode 0 always pays a profit, so it waits out the profit-close
        // quiet time like close_position (D23): otherwise a winner self-ADLs
        // ahead of its bankrupt counterparty's liquidation whenever any older
        // opposite pending exists, and escapes the D13 haircut
        require!(!in_profit_close_quiet(m, pool, now), PerpsError::ProfitCloseQuiet);
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
    // Deficit mode is permissionless, so it pays the close fee like any
    // close (D22); exposure mode is the pool's own risk control and is free
    let fee = if mode == ADL_MODE_DEFICIT { slice_close_fee(position, x, m.taker_fee) } else { 0 };
    Ok(AdlPlan { x, profit: u64::try_from(profit).map_err(|_| error!(PerpsError::MathOverflow))?, score, floor, triggers, target, fee })
}

/// §7.2 step 10: an exposure ADL stops at its target. Net keeps its sign
/// (or reaches 0), so a keeper cannot turn the pool's short into a larger
/// long, and `|net|` notional stays at or above the target.
pub fn exposure_stop_ok(net_before: i128, after: &PoolView, target: u128) -> bool {
    let same_sign = after.net == 0 || (after.net > 0) == (net_before > 0);
    same_sign && after.net_notional >= target
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
/// owner's free collateral along with its share of collateral, less the
/// mode-0 close fee. As in `settle_close`, the haircuts come first and the fee
/// is then taken from `C_x + paid`, never more.
#[inline(never)]
pub fn adl_apply(m: &mut Market, pool: &mut LpPool, v: &PoolView, position: &mut Position, user: &mut UserAccount, plan: &AdlPlan, now: i64) -> Result<AdlOutcome> {
    let side = position.side;
    let q = position.size;
    let x = plan.x;
    let po = pay_profit(pool, v, side, plan.profit, false)?;
    let c = position.collateral;
    let c_x = if x == q { c } else { (c as u128 * x as u128 / q as u128) as u64 };
    pool.position_collateral = pool.position_collateral.checked_sub(c_x).ok_or(PerpsError::BucketUnderflow)?;
    let gross = c_x.checked_add(po.paid).ok_or(PerpsError::MathOverflow)?;
    // No referral accounts reach adl, so the whole fee is split (§2.5)
    let fee = plan.fee.min(gross);
    distribute_fee(m, pool, fee)?;
    let credit = gross - fee;
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
    position.realized_pnl = position.realized_pnl.saturating_add(po.paid as i64).saturating_sub(fee as i64);
    position.last_updated_at = now;
    if x == q {
        position.status = PositionStatus::Closed;
        user.realized_pnl = user.realized_pnl.saturating_add(position.realized_pnl);
    } else {
        position.size = q - x;
        position.collateral = c - c_x;
    }
    Ok(AdlOutcome { paid: po.paid, solvency_hc: po.solvency_hc, bad_debt_hc: po.bad_debt_hc, fee, credit })
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
    // Funding due is charged first (D40); a no-op once settled
    crank_funding(&mut a.market, mark, now);

    // 6. Valuation (computed before the plan, which only reads it)
    let v = pool_view(&a.lp_pool, &a.market, mark, now, a.lp_pool.params.redeem_loss_haircut_bps)?;
    touch_peak(&mut a.lp_pool, &v, now);
    let plan = adl_plan(&a.market, &a.lp_pool, &v, &a.position, mode, size, mark, now)?;

    // 9. Effects
    let o = adl_apply(&mut a.market, &mut a.lp_pool, &v, &mut a.position, &mut a.user_account, &plan, now)?;

    // 10. Exposure mode stops at its target
    let after = pool_view(&a.lp_pool, &a.market, mark, now, a.lp_pool.params.redeem_loss_haircut_bps)?;
    if mode == ADL_MODE_EXPOSURE {
        require!(exposure_stop_ok(v.net, &after, plan.target), PerpsError::AdlOvershoot);
    }

    emit!(Adl {
        mode,
        position: key,
        owner: a.position.owner,
        size_closed: plan.x,
        mark,
        profit: o.paid,
        solvency_hc: o.solvency_hc,
        bad_debt_hc: o.bad_debt_hc,
        score: plan.score,
        floor: plan.floor,
        triggers: plan.triggers,
        net_before: v.net,
        net_after: after.net,
        cash_after: a.lp_pool.cash,
        b: after.b,
        fee: o.fee,
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
    let w = write_off_stale(m, pool, now);
    require!(w > 0, PerpsError::BadDebtNotStale);
    emit!(BadDebtWrittenOff { amount: w });
    Ok(())
}

/// Write off each side whose own pending has been on the books for
/// `ADL_WRITE_OFF_SECS` (D36). Returns the amount written off.
#[inline(never)]
pub fn write_off_stale(m: &Market, pool: &mut LpPool, now: i64) -> u64 {
    let mut w = 0u64;
    let sides = [
        (&mut pool.pending_bad_debt_long, &mut pool.pending_since_long),
        (&mut pool.pending_bad_debt_short, &mut pool.pending_since_short),
    ];
    for (pending, since) in sides {
        if *pending > 0 && pending_age(m, *since, now) >= ADL_WRITE_OFF_SECS {
            w = w.saturating_add(*pending);
            *pending = 0;
            *since = 0;
        }
    }
    pool.bad_debt_written_off_cum = pool.bad_debt_written_off_cum.saturating_add(w);
    w
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instructions::open_position::{house_open_pool, house_quote};
    use crate::state::{bucket_total, pool_add, quote_profit, ExecutionSource, PoolParams, ADL_FLOOR_DECAY_SECS};

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

    /// §17.1 #12: ADL is `−pay −C_x +fee split +(C_x+pay−fee)`, partial and
    /// full; deficit mode pays the slice's close fee (D22) and closes no more
    /// than the pending needs (D37).
    #[test]
    fn adl_is_zero_sum() {
        let (mut m, mut pool) = (market(), pool());
        let mut user = UserAccount::default();
        let mut short = open(&mut m, &mut pool, Side::Short, 100 * C6, 75 * C6, 1_500 * C6);
        let _long = open(&mut m, &mut pool, Side::Long, 100 * C6, 75 * C6, 1_500 * C6);
        pool.pending_bad_debt_long = 250 * C6;
        pool.pending_since_long = 1;
        let mark = 70 * C6;
        let now = 1 + ADL_FLOOR_DECAY_SECS; // the floor has decayed to 0
        let step = |m: &mut Market, pool: &mut LpPool, short: &mut Position, user: &mut UserAccount, size: u64| {
            let v = pool_view(pool, m, mark, now, 0).unwrap();
            let plan = adl_plan(m, pool, &v, short, ADL_MODE_DEFICIT, size, mark, now).unwrap();
            let before = bucket_total(pool, m);
            let o = adl_apply(m, pool, &v, short, user, &plan, now).unwrap();
            assert_eq!(bucket_total(pool, m), before);
            assert_eq!(user.collateral_balance, pool.free_collateral);
            (plan, o)
        };
        // 40 of 100: 40% of the collateral; the slice's $200 of profit all
        // repays pending; 5 bps of $3,000 opened notional
        let (_, o) = step(&mut m, &mut pool, &mut short, &mut user, 40 * C6);
        assert_eq!((o.paid, o.bad_debt_hc, o.fee), (0, 200 * C6, 1_500_000));
        assert_eq!((short.size, short.collateral), (60 * C6, 900 * C6));
        assert_eq!((user.collateral_balance, pool.pending_bad_debt_long), (600 * C6 - 1_500_000, 50 * C6));
        // Asking for all 60 closes only what the last $50 needs: $50 + 4
        // units of slack at $5 a contract, ceiled
        let (plan, o) = step(&mut m, &mut pool, &mut short, &mut user, 0);
        assert_eq!(plan.x, 10_000_001);
        assert_eq!((o.paid, o.bad_debt_hc, o.fee), (5, 50 * C6, 375_001));
        assert!(short.status == PositionStatus::Open);
        assert_eq!((short.size, pool.pending_bad_debt_long, pool.pending_since_long), (49_999_999, 0, 0));
        // Pending larger than the remaining profit: the whole rest
        pool.pending_bad_debt_long = 1_000 * C6;
        pool.pending_since_long = 1;
        let (plan, o) = step(&mut m, &mut pool, &mut short, &mut user, 0);
        assert_eq!(plan.x, 49_999_999);
        assert_eq!((o.paid, o.bad_debt_hc, o.fee), (0, 249_999_995, 1_875_000));
        assert!(short.status == PositionStatus::Closed);
        // All $1,500 of collateral and the $5 paid, less the three fees
        let fees = 1_500_000 + 375_001 + 1_875_000;
        assert_eq!(user.collateral_balance, 1_500 * C6 + 5 - fees);
        assert_eq!(short.realized_pnl, 5 - fees as i64);
        // The fees went through distribute_fee: 10% to treasury, the rest to
        // insurance (target $10)
        assert_eq!((pool.treasury, m.insurance_fund), (375_000, fees - 375_000));
        assert_eq!((m.short_open_interest, pool.short_entry_sum), (0, 0));
    }

    /// D37: a permissionless mode-0 ADL cannot close a whole winner for dust
    /// pending. The slice stops where its haircut covers the pending, scaled
    /// up when the solvency haircut applies (`u > cash`); the fee is on the
    /// slice only.
    #[test]
    fn deficit_adl_closes_only_what_the_pending_needs() {
        let (mut m, mut pool) = (market(), pool());
        let mut user = UserAccount::default();
        // Long 20,000 @ $75 ($1.5M), $150k of collateral, $200k up at $85
        let mut long = open(&mut m, &mut pool, Side::Long, 20_000 * C6, 75 * C6, 150_000 * C6);
        let _short = open(&mut m, &mut pool, Side::Short, 20_000 * C6, 75 * C6, 150_000 * C6);
        pool.pending_bad_debt_short = 1;
        pool.pending_since_short = 1;
        let (mark, now) = (85 * C6, 1 + ADL_FLOOR_DECAY_SECS);
        let v = pool_view(&pool, &m, mark, now, 0).unwrap();
        let plan = adl_plan(&m, &pool, &v, &long, ADL_MODE_DEFICIT, 0, mark, now).unwrap();
        // ceil(5 units · 20,000e6 / 200,000e6) = 1 base unit; a remainder
        // under liq_min_remaining would close in full, this one does not
        assert_eq!((plan.x, plan.profit, plan.fee), (1, 10, 1));
        let o = adl_apply(&mut m, &mut pool, &v, &mut long, &mut user, &plan, now).unwrap();
        // C_x = floor($150k · 1 / 20,000e6) = 7 units, + 9 paid − 1 fee
        assert_eq!((o.bad_debt_hc, o.paid, o.credit), (1, 9, 15));
        assert!(long.status == PositionStatus::Open);
        assert_eq!(long.size, 20_000 * C6 - 1);
        assert!(!pool.adl_required());

        // Insolvent at mark (u > cash): the slice must earn opp·u/cash
        let (mut m, mut pool) = (market(), super::tests::pool());
        let long = open(&mut m, &mut pool, Side::Long, 20_000 * C6, 75 * C6, 150_000 * C6);
        pool.cash = 100_000 * C6; // u = $200k
        pool.pending_bad_debt_short = 10 * C6;
        pool.pending_since_short = 1;
        let v = pool_view(&pool, &m, mark, now, 0).unwrap();
        assert_eq!(v.u, 200_000 * C6 as i128);
        let plan = adl_plan(&m, &pool, &v, &long, ADL_MODE_DEFICIT, 0, mark, now).unwrap();
        // need = $20: 2 contracts (+ slack)
        assert_eq!(plan.x, 2_000_001);
        let po = quote_profit(&pool, &v, Side::Long, plan.profit, false).unwrap();
        assert_eq!(po.bad_debt_hc, 10 * C6);
        // A caller may ask for less; the bound never raises the size
        let plan = adl_plan(&m, &pool, &v, &long, ADL_MODE_DEFICIT, 1_000_000, mark, now).unwrap();
        assert_eq!(plan.x, 1_000_000);
        // Exposure mode is not bounded by pending
        assert_eq!(deficit_slice(&long, 0, v.u, pool.cash, mark, 0), long.size);
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
        pool.pending_since_long = 1;
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
        // $100 pending: more than the 10-contract slice earns, so the D37
        // bound does not shrink it
        pool.pending_bad_debt_short = 100 * C6;
        pool.pending_since_short = 1;
        let now = 1 + ADL_FLOOR_DECAY_SECS;
        let mark = 96 * C6;
        let e = long.equity(mark, m.cumulative_funding);
        assert_eq!(e, 1_400 * C6 as i128);
        let v = pool_view(&pool, &m, mark, now, 0).unwrap();
        let plan = adl_plan(&m, &pool, &v, &long, ADL_MODE_DEFICIT, 10 * C6, mark, now).unwrap();
        // −$40 of PnL + $80 of funding
        assert_eq!((plan.x, plan.profit), (10 * C6, 40 * C6));
        let before = bucket_total(&pool, &m);
        let o = adl_apply(&mut m, &mut pool, &v, &mut long, &mut user, &plan, now).unwrap();
        assert_eq!(bucket_total(&pool, &m), before);
        assert_eq!((o.paid, o.bad_debt_hc, o.fee), (0, 40 * C6, 500_000));
        // $100 of collateral − the $0.50 fee to the owner (the $40 repays
        // pending); the remainder keeps its $720 of funding and 90% of the
        // equity
        assert_eq!(user.collateral_balance, 100 * C6 - 500_000);
        assert_eq!((long.size, long.collateral, long.last_funding_payment), (90 * C6, 900 * C6, 0));
        assert_eq!(long.equity(mark, m.cumulative_funding), 1_260 * C6 as i128);
        assert_eq!(user.collateral_balance as i128 + long.equity(mark, m.cumulative_funding), e - 40 * C6 as i128 - 500_000);
        assert_eq!(pool.long_funding_sum, 0);
        assert_eq!(pool.long_entry_sum, 90 * C6 as u128 * 100 * C6 as u128);
    }

    /// D22: a deficit slice whose profit all repays pending pays the fee from
    /// its collateral, and never more than `C_x + paid`. Exposure mode is
    /// fee-free.
    #[test]
    fn deficit_adl_fee_is_capped_by_what_the_slice_receives() {
        let (mut m, mut pool) = (market(), pool());
        let mut user = UserAccount::default();
        // Short 100 @ $75 with $1 of collateral (opened notional $7,500, fee
        // $3.75); $1 of profit at $74.99, all of it owed to pending
        let mut short = open(&mut m, &mut pool, Side::Short, 100 * C6, 75 * C6, C6);
        let _long = open(&mut m, &mut pool, Side::Long, 100 * C6, 75 * C6, 1_500 * C6);
        pool.pending_bad_debt_long = 1_000 * C6;
        pool.pending_since_long = 1;
        let (mark, now) = (74_990_000, 1 + ADL_FLOOR_DECAY_SECS);
        let v = pool_view(&pool, &m, mark, now, 0).unwrap();
        let plan = adl_plan(&m, &pool, &v, &short, ADL_MODE_DEFICIT, 0, mark, now).unwrap();
        assert_eq!((plan.profit, plan.fee), (C6, 3_750_000));
        let before = bucket_total(&pool, &m);
        let o = adl_apply(&mut m, &mut pool, &v, &mut short, &mut user, &plan, now).unwrap();
        assert_eq!(bucket_total(&pool, &m), before);
        assert_eq!((o.paid, o.bad_debt_hc, o.fee, o.credit), (0, C6, C6, 0));
        assert_eq!((user.collateral_balance, pool.free_collateral, pool.position_collateral), (0, 0, 1_500 * C6));
        assert!(short.status == PositionStatus::Closed);
        assert_eq!(short.realized_pnl, -(C6 as i64));

        // Exposure mode charges nothing
        let mut m = market();
        let mut p1 = super::tests::pool();
        p1.params.hard_net_bps = 1;
        p1.params.k_net_bps = 1;
        p1.params.adl_net_target_bps = 0;
        let long = open(&mut m, &mut p1, Side::Long, 100 * C6, 75 * C6, 1_500 * C6);
        let v = pool_view(&p1, &m, 76 * C6, 1, 0).unwrap();
        let plan = adl_plan(&m, &p1, &v, &long, ADL_MODE_EXPOSURE, 10 * C6, 76 * C6, 1).unwrap();
        assert_eq!(plan.fee, 0);
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
        // More pending than the leg's $2k of profit, so D37 asks for it all
        pool.pending_bad_debt_long = 10_000 * C6;
        pool.pending_since_long = 1;
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

    const FRIDAY_CLOSE: i64 = 1_794_607_200; // Fri 2026-11-13 17:00 ET
    const SUNDAY_OPEN: i64 = 1_794_783_600; // Sun 2026-11-15 18:00 ET
    const HOUR: i64 = 3_600;

    /// Review finding (D23 bypass): mode 0 always pays a profit, so it waits
    /// out the profit-close quiet time like close_position. Old long pending
    /// ($10, booked Friday 10:00 ET, 7 open hours old at the reopen, floor 0)
    /// must not let a short winner self-ADL before the gap's bankrupt longs
    /// are liquidated. Once settled, the quiet time runs from `settled_at`
    /// (D35).
    #[test]
    fn deficit_adl_waits_out_the_profit_close_quiet_time() {
        let (mut m, mut pool) = (market(), pool());
        m.session_schedule = session::SCHEDULE_CME_GLOBEX;
        pool.params.profit_close_quiet_secs = 300;
        let short = open(&mut m, &mut pool, Side::Short, 100 * C6, 75 * C6, 1_500 * C6);
        let _long = open(&mut m, &mut pool, Side::Long, 100 * C6, 75 * C6, 1_500 * C6);
        pool.pending_bad_debt_long = 10 * C6;
        pool.pending_since_long = FRIDAY_CLOSE - 7 * HOUR;
        let mark = 64 * C6;
        let plan = |m: &Market, pool: &LpPool, now: i64| {
            let v = pool_view(pool, m, mark, now, 0).unwrap();
            adl_plan(m, pool, &v, &short, ADL_MODE_DEFICIT, 0, mark, now)
        };
        for t in [SUNDAY_OPEN, SUNDAY_OPEN + 5, SUNDAY_OPEN + 299] {
            assert_eq!(plan(&m, &pool, t).unwrap_err(), PerpsError::ProfitCloseQuiet.into());
        }
        let ok = plan(&m, &pool, SUNDAY_OPEN + 300).unwrap();
        assert_eq!(ok.floor, 0);
        // The daily reopen too; 0 turns it off
        assert_eq!(plan(&m, &pool, SUNDAY_OPEN + 86_400 + 10).unwrap_err(), PerpsError::ProfitCloseQuiet.into());
        pool.params.profit_close_quiet_secs = 0;
        plan(&m, &pool, SUNDAY_OPEN + 86_400 + 10).unwrap();
        pool.params.profit_close_quiet_secs = 300;
        // Settled: the quiet time counts from settlement, in wall seconds
        let at = SUNDAY_OPEN + 1_000;
        m.settlement_price = mark;
        m.settled_at = at;
        assert_eq!(plan(&m, &pool, at + 299).unwrap_err(), PerpsError::ProfitCloseQuiet.into());
        plan(&m, &pool, at + 300).unwrap();
        // A market settled before settled_at existed has none
        m.settled_at = 0;
        plan(&m, &pool, at).unwrap();
    }

    /// Review finding (retroactive wall clock, D35): once settled, pending age
    /// is the open time up to `settled_at` plus wall time after it. Debt
    /// booked Fri 15:00 ET (2 open hours before the close) and a Sunday-noon
    /// settlement leave 22 h before it can be written off, not 0, and the
    /// floor still needs 4 more hours.
    #[test]
    fn settled_pending_age_counts_open_time_until_settlement() {
        let mut m = market();
        m.session_schedule = session::SCHEDULE_CME_GLOBEX;
        let since = FRIDAY_CLOSE - 2 * HOUR;
        let at = SUNDAY_OPEN - 6 * HOUR; // Sun 12:00 ET, closed
        assert_eq!(pending_age(&m, since, at), 2 * HOUR);
        m.settlement_price = 70 * C6;
        m.settled_at = at;
        assert_eq!(pending_age(&m, since, at), 2 * HOUR);
        assert_eq!(pending_age(&m, since, at + 22 * HOUR), ADL_WRITE_OFF_SECS);
        // Booked after settlement: wall clock from the booking
        assert_eq!(pending_age(&m, at + 100, at + 200), 100);
        // Settled before settled_at existed: wall clock
        m.settled_at = 0;
        assert_eq!(pending_age(&m, since, at), at - since);
        m.settled_at = at;

        let mut pool = pool();
        pool.pending_bad_debt_long = 5;
        pool.pending_since_long = since;
        assert_eq!(write_off_stale(&m, &mut pool, at), 0);
        assert_eq!(write_off_stale(&m, &mut pool, at + 22 * HOUR - 1), 0);
        assert_eq!(write_off_stale(&m, &mut pool, at + 22 * HOUR), 5);
        assert_eq!((pool.pending_bad_debt_long, pool.pending_since_long, pool.bad_debt_written_off_cum), (0, 0, 5));
        // The mode-0 floor reaches 0 after 6 h of age: 2 h before and 4 h after
        assert!(pool_math::adl_floor_now(pending_age(&m, since, at + 4 * HOUR - 1)) > 0);
        assert_eq!(pool_math::adl_floor_now(pending_age(&m, since, at + 4 * HOUR)), 0);
    }

    /// Review finding (one shared clock, D36): fresh debt on one side keeps
    /// its own age for its mode-0 floor and its write-off, however old the
    /// other side's debt is.
    #[test]
    fn each_sides_pending_has_its_own_floor_and_write_off() {
        let (mut m, mut pool) = (market(), pool());
        let long = open(&mut m, &mut pool, Side::Long, 100 * C6, 75 * C6, 1_500 * C6);
        let _short = open(&mut m, &mut pool, Side::Short, 100 * C6, 75 * C6, 1_500 * C6);
        let t1 = 1 + 23 * HOUR;
        pool.pending_bad_debt_long = 900 * C6;
        pool.pending_since_long = 1;
        pool.pending_bad_debt_short = 100 * C6;
        pool.pending_since_short = t1;
        let mark = 76 * C6;
        let plan = |pool: &LpPool, now: i64| {
            let v = pool_view(pool, &m, mark, now, 0).unwrap();
            adl_plan(&m, pool, &v, &long, ADL_MODE_DEFICIT, 0, mark, now)
        };
        // The long winner repays the fresh short debt: the floor starts at
        // the top, not at the merged ~20.7 h age
        assert_eq!(plan(&pool, t1).unwrap_err(), PerpsError::AdlScoreBelowFloor.into());
        assert_eq!(plan(&pool, t1 + ADL_FLOOR_DECAY_SECS).unwrap().floor, 0);
        // An hour later the old long debt is writable; the short is not
        assert_eq!(write_off_stale(&m, &mut pool, t1 + HOUR - 1), 0);
        assert_eq!(write_off_stale(&m, &mut pool, t1 + HOUR), 900 * C6);
        assert_eq!(
            (pool.pending_bad_debt_long, pool.pending_since_long, pool.pending_bad_debt_short, pool.pending_since_short),
            (0, 0, 100 * C6, t1)
        );
        assert_eq!(write_off_stale(&m, &mut pool, t1 + ADL_WRITE_OFF_SECS - 1), 0);
        assert_eq!(write_off_stale(&m, &mut pool, t1 + ADL_WRITE_OFF_SECS), 100 * C6);
        assert!(!pool.adl_required());
    }

    /// Mutation finding: `settle_pnl` needs `loss < C`; a loss equal to the
    /// collateral would leave an open position with zero collateral and zero
    /// equity. It also touches the drawdown peak (§2.2).
    #[test]
    fn settle_pnl_rejects_a_loss_equal_to_collateral_and_touches_the_peak() {
        let (mut m, mut pool) = (market(), pool());
        let mut p = open(&mut m, &mut pool, Side::Long, 10 * C6, 75 * C6, 50 * C6);
        assert_eq!(settle_pnl(&m, &mut pool, &mut p, 70 * C6, 1).unwrap_err(), PerpsError::SettleWouldBankrupt.into());
        pool.nps_peak = 1;
        let loss = settle_pnl(&m, &mut pool, &mut p, 70 * C6 + 1, 7).unwrap();
        assert_eq!((loss, p.collateral), (49_999_990, 10));
        let v = pool_view(&pool, &m, 70 * C6 + 1, 7, 0).unwrap();
        assert_eq!((pool.nps_peak, pool.nps_peak_at), (v.nps, 7));
    }

    /// Mutation finding (belt-and-braces guard, D24): `remainder_equity` is
    /// the pro-rata collateral plus the remainder's own PnL and funding. Its
    /// `≤ 0` guard in `adl_plan` cannot trigger for a profitable slice (the
    /// remainder's PnL and funding have the slice's sign up to a unit), so it
    /// is pinned here directly.
    #[test]
    fn remainder_equity_is_the_remainders_own_equity() {
        // Long 100 @ $100 with $100; longs owe $5 a contract
        let p = Position { side: Side::Long, size: 100 * C6, entry_price: 100 * C6, collateral: 100 * C6, ..Default::default() };
        let f = 5 * C6 as i64;
        // Half: $50 of collateral, $0 of PnL, −$250 of funding
        assert_eq!(remainder_equity(&p, 50 * C6, 100 * C6, f), -200 * C6 as i128);
        // and that slice is not profitable, so adl_plan never reaches the guard
        let slice = Position { size: 50 * C6, ..p };
        assert!(slice.unrealized_pnl(100 * C6) as i128 + slice_funding(&p, 50 * C6, f) <= 0);
        // Non-divisible collateral: the remainder keeps C − floor(C·x/q)
        let q = Position { collateral: 100 * C6 + 1, ..p };
        assert_eq!(remainder_equity(&q, 30 * C6, 100 * C6, 0), (100 * C6 + 1 - 30 * C6) as i128);
    }

    /// Mutation finding: a deficit ADL whose remainder notional is exactly
    /// `liq_min_remaining_notional` stays partial (`<`, not `≤`).
    #[test]
    fn adl_remainder_at_the_minimum_notional_stays_open() {
        let (mut m, mut pool) = (market(), pool());
        assert_eq!(pool.params.liq_min_remaining_notional, 100 * C6);
        let short = open(&mut m, &mut pool, Side::Short, 100 * C6, 75 * C6, 1_500 * C6);
        let _long = open(&mut m, &mut pool, Side::Long, 100 * C6, 75 * C6, 1_500 * C6);
        pool.pending_bad_debt_long = 10_000 * C6;
        pool.pending_since_long = 1;
        let (mark, now) = (50 * C6, 1 + ADL_FLOOR_DECAY_SECS);
        let v = pool_view(&pool, &m, mark, now, 0).unwrap();
        // 2 contracts at $50 = $100 left
        assert_eq!(adl_plan(&m, &pool, &v, &short, ADL_MODE_DEFICIT, 98 * C6, mark, now).unwrap().x, 98 * C6);
        assert_eq!(adl_plan(&m, &pool, &v, &short, ADL_MODE_DEFICIT, 98 * C6 + 1, mark, now).unwrap().x, 100 * C6);
    }

    /// Mutation finding (§7.2 step 8): exposure mode closes only the side
    /// that carries net. With traders net long, a profitable short is the
    /// wrong side, and the mirror.
    #[test]
    fn exposure_adl_closes_only_the_net_side() {
        for net_long in [true, false] {
            let (mut m, mut pool) = (market(), pool());
            pool.params.hard_net_bps = 1;
            pool.params.k_net_bps = 1;
            pool.params.adl_net_target_bps = 0;
            let (big, small) = if net_long { (Side::Long, Side::Short) } else { (Side::Short, Side::Long) };
            let _b = open(&mut m, &mut pool, big, 200 * C6, 75 * C6, 3_000 * C6);
            let s = open(&mut m, &mut pool, small, 100 * C6, 75 * C6, 1_500 * C6);
            // The small side's winner
            let mark = if net_long { 74 * C6 } else { 76 * C6 };
            let v = pool_view(&pool, &m, mark, 1, 0).unwrap();
            assert_eq!(v.net > 0, net_long);
            assert_eq!(adl_plan(&m, &pool, &v, &s, ADL_MODE_EXPOSURE, 0, mark, 1).unwrap_err(), PerpsError::AdlWrongSide.into());
        }
    }

    /// Mutation finding (§7.2 step 10): the exposure stop rejects a slice that
    /// flips net past zero even when `|net|` stays above the target, and
    /// accepts landing exactly on the target.
    #[test]
    fn exposure_stop_keeps_the_sign_and_the_target() {
        let after = |net: i128, nn: u128| PoolView { net, net_notional: nn, ..Default::default() };
        // +100 → −150 with |net| above a 30-contract target: a flip
        assert!(!exposure_stop_ok(100, &after(-150, 150 * 75), 30 * 75));
        assert!(!exposure_stop_ok(-100, &after(150, 150 * 75), 30 * 75));
        // Same side: at or above the target only
        assert!(exposure_stop_ok(100, &after(40, 40 * 75), 30 * 75));
        assert!(exposure_stop_ok(100, &after(30, 30 * 75), 30 * 75));
        assert!(!exposure_stop_ok(100, &after(29, 29 * 75), 30 * 75));
        // To exactly 0 with a 0 target
        assert!(exposure_stop_ok(100, &after(0, 0), 0));
    }

    /// Mutation finding (D28): exposure ADL's drawdown trigger reads the peak
    /// after `touch_peak`, so a 20% drawdown against a peak that is exactly
    /// `DD_WINDOW_SECS` old (reset by this instruction) triggers nothing;
    /// a second younger, it does.
    #[test]
    fn exposure_adl_drawdown_trigger_uses_the_touched_peak() {
        use crate::state::DD_WINDOW_SECS;
        let now = 10 * DD_WINDOW_SECS;
        for (age, triggers) in [(DD_WINDOW_SECS, false), (DD_WINDOW_SECS - 1, true)] {
            let (mut m, mut pool) = (market(), pool());
            pool.params.dd_adl_bps = 1_500;
            pool.params.adl_net_target_bps = 5_000;
            let long = open(&mut m, &mut pool, Side::Long, 1_000 * C6, 75 * C6, 15_000 * C6);
            let mark = 76 * C6;
            let v0 = pool_view(&pool, &m, mark, now, 0).unwrap();
            pool.nps_peak = v0.nps / 4 * 5; // a 20% drawdown
            pool.nps_peak_at = now - age;
            let v = pool_view(&pool, &m, mark, now, 0).unwrap();
            assert!(v.dd_bps >= 1_999);
            touch_peak(&mut pool, &v, now);
            let r = adl_plan(&m, &pool, &v, &long, ADL_MODE_EXPOSURE, 10 * C6, mark, now);
            if triggers {
                assert_eq!(r.unwrap().triggers, pool_math::TRIGGER_DRAWDOWN);
            } else {
                assert_eq!(r.unwrap_err(), PerpsError::AdlNotRequired.into());
            }
        }
    }

}
