//! The engine's economics, written out independently of perps-core's code
//! from the spec (docs/venue-settlement.md sections 2-3) and the instruction
//! handlers' documented behaviour. The simulation predicts every fill, close,
//! liquidation and funding step with these and flags any difference from what
//! the engine did, so a bug in the engine's own helpers cannot hide itself.
//! Nothing here calls perps-core code: the session comes from `schedule`, and
//! only perps-core's account types are used.

use crate::schedule;
use perps_core::state::{Market, Position, Side};

pub const BPS: i128 = 10_000;
pub const ONE: i128 = 1_000_000;

fn ceil_div(a: i128, b: i128) -> i128 {
    (a + b - 1) / b
}

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

pub fn sign(side: Side) -> i128 {
    match side {
        Side::Long => 1,
        Side::Short => -1,
    }
}

pub fn notional(size: u64, price: u64) -> i128 {
    size as i128 * price as i128 / ONE
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

/// Collateral + PnL + accrued funding at `mark`.
pub fn equity(p: &Position, mark: u64, cumulative: i64) -> i128 {
    p.collateral as i128 + pnl(p.side, p.size, p.entry_price, mark) + funding(p.side, p.size, p.last_funding_payment, cumulative)
}

/// Equity over entry notional in basis points, 0 when underwater.
pub fn margin_ratio_bps(p: &Position, mark: u64, cumulative: i64) -> i128 {
    let e = equity(p, mark, cumulative);
    let n = notional(p.size, p.entry_price);
    if e <= 0 || n == 0 {
        0
    } else {
        e * BPS / n
    }
}

pub fn liquidatable(m: &Market, p: &Position, mark: u64, now: i64) -> bool {
    margin_ratio_bps(p, mark, m.cumulative_funding) < margin_ratios(m, now).1 as i128
}

/// Collateral open_position takes: notional at the oracle / leverage, rounded
/// up; None when that is below the initial margin in force.
pub fn open_margin(m: &Market, size: u64, oracle: u64, leverage: u32, now: i64) -> Option<u64> {
    if leverage == 0 || leverage > m.max_leverage {
        return None;
    }
    let n = notional(size, oracle);
    let base = ceil_div(n * 1_000, leverage as i128);
    let (im, _) = margin_ratios(m, now);
    if base < ceil_div(n * im as i128, BPS) {
        return None;
    }
    u64::try_from(base).ok()
}

/// Collateral a venue fill posts for one side (spec 2.2):
/// `max(fill notional / lev, IM x oracle notional) + immediate loss at the oracle`.
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

/// The approval's adverse-only deviation bound (0 = off): a long pays at most
/// `bps` over the oracle, a short sells at most `bps` under it.
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

/// What close_position pays and how the insurance fund moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CloseOutcome {
    pub payout: u64,
    pub fee: u64,
    pub deficit: u64,
    pub fund_after: u64,
    pub uncovered: u64,
    /// pnl + funding - fee, what the engine records as realized
    pub realized: i128,
    pub mark: u64,
}

/// close_position at `mark` (the oracle, or the settlement price once settled).
pub fn close(m: &Market, p: &Position, mark: u64) -> CloseOutcome {
    let fee = notional(p.size, p.entry_price) * m.taker_fee as i128 / BPS;
    let realized = pnl(p.side, p.size, p.entry_price, mark) + funding(p.side, p.size, p.last_funding_payment, m.cumulative_funding) - fee;
    let final_equity = p.collateral as i128 + realized;
    let (payout, deficit) = if final_equity >= 0 { (final_equity, 0) } else { (0, -final_equity) };
    let available = m.insurance_fund as i128 + fee;
    let covered = deficit.min(available);
    CloseOutcome {
        payout: payout as u64,
        fee: fee as u64,
        deficit: deficit as u64,
        fund_after: (available - covered) as u64,
        uncovered: (deficit - covered) as u64,
        realized,
        mark,
    }
}

/// What liquidate pays the liquidator and how the insurance fund moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiquidationOutcome {
    pub reward: u64,
    /// Part of the reward the fund paid because the position's equity could not
    pub top_up: u64,
    pub deficit: u64,
    pub fund_after: u64,
    pub uncovered: u64,
    /// pnl + funding
    pub realized: i128,
}

