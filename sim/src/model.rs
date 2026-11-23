//! The engine's economics as the simulation predicts them: the venue margin
//! rules (docs/venue-settlement.md sections 2-3) and the LP pool that is the
//! house counterparty (docs/lp-pool.md, §0 D1-D40 where it departs from the
//! older text).
//!
//! Every instruction the simulation sends is first run here on copies of the
//! accounts it touches (Market, LpPool, Position, UserAccount, LpPosition, as
//! read from chain just before). The result is the error the engine must
//! fail with, or the exact account states and token flows it must produce;
//! the caller compares those with what the engine did, field by field.
//!
//! The pool's pure integer math (`pool_math.rs`) and the session helpers that
//! count open seconds (`session.rs`) are perps-core's own dependency-free
//! files, `#[path]`-included (docs/lp-pool.md §2.7, §16.7): they are already
//! unit-tested against brute force there, and re-deriving 256-bit mul-div or
//! the DST grid here would only add a second place for a rounding slip. The
//! instruction logic on top of them (check order, the stage-1 slice, the
//! waterfall, ADL sizing, LP share math, ...) is written out here from the
//! spec, and the weekend-margin schedule still comes from the independent
//! `schedule` module. Only perps-core's account types are used.

use crate::pool_math as pm;
use crate::predict::{core, oracle as oracle_check, OracleView, Rej, MAX_PRICE_AGE_SECS};
use crate::schedule;
use crate::session as ses;
use perps_core::state::{ExecutionSource, LpPool, LpPosition, Market, PoolParams, Position, PositionStatus, Side, UserAccount};

pub use pm::{Payout, PoolView};

pub const BPS: i128 = 10_000;
pub const ONE: i128 = 1_000_000;

fn ceil_div(a: i128, b: i128) -> i128 {
    (a + b - 1) / b
}

// --- session and margins ---------------------------------------------------------

/// Whether the market is in session at `now` (its schedule, from `schedule`).
pub fn in_session(m: &Market, now: i64) -> bool {
    schedule::market_open(m.session_schedule, now)
}

/// `(initial, maintenance)` margin in basis points at `now`: the weekend
/// values from `pre_close_window_secs` before the weekly close.
pub fn margin_ratios(m: &Market, now: i64) -> (u32, u32) {
    if schedule::market_pre_close(m.session_schedule, now, m.pre_close_window_secs) {
        (m.initial_margin_ratio.max(m.weekend_initial_margin_ratio), m.maintenance_margin_ratio.max(m.weekend_maintenance_margin_ratio))
    } else {
        (m.initial_margin_ratio, m.maintenance_margin_ratio)
    }
}

/// Highest leverage (3 dp) the initial margin in force allows.
pub fn max_leverage(m: &Market, now: i64) -> u32 {
    let (im, _) = margin_ratios(m, now);
    ((BPS * 1_000 / im as i128) as u32).min(m.max_leverage)
}

/// The pool's pre-close window `W` (`weekend_cap_window_secs`, §3).
pub fn in_pool_window(m: &Market, p: &PoolParams, now: i64) -> bool {
    schedule::market_pre_close(m.session_schedule, now, p.weekend_cap_window_secs)
}

/// The LP reopen quiet time `Q` (D17).
pub fn in_lp_quiet(m: &Market, p: &PoolParams, now: i64) -> bool {
    ses::seconds_since_open(m.session_schedule, now).map_or(false, |t| t < p.lp_reopen_quiet_secs as i64)
}

/// The profit-close quiet time (D23, D35): after a session open, or once
/// settled, after `settled_at`.
pub fn in_profit_close_quiet(m: &Market, p: &PoolParams, now: i64) -> bool {
    let q = p.profit_close_quiet_secs as i64;
    if settled(m) {
        m.settled_at > 0 && now.saturating_sub(m.settled_at) < q
    } else {
        ses::seconds_since_open(m.session_schedule, now).map_or(false, |t| t < q)
    }
}

pub fn settled(m: &Market) -> bool {
    m.settlement_price > 0
}

pub fn funding_current(m: &Market, now: i64) -> bool {
    ses::funding_current(m.session_schedule, settled(m), m.last_funding_time, m.funding_carry_secs, m.funding_interval, now)
}

/// Open seconds since the last crank plus the carry (§8 step 3).
pub fn funding_open_total(m: &Market, now: i64) -> i64 {
    ses::open_seconds_between(m.session_schedule, m.last_funding_time, now).saturating_add(m.funding_carry_secs as i64)
}

// --- position arithmetic --------------------------------------------------------------

pub fn sign(side: Side) -> i128 {
    match side {
        Side::Long => 1,
        Side::Short => -1,
    }
}

pub fn opposite(side: Side) -> Side {
    match side {
        Side::Long => Side::Short,
        Side::Short => Side::Long,
    }
}

pub fn notional(size: u64, price: u64) -> i128 {
    size as i128 * price as i128 / ONE
}

/// `floor(size·mark, 1e6)`: the mark notional liquidation uses (§6.1).
pub fn mark_notional(size: u64, mark: u64) -> u128 {
    size as u128 * mark as u128 / ONE as u128
}

/// `floor(size·opened_price, 1e6)` (D21).
pub fn opened_notional(p: &Position, size: u64) -> u64 {
    pm::notional_floor(size, p.opened_price)
}

/// Unrealised PnL at `mark`, truncated toward zero.
pub fn pnl(side: Side, size: u64, entry: u64, mark: u64) -> i128 {
    sign(side) * size as i128 * (mark as i128 - entry as i128) / ONE
}

/// Funding owed to the position since it last settled (positive = received).
/// `cumulative` is funding per whole contract in USDC; positive means longs pay.
pub fn funding(side: Side, size: u64, last: i64, cumulative: i64) -> i128 {
    -sign(side) * (size as i128 * (cumulative as i128 - last as i128) / ONE)
}

/// PnL plus accrued funding of the whole position.
pub fn realized(p: &Position, mark: u64, cumulative: i64) -> i128 {
    pnl(p.side, p.size, p.entry_price, mark) + funding(p.side, p.size, p.last_funding_payment, cumulative)
}

/// Collateral + PnL + accrued funding at `mark`.
pub fn equity(p: &Position, mark: u64, cumulative: i64) -> i128 {
    p.collateral as i128 + realized(p, mark, cumulative)
}

/// Equity over entry notional in basis points, 0 when underwater (display;
/// the agents' own margin management).
pub fn margin_ratio_bps(p: &Position, mark: u64, cumulative: i64) -> i128 {
    let e = equity(p, mark, cumulative);
    let n = notional(p.size, p.entry_price);
    if e <= 0 || n == 0 {
        0
    } else {
        e * BPS / n
    }
}

/// Equity over mark notional in basis points, 0 when underwater (§6.1).
pub fn margin_ratio_mark_bps(p: &Position, mark: u64, cumulative: i64) -> i128 {
    let e = equity(p, mark, cumulative);
    let n = mark_notional(p.size, mark) as i128;
    if e <= 0 || n == 0 {
        0
    } else {
        e * BPS / n
    }
}

/// §6.1: `equity·10_000 < mmr·N_mark`, with `N_mark == 0` only when equity < 0.
pub fn liquidatable(m: &Market, p: &Position, mark: u64, now: i64) -> bool {
    let (_, mmr) = margin_ratios(m, now);
    let e = equity(p, mark, m.cumulative_funding);
    let n = mark_notional(p.size, mark);
    if n == 0 {
        e < 0
    } else {
        e * BPS < mmr as i128 * n as i128
    }
}

/// Collateral at `leverage` on the notional at `price`, rounded up; None when
/// that is below the initial margin in force.
pub fn open_margin(m: &Market, size: u64, price: u64, leverage: u32, now: i64) -> Option<u64> {
    if leverage == 0 || leverage > m.max_leverage {
        return None;
    }
    let n = notional(size, price);
    let base = ceil_div(n * 1_000, leverage as i128);
    let (im, _) = margin_ratios(m, now);
    if base < ceil_div(n * im as i128, BPS) {
        return None;
    }
    u64::try_from(base).ok()
}

/// Collateral a venue fill (or a house open at `price`) posts for one side
/// (venue-settlement spec 2.2): `max(notional / lev, IM x oracle notional) +
/// immediate loss at the oracle`.
pub fn venue_margin(m: &Market, side: Side, size: u64, price: u64, oracle: u64, leverage: u32, now: i64) -> Option<u64> {
    let base = open_margin(m, size, price, leverage, now)? as i128;
    let (im, _) = margin_ratios(m, now);
    let im_oracle = ceil_div(notional(size, oracle) * im as i128, BPS);
    let adverse = (-pnl(side, size, price, oracle)).max(0);
    u64::try_from(base.max(im_oracle) + adverse).ok()
}

/// |price - oracle| <= bps of the oracle (spec 2: the venue band, inclusive).
pub fn within_band(price: u64, oracle: u64, bps: u16) -> bool {
    (price as i128 - oracle as i128).abs() * BPS <= oracle as i128 * bps as i128
}

/// The approval's adverse-only deviation bound (0 = off).
pub fn within_adverse(side: Side, price: u64, oracle: u64, bps: u16) -> bool {
    if bps == 0 {
        return true;
    }
    let (p, o, b) = (price as i128 * BPS, oracle as i128, bps as i128);
    match side {
        Side::Long => p <= o * (BPS + b),
        Side::Short => p >= o * (BPS - b).max(0),
    }
}

// --- funding (§8, D40) ---------------------------------------------------------------

/// update_funding's per-period rate (6 dp): a 0.01%-per-period base rate
/// scaled by the open-interest imbalance (L - S) / (L + S), toward zero.
pub fn funding_rate(long_oi: u64, short_oi: u64) -> i64 {
    let (l, s) = (long_oi as i128, short_oi as i128);
    if l + s == 0 {
        return 0;
    }
    (100 * (l - s) / (l + s)) as i64
}

/// One period's step of `cumulative_funding`: the rate applied to the
/// period's oracle price, per whole contract (USDC, 6 dp), toward zero.
pub fn funding_step(rate: i64, oracle: u64) -> i64 {
    (rate as i128 * oracle as i128 / ONE) as i64
}

/// One funding crank (the FundingUpdated event).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FundingEv {
    pub n: u64,
    pub dropped: u64,
    pub rate: i64,
    pub step: i64,
    pub carry: u32,
}

/// §8 steps 3-8 at `oracle`: every open period due since the last crank (up
/// to 168) at the current imbalance. None (and nothing changes) when none is due.
pub fn apply_funding(m: &mut Market, oracle: u64, now: i64) -> Option<FundingEv> {
    let c = pm::funding_catchup(funding_open_total(m, now), m.funding_interval)?;
    let rate = funding_rate(m.long_open_interest, m.short_open_interest);
    let step = funding_step(rate, oracle);
    m.funding_rate = rate;
    m.cumulative_funding = pm::funding_accumulate(m.cumulative_funding, c.n, step);
    m.last_funding_time = now;
    m.funding_carry_secs = c.carry;
    Some(FundingEv { n: c.n, dropped: c.dropped, rate, step, carry: c.carry })
}

