//! Ports of the perps-core helpers the keeper's decisions depend on, so it
//! only sends transactions the program will accept. Each function names its
//! on-chain original; keep them in step. The pure integer math is not ported
//! but shared: `pool_math.rs` and `session.rs` are `#[path]`-included.
//!
//! - programs/perps-core/src/state/position.rs: `unrealized_pnl`,
//!   `accrued_funding`, `equity`, `mark_notional`, `opened_notional`,
//!   `is_liquidatable`
//! - instructions/update_funding.rs: `apply_funding` (D40: liquidate and adl
//!   crank funding before they decide, so the keeper projects the crank)
//! - instructions/liquidate.rs: `slice_funding`
//! - instructions/settle_close.rs: `check_close_caps`, `slice_close_fee`,
//!   `in_profit_close_quiet`
//! - instructions/adl.rs: `pending_age`, `deficit_slice`, `adl_plan`,
//!   `remainder_equity`, `exposure_stop_ok`, `write_off_stale`
//! - instructions/lp_pool.rs: `sweep_insurance_excess` (the amount)
//! - state/pool.rs: `pool_view`, `touch_peak` + `drawdown_now`, `quote_profit`

use crate::pool_math::{self, Payout, PoolView};
use crate::session;
use crate::state::{LpPoolData, MarketData, PositionData, SIDE_LONG, SIDE_SHORT};

pub const ADL_MODE_DEFICIT: u8 = 0;
pub const ADL_MODE_EXPOSURE: u8 = 1;

/// The program error a mirrored check would raise (`PerpsError` names).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reject {
    InvalidPositionReduction,
    AdlCandidateNotProfitable,
    AdlScoreBelowFloor,
    AdlNotRequired,
    AdlWrongSide,
    ProfitCloseQuiet,
    PoolExposureLimit,
    PoolWeekendExposureLimit,
    PoolCashInsufficient,
    MathOverflow,
}

// ---------------------------------------------------------------------------
// Position (state/position.rs)
// ---------------------------------------------------------------------------

impl PositionData {
    pub fn is_long(&self) -> bool {
        self.side == SIDE_LONG
    }

    /// `Position::unrealized_pnl`: `size·(±(mark − entry)) / 1e6`, truncated
    /// toward zero, then cast to i64 as the program does.
    pub fn unrealized_pnl(&self, mark: u64) -> i64 {
        let diff = if self.is_long() {
            mark as i128 - self.entry_price as i128
        } else {
            self.entry_price as i128 - mark as i128
        };
        ((self.size as i128 * diff) / 1_000_000) as i64
    }

    /// `Position::accrued_funding`: owed to (+) or by (−) the position since
    /// it last settled. `cumulative_funding` is USDC per whole contract.
    pub fn accrued_funding(&self, cumulative_funding: i64) -> i64 {
        let per_contract = cumulative_funding as i128 - self.last_funding_payment as i128;
        let payment = (self.size as i128) * per_contract / 1_000_000;
        let signed = if self.is_long() { -payment } else { payment };
        signed.clamp(i64::MIN as i128, i64::MAX as i128) as i64
    }

    /// `Position::equity`: `C + pnl(mark) + accrued_funding(F)`, exact.
    pub fn equity(&self, mark: u64, cumulative_funding: i64) -> i128 {
        self.collateral as i128 + self.unrealized_pnl(mark) as i128 + self.accrued_funding(cumulative_funding) as i128
    }

    /// Realized PnL incl. funding of a full close at `mark` (no fee).
    pub fn pnl_with_funding(&self, mark: u64, cumulative_funding: i64) -> i128 {
        self.unrealized_pnl(mark) as i128 + self.accrued_funding(cumulative_funding) as i128
    }

    /// `floor(size·mark, 1e6)` (§6.1).
    pub fn mark_notional(&self, mark: u64) -> u128 {
        self.size as u128 * mark as u128 / 1_000_000
    }

    /// `floor(size·opened_price, 1e6)` (D21).
    pub fn opened_notional(&self, size: u64) -> u64 {
        pool_math::notional_floor(size, self.opened_price)
    }

    /// `Position::is_liquidatable` (§6.1): `equity·10_000 < mmr·N_mark`; with
    /// `N_mark == 0` only a negative equity.
    pub fn is_liquidatable(&self, mark: u64, accrued_funding: i64, mmr: u32) -> bool {
        let equity = self.collateral as i128 + self.unrealized_pnl(mark) as i128 + accrued_funding as i128;
        let n = self.mark_notional(mark);
        if n == 0 {
            return equity < 0;
        }
        equity * 10_000 < (mmr as u128 * n) as i128
    }

    /// A slice `x` with the same side and entry (for its PnL).
    fn slice(&self, x: u64) -> PositionData {
        PositionData { side: self.side, size: x, entry_price: self.entry_price, ..Default::default() }
    }
}

// ---------------------------------------------------------------------------
// Funding (update_funding.rs)
// ---------------------------------------------------------------------------

/// `funding_per_contract` (state/market.rs).
pub fn funding_per_contract(rate: i64, price: u64) -> i64 {
    let amount = rate as i128 * price as i128 / 1_000_000;
    amount.clamp(i64::MIN as i128, i64::MAX as i128) as i64
}

/// The per-period rate `apply_funding` charges for the current OI.
pub fn funding_rate(long_oi: u64, short_oi: u64) -> i64 {
    let (l, s) = (long_oi as i128, short_oi as i128);
    let total = l + s;
    if total == 0 {
        return 0;
    }
    let imbalance = ((l - s) * 1_000_000) / total;
    ((imbalance * 100) / 1_000_000) as i64
}

/// `crank_funding` at `oracle_price`: the market as `liquidate` and `adl`
/// see it after charging the periods due (D40). Unchanged when settled or
/// nothing is due.
pub fn project_funding(m: &MarketData, oracle_price: u64, now: i64) -> MarketData {
    let mut out = m.clone();
    if m.is_settled() {
        return out;
    }
    if let Some(c) = pool_math::funding_catchup(m.funding_open_secs(now), m.funding_interval) {
        let rate = funding_rate(m.long_open_interest, m.short_open_interest);
        let step = funding_per_contract(rate, oracle_price);
        out.funding_rate = rate;
        out.cumulative_funding = pool_math::funding_accumulate(m.cumulative_funding, c.n, step);
        out.last_funding_time = now;
        out.funding_carry_secs = c.carry;
    }
    out
}

