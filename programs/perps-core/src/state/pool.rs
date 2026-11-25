//! LP pool counterparty (docs/lp-pool.md §1.2, §1.3, §2, §10.3, §11, §15).
//!
//! The pure math lives in `crate::pool_math` (shared with the keeper and the
//! sim); this module holds the accounts and the wrappers that apply that math
//! to them. Every wrapper is `#[inline(never)]` to keep SBF frames small.

use anchor_lang::prelude::*;
use crate::errors::PerpsError;
use crate::events::BadDebt;
use crate::pool_math;
use crate::state::{Market, Side};

pub use crate::pool_math::{
    Payout, PoolView, ADL_FLOOR_DECAY_SECS, ADL_FLOOR_START_BPS, ADL_WRITE_OFF_SECS, DD_WINDOW_SECS,
    LOSS_WINDOW_SECS, LP_CLASS_COMMON, LP_DEAD_SHARES, MAX_FUNDING_CATCHUP_PERIODS, MAX_OPEN_INTEREST,
    MIN_POOL_TIMELOCK_SECS, PARAMS_TIMELOCK_MARGIN_SECS,
};

/// Pool risk and fee parameters (§1.2). Borsh, 104 bytes; offsets in comments.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq)]
pub struct PoolParams {
    pub skew_scale: u64,                     //  0  contracts (6 dp); 0 = no price impact
    pub insurance_target: u64,               //  8  USDC
    pub insurance_claim_cap_per_window: u64, // 16  USDC per loss window; 0 = unlimited
    pub liq_min_remaining_notional: u64,     // 24  USDC; smaller remainders are closed in full
    pub k_net_bps: u32,                      // 32  net cap, × capNAV
    pub hard_net_bps: u32,                   // 36  exposure-ADL line, × B; 0 = off
    pub per_account_cap_bps: u32,            // 40  × net cap; 0 = off
    pub weekend_lp_loss_bps: u32,            // 44  α
    pub weekend_gap_bps: u32,                // 48  G override; 0 = derive (§3.1)
    pub weekend_cap_window_secs: u32,        // 52  0 = weekend cap, force-reduce and LP pre-close gate off
    pub withdrawal_delay_secs: u32,          // 56
    pub withdrawal_claim_window_secs: u32,   // 60  open seconds after unlock; 0 = never lapses
    pub lp_reopen_quiet_secs: u32,           // 64  no LP deposit/withdraw this long after each session open
    pub adl_net_target_bps: u32,             // 68  drawdown-mode ADL reduces net to this × B
    pub adl_exposure_score_floor_bps: u32,   // 72  minimum score for exposure-mode ADL
    pub dd_freeze_bps: u16,                  // 76  drawdown that freezes net-raising house opens; 0 = off
    pub dd_adl_bps: u16,                     // 78  drawdown that enables exposure ADL; 0 = off
    pub dd_withdraw_block_bps: u16,          // 80  drawdown that blocks lp_withdraw; 0 = off
    pub redeem_loss_haircut_bps: u16,        // 82  h: share of net trader losses NAV_redeem counts
    pub open_fee_bps: u16,                   // 84
    pub max_impact_bps: u16,                 // 86  clamp on |skew premium|
    pub liq_penalty_bps: u16,                // 88  of closed mark notional (stage 1)
    pub liq_keeper_share_bps: u16,           // 90  of the penalty
    pub backstop_threshold_bps: u16,         // 92  backstop line = MMR_in_force × this / 10_000
    pub min_liq_slice_bps: u16,              // 94  minimum stage-1 slice, of size
    pub treasury_fee_bps: u16,               // 96  of net fees
    pub lp_loss_cap_window_bps: u16,         // 98  × loss_window_nav; the LP's share of one loss window
    pub profit_close_quiet_secs: u32,        // 100 no profitable close_position this long after each session open (§4.5)
}

impl PoolParams {
    pub const LEN: usize = 8 * 4 + 4 * 11 + 2 * 12 + 4; // 104

    /// Production defaults for OIL (§15.1).
    pub fn production() -> Self {
        PoolParams {
            skew_scale: 2_500_000_000_000,
            insurance_target: 250_000_000_000,
            insurance_claim_cap_per_window: 0,
            liq_min_remaining_notional: 100_000_000,
            k_net_bps: 4_000,
            hard_net_bps: 6_000,
            per_account_cap_bps: 2_500,
            weekend_lp_loss_bps: 1_000,
            weekend_gap_bps: 0,
            weekend_cap_window_secs: 86_400,
            withdrawal_delay_secs: 345_600,
            withdrawal_claim_window_secs: 259_200,
            lp_reopen_quiet_secs: 1_800,
            adl_net_target_bps: 2_000,
            adl_exposure_score_floor_bps: 0,
            dd_freeze_bps: 1_000,
            dd_adl_bps: 1_500,
            dd_withdraw_block_bps: 0,
            redeem_loss_haircut_bps: 5_000,
            open_fee_bps: 5,
            max_impact_bps: 100,
            liq_penalty_bps: 200,
            liq_keeper_share_bps: 2_000,
            backstop_threshold_bps: 6_667,
            min_liq_slice_bps: 2_000,
            treasury_fee_bps: 1_000,
            lp_loss_cap_window_bps: 500,
            profit_close_quiet_secs: 300,
        }
    }

    /// Neutral values (§15.2): every new pricing, fee and liquidation rule is
    /// off or wide, so the existing tests keep their numbers (D10).
    pub fn neutral() -> Self {
        PoolParams {
            skew_scale: 0,
            open_fee_bps: 0,
            treasury_fee_bps: 0,
            insurance_target: u64::MAX,
            k_net_bps: 100_000,
            hard_net_bps: 0,
            per_account_cap_bps: 0,
            weekend_cap_window_secs: 0,
            dd_freeze_bps: 0,
            dd_adl_bps: 0,
            dd_withdraw_block_bps: 0,
            backstop_threshold_bps: 10_000,
            liq_penalty_bps: 0,
            lp_loss_cap_window_bps: 10_000,
            withdrawal_delay_secs: 0,
            withdrawal_claim_window_secs: 0,
            lp_reopen_quiet_secs: 0,
            profit_close_quiet_secs: 0,
            // Kept at its D43 value: D45 lowered production's to 2_000 with
            // k_net, and neutral stays as it was
            adl_net_target_bps: 2_500,
            ..Self::production()
        }
    }

    /// `PoolParams::validate` (§1.2); false → `InvalidPoolParams`.
    pub fn validate(&self, m: &Market) -> bool {
        let p = self;
        let bps = |x: u16| x <= 10_000;
        let ranges = p.k_net_bps <= 100_000
            && (p.hard_net_bps == 0 || p.hard_net_bps >= p.k_net_bps)
            && p.adl_net_target_bps <= p.k_net_bps
            && p.per_account_cap_bps <= 10_000
            && p.weekend_lp_loss_bps <= 10_000
            && p.weekend_gap_bps <= 10_000
            && bps(p.redeem_loss_haircut_bps)
            && p.weekend_cap_window_secs <= 5 * 86_400
            && p.withdrawal_delay_secs <= 30 * 86_400
            && p.lp_reopen_quiet_secs <= 86_400
            && p.profit_close_quiet_secs <= 3_600
            // Every week keeps a day of open time with lp_withdraw ungated
            // (D38): W and Q together cannot shut LP exits for good
            && pool_math::lp_gated_secs_per_week(p.weekend_cap_window_secs, p.lp_reopen_quiet_secs)
                + pool_math::LP_UNGATED_SECS
                <= pool_math::WEEK_OPEN_SECS;
        let claim = p.withdrawal_claim_window_secs == 0
            || p.withdrawal_claim_window_secs as u64
                >= pool_math::min_claim_window_secs(p.weekend_cap_window_secs, p.lp_reopen_quiet_secs);
        // With the weekend bound on, the net cap alone keeps G·net within α
        // of the capital at the G in force now (D43). G can rise later
        // (update_market_config does not see the pool); the always-on
        // weekend cap covers that
        let weekend = !pool_math::weekend_bound_on(p.weekend_cap_window_secs)
            || pool_math::k_net_within_weekend_bound(p.k_net_bps, self.g_bps(m), p.weekend_lp_loss_bps);
        let drawdown = bps(p.dd_freeze_bps)
            && bps(p.dd_adl_bps)
            && bps(p.dd_withdraw_block_bps)
            && (p.dd_freeze_bps == 0 || p.dd_adl_bps == 0 || p.dd_freeze_bps <= p.dd_adl_bps);
        let fees = p.open_fee_bps <= 1_000 && p.max_impact_bps <= 1_000 && bps(p.treasury_fee_bps);
        let liq = (1..=10_000).contains(&p.backstop_threshold_bps)
            && (1..=10_000).contains(&p.min_liq_slice_bps)
            && bps(p.liq_keeper_share_bps)
            && bps(p.lp_loss_cap_window_bps);
        // The stage-1 penalty stays below the equity of any stage-1 position (§6.3)
        let penalty = (p.liq_penalty_bps as u64) * 10_000
            < m.maintenance_margin_ratio as u64 * p.backstop_threshold_bps as u64;
        // A self-liquidating owner recovers the keeper share; the rest must
        // cover the impact and close fee it would dodge (§2.4)
        let self_liq = p.backstop_threshold_bps >= 10_000
            || (p.liq_penalty_bps as u64) * (10_000 - p.liq_keeper_share_bps.min(10_000) as u64)
                >= (p.max_impact_bps as u64 + m.taker_fee as u64) * 10_000;
        ranges && claim && weekend && drawdown && fees && liq && penalty && self_liq
    }

    /// Whether the weekend bound is on (D43, `weekend_cap_window_secs > 0`).
    pub fn weekend_bound_on(&self) -> bool {
        pool_math::weekend_bound_on(self.weekend_cap_window_secs)
    }

    /// `G_bps` (§3.1): the override, else `max(IM, weekend IM)`.
    pub fn g_bps(&self, m: &Market) -> u64 {
        pool_math::g_bps(self.weekend_gap_bps, m.initial_margin_ratio, m.weekend_initial_margin_ratio)
    }

    /// Timelock for replacing `self` with `new` (§5).
    pub fn timelock_secs(&self, new: &PoolParams) -> i64 {
        pool_math::params_timelock_secs(
            self.withdrawal_delay_secs,
            new.withdrawal_delay_secs,
            self.weekend_cap_window_secs,
            new.weekend_cap_window_secs,
        )
    }
}