/// The inline crank of every trade (D40); nothing once settled.
pub fn crank(m: &mut Market, oracle: u64, now: i64) -> Option<FundingEv> {
    if settled(m) {
        None
    } else {
        apply_funding(m, oracle, now)
    }
}

/// update_funding: gates, then the crank (FundingNotDue when nothing is due).
pub fn update_funding(m: &mut Market, o: &OracleView, now: i64) -> Result<FundingEv, Rej> {
    if settled(m) {
        return Err(core("MarketSettled"));
    }
    if !in_session(m, now) {
        return Err(core("MarketClosed"));
    }
    if let Some(r) = oracle_check(m, o, now, MAX_PRICE_AGE_SECS) {
        return Err(r);
    }
    apply_funding(m, o.price, now).ok_or(core("FundingNotDue"))
}

// --- the pool's state (§2) ------------------------------------------------------------

// --- admin transitions (§4.9, §4.11, §4.12, §5; D32, D42) ----------------------

/// pause_market (§4.11): sets `is_paused`, nothing else.
pub fn pause_market(m: &mut Market) {
    m.is_paused = true;
}

/// update_market_config (§4.12): writes the session and margin fields and
/// `is_paused`; `max_open_interest == 0` keeps the current cap (red-team F19).
pub fn update_market_config(m: &mut Market, p: &perps_core::instructions::MarketConfigParams) -> Result<(), Rej> {
    if settled(m) {
        return Err(core("MarketSettled"));
    }
    m.is_paused = p.is_paused;
    m.session_schedule = p.session_schedule;
    m.pre_close_window_secs = p.pre_close_window_secs;
    m.weekend_initial_margin_ratio = p.weekend_initial_margin_ratio;
    m.weekend_maintenance_margin_ratio = p.weekend_maintenance_margin_ratio;
    m.max_confidence_bps = p.max_confidence_bps;
    m.min_oracle_price = p.min_oracle_price;
    if p.max_open_interest != 0 {
        m.max_open_interest = p.max_open_interest;
    }
    Ok(())
}

/// settle_market (§4.9): first charges the funding due since the last crank
/// at the settlement price (D42), then sets `settlement_price`,
/// `settled_at = now` and `is_paused`. Insurance, the aggregates, the peak
/// and the vesting are untouched (the pool account is not even passed).
pub fn settle_market(m: &mut Market, price: u64, now: i64) -> Result<Option<FundingEv>, Rej> {
    if settled(m) {
        return Err(core("MarketSettled"));
    }
    if price == 0 {
        return Err(core("InvalidOraclePrice"));
    }
    let ev = apply_funding(m, price, now);
    m.settlement_price = price;
    m.settled_at = now;
    m.is_paused = true;
    Ok(ev)
}

/// propose_pool_params (§5, D32): only `pending_params` and
/// `pending_params_at`, effective after `params_timelock_secs`; applied at
/// once (and nothing pending) while the pool has no LP shares.
pub fn propose_pool_params(pool: &mut LpPool, params: PoolParams, now: i64) {
    if pool.lp_shares == 0 {
        pool.params = params;
        pool.pending_params = PoolParams::default();
        pool.pending_params_at = 0;
        return;
    }
    let cur = &pool.params;
    let t = pm::params_timelock_secs(cur.withdrawal_delay_secs, params.withdrawal_delay_secs, cur.weekend_cap_window_secs, params.weekend_cap_window_secs);
    pool.pending_params = params;
    pool.pending_params_at = now + t;
}

/// cancel_pool_params (D32): resets the proposal to default.
pub fn cancel_pool_params(pool: &mut LpPool) -> Result<(), Rej> {
    if pool.pending_params_at == 0 {
        return Err(core("NoPendingParams"));
    }
    pool.pending_params = PoolParams::default();
    pool.pending_params_at = 0;
    Ok(())
}

/// The market and its pool, as one instruction sees them.
#[derive(Clone)]
pub struct St {
    pub m: Market,
    pub pool: LpPool,
}

fn overflow() -> Rej {
    core("MathOverflow")
}

fn underflow() -> Rej {
    core("BucketUnderflow")
}

fn add(x: u64, y: u64) -> Result<u64, Rej> {
    x.checked_add(y).ok_or_else(overflow)
}

fn sub(x: u64, y: u64) -> Result<u64, Rej> {
    x.checked_sub(y).ok_or_else(underflow)
}

fn to_u64(x: i128) -> Result<u64, Rej> {
    u64::try_from(x).map_err(|_| overflow())
}

fn sat_i64(x: i128) -> i64 {
    x.clamp(i64::MIN as i128, i64::MAX as i128) as i64
}

pub fn pool_state(p: &LpPool) -> pm::PoolState {
    pm::PoolState {
        cash: p.cash,
        long_entry_sum: p.long_entry_sum,
        short_entry_sum: p.short_entry_sum,
        long_funding_sum: p.long_funding_sum,
        short_funding_sum: p.short_funding_sum,
        open_positions: p.open_positions,
        lp_shares: p.lp_shares,
        pending_withdraw_shares: p.pending_withdraw_shares,
        nps_peak: p.nps_peak,
        vest_amount: p.vest_amount,
        vest_start: p.vest_start,
        vest_end: p.vest_end,
    }
}

/// §2.2 valuation at `mark` (no skew premium once settled, D39).
pub fn view(pool: &LpPool, m: &Market, mark: u64, now: i64) -> Result<PoolView, Rej> {
    let skew = if settled(m) { 0 } else { pool.params.skew_scale };
    pm::pool_view_raw(
        &pool_state(pool), m.long_open_interest, m.short_open_interest, m.cumulative_funding, mark, now,
        pool.params.redeem_loss_haircut_bps, skew, pool.params.max_impact_bps,
    )
    .ok_or_else(overflow)
}

/// §2.2 peak tracking with the 7-day rolling reset.
pub fn touch_peak(pool: &mut LpPool, v: &PoolView, now: i64) {
    if pm::peak_resets(v.nps, pool.nps_peak, pool.nps_peak_at, now) {
        pool.nps_peak = v.nps;
        pool.nps_peak_at = now;
    }
}

/// Drawdown against the peak after `touch_peak` (D28).
pub fn dd_now(pool: &LpPool, v: &PoolView) -> u64 {
    pm::drawdown_bps(v.nps, pool.nps_peak)
}

/// `view` then `touch_peak`, as every pool instruction starts.
pub fn view_touch(st: &mut St, mark: u64, now: i64) -> Result<PoolView, Rej> {
    let v = view(&st.pool, &st.m, mark, now)?;
    touch_peak(&mut st.pool, &v, now);
    Ok(v)
}

fn agg(pool: &mut LpPool, side: Side, size: u64, entry: u64, f: i64, add: bool) -> Result<(), Rej> {
    let e = (size as u128).checked_mul(entry as u128).ok_or_else(overflow)?;
    let fz = (size as i128).checked_mul(f as i128).ok_or_else(overflow)?;
    let (es, fs) = match side {
        Side::Long => (&mut pool.long_entry_sum, &mut pool.long_funding_sum),
        Side::Short => (&mut pool.short_entry_sum, &mut pool.short_funding_sum),
    };
    if add {
        *es = es.checked_add(e).ok_or_else(overflow)?;
        *fs = fs.checked_add(fz).ok_or_else(overflow)?;
    } else {
        *es = es.checked_sub(e).ok_or_else(overflow)?;
        *fs = fs.checked_sub(fz).ok_or_else(overflow)?;
    }
    Ok(())
}

pub fn pool_add(pool: &mut LpPool, side: Side, size: u64, entry: u64, f: i64) -> Result<(), Rej> {
    agg(pool, side, size, entry, f, true)?;
    pool.open_positions = add(pool.open_positions, 1)?;
    Ok(())
}

pub fn pool_remove(pool: &mut LpPool, side: Side, size: u64, entry: u64, f: i64) -> Result<(), Rej> {
    agg(pool, side, size, entry, f, false)?;
    pool.open_positions = pool.open_positions.checked_sub(1).ok_or_else(overflow)?;
    Ok(())
}

pub fn pool_replace(pool: &mut LpPool, side: Side, old: (u64, u64, i64), new: (u64, u64, i64)) -> Result<(), Rej> {
    agg(pool, side, old.0, old.1, old.2, false)?;
    agg(pool, side, new.0, new.1, new.2, true)
}

fn oi_add(m: &mut Market, side: Side, x: u64) -> Result<(), Rej> {
    match side {
        Side::Long => m.long_open_interest = add(m.long_open_interest, x)?,
        Side::Short => m.short_open_interest = add(m.short_open_interest, x)?,
    }
    Ok(())
}

fn oi_sub(m: &mut Market, side: Side, x: u64) -> Result<(), Rej> {
    match side {
        Side::Long => m.long_open_interest = m.long_open_interest.checked_sub(x).ok_or_else(overflow)?,
        Side::Short => m.short_open_interest = m.short_open_interest.checked_sub(x).ok_or_else(overflow)?,
    }
    Ok(())
}

/// House notional of a slice `x` of `q` (telescopes, D21).
fn house_sub(u: &mut UserAccount, p: &Position, q: u64, x: u64) -> Result<(), Rej> {
    if p.execution_source == ExecutionSource::OrderBook {
        let d = opened_notional(p, q) - opened_notional(p, q - x);
        u.house_entry_notional = sub(u.house_entry_notional, d)?;
    }
    Ok(())
}

// --- skew price, fees, waterfall, payouts (§2.4-§2.8) -----------------------------

/// SIP-279 execution price for `delta` (+q buy, -q sell) against skew `L - S`.
pub fn skew_price(oracle: u64, skew_before: i128, delta: i128, p: &PoolParams) -> Result<u64, Rej> {
    let price = pm::skew_price_raw(oracle, skew_before, delta, p.skew_scale, p.max_impact_bps).ok_or_else(overflow)?;
    if price <= 0 {
        return Err(core("InvalidOraclePrice"));
    }
    to_u64(price)
}

pub fn net(m: &Market) -> i128 {
    m.long_open_interest as i128 - m.short_open_interest as i128
}

/// Insurance first until its target, the overflow to LP cash.
pub fn credit_insurance(m: &mut Market, pool: &mut LpPool, y: u64) -> Result<(), Rej> {
    let (to_ins, to_cash) = pm::insurance_split(m.insurance_fund, pool.params.insurance_target, y);
    m.insurance_fund = add(m.insurance_fund, to_ins)?;
    pool.cash = add(pool.cash, to_cash)?;
    Ok(())
}

/// A net fee: the treasury's cut, then insurance (and its overflow to cash).
pub fn distribute_fee(m: &mut Market, pool: &mut LpPool, x: u64) -> Result<(), Rej> {
    let (t, rest) = pm::fee_split(x, pool.params.treasury_fee_bps);
    pool.treasury = add(pool.treasury, t)?;
    credit_insurance(m, pool, rest)
}

