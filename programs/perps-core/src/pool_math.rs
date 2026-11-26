//! Pure integer math for the LP pool (docs/lp-pool.md §2, §3, §7, §8, §9).
//!
//! No Anchor or Solana imports, like `session.rs` and `price.rs`:
//! `backend/keeper` (outside the workspace; its own lockfile) and `sim/`
//! `#[path]`-include this file. Keep it dependency-free and free of `crate::`
//! paths. `state/pool.rs` wraps these functions for the Anchor types.
//!
//! Conventions: prices, USDC and contract sizes are 6 dp; ratios are bps.
//! Signed division floors (`div_euclid`); `ceil_s` is its ceiling twin.

/// Upper bound on `Market.max_open_interest` (1e9 contracts). Bounds every
/// i128 product in the pool valuation.
pub const MAX_OPEN_INTEREST: u64 = 1_000_000_000_000_000;
pub const MAX_FUNDING_CATCHUP_PERIODS: u64 = 168;
/// Open seconds (§7.4).
pub const ADL_WRITE_OFF_SECS: i64 = 86_400;
pub const ADL_FLOOR_START_BPS: u64 = 1_000_000;
/// Open seconds (§7.2).
pub const ADL_FLOOR_DECAY_SECS: i64 = 21_600;
/// Wall clock (§2.6).
pub const LOSS_WINDOW_SECS: i64 = 86_400;
/// Wall clock (§2.2).
pub const DD_WINDOW_SECS: i64 = 7 * 86_400;
pub const MIN_POOL_TIMELOCK_SECS: i64 = 86_400;
pub const PARAMS_TIMELOCK_MARGIN_SECS: i64 = 5 * 86_400;
pub const LP_DEAD_SHARES: u64 = 1_000_000;
pub const LP_CLASS_COMMON: u8 = 0;

const SCALE: i128 = 1_000_000;
const BPS: u128 = 10_000;

/// `ceil(a / b)` for `b > 0`, on signed integers.
pub fn ceil_s(a: i128, b: i128) -> i128 {
    -((-a).div_euclid(b))
}

/// `ceil(a / b)` for `b > 0`, unsigned.
pub fn ceil_u(a: u128, b: u128) -> u128 {
    a / b + if a % b == 0 { 0 } else { 1 }
}

/// Full 128x128 -> 256-bit product as `(hi, lo)`.
fn mul_wide(a: u128, b: u128) -> (u128, u128) {
    const M: u128 = u64::MAX as u128;
    let (a1, a0) = (a >> 64, a & M);
    let (b1, b0) = (b >> 64, b & M);
    let p00 = a0 * b0;
    let p01 = a0 * b1;
    let p10 = a1 * b0;
    let p11 = a1 * b1;
    let mid = (p00 >> 64) + (p01 & M) + (p10 & M);
    let lo = (p00 & M) | (mid << 64);
    let hi = p11 + (p01 >> 64) + (p10 >> 64) + (mid >> 64);
    (hi, lo)
}

/// `floor(a·b / c)` exactly, with a 256-bit intermediate. `None` if `c == 0`
/// or the quotient does not fit in u128.
#[inline(never)]
pub fn mul_div_floor(a: u128, b: u128, c: u128) -> Option<u128> {
    if c == 0 {
        return None;
    }
    if let Some(p) = a.checked_mul(b) {
        return Some(p / c);
    }
    let (hi, lo) = mul_wide(a, b);
    if hi >= c {
        return None; // quotient >= 2^128
    }
    // Long division of (hi, lo) by c, one bit at a time; hi < c so the
    // quotient fits in 128 bits.
    let mut rem = hi;
    let mut q: u128 = 0;
    let mut i = 128;
    while i > 0 {
        i -= 1;
        let carry = rem >> 127;
        rem = (rem << 1) | ((lo >> i) & 1);
        if carry == 1 || rem >= c {
            rem = rem.wrapping_sub(c);
            q |= 1u128 << i;
        }
    }
    Some(q)
}

/// `a1·a2 <= b1·b2`, exact for any u128 inputs.
pub fn le_mul(a1: u128, a2: u128, b1: u128, b2: u128) -> bool {
    mul_wide(a1, a2) <= mul_wide(b1, b2)
}

// ---------------------------------------------------------------------------
// Valuation (§2.2)
// ---------------------------------------------------------------------------

/// The LpPool fields the valuation reads, as plain integers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PoolState {
    pub cash: u64,
    pub long_entry_sum: u128,
    pub short_entry_sum: u128,
    pub long_funding_sum: i128,
    pub short_funding_sum: i128,
    pub open_positions: u64,
    pub lp_shares: u64,
    pub pending_withdraw_shares: u64,
    pub nps_peak: u64,
    pub vest_amount: u64,
    pub vest_start: i64,
    pub vest_end: i64,
}

/// `pool_view` result (§2.2). 128 bytes; returned by value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PoolView {
    /// Traders' net unrealized PnL including funding (floored).
    pub u: i128,
    /// `L − S` in contracts; the pool holds `−net`.
    pub net: i128,
    /// `ceil(|net|·mark, 1e6)`, rounded against the opener.
    pub net_notional: u128,
    /// Upper bound of LP NAV; depositors pay it.
    pub nav_mint: i128,
    /// Lower bound of LP NAV; withdrawers get it.
    pub nav_redeem: i128,
    /// `max(NAV_redeem, 0)`: capital still in the pool, pending shares included.
    pub b: u128,
    /// `B` scaled by the non-pending share of `lp_shares`.
    pub cap_nav: u128,
    /// Drawdown of `nps` against `nps_peak`, bps.
    pub dd_bps: u64,
    /// Per-share mark NAV, scale 1e6 (saturates at u64::MAX).
    pub nps: u64,
}

/// Value swept or donated that has not vested into `NAV_redeem` yet (§10.3).
pub fn unvested(vest_amount: u64, vest_start: i64, vest_end: i64, now: i64) -> u64 {
    if vest_end <= now || vest_end <= vest_start {
        return 0;
    }
    let left = (vest_end - now) as u128;
    let total = (vest_end - vest_start) as u128;
    let v = vest_amount as u128 * left.min(total) / total;
    v as u64
}

/// `0` when there is no peak or `nps` is at/above it, else the drop in bps.
pub fn drawdown_bps(nps: u64, nps_peak: u64) -> u64 {
    if nps_peak == 0 || nps >= nps_peak {
        0
    } else {
        ((nps_peak - nps) as u128 * BPS / nps_peak as u128) as u64
    }
}

/// Per-share NAV (scale 1e6) of a non-negative `nav` over `lp_shares`.
pub fn nav_per_share(nav: i128, lp_shares: u64) -> u64 {
    if lp_shares == 0 || nav <= 0 {
        return 0;
    }
    mul_div_floor(nav as u128, SCALE as u128, lp_shares as u128)
        .map_or(u64::MAX, |v| u64::try_from(v).unwrap_or(u64::MAX))
}

/// The skew premium the pool pays back when the net skew unwinds (D39), at
/// `mark`: house opens and closes trade at the SIP-279 skew price, and the
/// premium paid to move the skew from 0 to `net` is returned in full by
/// closing back to 0 at the same oracle. Path independent, so an O(1) bound:
/// `min(ceil(mark·net²/(2·skew_scale·1e6)), ceil(net_notional·max_impact/1e4))`.
/// `0` with no price impact (`skew_scale == 0`, settled).
#[inline(never)]
pub fn unwind_premium(net: i128, mark: u64, skew_scale: u64, max_impact_bps: u16) -> Option<u128> {
    let n = net.unsigned_abs();
    if skew_scale == 0 || n == 0 {
        return Some(0);
    }
    let nn = ceil_u(n.checked_mul(mark as u128)?, SCALE as u128);
    let cap = ceil_u(nn.checked_mul(max_impact_bps as u128)?, BPS);
    let n2 = n.checked_mul(n)?;
    let den = 2 * skew_scale as u128 * SCALE as u128;
    let quad = match mul_div_floor(n2, mark as u128, den) {
        Some(q) if mul_wide(q, den) == mul_wide(n2, mark as u128) => q,
        Some(q) => q.saturating_add(1),
        None => u128::MAX,
    };
    Some(quad.min(cap))
}