// ---------------------------------------------------------------------------
// Pool (state/pool.rs)
// ---------------------------------------------------------------------------

/// `pool_view` with the pool's own `h`: no skew premium once settled.
pub fn pool_view(pool: &LpPoolData, m: &MarketData, mark: u64, now: i64) -> Option<PoolView> {
    pool_math::pool_view_raw(
        &pool.state(),
        m.long_open_interest,
        m.short_open_interest,
        m.cumulative_funding,
        mark,
        now,
        pool.params.redeem_loss_haircut_bps,
        if m.is_settled() { 0 } else { pool.params.skew_scale },
        pool.params.max_impact_bps,
    )
}

/// `touch_peak` then `drawdown_now` (D28): the drawdown against the peak as
/// the instruction sees it after touching it.
pub fn drawdown_after_touch(pool: &LpPoolData, v: &PoolView, now: i64) -> u64 {
    let peak = if pool_math::peak_resets(v.nps, pool.nps_peak, pool.nps_peak_at, now) { v.nps } else { pool.nps_peak };
    pool_math::drawdown_bps(v.nps, peak)
}

/// `quote_profit`; `PoolCashInsufficient` only with `allow_short == false`.
pub fn quote_profit(pool: &LpPoolData, v: &PoolView, side: u8, p: u64, allow_short: bool) -> Result<Payout, Reject> {
    pool_math::quote_profit_raw(v.u, pool.cash, pool.opposite_pending(side), p, allow_short).ok_or(Reject::PoolCashInsufficient)
}

// ---------------------------------------------------------------------------
// Closes (liquidate.rs, settle_close.rs)
// ---------------------------------------------------------------------------

/// `slice_funding`: the slice's own share of the accrued funding (D24).
pub fn slice_funding(position: &PositionData, x: u64, f: i64) -> i128 {
    let all = position.accrued_funding(f) as i128;
    if x >= position.size {
        return all;
    }
    let rest = PositionData { size: position.size - x, ..position.clone() };
    all - rest.accrued_funding(f) as i128
}

/// Profit of a slice `x` at `mark`: its PnL and its own funding.
pub fn slice_profit(position: &PositionData, x: u64, mark: u64, f: i64) -> i128 {
    position.slice(x).unrealized_pnl(mark) as i128 + slice_funding(position, x, f)
}

/// `slice_close_fee`: `ceil(opened_notional(x)·taker_fee, 10_000)` (D22).
pub fn slice_close_fee(position: &PositionData, x: u64, taker_fee: u32) -> u64 {
    let n = position.opened_notional(x) as u128;
    u64::try_from(pool_math::ceil_u(n * taker_fee as u128, 10_000)).unwrap_or(u64::MAX)
}

/// `check_close_caps` (D14): a close of `size` on `side` that raises `|net|`
/// must fit the net cap and, inside `W`, the weekend cap, against `B`.
pub fn check_close_caps(m: &MarketData, pool: &LpPoolData, v: &PoolView, side: u8, size: u64, mark: u64, now: i64) -> Result<(), Reject> {
    let s = size as i128;
    let after = if side == SIDE_LONG { v.net - s } else { v.net + s };
    if after.unsigned_abs() <= v.net.unsigned_abs() {
        return Ok(());
    }
    let nn = pool_math::ceil_u(after.unsigned_abs().checked_mul(mark as u128).ok_or(Reject::MathOverflow)?, 1_000_000);
    let p = &pool.params;
    if !pool_math::net_cap_ok(nn, p.k_net_bps, v.b) {
        return Err(Reject::PoolExposureLimit);
    }
    if session::in_pre_close_window(m.session_schedule, now, p.weekend_cap_window_secs)
        && !pool_math::weekend_cap_ok(nn, p.g_bps(m), p.weekend_lp_loss_bps, v.b)
    {
        return Err(Reject::PoolWeekendExposureLimit);
    }
    Ok(())
}

/// `in_profit_close_quiet` (D23, D35).
pub fn in_profit_close_quiet(m: &MarketData, pool: &LpPoolData, now: i64) -> bool {
    let q = pool.params.profit_close_quiet_secs as i64;
    if m.is_settled() {
        m.settled_at > 0 && now.saturating_sub(m.settled_at) < q
    } else {
        session::seconds_since_open(m.session_schedule, now).is_some_and(|t| t < q)
    }
}

// ---------------------------------------------------------------------------
// ADL (adl.rs)
// ---------------------------------------------------------------------------

/// `pending_age` (D35): open seconds; once settled, open seconds up to
/// `settled_at` plus wall seconds after it.
pub fn pending_age(m: &MarketData, since: i64, now: i64) -> i64 {
    if m.is_settled() {
        let s = since.max(m.settled_at);
        session::open_seconds_between(m.session_schedule, since, s).saturating_add(now.saturating_sub(s))
    } else {
        session::open_seconds_between(m.session_schedule, since, now)
    }
}

/// `deficit_slice` (D37): the smallest slice whose bad-debt haircut covers
/// `opp`, scaled by `u/cash` when the solvency haircut applies; else `q`.
pub fn deficit_slice(position: &PositionData, opp: u64, u: i128, cash: u64, mark: u64, f: i64) -> u64 {
    let q = position.size;
    let p_q = slice_profit(position, q, mark, f);
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
    let x = pool_math::ceil_s((need + 4) * q as i128, p_q).clamp(1, q as i128) as u64;
    if slice_profit(position, x, mark, f) < need {
        q
    } else {
        x
    }
}

/// `remainder_equity` (D24).
pub fn remainder_equity(position: &PositionData, x: u64, mark: u64, f: i64) -> i128 {
    let q = position.size;
    let c = position.collateral;
    let c_x = if x == q { c } else { (c as u128 * x as u128 / q as u128) as u64 };
    let rest = PositionData { size: q - x, ..position.clone() };
    (c - c_x) as i128 + rest.unrealized_pnl(mark) as i128 + rest.accrued_funding(f) as i128
}