pub fn liquidation(m: &Market, p: &Position, mark: u64) -> LiquidationOutcome {
    let realized = pnl(p.side, p.size, p.entry_price, mark) + funding(p.side, p.size, p.last_funding_payment, m.cumulative_funding);
    let e = p.collateral as i128 + realized;
    let (remaining, deficit) = if e >= 0 { (e, 0) } else { (0, -e) };
    // The reward is liquidation_fee of the (weekday) maintenance margin on the entry notional
    let target = notional(p.size, p.entry_price) * m.maintenance_margin_ratio as i128 * m.liquidation_fee as i128 / 100_000_000;
    let from_collateral = target.min(remaining);
    let mut fund = m.insurance_fund as i128 + remaining - from_collateral;
    let top_up = (target - from_collateral).min(fund);
    fund -= top_up;
    let covered = deficit.min(fund);
    LiquidationOutcome {
        reward: (from_collateral + top_up) as u64,
        top_up: top_up as u64,
        deficit: deficit as u64,
        fund_after: (fund - covered) as u64,
        uncovered: (deficit - covered) as u64,
        realized,
    }
}

/// update_funding's per-period rate (6 dp). From the code's documentation:
/// a 0.01%-per-period base rate (100 in 6 dp) scaled by the open-interest
/// imbalance (L - S) / (L + S), positive when longs outweigh shorts, rounded
/// toward zero.
pub fn funding_rate(long_oi: u64, short_oi: u64) -> i64 {
    let (l, s) = (long_oi as i128, short_oi as i128);
    if l + s == 0 {
        return 0;
    }
    (100 * (l - s) / (l + s)) as i64
}

/// The step update_funding adds to `cumulative_funding`: one period's rate
/// applied to that period's oracle price, per whole contract in USDC (6 dp),
/// rounded toward zero (Market field docs: "running funding per whole contract,
/// USDC (6 decimals); + = longs paid").
pub fn funding_step(rate: i64, oracle: u64) -> i64 {
    (rate as i128 * oracle as i128 / ONE) as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use perps_core::state::Market;

    fn oil() -> Market {
        Market { max_leverage: 20_000, initial_margin_ratio: 1_000, maintenance_margin_ratio: 500, taker_fee: 5, liquidation_fee: 100, ..Default::default() }
    }

    /// The spec's worked example (2.2): 100 contracts at $75.03 against a $75 oracle, 10x.
    #[test]
    fn spec_worked_example() {
        let m = oil();
        let size = 100 * 1_000_000;
        assert_eq!(venue_margin(&m, Side::Long, size, 75_030_000, 75_000_000, 10_000, 0), Some(753_300_000));
        assert_eq!(venue_margin(&m, Side::Short, size, 75_030_000, 75_000_000, 10_000, 0), Some(750_300_000));
        let long = Position { side: Side::Long, size, entry_price: 75_030_000, collateral: 753_300_000, ..Default::default() };
        let short = Position { side: Side::Short, collateral: 750_300_000, ..long.clone() };
        assert_eq!(close(&m, &long, 78_000_000).payout, 1_046_548_500);
        assert_eq!(close(&m, &short, 78_000_000).payout, 449_548_500);
        // Spec: payouts sum to c_T + c_M - 2 fees, and the fund grows by the fees
        let (a, b) = (close(&m, &long, 78_000_000), close(&m, &short, 78_000_000));
        assert_eq!(a.payout + b.payout, 1_503_600_000 - 7_503_000);
        assert_eq!(a.fee + b.fee, 7_503_000);
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
        // 3:1 long: imbalance 0.5, 0.005% per period
        assert_eq!(funding_rate(3_000_000, 1_000_000), 50);
        assert_eq!(funding_rate(1_000_000, 3_000_000), -50);
        assert_eq!(funding_rate(0, 0), 0);
        assert_eq!(funding_rate(1, 0), 100);
        // 0.005% of $80 per contract = $0.004
        assert_eq!(funding_step(50, 80_000_000), 4_000);
        assert_eq!(funding_step(-50, 80_000_000), -4_000);
    }

    #[test]
    fn liquidation_splits_like_the_spec() {
        let m = Market { insurance_fund: 1_000, ..oil() };
        // Underwater: no remaining equity, the fund pays the reward and absorbs what it can
        let p = Position { side: Side::Long, size: 100 * 1_000_000, entry_price: 75_000_000, collateral: 750_000_000, ..Default::default() };
        let out = liquidation(&m, &p, 60_000_000);
        assert_eq!(out.deficit, 750_000_000);
        assert_eq!(out.reward, 1_000);
        assert_eq!(out.top_up, 1_000);
        assert_eq!(out.uncovered, 750_000_000);
        assert_eq!(out.fund_after, 0);
    }
}