/// O(1) pool valuation (§2.2). `None` on arithmetic overflow (`MathOverflow`).
/// `h_bps` is `redeem_loss_haircut_bps`; `skew_scale` and `max_impact_bps`
/// price the refundable skew premium (`unwind_premium`, D39), which
/// `NAV_redeem` and the mark NAV count as owed to traders. `NAV_mint` does
/// not, which errs against the depositor.
#[inline(never)]
#[allow(clippy::too_many_arguments)]
pub fn pool_view_raw(
    p: &PoolState,
    long_oi: u64,
    short_oi: u64,
    cumulative_funding: i64,
    mark: u64,
    now: i64,
    h_bps: u16,
    skew_scale: u64,
    max_impact_bps: u16,
) -> Option<PoolView> {
    let l = long_oi as i128;
    let s = short_oi as i128;
    let m = mark as i128;
    let f = cumulative_funding as i128;
    let les = i128::try_from(p.long_entry_sum).ok()?;
    let ses = i128::try_from(p.short_entry_sum).ok()?;

    let pnl_long = m.checked_mul(l)?.checked_sub(les)?;
    let pnl_short = m.checked_mul(s)?.checked_sub(ses)?;
    let pnl_num = pnl_long.checked_sub(pnl_short)?;
    let fund_long = f.checked_mul(l)?.checked_sub(p.long_funding_sum)?;
    let fund_short = f.checked_mul(s)?.checked_sub(p.short_funding_sum)?;
    let fund_num = fund_short.checked_sub(fund_long)?;
    let u = pnl_num.checked_add(fund_num)?.div_euclid(SCALE);

    let net = l - s;
    let slack = 2 * p.open_positions as i128 + 1;
    let unv = unvested(p.vest_amount, p.vest_start, p.vest_end, now) as i128;
    let cash = p.cash as i128;
    // What traders are owed on unwinding: U plus the skew premium they get
    // back (D39)
    let x = i128::try_from(unwind_premium(net, mark, skew_scale, max_impact_bps)?).ok()?;
    let u_lp = u.checked_add(x)?;
    let gain = u_lp.max(0);
    let loss = (-u_lp).max(0);

    let nav_mint = cash.checked_sub(u)?.checked_add(slack)?;
    let counted_loss = loss.checked_mul(h_bps as i128)? / BPS as i128;
    let nav_redeem = cash
        .checked_sub(unv)?
        .checked_sub(gain)?
        .checked_add(counted_loss)?
        .checked_sub(slack)?;
    let nav_mark = cash.checked_sub(unv)?.checked_sub(u_lp)?;

    let nps = nav_per_share(nav_mark, p.lp_shares);
    let dd_bps = drawdown_bps(nps, p.nps_peak);

    let net_notional = ceil_u(net.unsigned_abs().checked_mul(mark as u128)?, SCALE as u128);
    let b = nav_redeem.max(0) as u128;
    let cap_nav = if p.lp_shares == 0 {
        0
    } else {
        let live = p.lp_shares.saturating_sub(p.pending_withdraw_shares);
        mul_div_floor(b, live as u128, p.lp_shares as u128)?
    };

    Some(PoolView { u, net, net_notional, nav_mint, nav_redeem, b, cap_nav, dd_bps, nps })
}

/// §2.2 peak tracking: whether `touch_peak` replaces the peak.
pub fn peak_resets(nps: u64, nps_peak: u64, nps_peak_at: i64, now: i64) -> bool {
    nps >= nps_peak || now >= nps_peak_at.saturating_add(DD_WINDOW_SECS)
}

// ---------------------------------------------------------------------------
// Skew price (§2.4)
// ---------------------------------------------------------------------------

/// SIP-279 average premium. `delta` is `+q` for a buy (open long / close
/// short) and `−q` for a sell. Returns the raw execution price (the caller
/// requires `> 0` and fits u64); `None` on overflow. Buys round up, sells down.
#[inline(never)]
pub fn skew_price_raw(oracle: u64, skew_before: i128, delta: i128, skew_scale: u64, max_impact_bps: u16) -> Option<i128> {
    let o = oracle as i128;
    if skew_scale == 0 {
        return Some(o);
    }
    let num = o.checked_mul(skew_before.checked_mul(2)?.checked_add(delta)?)?;
    let den = 2 * skew_scale as i128;
    let prem = if delta > 0 { ceil_s(num, den) } else { num.div_euclid(den) };
    let cap = o * max_impact_bps as i128 / BPS as i128;
    Some(o + prem.clamp(-cap, cap))
}

// ---------------------------------------------------------------------------
// Fees and the deficit waterfall (§2.5, §2.6)
// ---------------------------------------------------------------------------

/// `(treasury, rest)` of a net fee `x`.
pub fn fee_split(x: u64, treasury_fee_bps: u16) -> (u64, u64) {
    let t = (x as u128 * treasury_fee_bps as u128 / BPS) as u64;
    (t, x - t)
}

/// `(to_insurance, to_cash)`: insurance first until the target, then LPs.
pub fn insurance_split(insurance: u64, target: u64, y: u64) -> (u64, u64) {
    let room = target.saturating_sub(insurance);
    let to_ins = y.min(room);
    (to_ins, y - to_ins)
}

/// Debt-weighted booking time (§2.6 step 4) after adding `add > 0` to `old`.
pub fn weighted_since(since: i64, old: u64, now: i64, add: u64) -> i64 {
    if old == 0 {
        return now;
    }
    let num = since as i128 * old as i128 + now as i128 * add as i128;
    let den = old as i128 + add as i128;
    num.div_euclid(den) as i64
}

/// LP share of a loss window still available: `floor(nav·bps/1e4) − used`.
pub fn lp_window_cap(loss_window_nav: u64, lp_loss_cap_window_bps: u16, used: u64) -> u64 {
    ((loss_window_nav as u128 * lp_loss_cap_window_bps as u128 / BPS) as u64).saturating_sub(used)
}

/// Insurance claim room in the window; `cap == 0` is unlimited.
pub fn insurance_window_cap(cap: u64, used: u64) -> u64 {
    if cap == 0 {
        u64::MAX
    } else {
        cap.saturating_sub(used)
    }
}

// ---------------------------------------------------------------------------
// Paying profit (§2.8)
// ---------------------------------------------------------------------------

/// A quote of one pool-funded profit payout. 32 bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Payout {
    /// Debited from cash.
    pub paid: u64,
    /// `P − P1`: cut because the pool is insolvent at mark.
    pub solvency_hc: u64,
    /// Repays the opposite side's pending bad debt; stays in cash.
    pub bad_debt_hc: u64,
    /// Liquidations only: the part cash could not pay.
    pub cash_hc: u64,
}

/// Pure quote. `u` is the pool view's U, `opp_pending` the opposite side's
/// `pending_bad_debt`. `None` means `PoolCashInsufficient` (only possible
/// when `allow_short == false`).
#[inline(never)]
pub fn quote_profit_raw(u: i128, cash: u64, opp_pending: u64, p: u64, allow_short: bool) -> Option<Payout> {
    let p1 = if u > 0 && u > cash as i128 {
        (p as u128 * cash as u128 / u as u128) as u64
    } else {
        p
    };
    let hc = opp_pending.min(p1);
    let p2 = p1 - hc;
    let (paid, cash_hc) = if p2 > cash {
        if !allow_short {
            return None;
        }
        (cash, p2 - cash)
    } else {
        (p2, 0)
    };
    Some(Payout { paid, solvency_hc: p - p1, bad_debt_hc: hc, cash_hc })
}

// ---------------------------------------------------------------------------
// Caps and triggers (§3, §7.2)
// ---------------------------------------------------------------------------

/// `G_bps`: the override, else `max(IM, weekend IM)`.
pub fn g_bps(weekend_gap_bps: u32, initial_margin_ratio: u32, weekend_initial_margin_ratio: u32) -> u64 {
    if weekend_gap_bps != 0 {
        weekend_gap_bps as u64
    } else {
        initial_margin_ratio.max(weekend_initial_margin_ratio) as u64
    }
}

/// Net cap: `net_notional·10_000 ≤ k_net_bps·capital`.
pub fn net_cap_ok(net_notional: u128, k_net_bps: u32, capital: u128) -> bool {
    le_mul(net_notional, BPS, k_net_bps as u128, capital)
}

/// Weekend net cap: `net_notional·G ≤ α·capital`; skipped when `G == 0`.
pub fn weekend_cap_ok(net_notional: u128, g_bps: u64, alpha_bps: u32, capital: u128) -> bool {
    g_bps == 0 || le_mul(net_notional, g_bps as u128, alpha_bps as u128, capital)
}