/// `AdlPlan`: everything `adl` checks before it applies anything.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AdlPlan {
    pub x: u64,
    pub profit: u64,
    pub score: u128,
    pub floor: u64,
    pub triggers: u8,
    pub target: u128,
    pub fee: u64,
}

/// §7.2 step 8: the exposure triggers that hold on `v` and the smallest of
/// their targets (`u128::MAX` when none holds).
pub fn exposure_triggers(m: &MarketData, pool: &LpPoolData, v: &PoolView, now: i64) -> (u8, u128) {
    let p = &pool.params;
    let g = p.g_bps(m);
    let w = session::in_pre_close_window(m.session_schedule, now, p.weekend_cap_window_secs);
    let mut t = 0u8;
    let mut target = u128::MAX;
    if pool_math::n_trig(v.net_notional, p.hard_net_bps, v.b) {
        t |= pool_math::TRIGGER_NET;
        target = target.min(pool_math::bps_of(p.k_net_bps, v.b));
    }
    if pool_math::d_trig(drawdown_after_touch(pool, v, now), p.dd_adl_bps) {
        t |= pool_math::TRIGGER_DRAWDOWN;
        target = target.min(pool_math::bps_of(p.adl_net_target_bps, v.b));
    }
    if pool_math::w_trig(w, v.net_notional, g, p.weekend_lp_loss_bps, v.b) {
        t |= pool_math::TRIGGER_WEEKEND;
        target = target.min(pool_math::weekend_target(p.weekend_lp_loss_bps, v.b, g));
    }
    (t, target)
}

/// `adl_plan` (§7.2 steps 3–8) on the market as the instruction sees it
/// (funding cranked, `project_funding`) and the pool view `v` taken before
/// the plan. Same checks in the same order, so the first failure is the
/// error the program would return.
#[allow(clippy::too_many_arguments)]
pub fn adl_plan(m: &MarketData, pool: &LpPoolData, v: &PoolView, position: &PositionData, mode: u8, size: u64, mark: u64, now: i64) -> Result<AdlPlan, Reject> {
    let p = &pool.params;
    let f = m.cumulative_funding;
    // 3. Size
    let q = position.size;
    let mut x = if size == 0 { q } else { size };
    if x > q {
        return Err(Reject::InvalidPositionReduction);
    }
    let long = position.is_long();
    let (opp, opp_since) = pool.pending_of(if long { SIDE_SHORT } else { SIDE_LONG });
    if mode == ADL_MODE_DEFICIT {
        x = x.min(deficit_slice(position, opp, v.u, pool.cash, mark, f));
    }
    if pool_math::notional_floor(q - x, mark) < p.liq_min_remaining_notional {
        x = q;
    }
    if x < q && remainder_equity(position, x, mark, f) <= 0 {
        x = q;
    }

    // 4. Profit on the slice
    let profit = slice_profit(position, x, mark, f);
    if profit <= 0 {
        return Err(Reject::AdlCandidateNotProfitable);
    }
    let e = position.equity(mark, f);
    if e <= 0 {
        return Err(Reject::AdlCandidateNotProfitable);
    }

    // 5. Score and floor
    let score = pool_math::adl_score(long, mark, position.entry_price, position.mark_notional(mark), e);
    let floor = if mode == ADL_MODE_DEFICIT {
        pool_math::adl_floor_now(pending_age(m, opp_since, now))
    } else {
        p.adl_exposure_score_floor_bps as u64
    };
    if score < floor as u128 {
        return Err(Reject::AdlScoreBelowFloor);
    }

    let (triggers, target) = if mode == ADL_MODE_DEFICIT {
        // 7.
        if !pool.adl_required() {
            return Err(Reject::AdlNotRequired);
        }
        if opp == 0 {
            return Err(Reject::AdlWrongSide);
        }
        if in_profit_close_quiet(m, pool, now) {
            return Err(Reject::ProfitCloseQuiet);
        }
        if !m.is_settled() {
            check_close_caps(m, pool, v, position.side, x, mark, now)?;
        }
        (0u8, 0u128)
    } else {
        // 8.
        let (t, target) = exposure_triggers(m, pool, v, now);
        if t == 0 {
            return Err(Reject::AdlNotRequired);
        }
        let side_ok = (v.net > 0 && long) || (v.net < 0 && !long);
        if !side_ok {
            return Err(Reject::AdlWrongSide);
        }
        (t, target)
    };
    let fee = if mode == ADL_MODE_DEFICIT { slice_close_fee(position, x, m.taker_fee) } else { 0 };
    let profit = u64::try_from(profit).map_err(|_| Reject::MathOverflow)?;
    Ok(AdlPlan { x, profit, score, floor, triggers, target, fee })
}

/// `exposure_stop_ok` (§7.2 step 10) on the net after the slice: `net` keeps
/// its sign (or reaches 0) and `ceil(|net'|·mark, 1e6) ≥ target`.
pub fn exposure_stop_ok(net_before: i128, net_after: i128, mark: u64, target: u128) -> bool {
    let same_sign = net_after == 0 || (net_after > 0) == (net_before > 0);
    let nn = net_after.unsigned_abs().checked_mul(mark as u128).map(|v| pool_math::ceil_u(v, 1_000_000));
    same_sign && nn.is_some_and(|nn| nn >= target)
}

/// Net after an ADL of `x` on `side`: closing a long lowers `L − S`.
pub fn net_after_close(net: i128, side: u8, x: u64) -> i128 {
    if side == SIDE_LONG {
        net - x as i128
    } else {
        net + x as i128
    }
}

/// `write_off_stale` without the writes: what `write_off_bad_debt` would
/// write off at `now` (0 → `BadDebtNotStale` or `AdlNotRequired`).
pub fn write_off_amount(m: &MarketData, pool: &LpPoolData, now: i64) -> u64 {
    let mut w = 0u64;
    for (pending, since) in [(pool.pending_bad_debt_long, pool.pending_since_long), (pool.pending_bad_debt_short, pool.pending_since_short)] {
        if pending > 0 && pending_age(m, since, now) >= pool_math::ADL_WRITE_OFF_SECS {
            w = w.saturating_add(pending);
        }
    }
    w
}

