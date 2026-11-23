//! Counters and time series for the report.

use crate::ledger::Route;
use crate::model::CloseOutcome;
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
    /// The vault as counterparty of unpaired positions (house, or a venue leg whose other leg closed)
    pub peak_unpaired_net: i128,
    pub peak_unpaired_pnl: i128,
    pub peak_unpaired_count: usize,
    pub first_insolvent: Option<Value>,
    /// Engine paths and expected rejections reached (coverage)
    pub paths: BTreeMap<String, u64>,
    /// Transactions not sent because the model predicts a rejection the label never sends
    pub skips: BTreeMap<String, u64>,
    /// Funding periods that elapsed without being charged (one period per crank)
    pub funding_periods_dropped: u64,
    pub unsplit_probes: u64,
    /// Order pairs whose predicted-failing match was sent once
    pub probed_pairs: HashSet<(Pubkey, Pubkey)>,
    /// Position -> tick it was last probed with liquidate
    pub last_probe: HashMap<usize, u64>,
    /// Free collateral left in perps-core after unwind
    pub stuck_free: u64,
}

impl Metrics {
    pub fn record_volume(&mut self, route: Route, notional: u64) {
        let v = self.volume.entry(route).or_default();
        v.fills += 1;
        v.notional += notional as u128;
    }

    pub fn record_close(&mut self, c: &CloseOutcome) {
        self.closes += 1;
        self.close_fees += c.fee;
        self.close_bad_debt += c.deficit;
    }

    pub fn volume_json(&self) -> Value {
        let mut out = serde_json::Map::new();
        for (route, v) in &self.volume {
            out.insert(route.name().into(), json!({ "fills": v.fills, "notional_usdc": v.notional as f64 / 1e6 }));
        }
        Value::Object(out)
    }
}
