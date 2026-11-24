//! Counters and time series for the report.

use crate::ledger::Route;
use serde_json::{json, Value};
use crate::support::Pubkey;
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Default, Clone, Copy)]
pub struct Volume {
    pub fills: u64,
    pub notional: u128,
}

#[derive(Default)]
pub struct Metrics {
    pub volume: BTreeMap<Route, Volume>,
    pub closes: u64,
    pub close_fees: u64,
    pub close_bad_debt: u64,
    pub liquidations: u64,
    pub liquidations_by_route: BTreeMap<Route, u64>,
    pub liquidation_rewards: u64,
    pub liquidation_failures: BTreeMap<String, u64>,
    pub bad_debt_events: Vec<Value>,
    pub liquidation_events: Vec<Value>,
    pub uncovered_bad_debt: u64,
    pub funding_updates: Vec<Value>,
    /// Net funding the vault kept (paid by one side minus received by the other) over all updates
    pub funding_imbalance: i128,
    pub no_liquidity: u64,
    pub matches: u64,
    pub match_skips: BTreeMap<String, u64>,
    pub expired_orders_closed: u64,
    pub orders_placed: u64,
    pub orders_cancelled: u64,
    pub quote_posts: u64,
    pub quote_updates: u64,
    pub samples: Vec<Value>,
    pub insurance_series: Vec<(i64, u64)>,
    pub leverage_clamps: u64,
    pub final_vault: u64,
    pub final_insurance: u64,
    pub unwind_failures: u64,
    pub first_insolvent: Option<Value>,
    /// Engine paths and expected rejections reached (coverage)
    pub paths: BTreeMap<String, u64>,
    /// Transactions not sent because the model predicts a rejection the label never sends
    pub skips: BTreeMap<String, u64>,
    /// Funding periods beyond the catch-up cap, from FundingUpdated events (must stay 0)
    pub funding_periods_dropped: u64,
    pub unsplit_probes: u64,
    /// Order pairs whose predicted-failing match was sent once
    pub probed_pairs: HashSet<(Pubkey, Pubkey)>,
    /// Position -> tick it was last probed with liquidate
    pub last_probe: HashMap<usize, u64>,
    /// Free collateral left in perps-core after unwind
    pub stuck_free: u64,
    /// Probes by name (invariant 13): sent and checked, or why not
    pub probes: BTreeMap<String, ProbeStat>,
}

/// One probe's tally: how often its on-chain outcome was checked, how often
/// it was the one the spec requires, and why it was not reached otherwise.
#[derive(Default, Clone)]
pub struct ProbeStat {
    pub checked: u64,
    pub ok: u64,
    pub not_reached: BTreeMap<String, u64>,
    /// The last outcome seen, for the report
    pub last: String,
}

impl Metrics {
    pub fn record_volume(&mut self, route: Route, notional: u64) {
        let v = self.volume.entry(route).or_default();
        v.fills += 1;
        v.notional += notional as u128;
    }

    pub fn record_close(&mut self, fee: u64, deficit: u64) {
        self.closes += 1;
        self.close_fees += fee;
        self.close_bad_debt += deficit;
    }

    pub fn volume_json(&self) -> Value {
        let mut out = serde_json::Map::new();
        for (route, v) in &self.volume {
            out.insert(route.name().into(), json!({ "fills": v.fills, "notional_usdc": v.notional as f64 / 1e6 }));
        }
        Value::Object(out)
    }
}