/// `sweep_insurance_excess` step 2: the amount it would sweep (0 →
/// `NothingToSweep`, or `PoolHasNoLps` without LPs).
pub fn sweep_amount(m: &MarketData, pool: &LpPoolData) -> u64 {
    if pool.lp_shares == 0 {
        return 0;
    }
    if m.is_settled() && pool.open_positions == 0 {
        m.insurance_fund
    } else {
        m.insurance_fund.saturating_sub(pool.params.insurance_target)
    }
}

#[cfg(test)]
mod tests {
    //! Vectors from the perps-core unit tests (instructions/adl.rs,
    //! settle_close.rs, state/position.rs), checked against the mirrors.
    use super::*;
    use crate::pool_math::{ADL_FLOOR_DECAY_SECS, ADL_WRITE_OFF_SECS, DD_WINDOW_SECS};
    use crate::state::PoolParamsData;
    use crate::testkit::*;

    fn plan(m: &MarketData, pool: &LpPoolData, p: &PositionData, mode: u8, size: u64, mark: u64, now: i64) -> Result<AdlPlan, Reject> {
        let v = pool_view(pool, m, mark, now).unwrap();
        adl_plan(m, pool, &v, p, mode, size, mark, now)
    }

    /// state/position.rs `margin_ratio_includes_accrued_funding`, and the
    /// mark-notional rule of §6.1 where it differs from entry notional.
    #[test]
    fn liquidation_uses_mark_notional() {
        let long = PositionData { side: SIDE_LONG, size: 100 * C6, collateral: 750 * C6, entry_price: 75 * C6, ..Default::default() };
        assert!(!long.is_liquidatable(75 * C6, 0, 500));
        assert!(long.is_liquidatable(75 * C6, -400 * C6 as i64, 500));
        // Short 100 @ $75 with $750 at $78.60: E = $390; 5% of the $7,860 mark
        // notional is $393 (liquidatable), 5% of the $7,500 entry notional $375
        let short = PositionData { side: SIDE_SHORT, ..long.clone() };
        assert_eq!(short.equity(78_600_000, 0), 390 * C6 as i128);
        assert!(short.is_liquidatable(78_600_000, 0, 500));
        assert!(!short.is_liquidatable(78_500_000, 0, 500)); // E $400 vs $392.5
        // A long at a lower mark needs less: $72 → E $450 vs $360
        assert!(!long.is_liquidatable(72 * C6, 0, 500));
        // Zero mark notional: only a negative equity
        let dust = PositionData { size: 1, collateral: 0, entry_price: 1, ..long.clone() };
        assert!(!dust.is_liquidatable(1, 0, 500));
        assert!(dust.is_liquidatable(1, -1, 500));
    }

    #[test]
    fn funding_projection_mirrors_apply_funding() {
        let mut m = market();
        m.long_open_interest = 300 * C6;
        m.short_open_interest = 100 * C6;
        m.last_funding_time = WEDNESDAY - 3 * HOUR - 17;
        m.funding_carry_secs = 0;
        m.cumulative_funding = 1_000;
        // imbalance 0.5 → 50 (0.005%) per period; at $80 that is $0.004 a contract
        assert_eq!(funding_rate(300 * C6, 100 * C6), 50);
        let p = project_funding(&m, 80 * C6, WEDNESDAY);
        assert_eq!(p.cumulative_funding, 1_000 + 3 * 4_000);
        assert_eq!((p.last_funding_time, p.funding_carry_secs, p.funding_rate), (WEDNESDAY, 17, 50));
        // Not due: unchanged
        m.last_funding_time = WEDNESDAY - 3_599;
        assert!(!m.funding_due(WEDNESDAY));
        assert_eq!(project_funding(&m, 80 * C6, WEDNESDAY), m);
        // The carry counts: 3_599 + 1
        m.funding_carry_secs = 1;
        assert!(m.funding_due(WEDNESDAY));
        assert_eq!(project_funding(&m, 80 * C6, WEDNESDAY).funding_carry_secs, 0);
        // Settled: never cranks
        m.settlement_price = 70 * C6;
        assert_eq!(project_funding(&m, 80 * C6, WEDNESDAY), m);
    }

    /// Funding is due on open seconds, not wall clock: the weekend does not
    /// count (§16.4).
    #[test]
    fn funding_due_counts_open_seconds_only() {
        let m = MarketData {
            session_schedule: session::SCHEDULE_CME_GLOBEX,
            funding_interval: 3_600,
            last_funding_time: FRIDAY_CLOSE - 1_800,
            ..Default::default()
        };
        // 1_800 open seconds before the close; nothing over the weekend
        assert!(!m.funding_due(SUNDAY_OPEN - 1));
        assert!(!m.funding_due(SUNDAY_OPEN + 1_799));
        assert!(m.funding_due(SUNDAY_OPEN + 1_800));
        // With 600 carried it is due 600 s earlier
        let c = MarketData { funding_carry_secs: 600, ..m };
        assert!(c.funding_due(SUNDAY_OPEN + 1_200));
        assert!(!c.funding_due(SUNDAY_OPEN + 1_199));
    }

