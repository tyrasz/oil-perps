//! The shared body of a full close (docs/lp-pool.md §4.5 steps 2–7), used by
//! `close_settled_position` (§10.5) and by `close_position`.
//!
//! The caller picks the price (step 1: settlement price, or the oracle and
//! the skew execution price), does the token transfers and the logging
//! (steps 8–9). Everything here only moves value between buckets.

use anchor_lang::prelude::*;
use crate::errors::PerpsError;
use crate::instructions::close_position::{referral_fees, referral_reward_paid};
use crate::pool_math;
use crate::session;
use crate::state::{
    absorb_deficit, distribute_fee, pay_profit, pool_remove, pool_view, touch_peak, ExecutionSource, LpPool, Market,
    PoolView, Position, PositionStatus, Side, UserAccount,
};

/// What a close realized. `fee` is what was collected (`fee_c`), after any
/// referral discount; `payout` is the equity owed to the owner.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CloseOutcome {
    pub pnl: i64,
    pub funding: i64,
    /// Realized PnL incl. funding after the haircuts, before the fee.
    pub p_eff: i64,
    pub solvency_hc: u64,
    pub bad_debt_hc: u64,
    /// The fee after the referral discount (what the position owed).
    pub fee_charged: u64,
    /// `fee_c`: the part of `fee_charged` the position covered.
    pub fee: u64,
    pub referral_paid: u64,
    pub payout: u64,
    pub deficit: u64,
}

/// Close-path caps (§3 "Net (close)" and "Weekend net", D14): a close that
/// raises `|net|` must fit the net cap and, with the weekend bound on
/// (`weekend_cap_window_secs > 0`, D43), the weekend cap, both against `B`;
/// the weekend cap's `B` is net of the close's skew-premium shortfall at
/// `exec` (`premium_shortfall`; mode-0 ADL closes at the mark, so all of
/// its `Δx`). Not applied to settled markets or liquidations; mode-0 ADL
/// uses it (D25).
#[inline(never)]
#[allow(clippy::too_many_arguments)]
pub fn check_close_caps(m: &Market, pool: &LpPool, v: &PoolView, side: Side, size: u64, mark: u64, exec: u64) -> Result<()> {
    let s = size as i128;
    let after = match side {
        Side::Long => v.net - s,
        Side::Short => v.net + s,
    };
    if after.unsigned_abs() <= v.net.unsigned_abs() {
        return Ok(());
    }
    let nn = pool_math::ceil_u(
        after.unsigned_abs().checked_mul(mark as u128).ok_or(PerpsError::MathOverflow)?,
        1_000_000,
    );
    let p = &pool.params;
    require!(pool_math::net_cap_ok(nn, p.k_net_bps, v.b), PerpsError::PoolExposureLimit);
    // Always on with the weekend bound (D43), not only inside W
    if p.weekend_bound_on() {
        let short = pool_math::premium_shortfall(v.net, after, mark, exec, size, p.skew_scale, p.max_impact_bps)
            .ok_or(PerpsError::MathOverflow)?;
        require!(
            pool_math::weekend_cap_ok(nn, p.g_bps(m), p.weekend_lp_loss_bps, v.b.saturating_sub(short)),
            PerpsError::PoolWeekendExposureLimit
        );
    }
    Ok(())
}

/// `ceil(opened_notional(size)·taker_fee, 10_000)` (D21).
pub fn close_base_fee(position: &Position, taker_fee: u32) -> u64 {
    slice_close_fee(position, position.size, taker_fee)
}

/// The close fee of a slice `x`: `ceil(opened_notional(x)·taker_fee, 10_000)`.
/// Deficit-mode ADL charges it on each slice (D22).
pub fn slice_close_fee(position: &Position, x: u64, taker_fee: u32) -> u64 {
    let n = position.opened_notional(x) as u128;
    let fee = pool_math::ceil_u(n * taker_fee as u128, 10_000);
    u64::try_from(fee).unwrap_or(u64::MAX)
}

/// Profit-close quiet time (D23): within `profit_close_quiet_secs` of a
/// session open (never for a schedule that does not close), or, once
/// settled, of `settled_at` (D35), so the bankrupt losers a settlement price
/// creates can be closed and booked before their winners are paid.
pub fn in_profit_close_quiet(m: &Market, pool: &LpPool, now: i64) -> bool {
    let q = pool.params.profit_close_quiet_secs as i64;
    if m.is_settled() {
        m.settled_at > 0 && now.saturating_sub(m.settled_at) < q
    } else {
        session::seconds_since_open(m.session_schedule, now).map_or(false, |t| t < q)
    }
}

