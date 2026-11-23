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