    /// adl.rs `adl_is_zero_sum`: the plans of its three slices.
    #[test]
    fn deficit_plans_match_adl_is_zero_sum() {
        let (mut m, mut pool) = (market(), pool());
        let short = open(&mut m, &mut pool, SIDE_SHORT, 100 * C6, 75 * C6, 1_500 * C6);
        let _long = open(&mut m, &mut pool, SIDE_LONG, 100 * C6, 75 * C6, 1_500 * C6);
        pool.pending_bad_debt_long = 250 * C6;
        pool.pending_since_long = 1;
        let (mark, now) = (70 * C6, 1 + ADL_FLOOR_DECAY_SECS);
        let p = plan(&m, &pool, &short, ADL_MODE_DEFICIT, 40 * C6, mark, now).unwrap();
        assert_eq!((p.x, p.profit, p.fee, p.floor), (40 * C6, 200 * C6, 1_500_000, 0));
        let v = pool_view(&pool, &m, mark, now).unwrap();
        let q = quote_profit(&pool, &v, SIDE_SHORT, p.profit, false).unwrap();
        assert_eq!((q.paid, q.bad_debt_hc), (0, 200 * C6));

        // The state after it: 60 left with $900, $50 pending
        let mut short = PositionData { size: 60 * C6, collateral: 900 * C6, ..short };
        pool.short_entry_sum = 60 * C6 as u128 * 75 * C6 as u128;
        m.short_open_interest = 60 * C6;
        pool.pending_bad_debt_long = 50 * C6;
        let p = plan(&m, &pool, &short, ADL_MODE_DEFICIT, 0, mark, now).unwrap();
        assert_eq!((p.x, p.fee), (10_000_001, 375_001));
        let v = pool_view(&pool, &m, mark, now).unwrap();
        let q = quote_profit(&pool, &v, SIDE_SHORT, p.profit, false).unwrap();
        assert_eq!((q.paid, q.bad_debt_hc), (5, 50 * C6));

        // Then 49_999_999 left with $749.999985 and $1,000 pending: all of it
        short.size = 49_999_999;
        short.collateral = 749_999_985;
        pool.short_entry_sum = 49_999_999u128 * 75 * C6 as u128;
        m.short_open_interest = 49_999_999;
        pool.pending_bad_debt_long = 1_000 * C6;
        let p = plan(&m, &pool, &short, ADL_MODE_DEFICIT, 0, mark, now).unwrap();
        assert_eq!((p.x, p.fee), (49_999_999, 1_875_000));
        let v = pool_view(&pool, &m, mark, now).unwrap();
        assert_eq!(quote_profit(&pool, &v, SIDE_SHORT, p.profit, false).unwrap().bad_debt_hc, 249_999_995);
    }

    /// adl.rs `deficit_adl_closes_only_what_the_pending_needs` (D37).
    #[test]
    fn deficit_slice_matches_the_d37_vectors() {
        let (mut m, mut pool) = (market(), pool());
        let long = open(&mut m, &mut pool, SIDE_LONG, 20_000 * C6, 75 * C6, 150_000 * C6);
        let _short = open(&mut m, &mut pool, SIDE_SHORT, 20_000 * C6, 75 * C6, 150_000 * C6);
        pool.pending_bad_debt_short = 1;
        pool.pending_since_short = 1;
        let (mark, now) = (85 * C6, 1 + ADL_FLOOR_DECAY_SECS);
        let p = plan(&m, &pool, &long, ADL_MODE_DEFICIT, 0, mark, now).unwrap();
        assert_eq!((p.x, p.profit, p.fee), (1, 10, 1));
        let v = pool_view(&pool, &m, mark, now).unwrap();
        let q = quote_profit(&pool, &v, SIDE_LONG, p.profit, false).unwrap();
        assert_eq!((q.bad_debt_hc, q.paid), (1, 9));

        // Insolvent at mark: the slice must earn opp·u/cash
        let (mut m, mut pool) = (market(), crate::testkit::pool());
        let long = open(&mut m, &mut pool, SIDE_LONG, 20_000 * C6, 75 * C6, 150_000 * C6);
        pool.cash = 100_000 * C6;
        pool.pending_bad_debt_short = 10 * C6;
        pool.pending_since_short = 1;
        let v = pool_view(&pool, &m, mark, now).unwrap();
        assert_eq!(v.u, 200_000 * C6 as i128);
        let p = plan(&m, &pool, &long, ADL_MODE_DEFICIT, 0, mark, now).unwrap();
        assert_eq!(p.x, 2_000_001);
        assert_eq!(quote_profit(&pool, &v, SIDE_LONG, p.profit, false).unwrap().bad_debt_hc, 10 * C6);
        assert_eq!(plan(&m, &pool, &long, ADL_MODE_DEFICIT, C6, mark, now).unwrap().x, C6);
        assert_eq!(deficit_slice(&long, 0, v.u, pool.cash, mark, 0), long.size);
        // cash 0 with u > 0: the whole position
        assert_eq!(deficit_slice(&long, 10, v.u, 0, mark, 0), long.size);
    }

    /// adl.rs `partial_adl_cannot_pay_out_the_remainders_funding` and
    /// `partial_adl_keeps_the_remainders_funding` (D24).
    #[test]
    fn slice_only_funding() {
        let (mut m, mut pool) = (market(), pool());
        let long = open(&mut m, &mut pool, SIDE_LONG, 100 * C6, 100 * C6, 1_000 * C6);
        m.cumulative_funding = -8 * C6 as i64;
        pool.pending_bad_debt_short = C6;
        pool.pending_since_long = 1;
        let now = 1 + ADL_FLOOR_DECAY_SECS;
        assert_eq!(long.equity(88 * C6, m.cumulative_funding), 600 * C6 as i128);
        assert_eq!(plan(&m, &pool, &long, ADL_MODE_DEFICIT, 10 * C6, 88 * C6, now), Err(Reject::AdlCandidateNotProfitable));
        assert_eq!(plan(&m, &pool, &long, ADL_MODE_DEFICIT, 0, 88 * C6, now), Err(Reject::AdlCandidateNotProfitable));

        let (mut m, mut pool) = (market(), crate::testkit::pool());
        let long = open(&mut m, &mut pool, SIDE_LONG, 100 * C6, 100 * C6, 1_000 * C6);
        let _short = open(&mut m, &mut pool, SIDE_SHORT, 100 * C6, 100 * C6, 1_000 * C6);
        m.cumulative_funding = -8 * C6 as i64;
        pool.pending_bad_debt_short = 100 * C6;
        pool.pending_since_short = 1;
        let p = plan(&m, &pool, &long, ADL_MODE_DEFICIT, 10 * C6, 96 * C6, now).unwrap();
        assert_eq!((p.x, p.profit, p.fee), (10 * C6, 40 * C6, 500_000));
        // The funding split telescopes
        assert_eq!(slice_funding(&long, 10 * C6, m.cumulative_funding), 80 * C6 as i128);
        assert_eq!(slice_funding(&long, 100 * C6, m.cumulative_funding), 800 * C6 as i128);
    }