/// The LP pool's side of the report (docs/lp-pool.md §16.7).
#[derive(Default)]
pub struct PoolStats {
    /// Hourly: NAV, per-share NAV, drawdown, capital use
    pub series: Vec<Value>,
    pub peak_dd_bps: u64,
    /// Highest net notional over the net cap's capital (k_net·capNAV) and over B
    pub peak_net_over_cap_nav: f64,
    pub peak_net_over_b: f64,
    pub peak_net_notional: u128,
    pub min_nav_redeem: Option<i128>,
    /// Haircuts by kind (USDC) and count
    pub haircuts: BTreeMap<String, (u64, u64)>,
    /// PoolCashInsufficient rejections by instruction
    pub cash_insufficient: BTreeMap<String, u64>,
    /// ADL by mode and triggers
    pub adl_by_kind: BTreeMap<String, u64>,
    pub adl_events: Vec<Value>,
    /// Deficits booked (BadDebt), and the part beyond insurance
    pub deficits: Vec<Value>,
    pub deficit_total: u64,
    pub deficit_beyond_insurance: u64,
    pub deficit_lp_absorbed: u64,
    pub deficit_pending: u64,
    pub written_off: u64,
    pub swept: u64,
    pub settle_pnl: (u64, u64),
    /// Per weekly close: net, capital, the weekend criterion
    pub weekly_closes: Vec<Value>,
    pub open_fees: u64,
    pub house_opens: u64,
    pub liquidation_stages: BTreeMap<String, u64>,
    pub lp_flows: Vec<Value>,
    /// The pool once everything is closed, before the LPs redeem
    pub before_redeem: Option<Value>,
    /// Counters at the start of the current week (the last weekly close), for its record
    pub week_base: Option<WeekBase>,
    /// Peak drawdown since the last weekly close
    pub week_peak_dd_bps: u64,
    /// The weekend bound `G·net_notional ≤ α·B` step by step (D43, §17.4)
    pub bound: BoundTracker,
}

/// One look at the weekend bound: before or after a transaction, or at a tick.
#[derive(Clone, Copy, Debug, Default)]
pub struct BoundObs {
    pub px: u64,
    pub net_abs: u128,
    pub nn: u128,
    pub b: u128,
    pub g: u64,
    pub alpha: u32,
    pub bad_debt_cum: u64,
    pub settled: bool,
    pub on: bool,
    pub holds: bool,
    /// Pool cash: a "tick" step is a pure price move only if it is unchanged
    pub cash: u64,
    /// `NAV_redeem` with every trader loss counted (h = 100%): the mark NAV
    /// less the rounding slack. `B` is this less `hc` (when positive).
    pub nav_full: i128,
    /// The traders' net loss `NAV_redeem` leaves uncounted, `(1 − h)·loss`:
    /// what the redeem haircut takes off `B` (§2.2)
    pub hc: i128,
}

impl BoundObs {
    /// `G·net_notional − α·B` (bps·micro-USDC): the carried excess times `G`.
    pub fn excess(&self) -> i128 {
        (self.nn * self.g as u128) as i128 - (self.b * self.alpha as u128) as i128
    }
}

/// Why `G·net_notional/B` rose past `α` since it last held (§3.1, D43). The
/// documented exceptions are the only explanations the weekly-close check
/// (invariant 15) accepts for a positive `carried_excess`.
#[derive(Clone, Debug, Default)]
pub struct BoundTracker {
    pub last: Option<BoundObs>,
    /// Documented exceptions seen since the bound last held, with counts
    pub reasons: BTreeMap<String, u64>,
    /// Steps whose worsening of a broken bound the documented exceptions do
    /// not cover, with the uncovered part (`G·nn − α·B` units, summed); any
    /// entry makes a weekly close with an excess fatal (invariant 15)
    pub other: BTreeMap<String, u128>,
    /// Every step that broke the bound, by reason (the report's totals)
    pub breaks: BTreeMap<String, u64>,
}


/// Rounding allowed per step in the weekend-bound tracker, micro-USDC of B
/// (the slack's 2 units per position, the floors of U and the caps).
pub const BOUND_ROUNDING: u128 = 64;