/// D15: a new 24 h loss window when none is open.
pub fn roll_window(pool: &mut LpPool, nav_before: i128, now: i64) {
    if pool.loss_window_start == 0 || now >= pool.loss_window_start.saturating_add(pm::LOSS_WINDOW_SECS) {
        pool.loss_window_start = now;
        pool.loss_window_nav = u64::try_from(nav_before.max(0)).unwrap_or(u64::MAX);
        pool.lp_absorbed_window = 0;
        pool.ins_paid_window = 0;
    }
}

/// One booked deficit (the BadDebt event).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Deficit {
    pub side: Side,
    pub d: u64,
    pub ins_paid: u64,
    pub lp_absorbed: u64,
    pub pending: u64,
}

/// §2.6: insurance (window-capped), the LP window cap, then pending on the
/// bankrupt side with its own debt-weighted clock (D36).
pub fn absorb(m: &mut Market, pool: &mut LpPool, d: u64, side: Side, nav_before: i128, now: i64) -> Result<Deficit, Rej> {
    roll_window(pool, nav_before, now);
    let cap_ins = pm::insurance_window_cap(pool.params.insurance_claim_cap_per_window, pool.ins_paid_window);
    let ins_paid = d.min(m.insurance_fund).min(cap_ins);
    m.insurance_fund = sub(m.insurance_fund, ins_paid)?;
    pool.cash = add(pool.cash, ins_paid)?;
    pool.ins_paid_window = pool.ins_paid_window.saturating_add(ins_paid);
    let lp_cap = pm::lp_window_cap(pool.loss_window_nav, pool.params.lp_loss_cap_window_bps, pool.lp_absorbed_window);
    let lp_absorbed = (d - ins_paid).min(lp_cap);
    pool.lp_absorbed_window = pool.lp_absorbed_window.saturating_add(lp_absorbed);
    let pending = d - ins_paid - lp_absorbed;
    if pending > 0 {
        let (slot, since) = match side {
            Side::Long => (&mut pool.pending_bad_debt_long, &mut pool.pending_since_long),
            Side::Short => (&mut pool.pending_bad_debt_short, &mut pool.pending_since_short),
        };
        *since = pm::weighted_since(*since, *slot, now, pending);
        *slot = add(*slot, pending)?;
    }
    pool.bad_debt_cum = pool.bad_debt_cum.saturating_add(d);
    Ok(Deficit { side, d, ins_paid, lp_absorbed, pending })
}

impl std::fmt::Debug for Deficit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Deficit {{ side: {}, d: {}, ins_paid: {}, lp_absorbed: {}, pending: {} }}", self.side as u8, self.d, self.ins_paid, self.lp_absorbed, self.pending)
    }
}

/// The pending a winner on `side` repays (D13): a short repays bankrupt longs.
pub fn opp_pending(pool: &LpPool, side: Side) -> u64 {
    match side {
        Side::Short => pool.pending_bad_debt_long,
        Side::Long => pool.pending_bad_debt_short,
    }
}

/// `(pending, since)` of the bankrupt `side`.
pub fn pending_of(pool: &LpPool, bankrupt: Side) -> (u64, i64) {
    match bankrupt {
        Side::Long => (pool.pending_bad_debt_long, pool.pending_since_long),
        Side::Short => (pool.pending_bad_debt_short, pool.pending_since_short),
    }
}

/// §2.8 quote: solvency haircut, bad-debt haircut, then cash.
pub fn quote_profit(pool: &LpPool, v: &PoolView, side: Side, p: u64, allow_short: bool) -> Result<Payout, Rej> {
    pm::quote_profit_raw(v.u, pool.cash, opp_pending(pool, side), p, allow_short).ok_or(core("PoolCashInsufficient"))
}

pub fn apply_payout(pool: &mut LpPool, side: Side, po: &Payout) -> Result<(), Rej> {
    if po.bad_debt_hc > 0 {
        let (slot, since) = match side {
            Side::Short => (&mut pool.pending_bad_debt_long, &mut pool.pending_since_long),
            Side::Long => (&mut pool.pending_bad_debt_short, &mut pool.pending_since_short),
        };
        *slot = sub(*slot, po.bad_debt_hc)?;
        if *slot == 0 {
            *since = 0;
        }
        pool.bad_debt_recovered_cum = pool.bad_debt_recovered_cum.saturating_add(po.bad_debt_hc);
    }
    pool.cash = sub(pool.cash, po.paid)?;
    Ok(())
}

fn pay_profit(pool: &mut LpPool, v: &PoolView, side: Side, p: u64, allow_short: bool) -> Result<Payout, Rej> {
    let po = quote_profit(pool, v, side, p, allow_short)?;
    apply_payout(pool, side, &po)?;
    Ok(po)
}

/// §10.1 step 4: new pool value repays pending, longs first.
pub fn recover_pending(pool: &mut LpPool, x: u64) -> u64 {
    let r_l = x.min(pool.pending_bad_debt_long);
    let r_s = (x - r_l).min(pool.pending_bad_debt_short);
    pool.pending_bad_debt_long -= r_l;
    pool.pending_bad_debt_short -= r_s;
    if pool.pending_bad_debt_long == 0 {
        pool.pending_since_long = 0;
    }
    if pool.pending_bad_debt_short == 0 {
        pool.pending_since_short = 0;
    }
    pool.bad_debt_recovered_cum = pool.bad_debt_recovered_cum.saturating_add(r_l + r_s);
    r_l + r_s
}

/// §10.3 with the amount-weighted, floored end (D26).
pub fn add_vesting(pool: &mut LpPool, y: u64, now: i64) -> Result<(), Rej> {
    if y == 0 {
        return Ok(());
    }
    let delay = (pool.params.withdrawal_delay_secs as i64).max(1);
    let rem = pm::unvested(pool.vest_amount, pool.vest_start, pool.vest_end, now);
    let total = add(rem, y)?;
    let left_old = if rem > 0 { (pool.vest_end - now) as u128 } else { 0 };
    let weighted = rem as u128 * left_old + y as u128 * delay as u128;
    let left = (weighted / total as u128).max(1);
    pool.vest_amount = total;
    pool.vest_start = now;
    pool.vest_end = now + left as i64;
    Ok(())
}

/// Every open position must equal Σ the aggregates; free and position
/// collateral are buckets. The vault holds them plus insurance, cash and treasury.
pub fn bucket_total(st: &St) -> u128 {
    let p = &st.pool;
    p.free_collateral as u128 + p.position_collateral as u128 + st.m.insurance_fund as u128 + p.cash as u128 + p.treasury as u128
}

// --- collateral flows (§4.1-§4.3) -----------------------------------------------------

pub fn deposit(st: &mut St, u: &mut UserAccount, amount: u64) -> Result<(), Rej> {
    u.collateral_balance = add(u.collateral_balance, amount)?;
    st.pool.free_collateral = add(st.pool.free_collateral, amount)?;
    Ok(())
}

pub fn withdraw(st: &mut St, u: &mut UserAccount, amount: u64) -> Result<(), Rej> {
    if u.collateral_balance < amount {
        return Err(core("InsufficientCollateral"));
    }
    u.collateral_balance -= amount;
    st.pool.free_collateral = sub(st.pool.free_collateral, amount)?;
    Ok(())
}

pub fn add_margin(st: &mut St, u: &mut UserAccount, p: &mut Position, amount: u64, now: i64) -> Result<(), Rej> {
    if u.collateral_balance < amount {
        return Err(core("InsufficientCollateral"));
    }
    u.collateral_balance -= amount;
    p.collateral = add(p.collateral, amount)?;
    st.pool.free_collateral = sub(st.pool.free_collateral, amount)?;
    st.pool.position_collateral = add(st.pool.position_collateral, amount)?;
    p.last_updated_at = now;
    Ok(())
}

// --- open_position (§4.4) ---------------------------------------------------------------

#[derive(Clone)]
pub struct OpenOut {
    pub position: Position,
    pub exec: u64,
    pub notional: u64,
    pub fee: u64,
    pub funding: Option<FundingEv>,
}

fn acceptable_ok(buy: bool, exec: u64, bound: u64) -> bool {
    bound == 0 || if buy { exec <= bound } else { exec >= bound }
}

/// open_position by `owner` (house path), with the account constraints first.
#[allow(clippy::too_many_arguments)]
pub fn open(st: &mut St, u: &mut UserAccount, o: &OracleView, now: i64, side: Side, size: u64, leverage: u32, acceptable_price: u64) -> Result<OpenOut, Rej> {
    if st.m.is_paused {
        return Err(core("MarketPaused"));
    }
    if leverage == 0 || leverage > st.m.max_leverage {
        return Err(core("ExcessiveLeverage"));
    }
    if settled(&st.m) {
        return Err(core("MarketSettled"));
    }
    if !in_session(&st.m, now) {
        return Err(core("MarketClosed"));
    }
    if size == 0 {
        return Err(core("PositionTooSmall"));
    }
    if let Some(r) = oracle_check(&st.m, o, now, MAX_PRICE_AGE_SECS) {
        return Err(r);
    }
    let oracle = o.price;
    let ev = crank(&mut st.m, oracle, now);
    let oi = match side {
        Side::Long => st.m.long_open_interest,
        Side::Short => st.m.short_open_interest,
    };
    if oi.checked_add(size).map_or(true, |x| x > st.m.max_open_interest) {
        return Err(core("OpenInterestCapExceeded"));
    }
    if st.pool.lp_shares == 0 {
        return Err(core("PoolHasNoLps"));
    }
    // 4-6. Execution price, slippage, margin, fee
    let delta = sign(side) * size as i128;
    let exec = skew_price(oracle, net(&st.m), delta, &st.pool.params)?;
    if !acceptable_ok(side == Side::Long, exec, acceptable_price) {
        return Err(core("SlippageExceeded"));
    }
    let notional_exec = to_u64(size as i128 * exec as i128 / ONE)?;
    if notional_exec == 0 {
        return Err(core("PositionTooSmall"));
    }
    let margin = venue_margin(&st.m, side, size, exec, oracle, leverage, now).ok_or(core("ExcessiveLeverage"))?;
    let fee = to_u64(pm::ceil_u(notional_exec as u128 * st.pool.params.open_fee_bps as u128, 10_000) as i128)?;
    if u.collateral_balance < margin.saturating_add(fee) {
        return Err(core("InsufficientCollateral"));
    }
    // 7-8. Caps, at the oracle, against capNAV
    let v = view_touch(st, oracle, now)?;
    let p = st.pool.params;
    let after = v.net + delta;
    if after.unsigned_abs() > v.net.unsigned_abs() {
        if st.pool.pending_bad_debt_long > 0 || st.pool.pending_bad_debt_short > 0 {
            return Err(core("PoolAdlPending"));
        }
        if pm::dd_freeze(dd_now(&st.pool, &v), p.dd_freeze_bps) {
            return Err(core("PoolDrawdownFreeze"));
        }
        let nn = pm::ceil_u(after.unsigned_abs() * oracle as u128, ONE as u128);
        if !pm::net_cap_ok(nn, p.k_net_bps, v.cap_nav) {
            return Err(core("PoolExposureLimit"));
        }
        if in_pool_window(&st.m, &p, now) && !pm::weekend_cap_ok(nn, g_bps(&st.m, &p), p.weekend_lp_loss_bps, v.cap_nav) {
            return Err(core("PoolWeekendExposureLimit"));
        }
    }
    if !pm::per_account_ok(u.house_entry_notional, notional_exec, p.per_account_cap_bps, p.k_net_bps, v.cap_nav) {
        return Err(core("AccountExposureLimit"));
    }
    // 9. Effects
    let total = margin + fee;
    u.collateral_balance -= total;
    st.pool.free_collateral = sub(st.pool.free_collateral, total)?;
    st.pool.position_collateral = add(st.pool.position_collateral, margin)?;
    distribute_fee(&mut st.m, &mut st.pool, fee)?;
    let f = st.m.cumulative_funding;
    pool_add(&mut st.pool, side, size, exec, f)?;
    oi_add(&mut st.m, side, size)?;
    u.house_entry_notional = add(u.house_entry_notional, notional_exec)?;
    st.m.total_positions += 1;
    st.m.total_trades += 1;
    u.total_positions += 1;
    u.total_trades += 1;
    let position = Position {
        owner: u.owner, market: Default::default(), side, size, collateral: margin, entry_price: exec, leverage,
        realized_pnl: 0, last_funding_payment: f, opened_at: now, last_updated_at: now, status: PositionStatus::Open,
        execution_source: ExecutionSource::OrderBook, bump: 0, opened_price: exec,
    };
    Ok(OpenOut { position, exec, notional: notional_exec, fee, funding: ev })
}