    /// adl.rs `deficit_adl_fee_is_capped_by_what_the_slice_receives` (D22):
    /// the plan's fee; mode 1 is fee-free.
    #[test]
    fn deficit_fee_and_free_exposure_mode() {
        let (mut m, mut pool) = (market(), pool());
        let short = open(&mut m, &mut pool, SIDE_SHORT, 100 * C6, 75 * C6, C6);
        let _long = open(&mut m, &mut pool, SIDE_LONG, 100 * C6, 75 * C6, 1_500 * C6);
        pool.pending_bad_debt_long = 1_000 * C6;
        pool.pending_since_long = 1;
        let p = plan(&m, &pool, &short, ADL_MODE_DEFICIT, 0, 74_990_000, 1 + ADL_FLOOR_DECAY_SECS).unwrap();
        assert_eq!((p.profit, p.fee), (C6, 3_750_000));

        let mut m = market();
        let mut p1 = crate::testkit::pool();
        p1.params.hard_net_bps = 1;
        p1.params.k_net_bps = 1;
        p1.params.adl_net_target_bps = 0;
        let long = open(&mut m, &mut p1, SIDE_LONG, 100 * C6, 75 * C6, 1_500 * C6);
        let p = plan(&m, &p1, &long, ADL_MODE_EXPOSURE, 10 * C6, 76 * C6, 1).unwrap();
        assert_eq!((p.fee, p.triggers), (0, pool_math::TRIGGER_NET));
    }

    /// adl.rs `deficit_adl_that_raises_net_is_capped` (D25).
    #[test]
    fn deficit_adl_that_raises_net_is_capped() {
        let (mut m, mut pool) = (market(), pool());
        pool.params.k_net_bps = 10_000;
        let _long = open(&mut m, &mut pool, SIDE_LONG, 20_000 * C6, 75 * C6, 150_000 * C6);
        let short = open(&mut m, &mut pool, SIDE_SHORT, 20_000 * C6, 75 * C6, 150_000 * C6);
        pool.pending_bad_debt_long = 10_000 * C6;
        pool.pending_since_long = 1;
        let (mark, now) = (74_900_000, 1 + ADL_FLOOR_DECAY_SECS);
        assert_eq!(plan(&m, &pool, &short, ADL_MODE_DEFICIT, 0, mark, now), Err(Reject::PoolExposureLimit));
        assert_eq!(plan(&m, &pool, &short, ADL_MODE_DEFICIT, 10_000 * C6, mark, now).unwrap().x, 10_000 * C6);
        m.settlement_price = mark;
        plan(&m, &pool, &short, ADL_MODE_DEFICIT, 0, mark, now).unwrap();
    }

    /// adl.rs `deficit_adl_waits_out_the_profit_close_quiet_time` (D23, D35).
    #[test]
    fn deficit_adl_waits_out_the_quiet_time() {
        let (mut m, mut pool) = (market(), pool());
        m.session_schedule = session::SCHEDULE_CME_GLOBEX;
        pool.params.profit_close_quiet_secs = 300;
        let short = open(&mut m, &mut pool, SIDE_SHORT, 100 * C6, 75 * C6, 1_500 * C6);
        let _long = open(&mut m, &mut pool, SIDE_LONG, 100 * C6, 75 * C6, 1_500 * C6);
        pool.pending_bad_debt_long = 10 * C6;
        pool.pending_since_long = FRIDAY_CLOSE - 7 * HOUR;
        let mark = 64 * C6;
        for t in [SUNDAY_OPEN, SUNDAY_OPEN + 5, SUNDAY_OPEN + 299] {
            assert_eq!(plan(&m, &pool, &short, ADL_MODE_DEFICIT, 0, mark, t), Err(Reject::ProfitCloseQuiet));
        }
        assert_eq!(plan(&m, &pool, &short, ADL_MODE_DEFICIT, 0, mark, SUNDAY_OPEN + 300).unwrap().floor, 0);
        assert_eq!(plan(&m, &pool, &short, ADL_MODE_DEFICIT, 0, mark, SUNDAY_OPEN + 86_400 + 10), Err(Reject::ProfitCloseQuiet));
        pool.params.profit_close_quiet_secs = 0;
        plan(&m, &pool, &short, ADL_MODE_DEFICIT, 0, mark, SUNDAY_OPEN + 86_400 + 10).unwrap();
        pool.params.profit_close_quiet_secs = 300;
        let at = SUNDAY_OPEN + 1_000;
        m.settlement_price = mark;
        m.settled_at = at;
        assert_eq!(plan(&m, &pool, &short, ADL_MODE_DEFICIT, 0, mark, at + 299), Err(Reject::ProfitCloseQuiet));
        plan(&m, &pool, &short, ADL_MODE_DEFICIT, 0, mark, at + 300).unwrap();
        m.settled_at = 0;
        plan(&m, &pool, &short, ADL_MODE_DEFICIT, 0, mark, at).unwrap();
    }

    /// adl.rs `settled_pending_age_counts_open_time_until_settlement` (D35).
    #[test]
    fn settled_pending_age_and_write_off() {
        let mut m = market();
        m.session_schedule = session::SCHEDULE_CME_GLOBEX;
        let since = FRIDAY_CLOSE - 2 * HOUR;
        let at = SUNDAY_OPEN - 6 * HOUR;
        assert_eq!(pending_age(&m, since, at), 2 * HOUR);
        m.settlement_price = 70 * C6;
        m.settled_at = at;
        assert_eq!(pending_age(&m, since, at), 2 * HOUR);
        assert_eq!(pending_age(&m, since, at + 22 * HOUR), ADL_WRITE_OFF_SECS);
        assert_eq!(pending_age(&m, at + 100, at + 200), 100);
        m.settled_at = 0;
        assert_eq!(pending_age(&m, since, at), at - since);
        m.settled_at = at;

        let mut pool = pool();
        pool.pending_bad_debt_long = 5;
        pool.pending_since_long = since;
        assert_eq!(write_off_amount(&m, &pool, at), 0);
        assert_eq!(write_off_amount(&m, &pool, at + 22 * HOUR - 1), 0);
        assert_eq!(write_off_amount(&m, &pool, at + 22 * HOUR), 5);
        assert!(pool_math::adl_floor_now(pending_age(&m, since, at + 4 * HOUR - 1)) > 0);
        assert_eq!(pool_math::adl_floor_now(pending_age(&m, since, at + 4 * HOUR)), 0);
    }