impl BoundTracker {
    /// One step: from the last observation to `cur`. A step
    /// that raises the excess `E = G·nn − α·B` above `max(E_prev, 0)` is
    /// covered, by size, by the documented exceptions (§3.1) it shows:
    /// - `settled`, `g_raised` (`update_market_config` raising a derived G),
    ///   `params_tightened` (`apply_pool_params`: W turned on, α lowered or
    ///   `weekend_gap_bps` raised over existing net): the whole step;
    /// - `price_rise`: a pure price move (a "tick": net and cash unchanged)
    ///   upward, the whole step;
    /// - `liquidation`: a liquidation that raised |net|, the whole step;
    /// - `redeem_haircut_payout`: `α·Δhc`, the part of the `B` drop the
    ///   redeem haircut takes as the traders' net loss grows (a profit paid
    ///   while they are net losing);
    /// - `deficit`: `α·min(fall of the full-count NAV, Δbad_debt_cum)`;
    /// - `rounding`: `α·BOUND_ROUNDING + G·2` per step.
    ///
    /// What is left goes to `other`, with its size, and fails the weekly close.
    pub fn observe(&mut self, label: &str, cur: BoundObs) {
        let t = self;
        let prev = t.last.replace(cur);
        if cur.holds {
            t.reasons.clear();
            t.other.clear();
            return;
        }
        let Some(prev) = prev else {
            *t.other.entry(format!("first look: {}", label)).or_default() += cur.excess().max(0) as u128;
            return;
        };
        let (e0, e1) = (if prev.holds { 0 } else { prev.excess().max(0) }, cur.excess());
        if e1 <= e0 {
            return;
        }
        let worse = e1 - e0;
        let alpha = cur.alpha as i128;
        let mut why: Vec<&'static str> = Vec::new();
        let mut whole = false;
        if cur.settled {
            why.push("settled");
            whole = true;
        }
        if cur.g > prev.g {
            why.push(if label.starts_with("apply_pool_params") { "params_tightened" } else { "g_raised" });
            whole = true;
        }
        if cur.alpha < prev.alpha || (cur.on && !prev.on) {
            why.push("params_tightened");
            whole = true;
        }
        let pure_tick = label == "tick" && cur.net_abs == prev.net_abs && cur.cash == prev.cash && cur.bad_debt_cum == prev.bad_debt_cum;
        if pure_tick && cur.px > prev.px {
            why.push("price_rise");
            whole = true;
        }
        if cur.net_abs > prev.net_abs && label.starts_with("liquidate") {
            why.push("liquidation");
            whole = true;
        }
        let mut covered: i128 = 0;
        if !whole {
            // B = nav_full − hc (both ≥ 0 here): split its fall
            let hc_part = alpha * (cur.hc - prev.hc).max(0);
            if hc_part > 0 {
                why.push("redeem_haircut_payout");
                covered += hc_part;
            }
            let nav_fall = (prev.nav_full - cur.nav_full).max(0);
            let debt = cur.bad_debt_cum.saturating_sub(prev.bad_debt_cum) as i128;
            if nav_fall > 0 && debt > 0 {
                why.push("deficit");
                covered += alpha * nav_fall.min(debt);
            }
            let round = alpha * BOUND_ROUNDING as i128 + cur.g as i128 * 2;
            covered += round;
            if why.is_empty() && worse <= round {
                why.push("rounding");
            }
        }
        let uncovered = if whole { 0 } else { (worse - covered).max(0) };
        if uncovered > 0 {
            *t.other.entry(label.to_string()).or_default() += uncovered as u128;
            *t.breaks.entry(format!("other:{}", label)).or_default() += 1;
        }
        for w in why {
            *t.reasons.entry(w.to_string()).or_default() += 1;
            *t.breaks.entry(w.to_string()).or_default() += 1;
        }
    }
}

/// The counters a weekly close's record takes the differences of.
#[derive(Clone, Copy, Default)]
pub struct WeekBase {
    pub liquidations: u64,
    pub adl_mode0: u64,
    pub adl_mode1: u64,
    pub haircuts: u64,
    pub deficits: u64,
    pub beyond_insurance: u64,
    pub lp_flows: u64,
    pub nav_redeem: i128,
}