/// What a net-raising trade can take out of `B` through the skew premium
/// (D43): the rise in the unwind premium `NAV_redeem` counts as owed
/// (`unwind_premium`, D39) beyond the premium the trader paid,
/// `max(0, x(net_after) − x(net_before) − floor(size·|exec − oracle|/1e6))`.
/// Up to rounding it is 0 while the skew stays in the quadratic branch; a
/// trade that crosses into the `max_impact` clamp pays `size·o·max_impact`
/// but adds more to `x`, up to `o·skew_scale·max_impact²/2`. A deficit-mode
/// ADL closes at the mark (`exec == oracle`), so its whole `Δx` counts.
/// The weekend cap subtracts it from the capital it checks against, so a
/// capped trade keeps `G·net_notional ≤ α·B` after it. `None` on overflow.
#[inline(never)]
pub fn premium_shortfall(net_before: i128, net_after: i128, oracle: u64, exec: u64, size: u64, skew_scale: u64, max_impact_bps: u16) -> Option<u128> {
    let x0 = unwind_premium(net_before, oracle, skew_scale, max_impact_bps)?;
    let x1 = unwind_premium(net_after, oracle, skew_scale, max_impact_bps)?;
    let paid = (size as u128).checked_mul(exec.abs_diff(oracle) as u128)? / SCALE as u128;
    Some(x1.saturating_sub(x0).saturating_sub(paid))
}

/// Whether the weekend bound is on (D43): `weekend_cap_window_secs > 0`.
/// The weekend cap then binds every net-raising house open, close and
/// deficit-mode ADL at all times, not only inside `W`, and `lp_withdraw`
/// keeps the capital behind the net (`weekend_cap_ok` against `B'`).
pub fn weekend_bound_on(weekend_cap_window_secs: u32) -> bool {
    weekend_cap_window_secs > 0
}

/// `validate` with the weekend bound on (D43): `k_net·G ≤ α·10_000`, so the
/// net cap alone keeps `net_notional·G ≤ α·capital` at the `G` in force when
/// the params are applied (at most 0.5× with G 20% and α 10%). Production's
/// 0.4× sits below that edge, leaving LPs a `1 − k_net·G/α` = 20% exit
/// buffer with traders at the net cap (D45).
pub fn k_net_within_weekend_bound(k_net_bps: u32, g_bps: u64, alpha_bps: u32) -> bool {
    k_net_bps as u128 * g_bps as u128 <= alpha_bps as u128 * BPS
}

/// Per-account cap: `(house + opened)·1e8 ≤ per_account·k_net·capNAV`.
/// `per_account_cap_bps == 0` is off.
pub fn per_account_ok(house_entry_notional: u64, opened_notional: u64, per_account_cap_bps: u32, k_net_bps: u32, cap_nav: u128) -> bool {
    if per_account_cap_bps == 0 {
        return true;
    }
    let lhs = house_entry_notional as u128 + opened_notional as u128;
    le_mul(lhs, BPS * BPS, per_account_cap_bps as u128 * k_net_bps as u128, cap_nav)
}

/// Exposure-ADL net trigger (also the LP-exit hard line): `hard_net > 0 && nn·1e4 > hard·B`.
pub fn n_trig(net_notional: u128, hard_net_bps: u32, b: u128) -> bool {
    hard_net_bps > 0 && !le_mul(net_notional, BPS, hard_net_bps as u128, b)
}

/// Exposure-ADL drawdown trigger.
pub fn d_trig(dd_bps: u64, dd_adl_bps: u16) -> bool {
    dd_adl_bps > 0 && dd_bps >= dd_adl_bps as u64
}

/// Weekend force-reduce trigger (D20): inside `W`, `nn·G > α·B`.
pub fn w_trig(in_window: bool, net_notional: u128, g_bps: u64, alpha_bps: u32, b: u128) -> bool {
    in_window && g_bps > 0 && !le_mul(net_notional, g_bps as u128, alpha_bps as u128, b)
}

/// Drawdown freeze of net-raising house opens (`dd_freeze_bps == 0` is off).
pub fn dd_freeze(dd_bps: u64, dd_freeze_bps: u16) -> bool {
    dd_freeze_bps > 0 && dd_bps >= dd_freeze_bps as u64
}

/// LP withdraw drawdown block (`dd_withdraw_block_bps == 0` is off).
pub fn dd_withdraw_blocked(dd_bps: u64, dd_withdraw_block_bps: u16) -> bool {
    dd_withdraw_block_bps > 0 && dd_bps >= dd_withdraw_block_bps as u64
}

/// `lp_withdraw` post-check: `hard_net == 0 || net == 0 || nn·1e4 ≤ hard·B'`.
pub fn hard_net_ok(net_notional: u128, hard_net_bps: u32, b_after: u128) -> bool {
    hard_net_bps == 0 || le_mul(net_notional, BPS, hard_net_bps as u128, b_after)
}

/// `floor(bps·B / 10_000)`: the net-mode and drawdown-mode ADL targets.
pub fn bps_of(bps: u32, b: u128) -> u128 {
    mul_div_floor(b, bps as u128, BPS).unwrap_or(u128::MAX)
}

/// Weekend force-reduce target `floor(α·B / G)`; `G == 0` gives `u128::MAX`.
pub fn weekend_target(alpha_bps: u32, b: u128, g_bps: u64) -> u128 {
    if g_bps == 0 {
        return u128::MAX;
    }
    mul_div_floor(b, alpha_bps as u128, g_bps as u128).unwrap_or(u128::MAX)
}

/// Bit flags for the `Adl` event's `triggers`.
pub const TRIGGER_NET: u8 = 1;
pub const TRIGGER_DRAWDOWN: u8 = 2;
pub const TRIGGER_WEEKEND: u8 = 4;

// ---------------------------------------------------------------------------
// ADL (§7.2)
// ---------------------------------------------------------------------------

/// Hyperliquid's `(m/entry)·(notional/equity)` in bps (u128). Long:
/// `m·N·1e4 / (entry·E)`; short: `entry·N·1e4 / (m·E)`. Exact (nested floors
/// equal the single floor); saturates at u128::MAX; 0 when `E ≤ 0`.
#[inline(never)]
pub fn adl_score(long: bool, mark: u64, entry: u64, n_mark: u128, equity: i128) -> u128 {
    if equity <= 0 {
        return 0;
    }
    let (num_px, den_px) = if long { (mark, entry) } else { (entry, mark) };
    if den_px == 0 {
        return u128::MAX;
    }
    let a = match n_mark.checked_mul(BPS) {
        Some(a) => a,
        None => return u128::MAX,
    };
    match mul_div_floor(a, num_px as u128, den_px as u128) {
        Some(q) => q / equity as u128,
        None => u128::MAX,
    }
}

/// Mode-0 floor after `dt` open (or, settled, wall-clock) seconds since the
/// debt-weighted `pending_since`: decays linearly from `ADL_FLOOR_START_BPS`
/// to 0 over `ADL_FLOOR_DECAY_SECS`.
pub fn adl_floor_now(dt: i64) -> u64 {
    let dt = dt.clamp(0, ADL_FLOOR_DECAY_SECS);
    (ADL_FLOOR_START_BPS as u128 * (ADL_FLOOR_DECAY_SECS - dt) as u128 / ADL_FLOOR_DECAY_SECS as u128) as u64
}

// ---------------------------------------------------------------------------
// Funding catch-up (§8 step 5)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FundingCatchup {
    /// Periods charged now (≤ 168).
    pub n: u64,
    /// Whole periods beyond the cap, not charged.
    pub dropped: u64,
    /// Sub-period open seconds carried to the next crank.
    pub carry: u32,
}

/// `total` = open seconds since the last crank + carry. `None` when not due
/// (`FundingNotDue`) or `interval <= 0`.
pub fn funding_catchup(total: i64, interval: i64) -> Option<FundingCatchup> {
    if interval <= 0 || total < interval {
        return None;
    }
    let due = (total / interval) as u64;
    let n = due.min(MAX_FUNDING_CATCHUP_PERIODS);
    let dropped = due - n;
    let carry = if due > MAX_FUNDING_CATCHUP_PERIODS { 0 } else { total - n as i64 * interval };
    Some(FundingCatchup { n, dropped, carry: u32::try_from(carry).unwrap_or(u32::MAX) })
}

/// `clamp(cumulative + n·step)` to i64 (§8 step 7).
pub fn funding_accumulate(cumulative: i64, n: u64, step: i64) -> i64 {
    let v = cumulative as i128 + n as i128 * step as i128;
    v.clamp(i64::MIN as i128, i64::MAX as i128) as i64
}

// ---------------------------------------------------------------------------
// LP share math (§9)
// ---------------------------------------------------------------------------