/// PDA `[b"lp_pool", market]`, one per market. Offsets (with the
/// discriminator) in comments; pinned by `lp_pool_layout_is_pinned` and the
/// `tests/fixtures/lp_pool.bin` fixture the keeper decodes.
#[account]
#[derive(Default)]
pub struct LpPool {                          // offset
    pub market: Pubkey,                      //   8
    pub adl_keeper: Pubkey,                  //  40  signs exposure-mode adl (§7)
    // Buckets (USDC). insurance = Market.insurance_fund (D1)
    pub free_collateral: u64,                //  72  Σ UserAccount.collateral_balance
    pub position_collateral: u64,            //  80  Σ open Position.collateral
    pub cash: u64,                           //  88  LP-owned
    pub treasury: u64,                       //  96  protocol revenue
    // Aggregates over OPEN positions (exact integer sums, never rounded)
    pub long_entry_sum: u128,                // 104  Σ size·entry_price      (scale 1e12)
    pub short_entry_sum: u128,               // 120
    pub long_funding_sum: i128,              // 136  Σ size·last_funding_payment
    pub short_funding_sum: i128,             // 152
    pub open_positions: u64,                 // 168  real count (≠ Market.total_positions nonce)
    // LP shares (one class)
    pub lp_shares: u64,                      // 176  includes LP_DEAD_SHARES once seeded
    pub pending_withdraw_shares: u64,        // 184
    pub max_pool_cash: u64,                  // 192  LP-capital cap (compared with NAV_mint, §9.1); 0 = unlimited
    // Bad debt / ADL
    pub pending_bad_debt_long: u64,          // 200  deficit of bankrupt LONGS (recovered from profitable shorts)
    pub pending_bad_debt_short: u64,         // 208
    pub pending_since_long: i64,             // 216  debt-weighted booking time of pending_bad_debt_long (§2.6); 0 when it is 0
    pub bad_debt_cum: u64,                   // 224  Σ deficits D, saturating
    pub bad_debt_recovered_cum: u64,         // 232  haircuts + sweeps + donations applied to pending
    pub bad_debt_written_off_cum: u64,       // 240
    // Loss window (D15)
    pub loss_window_start: i64,              // 248  0 = no window yet
    pub loss_window_nav: u64,                // 256  max(NAV_redeem,0) when the window opened
    pub lp_absorbed_window: u64,             // 264
    pub ins_paid_window: u64,                // 272
    // Drawdown (D12)
    pub nps_peak: u64,                       // 280  peak per-share mark NAV (scale 1e6)
    pub nps_peak_at: i64,                    // 288
    // Vesting of swept / donated value (D18)
    pub vest_amount: u64,                    // 296
    pub vest_start: i64,                     // 304
    pub vest_end: i64,                       // 312  0 = nothing vesting
    pub donated_cum: u64,                    // 320  stat
    pub pending_params_at: i64,              // 328  0 = no proposal
    pub params: PoolParams,                  // 336  104 bytes
    pub pending_params: PoolParams,          // 440  104 bytes
    pub bump: u8,                            // 544
    pub pending_since_short: i64,            // 545  the same for pending_bad_debt_short (D36)
}                                            // 553..601 padding (48): reserved for a junior/first-loss tranche (§9.6)

impl LpPool {
    pub const SEED: &'static [u8] = b"lp_pool";
    /// Reserved tail. `profit_close_quiet_secs` took 2 × 4 bytes of it (one
    /// per `PoolParams` copy) and `pending_since_short` 8, so `LEN` and the
    /// rent did not change.
    pub const PADDING: usize = 48;
    pub const LEN: usize = 8 + 32 * 2 + 8 * 4 + 16 * 4 + 8 * 22 + PoolParams::LEN * 2 + 1 + LpPool::PADDING; // 601

    pub fn adl_required(&self) -> bool {
        self.pending_bad_debt_long > 0 || self.pending_bad_debt_short > 0
    }

    /// The fields the valuation reads.
    pub fn state(&self) -> pool_math::PoolState {
        pool_math::PoolState {
            cash: self.cash,
            long_entry_sum: self.long_entry_sum,
            short_entry_sum: self.short_entry_sum,
            long_funding_sum: self.long_funding_sum,
            short_funding_sum: self.short_funding_sum,
            open_positions: self.open_positions,
            lp_shares: self.lp_shares,
            pending_withdraw_shares: self.pending_withdraw_shares,
            nps_peak: self.nps_peak,
            vest_amount: self.vest_amount,
            vest_start: self.vest_start,
            vest_end: self.vest_end,
        }
    }

    /// Value swept or donated that has not vested yet (§10.3).
    pub fn unvested(&self, now: i64) -> u64 {
        pool_math::unvested(self.vest_amount, self.vest_start, self.vest_end, now)
    }

    /// Pending bad debt a winner on `side` repays (D13): a short winner repays
    /// bankrupt longs.
    pub fn opposite_pending(&self, side: Side) -> u64 {
        match side {
            Side::Short => self.pending_bad_debt_long,
            Side::Long => self.pending_bad_debt_short,
        }
    }

    /// `(pending, pending_since)` of the bankrupt `side` (D36).
    pub fn pending_of(&self, bankrupt: Side) -> (u64, i64) {
        match bankrupt {
            Side::Long => (self.pending_bad_debt_long, self.pending_since_long),
            Side::Short => (self.pending_bad_debt_short, self.pending_since_short),
        }
    }
}

/// PDA `[b"lp_position", lp_pool, owner, &[class]]`.
#[account]
#[derive(Default)]
pub struct LpPosition {
    pub owner: Pubkey,             //   8
    pub pool: Pubkey,              //  40
    pub shares: u64,               //  72  includes pending_shares
    pub pending_shares: u64,       //  80  locked by a withdrawal request
    pub requested_value: u64,      //  88  floor(pending·max(NAV_redeem,0)/lp_shares) at request time
    pub requested_at: i64,         //  96  0 = none
    pub unlock_at: i64,            // 104  requested_at + the delay in force at request time
    pub deposited_cum: u64,        // 112  stat
    pub withdrawn_cum: u64,        // 120  stat
    pub claim_window_secs: u32,    // 128  the claim window in force at request time
    pub class: u8,                 // 132  0 only (§9.6)
    pub bump: u8,                  // 133
}                                  // 134..166 padding (32)

impl LpPosition {
    pub const SEED: &'static [u8] = b"lp_position";
    pub const LEN: usize = 8 + 32 * 2 + 8 * 7 + 4 + 1 + 1 + 32; // 166
}

pub fn side_u8(side: Side) -> u8 {
    match side {
        Side::Long => 0,
        Side::Short => 1,
    }
}

// ---------------------------------------------------------------------------
// §2.1 Aggregate maintenance
// ---------------------------------------------------------------------------

#[inline(never)]
fn agg_apply(pool: &mut LpPool, side: Side, size: u64, entry: u64, funding: i64, add: bool) -> Result<()> {
    let e = (size as u128).checked_mul(entry as u128).ok_or(PerpsError::MathOverflow)?;
    let f = (size as i128).checked_mul(funding as i128).ok_or(PerpsError::MathOverflow)?;
    let (es, fs) = match side {
        Side::Long => (&mut pool.long_entry_sum, &mut pool.long_funding_sum),
        Side::Short => (&mut pool.short_entry_sum, &mut pool.short_funding_sum),
    };
    if add {
        *es = es.checked_add(e).ok_or(PerpsError::MathOverflow)?;
        *fs = fs.checked_add(f).ok_or(PerpsError::MathOverflow)?;
    } else {
        *es = es.checked_sub(e).ok_or(PerpsError::MathOverflow)?;
        *fs = fs.checked_sub(f).ok_or(PerpsError::MathOverflow)?;
    }
    Ok(())
}

/// Add an open position `(size, entry, last_funding_payment)` to the aggregates.
#[inline(never)]
pub fn pool_add(pool: &mut LpPool, side: Side, size: u64, entry: u64, funding: i64) -> Result<()> {
    agg_apply(pool, side, size, entry, funding, true)?;
    pool.open_positions = pool.open_positions.checked_add(1).ok_or(PerpsError::MathOverflow)?;
    Ok(())
}

/// Exact inverse of `pool_add`.
#[inline(never)]
pub fn pool_remove(pool: &mut LpPool, side: Side, size: u64, entry: u64, funding: i64) -> Result<()> {
    agg_apply(pool, side, size, entry, funding, false)?;
    pool.open_positions = pool.open_positions.checked_sub(1).ok_or(PerpsError::MathOverflow)?;
    Ok(())
}

/// An open position's `(size, entry_price, last_funding_payment)` changed
/// from `old` to `new`; the open count does not move.
#[inline(never)]
pub fn pool_replace(pool: &mut LpPool, side: Side, old: (u64, u64, i64), new: (u64, u64, i64)) -> Result<()> {
    agg_apply(pool, side, old.0, old.1, old.2, false)?;
    agg_apply(pool, side, new.0, new.1, new.2, true)
}

// ---------------------------------------------------------------------------
// §2.2 Valuation
// ---------------------------------------------------------------------------

/// O(1) valuation at `mark` (the oracle, or `settlement_price` once settled).
/// `h` is `redeem_loss_haircut_bps`.
#[inline(never)]
pub fn pool_view(pool: &LpPool, market: &Market, mark: u64, now: i64, h: u16) -> Result<PoolView> {
    let st = pool.state();
    pool_math::pool_view_raw(
        &st,
        market.long_open_interest,
        market.short_open_interest,
        market.cumulative_funding,
        mark,
        now,
        h,
        // A settled market closes at the settlement price, with no impact
        if market.is_settled() { 0 } else { pool.params.skew_scale },
        pool.params.max_impact_bps,
    )
    .ok_or_else(|| error!(PerpsError::MathOverflow))
}

/// Raise the drawdown peak, or reset it once it is `DD_WINDOW_SECS` old.
#[inline(never)]
pub fn touch_peak(pool: &mut LpPool, v: &PoolView, now: i64) {
    if pool_math::peak_resets(v.nps, pool.nps_peak, pool.nps_peak_at, now) {
        pool.nps_peak = v.nps;
        pool.nps_peak_at = now;
    }
}

/// Drawdown against the peak as it stands after `touch_peak`: once the peak
/// resets after `DD_WINDOW_SECS`, the instruction that reset it already sees
/// the healed drawdown (`v.dd_bps` is against the peak before the touch).
pub fn drawdown_now(pool: &LpPool, v: &PoolView) -> u64 {
    pool_math::drawdown_bps(v.nps, pool.nps_peak)
}

// ---------------------------------------------------------------------------
// §2.4 Skew price
// ---------------------------------------------------------------------------

/// Execution price for a trade of `delta` (+q buy, −q sell) against skew
/// `L − S` (`skew_before`).
#[inline(never)]
pub fn skew_price(oracle: u64, skew_before: i128, delta: i128, p: &PoolParams) -> Result<u64> {
    let price = pool_math::skew_price_raw(oracle, skew_before, delta, p.skew_scale, p.max_impact_bps)
        .ok_or(PerpsError::MathOverflow)?;
    require!(price > 0, PerpsError::InvalidOraclePrice);
    u64::try_from(price).map_err(|_| error!(PerpsError::MathOverflow))
}

// ---------------------------------------------------------------------------
// §2.5 Fees
// ---------------------------------------------------------------------------