fn to_i64(x: i128) -> i64 {
    x.clamp(i64::MIN as i128, i64::MAX as i128) as i64
}

/// §4.5 steps 2–7 at `mark` (valuation) and `exec` (the position's price).
/// `referral` is `(discount_bps, reward_bps)` when a valid referral applies.
/// On return the position is Closed, its collateral has left
/// `position_collateral`, the aggregates and OI no longer hold it, and the
/// fee has been split; the caller pays `payout` (and `referral_paid`) out.
#[inline(never)]
#[allow(clippy::too_many_arguments)]
pub fn settle_close(
    m: &mut Market,
    pool: &mut LpPool,
    position: &mut Position,
    user: &mut UserAccount,
    position_key: Pubkey,
    mark: u64,
    exec: u64,
    now: i64,
    referral: Option<(u16, u16)>,
) -> Result<CloseOutcome> {
    let settled = m.is_settled();
    let side = position.side;
    let size = position.size;
    let c = position.collateral;

    // 2. Value and caps
    let v = pool_view(pool, m, mark, now, pool.params.redeem_loss_haircut_bps)?;
    touch_peak(pool, &v, now);
    let pnl = position.unrealized_pnl(exec);
    let fnd = position.accrued_funding(m.cumulative_funding);
    let p = pnl as i128 + fnd as i128;
    // A winner waits out the quiet time after a session open (or after
    // settlement), so the keeper can liquidate or close its bankrupt matched
    // counterparty first and the deficit reaches the bad-debt haircut (D23,
    // D35)
    require!(!(p > 0 && in_profit_close_quiet(m, pool, now)), PerpsError::ProfitCloseQuiet);
    if !settled {
        check_close_caps(m, pool, &v, side, size, mark, exec)?;
    }

    // 3. Profit (paid out of cash, after the haircuts) or loss
    let (p_eff, solvency_hc, bad_debt_hc) = if p > 0 {
        let amount = u64::try_from(p).map_err(|_| error!(PerpsError::MathOverflow))?;
        let po = pay_profit(pool, &v, side, amount, false)?;
        (po.paid as i128, po.solvency_hc, po.bad_debt_hc)
    } else {
        (p, 0, 0)
    };

    // 4. Fee and referral
    let base_fee = close_base_fee(position, m.taker_fee);
    let (fee, reward) = referral_fees(base_fee, referral);
    let eq_before_fee = c as i128 + p_eff;
    let fee_c = eq_before_fee.clamp(0, fee as i128) as u64;
    let referral_paid = referral_reward_paid(reward, fee, to_i64(eq_before_fee));

    // 5. Equity
    let e = eq_before_fee - fee_c as i128;

    // 6. Pool
    let (payout, deficit) = if e >= 0 {
        if p_eff <= 0 {
            let loss = u64::try_from(-p_eff).map_err(|_| error!(PerpsError::MathOverflow))?;
            pool.cash = pool.cash.checked_add(loss).ok_or(PerpsError::MathOverflow)?;
        }
        distribute_fee(m, pool, fee_c.checked_sub(referral_paid).ok_or(PerpsError::BucketUnderflow)?)?;
        (u64::try_from(e).map_err(|_| error!(PerpsError::MathOverflow))?, 0u64)
    } else {
        // Only when p_eff < 0, so fee_c == referral_paid == 0
        let d = u64::try_from(-e).map_err(|_| error!(PerpsError::MathOverflow))?;
        pool.cash = pool.cash.checked_add(c).ok_or(PerpsError::MathOverflow)?;
        absorb_deficit(m, pool, position_key, d, side, v.nav_redeem, now)?;
        (0u64, d)
    };
    pool.position_collateral = pool.position_collateral.checked_sub(c).ok_or(PerpsError::BucketUnderflow)?;
    pool_remove(pool, side, size, position.entry_price, position.last_funding_payment)?;
    match side {
        Side::Long => {
            m.long_open_interest = m.long_open_interest.checked_sub(size).ok_or(PerpsError::MathOverflow)?;
        }
        Side::Short => {
            m.short_open_interest = m.short_open_interest.checked_sub(size).ok_or(PerpsError::MathOverflow)?;
        }
    }
    if position.execution_source == ExecutionSource::OrderBook {
        user.house_entry_notional = user
            .house_entry_notional
            .checked_sub(position.opened_notional(size))
            .ok_or(PerpsError::BucketUnderflow)?;
    }

    // 7. Position and user
    position.status = PositionStatus::Closed;
    position.realized_pnl = position.realized_pnl.saturating_add(to_i64(p_eff - fee_c as i128));
    position.last_updated_at = now;
    user.realized_pnl = user.realized_pnl.saturating_add(position.realized_pnl);
    user.total_trades = user.total_trades.saturating_add(1);

    Ok(CloseOutcome {
        pnl,
        funding: fnd,
        p_eff: to_i64(p_eff),
        solvency_hc,
        bad_debt_hc,
        fee_charged: fee,
        fee: fee_c,
        referral_paid,
        payout,
        deficit,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{bucket_total, pool_add, PoolParams};

    const C6: u64 = 1_000_000;

    fn market() -> Market {
        Market {
            maintenance_margin_ratio: 500,
            initial_margin_ratio: 1_000,
            taker_fee: 5,
            max_open_interest: u64::MAX,
            ..Default::default()
        }
    }

    fn mk_pool(cash: u64) -> LpPool {
        LpPool { cash, lp_shares: cash.max(1), params: PoolParams::neutral(), ..Default::default() }
    }

    /// Open `contracts` at `entry` dollars with `collateral` dollars, mirrored
    /// into the pool and the market as the house open will do it.
    fn open(m: &mut Market, pool: &mut LpPool, user: &mut UserAccount, side: Side, contracts: u64, entry: u64, collateral: u64) -> Position {
        let mut p = Position::default();
        p.init_open(Pubkey::default(), Pubkey::default(), side, contracts * C6, collateral * C6, entry * C6, 5_000, m.cumulative_funding, 0, ExecutionSource::OrderBook, 0);
        pool_add(pool, side, p.size, p.entry_price, p.last_funding_payment).unwrap();
        pool.position_collateral += p.collateral;
        match side {
            Side::Long => m.long_open_interest += p.size,
            Side::Short => m.short_open_interest += p.size,
        }
        user.house_entry_notional += p.opened_notional(p.size);
        p
    }

    /// Run settle_close at `mark` and check the bucket step: the buckets fall
    /// by exactly what leaves (`payout + referral_paid`), §11.
    fn close(m: &mut Market, pool: &mut LpPool, user: &mut UserAccount, p: &mut Position, mark: u64, referral: Option<(u16, u16)>) -> CloseOutcome {
        let before = bucket_total(pool, m);
        let o = settle_close(m, pool, p, user, Pubkey::new_unique(), mark * C6, mark * C6, 1, referral).unwrap();
        assert_eq!(bucket_total(pool, m), before - o.payout as u128 - o.referral_paid as u128);
        assert!(p.status == PositionStatus::Closed);
        o
    }

    #[test]
    fn matched_pair_closes_are_zero_sum_and_unwind_the_aggregates() {
        let (mut m, mut pool, mut user) = (market(), mk_pool(1_000_000 * C6), UserAccount::default());
        let mut long = open(&mut m, &mut pool, &mut user, Side::Long, 10, 75, 150);
        let mut short = open(&mut m, &mut pool, &mut user, Side::Short, 10, 75, 150);
        // Winner first: paid from cash; $750 at 5 bps = $0.375 fee
        let o = close(&mut m, &mut pool, &mut user, &mut short, 70, None);
        assert_eq!((o.p_eff, o.fee, o.payout, o.deficit), (50 * C6 as i64, 375_000, 199_625_000, 0));
        assert_eq!(pool.cash, 1_000_000 * C6 - 50 * C6);
        let o = close(&mut m, &mut pool, &mut user, &mut long, 70, None);
        assert_eq!((o.p_eff, o.payout), (-50 * C6 as i64, 99_625_000));
        // Cash is back where it started; fees went to insurance (neutral target)
        assert_eq!(pool.cash, 1_000_000 * C6);
        assert_eq!(m.insurance_fund, 750_000);
        assert_eq!((pool.open_positions, pool.long_entry_sum, pool.short_entry_sum, pool.position_collateral), (0, 0, 0, 0));
        assert_eq!((m.long_open_interest, m.short_open_interest, user.house_entry_notional), (0, 0, 0));
        assert_eq!(long.realized_pnl, -50 * C6 as i64 - 375_000);
    }

    #[test]
    fn deficit_goes_through_the_waterfall() {
        let (mut m, mut pool, mut user) = (market(), mk_pool(1_000_000 * C6), UserAccount::default());
        m.insurance_fund = 10 * C6;
        let mut long = open(&mut m, &mut pool, &mut user, Side::Long, 10, 75, 75);
        let _short = open(&mut m, &mut pool, &mut user, Side::Short, 10, 75, 150);
        // $150 loss on $75: $75 deficit, $10 from insurance, the rest the LP absorbs
        let o = close(&mut m, &mut pool, &mut user, &mut long, 60, None);
        assert_eq!((o.payout, o.fee, o.deficit), (0, 0, 75 * C6));
        assert_eq!(m.insurance_fund, 0);
        assert_eq!(pool.cash, 1_000_000 * C6 + 75 * C6 + 10 * C6);
        assert_eq!((pool.lp_absorbed_window, pool.ins_paid_window, pool.bad_debt_cum), (65 * C6, 10 * C6, 75 * C6));
        assert!(!pool.adl_required());
        assert_eq!(user.realized_pnl, -150 * C6 as i64);
    }

    #[test]
    fn winner_needs_cash_and_pays_opposite_pending_first() {
        let (mut m, mut pool, mut user) = (market(), mk_pool(20 * C6), UserAccount::default());
        let mut short = open(&mut m, &mut pool, &mut user, Side::Short, 10, 75, 150);
        let _other = open(&mut m, &mut pool, &mut user, Side::Short, 10, 75, 150);
        // Two unhedged shorts up $50 each with $20 of cash: U = $100 > cash, so
        // each winner is paid pro rata, 50·20/100 = $10
        let o = close(&mut m, &mut pool, &mut user, &mut short, 70, None);
        assert_eq!((o.p_eff, o.solvency_hc), (10 * C6 as i64, 40 * C6));

        // Pending long bad debt is repaid by a short winner before it is paid
        let (mut m2, mut pool2, mut user2) = (market(), mk_pool(1_000_000 * C6), UserAccount::default());
        let mut short2 = open(&mut m2, &mut pool2, &mut user2, Side::Short, 10, 75, 150);
        let _long2 = open(&mut m2, &mut pool2, &mut user2, Side::Long, 10, 75, 150);
        pool2.pending_bad_debt_long = 30 * C6;
        pool2.pending_since_long = 1;
        let o = close(&mut m2, &mut pool2, &mut user2, &mut short2, 70, None);
        assert_eq!((o.p_eff, o.bad_debt_hc), (20 * C6 as i64, 30 * C6));
        assert_eq!((pool2.pending_bad_debt_long, pool2.pending_since_long, pool2.bad_debt_recovered_cum), (0, 0, 30 * C6));
    }

    #[test]
    fn winner_without_cash_fails() {
        let (mut m, mut pool, mut user) = (market(), mk_pool(10 * C6), UserAccount::default());
        let mut short = open(&mut m, &mut pool, &mut user, Side::Short, 10, 75, 150);
        let mut long = open(&mut m, &mut pool, &mut user, Side::Long, 10, 75, 150);
        m.settlement_price = 70 * C6; // settled: no close-path caps
        // U = 0 (matched), so no solvency haircut, and $50 > $10 of cash
        let e = settle_close(&mut m, &mut pool, &mut short, &mut user, Pubkey::default(), 70 * C6, 70 * C6, 1, None).unwrap_err();
        assert_eq!(e, PerpsError::PoolCashInsufficient.into());
        // The loser settles in first, then the winner is paid
        close(&mut m, &mut pool, &mut user, &mut long, 70, None);
        close(&mut m, &mut pool, &mut user, &mut short, 70, None);
    }

    #[test]
    fn referral_reward_leaves_with_the_payout() {
        let (mut m, mut pool, mut user) = (market(), mk_pool(1_000_000 * C6), UserAccount::default());
        let mut long = open(&mut m, &mut pool, &mut user, Side::Long, 10, 75, 150);
        // 10% discount: fee $0.3375, 20% of it ($0.0675) to the referrer
        let o = close(&mut m, &mut pool, &mut user, &mut long, 75, Some((1_000, 2_000)));
        assert_eq!((o.fee, o.referral_paid, o.payout), (337_500, 67_500, 150 * C6 - 337_500));
        assert_eq!(m.insurance_fund, 337_500 - 67_500);
    }

    #[test]
    fn close_fee_is_a_ceiling_of_opened_notional() {
        let mut p = Position::default();
        // 1 contract at $75.000001: notional 75.000001, fee 0.0375000005 → 37_501
        p.init_open(Pubkey::default(), Pubkey::default(), Side::Long, C6, C6, 75_000_001, 1_000, 0, 0, ExecutionSource::OrderBook, 0);
        assert_eq!(close_base_fee(&p, 5), 37_501);
        p.entry_price = 1; // settle_position_pnl rewrites entry, not the fee basis (D21)
        assert_eq!(close_base_fee(&p, 5), 37_501);
        p.opened_price = 75 * C6;
        assert_eq!(close_base_fee(&p, 5), 37_500);
    }

    /// D23: inside `profit_close_quiet_secs` after a session open a winner
    /// cannot close; a loser can, and a settled market is exempt.
    #[test]
    fn profitable_closes_wait_out_the_quiet_time() {
        let sunday_open = 1_794_783_600; // Sun 2026-11-15 18:00 ET
        let (mut m, mut pool, mut user) = (market(), mk_pool(1_000_000 * C6), UserAccount::default());
        m.session_schedule = session::SCHEDULE_CME_GLOBEX;
        pool.params.profit_close_quiet_secs = 300;
        let mut long = open(&mut m, &mut pool, &mut user, Side::Long, 10, 75, 150);
        let mut short = open(&mut m, &mut pool, &mut user, Side::Short, 10, 75, 150);
        let at = |m: &mut Market, pool: &mut LpPool, user: &mut UserAccount, p: &mut Position, t: i64| {
            settle_close(m, pool, p, user, Pubkey::default(), 70 * C6, 70 * C6, t, None)
        };
        let e = at(&mut m, &mut pool, &mut user, &mut short, sunday_open + 299).unwrap_err();
        assert_eq!(e, PerpsError::ProfitCloseQuiet.into());
        assert!(short.status == PositionStatus::Open);
        // The loser closes at once, and so does a close at exactly P = 0
        at(&mut m, &mut pool, &mut user, &mut long, sunday_open).unwrap();
        let mut flat = open(&mut m, &mut pool, &mut user, Side::Short, 10, 70, 150);
        at(&mut m, &mut pool, &mut user, &mut flat, sunday_open + 1).unwrap();
        at(&mut m, &mut pool, &mut user, &mut short, sunday_open + 300).unwrap();
        // 0 turns it off; settled markets and 24/7 schedules never wait
        let mut w: Vec<Position> = (0..3).map(|_| open(&mut m, &mut pool, &mut user, Side::Short, 10, 75, 150)).collect();
        pool.params.profit_close_quiet_secs = 0;
        at(&mut m, &mut pool, &mut user, &mut w[0], sunday_open).unwrap();
        pool.params.profit_close_quiet_secs = 300;
        m.session_schedule = session::SCHEDULE_ALWAYS_OPEN;
        at(&mut m, &mut pool, &mut user, &mut w[1], sunday_open).unwrap();
        m.session_schedule = session::SCHEDULE_CME_GLOBEX;
        m.settlement_price = 70 * C6;
        at(&mut m, &mut pool, &mut user, &mut w[2], sunday_open).unwrap();
        // ... unless settle_market recorded `settled_at`: then winners wait
        // out the quiet time after settlement (D35)
        m.settled_at = sunday_open;
        let mut w3 = open(&mut m, &mut pool, &mut user, Side::Short, 10, 75, 150);
        let e = at(&mut m, &mut pool, &mut user, &mut w3, sunday_open + 299).unwrap_err();
        assert_eq!(e, PerpsError::ProfitCloseQuiet.into());
        at(&mut m, &mut pool, &mut user, &mut w3, sunday_open + 300).unwrap();
    }

    #[test]
    fn close_caps_apply_only_when_net_rises() {
        let m = market();
        let mut pool = mk_pool(1_000_000 * C6);
        pool.params.k_net_bps = 10_000;
        // B = $1M; net = +20k contracts long (the counterparties left already)
        let v = PoolView { net: 20_000 * C6 as i128, b: 1_000_000 * C6 as u128, ..Default::default() };
        // Closing a long lowers net: never capped
        check_close_caps(&m, &pool, &v, Side::Long, 1_000 * C6, 75 * C6, 75 * C6).unwrap();
        // Closing a short raises it: 21k · $75 = $1.575M > 1 × $1M
        let e = check_close_caps(&m, &pool, &v, Side::Short, 1_000 * C6, 75 * C6, 75 * C6).unwrap_err();
        assert_eq!(e, PerpsError::PoolExposureLimit.into());
        // Exactly at the cap is allowed: (13_333_333_333 + 1)·75 rounds up to ≥ $1M
        let v0 = PoolView { net: 0, b: 1_000_000 * C6 as u128, ..Default::default() };
        let at_cap = 13_333_333_333;
        check_close_caps(&m, &pool, &v0, Side::Short, at_cap, 75 * C6, 75 * C6).unwrap();
        assert!(check_close_caps(&m, &pool, &v0, Side::Short, at_cap + 1, 75 * C6, 75 * C6).is_err());
    }

    /// D43 review finding (skew clamp): the weekend cap on a net-raising
    /// close counts the close's skew-premium shortfall against `B`, and a
    /// deficit-mode ADL (at the mark, no premium) all of its `Δx`.
    #[test]
    fn weekend_close_cap_counts_the_premium_shortfall() {
        let mut m = market();
        m.weekend_initial_margin_ratio = 2_000;
        let mut pool = mk_pool(1_000_000 * C6);
        pool.params.weekend_cap_window_secs = 86_400;
        pool.params.weekend_lp_loss_bps = 1_000;
        pool.params.skew_scale = 2_500_000_000_000;
        pool.params.max_impact_bps = 100;
        let o = 75 * C6;
        // net +25k; B $12.75M: α·B/G = $6.375M = 85k contracts at $75
        let n = 25_000 * C6 as i128;
        let v = PoolView { net: n, b: 12_750_000 * C6 as u128, ..Default::default() };
        let s = 60_000 * C6;
        // Closing a short is a buy at the clamped skew price
        let exec = crate::state::skew_price(o, n, s as i128, &pool.params).unwrap();
        assert_eq!(exec, o + o / 100);
        // Without the skew the 60k close sits exactly on the cap
        let mut flat = pool.clone();
        flat.params.skew_scale = 0;
        check_close_caps(&m, &flat, &v, Side::Short, s, o, o).unwrap();
        // With it, $9,375 of B is owed on top: refused at the edge ...
        let e = check_close_caps(&m, &pool, &v, Side::Short, s, o, exec).unwrap_err();
        assert_eq!(e, PerpsError::PoolWeekendExposureLimit.into());
        // ... and allowed once G·nn fits α·(B − shortfall)
        let short = pool_math::premium_shortfall(n, n + s as i128, o, exec, s, pool.params.skew_scale, 100).unwrap();
        assert_eq!(short, 9_375 * C6 as u128);
        let v_more = PoolView { b: v.b + short, ..v };
        check_close_caps(&m, &pool, &v_more, Side::Short, s, o, exec).unwrap();
        // ADL at the mark: all of Δx (here $54,375) comes off B
        let e = check_close_caps(&m, &pool, &v_more, Side::Short, s, o, o).unwrap_err();
        assert_eq!(e, PerpsError::PoolWeekendExposureLimit.into());
        let v_adl = PoolView { b: v.b + 54_375 * C6 as u128, ..v };
        check_close_caps(&m, &pool, &v_adl, Side::Short, s, o, o).unwrap();
    }

    /// D43: the weekend cap binds a net-raising close at any time while the
    /// weekend bound is on (W > 0), not only inside W, and not when W = 0.
    #[test]
    fn weekend_close_cap_is_always_on() {
        let mut m = market();
        m.session_schedule = session::SCHEDULE_CME_GLOBEX;
        m.weekend_initial_margin_ratio = 2_000;
        let mut pool = mk_pool(1_000_000 * C6);
        pool.params.weekend_cap_window_secs = 86_400;
        pool.params.weekend_lp_loss_bps = 1_000; // α/G = 0.5 × B = $500k
        let v = PoolView { net: 0, b: 1_000_000 * C6 as u128, ..Default::default() };
        let size = 8_000 * C6; // $600k of net
        let e = check_close_caps(&m, &pool, &v, Side::Short, size, 75 * C6, 75 * C6).unwrap_err();
        assert_eq!(e, PerpsError::PoolWeekendExposureLimit.into());
        // The edge: floor($500k / $75) contracts fit, one unit more does not
        let edge = 500_000 * C6 * C6 / (75 * C6);
        check_close_caps(&m, &pool, &v, Side::Short, edge, 75 * C6, 75 * C6).unwrap();
        assert!(check_close_caps(&m, &pool, &v, Side::Short, edge + 1, 75 * C6, 75 * C6).is_err());
        // An explicit G of 10% doubles the room
        pool.params.weekend_gap_bps = 1_000;
        check_close_caps(&m, &pool, &v, Side::Short, size, 75 * C6, 75 * C6).unwrap();
        // A derived G follows the market's IM: raised to 25% after the
        // params were applied, the cap tightens to 0.4 × B = $400k
        pool.params.weekend_gap_bps = 0;
        m.weekend_initial_margin_ratio = 2_500;
        let at_04 = 400_000 * C6 * C6 / (75 * C6);
        check_close_caps(&m, &pool, &v, Side::Short, at_04, 75 * C6, 75 * C6).unwrap();
        let e = check_close_caps(&m, &pool, &v, Side::Short, at_04 + 1, 75 * C6, 75 * C6).unwrap_err();
        assert_eq!(e, PerpsError::PoolWeekendExposureLimit.into());
        // W = 0 turns the weekend bound off
        pool.params.weekend_cap_window_secs = 0;
        check_close_caps(&m, &pool, &v, Side::Short, size, 75 * C6, 75 * C6).unwrap();
    }

    /// Mutation finding: equity exactly 0 after the fee is the solvent branch.
    /// The fee is split as a fee (not left in LP cash), nothing is a deficit,
    /// and no loss window opens. Long 10 @ $75 with $10 at $74.0375: P =
    /// −$9.625, fee $0.375. The close also counts a trade and touches the
    /// drawdown peak.
    #[test]
    fn close_with_exactly_zero_equity_after_the_fee() {
        let (mut m, mut pool, mut user) = (market(), mk_pool(1_000_000 * C6), UserAccount::default());
        let mut p = open(&mut m, &mut pool, &mut user, Side::Long, 10, 75, 10);
        let (cash0, ins0, trades0) = (pool.cash, m.insurance_fund, user.total_trades);
        pool.nps_peak = 1;
        let before = bucket_total(&pool, &m);
        let o = settle_close(&mut m, &mut pool, &mut p, &mut user, Pubkey::new_unique(), 74_037_500, 74_037_500, 1, None).unwrap();
        assert_eq!(bucket_total(&pool, &m), before);
        assert_eq!((o.payout, o.deficit, o.fee, o.p_eff), (0, 0, 375_000, -9_625_000));
        assert_eq!((pool.cash, m.insurance_fund), (cash0 + 9_625_000, ins0 + 375_000));
        assert_eq!((pool.loss_window_start, pool.bad_debt_cum), (0, 0));
        assert_eq!(user.total_trades, trades0 + 1);
        assert!(pool.nps_peak > 1);
    }

    /// Mutation findings on the close caps (D14): a close that leaves `|net|`
    /// unchanged is not checked, and the net notional is a ceiling, so a
    /// close whose floored notional just fits B is refused.
    #[test]
    fn close_caps_skip_equal_net_and_ceil_the_notional() {
        let (m, mut pool) = (market(), mk_pool(0));
        pool.params.k_net_bps = 10_000;
        // net +5, closing a 10 long: |after| = |net|, no check even with B = 0
        let v = PoolView { net: 5, b: 0, ..Default::default() };
        check_close_caps(&m, &pool, &v, Side::Long, 10, 75 * C6, 75 * C6).unwrap();
        // net 0, closing 3 units of a short at $75.000001: 225.000003 → 226
        let v = PoolView { net: 0, b: 225, ..Default::default() };
        let e = check_close_caps(&m, &pool, &v, Side::Short, 3, 75_000_001, 75_000_001).unwrap_err();
        assert_eq!(e, PerpsError::PoolExposureLimit.into());
        let v = PoolView { net: 0, b: 226, ..Default::default() };
        check_close_caps(&m, &pool, &v, Side::Short, 3, 75_000_001, 75_000_001).unwrap();
    }
}