/// `floor(amount·lp_shares / nav_mint)` for a non-first deposit; `None` when
/// `nav_mint ≤ 0` (`PoolInsolvent`) or the result overflows u64.
pub fn shares_for_deposit(amount: u64, lp_shares: u64, nav_mint: i128) -> Option<u64> {
    if nav_mint <= 0 {
        return None;
    }
    let s = amount as u128 * lp_shares as u128 / nav_mint as u128;
    u64::try_from(s).ok()
}

/// `floor(shares·max(nav_redeem, 0) / lp_shares)`, saturating at u64::MAX.
pub fn redeem_value(shares: u64, lp_shares: u64, nav_redeem: i128) -> u64 {
    if lp_shares == 0 || nav_redeem <= 0 {
        return 0;
    }
    mul_div_floor(nav_redeem as u128, shares as u128, lp_shares as u128)
        .map_or(u64::MAX, |v| u64::try_from(v).unwrap_or(u64::MAX))
}

/// Params timelock (§5): `max(MIN, max(delays) + max(windows) + MARGIN)`.
pub fn params_timelock_secs(cur_delay: u32, new_delay: u32, cur_window: u32, new_window: u32) -> i64 {
    let t = cur_delay.max(new_delay) as i64 + cur_window.max(new_window) as i64 + PARAMS_TIMELOCK_MARGIN_SECS;
    t.max(MIN_POOL_TIMELOCK_SECS)
}

/// Open seconds in one CME week (Sun 6 h + Mon–Thu 4 × 23 h + Fri 17 h),
/// the most `SCHEDULE_ALWAYS_OPEN` and `SCHEDULE_CME_GLOBEX` are held to
/// between two weekly closes (`week_open_secs`).
pub const WEEK_OPEN_SECS: u64 = 414_000;
/// One day of open time that every week, and every claim window, must leave
/// ungated for `lp_withdraw` (§1.2, D38), on those two schedules.
pub const LP_UNGATED_SECS: u64 = 86_400;
/// Session opens per CME week the reopen quiet `Q` gates (Sunday and
/// Monday–Thursday 18:00 ET).
pub const SESSION_OPENS_PER_WEEK: u64 = 5;

/// `session::SCHEDULE_TEST_HOURLY` (localnet only, D46; this file takes no
/// `crate::` paths). Pinned to the session constant by a perps-core test.
const SCHEDULE_TEST_HOURLY: u8 = 2;
/// Open seconds per `SCHEDULE_TEST_HOURLY` week (`session::TEST_WEEK_OPEN`).
pub const TEST_WEEK_OPEN_SECS: u64 = 2_820;
/// The test week's ungated LP exit time: the CME ratio, 86_400 / 414_000 of
/// the week's open time (589 s), rounded to 600 s.
pub const TEST_LP_UNGATED_SECS: u64 = 600;
/// Session opens per test week: the week start and the end of each of the
/// three daily breaks.
pub const TEST_SESSION_OPENS_PER_WEEK: u64 = 4;

/// Open seconds per week of `schedule` (D38, D46): the test schedule's
/// compressed week, else the CME week for every schedule, as before.
pub fn week_open_secs(schedule: u8) -> u64 {
    if schedule == SCHEDULE_TEST_HOURLY { TEST_WEEK_OPEN_SECS } else { WEEK_OPEN_SECS }
}

/// Open time a week of `schedule` must leave ungated for `lp_withdraw` (D38, D46).
pub fn lp_ungated_secs(schedule: u8) -> u64 {
    if schedule == SCHEDULE_TEST_HOURLY { TEST_LP_UNGATED_SECS } else { LP_UNGATED_SECS }
}

/// Session opens per week of `schedule` that the reopen quiet gates (D38, D46).
pub fn session_opens_per_week(schedule: u8) -> u64 {
    if schedule == SCHEDULE_TEST_HOURLY { TEST_SESSION_OPENS_PER_WEEK } else { SESSION_OPENS_PER_WEEK }
}

/// Upper bound on the open seconds per week the LP gates close
/// `lp_withdraw` (D38): the pre-close window `W` (wall seconds, so at least
/// its open seconds) plus the reopen quiet `Q` after each of the week's
/// session opens (five on CME: Sunday and Monday–Thursday 18:00 ET).
pub fn lp_gated_secs_per_week(schedule: u8, window: u32, quiet: u32) -> u64 {
    window as u64 + session_opens_per_week(schedule) * quiet as u64
}

/// `validate`'s D38 rule: `W + opens·Q + ungated ≤ week open time`, so every
/// week keeps its ungated LP exit time (`W + 5·Q + 86_400 ≤ 414_000` on CME).
pub fn lp_gates_leave_exit_time(schedule: u8, window: u32, quiet: u32) -> bool {
    lp_gated_secs_per_week(schedule, window, quiet) + lp_ungated_secs(schedule) <= week_open_secs(schedule)
}

/// Shortest claim window (open seconds) that always holds the ungated
/// `lp_withdraw` time: `W + 5·Q + 1 day` on CME (D38).
pub fn min_claim_window_secs(schedule: u8, window: u32, quiet: u32) -> u64 {
    lp_gated_secs_per_week(schedule, window, quiet) + lp_ungated_secs(schedule)
}

/// The claim window a request is held to: the one stored at request time,
/// but never shorter than what the gates in force now need, so a later
/// `apply_pool_params` that widens `W` or `Q` cannot expire it inside gated
/// time (D38). `0` (never lapses) stays 0.
pub fn effective_claim_window_secs(schedule: u8, stored: u32, window: u32, quiet: u32) -> u64 {
    if stored == 0 {
        0
    } else {
        (stored as u64).max(min_claim_window_secs(schedule, window, quiet))
    }
}

/// A settled market's LP exit quiet time (D41): `lp_withdraw` waits
/// `max(profit_close_quiet_secs, lp_reopen_quiet_secs)` of wall time after
/// `settled_at`, so the losers the settlement price bankrupted are closed
/// and their deficits booked before an LP exits at a NAV that still counts
/// their losses. `settled_at == 0` (settled before the field existed): none.
pub fn settled_withdraw_quiet(settled_at: i64, now: i64, profit_close_quiet_secs: u32, lp_reopen_quiet_secs: u32) -> bool {
    let q = profit_close_quiet_secs.max(lp_reopen_quiet_secs) as i64;
    settled_at > 0 && now.saturating_sub(settled_at) < q
}