    /// adl.rs `each_sides_pending_has_its_own_floor_and_write_off` (D36).
    #[test]
    fn each_side_has_its_own_clock() {
        let (mut m, mut pool) = (market(), pool());
        let long = open(&mut m, &mut pool, SIDE_LONG, 100 * C6, 75 * C6, 1_500 * C6);
        let _short = open(&mut m, &mut pool, SIDE_SHORT, 100 * C6, 75 * C6, 1_500 * C6);
        let t1 = 1 + 23 * HOUR;
        pool.pending_bad_debt_long = 900 * C6;
        pool.pending_since_long = 1;
        pool.pending_bad_debt_short = 100 * C6;
        pool.pending_since_short = t1;
        let mark = 76 * C6;
        assert_eq!(plan(&m, &pool, &long, ADL_MODE_DEFICIT, 0, mark, t1), Err(Reject::AdlScoreBelowFloor));
        assert_eq!(plan(&m, &pool, &long, ADL_MODE_DEFICIT, 0, mark, t1 + ADL_FLOOR_DECAY_SECS).unwrap().floor, 0);
        assert_eq!(write_off_amount(&m, &pool, t1 + HOUR - 1), 0);
        assert_eq!(write_off_amount(&m, &pool, t1 + HOUR), 900 * C6);
        pool.pending_bad_debt_long = 0;
        pool.pending_since_long = 0;
        assert_eq!(write_off_amount(&m, &pool, t1 + ADL_WRITE_OFF_SECS - 1), 0);
        assert_eq!(write_off_amount(&m, &pool, t1 + ADL_WRITE_OFF_SECS), 100 * C6);
    }

    /// adl.rs `remainder_equity_is_the_remainders_own_equity` and
    /// `adl_remainder_at_the_minimum_notional_stays_open`.
    #[test]
    fn remainder_rules() {
        let p = PositionData { side: SIDE_LONG, size: 100 * C6, entry_price: 100 * C6, collateral: 100 * C6, ..Default::default() };
        assert_eq!(remainder_equity(&p, 50 * C6, 100 * C6, 5 * C6 as i64), -200 * C6 as i128);
        let q = PositionData { collateral: 100 * C6 + 1, ..p };
        assert_eq!(remainder_equity(&q, 30 * C6, 100 * C6, 0), (100 * C6 + 1 - 30 * C6) as i128);

        let (mut m, mut pool) = (market(), pool());
        let short = open(&mut m, &mut pool, SIDE_SHORT, 100 * C6, 75 * C6, 1_500 * C6);
        let _long = open(&mut m, &mut pool, SIDE_LONG, 100 * C6, 75 * C6, 1_500 * C6);
        pool.pending_bad_debt_long = 10_000 * C6;
        pool.pending_since_long = 1;
        let (mark, now) = (50 * C6, 1 + ADL_FLOOR_DECAY_SECS);
        assert_eq!(plan(&m, &pool, &short, ADL_MODE_DEFICIT, 98 * C6, mark, now).unwrap().x, 98 * C6);
        assert_eq!(plan(&m, &pool, &short, ADL_MODE_DEFICIT, 98 * C6 + 1, mark, now).unwrap().x, 100 * C6);
        assert_eq!(plan(&m, &pool, &short, ADL_MODE_DEFICIT, 100 * C6 + 1, mark, now), Err(Reject::InvalidPositionReduction));
    }

    /// adl.rs `exposure_adl_closes_only_the_net_side` and
    /// `exposure_stop_keeps_the_sign_and_the_target`.
    #[test]
    fn exposure_side_and_stop() {
        for net_long in [true, false] {
            let (mut m, mut pool) = (market(), pool());
            pool.params.hard_net_bps = 1;
            pool.params.k_net_bps = 1;
            pool.params.adl_net_target_bps = 0;
            let (big, small) = if net_long { (SIDE_LONG, SIDE_SHORT) } else { (SIDE_SHORT, SIDE_LONG) };
            let _b = open(&mut m, &mut pool, big, 200 * C6, 75 * C6, 3_000 * C6);
            let s = open(&mut m, &mut pool, small, 100 * C6, 75 * C6, 1_500 * C6);
            let mark = if net_long { 74 * C6 } else { 76 * C6 };
            assert_eq!(plan(&m, &pool, &s, ADL_MODE_EXPOSURE, 0, mark, 1), Err(Reject::AdlWrongSide));
        }
        // net in contracts at a $75e-6 mark: notional = net·75 / 1e6, ceiled
        let mark = 75 * C6;
        let nn = |n: i128| n as u128 * 75;
        assert!(!exposure_stop_ok(100, -150, mark, nn(30)));
        assert!(!exposure_stop_ok(-100, 150, mark, nn(30)));
        assert!(exposure_stop_ok(100, 40, mark, nn(30)));
        assert!(exposure_stop_ok(100, 30, mark, nn(30)));
        assert!(!exposure_stop_ok(100, 29, mark, nn(30)));
        assert!(exposure_stop_ok(100, 0, mark, 0));
        assert_eq!(net_after_close(10, SIDE_LONG, 4), 6);
        assert_eq!(net_after_close(10, SIDE_SHORT, 4), 14);
    }

