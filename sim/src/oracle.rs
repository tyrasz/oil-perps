//! The OIL price: a geometric random walk (about 40% annualised volatility)
//! with rare 2-5% jumps, frozen while the CME session is closed and gapping at
//! the reopen (by the scenario's ranges). Occasionally the feed goes quiet (stale) or publishes a wide
//! confidence interval, as Pyth does.

use rand::rngs::StdRng;
use rand::Rng;

const YEAR_SECS: f64 = 365.0 * 86_400.0;

pub struct OracleConfig {
    pub annual_vol: f64,
    /// Expected jumps per day of trading
    pub jumps_per_day: f64,
    /// Chance per tick that the feed stops publishing, for 1-3 ticks
    pub outage_per_tick: f64,
    /// Chance per tick of a confidence interval over the market's 1% limit
    pub wide_conf_per_tick: f64,
    /// Weekend reopen gap size range, either sign
    pub weekend_gap: (f64, f64),
    /// Daily-break reopen gap size range, either sign; None: a 0.25% sd move
    pub daily_gap: Option<(f64, f64)>,
}

impl Default for OracleConfig {
    fn default() -> Self {
        OracleConfig { annual_vol: 0.40, jumps_per_day: 0.5, outage_per_tick: 0.002, wide_conf_per_tick: 0.002, weekend_gap: (0.01, 0.06), daily_gap: None }
    }
}

/// One tick's publication.
#[derive(Debug, Clone, Copy)]
pub struct Publication {
    /// 6-decimal price
    pub price: u64,
    /// 6-decimal confidence
    pub conf: u64,
    /// False during an outage: the feed keeps its old timestamp
    pub published: bool,
}

pub struct Oracle {
    cfg: OracleConfig,
    price: f64,
    was_open: bool,
    closed_secs: i64,
    outage_ticks: u32,
    /// (time, signed move %)
    pub jumps: Vec<(i64, f64)>,
    pub gaps: Vec<(i64, f64)>,
    pub outages: u32,
    pub wide_conf: u32,
    /// The next reopen's gap, set by the adversarial scenario (overrides the drawn one)
    pub forced_gap: Option<f64>,
    /// Moves the adversarial scenario made inside a session: (time, signed move %)
    pub shocks: Vec<(i64, f64)>,
}

impl Oracle {
    pub fn new(cfg: OracleConfig, start_price: f64) -> Oracle {
        Oracle { cfg, price: start_price, was_open: true, closed_secs: 0, outage_ticks: 0, jumps: Vec::new(), gaps: Vec::new(), outages: 0, wide_conf: 0, forced_gap: None, shocks: Vec::new() }
    }

    /// Advance by `dt` seconds to `now`, `open` being the session state at `now`.
    pub fn step(&mut self, rng: &mut StdRng, now: i64, dt: i64, open: bool) -> Publication {
        // A jump or gap this tick: no outage or confidence spike on top
        let mut moved = false;
        if !open {
            // Pyth publishes the last traded price while CME is shut
            self.closed_secs += dt;
            self.was_open = false;
        } else {
            if !self.was_open {
                // Reopen: a weekend closure gaps by the weekend range, a daily break by its own
                let weekend = self.closed_secs > 6 * 3_600;
                let range = if weekend { Some(self.cfg.weekend_gap) } else { self.cfg.daily_gap };
                let drawn = match range {
                    Some((lo, hi)) => {
                        let g = rng.gen_range(lo..hi);
                        if rng.gen_bool(0.5) { g } else { -g }
                    }
                    None => normal(rng) * 0.0025,
                };
                // The draw is made either way, so the path after it does not depend on the override
                let gap = self.forced_gap.take().unwrap_or(drawn);
                self.price *= 1.0 + gap;
                moved = true;
                self.gaps.push((now, gap * 100.0));
                self.closed_secs = 0;
                self.was_open = true;
            } else {
                let years = dt as f64 / YEAR_SECS;
                let sigma = self.cfg.annual_vol;
                self.price *= ((-0.5 * sigma * sigma) * years + sigma * years.sqrt() * normal(rng)).exp();
                let p_jump = self.cfg.jumps_per_day * dt as f64 / 86_400.0;
                if rng.gen_bool(p_jump.min(1.0)) {
                    let j = rng.gen_range(0.02..0.05);
                    let j = if rng.gen_bool(0.5) { j } else { -j };
                    self.price *= 1.0 + j;
                    moved = true;
                    self.jumps.push((now, j * 100.0));
                }
            }
        }
        let price = to_units(self.price);
        // Normal confidence: 2 bps of the price
        let mut conf = price / 5_000;
        let mut published = true;
        if self.outage_ticks > 0 {
            self.outage_ticks -= 1;
            published = false;
        } else if !moved && rng.gen_bool(self.cfg.outage_per_tick) {
            self.outage_ticks = rng.gen_range(0..3);
            self.outages += 1;
            published = false;
        } else if !moved && rng.gen_bool(self.cfg.wide_conf_per_tick) {
            conf = price / 50; // 2%: over the 1% limit
            self.wide_conf += 1;
        }
        Publication { price, conf, published }
    }
}

impl Oracle {
    /// Move the price to `price` (6 dp) now, inside a session: the
    /// adversarial scenario's flash move. The caller publishes it.
    pub fn shock_to(&mut self, now: i64, price: u64) {
        let before = self.price;
        self.price = price as f64 / 1e6;
        self.shocks.push((now, (self.price / before - 1.0) * 100.0));
    }
}

pub fn to_units(dollars: f64) -> u64 {
    (dollars * 1e6).round() as u64
}

/// Standard normal (Box-Muller).
pub fn normal(rng: &mut StdRng) -> f64 {
    let u1: f64 = rng.gen_range(f64::EPSILON..1.0);
    let u2: f64 = rng.gen();
    (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
}