/// `floor(size·price / 1e6)`, saturating at u64::MAX (only reachable far
/// beyond `MAX_OPEN_INTEREST` at realistic prices).
pub fn notional_floor(size: u64, price: u64) -> u64 {
    let v = size as u128 * price as u128 / SCALE as u128;
    u64::try_from(v).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {

    #[test]
    fn settled_withdraw_quiet_is_the_longer_quiet_time_from_settlement() {
        // max(300, 1_800) = 1_800 s of wall time after settled_at
        assert!(settled_withdraw_quiet(1_000, 1_000, 300, 1_800));
        assert!(settled_withdraw_quiet(1_000, 2_799, 300, 1_800));
        assert!(!settled_withdraw_quiet(1_000, 2_800, 300, 1_800));
        assert!(settled_withdraw_quiet(1_000, 1_299, 300, 0));
        assert!(!settled_withdraw_quiet(1_000, 1_300, 300, 0));
        // Neutral params, and markets settled before settled_at existed
        assert!(!settled_withdraw_quiet(1_000, 1_000, 0, 0));
        assert!(!settled_withdraw_quiet(0, 1, 300, 1_800));
    }

    use super::*;

    /// xorshift64*: deterministic randomness without a dev-dependency.
    pub(crate) struct Rng(pub u64);
    impl Rng {
        pub fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.0 = x;
            x.wrapping_mul(0x2545_f491_4f6c_dd1d)
        }
        pub fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    #[test]
    fn rounding_helpers() {
        for a in -50i128..=50 {
            for b in 1i128..=9 {
                let exact = a as f64 / b as f64;
                assert_eq!(ceil_s(a, b), exact.ceil() as i128, "{} {}", a, b);
                assert_eq!(a.div_euclid(b), exact.floor() as i128, "{} {}", a, b);
                if a >= 0 {
                    assert_eq!(ceil_u(a as u128, b as u128), exact.ceil() as u128);
                }
            }
        }
    }

    #[test]
    fn mul_div_is_exact_beyond_u128() {
        let mut r = Rng(7);
        for _ in 0..2_000 {
            // Small cases checked against plain u128 math
            let a = r.next() as u128;
            let b = r.next() as u128;
            let c = (r.next() as u128).max(1);
            assert_eq!(mul_div_floor(a, b, c), Some(a * b / c));
        }
        let big = u128::MAX;
        assert_eq!(mul_div_floor(big, big, big), Some(big));
        assert_eq!(mul_div_floor(big, 3, 3), Some(big));
        assert_eq!(mul_div_floor(big, 2, 1), None);
        assert_eq!(mul_div_floor(1, 1, 0), None);
        assert_eq!(mul_div_floor(1u128 << 100, 1u128 << 100, 1u128 << 90), Some(1u128 << 110));
        assert_eq!(mul_div_floor((1u128 << 100) + 1, 1u128 << 100, 1u128 << 101), Some(1u128 << 99));
        // (2^127+1)(2^127+3) / (2^127+5): compare with identity q·c + r
        let (a, b, c) = ((1u128 << 127) + 1, (1u128 << 127) + 3, (1u128 << 127) + 5);
        let q = mul_div_floor(a, b, c).unwrap();
        let (hi, lo) = mul_wide(a, b);
        let (qh, ql) = mul_wide(q, c);
        assert!((qh, ql) <= (hi, lo));
        let (qh2, ql2) = mul_wide(q + 1, c);
        assert!((qh2, ql2) > (hi, lo));
    }

    #[test]
    fn le_mul_is_exact() {
        assert!(le_mul(u128::MAX, u128::MAX, u128::MAX, u128::MAX));
        assert!(!le_mul(u128::MAX, u128::MAX, u128::MAX, u128::MAX - 1));
        assert!(le_mul(3, 4, 2, 6));
        assert!(!le_mul(3, 5, 2, 7));
    }

    #[test]
    fn unvested_is_linear_and_bounded() {
        assert_eq!(unvested(1_000, 100, 200, 100), 1_000);
        assert_eq!(unvested(1_000, 100, 200, 150), 500);
        assert_eq!(unvested(1_000, 100, 200, 151), 490);
        assert_eq!(unvested(1_000, 100, 200, 199), 10);
        assert_eq!(unvested(1_000, 100, 200, 200), 0);
        assert_eq!(unvested(1_000, 100, 200, 50), 1_000); // never above the amount
        assert_eq!(unvested(1_000, 0, 0, 0), 0);
        // Floors: 7 over 3 seconds
        assert_eq!(unvested(7, 0, 3, 1), 4);
    }

    #[test]
    fn drawdown_lines() {
        assert_eq!(drawdown_bps(0, 0), 0);
        assert_eq!(drawdown_bps(1_100_000, 1_000_000), 0);
        assert_eq!(drawdown_bps(900_000, 1_000_000), 1_000);
        assert_eq!(drawdown_bps(899_999, 1_000_000), 1_000); // floors
        assert_eq!(drawdown_bps(0, 1_000_000), 10_000);
        assert!(peak_resets(5, 5, 0, 1));
        assert!(!peak_resets(4, 5, 0, DD_WINDOW_SECS - 1));
        assert!(peak_resets(4, 5, 0, DD_WINDOW_SECS));
        assert!(!dd_freeze(999, 1_000) && dd_freeze(1_000, 1_000) && !dd_freeze(10_000, 0));
        assert!(!d_trig(1_499, 1_500) && d_trig(1_500, 1_500) && !d_trig(10_000, 0));
        assert!(!dd_withdraw_blocked(10_000, 0) && dd_withdraw_blocked(5, 5));
    }

    fn state(cash: u64) -> PoolState {
        PoolState { cash, lp_shares: 1_000_000_000_000, ..Default::default() }
    }

    #[test]
    fn empty_pool_view() {
        let v = pool_view_raw(&state(1_000_000_000_000), 0, 0, 0, 75_000_000, 0, 5_000, 0, 0).unwrap();
        assert_eq!(v.u, 0);
        assert_eq!(v.nav_mint, 1_000_000_000_001);
        assert_eq!(v.nav_redeem, 999_999_999_999);
        assert_eq!(v.b, 999_999_999_999);
        assert_eq!(v.cap_nav, 999_999_999_999);
        assert_eq!(v.nps, 1_000_000);
        assert_eq!((v.net, v.net_notional, v.dd_bps), (0, 0, 0));
    }

    #[test]
    fn view_of_a_single_long() {
        // 100 contracts long at $75, mark $76, funding index moved +7_500 per
        // contract since entry (longs pay $0.75), one open position.
        let mut p = state(1_000_000_000);
        p.long_entry_sum = 100_000_000u128 * 75_000_000;
        p.long_funding_sum = 100_000_000i128 * 10;
        p.open_positions = 1;
        p.pending_withdraw_shares = p.lp_shares / 4;
        let v = pool_view_raw(&p, 100_000_000, 0, 7_510, 76_000_000, 0, 5_000, 0, 0).unwrap();
        // pnl +$100, funding −$0.75 → U = 99.25
        assert_eq!(v.u, 99_250_000);
        assert_eq!(v.nav_mint, 1_000_000_000 - 99_250_000 + 3);
        assert_eq!(v.nav_redeem, 1_000_000_000 - 99_250_000 - 3);
        assert_eq!(v.net, 100_000_000);
        assert_eq!(v.net_notional, 7_600_000_000);
        assert_eq!(v.cap_nav, v.b * 3 / 4);
        // Short side: same numbers, opposite sign of U, h counts half the loss
        let mut q = state(1_000_000_000);
        q.short_entry_sum = p.long_entry_sum;
        q.short_funding_sum = p.long_funding_sum;
        q.open_positions = 1;
        let w = pool_view_raw(&q, 0, 100_000_000, 7_510, 76_000_000, 0, 5_000, 0, 0).unwrap();
        assert_eq!(w.u, -99_250_000);
        assert_eq!(w.nav_redeem, 1_000_000_000 + 49_625_000 - 3);
        assert_eq!(w.net, -100_000_000);
        assert_eq!(w.net_notional, 7_600_000_000);
    }

    #[test]
    fn net_notional_rounds_against_the_opener() {
        let v = pool_view_raw(&state(0), 1, 0, 0, 1, 0, 0, 0, 0);
        // pool_view needs positions in the aggregates to be meaningful; only
        // the net term matters here: |1|·1 / 1e6 rounds up to 1.
        assert_eq!(v.unwrap().net_notional, 1);
    }

    /// Mutation finding: U floors (`div_euclid`), so a sub-unit trader loss
    /// counts as a whole unit (truncation would count 0).
    #[test]
    fn u_floors_a_fractional_trader_loss() {
        let p = PoolState { cash: 1_000, long_entry_sum: 2, open_positions: 1, lp_shares: 1_000, ..Default::default() };
        // One unit long, entry sum 2, at mark 1: PnL −1e-6 units
        let v = pool_view_raw(&p, 1, 0, 0, 1, 0, 0, 0, 0).unwrap();
        assert_eq!((v.u, v.nav_mint), (-1, 1_000 + 1 + 3));
        // hard_net_ok with no net always holds
        assert!(hard_net_ok(0, 1, 0));
        assert!(!hard_net_ok(1, 1, 0));
    }

    #[test]
    fn net_notional_is_a_ceiling_and_cap_nav_a_floor() {
        let mut r = Rng(17);
        for _ in 0..20_000 {
            let l = r.below(MAX_OPEN_INTEREST);
            let s = r.below(MAX_OPEN_INTEREST);
            let mark = 1 + r.below(1_000_000_000);
            let mut p = state(r.below(u64::MAX / 2));
            p.long_entry_sum = l as u128 * mark as u128;
            p.short_entry_sum = s as u128 * mark as u128;
            p.lp_shares = 1_000_000 + r.below(u64::MAX / 2);
            p.pending_withdraw_shares = r.below(p.lp_shares + 1);
            let v = pool_view_raw(&p, l, s, 0, mark, 0, 0, 0, 0).unwrap();
            assert_eq!(v.u, 0);
            let exact = (l as i128 - s as i128).unsigned_abs() * mark as u128;
            assert!(v.net_notional * 1_000_000 >= exact);
            assert!(v.net_notional == 0 || (v.net_notional - 1) * 1_000_000 < exact);
            let live = (p.lp_shares - p.pending_withdraw_shares) as u128;
            assert!(v.cap_nav * p.lp_shares as u128 <= v.b * live);
            assert!((v.cap_nav + 1) * p.lp_shares as u128 > v.b * live);
        }
    }

    #[test]
    fn view_overflow_is_reported() {
        let mut p = state(0);
        p.long_entry_sum = u128::MAX;
        assert_eq!(pool_view_raw(&p, 1, 0, 0, 1, 0, 0, 0, 0), None);
        // The bounds in §2.2 never overflow
        let mut p = state(u64::MAX);
        p.long_entry_sum = MAX_OPEN_INTEREST as u128 * u64::MAX as u128;
        p.short_funding_sum = i64::MIN as i128 * MAX_OPEN_INTEREST as i128;
        p.open_positions = u64::MAX / 4;
        assert!(pool_view_raw(&p, MAX_OPEN_INTEREST, MAX_OPEN_INTEREST, i64::MAX, u64::MAX, 0, 10_000, 0, 0).is_some());
    }

    #[test]
    fn vesting_widens_mint_minus_redeem_by_exactly_the_unvested_part() {
        let mut p = state(5_000_000_000);
        let base = pool_view_raw(&p, 0, 0, 0, 1, 50, 0, 0, 0).unwrap();
        p.vest_amount = 1_000_000;
        p.vest_start = 0;
        p.vest_end = 100;
        let v = pool_view_raw(&p, 0, 0, 0, 1, 50, 0, 0, 0).unwrap();
        assert_eq!(v.nav_mint, base.nav_mint);
        assert_eq!(v.nav_redeem, base.nav_redeem - 500_000);
        assert_eq!((v.nav_mint - v.nav_redeem) - (base.nav_mint - base.nav_redeem), 500_000);
    }

    #[test]
    fn skew_price_scale_zero_is_the_oracle() {
        assert_eq!(skew_price_raw(75_000_000, 123, 456, 0, 100), Some(75_000_000));
    }

    #[test]
    fn skew_price_rounds_buys_up_and_sells_down() {
        // oracle·(2·0 + 1) / (2·3) = 1/6 of the oracle price unit
        let o = 7u64;
        let buy = skew_price_raw(o, 0, 1, 3, 1_000).unwrap();
        let sell = skew_price_raw(o, 0, -1, 3, 1_000).unwrap();
        // num = 7, den = 6 → buy ceil(7/6) = 2 → capped at floor(7·1000/1e4)=0
        assert_eq!((buy, sell), (7, 7));
        let o = 75_000_000u64;
        // premium = 75e6·(0+1)/(2·7) = 5_357_142.857
        assert_eq!(skew_price_raw(o, 0, 1, 7, 10_000).unwrap(), 75_000_000 + 5_357_143);
        assert_eq!(skew_price_raw(o, 0, -1, 7, 10_000).unwrap(), 75_000_000 - 5_357_143);
        // Exhaustive small grid: buys ≥ exact, sells ≤ exact, within 1 unit
        for skew in -20i128..=20 {
            for d in 1i128..=15 {
                for scale in [1u64, 3, 7, 50] {
                    let o = 1_000_003i128;
                    let exact = (o * (2 * skew + d)) as f64 / (2 * scale) as f64;
                    let b = skew_price_raw(o as u64, skew, d, scale, 10_000).unwrap() - o;
                    let s = skew_price_raw(o as u64, skew, -d, scale, 10_000).unwrap() - o;
                    let exact_s = (o * (2 * skew - d)) as f64 / (2 * scale) as f64;
                    let cap = (o * 10_000 / 10_000) as f64;
                    if exact.abs() < cap {
                        assert!(b as f64 >= exact && (b as f64) < exact + 1.0, "{} {} {}", skew, d, scale);
                    }
                    if exact_s.abs() < cap {
                        assert!(s as f64 <= exact_s && (s as f64) > exact_s - 1.0, "{} {} {}", skew, d, scale);
                    }
                }
            }
        }
    }

    #[test]
    fn skew_price_is_clamped() {
        let o = 75_000_000u64;
        // A huge skew is clamped to ±1% (100 bps)
        assert_eq!(skew_price_raw(o, 1_000_000_000_000, 1, 1, 100), Some(75_750_000));
        assert_eq!(skew_price_raw(o, -1_000_000_000_000, -1, 1, 100), Some(74_250_000));
        // max_impact 0 means no impact even with a scale
        assert_eq!(skew_price_raw(o, 5_000_000, 5_000_000, 1_000_000, 0), Some(o as i128));
    }

    #[test]
    fn reducing_skew_gets_a_better_price() {
        let o = 75_000_000u64;
        let scale = 2_500_000_000_000;
        // Skew +10k contracts: a sell (reduces) gets above the oracle, a buy pays more
        let skew = 10_000_000_000i128;
        let sell = skew_price_raw(o, skew, -1_000_000_000, scale, 100).unwrap();
        let buy = skew_price_raw(o, skew, 1_000_000_000, scale, 100).unwrap();
        assert!(sell > o as i128);
        assert!(buy > sell);
        // And from a negative skew a buy is below the oracle
        assert!(skew_price_raw(o, -skew, 1_000_000_000, scale, 100).unwrap() < o as i128);
    }

    #[test]
    fn open_then_close_in_the_same_skew_state_nets_to_zero() {
        let mut r = Rng(11);
        for _ in 0..5_000 {
            let o = 1 + r.below(200_000_000);
            let scale = 1 + r.below(5_000_000_000_000);
            let skew = r.below(20_000_000_000) as i128 - 10_000_000_000;
            let q = 1 + r.below(5_000_000_000) as i128;
            for buy in [true, false] {
                let d = if buy { q } else { -q };
                let open = skew_price_raw(o, skew, d, scale, 1_000).unwrap();
                let close = skew_price_raw(o, skew + d, -d, scale, 1_000).unwrap();
                // Same premium both ways, rounded against the trader by at most 1 unit
                let cost = if buy { open - close } else { close - open };
                assert!((0..=1).contains(&cost), "{} {} {} {} {}", o, scale, skew, q, cost);
            }
        }
    }

    #[test]
    fn fee_and_insurance_splits() {
        assert_eq!(fee_split(1_000, 1_000), (100, 900));
        assert_eq!(fee_split(999, 1_000), (99, 900)); // treasury floors
        assert_eq!(fee_split(5, 0), (0, 5));
        assert_eq!(fee_split(5, 10_000), (5, 0));
        // below, at and above the target
        assert_eq!(insurance_split(10, 100, 50), (50, 0));
        assert_eq!(insurance_split(10, 100, 90), (90, 0));
        assert_eq!(insurance_split(10, 100, 95), (90, 5));
        assert_eq!(insurance_split(100, 100, 7), (0, 7));
        assert_eq!(insurance_split(150, 100, 7), (0, 7));
        assert_eq!(insurance_split(0, u64::MAX, u64::MAX), (u64::MAX, 0));
    }

    #[test]
    fn debt_weighted_booking_time() {
        assert_eq!(weighted_since(0, 0, 1_000, 5), 1_000);
        // A large new deficit moves the clock almost to now
        assert_eq!(weighted_since(0, 1, 86_400, 1_000_000), 86_399);
        // Dust barely moves it
        assert_eq!(weighted_since(0, 1_000_000, 86_400, 1), 0);
        // Exact weighted mean, floored
        assert_eq!(weighted_since(100, 3, 200, 1), 125);
        assert_eq!(weighted_since(100, 2, 201, 1), 133);
    }

    #[test]
    fn window_caps() {
        assert_eq!(lp_window_cap(1_000_000, 500, 0), 50_000);
        assert_eq!(lp_window_cap(1_000_000, 500, 49_999), 1);
        assert_eq!(lp_window_cap(1_000_000, 500, 60_000), 0);
        assert_eq!(insurance_window_cap(0, 123), u64::MAX);
        assert_eq!(insurance_window_cap(100, 30), 70);
        assert_eq!(insurance_window_cap(100, 130), 0);
    }

    #[test]
    fn quote_profit_haircuts() {
        // Solvent: paid in full
        assert_eq!(quote_profit_raw(50, 100, 0, 40, false), Some(Payout { paid: 40, ..Default::default() }));
        // U == cash is not insolvent
        assert_eq!(quote_profit_raw(100, 100, 0, 40, false).unwrap().solvency_hc, 0);
        // U > cash: pro rata floor(P·cash/U)
        let q = quote_profit_raw(300, 100, 0, 40, false).unwrap();
        assert_eq!(q, Payout { paid: 13, solvency_hc: 27, bad_debt_hc: 0, cash_hc: 0 });
        // Negative U never haircuts
        assert_eq!(quote_profit_raw(-1_000, 100, 0, 40, false).unwrap().paid, 40);
        // Bad-debt haircut: min(opp, P1)
        assert_eq!(quote_profit_raw(0, 100, 15, 40, false), Some(Payout { paid: 25, bad_debt_hc: 15, ..Default::default() }));
        assert_eq!(quote_profit_raw(0, 100, 75, 40, false), Some(Payout { paid: 0, bad_debt_hc: 40, ..Default::default() }));
        // Cash short: close fails, liquidation pays cash and reports the rest
        assert_eq!(quote_profit_raw(0, 30, 0, 40, false), None);
        assert_eq!(quote_profit_raw(0, 30, 0, 40, true), Some(Payout { paid: 30, cash_hc: 10, ..Default::default() }));
        // The bad-debt haircut can make a cash-short payout affordable
        assert_eq!(quote_profit_raw(0, 30, 10, 40, false), Some(Payout { paid: 30, bad_debt_hc: 10, ..Default::default() }));
    }

    #[test]
    fn quote_profit_parts_always_sum_to_p() {
        let mut r = Rng(5);
        for _ in 0..20_000 {
            let u = r.below(2_000) as i128 - 1_000;
            let cash = r.below(1_000);
            let opp = r.below(300);
            let p = r.below(1_000);
            for allow in [false, true] {
                if let Some(q) = quote_profit_raw(u, cash, opp, p, allow) {
                    assert_eq!(q.paid + q.solvency_hc + q.bad_debt_hc + q.cash_hc, p);
                    assert!(q.paid <= cash);
                    assert!(q.bad_debt_hc <= opp);
                    if !allow {
                        assert_eq!(q.cash_hc, 0);
                    }
                } else {
                    assert!(!allow);
                }
            }
        }
    }

    /// D43 review finding: the skew-premium shortfall of a net-raising
    /// trade. Production skew (K 2.5M contracts, 1% clamp) at $75.
    #[test]
    fn premium_shortfall_counts_the_clamp_crossing() {
        let (k, imp, o) = (2_500_000_000_000u64, 100u16, 75_000_000u64);
        let c = 1_000_000i128;
        let buy = |n: i128, s: i128| skew_price_raw(o, n, s, k, imp).unwrap() as u64;
        // Quadratic branch on both sides: the premium paid covers Δx up to rounding
        for (n, s) in [(0, 1_000), (10_000, 5_000), (-20_000, -10_000), (0, 1)] {
            let (n, s) = (n * c, s * c);
            let exec = skew_price_raw(o, n, s, k, imp).unwrap() as u64;
            let short = premium_shortfall(n, n + s, o, exec, s.unsigned_abs() as u64, k, imp).unwrap();
            assert!(short <= 2, "{} {} {}", n, s, short);
        }
        // +25k then a 60k long: N/2K 0.5% before, (N+s)/2K 1.7% after. The
        // trader pays 1% of $4.5M = $45k; x rises from o·N²/2K = $9,375 to
        // the 1% cap on $6.375M, $63,750: $9,375 short (o·K·1%²/2 at most)
        let (n, s) = (25_000 * c, 60_000 * c);
        let exec = buy(n, s);
        assert_eq!(exec, o + o / 100);
        let short = premium_shortfall(n, n + s, o, exec, s as u64, k, imp).unwrap();
        let x0 = unwind_premium(n, o, k, imp).unwrap();
        let x1 = unwind_premium(n + s, o, k, imp).unwrap();
        assert_eq!((x0, x1), (9_375_000_000, 63_750_000_000));
        assert_eq!(short, x1 - x0 - 45_000_000_000);
        assert_eq!(short, 9_375_000_000);
        // A deficit-mode ADL closes at the mark: no premium, all of Δx counts
        assert_eq!(premium_shortfall(n, n + s, o, o, s as u64, k, imp).unwrap(), x1 - x0);
        // No skew, or a trade that lowers |net|: nothing
        assert_eq!(premium_shortfall(n, n + s, o, o, s as u64, 0, imp).unwrap(), 0);
        assert_eq!(premium_shortfall(n + s, n, o, o, s as u64, k, imp).unwrap(), 0);
    }

    #[test]
    fn caps_and_triggers() {
        // Net cap at the limit and +1 unit
        assert!(net_cap_ok(1_000_000, 10_000, 1_000_000));
        assert!(!net_cap_ok(1_000_001, 10_000, 1_000_000));
        assert!(net_cap_ok(0, 0, 0));
        // Weekend: nn·G ≤ α·X; G = 0 skips
        assert!(weekend_cap_ok(500_000, 2_000, 1_000, 1_000_000));
        assert!(!weekend_cap_ok(500_001, 2_000, 1_000, 1_000_000));
        assert!(weekend_cap_ok(u128::MAX, 0, 0, 0));
        // D43: the bound is on with W > 0; k_net·G ≤ α·1e4 at the edge
        assert!(!weekend_bound_on(0) && weekend_bound_on(1));
        assert!(k_net_within_weekend_bound(5_000, 2_000, 1_000));
        assert!(!k_net_within_weekend_bound(5_001, 2_000, 1_000));
        assert!(k_net_within_weekend_bound(100_000, 0, 0));
        assert!(k_net_within_weekend_bound(u32::MAX, 0, 0));
        assert_eq!(g_bps(0, 1_000, 2_000), 2_000);
        assert_eq!(g_bps(0, 1_000, 0), 1_000);
        assert_eq!(g_bps(1_500, 1_000, 2_000), 1_500);
        // Per account: 25% of a 1× net cap on $1M = $250k
        assert!(per_account_ok(200_000, 50_000, 2_500, 10_000, 1_000_000));
        assert!(!per_account_ok(200_000, 50_001, 2_500, 10_000, 1_000_000));
        assert!(per_account_ok(u64::MAX, u64::MAX, 0, 0, 0));
        // Exposure triggers
        assert!(!n_trig(1_500_000, 15_000, 1_000_000) && n_trig(1_500_001, 15_000, 1_000_000));
        assert!(!n_trig(u128::MAX, 0, 0));
        assert!(!w_trig(true, 500_000, 2_000, 1_000, 1_000_000));
        assert!(w_trig(true, 500_001, 2_000, 1_000, 1_000_000));
        assert!(!w_trig(false, u128::MAX, 2_000, 1_000, 0));
        assert!(!w_trig(true, u128::MAX, 0, 1_000, 0));
        assert!(hard_net_ok(1_500_000, 15_000, 1_000_000) && !hard_net_ok(1_500_001, 15_000, 1_000_000));
        assert!(hard_net_ok(u128::MAX, 0, 0) && hard_net_ok(0, 15_000, 0));
        // Targets
        assert_eq!(bps_of(10_000, 1_000_001), 1_000_001);
        assert_eq!(bps_of(5_000, 1_000_001), 500_000);
        assert_eq!(weekend_target(1_000, 1_000_001, 2_000), 500_000);
        assert_eq!(weekend_target(1_000, 1_000_001, 0), u128::MAX);
    }

    #[test]
    fn adl_score_matches_the_formula() {
        // Long 100 at $75, mark $90, equity $2,250 on $9,000 notional:
        // (90/75)·(9000/2250) = 4.8 = 48_000 bps
        assert_eq!(adl_score(true, 90_000_000, 75_000_000, 9_000_000_000, 2_250_000_000), 48_000);
        // Short 100 at $90, mark $75, equity $2,250 on $7,500: (90/75)·(7500/2250) = 4.0
        assert_eq!(adl_score(false, 75_000_000, 90_000_000, 7_500_000_000, 2_250_000_000), 40_000);
        assert_eq!(adl_score(true, 1, 1, 1, 0), 0);
        assert_eq!(adl_score(true, 1, 0, 1, 1), u128::MAX);
        // Exact on overflowing intermediates
        let n = MAX_OPEN_INTEREST as u128 * u64::MAX as u128 / 1_000_000;
        let s = adl_score(true, u64::MAX, u64::MAX, n, 1);
        assert_eq!(s, n * 10_000);
        let half = u64::MAX / 2;
        let expected = mul_div_floor(n * 10_000, u64::MAX as u128, half as u128).unwrap() / 7;
        assert_eq!(adl_score(true, u64::MAX, half, n, 7), expected);
        assert_eq!(adl_score(false, half, u64::MAX, n, 7), expected);
        // Quotient beyond u128 saturates
        assert_eq!(adl_score(true, u64::MAX, 1, n, 1), u128::MAX);
    }

    #[test]
    fn adl_floor_decays_linearly() {
        assert_eq!(adl_floor_now(0), ADL_FLOOR_START_BPS);
        assert_eq!(adl_floor_now(-5), ADL_FLOOR_START_BPS);
        assert_eq!(adl_floor_now(ADL_FLOOR_DECAY_SECS / 2), ADL_FLOOR_START_BPS / 2);
        assert_eq!(adl_floor_now(1), 999_953); // floors
        assert_eq!(adl_floor_now(ADL_FLOOR_DECAY_SECS), 0);
        assert_eq!(adl_floor_now(10 * ADL_FLOOR_DECAY_SECS), 0);
        // Settled markets pass wall-clock seconds: same curve
        assert_eq!(adl_floor_now(3 * 3_600), 500_000);
    }

    #[test]
    fn funding_catch_up() {
        assert_eq!(funding_catchup(3_599, 3_600), None);
        assert_eq!(funding_catchup(1, 0), None);
        assert_eq!(funding_catchup(3_600, 3_600), Some(FundingCatchup { n: 1, dropped: 0, carry: 0 }));
        assert_eq!(funding_catchup(3 * 3_600 + 17, 3_600), Some(FundingCatchup { n: 3, dropped: 0, carry: 17 }));
        // Exactly 168 due with a remainder: carry kept, nothing dropped (F24)
        assert_eq!(funding_catchup(168 * 3_600 + 59, 3_600), Some(FundingCatchup { n: 168, dropped: 0, carry: 59 }));
        // 169 due: carry 0, one dropped
        assert_eq!(funding_catchup(169 * 3_600 + 59, 3_600), Some(FundingCatchup { n: 168, dropped: 1, carry: 0 }));
        assert_eq!(funding_catchup(i64::MAX, 60).unwrap().n, 168);
        // i64 saturation of the cumulative index
        assert_eq!(funding_accumulate(i64::MAX - 5, 168, 1_000), i64::MAX);
        assert_eq!(funding_accumulate(i64::MIN + 5, 168, -1_000), i64::MIN);
        assert_eq!(funding_accumulate(100, 3, -7), 79);
    }

    #[test]
    fn share_math() {
        assert_eq!(shares_for_deposit(100, 1_000, 1_000), Some(100));
        assert_eq!(shares_for_deposit(100, 1_000, 1_001), Some(99)); // floors
        assert_eq!(shares_for_deposit(100, 1_000, 0), None);
        assert_eq!(shares_for_deposit(100, 1_000, -5), None);
        assert_eq!(redeem_value(100, 1_000, 1_001), 100);
        assert_eq!(redeem_value(100, 1_000, 999), 99);
        assert_eq!(redeem_value(100, 1_000, -1), 0);
        assert_eq!(redeem_value(100, 0, 1_000), 0);
    }

    /// §17.1 #13: deposits at NAV_mint and withdrawals at NAV_redeem never
    /// lower per-share mark NAV.
    #[test]
    fn lp_flows_never_lower_nps() {
        let mut r = Rng(3);
        for _ in 0..20_000 {
            let mut p = PoolState {
                cash: r.below(10_000_000_000_000),
                lp_shares: 1_000_000 + r.below(10_000_000_000_000),
                open_positions: r.below(50),
                ..Default::default()
            };
            let l = r.below(1_000_000_000);
            let s = r.below(1_000_000_000);
            p.long_entry_sum = l as u128 * (1 + r.below(100_000_000)) as u128;
            p.short_entry_sum = s as u128 * (1 + r.below(100_000_000)) as u128;
            let mark = 1 + r.below(100_000_000);
            let h = r.below(10_001) as u16;
            // Half the cases price the refundable skew premium (D39)
            let (k, mi) = if r.below(2) == 0 { (0, 0) } else { (1 + r.below(10_000_000_000_000), r.below(1_001) as u16) };
            let v = pool_view_raw(&p, l, s, 0, mark, 0, h, k, mi).unwrap();
            assert!(v.nav_redeem < v.nav_mint);
            if v.nav_mint <= 0 {
                continue;
            }
            let before = v.nps;
            // Deposit
            let amount = 1 + r.below(1_000_000_000_000);
            let minted = shares_for_deposit(amount, p.lp_shares, v.nav_mint).unwrap();
            let mut d = p;
            d.cash += amount;
            d.lp_shares += minted;
            let after = pool_view_raw(&d, l, s, 0, mark, 0, h, k, mi).unwrap().nps;
            assert!(after >= before, "deposit {} -> {}", before, after);
            // Withdraw
            let shares = 1 + r.below(p.lp_shares - 1);
            let value = redeem_value(shares, p.lp_shares, v.nav_redeem);
            if value > p.cash {
                continue;
            }
            let mut w = p;
            w.cash -= value;
            w.lp_shares -= shares;
            let after = pool_view_raw(&w, l, s, 0, mark, 0, h, k, mi).unwrap().nps;
            assert!(after >= before, "withdraw {} -> {}", before, after);
        }
    }

    /// D39: a house open pays the skew premium into U, and closing back at
    /// the same oracle pays it out again, so NAV_redeem must not count it.
    /// Open then close at a flat oracle: NAV_redeem in between is never above
    /// the cash the pool ends with, and the mark NAV does not move.
    #[test]
    fn refundable_skew_premium_is_not_lp_value() {
        let (k, mi, h) = (2_500_000_000_000u64, 100u16, 5_000u16);
        let o = 75_000_000u64;
        for (cash, q) in [(10_000_000_000_000u64, 133_333_000_000u64), (1_000_000_000_000, 13_333_000_000), (1_000_000_000_000, 1_000_000_000), (1_000_000_000_000, 1)] {
            let p0 = PoolState { cash, lp_shares: cash, ..Default::default() };
            let before = pool_view_raw(&p0, 0, 0, 0, o, 0, h, k, mi).unwrap();
            let open = skew_price_raw(o, 0, q as i128, k, mi).unwrap() as u64;
            let p1 = PoolState { long_entry_sum: q as u128 * open as u128, open_positions: 1, ..p0 };
            let v = pool_view_raw(&p1, q, 0, 0, o, 0, h, k, mi).unwrap();
            // The close sells back down the same skew
            let close = skew_price_raw(o, q as i128, -(q as i128), k, mi).unwrap() as u64;
            let pnl = (q as i128 * (close as i128 - open as i128)).div_euclid(SCALE);
            let final_cash = cash as i128 - pnl;
            assert!(v.nav_redeem <= final_cash, "q={} redeem {} > cash {}", q, v.nav_redeem, final_cash);
            // Exact up to the slack and the 1-unit roundings
            assert!(final_cash - v.nav_redeem <= 4, "q={} {} {}", q, final_cash, v.nav_redeem);
            assert!((v.nps as i128 - before.nps as i128).abs() <= 1, "q={}", q);
            // Without the premium term the open would raise NAV_redeem
            if q > 1_000_000 {
                let raw = pool_view_raw(&p1, q, 0, 0, o, 0, h, 0, 0).unwrap();
                assert!(raw.nav_redeem > final_cash);
            }
        }
        // $10M pool, 133,333 contracts: clamped at 1%, $100k of premium
        let q = 133_333_000_000u128;
        assert_eq!(unwind_premium(q as i128, o, k, mi), Some(ceil_u(ceil_u(q * o as u128, SCALE as u128) * 100, BPS)));
        assert_eq!(unwind_premium(-(q as i128), o, k, mi), unwind_premium(q as i128, o, k, mi));
        // Unclamped: o·n²/(2K·1e6), ceiled; none without impact
        assert_eq!(unwind_premium(1_000_000_000, o, k, mi), Some(15_000_000));
        assert_eq!(unwind_premium(1_000_000_001, o, k, mi), Some(15_000_001));
        assert_eq!(unwind_premium(1_000_000_000, o, 0, mi), Some(0));
        assert_eq!(unwind_premium(0, o, k, mi), Some(0));
    }

    #[test]
    fn timelock_takes_the_longer_of_current_and_proposed() {
        // Defaults: 4 d delay + 1 d window + 5 d margin = 10 d
        assert_eq!(params_timelock_secs(345_600, 345_600, 86_400, 86_400), 10 * 86_400);
        // Cutting the delay still waits for the current one
        assert_eq!(params_timelock_secs(345_600, 0, 86_400, 0), 10 * 86_400);
        assert_eq!(params_timelock_secs(0, 345_600, 0, 86_400), 10 * 86_400);
        assert_eq!(params_timelock_secs(0, 0, 0, 0), PARAMS_TIMELOCK_MARGIN_SECS);
    }

    #[test]
    fn notional_floor_saturates() {
        assert_eq!(notional_floor(1_500_000, 75_000_001), 112_500_001);
        assert_eq!(notional_floor(u64::MAX, u64::MAX), u64::MAX);
    }
}