impl PoolStats {
    pub fn haircut(&mut self, kind: &str, amount: u64) {
        if amount > 0 {
            let e = self.haircuts.entry(kind.to_string()).or_default();
            e.0 += amount;
            e.1 += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const C6: u128 = 1_000_000;

    /// An observation at G 20%, α 10%: `nn` and `b` in USDC.
    fn obs(px: u64, nn: u128, b: u128, nav_full: i128, hc: i128) -> BoundObs {
        let (g, alpha) = (2_000u64, 1_000u32);
        let (nn, b) = (nn * C6, b * C6);
        BoundObs {
            px, net_abs: 10_000 * C6, nn, b, g, alpha, bad_debt_cum: 0, settled: false, on: true,
            holds: nn * g as u128 <= b * alpha as u128, cash: 1_000, nav_full: nav_full * C6 as i128, hc: hc * C6 as i128,
        }
    }

    /// Review finding (invariant 15): once one documented reason is seen, a
    /// later undocumented worsening must still fail the close; coverage is
    /// by size, not by presence.
    #[test]
    fn an_uncovered_step_is_kept_after_a_documented_one() {
        let mut t = BoundTracker::default();
        t.observe("tick", obs(75, 500_000, 1_000_000, 1_000_000, 0));
        assert!(t.reasons.is_empty() && t.other.is_empty());
        // A pure 1-unit price rise breaks it: documented
        t.observe("tick", obs(76, 500_010, 1_000_000, 1_000_000, 0));
        assert_eq!(t.reasons.keys().collect::<Vec<_>>(), ["price_rise"]);
        assert!(t.other.is_empty());
        // A close that loses $1,000 of full-count NAV with no deficit: uncovered
        t.observe("close_position", obs(76, 500_010, 999_000, 999_000, 0));
        assert_eq!(t.other.get("close_position"), Some(&(1_000 * 1_000 * C6 - 64_000 - 4_000)));
        // Restoring the bound clears the episode
        t.observe("lp_deposit", obs(76, 500_010, 2_000_000, 2_000_000, 0));
        assert!(t.reasons.is_empty() && t.other.is_empty());
    }

    /// The redeem haircut covers only what it took (`α·Δhc`), and a deficit
    /// only up to the bad debt booked.
    #[test]
    fn payouts_and_deficits_are_covered_by_size() {
        let start = || {
            let mut t = BoundTracker::default();
            t.observe("tick", obs(75, 500_000, 1_000_000, 1_100_000, 100_000));
            t.observe("tick", obs(76, 500_100, 1_000_000, 1_100_000, 100_000));
            t
        };
        // A payout while traders are net losing: B falls by the haircut's growth
        let mut t = start();
        t.observe("close_position", obs(76, 500_100, 995_000, 1_100_000, 105_000));
        assert!(t.reasons.contains_key("redeem_haircut_payout") && t.other.is_empty(), "{:?}", t.other);
        // The same drop with $2k of it out of the full-count NAV: uncovered
        let mut t = start();
        t.observe("close_position", obs(76, 500_100, 995_000, 1_098_000, 103_000));
        assert!(t.other.contains_key("close_position"));
        // A deficit covers a NAV fall up to the bad debt it booked
        let mut t = start();
        let mut o = obs(76, 500_100, 998_000, 1_098_000, 100_000);
        o.bad_debt_cum = 2_000_000_000;
        t.observe("liquidate", o);
        assert!(t.reasons.contains_key("deficit") && t.other.is_empty());
        let mut t = start();
        o.bad_debt_cum = 1_000_000_000;
        t.observe("liquidate", o);
        assert!(t.other.contains_key("liquidate"));
        // A sub-rounding step is rounding
        let mut t = start();
        let mut o = obs(76, 500_100, 1_000_000, 1_100_000, 100_000);
        o.b -= 10;
        o.nav_full -= 10;
        t.observe("close_position", o);
        assert!(t.reasons.contains_key("rounding") && t.other.is_empty());
    }

    /// Review finding: a tick is a price move only when nothing else
    /// changed; params that tighten the bound over existing net are their
    /// own documented exception.
    #[test]
    fn ticks_must_be_pure_and_params_are_attributed() {
        let mut t = BoundTracker::default();
        t.observe("tick", obs(75, 500_000, 1_000_000, 1_000_000, 0));
        let mut o = obs(76, 500_000, 990_000, 990_000, 0);
        o.cash -= 1;
        t.observe("tick", o);
        assert!(t.other.contains_key("tick") && !t.reasons.contains_key("price_rise"));
        // α lowered from 20% to 10% by apply_pool_params over $500k of net and B $1M
        let mut t = BoundTracker::default();
        let mut o = obs(75, 500_000, 1_000_000, 1_000_000, 0);
        o.alpha = 2_000;
        o.nn += 1_000 * C6;
        o.holds = true;
        t.observe("tick", o);
        let mut o2 = obs(75, 501_000, 1_000_000, 1_000_000, 0);
        o2.alpha = 1_000;
        t.observe("apply_pool_params", o2);
        assert!(t.reasons.contains_key("params_tightened") && t.other.is_empty());
        // W turned on over existing net
        let mut t = BoundTracker::default();
        let mut o = obs(75, 600_000, 1_000_000, 1_000_000, 0);
        o.on = false;
        o.holds = true;
        t.observe("tick", o);
        t.observe("apply_pool_params", obs(75, 600_000, 1_000_000, 1_000_000, 0));
        assert!(t.reasons.contains_key("params_tightened") && t.other.is_empty());
    }
}