/// `G_bps` (§3.1).
pub fn g_bps(m: &Market, p: &PoolParams) -> u64 {
    pm::g_bps(p.weekend_gap_bps, m.initial_margin_ratio, m.weekend_initial_margin_ratio)
}

// --- venue_fill's pool side (§4.8) --------------------------------------------------------

/// The effects of a landed venue fill on the market and pool (its rejections
/// are predict::venue_fill): the inline crank, both legs' margin into
/// position collateral, both legs into the aggregates, OI.
pub fn fill(st: &mut St, taker_side: Side, size: u64, price: u64, tc: u64, mc: u64, oracle: u64, now: i64) -> Result<Option<FundingEv>, Rej> {
    let ev = crank(&mut st.m, oracle, now);
    let both = add(tc, mc)?;
    st.pool.free_collateral = sub(st.pool.free_collateral, both)?;
    st.pool.position_collateral = add(st.pool.position_collateral, both)?;
    let f = st.m.cumulative_funding;
    pool_add(&mut st.pool, taker_side, size, price, f)?;
    pool_add(&mut st.pool, opposite(taker_side), size, price, f)?;
    oi_add(&mut st.m, Side::Long, size)?;
    oi_add(&mut st.m, Side::Short, size)?;
    st.m.total_trades += 1;
    Ok(ev)
}

// --- closes (§4.5, §10.5) --------------------------------------------------------------------

/// What a full close realized.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CloseOut {
    pub mark: u64,
    pub exec: u64,
    pub pnl: i128,
    pub funding_paid: i128,
    /// pnl + funding, before any haircut
    pub p: i128,
    pub p_eff: i128,
    pub solvency_hc: u64,
    pub bad_debt_hc: u64,
    pub fee: u64,
    pub payout: u64,
    pub deficit: Option<Deficit>,
    pub funding: Option<FundingEv>,
}

/// D14: a close (or mode-0 ADL slice) that raises |net| fits the net cap and,
/// inside W, the weekend cap, both against B.
pub fn check_close_caps(m: &Market, pool: &LpPool, v: &PoolView, side: Side, size: u64, mark: u64, now: i64) -> Result<(), Rej> {
    let after = v.net - sign(side) * size as i128;
    if after.unsigned_abs() <= v.net.unsigned_abs() {
        return Ok(());
    }
    let nn = pm::ceil_u(after.unsigned_abs() * mark as u128, ONE as u128);
    let p = &pool.params;
    if !pm::net_cap_ok(nn, p.k_net_bps, v.b) {
        return Err(core("PoolExposureLimit"));
    }
    if in_pool_window(m, p, now) && !pm::weekend_cap_ok(nn, g_bps(m, p), p.weekend_lp_loss_bps, v.b) {
        return Err(core("PoolWeekendExposureLimit"));
    }
    Ok(())
}

/// `ceil(opened_notional(x)·taker_fee, 10_000)` (D21, D22).
pub fn slice_close_fee(p: &Position, x: u64, taker_fee: u32) -> u64 {
    pm::ceil_u(opened_notional(p, x) as u128 * taker_fee as u128, 10_000) as u64
}

/// §4.5 steps 2-7 at `mark` (valuation) and `exec` (the position's price).
pub fn settle_close(st: &mut St, p: &mut Position, u: &mut UserAccount, mark: u64, exec: u64, now: i64) -> Result<CloseOut, Rej> {
    let is_settled = settled(&st.m);
    let (side, size, c) = (p.side, p.size, p.collateral);
    let v = view_touch(st, mark, now)?;
    let pnl_x = pnl(side, size, p.entry_price, exec);
    let fnd = funding(side, size, p.last_funding_payment, st.m.cumulative_funding);
    let total = pnl_x + fnd;
    if total > 0 && in_profit_close_quiet(&st.m, &st.pool.params, now) {
        return Err(core("ProfitCloseQuiet"));
    }
    if !is_settled {
        check_close_caps(&st.m, &st.pool, &v, side, size, mark, now)?;
    }
    let (p_eff, solvency_hc, bad_debt_hc) = if total > 0 {
        let po = pay_profit(&mut st.pool, &v, side, to_u64(total)?, false)?;
        (po.paid as i128, po.solvency_hc, po.bad_debt_hc)
    } else {
        (total, 0, 0)
    };
    let fee = slice_close_fee(p, size, st.m.taker_fee);
    let eq = c as i128 + p_eff;
    let fee_c = eq.clamp(0, fee as i128) as u64;
    let e = eq - fee_c as i128;
    let (payout, deficit) = if e >= 0 {
        if p_eff <= 0 {
            st.pool.cash = add(st.pool.cash, to_u64(-p_eff)?)?;
        }
        distribute_fee(&mut st.m, &mut st.pool, fee_c)?;
        (to_u64(e)?, None)
    } else {
        st.pool.cash = add(st.pool.cash, c)?;
        let d = absorb(&mut st.m, &mut st.pool, to_u64(-e)?, side, v.nav_redeem, now)?;
        (0, Some(d))
    };
    st.pool.position_collateral = sub(st.pool.position_collateral, c)?;
    pool_remove(&mut st.pool, side, size, p.entry_price, p.last_funding_payment)?;
    oi_sub(&mut st.m, side, size)?;
    house_sub(u, p, size, size)?;
    p.status = PositionStatus::Closed;
    p.realized_pnl = p.realized_pnl.saturating_add(sat_i64(p_eff - fee_c as i128));
    p.last_updated_at = now;
    u.realized_pnl = u.realized_pnl.saturating_add(p.realized_pnl);
    u.total_trades += 1;
    Ok(CloseOut { mark, exec, pnl: pnl_x, funding_paid: fnd, p: total, p_eff, solvency_hc, bad_debt_hc, fee: fee_c, payout, deficit, funding: None })
}

/// close_position by the owner (no referral): price, slippage, crank, settle.
pub fn close_position(st: &mut St, p: &mut Position, u: &mut UserAccount, o: &OracleView, now: i64, acceptable_price: u64) -> Result<CloseOut, Rej> {
    if p.status != PositionStatus::Open {
        return Err(core("PositionAlreadyClosed"));
    }
    let (mark, exec) = if settled(&st.m) {
        (st.m.settlement_price, st.m.settlement_price)
    } else {
        if !in_session(&st.m, now) {
            return Err(core("MarketClosed"));
        }
        if let Some(r) = oracle_check(&st.m, o, now, MAX_PRICE_AGE_SECS) {
            return Err(r);
        }
        // Closing a long is a sell
        let exec = skew_price(o.price, net(&st.m), -sign(p.side) * p.size as i128, &st.pool.params)?;
        (o.price, exec)
    };
    if !acceptable_ok(p.side == Side::Short, exec, acceptable_price) {
        return Err(core("SlippageExceeded"));
    }
    let ev = crank(&mut st.m, mark, now);
    let mut out = settle_close(st, p, u, mark, exec, now)?;
    out.funding = ev;
    Ok(out)
}

/// close_settled_position: anyone, at the settlement price; the payout goes to free collateral.
pub fn close_settled(st: &mut St, p: &mut Position, u: &mut UserAccount, now: i64) -> Result<CloseOut, Rej> {
    if !settled(&st.m) {
        return Err(core("MarketNotSettled"));
    }
    if p.status != PositionStatus::Open {
        return Err(core("PositionAlreadyClosed"));
    }
    let price = st.m.settlement_price;
    let out = settle_close(st, p, u, price, price, now)?;
    u.collateral_balance = add(u.collateral_balance, out.payout)?;
    st.pool.free_collateral = add(st.pool.free_collateral, out.payout)?;
    Ok(out)
}

// --- liquidation (§6) -------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LiqOut {
    pub stage: u8,
    pub size_closed: u64,
    pub size_remaining: u64,
    pub penalty: u64,
    /// Paid to the liquidator's token account
    pub keeper_reward: u64,
    /// Stage 2: the part of the reward the insurance fund paid
    pub top_up: u64,
    pub owner_leftover: u64,
    /// pnl + funding of what closed, before haircuts
    pub realized: i128,
    /// What it realized after the haircuts
    pub r: i128,
    pub solvency_hc: u64,
    pub bad_debt_hc: u64,
    pub cash_hc: u64,
    pub deficit: Option<Deficit>,
    pub funding: Option<FundingEv>,
    pub mark: u64,
}

/// §6.3 step 1: the smallest slice after which `E − penalty ≥ IM·remaining`,
/// at least the minimum slice, the whole position below the minimum remainder.
pub fn stage1_slice(size: u64, mark: u64, equity: i128, im: u32, p: u16, min_slice_bps: u16, min_remaining: u64) -> u64 {
    let x_need: i128 = if im as i128 > p as i128 {
        let num = (im as i128).checked_mul(size as i128).and_then(|v| v.checked_mul(mark as i128)).and_then(|v| equity.checked_mul(10_000_000_000).and_then(|e| v.checked_sub(e)));
        let den = (mark as i128).checked_mul(im as i128 - p as i128);
        match (num, den) {
            (Some(n), Some(d)) if d > 0 => pm::ceil_s(n, d).max(0),
            _ => size as i128,
        }
    } else {
        size as i128
    };
    let min_slice = pm::ceil_u(size as u128 * min_slice_bps as u128, 10_000) as i128;
    let x = (x_need.max(min_slice).clamp(1, size.max(1) as i128) as u64).min(size);
    if pm::notional_floor(size - x, mark) < min_remaining {
        size
    } else {
        x
    }
}