/// Split a net fee `x` (after referral): treasury cut, then insurance.
#[inline(never)]
pub fn distribute_fee(m: &mut Market, pool: &mut LpPool, x: u64) -> Result<()> {
    let (t, rest) = pool_math::fee_split(x, pool.params.treasury_fee_bps);
    pool.treasury = pool.treasury.checked_add(t).ok_or(PerpsError::MathOverflow)?;
    credit_insurance(m, pool, rest)
}

/// Insurance first until `insurance_target`, then the overflow to LP cash.
#[inline(never)]
pub fn credit_insurance(m: &mut Market, pool: &mut LpPool, y: u64) -> Result<()> {
    let (to_ins, to_cash) = pool_math::insurance_split(m.insurance_fund, pool.params.insurance_target, y);
    m.insurance_fund = m.insurance_fund.checked_add(to_ins).ok_or(PerpsError::MathOverflow)?;
    pool.cash = pool.cash.checked_add(to_cash).ok_or(PerpsError::MathOverflow)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// §2.6 Deficit waterfall
// ---------------------------------------------------------------------------

/// Open a new 24 h loss window if none is open (D15). `nav_before` is
/// `NAV_redeem` at the start of the instruction.
#[inline(never)]
pub fn roll_loss_window(pool: &mut LpPool, nav_before: i128, now: i64) {
    if pool.loss_window_start == 0 || now >= pool.loss_window_start.saturating_add(LOSS_WINDOW_SECS) {
        pool.loss_window_start = now;
        pool.loss_window_nav = u64::try_from(nav_before.max(0)).unwrap_or(u64::MAX);
        pool.lp_absorbed_window = 0;
        pool.ins_paid_window = 0;
    }
}

/// Book a deficit `d > 0` of a bankrupt position on `bankrupt_side`:
/// insurance (window-capped), then the LP window cap, then pending (ADL
/// target). The position's collateral must already be in `cash`. Emits
/// `BadDebt` for `position`. Returns `(ins_paid, lp_absorbed, pending)`.
#[inline(never)]
#[allow(clippy::too_many_arguments)]
pub fn absorb_deficit(
    m: &mut Market,
    pool: &mut LpPool,
    position: Pubkey,
    d: u64,
    bankrupt_side: Side,
    nav_before: i128,
    now: i64,
) -> Result<(u64, u64, u64)> {
    roll_loss_window(pool, nav_before, now);

    // Insurance
    let cap_ins = pool_math::insurance_window_cap(pool.params.insurance_claim_cap_per_window, pool.ins_paid_window);
    let ins_paid = d.min(m.insurance_fund).min(cap_ins);
    m.insurance_fund = m.insurance_fund.checked_sub(ins_paid).ok_or(PerpsError::BucketUnderflow)?;
    pool.cash = pool.cash.checked_add(ins_paid).ok_or(PerpsError::MathOverflow)?;
    pool.ins_paid_window = pool.ins_paid_window.saturating_add(ins_paid);

    // LP: nothing moves; cash never received this amount
    let lp_cap = pool_math::lp_window_cap(pool.loss_window_nav, pool.params.lp_loss_cap_window_bps, pool.lp_absorbed_window);
    let lp_absorbed = (d - ins_paid).min(lp_cap);
    pool.lp_absorbed_window = pool.lp_absorbed_window.saturating_add(lp_absorbed);

    // Pending: the ADL recovery target
    // Each side keeps its own debt-weighted clock (D36), so fresh debt on
    // one side never inherits the other side's age
    let pending = d - ins_paid - lp_absorbed;
    if pending > 0 {
        let (slot, since) = match bankrupt_side {
            Side::Long => (&mut pool.pending_bad_debt_long, &mut pool.pending_since_long),
            Side::Short => (&mut pool.pending_bad_debt_short, &mut pool.pending_since_short),
        };
        *since = pool_math::weighted_since(*since, *slot, now, pending);
        *slot = slot.checked_add(pending).ok_or(PerpsError::MathOverflow)?;
    }
    pool.bad_debt_cum = pool.bad_debt_cum.saturating_add(d);

    emit!(BadDebt {
        position,
        side: side_u8(bankrupt_side),
        deficit: d,
        insurance_paid: ins_paid,
        lp_absorbed,
        pending_added: pending,
        pending_long: pool.pending_bad_debt_long,
        pending_short: pool.pending_bad_debt_short,
        pending_since_long: pool.pending_since_long,
        pending_since_short: pool.pending_since_short,
        window_lp_absorbed: pool.lp_absorbed_window,
        window_ins_paid: pool.ins_paid_window,
    });
    Ok((ins_paid, lp_absorbed, pending))
}

// ---------------------------------------------------------------------------
// §2.8 Paying profit
// ---------------------------------------------------------------------------

/// Pure quote of paying a positive realized amount `p` to a winner on
/// `side`: solvency haircut, bad-debt haircut, then cash. With
/// `allow_short == false` a cash shortfall is `PoolCashInsufficient`.
#[inline(never)]
pub fn quote_profit(pool: &LpPool, v: &PoolView, side: Side, p: u64, allow_short: bool) -> Result<Payout> {
    pool_math::quote_profit_raw(v.u, pool.cash, pool.opposite_pending(side), p, allow_short)
        .ok_or_else(|| error!(PerpsError::PoolCashInsufficient))
}

/// Apply a quote: reduce the opposite side's pending by the bad-debt
/// haircut and debit cash by `paid`.
#[inline(never)]
pub fn apply_payout(pool: &mut LpPool, side: Side, po: &Payout) -> Result<()> {
    if po.bad_debt_hc > 0 {
        let (slot, since) = match side {
            Side::Short => (&mut pool.pending_bad_debt_long, &mut pool.pending_since_long),
            Side::Long => (&mut pool.pending_bad_debt_short, &mut pool.pending_since_short),
        };
        *slot = slot.checked_sub(po.bad_debt_hc).ok_or(PerpsError::BucketUnderflow)?;
        if *slot == 0 {
            *since = 0;
        }
        pool.bad_debt_recovered_cum = pool.bad_debt_recovered_cum.saturating_add(po.bad_debt_hc);
    }
    pool.cash = pool.cash.checked_sub(po.paid).ok_or(PerpsError::BucketUnderflow)?;
    Ok(())
}

/// `quote_profit` then `apply_payout`.
#[inline(never)]
pub fn pay_profit(pool: &mut LpPool, v: &PoolView, side: Side, p: u64, allow_short: bool) -> Result<Payout> {
    let po = quote_profit(pool, v, side, p, allow_short)?;
    apply_payout(pool, side, &po)?;
    Ok(po)
}

// ---------------------------------------------------------------------------
// §10.1 / §10.3 Recovery of pending and vesting
// ---------------------------------------------------------------------------

/// Apply `x` of new pool value to pending bad debt, longs first (§10.1 step
/// 4). Returns the amount recovered; the caller vests the rest.
#[inline(never)]
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
    let r = r_l + r_s;
    pool.bad_debt_recovered_cum = pool.bad_debt_recovered_cum.saturating_add(r);
    r
}