    /// adl.rs `exposure_adl_drawdown_trigger_uses_the_touched_peak` (D28).
    #[test]
    fn exposure_drawdown_trigger_uses_the_touched_peak() {
        let now = 10 * DD_WINDOW_SECS;
        for (age, triggers) in [(DD_WINDOW_SECS, false), (DD_WINDOW_SECS - 1, true)] {
            let (mut m, mut pool) = (market(), pool());
            pool.params.dd_adl_bps = 1_500;
            pool.params.adl_net_target_bps = 5_000;
            let long = open(&mut m, &mut pool, SIDE_LONG, 1_000 * C6, 75 * C6, 15_000 * C6);
            let mark = 76 * C6;
            let v0 = pool_view(&pool, &m, mark, now).unwrap();
            pool.nps_peak = v0.nps / 4 * 5;
            pool.nps_peak_at = now - age;
            let r = plan(&m, &pool, &long, ADL_MODE_EXPOSURE, 10 * C6, mark, now);
            if triggers {
                assert_eq!(r.unwrap().triggers, pool_math::TRIGGER_DRAWDOWN);
            } else {
                assert_eq!(r, Err(Reject::AdlNotRequired));
            }
        }
    }

    /// settle_close.rs `close_caps_apply_only_when_net_rises`,
    /// `weekend_close_cap_inside_the_window` and
    /// `close_caps_skip_equal_net_and_ceil_the_notional`.
    #[test]
    fn close_caps() {
        let m = market();
        let mut pool = LpPoolData { params: PoolParamsData::neutral(), ..Default::default() };
        pool.params.k_net_bps = 10_000;
        let v = PoolView { net: 20_000 * C6 as i128, b: 1_000_000 * C6 as u128, ..Default::default() };
        check_close_caps(&m, &pool, &v, SIDE_LONG, 1_000 * C6, 75 * C6, 0).unwrap();
        assert_eq!(check_close_caps(&m, &pool, &v, SIDE_SHORT, 1_000 * C6, 75 * C6, 0), Err(Reject::PoolExposureLimit));
        let v0 = PoolView { net: 0, b: 1_000_000 * C6 as u128, ..Default::default() };
        check_close_caps(&m, &pool, &v0, SIDE_SHORT, 13_333_333_333, 75 * C6, 0).unwrap();
        assert!(check_close_caps(&m, &pool, &v0, SIDE_SHORT, 13_333_333_334, 75 * C6, 0).is_err());

        let mut m = market();
        m.session_schedule = session::SCHEDULE_CME_GLOBEX;
        m.weekend_initial_margin_ratio = 2_000;
        let mut pool = LpPoolData { params: PoolParamsData::neutral(), ..Default::default() };
        pool.params.weekend_cap_window_secs = 86_400;
        pool.params.weekend_lp_loss_bps = 1_000;
        let size = 8_000 * C6;
        check_close_caps(&m, &pool, &v0, SIDE_SHORT, size, 75 * C6, WEDNESDAY).unwrap();
        assert_eq!(check_close_caps(&m, &pool, &v0, SIDE_SHORT, size, 75 * C6, FRIDAY_CLOSE - 3_600), Err(Reject::PoolWeekendExposureLimit));
        pool.params.weekend_gap_bps = 1_000;
        check_close_caps(&m, &pool, &v0, SIDE_SHORT, size, 75 * C6, FRIDAY_CLOSE - 3_600).unwrap();

        let m = market();
        let mut pool = LpPoolData { params: PoolParamsData::neutral(), ..Default::default() };
        pool.params.k_net_bps = 10_000;
        check_close_caps(&m, &pool, &PoolView { net: 5, b: 0, ..Default::default() }, SIDE_LONG, 10, 75 * C6, 0).unwrap();
        let e = check_close_caps(&m, &pool, &PoolView { net: 0, b: 225, ..Default::default() }, SIDE_SHORT, 3, 75_000_001, 0);
        assert_eq!(e, Err(Reject::PoolExposureLimit));
        check_close_caps(&m, &pool, &PoolView { net: 0, b: 226, ..Default::default() }, SIDE_SHORT, 3, 75_000_001, 0).unwrap();
    }

    /// settle_close.rs `profitable_closes_wait_out_the_quiet_time` and
    /// `close_fee_is_a_ceiling_of_opened_notional`.
    #[test]
    fn quiet_time_and_close_fee() {
        let mut m = market();
        let mut pool = pool();
        m.session_schedule = session::SCHEDULE_CME_GLOBEX;
        pool.params.profit_close_quiet_secs = 300;
        assert!(in_profit_close_quiet(&m, &pool, SUNDAY_OPEN + 299));
        assert!(!in_profit_close_quiet(&m, &pool, SUNDAY_OPEN + 300));
        assert!(!in_profit_close_quiet(&m, &pool, SUNDAY_OPEN - 1)); // closed
        m.session_schedule = session::SCHEDULE_ALWAYS_OPEN;
        assert!(!in_profit_close_quiet(&m, &pool, SUNDAY_OPEN));
        m.session_schedule = session::SCHEDULE_CME_GLOBEX;
        m.settlement_price = 70 * C6;
        assert!(!in_profit_close_quiet(&m, &pool, SUNDAY_OPEN)); // settled_at 0
        m.settled_at = SUNDAY_OPEN;
        assert!(in_profit_close_quiet(&m, &pool, SUNDAY_OPEN + 299));
        assert!(!in_profit_close_quiet(&m, &pool, SUNDAY_OPEN + 300));

        let mut p = PositionData { size: C6, opened_price: 75_000_001, entry_price: 75_000_001, ..Default::default() };
        assert_eq!(slice_close_fee(&p, p.size, 5), 37_501);
        p.entry_price = 1;
        assert_eq!(slice_close_fee(&p, p.size, 5), 37_501);
    }

    #[test]
    fn sweep_amount_mirrors_the_instruction() {
        let mut m = market();
        let mut pool = pool();
        m.insurance_fund = 15 * C6; // target $10
        assert_eq!(sweep_amount(&m, &pool), 5 * C6);
        m.insurance_fund = 10 * C6;
        assert_eq!(sweep_amount(&m, &pool), 0);
        // Settled and empty: the whole fund
        m.settlement_price = 70 * C6;
        assert_eq!(sweep_amount(&m, &pool), 10 * C6);
        pool.open_positions = 1;
        assert_eq!(sweep_amount(&m, &pool), 0);
        pool.open_positions = 0;
        pool.lp_shares = 0;
        assert_eq!(sweep_amount(&m, &pool), 0);
    }
}