/// §6.3 step 2: `min(ceil(floor(x·m)·p, 1e4), max(E, 0))`.
pub fn stage1_penalty(x: u64, mark: u64, p: u16, equity: i128) -> u64 {
    let n = pm::notional_floor(x, mark) as u128;
    pm::ceil_u(n * p as u128, 10_000).min(equity.max(0) as u128).min(u64::MAX as u128) as u64
}

/// A slice's own share of accrued funding (D24): the whole's minus the remainder's.
pub fn slice_funding(p: &Position, x: u64, f: i64) -> i128 {
    let all = funding(p.side, p.size, p.last_funding_payment, f);
    if x >= p.size {
        return all;
    }
    all - funding(p.side, p.size - x, p.last_funding_payment, f)
}

/// pnl at `mark` of a slice `x` plus its funding share.
pub fn slice_profit(p: &Position, x: u64, mark: u64, f: i64) -> i128 {
    pnl(p.side, x, p.entry_price, mark) + slice_funding(p, x, f)
}

fn finish_full(st: &mut St, p: &mut Position, u: &mut UserAccount, status: PositionStatus, now: i64) -> Result<(), Rej> {
    let size = p.size;
    pool_remove(&mut st.pool, p.side, size, p.entry_price, p.last_funding_payment)?;
    oi_sub(&mut st.m, p.side, size)?;
    house_sub(u, p, size, size)?;
    p.status = status;
    p.last_updated_at = now;
    u.realized_pnl = u.realized_pnl.saturating_add(p.realized_pnl);
    Ok(())
}

fn liq_stage1(st: &mut St, v: &PoolView, p: &mut Position, u: &mut UserAccount, mark: u64, im: u32, e: i128, now: i64) -> Result<LiqOut, Rej> {
    let params = st.pool.params;
    let (size, side, c) = (p.size, p.side, p.collateral);
    let f = st.m.cumulative_funding;
    let x = stage1_slice(size, mark, e, im, params.liq_penalty_bps, params.min_liq_slice_bps, params.liq_min_remaining_notional);
    if x < size {
        let realized = slice_profit(p, x, mark, f);
        let pen = stage1_penalty(x, mark, params.liq_penalty_bps, e);
        let quote = if realized > 0 { quote_profit(&st.pool, v, side, to_u64(realized)?, true)? } else { Payout::default() };
        let r = if realized > 0 { quote.paid as i128 } else { realized };
        let c_new = c as i128 + r - pen as i128;
        let rest_equity = c_new + pnl(side, size - x, p.entry_price, mark) + funding(side, size - x, p.last_funding_payment, f);
        if c_new > 0 && rest_equity > 0 {
            if realized > 0 {
                apply_payout(&mut st.pool, side, &quote)?;
            } else {
                st.pool.cash = add(st.pool.cash, to_u64(-realized)?)?;
            }
            let pc = st.pool.position_collateral as i128 + (c_new - c as i128);
            st.pool.position_collateral = u64::try_from(pc).map_err(|_| underflow())?;
            let keeper = (pen as u128 * params.liq_keeper_share_bps as u128 / 10_000) as u64;
            credit_insurance(&mut st.m, &mut st.pool, pen - keeper)?;
            let rest = size - x;
            let f_old = p.last_funding_payment;
            pool_replace(&mut st.pool, side, (size, p.entry_price, f_old), (rest, p.entry_price, f_old))?;
            oi_sub(&mut st.m, side, x)?;
            house_sub(u, p, size, x)?;
            p.size = rest;
            p.collateral = to_u64(c_new)?;
            p.realized_pnl = p.realized_pnl.saturating_add(sat_i64(r - pen as i128));
            p.last_updated_at = now;
            return Ok(LiqOut {
                stage: 1, size_closed: x, size_remaining: rest, penalty: pen, keeper_reward: keeper, realized, r,
                solvency_hc: quote.solvency_hc, bad_debt_hc: quote.bad_debt_hc, cash_hc: quote.cash_hc, mark, ..Default::default()
            });
        }
    }
    // Full stage-1 close
    let realized = realized(p, mark, f);
    let (r, po) = if realized > 0 {
        let po = pay_profit(&mut st.pool, v, side, to_u64(realized)?, true)?;
        (po.paid as i128, po)
    } else {
        st.pool.cash = add(st.pool.cash, to_u64(-realized)?)?;
        (realized, Payout::default())
    };
    let e_after = (c as i128 + r).max(0);
    let pen = stage1_penalty(size, mark, params.liq_penalty_bps, e).min(to_u64(e_after)?);
    let keeper = (pen as u128 * params.liq_keeper_share_bps as u128 / 10_000) as u64;
    let leftover = to_u64(c as i128 + r - pen as i128)?;
    st.pool.position_collateral = sub(st.pool.position_collateral, c)?;
    credit_insurance(&mut st.m, &mut st.pool, pen - keeper)?;
    u.collateral_balance = add(u.collateral_balance, leftover)?;
    st.pool.free_collateral = add(st.pool.free_collateral, leftover)?;
    p.realized_pnl = p.realized_pnl.saturating_add(sat_i64(r - pen as i128));
    finish_full(st, p, u, PositionStatus::Liquidated, now)?;
    Ok(LiqOut {
        stage: 1, size_closed: size, size_remaining: 0, penalty: pen, keeper_reward: keeper, owner_leftover: leftover, realized, r,
        solvency_hc: po.solvency_hc, bad_debt_hc: po.bad_debt_hc, cash_hc: po.cash_hc, mark, ..Default::default()
    })
}

#[allow(clippy::too_many_arguments)]
fn liq_stage2(st: &mut St, v: &PoolView, p: &mut Position, u: &mut UserAccount, mark: u64, mmr: u32, nav0: i128, now: i64) -> Result<LiqOut, Rej> {
    let (size, side, c) = (p.size, p.side, p.collateral);
    let on = opened_notional(p, size);
    let target = (on as u128 * mmr as u128 * st.m.liquidation_fee as u128 / 100_000_000).min(u64::MAX as u128) as u64;
    let realized = realized(p, mark, st.m.cumulative_funding);
    let (r, po) = if realized > 0 {
        let po = pay_profit(&mut st.pool, v, side, to_u64(realized)?, true)?;
        (po.paid as i128, po)
    } else {
        (realized, Payout::default())
    };
    let e_after = c as i128 + r;
    roll_window(&mut st.pool, nav0, now);
    let cap_ins = pm::insurance_window_cap(st.pool.params.insurance_claim_cap_per_window, st.pool.ins_paid_window);
    let (from_col, top, deficit) = if e_after >= 0 {
        if realized <= 0 {
            st.pool.cash = add(st.pool.cash, to_u64(-realized)?)?;
        }
        let e = to_u64(e_after)?;
        let from_col = target.min(e);
        credit_insurance(&mut st.m, &mut st.pool, e - from_col)?;
        let top = (target - from_col).min(st.m.insurance_fund).min(cap_ins);
        st.m.insurance_fund -= top;
        st.pool.ins_paid_window = st.pool.ins_paid_window.saturating_add(top);
        (from_col, top, None)
    } else {
        let top = target.min(st.m.insurance_fund).min(cap_ins);
        st.m.insurance_fund -= top;
        st.pool.ins_paid_window = st.pool.ins_paid_window.saturating_add(top);
        st.pool.cash = add(st.pool.cash, c)?;
        let d = absorb(&mut st.m, &mut st.pool, to_u64(-e_after)?, side, nav0, now)?;
        (0, top, Some(d))
    };
    st.pool.position_collateral = sub(st.pool.position_collateral, c)?;
    p.realized_pnl = p.realized_pnl.saturating_add(sat_i64(r));
    finish_full(st, p, u, PositionStatus::Liquidated, now)?;
    Ok(LiqOut {
        stage: 2, size_closed: size, size_remaining: 0, keeper_reward: from_col + top, top_up: top, realized, r,
        solvency_hc: po.solvency_hc, bad_debt_hc: po.bad_debt_hc, cash_hc: po.cash_hc, deficit, mark, ..Default::default()
    })
}

/// liquidate: gates, the crank, the trigger on mark notional, then the stage.
pub fn liquidate(st: &mut St, p: &mut Position, u: &mut UserAccount, o: &OracleView, now: i64) -> Result<LiqOut, Rej> {
    if p.status != PositionStatus::Open {
        return Err(core("PositionAlreadyClosed"));
    }
    if settled(&st.m) {
        return Err(core("MarketSettled"));
    }
    if !in_session(&st.m, now) {
        return Err(core("MarketClosed"));
    }
    if let Some(r) = oracle_check(&st.m, o, now, MAX_PRICE_AGE_SECS) {
        return Err(r);
    }
    let mark = o.price;
    let ev = crank(&mut st.m, mark, now);
    let mut out = liquidate_core(st, p, u, mark, now)?;
    out.funding = ev;
    Ok(out)
}

/// §6.2 steps 2-4 and the stage, after the gates and the crank.
pub fn liquidate_core(st: &mut St, p: &mut Position, u: &mut UserAccount, mark: u64, now: i64) -> Result<LiqOut, Rej> {
    let (im, mmr) = margin_ratios(&st.m, now);
    if !liquidatable(&st.m, p, mark, now) {
        return Err(core("NotLiquidatable"));
    }
    let e = equity(p, mark, st.m.cumulative_funding);
    let n = mark_notional(p.size, mark);
    let v = view_touch(st, mark, now)?;
    let backstop = mmr as u128 * st.pool.params.backstop_threshold_bps as u128 / 10_000;
    let stage1 = e >= 0 && (e as u128) * 10_000 >= backstop * n;
    if stage1 {
        liq_stage1(st, &v, p, u, mark, im, e, now)
    } else {
        liq_stage2(st, &v, p, u, mark, mmr, v.nav_redeem, now)
    }
}

// --- settle_position_pnl (§6.6) -----------------------------------------------------------

fn mark_price(m: &Market, o: &OracleView, now: i64) -> Result<u64, Rej> {
    if settled(m) {
        return Ok(m.settlement_price);
    }
    if !in_session(m, now) {
        return Err(core("MarketClosed"));
    }
    if let Some(r) = oracle_check(m, o, now, MAX_PRICE_AGE_SECS) {
        return Err(r);
    }
    Ok(o.price)
}

/// Moves a loser's unrealized loss into cash and resets its basis to `mark`.
pub fn settle_pnl(st: &mut St, p: &mut Position, o: &OracleView, now: i64) -> Result<u64, Rej> {
    if p.status != PositionStatus::Open {
        return Err(core("PositionAlreadyClosed"));
    }
    let mark = mark_price(&st.m, o, now)?;
    let f = st.m.cumulative_funding;
    let total = realized(p, mark, f);
    if total >= 0 {
        return Err(core("NothingToSettle"));
    }
    let loss = to_u64(-total)?;
    if loss >= p.collateral {
        return Err(core("SettleWouldBankrupt"));
    }
    p.collateral -= loss;
    st.pool.position_collateral = sub(st.pool.position_collateral, loss)?;
    st.pool.cash = add(st.pool.cash, loss)?;
    pool_replace(&mut st.pool, p.side, (p.size, p.entry_price, p.last_funding_payment), (p.size, mark, f))?;
    p.entry_price = mark;
    p.last_funding_payment = f;
    p.realized_pnl = p.realized_pnl.saturating_sub(loss as i64);
    p.last_updated_at = now;
    view_touch(st, mark, now)?;
    Ok(loss)
}