/// Vest `y` (already in `cash`) into `NAV_redeem` over `withdrawal_delay_secs`,
/// merging with what is still unvested.
///
/// Departure from §10.3 (which restarts the whole schedule at `now + delay`):
/// the merged schedule ends at the amount-weighted end time
/// `floor((rem·(vest_end − now) + y·delay) / (rem + y))` after `now`, so
/// `unvested(now)` is unchanged by the merge and a dust donation barely moves
/// the end. Restarting the clock let anyone hold swept or donated value out
/// of `NAV_redeem` indefinitely with 1-unit donations.
#[inline(never)]
pub fn add_vesting(pool: &mut LpPool, y: u64, now: i64) -> Result<()> {
    if y == 0 {
        return Ok(());
    }
    let delay = (pool.params.withdrawal_delay_secs as i64).max(1);
    let rem = pool.unvested(now);
    let total = rem.checked_add(y).ok_or(PerpsError::MathOverflow)?;
    // rem > 0 implies vest_end > now
    let left_old = if rem > 0 { (pool.vest_end - now) as u128 } else { 0 };
    let weighted = rem as u128 * left_old + y as u128 * delay as u128;
    // Floored: a ceiling would let a 1-unit donation every second hold the
    // end in place for ever
    let left = (weighted / total as u128).max(1);
    pool.vest_amount = total;
    pool.vest_start = now;
    pool.vest_end = now.checked_add(i64::try_from(left).map_err(|_| error!(PerpsError::MathOverflow))?).ok_or(PerpsError::MathOverflow)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// §11 Bucket invariant
// ---------------------------------------------------------------------------

/// `free + position_collateral + insurance + cash + treasury` (u128).
pub fn bucket_total(pool: &LpPool, m: &Market) -> u128 {
    pool.free_collateral as u128
        + pool.position_collateral as u128
        + m.insurance_fund as u128
        + pool.cash as u128
        + pool.treasury as u128
}

/// `vault_token.amount ≥ Σ buckets`, where `amount_after` is the pre-CPI
/// vault balance ± this instruction's transfer.
#[inline(never)]
pub fn assert_backed(pool: &LpPool, m: &Market, amount_after: u64) -> Result<()> {
    require!(amount_after as u128 >= bucket_total(pool, m), PerpsError::VaultInvariantViolated);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Position;

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.0 = x;
            x.wrapping_mul(0x2545_f491_4f6c_dd1d)
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    fn oil() -> Market {
        Market {
            max_leverage: 20_000,
            initial_margin_ratio: 1_000,
            maintenance_margin_ratio: 500,
            taker_fee: 50,
            liquidation_fee: 100,
            weekend_initial_margin_ratio: 2_000,
            weekend_maintenance_margin_ratio: 1_000,
            max_open_interest: MAX_OPEN_INTEREST,
            ..Default::default()
        }
    }

    fn seeded(cash: u64, params: PoolParams) -> LpPool {
        LpPool { cash, lp_shares: cash, params, ..Default::default() }
    }

    // ---------------- §17.1 #1 layout ----------------

    #[test]
    fn account_sizes_are_pinned() {
        assert_eq!(PoolParams::LEN, 104);
        assert_eq!(PoolParams::default().try_to_vec().unwrap().len(), 104);
        assert_eq!(LpPool::LEN, 601);
        assert_eq!(8 + LpPool::default().try_to_vec().unwrap().len() + LpPool::PADDING, LpPool::LEN);
        assert_eq!(LpPool::PADDING, 48);
        assert_eq!(LpPosition::LEN, 166);
        assert_eq!(8 + LpPosition::default().try_to_vec().unwrap().len() + 32, LpPosition::LEN);
    }

    fn bytes<T: anchor_lang::AccountSerialize>(account: &T) -> Vec<u8> {
        let mut out = Vec::new();
        account.try_serialize(&mut out).unwrap();
        out
    }

    /// Every field distinct, so a moved field shows up as a mismatch.
    fn sample_params(base: u64) -> PoolParams {
        PoolParams {
            skew_scale: base + 1,
            insurance_target: base + 2,
            insurance_claim_cap_per_window: base + 3,
            liq_min_remaining_notional: base + 4,
            k_net_bps: 0x0100_0005,
            hard_net_bps: 0x0100_0006,
            per_account_cap_bps: 0x0100_0007,
            weekend_lp_loss_bps: 0x0100_0008,
            weekend_gap_bps: 0x0100_0009,
            weekend_cap_window_secs: 0x0100_000a,
            withdrawal_delay_secs: 0x0100_000b,
            withdrawal_claim_window_secs: 0x0100_000c,
            lp_reopen_quiet_secs: 0x0100_000d,
            adl_net_target_bps: 0x0100_000e,
            adl_exposure_score_floor_bps: 0x0100_000f,
            dd_freeze_bps: 0x0210,
            dd_adl_bps: 0x0211,
            dd_withdraw_block_bps: 0x0212,
            redeem_loss_haircut_bps: 0x0213,
            open_fee_bps: 0x0214,
            max_impact_bps: 0x0215,
            liq_penalty_bps: 0x0216,
            liq_keeper_share_bps: 0x0217,
            backstop_threshold_bps: 0x0218,
            min_liq_slice_bps: 0x0219,
            treasury_fee_bps: 0x021a,
            lp_loss_cap_window_bps: 0x021b,
            profit_close_quiet_secs: 0x0100_001c,
        }
    }

    pub(crate) fn sample_pool() -> LpPool {
        LpPool {
            market: Pubkey::new_from_array([0x11; 32]),
            adl_keeper: Pubkey::new_from_array([0x22; 32]),
            free_collateral: 0x0300_0000_0000_0001,
            position_collateral: 0x0300_0000_0000_0002,
            cash: 0x0300_0000_0000_0003,
            treasury: 0x0300_0000_0000_0004,
            long_entry_sum: 0x0400_0000_0000_0000_0000_0000_0000_0005,
            short_entry_sum: 0x0400_0000_0000_0000_0000_0000_0000_0006,
            long_funding_sum: -0x0400_0000_0000_0000_0000_0000_0000_0007,
            short_funding_sum: 0x0400_0000_0000_0000_0000_0000_0000_0008,
            open_positions: 0x0500_0000_0000_0009,
            lp_shares: 0x0500_0000_0000_000a,
            pending_withdraw_shares: 0x0500_0000_0000_000b,
            max_pool_cash: 0x0500_0000_0000_000c,
            pending_bad_debt_long: 0x0500_0000_0000_000d,
            pending_bad_debt_short: 0x0500_0000_0000_000e,
            pending_since_long: -0x0500_0000_0000_000f,
            pending_since_short: -0x0500_0000_0000_001f,
            bad_debt_cum: 0x0500_0000_0000_0010,
            bad_debt_recovered_cum: 0x0500_0000_0000_0011,
            bad_debt_written_off_cum: 0x0500_0000_0000_0012,
            loss_window_start: 0x0500_0000_0000_0013,
            loss_window_nav: 0x0500_0000_0000_0014,
            lp_absorbed_window: 0x0500_0000_0000_0015,
            ins_paid_window: 0x0500_0000_0000_0016,
            nps_peak: 0x0500_0000_0000_0017,
            nps_peak_at: 0x0500_0000_0000_0018,
            vest_amount: 0x0500_0000_0000_0019,
            vest_start: 0x0500_0000_0000_001a,
            vest_end: 0x0500_0000_0000_001b,
            donated_cum: 0x0500_0000_0000_001c,
            pending_params_at: 0x0500_0000_0000_001d,
            params: sample_params(0x0600_0000_0000_0000),
            pending_params: sample_params(0x0700_0000_0000_0000),
            bump: 0xfe,
        }
    }

    pub(crate) fn sample_lp_position() -> LpPosition {
        LpPosition {
            owner: Pubkey::new_from_array([0x33; 32]),
            pool: Pubkey::new_from_array([0x44; 32]),
            shares: 0x0800_0000_0000_0001,
            pending_shares: 0x0800_0000_0000_0002,
            requested_value: 0x0800_0000_0000_0003,
            requested_at: 0x0800_0000_0000_0004,
            unlock_at: 0x0800_0000_0000_0005,
            deposited_cum: 0x0800_0000_0000_0006,
            withdrawn_cum: 0x0800_0000_0000_0007,
            claim_window_secs: 0x0900_0008,
            class: 0x0a,
            bump: 0xfd,
        }
    }

    /// The §1.2 / §1.3 offsets the keeper (`LpPoolData`) and SDK decode.
    #[test]
    fn lp_pool_layout_is_pinned() {
        let p = sample_pool();
        let b = bytes(&p);
        assert_eq!(b.len(), LpPool::LEN - LpPool::PADDING);
        assert_eq!(&b[8..40], p.market.as_ref());
        assert_eq!(&b[40..72], p.adl_keeper.as_ref());
        let u64_at = |o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap());
        let i64_at = |o: usize| i64::from_le_bytes(b[o..o + 8].try_into().unwrap());
        let u128_at = |o: usize| u128::from_le_bytes(b[o..o + 16].try_into().unwrap());
        let i128_at = |o: usize| i128::from_le_bytes(b[o..o + 16].try_into().unwrap());
        assert_eq!(u64_at(72), p.free_collateral);
        assert_eq!(u64_at(80), p.position_collateral);
        assert_eq!(u64_at(88), p.cash);
        assert_eq!(u64_at(96), p.treasury);
        assert_eq!(u128_at(104), p.long_entry_sum);
        assert_eq!(u128_at(120), p.short_entry_sum);
        assert_eq!(i128_at(136), p.long_funding_sum);
        assert_eq!(i128_at(152), p.short_funding_sum);
        assert_eq!(u64_at(168), p.open_positions);
        assert_eq!(u64_at(176), p.lp_shares);
        assert_eq!(u64_at(184), p.pending_withdraw_shares);
        assert_eq!(u64_at(192), p.max_pool_cash);
        assert_eq!(u64_at(200), p.pending_bad_debt_long);
        assert_eq!(u64_at(208), p.pending_bad_debt_short);
        assert_eq!(i64_at(216), p.pending_since_long);
        assert_eq!(u64_at(224), p.bad_debt_cum);
        assert_eq!(u64_at(232), p.bad_debt_recovered_cum);
        assert_eq!(u64_at(240), p.bad_debt_written_off_cum);
        assert_eq!(i64_at(248), p.loss_window_start);
        assert_eq!(u64_at(256), p.loss_window_nav);
        assert_eq!(u64_at(264), p.lp_absorbed_window);
        assert_eq!(u64_at(272), p.ins_paid_window);
        assert_eq!(u64_at(280), p.nps_peak);
        assert_eq!(i64_at(288), p.nps_peak_at);
        assert_eq!(u64_at(296), p.vest_amount);
        assert_eq!(i64_at(304), p.vest_start);
        assert_eq!(i64_at(312), p.vest_end);
        assert_eq!(u64_at(320), p.donated_cum);
        assert_eq!(i64_at(328), p.pending_params_at);
        assert_eq!(&b[336..440], p.params.try_to_vec().unwrap().as_slice());
        assert_eq!(&b[440..544], p.pending_params.try_to_vec().unwrap().as_slice());
        assert_eq!(b[544], p.bump);
        assert_eq!(i64_at(545), p.pending_since_short);

        // PoolParams offsets (relative to the struct)
        let q = p.params.try_to_vec().unwrap();
        let qu64 = |o: usize| u64::from_le_bytes(q[o..o + 8].try_into().unwrap());
        let qu32 = |o: usize| u32::from_le_bytes(q[o..o + 4].try_into().unwrap());
        let qu16 = |o: usize| u16::from_le_bytes(q[o..o + 2].try_into().unwrap());
        let pp = p.params;
        assert_eq!(
            [qu64(0), qu64(8), qu64(16), qu64(24)],
            [pp.skew_scale, pp.insurance_target, pp.insurance_claim_cap_per_window, pp.liq_min_remaining_notional]
        );
        assert_eq!(
            [qu32(32), qu32(36), qu32(40), qu32(44), qu32(48), qu32(52), qu32(56), qu32(60), qu32(64), qu32(68), qu32(72)],
            [
                pp.k_net_bps, pp.hard_net_bps, pp.per_account_cap_bps, pp.weekend_lp_loss_bps, pp.weekend_gap_bps,
                pp.weekend_cap_window_secs, pp.withdrawal_delay_secs, pp.withdrawal_claim_window_secs,
                pp.lp_reopen_quiet_secs, pp.adl_net_target_bps, pp.adl_exposure_score_floor_bps,
            ]
        );
        assert_eq!(
            [qu16(76), qu16(78), qu16(80), qu16(82), qu16(84), qu16(86), qu16(88), qu16(90), qu16(92), qu16(94), qu16(96), qu16(98)],
            [
                pp.dd_freeze_bps, pp.dd_adl_bps, pp.dd_withdraw_block_bps, pp.redeem_loss_haircut_bps, pp.open_fee_bps,
                pp.max_impact_bps, pp.liq_penalty_bps, pp.liq_keeper_share_bps, pp.backstop_threshold_bps,
                pp.min_liq_slice_bps, pp.treasury_fee_bps, pp.lp_loss_cap_window_bps,
            ]
        );
        assert_eq!(qu32(100), pp.profit_close_quiet_secs);

        let lp = sample_lp_position();
        let b = bytes(&lp);
        assert_eq!(b.len(), LpPosition::LEN - 32);
        let u64_at = |o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap());
        assert_eq!(&b[8..40], lp.owner.as_ref());
        assert_eq!(&b[40..72], lp.pool.as_ref());
        assert_eq!(
            [u64_at(72), u64_at(80), u64_at(88), u64_at(96), u64_at(104), u64_at(112), u64_at(120)],
            [
                lp.shares, lp.pending_shares, lp.requested_value, lp.requested_at as u64, lp.unlock_at as u64,
                lp.deposited_cum, lp.withdrawn_cum,
            ]
        );
        assert_eq!(u32::from_le_bytes(b[128..132].try_into().unwrap()), lp.claim_window_secs);
        assert_eq!((b[132], b[133]), (lp.class, lp.bump));
    }

    /// The keeper's `keeper_pool_layout_is_pinned` decodes these same files
    /// (§16.4). Regenerate with `UPDATE_FIXTURES=1 cargo test -p perps-core --lib fixtures`.
    #[test]
    fn fixtures_match_the_serialized_accounts() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
        for (name, data) in [("lp_pool.bin", bytes(&sample_pool())), ("lp_position.bin", bytes(&sample_lp_position()))] {
            let path = format!("{}/{}", dir, name);
            if std::env::var_os("UPDATE_FIXTURES").is_some() {
                std::fs::create_dir_all(dir).unwrap();
                std::fs::write(&path, &data).unwrap();
            }
            let fixture = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {}", path, e));
            assert_eq!(fixture, data, "{} is stale", name);
        }
        // And they decode back to the same accounts
        let pool_bytes = std::fs::read(format!("{}/lp_pool.bin", dir)).unwrap();
        let decoded = <LpPool as anchor_lang::AccountDeserialize>::try_deserialize(&mut pool_bytes.as_slice()).unwrap();
        assert_eq!(bytes(&decoded), pool_bytes);
    }

    // ---------------- §17.1 #11 validate ----------------

    #[test]
    fn production_and_neutral_params_validate() {
        let m = oil();
        assert!(PoolParams::production().validate(&m));
        assert!(PoolParams::neutral().validate(&m));
        let n = PoolParams::neutral();
        assert_eq!((n.skew_scale, n.open_fee_bps, n.treasury_fee_bps, n.insurance_target), (0, 0, 0, u64::MAX));
        assert_eq!((n.k_net_bps, n.hard_net_bps, n.per_account_cap_bps, n.weekend_cap_window_secs), (100_000, 0, 0, 0));
        assert_eq!((n.dd_freeze_bps, n.dd_adl_bps, n.dd_withdraw_block_bps), (0, 0, 0));
        assert_eq!((n.backstop_threshold_bps, n.liq_penalty_bps, n.lp_loss_cap_window_bps), (10_000, 0, 10_000));
        assert_eq!((n.withdrawal_delay_secs, n.withdrawal_claim_window_secs, n.lp_reopen_quiet_secs), (0, 0, 0));
        assert_eq!((n.profit_close_quiet_secs, PoolParams::production().profit_close_quiet_secs), (0, 300));
        // Everything else is the production value
        let p = PoolParams::production();
        assert_eq!(n.max_impact_bps, p.max_impact_bps);
        assert_eq!(n.redeem_loss_haircut_bps, p.redeem_loss_haircut_bps);
        assert_eq!(n.liq_keeper_share_bps, p.liq_keeper_share_bps);
        assert_eq!(n.min_liq_slice_bps, p.min_liq_slice_bps);
        // Pinned at its D43 value, so D45 left neutral unchanged
        assert_eq!((n.adl_net_target_bps, p.adl_net_target_bps), (2_500, 2_000));
        assert_eq!(n.weekend_lp_loss_bps, p.weekend_lp_loss_bps);
        assert_eq!(n.liq_min_remaining_notional, p.liq_min_remaining_notional);
        // §5: the default timelock is 10 days
        assert_eq!(p.timelock_secs(&p), 10 * 86_400);
    }

    /// Each bound at its limit passes and one step past it fails.
    /// D38, scanned minute by minute over three CME weeks (DST included):
    /// for parameters that validate, every claim window of the minimum length
    /// `W + 5·Q + 1 day` open seconds, wherever it starts, holds at least a
    /// day of open time in which `lp_withdraw` is not gated by `W` or `Q`,
    /// and so does every week. `effective_claim_window_secs` keeps that for a
    /// request stored under narrower gates.
    #[test]
    fn valid_lp_gates_always_leave_a_day_to_withdraw() {
        use crate::session::{in_pre_close_window, is_open, seconds_since_open, SCHEDULE_CME_GLOBEX as CME};
        let start = 1_794_783_600 - 7 * 86_400; // Sun 2026-11-08 18:00 ET
        let n = 3 * 7 * 1_440;
        for (w, q) in [(86_400u32, 1_800u32), (318_600, 1_800), (86_400, 48_240), (327_600, 0), (0, 65_520), (241_200, 17_280)] {
            let p = PoolParams { weekend_cap_window_secs: w, lp_reopen_quiet_secs: q, withdrawal_claim_window_secs: 0, ..PoolParams::production() };
            assert!(p.validate(&oil()), "W={} Q={}", w, q);
            // Per minute: open, and open with lp_withdraw ungated
            let mut open = vec![0i64; n + 1];
            let mut free = vec![0i64; n + 1];
            for i in 0..n {
                let t = start + 60 * i as i64;
                let o = is_open(CME, t);
                let gated = in_pre_close_window(CME, t, w) || seconds_since_open(CME, t).map_or(false, |s| s < q as i64);
                open[i + 1] = open[i] + o as i64 * 60;
                free[i + 1] = free[i] + (o && !gated) as i64 * 60;
            }
            let c = crate::pool_math::min_claim_window_secs(w, q) as i64;
            let mut j = 0;
            for i in 0..n {
                while j < n && open[j] - open[i] < c {
                    j += 1;
                }
                if open[j] - open[i] < c {
                    break; // runs past the scan
                }
                assert!(free[j] - free[i] >= 86_400, "W={} Q={} start {} has {}", w, q, i, free[j] - free[i]);
            }
            // Every week (Sunday open to Sunday open)
            for wk in 0..3 {
                let (a, b) = (wk * 7 * 1_440, ((wk + 1) * 7 * 1_440).min(n));
                if b - a == 7 * 1_440 {
                    assert!(free[b] - free[a] >= 86_400, "W={} Q={} week {}", w, q, wk);
                }
            }
        }
        // A request stored under the production gates is held to the wider
        // gates a later apply brings in, never to less than it stored
        assert_eq!(crate::pool_math::effective_claim_window_secs(259_200, 86_400, 1_800), 259_200);
        assert_eq!(crate::pool_math::effective_claim_window_secs(181_800, 318_600, 1_800), 414_000);
        assert_eq!(crate::pool_math::effective_claim_window_secs(0, 318_600, 1_800), 0);
    }

    /// D43: with the weekend bound on (`W > 0`), `k_net·G ≤ α·10_000`.
    /// The edge at G 20%, α 10% is 0.5×; production (D45) sits at 0.4×,
    /// a 20% exit buffer below it.
    #[test]
    fn validate_holds_k_net_to_the_weekend_bound() {
        let m = oil();
        assert_eq!(PoolParams::production().g_bps(&m), 2_000);
        let ok = |f: &dyn Fn(&mut PoolParams)| {
            let mut p = PoolParams::production();
            f(&mut p);
            p.validate(&m)
        };
        // The edge (no exit buffer) is still valid; one past it is not
        assert!(ok(&|p| p.k_net_bps = 5_000));
        assert!(!ok(&|p| p.k_net_bps = 5_001));
        let p = PoolParams::production();
        // Production's exit buffer, 1 − k_net·G/α (§15.1, D45): 20%
        assert_eq!(10_000 - p.k_net_bps as u64 * p.g_bps(&m) / p.weekend_lp_loss_bps as u64, 2_000);
        // Off with W = 0: the old 10× range applies
        assert!(ok(&|p| { p.k_net_bps = 5_001; p.weekend_cap_window_secs = 0; }));
        // α scales it: α 15% allows 0.75×
        assert!(ok(&|p| { p.weekend_lp_loss_bps = 1_500; p.k_net_bps = 7_500; p.hard_net_bps = 11_250; }));
        assert!(!ok(&|p| { p.weekend_lp_loss_bps = 1_500; p.k_net_bps = 7_501; p.hard_net_bps = 11_250; }));
        // An overridden G replaces max(IM, weekend IM): G 10% allows 1×, G 25% only 0.4×
        assert!(ok(&|p| { p.weekend_gap_bps = 1_000; p.k_net_bps = 10_000; p.hard_net_bps = 15_000; }));
        assert!(!ok(&|p| { p.weekend_gap_bps = 1_000; p.k_net_bps = 10_001; p.hard_net_bps = 15_000; }));
        assert!(ok(&|p| { p.weekend_gap_bps = 2_500; p.k_net_bps = 4_000; p.adl_net_target_bps = 2_500; }));
        assert!(!ok(&|p| { p.weekend_gap_bps = 2_500; p.k_net_bps = 4_001; p.adl_net_target_bps = 2_500; }));
        // A derived G follows the market: weekend IM 25% (not overridden)
        // gives 0.4×, production's k_net with no buffer left; 30% gives 0.333×
        let m25 = Market { weekend_initial_margin_ratio: 2_500, ..oil() };
        assert!(PoolParams::production().validate(&m25));
        assert!(!PoolParams { k_net_bps: 4_001, ..PoolParams::production() }.validate(&m25));
        let m30 = Market { weekend_initial_margin_ratio: 3_000, ..oil() };
        assert!(!PoolParams::production().validate(&m30));
        assert!(PoolParams { k_net_bps: 3_333, adl_net_target_bps: 1_666, ..PoolParams::production() }.validate(&m30));
        // α = 0 with the bound on allows no net cap at all
        assert!(ok(&|p| { p.weekend_lp_loss_bps = 0; p.k_net_bps = 0; p.adl_net_target_bps = 0; }));
        assert!(!ok(&|p| { p.weekend_lp_loss_bps = 0; p.k_net_bps = 1; p.adl_net_target_bps = 0; }));
    }

    #[test]
    fn validate_checks_every_bound() {
        let m = oil();
        let base = PoolParams::production();
        let ok = |f: &dyn Fn(&mut PoolParams)| {
            let mut p = base;
            f(&mut p);
            p.validate(&m)
        };
        // k_net ≤ 10× (weekend bound off, D43); hard_net 0 or ≥ k_net; adl target ≤ k_net
        assert!(ok(&|p| { p.k_net_bps = 100_000; p.hard_net_bps = 0; p.weekend_cap_window_secs = 0; }));
        assert!(!ok(&|p| { p.k_net_bps = 100_001; p.hard_net_bps = 0; p.weekend_cap_window_secs = 0; }));
        assert!(ok(&|p| p.hard_net_bps = 4_000));
        assert!(!ok(&|p| p.hard_net_bps = 3_999));
        assert!(ok(&|p| p.hard_net_bps = 0));
        assert!(ok(&|p| p.adl_net_target_bps = 4_000));
        assert!(!ok(&|p| p.adl_net_target_bps = 4_001));
        // bps fields ≤ 100%
        assert!(ok(&|p| p.per_account_cap_bps = 10_000));
        assert!(!ok(&|p| p.per_account_cap_bps = 10_001));
        assert!(ok(&|p| p.weekend_lp_loss_bps = 10_000));
        assert!(!ok(&|p| p.weekend_lp_loss_bps = 10_001));
        assert!(ok(&|p| { p.weekend_gap_bps = 10_000; p.weekend_cap_window_secs = 0; }));
        assert!(!ok(&|p| { p.weekend_gap_bps = 10_001; p.weekend_cap_window_secs = 0; }));
        assert!(ok(&|p| p.redeem_loss_haircut_bps = 10_000));
        assert!(!ok(&|p| p.redeem_loss_haircut_bps = 10_001));
        // windows (claim window off so only the bound under test bites).
        // W + 5·Q + 1 day ≤ 414_000 open seconds a week (D38): with Q = 1_800,
        // W ≤ 318_600; with W = 1 day, Q ≤ 48_240
        let wq = |w: u32, q: u32| ok(&|p| { p.weekend_cap_window_secs = w; p.lp_reopen_quiet_secs = q; p.withdrawal_claim_window_secs = 0; });
        assert!(wq(318_600, 1_800));
        assert!(!wq(318_601, 1_800));
        assert!(wq(86_400, 48_240));
        assert!(!wq(86_400, 48_241));
        assert!(wq(327_600, 0));
        assert!(wq(0, 65_520));
        // The combinations that shut lp_withdraw for a whole CME week all fail
        for (w, q) in [(432_000, 0), (432_000, 1_800), (428_400, 0), (345_600, 86_400), (86_400, 82_800), (0, 86_400)] {
            assert!(!wq(w, q), "W={} Q={}", w, q);
        }
        assert!(!wq(5 * 86_400 + 1, 0));
        assert!(!wq(0, 86_401));
        assert!(ok(&|p| p.withdrawal_delay_secs = 30 * 86_400));
        assert!(!ok(&|p| p.withdrawal_delay_secs = 30 * 86_400 + 1));
        assert!(ok(&|p| p.profit_close_quiet_secs = 3_600));
        assert!(!ok(&|p| p.profit_close_quiet_secs = 3_601));
        // claim window ≥ W + 5·Q + 1 day = 181_800 with the defaults
        assert!(ok(&|p| p.withdrawal_claim_window_secs = 181_800));
        assert!(!ok(&|p| p.withdrawal_claim_window_secs = 181_799));
        assert!(ok(&|p| p.withdrawal_claim_window_secs = 0));
        assert!(!ok(&|p| p.withdrawal_claim_window_secs = 1));
        // drawdown lines
        assert!(ok(&|p| { p.dd_freeze_bps = 1_500; p.dd_adl_bps = 1_500; }));
        assert!(!ok(&|p| { p.dd_freeze_bps = 1_501; p.dd_adl_bps = 1_500; }));
        assert!(ok(&|p| { p.dd_freeze_bps = 9_000; p.dd_adl_bps = 0; }));
        assert!(ok(&|p| { p.dd_freeze_bps = 0; p.dd_adl_bps = 10_000; }));
        assert!(!ok(&|p| { p.dd_freeze_bps = 0; p.dd_adl_bps = 10_001; }));
        assert!(!ok(&|p| { p.dd_freeze_bps = 10_001; p.dd_adl_bps = 0; }));
        assert!(ok(&|p| p.dd_withdraw_block_bps = 10_000));
        assert!(!ok(&|p| p.dd_withdraw_block_bps = 10_001));
        // fees
        assert!(ok(&|p| p.open_fee_bps = 1_000));
        assert!(!ok(&|p| p.open_fee_bps = 1_001));
        assert!(ok(&|p| p.treasury_fee_bps = 10_000));
        assert!(!ok(&|p| p.treasury_fee_bps = 10_001));
        assert!(!ok(&|p| p.max_impact_bps = 1_001));
        // liquidation shape
        assert!(!ok(&|p| p.backstop_threshold_bps = 0));
        assert!(!ok(&|p| p.backstop_threshold_bps = 10_001));
        assert!(ok(&|p| { p.backstop_threshold_bps = 10_000; }));
        assert!(!ok(&|p| p.min_liq_slice_bps = 0));
        assert!(ok(&|p| p.min_liq_slice_bps = 1));
        assert!(ok(&|p| p.min_liq_slice_bps = 10_000));
        assert!(!ok(&|p| p.min_liq_slice_bps = 10_001));
        assert!(!ok(&|p| p.liq_keeper_share_bps = 10_001));
        assert!(ok(&|p| p.lp_loss_cap_window_bps = 10_000));
        assert!(!ok(&|p| p.lp_loss_cap_window_bps = 10_001));
    }

    #[test]
    fn validate_keeps_the_penalty_below_stage_one_equity() {
        let m = oil(); // MMR 500
        // penalty·1e4 < 500·backstop; backstop 6_667 → penalty ≤ 333
        let with = |pen: u16, backstop: u16| PoolParams {
            liq_penalty_bps: pen,
            backstop_threshold_bps: backstop,
            liq_keeper_share_bps: 0,
            max_impact_bps: 0,
            ..PoolParams::production()
        };
        assert!(with(333, 6_667).validate(&m));
        assert!(!with(334, 6_667).validate(&m));
        // Stage 2 only: still bounded by MMR·1
        assert!(with(499, 10_000).validate(&m));
        assert!(!with(500, 10_000).validate(&m));
        assert!(with(0, 10_000).validate(&m));
    }

    #[test]
    fn validate_makes_self_liquidation_cost_more_than_a_close() {
        let m = oil(); // taker 50 bps
        // pen·(1e4 − share) ≥ (impact + taker)·1e4: 200·0.8 = 160 ≥ 100 + 50
        let with = |pen: u16, share: u16, impact: u16| PoolParams {
            liq_penalty_bps: pen,
            liq_keeper_share_bps: share,
            max_impact_bps: impact,
            ..PoolParams::production()
        };
        assert!(with(200, 2_000, 100).validate(&m));
        assert!(with(200, 2_500, 100).validate(&m)); // 150 == 150
        assert!(!with(200, 2_501, 100).validate(&m));
        assert!(!with(200, 2_000, 111).validate(&m)); // 160 < 161
        assert!(with(200, 2_000, 110).validate(&m));
        // With stage 1 off the rule does not apply
        let mut p = with(0, 10_000, 1_000);
        p.backstop_threshold_bps = 10_000;
        assert!(p.validate(&m));
        p.backstop_threshold_bps = 9_999;
        assert!(!p.validate(&m));
    }

    // ---------------- §17.1 #2 aggregates ----------------

    #[derive(Clone)]
    struct Book {
        positions: Vec<Position>,
        pool: LpPool,
        market: Market,
    }

    impl Book {
        fn new(cash: u64) -> Self {
            Book { positions: vec![], pool: seeded(cash, PoolParams::neutral()), market: oil() }
        }
        fn open(&mut self, side: Side, size: u64, entry: u64, f0: i64) {
            pool_add(&mut self.pool, side, size, entry, f0).unwrap();
            match side {
                Side::Long => self.market.long_open_interest += size,
                Side::Short => self.market.short_open_interest += size,
            }
            self.positions.push(Position { side, size, entry_price: entry, last_funding_payment: f0, ..Default::default() });
        }
        /// What closing everything at `mark` would realize.
        fn u_eng(&self, mark: u64) -> i128 {
            self.positions
                .iter()
                .map(|p| p.unrealized_pnl(mark) as i128 + p.accrued_funding(self.market.cumulative_funding) as i128)
                .sum()
        }
    }

    #[test]
    fn matched_pairs_cancel_exactly() {
        let mut r = Rng(1);
        for _ in 0..300 {
            let mut b = Book::new(1_000_000_000_000);
            let pairs = 1 + r.below(20);
            for _ in 0..pairs {
                let size = 1 + r.below(1_000_000_000);
                let price = 1 + r.below(200_000_000);
                let f0 = r.below(2_000_000) as i64 - 1_000_000;
                b.open(Side::Long, size, price, f0);
                b.open(Side::Short, size, price, f0);
            }
            b.market.cumulative_funding = r.below(4_000_000) as i64 - 2_000_000;
            for _ in 0..5 {
                let mark = 1 + r.below(300_000_000);
                let v = pool_view(&b.pool, &b.market, mark, 0, 5_000).unwrap();
                assert_eq!(v.u, 0);
                assert_eq!(b.u_eng(mark), 0);
                assert_eq!(v.net, 0);
            }
        }
    }

    #[test]
    fn aggregate_valuation_brackets_the_engine() {
        let mut r = Rng(2);
        for _ in 0..500 {
            let mut b = Book::new(r.below(10_000_000_000_000));
            for _ in 0..r.below(10) {
                let size = 1 + r.below(1_000_000_000);
                let price = 1 + r.below(200_000_000);
                let f0 = r.below(2_000_000) as i64 - 1_000_000;
                b.open(Side::Long, size, price, f0);
                b.open(Side::Short, size, price, f0);
            }
            for _ in 0..r.below(12) {
                let side = if r.below(2) == 0 { Side::Long } else { Side::Short };
                b.open(side, 1 + r.below(1_000_000_000), 1 + r.below(200_000_000), r.below(2_000_000) as i64 - 1_000_000);
            }
            b.market.cumulative_funding = r.below(4_000_000) as i64 - 2_000_000;
            let mark = 1 + r.below(300_000_000);
            let h = r.below(10_001) as u16;
            let v = pool_view(&b.pool, &b.market, mark, 0, h).unwrap();
            let u_eng = b.u_eng(mark);
            let slack = 2 * b.pool.open_positions as i128 + 1;
            assert!((u_eng - v.u).abs() <= slack, "{} {} {}", u_eng, v.u, slack);
            let cash = b.pool.cash as i128;
            assert!(v.nav_mint >= cash - u_eng, "{} {}", v.nav_mint, cash - u_eng);
            assert!(cash - u_eng >= v.nav_redeem, "{} {}", cash - u_eng, v.nav_redeem);
            // Aggregates equal Σ over open positions exactly
            let (mut les, mut ses, mut lfs, mut sfs) = (0u128, 0u128, 0i128, 0i128);
            for p in &b.positions {
                match p.side {
                    Side::Long => {
                        les += p.size as u128 * p.entry_price as u128;
                        lfs += p.size as i128 * p.last_funding_payment as i128;
                    }
                    Side::Short => {
                        ses += p.size as u128 * p.entry_price as u128;
                        sfs += p.size as i128 * p.last_funding_payment as i128;
                    }
                }
            }
            assert_eq!((b.pool.long_entry_sum, b.pool.short_entry_sum), (les, ses));
            assert_eq!((b.pool.long_funding_sum, b.pool.short_funding_sum), (lfs, sfs));
            assert_eq!(b.pool.open_positions as usize, b.positions.len());
        }
    }

    #[test]
    fn closing_one_leg_leaves_nav_unchanged() {
        // §2.3.2: after the winner (or the loser) of a matched pair closes,
        // `cash − U` is unchanged; the survivor is valued as net exposure.
        let mut b = Book::new(1_000_000_000_000);
        b.open(Side::Long, 100_000_000, 75_000_000, 0);
        b.open(Side::Short, 100_000_000, 75_000_000, 0);
        let mark = 80_000_000;
        let v0 = pool_view(&b.pool, &b.market, mark, 0, 10_000).unwrap();
        for winner_first in [true, false] {
            let mut c = b.clone();
            let idx = if winner_first { 0 } else { 1 };
            let p = c.positions.remove(idx);
            let realized = p.unrealized_pnl(mark) as i128;
            pool_remove(&mut c.pool, p.side, p.size, p.entry_price, p.last_funding_payment).unwrap();
            match p.side {
                Side::Long => c.market.long_open_interest -= p.size,
                Side::Short => c.market.short_open_interest -= p.size,
            }
            c.pool.cash = (c.pool.cash as i128 - realized) as u64;
            let v = pool_view(&c.pool, &c.market, mark, 0, 10_000).unwrap();
            assert_eq!(c.pool.cash as i128 - v.u, b.pool.cash as i128 - v0.u);
        }
    }

    #[test]
    fn pool_replace_round_trips() {
        let mut r = Rng(9);
        for _ in 0..1_000 {
            let mut pool = seeded(1, PoolParams::neutral());
            let side = if r.below(2) == 0 { Side::Long } else { Side::Short };
            let old = (1 + r.below(u32::MAX as u64), 1 + r.below(u32::MAX as u64), r.next() as i64 / 4);
            let new = (1 + r.below(u32::MAX as u64), 1 + r.below(u32::MAX as u64), r.next() as i64 / 4);
            pool_add(&mut pool, side, old.0, old.1, old.2).unwrap();
            let before = bytes(&pool);
            pool_replace(&mut pool, side, old, new).unwrap();
            assert_eq!(pool.open_positions, 1);
            let mut direct = seeded(1, PoolParams::neutral());
            pool_add(&mut direct, side, new.0, new.1, new.2).unwrap();
            assert_eq!(bytes(&pool), bytes(&direct));
            pool_replace(&mut pool, side, new, old).unwrap();
            assert_eq!(bytes(&pool), before);
            pool_remove(&mut pool, side, old.0, old.1, old.2).unwrap();
            assert_eq!(bytes(&pool), bytes(&seeded(1, PoolParams::neutral())));
        }
    }

    #[test]
    fn pool_remove_underflow_is_an_error() {
        let mut pool = seeded(1, PoolParams::neutral());
        assert!(pool_remove(&mut pool, Side::Long, 1, 1, 0).is_err());
        let mut pool = seeded(1, PoolParams::neutral());
        pool_add(&mut pool, Side::Long, 1, 1, 0).unwrap();
        assert!(pool_remove(&mut pool, Side::Short, 1, 1, 0).is_err());
    }

    // ---------------- §17.1 #3 skew price wrapper ----------------

    #[test]
    fn skew_price_wrapper() {
        let p = PoolParams::production();
        assert_eq!(skew_price(75_000_000, 0, 1, &PoolParams::neutral()).unwrap(), 75_000_000);
        // 1k contracts of average skew = 4 bps with the default scale
        let buy = skew_price(75_000_000, 0, 2_000_000_000, &p).unwrap();
        assert_eq!(buy, 75_000_000 + 30_000);
        let sell = skew_price(75_000_000, 2_000_000_000, -2_000_000_000, &p).unwrap();
        assert_eq!(sell, 75_000_000 + 30_000);
        // A clamped sell at a 1 unit oracle stays positive; 0 is refused
        assert_eq!(skew_price(1, 0, -1, &p).unwrap(), 1);
        let mut wide = p;
        wide.max_impact_bps = 10_000;
        assert_eq!(skew_price(100, -1_000_000_000_000, -1, &wide).unwrap(), 59);
        assert!(skew_price(100, -100_000_000_000_000, -1, &wide).is_err());
    }

    // ---------------- §17.1 #4 fees ----------------

    #[test]
    fn fee_distribution_at_below_and_above_target() {
        let mut params = PoolParams::production();
        params.insurance_target = 1_000;
        params.treasury_fee_bps = 1_000;
        for (ins0, x, ins1, cash1, tre1) in [
            (0u64, 100u64, 90u64, 0u64, 10u64), // below
            (910, 100, 1_000, 0, 10),          // lands on the target
            (950, 100, 1_000, 40, 10),         // overflows to cash
            (1_000, 100, 1_000, 90, 10),       // at the target
            (2_000, 100, 2_000, 90, 10),       // above (target lowered)
        ] {
            let mut m = Market { insurance_fund: ins0, ..oil() };
            let mut pool = seeded(0, params);
            let before = bucket_total(&pool, &m);
            distribute_fee(&mut m, &mut pool, x).unwrap();
            assert_eq!((m.insurance_fund, pool.cash, pool.treasury), (ins1, cash1, tre1));
            assert_eq!(bucket_total(&pool, &m), before + x as u128);
        }
        // Neutral: everything to insurance
        let mut m = oil();
        let mut pool = seeded(0, PoolParams::neutral());
        distribute_fee(&mut m, &mut pool, 12_345).unwrap();
        assert_eq!((m.insurance_fund, pool.cash, pool.treasury), (12_345, 0, 0));
    }

    // ---------------- §17.1 #5 deficit waterfall ----------------

    fn waterfall_pool(ins_cap: u64, lp_bps: u16) -> LpPool {
        let mut params = PoolParams::production();
        params.insurance_claim_cap_per_window = ins_cap;
        params.lp_loss_cap_window_bps = lp_bps;
        seeded(1_000_000, params)
    }

    #[test]
    fn deficit_runs_insurance_then_lp_cap_then_pending() {
        let key = Pubkey::new_unique();
        // Insurance covers it all
        let mut m = Market { insurance_fund: 500, ..oil() };
        let mut pool = waterfall_pool(0, 500);
        let total = bucket_total(&pool, &m);
        assert_eq!(absorb_deficit(&mut m, &mut pool, key, 300, Side::Long, 1_000_000, 100).unwrap(), (300, 0, 0));
        assert_eq!((m.insurance_fund, pool.cash), (200, 1_000_300));
        assert_eq!(bucket_total(&pool, &m), total); // insurance → cash only
        assert!(!pool.adl_required());
        // Insurance exhausted, LP cap 5% of 1_000_000 = 50_000 takes the next part
        assert_eq!(absorb_deficit(&mut m, &mut pool, key, 30_000, Side::Long, 1_000_000, 200).unwrap(), (200, 29_800, 0));
        assert_eq!(m.insurance_fund, 0);
        // LP cap exhausted: the rest is pending on the bankrupt side
        assert_eq!(absorb_deficit(&mut m, &mut pool, key, 30_000, Side::Short, 1_000_000, 300).unwrap(), (0, 20_200, 9_800));
        assert_eq!((pool.pending_bad_debt_long, pool.pending_bad_debt_short), (0, 9_800));
        assert_eq!((pool.pending_since_long, pool.pending_since_short), (0, 300));
        assert!(pool.adl_required());
        assert_eq!(pool.bad_debt_cum, 60_300);
        assert_eq!((pool.lp_absorbed_window, pool.ins_paid_window), (50_000, 500));
    }

    #[test]
    fn insurance_claim_cap_is_per_window() {
        let key = Pubkey::new_unique();
        let mut m = Market { insurance_fund: 10_000, ..oil() };
        let mut pool = waterfall_pool(1_000, 0);
        // Two deficits in one window share one insurance cap
        assert_eq!(absorb_deficit(&mut m, &mut pool, key, 700, Side::Long, 1_000_000, 10).unwrap(), (700, 0, 0));
        assert_eq!(absorb_deficit(&mut m, &mut pool, key, 700, Side::Long, 1_000_000, 20).unwrap(), (300, 0, 400));
        // A deficit after LOSS_WINDOW_SECS rolls it
        let later = 10 + LOSS_WINDOW_SECS;
        assert_eq!(absorb_deficit(&mut m, &mut pool, key, 700, Side::Long, 1_000_000, later).unwrap(), (700, 0, 0));
        assert_eq!(pool.loss_window_start, later);
        assert_eq!(pool.ins_paid_window, 700);
    }

    #[test]
    fn two_deficits_share_one_lp_cap_and_a_later_one_rolls_it() {
        let key = Pubkey::new_unique();
        let mut m = oil();
        let mut pool = waterfall_pool(0, 500);
        assert_eq!(absorb_deficit(&mut m, &mut pool, key, 40_000, Side::Long, 1_000_000, 1_000).unwrap(), (0, 40_000, 0));
        // Window NAV is fixed at the window start, even if NAV has fallen since
        assert_eq!(absorb_deficit(&mut m, &mut pool, key, 40_000, Side::Long, 960_000, 2_000).unwrap(), (0, 10_000, 30_000));
        assert_eq!(pool.loss_window_nav, 1_000_000);
        // Just before the end of the window: still shared
        assert_eq!(
            absorb_deficit(&mut m, &mut pool, key, 1, Side::Long, 1_000_000, 1_000 + LOSS_WINDOW_SECS - 1).unwrap(),
            (0, 0, 1)
        );
        // At the end it rolls, measured against the NAV passed now
        assert_eq!(
            absorb_deficit(&mut m, &mut pool, key, 60_000, Side::Long, 900_000, 1_000 + LOSS_WINDOW_SECS).unwrap(),
            (0, 45_000, 15_000)
        );
        assert_eq!(pool.loss_window_nav, 900_000);
        // A negative NAV opens a window with no LP room
        let mut pool = waterfall_pool(0, 500);
        assert_eq!(absorb_deficit(&mut m, &mut pool, key, 5, Side::Long, -100, 1).unwrap(), (0, 0, 5));
        assert_eq!(pool.loss_window_nav, 0);
    }

    #[test]
    fn roll_loss_window_is_idempotent_within_an_instruction() {
        let mut pool = waterfall_pool(0, 500);
        roll_loss_window(&mut pool, 1_000, 50);
        pool.ins_paid_window = 7; // stage 2's keeper top-up
        roll_loss_window(&mut pool, 2_000, 50);
        assert_eq!((pool.loss_window_start, pool.loss_window_nav, pool.ins_paid_window), (50, 1_000, 7));
    }

    #[test]
    fn pending_since_is_debt_weighted() {
        let key = Pubkey::new_unique();
        let mut m = oil();
        let mut pool = waterfall_pool(0, 0); // everything goes to pending
        absorb_deficit(&mut m, &mut pool, key, 10, Side::Long, 0, 1_000).unwrap();
        assert_eq!(pool.pending_since_long, 1_000);
        // Large after small: almost now
        absorb_deficit(&mut m, &mut pool, key, 1_000_000, Side::Long, 0, 1_000 + 86_400).unwrap();
        assert_eq!(pool.pending_since_long, 1_000 + 86_400 - 1); // floor((1000·10 + 87400·1e6)/1000010)
        // Dust barely moves it
        let before = pool.pending_since_long;
        absorb_deficit(&mut m, &mut pool, key, 1, Side::Long, 0, before + 86_400).unwrap();
        assert_eq!(pool.pending_since_long, before);
    }

    /// D36: each side has its own clock. Fresh debt on one side starts at
    /// `now` whatever the other side's age, and repaying one side to 0
    /// resets only that side's clock.
    #[test]
    fn pending_clocks_are_per_side() {
        let key = Pubkey::new_unique();
        let mut m = oil();
        let mut pool = waterfall_pool(0, 0);
        let t0 = 1_000;
        absorb_deficit(&mut m, &mut pool, key, 1_000_000, Side::Long, 0, t0).unwrap();
        // 23.5 h later a short deficit: its own clock starts now (a shared
        // clock would have weighted it to ~t0 + 3.9 h)
        let t1 = t0 + 84_600;
        absorb_deficit(&mut m, &mut pool, key, 200_000, Side::Short, 0, t1).unwrap();
        assert_eq!((pool.pending_since_long, pool.pending_since_short), (t0, t1));
        assert_eq!(pool.pending_of(Side::Short), (200_000, t1));
        // Haircuts repay the longs in full: only the long clock resets
        pool.cash = 10_000_000;
        pay_profit(&mut pool, &view(0), Side::Short, 1_000_000, false).unwrap();
        assert_eq!((pool.pending_bad_debt_long, pool.pending_since_long, pool.pending_since_short), (0, 0, t1));
        // recover_pending likewise
        pool.pending_bad_debt_long = 5;
        pool.pending_since_long = 9;
        recover_pending(&mut pool, 5);
        assert_eq!((pool.pending_since_long, pool.pending_since_short), (0, t1));
        recover_pending(&mut pool, 200_000);
        assert_eq!((pool.pending_bad_debt_short, pool.pending_since_short), (0, 0));
    }

    // ---------------- §17.1 #6 quote / apply ----------------

    fn view(u: i128) -> PoolView {
        PoolView { u, ..Default::default() }
    }

    #[test]
    fn solvency_haircut_only_when_u_exceeds_cash() {
        let pool = seeded(1_000, PoolParams::neutral());
        assert_eq!(quote_profit(&pool, &view(1_000), Side::Long, 100, false).unwrap().paid, 100);
        let q = quote_profit(&pool, &view(4_000), Side::Long, 100, false).unwrap();
        assert_eq!((q.paid, q.solvency_hc), (25, 75));
    }

    #[test]
    fn bad_debt_haircut_takes_the_opposite_side() {
        let mut pool = seeded(1_000, PoolParams::neutral());
        pool.pending_bad_debt_long = 30;
        pool.pending_since_long = 77;
        // A long winner does not repay bankrupt longs
        assert_eq!(quote_profit(&pool, &view(0), Side::Long, 100, false).unwrap().bad_debt_hc, 0);
        // A short winner does
        let q = quote_profit(&pool, &view(0), Side::Short, 100, false).unwrap();
        assert_eq!((q.paid, q.bad_debt_hc), (70, 30));
        // Quoting has no side effects
        assert_eq!((pool.pending_bad_debt_long, pool.cash), (30, 1_000));
        let total = bucket_total(&pool, &oil());
        apply_payout(&mut pool, Side::Short, &q).unwrap();
        assert_eq!((pool.pending_bad_debt_long, pool.cash, pool.bad_debt_recovered_cum), (0, 930, 30));
        assert_eq!(pool.pending_since_long, 0); // the long pending is 0 now
        assert_eq!(bucket_total(&pool, &oil()), total - q.paid as u128);
        // Partial recovery keeps the clock
        let mut pool = seeded(1_000, PoolParams::neutral());
        pool.pending_bad_debt_short = 300;
        pool.pending_since_short = 77;
        pay_profit(&mut pool, &view(0), Side::Long, 100, false).unwrap();
        assert_eq!((pool.pending_bad_debt_short, pool.pending_since_short, pool.cash), (200, 77, 1_000));
    }

    #[test]
    fn cash_shortfall_fails_closes_and_shorts_liquidations() {
        let mut pool = seeded(40, PoolParams::neutral());
        assert!(quote_profit(&pool, &view(0), Side::Long, 100, false).is_err());
        let q = pay_profit(&mut pool, &view(0), Side::Long, 100, true).unwrap();
        assert_eq!((q.paid, q.cash_hc, pool.cash), (40, 60, 0));
    }

    // ---------------- §10.1 / §10.3 / §17.1 #13, #14 ----------------

    #[test]
    fn recover_pending_takes_longs_first() {
        let mut pool = seeded(0, PoolParams::neutral());
        pool.pending_bad_debt_long = 10;
        pool.pending_bad_debt_short = 20;
        pool.pending_since_long = 5;
        pool.pending_since_short = 6;
        assert_eq!(recover_pending(&mut pool, 15), 15);
        // The long side is repaid, so only its clock resets (D36)
        assert_eq!((pool.pending_bad_debt_long, pool.pending_bad_debt_short, pool.pending_since_long, pool.pending_since_short), (0, 15, 0, 6));
        assert_eq!(recover_pending(&mut pool, 100), 15);
        assert_eq!((pool.pending_bad_debt_short, pool.pending_since_short, pool.bad_debt_recovered_cum), (0, 0, 30));
        assert_eq!(recover_pending(&mut pool, 100), 0);
    }

    #[test]
    fn vesting_is_linear_and_merges() {
        let mut params = PoolParams::neutral();
        params.withdrawal_delay_secs = 100;
        let mut pool = seeded(0, params);
        add_vesting(&mut pool, 1_000, 0).unwrap();
        assert_eq!((pool.vest_amount, pool.vest_start, pool.vest_end), (1_000, 0, 100));
        assert_eq!([pool.unvested(0), pool.unvested(25), pool.unvested(50), pool.unvested(100)], [1_000, 750, 500, 0]);
        // A second sweep merges what is still unvested; the schedule ends at
        // the amount-weighted end: floor((500·50 + 200·100) / 700) = 64 s on
        assert_eq!(pool.unvested(50), 500);
        add_vesting(&mut pool, 200, 50).unwrap();
        assert_eq!((pool.vest_amount, pool.vest_start, pool.vest_end), (700, 50, 114));
        assert_eq!(pool.unvested(50), 700);
        // Delay 0 still vests over one second
        let mut pool = seeded(0, PoolParams::neutral());
        add_vesting(&mut pool, 9, 10).unwrap();
        assert_eq!((pool.unvested(10), pool.unvested(11)), (9, 0));
    }

    /// Review finding: dust donations cannot keep value unvested. $1,000
    /// vesting over 100 s with a 1-unit donation every 10 s is still fully
    /// vested (but for the dust) shortly after the original end; restarting
    /// the clock left about 39% of it unvested at 100 s.
    #[test]
    fn dust_donations_do_not_restart_vesting() {
        let mut params = PoolParams::neutral();
        params.withdrawal_delay_secs = 100;
        let mut pool = seeded(0, params);
        add_vesting(&mut pool, 1_000_000_000, 0).unwrap();
        for t in (10..100).step_by(10) {
            let before = pool.unvested(t);
            add_vesting(&mut pool, 1, t).unwrap();
            // A merge never changes what is unvested now, but for the new unit
            assert_eq!(pool.unvested(t), before + 1);
        }
        assert_eq!(pool.vest_end, 100);
        assert_eq!(pool.unvested(100), 0);
        // and a 1-unit donation every second cannot hold the end in place
        let mut pool = seeded(0, params);
        add_vesting(&mut pool, 1_000_000_000, 0).unwrap();
        for t in 1..100 {
            add_vesting(&mut pool, 1, t).unwrap();
        }
        assert_eq!(pool.vest_end, 100);
    }

    #[test]
    fn a_sweep_does_not_move_nps_and_widens_mint_minus_redeem() {
        let mut params = PoolParams::neutral();
        params.withdrawal_delay_secs = 345_600;
        let m = oil();
        let mut pool = seeded(1_000_000_000_000, params);
        let v0 = pool_view(&pool, &m, 75_000_000, 1_000, 5_000).unwrap();
        // Sweep 10k into cash and vest it
        pool.cash += 10_000_000_000;
        add_vesting(&mut pool, 10_000_000_000, 1_000).unwrap();
        let v1 = pool_view(&pool, &m, 75_000_000, 1_000, 5_000).unwrap();
        assert_eq!(v1.nps, v0.nps);
        assert_eq!(v1.nav_redeem, v0.nav_redeem);
        assert_eq!(v1.nav_mint, v0.nav_mint + 10_000_000_000);
        // Half way through, half is counted
        let v2 = pool_view(&pool, &m, 75_000_000, 1_000 + 172_800, 5_000).unwrap();
        assert_eq!(v2.nav_redeem, v0.nav_redeem + 5_000_000_000);
        assert_eq!((v2.nav_mint - v2.nav_redeem) - (v0.nav_mint - v0.nav_redeem), 5_000_000_000);
    }

    #[test]
    fn touch_peak_raises_and_resets() {
        let mut pool = seeded(0, PoolParams::neutral());
        let v = |nps| PoolView { nps, ..Default::default() };
        touch_peak(&mut pool, &v(1_000_000), 10);
        assert_eq!((pool.nps_peak, pool.nps_peak_at), (1_000_000, 10));
        touch_peak(&mut pool, &v(900_000), 20); // a drawdown keeps the peak
        assert_eq!((pool.nps_peak, pool.nps_peak_at), (1_000_000, 10));
        touch_peak(&mut pool, &v(1_100_000), 30);
        assert_eq!((pool.nps_peak, pool.nps_peak_at), (1_100_000, 30));
        touch_peak(&mut pool, &v(500_000), 30 + DD_WINDOW_SECS - 1);
        assert_eq!(pool.nps_peak, 1_100_000);
        touch_peak(&mut pool, &v(500_000), 30 + DD_WINDOW_SECS); // a week old: resets
        assert_eq!((pool.nps_peak, pool.nps_peak_at), (500_000, 30 + DD_WINDOW_SECS));
        // dd_bps in the view follows the stored peak
        let mut q = seeded(900_000, PoolParams::neutral());
        q.lp_shares = 1_000_000;
        q.nps_peak = 1_000_000;
        let dd = pool_view(&q, &oil(), 1, 0, 0).unwrap().dd_bps;
        assert_eq!(dd, 1_000);
    }

    // ---------------- §17.1 #12 bucket steps (helper level) ----------------

    #[test]
    fn assert_backed_compares_the_bucket_sum() {
        let m = Market { insurance_fund: 5, ..oil() };
        let pool = LpPool { free_collateral: 1, position_collateral: 2, cash: 3, treasury: 4, ..Default::default() };
        assert_eq!(bucket_total(&pool, &m), 15);
        assert!(assert_backed(&pool, &m, 15).is_ok());
        assert!(assert_backed(&pool, &m, 16).is_ok());
        assert!(assert_backed(&pool, &m, 14).is_err());
        let full = LpPool { free_collateral: u64::MAX, cash: u64::MAX, ..Default::default() };
        assert!(assert_backed(&full, &m, u64::MAX).is_err()); // u128 sum, no wrap
    }

    #[test]
    fn credit_insurance_and_waterfall_are_zero_sum_moves() {
        let key = Pubkey::new_unique();
        let mut m = Market { insurance_fund: 100, ..oil() };
        let mut pool = waterfall_pool(0, 500);
        pool.params.insurance_target = 150;
        let t0 = bucket_total(&pool, &m);
        credit_insurance(&mut m, &mut pool, 80).unwrap();
        assert_eq!((m.insurance_fund, pool.cash), (150, 1_000_030));
        assert_eq!(bucket_total(&pool, &m), t0 + 80);
        let t1 = bucket_total(&pool, &m);
        absorb_deficit(&mut m, &mut pool, key, 1_000, Side::Long, 1_000_000, 1).unwrap();
        assert_eq!(bucket_total(&pool, &m), t1);
    }
}