// --- ADL (§7) ------------------------------------------------------------------------------

pub const ADL_DEFICIT: u8 = 0;
pub const ADL_EXPOSURE: u8 = 1;

/// Age of pending booked at `since` (D35): open seconds, or once settled the
/// open seconds up to `settled_at` plus wall seconds after it.
pub fn pending_age(m: &Market, since: i64, now: i64) -> i64 {
    if settled(m) {
        let s = since.max(m.settled_at);
        ses::open_seconds_between(m.session_schedule, since, s).saturating_add(now.saturating_sub(s))
    } else {
        ses::open_seconds_between(m.session_schedule, since, now)
    }
}

/// D37: the smallest slice whose bad-debt haircut covers `opp`, else `q`.
pub fn deficit_slice(p: &Position, opp: u64, u: i128, cash: u64, mark: u64, f: i64) -> u64 {
    let q = p.size;
    let p_q = slice_profit(p, q, mark, f);
    if opp == 0 || p_q <= 0 {
        return q;
    }
    let need: i128 = if u > cash as i128 {
        if cash == 0 {
            return q;
        }
        pm::ceil_s(opp as i128 * u, cash as i128)
    } else {
        opp as i128
    };
    if need >= p_q {
        return q;
    }
    let x = pm::ceil_s((need + 4) * q as i128, p_q).clamp(1, q as i128) as u64;
    if slice_profit(p, x, mark, f) < need {
        q
    } else {
        x
    }
}

/// D24: equity a partial close of `x` leaves behind.
pub fn remainder_equity(p: &Position, x: u64, mark: u64, f: i64) -> i128 {
    let q = p.size;
    let c_x = if x == q { p.collateral } else { (p.collateral as u128 * x as u128 / q as u128) as u64 };
    (p.collateral - c_x) as i128 + pnl(p.side, q - x, p.entry_price, mark) + funding(p.side, q - x, p.last_funding_payment, f)
}

/// The position's ADL score (bps) at `mark`.
#[allow(dead_code)] // the ADL probes (adversary.rs)
pub fn adl_score(m: &Market, p: &Position, mark: u64) -> u128 {
    pm::adl_score(p.side == Side::Long, mark, p.entry_price, mark_notional(p.size, mark), equity(p, mark, m.cumulative_funding))
}

/// The mode-0 floor now for a candidate on `side` (the clock of the side it repays).
#[allow(dead_code)] // the ADL probes (adversary.rs)
pub fn adl_floor(m: &Market, pool: &LpPool, side: Side, now: i64) -> u64 {
    let (_, since) = pending_of(pool, opposite(side));
    pm::adl_floor_now(pending_age(m, since, now))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AdlOut {
    pub mode: u8,
    pub x: u64,
    pub profit: u64,
    pub paid: u64,
    pub solvency_hc: u64,
    pub bad_debt_hc: u64,
    pub fee: u64,
    pub credit: u64,
    pub score: u128,
    pub floor: u64,
    pub triggers: u8,
    pub target: u128,
    pub net_before: i128,
    pub net_after: i128,
    pub mark: u64,
    pub funding: Option<FundingEv>,
}

/// The exposure triggers that hold on `v`, and the smallest target among them.
pub fn exposure_triggers(m: &Market, pool: &LpPool, v: &PoolView, now: i64) -> (u8, u128) {
    let p = &pool.params;
    let g = g_bps(m, p);
    let w = in_pool_window(m, p, now);
    let (mut t, mut target) = (0u8, u128::MAX);
    if pm::n_trig(v.net_notional, p.hard_net_bps, v.b) {
        t |= pm::TRIGGER_NET;
        target = target.min(pm::bps_of(p.k_net_bps, v.b));
    }
    if pm::d_trig(dd_now(pool, v), p.dd_adl_bps) {
        t |= pm::TRIGGER_DRAWDOWN;
        target = target.min(pm::bps_of(p.adl_net_target_bps, v.b));
    }
    if pm::w_trig(w, v.net_notional, g, p.weekend_lp_loss_bps, v.b) {
        t |= pm::TRIGGER_WEEKEND;
        target = target.min(pm::weekend_target(p.weekend_lp_loss_bps, v.b, g));
    }
    (t, target)
}

/// adl(mode, size). `authorized` is whether the signer is the pool's
/// adl_keeper or the admin (mode 1 only).
#[allow(clippy::too_many_arguments)]
pub fn adl(st: &mut St, p: &mut Position, u: &mut UserAccount, mode: u8, size: u64, authorized: bool, o: &OracleView, now: i64) -> Result<AdlOut, Rej> {
    if p.status != PositionStatus::Open {
        return Err(core("PositionAlreadyClosed"));
    }
    if mode > ADL_EXPOSURE {
        return Err(core("InvalidAdlMode"));
    }
    if mode == ADL_EXPOSURE {
        if !authorized {
            return Err(core("AdlUnauthorized"));
        }
        if settled(&st.m) {
            return Err(core("MarketSettled"));
        }
    }
    let mark = mark_price(&st.m, o, now)?;
    let ev = crank(&mut st.m, mark, now);
    let mut out = adl_core(st, p, u, mode, size, mark, now)?;
    out.funding = ev;
    Ok(out)
}

/// §7.2 steps 3-10 at `mark`, after the mode and signer gates and the crank.
#[allow(clippy::too_many_arguments)]
pub fn adl_core(st: &mut St, p: &mut Position, u: &mut UserAccount, mode: u8, size: u64, mark: u64, now: i64) -> Result<AdlOut, Rej> {
    let v = view_touch(st, mark, now)?;
    let f = st.m.cumulative_funding;
    let params = st.pool.params;
    // 3. Size
    let q = p.size;
    let mut x = if size == 0 { q } else { size };
    if x > q {
        return Err(core("InvalidPositionReduction"));
    }
    let long = p.side == Side::Long;
    let (opp, opp_since) = pending_of(&st.pool, opposite(p.side));
    if mode == ADL_DEFICIT {
        x = x.min(deficit_slice(p, opp, v.u, st.pool.cash, mark, f));
    }
    if pm::notional_floor(q - x, mark) < params.liq_min_remaining_notional {
        x = q;
    }
    if x < q && remainder_equity(p, x, mark, f) <= 0 {
        x = q;
    }
    // 4. Profit on the slice
    let profit = slice_profit(p, x, mark, f);
    if profit <= 0 {
        return Err(core("AdlCandidateNotProfitable"));
    }
    let e = equity(p, mark, f);
    if e <= 0 {
        return Err(core("AdlCandidateNotProfitable"));
    }
    // 5. Score and floor
    let score = pm::adl_score(long, mark, p.entry_price, mark_notional(q, mark), e);
    let floor = if mode == ADL_DEFICIT { pm::adl_floor_now(pending_age(&st.m, opp_since, now)) } else { params.adl_exposure_score_floor_bps as u64 };
    if score < floor as u128 {
        return Err(core("AdlScoreBelowFloor"));
    }
    let (triggers, target) = if mode == ADL_DEFICIT {
        if st.pool.pending_bad_debt_long == 0 && st.pool.pending_bad_debt_short == 0 {
            return Err(core("AdlNotRequired"));
        }
        if opp == 0 {
            return Err(core("AdlWrongSide"));
        }
        if in_profit_close_quiet(&st.m, &params, now) {
            return Err(core("ProfitCloseQuiet"));
        }
        if !settled(&st.m) {
            check_close_caps(&st.m, &st.pool, &v, p.side, x, mark, now)?;
        }
        (0, 0)
    } else {
        let (t, target) = exposure_triggers(&st.m, &st.pool, &v, now);
        if t == 0 {
            return Err(core("AdlNotRequired"));
        }
        if !((v.net > 0 && long) || (v.net < 0 && !long)) {
            return Err(core("AdlWrongSide"));
        }
        (t, target)
    };
    let fee = if mode == ADL_DEFICIT { slice_close_fee(p, x, st.m.taker_fee) } else { 0 };
    // 9. Effects
    let side = p.side;
    let po = pay_profit(&mut st.pool, &v, side, to_u64(profit)?, false)?;
    let c = p.collateral;
    let c_x = if x == q { c } else { (c as u128 * x as u128 / q as u128) as u64 };
    st.pool.position_collateral = sub(st.pool.position_collateral, c_x)?;
    let gross = add(c_x, po.paid)?;
    let fee_c = fee.min(gross);
    distribute_fee(&mut st.m, &mut st.pool, fee_c)?;
    let credit = gross - fee_c;
    u.collateral_balance = add(u.collateral_balance, credit)?;
    st.pool.free_collateral = add(st.pool.free_collateral, credit)?;
    if x == q {
        pool_remove(&mut st.pool, side, q, p.entry_price, p.last_funding_payment)?;
    } else {
        let f_old = p.last_funding_payment;
        pool_replace(&mut st.pool, side, (q, p.entry_price, f_old), (q - x, p.entry_price, f_old))?;
    }
    oi_sub(&mut st.m, side, x)?;
    house_sub(u, p, q, x)?;
    p.realized_pnl = p.realized_pnl.saturating_add(po.paid as i64).saturating_sub(fee_c as i64);
    p.last_updated_at = now;
    if x == q {
        p.status = PositionStatus::Closed;
        u.realized_pnl = u.realized_pnl.saturating_add(p.realized_pnl);
    } else {
        p.size = q - x;
        p.collateral = c - c_x;
    }
    // 10. Exposure mode stops at its target
    let after = view(&st.pool, &st.m, mark, now)?;
    if mode == ADL_EXPOSURE {
        let same_sign = after.net == 0 || (after.net > 0) == (v.net > 0);
        if !(same_sign && after.net_notional >= target) {
            return Err(core("AdlOvershoot"));
        }
    }
    Ok(AdlOut {
        mode, x, profit: profit as u64, paid: po.paid, solvency_hc: po.solvency_hc, bad_debt_hc: po.bad_debt_hc, fee: fee_c, credit,
        score, floor, triggers, target, net_before: v.net, net_after: after.net, mark, funding: None,
    })
}

/// write_off_bad_debt: each side whose own age is ≥ 24 open hours (D36).
pub fn write_off(st: &mut St, now: i64) -> Result<u64, Rej> {
    if st.pool.pending_bad_debt_long == 0 && st.pool.pending_bad_debt_short == 0 {
        return Err(core("AdlNotRequired"));
    }
    let mut w = 0u64;
    let m = st.m.clone();
    for (pending, since) in [
        (&mut st.pool.pending_bad_debt_long, &mut st.pool.pending_since_long),
        (&mut st.pool.pending_bad_debt_short, &mut st.pool.pending_since_short),
    ] {
        if *pending > 0 && pending_age(&m, *since, now) >= pm::ADL_WRITE_OFF_SECS {
            w += *pending;
            *pending = 0;
            *since = 0;
        }
    }
    if w == 0 {
        return Err(core("BadDebtNotStale"));
    }
    st.pool.bad_debt_written_off_cum = st.pool.bad_debt_written_off_cum.saturating_add(w);
    Ok(w)
}

/// Whether some side's pending is old enough to write off now.
pub fn write_off_due(st: &St, now: i64) -> bool {
    [(st.pool.pending_bad_debt_long, st.pool.pending_since_long), (st.pool.pending_bad_debt_short, st.pool.pending_since_short)]
        .iter()
        .any(|&(p, since)| p > 0 && pending_age(&st.m, since, now) >= pm::ADL_WRITE_OFF_SECS)
}

// --- insurance, donations (§10) ---------------------------------------------------------------

/// `(amount, recovered, vested)`.
pub fn sweep(st: &mut St, now: i64) -> Result<(u64, u64, u64), Rej> {
    if st.pool.lp_shares == 0 {
        return Err(core("PoolHasNoLps"));
    }
    let x = if settled(&st.m) && st.pool.open_positions == 0 {
        st.m.insurance_fund
    } else {
        st.m.insurance_fund.saturating_sub(st.pool.params.insurance_target)
    };
    if x == 0 {
        return Err(core("NothingToSweep"));
    }
    st.m.insurance_fund -= x;
    st.pool.cash = add(st.pool.cash, x)?;
    let r = recover_pending(&mut st.pool, x);
    add_vesting(&mut st.pool, x - r, now)?;
    Ok((x, r, x - r))
}

#[allow(dead_code)] // the donation probes (adversary.rs)
pub fn donate(st: &mut St, amount: u64, now: i64) -> Result<(u64, u64), Rej> {
    if amount == 0 {
        return Err(core("InvalidLpAmount"));
    }
    if st.pool.lp_shares == 0 {
        return Err(core("PoolHasNoLps"));
    }
    st.pool.cash = add(st.pool.cash, amount)?;
    st.pool.donated_cum = st.pool.donated_cum.saturating_add(amount);
    let r = recover_pending(&mut st.pool, amount);
    add_vesting(&mut st.pool, amount - r, now)?;
    Ok((r, amount - r))
}

// --- LP flows (§9) ------------------------------------------------------------------------------

/// lp_deposit: the depositor's shares.
#[allow(dead_code)] // the LP agents
pub fn lp_deposit(st: &mut St, lp: &mut LpPosition, amount: u64, min_shares_out: u64, o: &OracleView, now: i64) -> Result<u64, Rej> {
    if amount == 0 {
        return Err(core("InvalidLpAmount"));
    }
    if st.m.is_paused {
        return Err(core("MarketPaused"));
    }
    if !in_session(&st.m, now) {
        return Err(core("MarketClosed"));
    }
    if let Some(r) = oracle_check(&st.m, o, now, MAX_PRICE_AGE_SECS) {
        return Err(r);
    }
    if in_lp_quiet(&st.m, &st.pool.params, now) {
        return Err(core("LpFlowWindowClosed"));
    }
    if !funding_current(&st.m, now) {
        return Err(core("FundingStale"));
    }
    if st.pool.pending_bad_debt_long > 0 || st.pool.pending_bad_debt_short > 0 {
        return Err(core("PoolAdlPending"));
    }
    let v = view_touch(st, o.price, now)?;
    let (minted, owner_shares) = if st.pool.lp_shares == 0 {
        if !(st.pool.cash == 0 && st.pool.open_positions == 0) {
            return Err(core("MarketNotFresh"));
        }
        if amount <= pm::LP_DEAD_SHARES {
            return Err(core("InvalidLpAmount"));
        }
        (amount, amount - pm::LP_DEAD_SHARES)
    } else {
        if v.nav_mint <= 0 {
            return Err(core("PoolInsolvent"));
        }
        let s = pm::shares_for_deposit(amount, st.pool.lp_shares, v.nav_mint).ok_or_else(overflow)?;
        (s, s)
    };
    if st.pool.max_pool_cash != 0 && v.nav_mint.max(0) as u128 + amount as u128 > st.pool.max_pool_cash as u128 {
        return Err(core("PoolCashCapExceeded"));
    }
    if owner_shares == 0 || owner_shares < min_shares_out {
        return Err(core("SlippageExceeded"));
    }
    st.pool.cash = add(st.pool.cash, amount)?;
    st.pool.lp_shares = add(st.pool.lp_shares, minted)?;
    lp.shares = add(lp.shares, owner_shares)?;
    lp.deposited_cum = lp.deposited_cum.saturating_add(amount);
    Ok(owner_shares)
}

/// lp_request_withdrawal: the requested value.
pub fn lp_request(st: &mut St, lp: &mut LpPosition, shares: u64, o: &OracleView, now: i64) -> Result<u64, Rej> {
    let free = lp.shares.saturating_sub(lp.pending_shares);
    if shares == 0 || shares > free {
        return Err(core("InsufficientShares"));
    }
    if lp.pending_shares != 0 {
        return Err(core("WithdrawalPending"));
    }
    let mark = if settled(&st.m) {
        st.m.settlement_price
    } else {
        if !in_session(&st.m, now) {
            return Err(core("MarketClosed"));
        }
        if let Some(r) = oracle_check(&st.m, o, now, MAX_PRICE_AGE_SECS) {
            return Err(r);
        }
        if !funding_current(&st.m, now) {
            return Err(core("FundingStale"));
        }
        o.price
    };
    let v = view_touch(st, mark, now)?;
    let value = pm::redeem_value(shares, st.pool.lp_shares, v.nav_redeem);
    lp.pending_shares = shares;
    lp.requested_value = value;
    lp.requested_at = now;
    lp.unlock_at = now + st.pool.params.withdrawal_delay_secs as i64;
    lp.claim_window_secs = st.pool.params.withdrawal_claim_window_secs;
    st.pool.pending_withdraw_shares = add(st.pool.pending_withdraw_shares, shares)?;
    Ok(value)
}

fn clear_request(lp: &mut LpPosition) {
    lp.pending_shares = 0;
    lp.requested_value = 0;
    lp.requested_at = 0;
    lp.unlock_at = 0;
    lp.claim_window_secs = 0;
}

#[allow(dead_code)] // the LP agents
pub fn lp_cancel(st: &mut St, lp: &mut LpPosition) -> Result<(), Rej> {
    if lp.pending_shares == 0 {
        return Err(core("NoWithdrawalPending"));
    }
    st.pool.pending_withdraw_shares = sub(st.pool.pending_withdraw_shares, lp.pending_shares)?;
    clear_request(lp);
    Ok(())
}

/// lp_withdraw: the value paid out.
pub fn lp_withdraw(st: &mut St, lp: &mut LpPosition, o: &OracleView, now: i64) -> Result<u64, Rej> {
    let is_settled = settled(&st.m);
    if lp.pending_shares == 0 {
        return Err(core("NoWithdrawalPending"));
    }
    if now < lp.unlock_at {
        return Err(core("WithdrawalDelayNotElapsed"));
    }
    let params = st.pool.params;
    if !is_settled && lp.claim_window_secs > 0 {
        let window = pm::effective_claim_window_secs(lp.claim_window_secs, params.weekend_cap_window_secs, params.lp_reopen_quiet_secs);
        let open = ses::open_seconds_between(st.m.session_schedule, lp.unlock_at, now);
        if open as u64 > window {
            return Err(core("WithdrawalExpired"));
        }
    }
    let mark = if is_settled {
        // D41: wall time max(profit-close quiet, Q) after settled_at
        let q = params.profit_close_quiet_secs.max(params.lp_reopen_quiet_secs) as i64;
        if st.m.settled_at > 0 && now - st.m.settled_at < q {
            return Err(core("LpFlowWindowClosed"));
        }
        st.m.settlement_price
    } else {
        if !in_session(&st.m, now) {
            return Err(core("MarketClosed"));
        }
        if let Some(r) = oracle_check(&st.m, o, now, MAX_PRICE_AGE_SECS) {
            return Err(r);
        }
        if in_pool_window(&st.m, &params, now) || in_lp_quiet(&st.m, &params, now) {
            return Err(core("LpFlowWindowClosed"));
        }
        if !funding_current(&st.m, now) {
            return Err(core("FundingStale"));
        }
        o.price
    };
    if st.pool.pending_bad_debt_long > 0 || st.pool.pending_bad_debt_short > 0 {
        return Err(core("PoolAdlPending"));
    }
    let v = view_touch(st, mark, now)?;
    if !is_settled && pm::dd_withdraw_blocked(dd_now(&st.pool, &v), params.dd_withdraw_block_bps) {
        return Err(core("PoolWithdrawalsBlocked"));
    }
    let pending = lp.pending_shares;
    let value = pm::redeem_value(pending, st.pool.lp_shares, v.nav_redeem).min(lp.requested_value);
    if value > st.pool.cash {
        return Err(core("PoolCashInsufficient"));
    }
    if !is_settled {
        let b_after = (v.nav_redeem - value as i128).max(0) as u128;
        if !pm::hard_net_ok(v.net_notional, params.hard_net_bps, b_after) {
            return Err(core("PoolCapWouldBreach"));
        }
    }
    st.pool.cash -= value;
    st.pool.lp_shares = sub(st.pool.lp_shares, pending)?;
    st.pool.pending_withdraw_shares = sub(st.pool.pending_withdraw_shares, pending)?;
    lp.shares = sub(lp.shares, pending)?;
    lp.withdrawn_cum = lp.withdrawn_cum.saturating_add(value);
    clear_request(lp);
    Ok(value)
}

// --- state comparison ------------------------------------------------------------------------

macro_rules! diff_fields {
    ($out:ident, $a:expr, $b:expr, $($f:ident),+ $(,)?) => {
        $(if $a.$f != $b.$f {
            $out.push(format!("{}: engine {:?}, model {:?}", stringify!($f), $a.$f, $b.$f));
        })+
    };
}

/// Fields where the engine's pool differs from the model's.
pub fn diff_pool(engine: &LpPool, model: &LpPool) -> Vec<String> {
    let mut out = Vec::new();
    diff_fields!(out, engine, model,
        free_collateral, position_collateral, cash, treasury, long_entry_sum, short_entry_sum, long_funding_sum, short_funding_sum,
        open_positions, lp_shares, pending_withdraw_shares, max_pool_cash, pending_bad_debt_long, pending_bad_debt_short,
        pending_since_long, pending_since_short, bad_debt_cum, bad_debt_recovered_cum, bad_debt_written_off_cum,
        loss_window_start, loss_window_nav, lp_absorbed_window, ins_paid_window, nps_peak, nps_peak_at,
        vest_amount, vest_start, vest_end, donated_cum, pending_params_at, params, pending_params, adl_keeper,
    );
    out
}

pub fn diff_market(engine: &Market, model: &Market) -> Vec<String> {
    let mut out = Vec::new();
    diff_fields!(out, engine, model,
        long_open_interest, short_open_interest, max_open_interest, cumulative_funding, last_funding_time, funding_interval,
        insurance_fund, total_positions, total_trades, is_paused, settlement_price, settled_at, funding_rate, funding_carry_secs,
        session_schedule, pre_close_window_secs,
    );
    out
}

pub fn diff_position(engine: &Position, model: &Position) -> Vec<String> {
    let mut out = Vec::new();
    diff_fields!(out, engine, model,
        owner, size, collateral, entry_price, leverage, realized_pnl, last_funding_payment, opened_at, last_updated_at, opened_price,
    );
    if engine.side != model.side {
        out.push(format!("side: engine {}, model {}", engine.side as u8, model.side as u8));
    }
    if engine.status != model.status {
        out.push(format!("status: engine {}, model {}", engine.status as u8, model.status as u8));
    }
    if engine.execution_source != model.execution_source {
        out.push(format!("execution_source: engine {}, model {}", engine.execution_source as u8, model.execution_source as u8));
    }
    out
}

pub fn diff_user(engine: &UserAccount, model: &UserAccount) -> Vec<String> {
    let mut out = Vec::new();
    diff_fields!(out, engine, model, collateral_balance, total_positions, total_trades, realized_pnl, house_entry_notional);
    out
}

pub fn diff_lp(engine: &LpPosition, model: &LpPosition) -> Vec<String> {
    let mut out = Vec::new();
    diff_fields!(out, engine, model, shares, pending_shares, requested_value, requested_at, unlock_at, deposited_cum, withdrawn_cum, claim_window_secs);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use perps_core::state::Market;

    fn oil() -> Market {
        Market { max_leverage: 20_000, initial_margin_ratio: 1_000, maintenance_margin_ratio: 500, taker_fee: 5, liquidation_fee: 100, max_open_interest: u64::MAX, ..Default::default() }
    }

    fn pool(params: PoolParams, cash: u64) -> LpPool {
        LpPool { cash, lp_shares: cash, params, ..Default::default() }
    }

    /// Admin transitions are predicted, not copied (third review): only the
    /// fields each instruction writes move.
    #[test]
    fn admin_transitions_touch_only_their_fields() {
        let m0 = Market { max_open_interest: 7_000_000, insurance_fund: 123, funding_interval: 3_600, long_open_interest: 5, ..oil() };
        let mut m = m0.clone();
        pause_market(&mut m);
        assert_eq!(diff_market(&m, &Market { is_paused: true, ..m0.clone() }), Vec::<String>::new());
        // max_open_interest 0 keeps the cap (§4.12, F19)
        let cfg = perps_core::instructions::MarketConfigParams {
            is_paused: false, session_schedule: 1, pre_close_window_secs: 7_200, weekend_initial_margin_ratio: 2_000,
            weekend_maintenance_margin_ratio: 1_000, max_confidence_bps: 100, min_oracle_price: 0, max_open_interest: 0,
        };
        update_market_config(&mut m, &cfg).unwrap();
        assert_eq!((m.is_paused, m.max_open_interest, m.session_schedule, m.insurance_fund), (false, 7_000_000, 1, 123));
        update_market_config(&mut m, &perps_core::instructions::MarketConfigParams { max_open_interest: 9, ..cfg }).unwrap();
        assert_eq!(m.max_open_interest, 9);
        // settle_market: the funding due (D42), then the three settlement fields
        let mut m = Market { last_funding_time: 1_000, ..m0.clone() };
        let ev = settle_market(&mut m, 60_000_000, 1_000 + 2 * 3_600 + 5).unwrap().unwrap();
        assert_eq!((ev.n, ev.carry), (2, 5));
        let mut want = Market { last_funding_time: 1_000, ..m0.clone() };
        apply_funding(&mut want, 60_000_000, 1_000 + 2 * 3_600 + 5).unwrap();
        assert_eq!(diff_market(&m, &Market { settlement_price: 60_000_000, settled_at: 1_000 + 2 * 3_600 + 5, is_paused: true, ..want }), Vec::<String>::new());
        assert_eq!(settle_market(&mut m, 1, 99_999), Err(core("MarketSettled")));
        assert_eq!(update_market_config(&mut m, &cfg), Err(core("MarketSettled")));
        // Pool params: only the proposal fields (D32)
        let p0 = pool(PoolParams::production(), 1_000);
        let mut p = p0.clone();
        let next = PoolParams { profit_close_quiet_secs: 60, ..PoolParams::production() };
        propose_pool_params(&mut p, next, 10);
        assert_eq!((p.pending_params, p.pending_params_at - 10, p.params), (next, PoolParams::production().timelock_secs(&next), p0.params));
        cancel_pool_params(&mut p).unwrap();
        assert_eq!(diff_pool(&p, &p0), Vec::<String>::new());
        assert_eq!(cancel_pool_params(&mut p), Err(core("NoPendingParams")));
        // No LP shares yet: applied at once
        let mut empty = LpPool { params: PoolParams::neutral(), ..Default::default() };
        propose_pool_params(&mut empty, next, 10);
        assert_eq!((empty.params, empty.pending_params_at), (next, 0));
    }

    /// The spec's worked example (venue-settlement 2.2): 100 contracts at $75.03 against a $75 oracle, 10x.
    #[test]
    fn spec_worked_example() {
        let m = oil();
        let size = 100 * 1_000_000;
        assert_eq!(venue_margin(&m, Side::Long, size, 75_030_000, 75_000_000, 10_000, 0), Some(753_300_000));
        assert_eq!(venue_margin(&m, Side::Short, size, 75_030_000, 75_000_000, 10_000, 0), Some(750_300_000));
    }

    #[test]
    fn matches_the_engine_helpers() {
        let m = oil();
        for &(size, price, oracle, lev) in &[(7_000_001u64, 74_970_001u64, 75_000_000u64, 3_000u32), (1_000, 80_000_000, 79_700_000, 10_000)] {
            for side in [Side::Long, Side::Short] {
                assert_eq!(venue_margin(&m, side, size, price, oracle, lev, 0), perps_core::state::venue_margin(&m, side, size, price, oracle, lev, 0));
            }
        }
        for &(price, oracle, bps) in &[(75_375_000u64, 75_000_000u64, 50u16), (75_375_001, 75_000_000, 50), (74_625_000, 75_000_000, 50), (1, 2, 500)] {
            assert_eq!(within_band(price, oracle, bps), perps_core::state::within_band(price, oracle, bps));
            for side in [Side::Long, Side::Short] {
                assert_eq!(within_adverse(side, price, oracle, bps), perps_core::state::within_adverse_bound(side, price, oracle, bps));
            }
        }
    }

    #[test]
    fn funding_matches_the_spec_formula() {
        assert_eq!(funding_rate(3_000_000, 1_000_000), 50);
        assert_eq!(funding_rate(1_000_000, 3_000_000), -50);
        assert_eq!(funding_rate(0, 0), 0);
        assert_eq!(funding_rate(1, 0), 100);
        assert_eq!(funding_step(50, 80_000_000), 4_000);
        assert_eq!(funding_step(-50, 80_000_000), -4_000);
    }

    /// Funding is charged for open seconds only: the weekend accrues nothing (§8).
    #[test]
    fn funding_catch_up_counts_open_seconds() {
        let friday_close = 1_794_607_200;
        let mut m = Market { session_schedule: 1, funding_interval: 3_600, last_funding_time: friday_close - 1_800, long_open_interest: 3_000_000, short_open_interest: 1_000_000, ..oil() };
        // Sunday 18:00 ET reopen + 30 min: half an hour before the close and half after the reopen
        let sunday_open = friday_close + 2 * 86_400 + 3_600;
        let ev = apply_funding(&mut m, 80_000_000, sunday_open + 1_800).unwrap();
        assert_eq!((ev.n, ev.dropped, ev.carry), (1, 0, 0));
        assert_eq!(m.cumulative_funding, 4_000);
        assert!(apply_funding(&mut m, 80_000_000, sunday_open + 1_801).is_none());
    }

    /// §2.3: a matched pair added to the aggregates leaves U unchanged, and
    /// closing both legs at one price moves cash by nothing but fees.
    #[test]
    fn matched_pair_is_zero_sum() {
        let params = PoolParams::neutral();
        let mut st = St { m: oil(), pool: pool(params, 1_000_000_000_000) };
        st.pool.free_collateral = 2_000_000_000;
        fill(&mut st, Side::Long, 10_000_000, 75_000_000, 100_000_000, 100_000_000, 75_000_000, 0).unwrap();
        let v = view(&st.pool, &st.m, 70_000_000, 0).unwrap();
        assert_eq!(v.u, 0);
        let mut long = Position { side: Side::Long, size: 10_000_000, entry_price: 75_000_000, opened_price: 75_000_000, collateral: 100_000_000, execution_source: ExecutionSource::Venue, ..Default::default() };
        let mut short = Position { side: Side::Short, ..long.clone() };
        let mut u = UserAccount::default();
        let cash0 = st.pool.cash;
        let a = settle_close(&mut st, &mut short, &mut u, 70_000_000, 70_000_000, 0).unwrap();
        let b = settle_close(&mut st, &mut long, &mut u, 70_000_000, 70_000_000, 0).unwrap();
        assert_eq!(a.p_eff, 50_000_000);
        assert_eq!(b.p_eff, -50_000_000);
        assert_eq!(st.pool.cash, cash0);
        assert_eq!((st.pool.open_positions, st.pool.long_entry_sum, st.pool.short_entry_sum), (0, 0, 0));
        assert_eq!(st.m.insurance_fund, 2 * 375_000);
    }

    /// The stage-1 slice leaves equity ≥ IM on the remainder (§6.3).
    #[test]
    fn stage1_slice_restores_initial_margin() {
        let (size, mark, im, pen) = (100_000_000u64, 75_000_000u64, 1_000u32, 200u16);
        // Equity 4% of $7,500 = $300
        let e = 300_000_000i128;
        let x = stage1_slice(size, mark, e, im, pen, 2_000, 100_000_000);
        let penalty = stage1_penalty(x, mark, pen, e) as i128;
        let rest = notional(size - x, mark);
        assert!(e - penalty >= rest * im as i128 / BPS);
        assert!(x < size);
    }

    /// D37: a mode-0 slice covers the pending and no more than a few units of it.
    #[test]
    fn deficit_slice_covers_the_pending() {
        let p = Position { side: Side::Short, size: 100_000_000, entry_price: 80_000_000, collateral: 1_000_000_000, ..Default::default() };
        // $1,000 of profit at $70; $100 pending
        let x = deficit_slice(&p, 100_000_000, 0, 1_000_000_000_000, 70_000_000, 0);
        assert!(slice_profit(&p, x, 70_000_000, 0) >= 100_000_000);
        assert!(x < p.size / 5);
    }
}
