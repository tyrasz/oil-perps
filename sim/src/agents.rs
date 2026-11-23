//! Agents: market makers quoting on mm-registry, and retail traders taking
//! those quotes, resting orderbook orders or opening at the oracle; in the
//! stress scenario also a whale, 10x gap holders and large limit traders.
//! The adversarial scenario's colluders and prober are driven by adversary.rs.

use crate::actions::opposite;
use crate::ledger::Route;
use crate::model;
use crate::support::*;
use crate::world::{maker_approval, Sim, MAX_QUOTE_SIZE};
use crate::schedule;
use mm_registry::instructions::{PostQuoteParams, UpdateQuoteParams};
use mm_registry::{MarketMaker, TwoSidedQuote};
use orderbook::{OrderBook, OrderSide, OrderType, PlaceOrderParams};
use perps_core::instructions::VenueApprovalParams;
use perps_core::state::{Side, VenueApproval};
use rand::rngs::StdRng;
use rand::Rng;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Persona {
    MarketMaker,
    Momentum,
    MeanReversion,
    Degen,
    Limit,
    House,
    /// Stress: large taker whose wanted size is above the per-fill cap
    Whale,
    /// Stress: opens at the maximum leverage just before a close and carries it over the gap
    Gapper,
    /// Stress: limit orders far above the per-fill cap, split by the keeper
    LimitBig,
    /// Adversarial: the colluding pair
    Colluder,
    /// Adversarial: sends requests the engine must reject
    Prober,
}

impl Persona {
    pub fn name(self) -> &'static str {
        match self {
            Persona::MarketMaker => "market_maker",
            Persona::Momentum => "momentum",
            Persona::MeanReversion => "mean_reversion",
            Persona::Degen => "degen",
            Persona::Limit => "limit",
            Persona::House => "house",
            Persona::Whale => "whale",
            Persona::Gapper => "gapper",
            Persona::LimitBig => "limit_big",
            Persona::Colluder => "colluder",
            Persona::Prober => "prober",
        }
    }
}

/// Behaviour parameters, drawn per agent.
#[derive(Clone)]
pub struct Strategy {
    // Market makers
    pub half_spread_bps: f64,
    pub quote_contracts: u64,
    pub max_inventory_contracts: u64,
    // Retail
    pub route: Route,
    pub leverage_x: f64,
    /// Share of free collateral committed per entry
    pub fraction: f64,
    pub lookback: usize,
    pub threshold: f64,
    pub take_profit: f64,
    pub stop_loss: f64,
    pub max_hold_ticks: u64,
    pub max_positions: usize,
    /// Chance per tick of acting on an entry decision
    pub activity: f64,
    /// Orderbook min_fill_size (0 = the program's default, size / max_fills)
    pub min_fill_size: u64,
    /// Orderbook max_fills per order
    pub max_fills: u8,
    /// Orderbook order size range in whole contracts (0: sized from free collateral)
    pub order_contracts: (u64, u64),
    /// Chance an orderbook order crosses the mid
    pub cross: f64,
}

impl Strategy {
    pub fn mm(spread_bps: f64, quote_contracts: u64, max_inventory_contracts: u64) -> Strategy {
        Strategy {
            half_spread_bps: spread_bps / 2.0, quote_contracts, max_inventory_contracts,
            route: Route::MmRegistry, leverage_x: 5.0, fraction: 0.0, lookback: 0, threshold: 0.0,
            take_profit: 0.0, stop_loss: 0.0, max_hold_ticks: 0, max_positions: 0, activity: 1.0, min_fill_size: 0,
            max_fills: 4, order_contracts: (0, 0), cross: 0.3,
        }
    }

    pub fn retail(persona: Persona, rng: &mut StdRng) -> Strategy {
        let mut s = Strategy::mm(0.0, 0, 0);
        match persona {
            Persona::Momentum => {
                s.leverage_x = rng.gen_range(2.0..10.0);
                s.fraction = rng.gen_range(0.3..0.6);
                s.lookback = rng.gen_range(6..24);
                s.threshold = rng.gen_range(0.0025..0.005);
                s.take_profit = rng.gen_range(0.01..0.02);
                s.stop_loss = rng.gen_range(0.006..0.012);
                s.max_hold_ticks = rng.gen_range(36..144);
                s.max_positions = 2;
                s.activity = 0.5;
            }
            Persona::MeanReversion => {
                s.leverage_x = rng.gen_range(2.0..10.0);
                s.fraction = rng.gen_range(0.3..0.6);
                s.lookback = rng.gen_range(12..48);
                s.threshold = rng.gen_range(0.003..0.006);
                s.take_profit = rng.gen_range(0.006..0.012);
                s.stop_loss = rng.gen_range(0.01..0.015);
                s.max_hold_ticks = rng.gen_range(24..96);
                s.max_positions = 2;
                s.activity = 0.5;
            }
            Persona::Degen => {
                // Asks for 15-20x; the market's initial margin caps it lower
                s.leverage_x = rng.gen_range(15.0..20.0);
                s.fraction = 0.9;
                s.take_profit = 0.10;
                s.max_positions = 1;
                s.activity = 0.08;
            }
            Persona::Limit => {
                s.route = Route::Orderbook;
                s.leverage_x = rng.gen_range(2.0..5.0);
                s.fraction = rng.gen_range(0.1..0.25);
                s.take_profit = rng.gen_range(0.006..0.015);
                s.stop_loss = rng.gen_range(0.006..0.015);
                s.max_hold_ticks = rng.gen_range(24..144);
                s.max_positions = 6;
                s.activity = 0.35;
                // Some traders accept fills down to one contract; the rest keep the default
                s.min_fill_size = if rng.gen_bool(0.5) { CONTRACT } else { 0 };
            }
            Persona::House => {
                s.route = Route::House;
                s.leverage_x = rng.gen_range(2.0..8.0);
                s.fraction = rng.gen_range(0.2..0.5);
                s.lookback = 6;
                s.take_profit = rng.gen_range(0.008..0.02);
                s.stop_loss = rng.gen_range(0.008..0.02);
                s.max_hold_ticks = rng.gen_range(12..288);
                s.max_positions = 2;
                s.activity = 0.05;
            }
            Persona::Whale => {
                s.leverage_x = rng.gen_range(3.0..4.0);
                s.fraction = 0.6;
                s.lookback = rng.gen_range(6..24);
                s.threshold = rng.gen_range(0.002..0.004);
                s.take_profit = rng.gen_range(0.008..0.015);
                s.stop_loss = rng.gen_range(0.008..0.015);
                s.max_hold_ticks = rng.gen_range(24..96);
                s.max_positions = 3;
                s.activity = 0.15;
            }
            Persona::Gapper => {
                // The maximum the initial margin allows: 10x, or 5x before the weekly close
                s.leverage_x = 20.0;
                s.fraction = 0.9;
                s.max_positions = 1;
            }
            Persona::LimitBig => {
                s.route = Route::Orderbook;
                s.leverage_x = 3.0;
                s.take_profit = rng.gen_range(0.006..0.015);
                s.stop_loss = rng.gen_range(0.01..0.02);
                s.max_hold_ticks = rng.gen_range(24..96);
                s.max_positions = 12;
                s.activity = 0.3;
                s.min_fill_size = CONTRACT;
                s.max_fills = 8;
                s.order_contracts = (1_200, 2_500);
                s.cross = 0.5;
            }
            Persona::MarketMaker | Persona::Colluder | Persona::Prober => {}
        }
        s
    }
}

#[derive(Default)]
pub struct MmEcon {
    pub fills: u64,
    pub volume: u128,
    /// Edge at the oracle on each fill: (fill - oracle) x size for the MM's side
    pub spread_captured: i128,
    pub close_fees: u64,
    pub pair_closes: u64,
    pub inventory_closes: u64,
    pub allowance_refills: u64,
    pub reposts: u64,
    pub peak_gross_contracts: f64,
    pub peak_locked: u64,
    /// Hourly sums, for time averages
    pub locked_sum: f64,
    pub net_margin_sum: f64,
    pub gross_notional_sum: f64,
    pub samples: u64,
}

pub struct MmBook {
    pub market_maker: Pubkey,
    pub bond: Pubkey,
    pub quote: Option<Pubkey>,
    pub budget: u64,
    pub econ: MmEcon,
}

impl MmBook {
    pub fn new(market_maker: Pubkey, bond: Pubkey, budget: u64) -> MmBook {
        MmBook { market_maker, bond, quote: None, budget, econ: MmEcon::default() }
    }
}

#[derive(Default)]
pub struct AgentStats {
    pub opens: u64,
    pub maker_fills: u64,
    pub closes: u64,
    pub liquidated: u64,
    pub fees_paid: u64,
    pub orders: u64,
}

pub struct Agent {
    pub name: &'static str,
    pub persona: Persona,
    pub t: Trader,
    /// Every token minted to this agent
    pub minted: u64,
    /// Initial perps-core deposit
    pub deposit: u64,
    pub s: Strategy,
    pub mm: Option<MmBook>,
    pub stats: AgentStats,
    pub probed: bool,
    pub last_entry_tick: u64,
    pub ema: f64,
}

impl Agent {
    pub fn new(name: &'static str, persona: Persona, t: Trader, minted: u64, deposit: u64, s: Strategy, mm: Option<MmBook>) -> Agent {
        Agent { name, persona, t, minted, deposit, s, mm, stats: AgentStats::default(), probed: false, last_entry_tick: 0, ema: 0.0 }
    }
}

const QUOTE_TTL: i64 = 600;

impl Sim {
    /// One agent's turn. Bots idle while the session is closed.
    pub async fn act(&mut self, i: usize) {
        if !self.in_session() {
            return;
        }
        self.redeposit(i).await;
        match self.agents[i].persona {
            Persona::MarketMaker => self.act_mm(i).await,
            Persona::Limit | Persona::LimitBig => self.act_limit(i).await,
            Persona::Gapper => self.act_gapper(i).await,
            Persona::Colluder | Persona::Prober => {}
            _ => self.act_taker(i).await,
        }
    }

    /// Highest leverage (3 dp) the market allows now, clamping `x`.
    async fn leverage_for(&mut self, x: f64) -> (u32, bool) {
        let m = self.market().await;
        let want = (x * 1_000.0) as u32;
        let cap = model::max_leverage(&m, self.now);
        (want.min(cap), want > cap)
    }

    fn contracts_for(&self, free: u64, fraction: f64, leverage: u32) -> u64 {
        let notional = free as f64 * fraction * leverage as f64 / 1_000.0;
        let size = notional / self.px as f64 * 1e6;
        (size as u64) / 1_000 * 1_000
    }

    // --- market makers -----------------------------------------------------------

    async fn act_mm(&mut self, i: usize) {
        self.mm_manage_risk(i).await;
        self.mm_refill_allowance(i).await;
        self.mm_quote(i).await;
    }

    /// Net inventory (base units, + = long), gross, and collateral locked in open positions.
    pub fn inventory(&self, i: usize) -> (i128, u64, u64) {
        let mut net = 0i128;
        let mut gross = 0u64;
        let mut locked = 0u64;
        for idx in self.ledger.open_of(i) {
            let p = &self.ledger.positions[idx];
            net += p.signed_size();
            gross += p.state.size;
            locked += p.state.collateral;
        }
        (net, gross, locked)
    }

    fn largest_open(&self, i: usize, side: Side) -> Option<usize> {
        self.ledger.open_of(i).into_iter().filter(|&p| self.ledger.positions[p].side() == side).max_by_key(|&p| self.ledger.positions[p].state.size)
    }

    /// No netting in perps-core: a flat MM still holds a margined long and short.
    /// When free collateral runs low, close offsetting long/short pairs; when
    /// inventory is past the limit, close the heavy side, oldest first.
    async fn mm_manage_risk(&mut self, i: usize) {
        let free = self.user_of(i).await.collateral_balance;
        let deposit = self.agents[i].deposit;
        let max_inv = (self.agents[i].s.max_inventory_contracts * CONTRACT) as i128;
        let mut closed_any = false;
        if free < deposit / 4 {
            let mut recovered = 0u64;
            while free + recovered < deposit / 2 {
                let (Some(l), Some(s)) = (self.largest_open(i, Side::Long), self.largest_open(i, Side::Short)) else { break };
                let freed = self.ledger.positions[l].state.collateral + self.ledger.positions[s].state.collateral;
                let ok_l = self.close_tracked(l, "close_position").await;
                let ok_s = self.close_tracked(s, "close_position").await;
                if !(ok_l && ok_s) {
                    break;
                }
                recovered += freed;
                closed_any = true;
                if let Some(b) = self.agents[i].mm.as_mut() {
                    b.econ.pair_closes += 1;
                }
            }
        }
        let (net, _, _) = self.inventory(i);
        if net.abs() > max_inv {
            let heavy = if net > 0 { Side::Long } else { Side::Short };
            let mut excess = net.abs() - max_inv / 2;
            for idx in self.ledger.open_of(i) {
                if excess <= 0 {
                    break;
                }
                if self.ledger.positions[idx].side() != heavy {
                    continue;
                }
                let size = self.ledger.positions[idx].state.size as i128;
                if self.close_tracked(idx, "close_position").await {
                    excess -= size;
                    closed_any = true;
                    if let Some(b) = self.agents[i].mm.as_mut() {
                        b.econ.inventory_closes += 1;
                    }
                }
            }
        }
        if closed_any {
            self.redeposit(i).await;
        }
    }

    /// Allowances are spent down and never refill: top up the risk budget.
    async fn mm_refill_allowance(&mut self, i: usize) {
        let owner = self.agents[i].t.key();
        let address = approval_pda(&owner, &self.mm_venue, ROLE_MAKER);
        // Revoked (by the adversarial scenario, which restores it): nothing to refill
        let Some(approval) = try_load::<VenueApproval>(self.chain.base(), address).await else { return };
        let budget = self.agents[i].mm.as_ref().unwrap().budget;
        if approval.allowance >= budget / 2 && approval.expires_at > self.now + 86_400 {
            return;
        }
        let params = maker_approval(budget, self.now);
        let ix = update_approval_ix(&self.chain.base, &owner, &self.mm_venue, ROLE_MAKER, params);
        let kp = self.agents[i].t.kp.insecure_clone();
        if self.run("update_venue_approval", &[ix], &[&kp], None).await.is_ok() {
            self.ledger.track_approval(address, budget);
            self.agents[i].mm.as_mut().unwrap().econ.allowance_refills += 1;
        }
    }

    /// Refresh the two-sided quote around the oracle, skewed against inventory.
    async fn mm_quote(&mut self, i: usize) {
        let s = self.agents[i].s.clone();
        let (net, _, _) = self.inventory(i);
        let max_inv = (s.max_inventory_contracts * CONTRACT) as f64;
        let skew = (net as f64 / max_inv).clamp(-1.0, 1.0);
        let px = self.px as f64;
        // Long inventory: shade both prices down to sell more and buy less
        let mid = px * (1.0 - skew * s.half_spread_bps / 10_000.0);
        let bid = (mid * (1.0 - s.half_spread_bps / 10_000.0)).floor() as u64;
        let ask = ((mid * (1.0 + s.half_spread_bps / 10_000.0)).ceil() as u64).max(bid + 1);
        let base = (s.quote_contracts * CONTRACT) as f64;
        let bid_size = ((base * (1.0 - skew.max(0.0))) as u64).max(CONTRACT) / 1_000 * 1_000;
        let ask_size = ((base * (1.0 + skew.min(0.0))) as u64).max(CONTRACT) / 1_000 * 1_000;
        let (leverage, _) = self.leverage_for(s.leverage_x).await;
        let owner = self.agents[i].t.key();
        let kp = self.agents[i].t.kp.insecure_clone();

        let quote = self.agents[i].mm.as_ref().unwrap().quote;
        let existing = match quote {
            Some(q) => try_load::<TwoSidedQuote>(self.chain.base(), q).await.filter(|q| q.is_active),
            None => None,
        };
        if let (Some(address), Some(q)) = (quote, existing) {
            let (bid_filled, ask_filled) = (q.bid_size - q.bid_remaining, q.ask_size - q.ask_remaining);
            // update_quote keeps what was filled: size = remaining wanted + filled
            if bid_filled + bid_size <= MAX_QUOTE_SIZE && ask_filled + ask_size <= MAX_QUOTE_SIZE {
                let params = UpdateQuoteParams {
                    bid_price: Some(bid), bid_size: Some(bid_size + bid_filled), ask_price: Some(ask), ask_size: Some(ask_size + ask_filled),
                    leverage: Some(leverage), expires_in: Some(QUOTE_TTL),
                };
                let ix = self.update_quote_ix(&owner, address, params);
                if self.run("update_quote", &[ix], &[&kp], None).await.is_ok() {
                    self.metrics.quote_updates += 1;
                }
                return;
            }
            // Filled through the registry's size bound: start a fresh quote
            let ix = self.cancel_quote_ix(&owner, address);
            if self.run("cancel_quote", &[ix], &[&kp], None).await.is_err() {
                return;
            }
            self.agents[i].mm.as_mut().unwrap().econ.reposts += 1;
        }
        let market_maker = self.agents[i].mm.as_ref().unwrap().market_maker;
        let index = load::<MarketMaker>(self.chain.base(), market_maker).await.quotes_posted;
        let params = PostQuoteParams { bid_price: bid, bid_size, ask_price: ask, ask_size, min_fill_size: CONTRACT, expires_in: QUOTE_TTL, leverage };
        let (ix, address) = self.post_quote_ix(&owner, index, params);
        if self.run("post_quote", &[ix], &[&kp], None).await.is_ok() {
            self.agents[i].mm.as_mut().unwrap().quote = Some(address);
            self.ledger.record_quote(address);
            self.metrics.quote_posts += 1;
        }
    }

    // --- retail takers ---------------------------------------------------------------

    /// Exits on take-profit, stop-loss or holding time. Takers other than
    /// degens rescue a position close to maintenance with add_margin.
    async fn manage_exits(&mut self, i: usize) {
        let s = self.agents[i].s.clone();
        let persona = self.agents[i].persona;
        let m = self.market().await;
        let (im, maintenance) = model::margin_ratios(&m, self.now);
        for idx in self.ledger.open_of(i) {
            let p = &self.ledger.positions[idx];
            let entry = p.state.entry_price as f64;
            let moved = model::sign(p.side()) as f64 * (self.px as f64 - entry) / entry;
            let held = self.tick - p.opened_tick;
            let exit = match persona {
                // Rarely close: a big win, or now and then
                Persona::Degen => moved >= s.take_profit || self.rng.gen_bool(0.002),
                _ => moved >= s.take_profit || moved <= -s.stop_loss || held >= s.max_hold_ticks,
            };
            if exit {
                self.close_tracked(idx, "close_position").await;
                continue;
            }
            let ratio = model::margin_ratio_bps(&p.state, self.px, m.cumulative_funding);
            if persona != Persona::Degen && ratio > 0 && ratio < maintenance as i128 + 150 {
                // Back to the initial margin on the entry notional
                let target = model::notional(p.state.size, p.state.entry_price) * im as i128 / model::BPS;
                let needed = (target - model::equity(&p.state, self.px, m.cumulative_funding)).max(0) as u64;
                let free = self.user_of(i).await.collateral_balance;
                if needed > 0 && free >= needed {
                    self.add_margin(idx, needed, "add_margin").await;
                }
            }
        }
    }

    fn signal(&self, i: usize) -> Option<Side> {
        let a = &self.agents[i];
        let s = &a.s;
        let h = &self.history;
        let px = self.px as f64;
        match a.persona {
            Persona::Momentum => {
                let past = *h.get(h.len().checked_sub(s.lookback + 1)?)? as f64;
                let r = px / past - 1.0;
                if r > s.threshold {
                    Some(Side::Long)
                } else if r < -s.threshold {
                    Some(Side::Short)
                } else {
                    None
                }
            }
            Persona::MeanReversion => {
                if h.len() < s.lookback {
                    return None;
                }
                let dev = px / a.ema - 1.0;
                if dev > s.threshold {
                    Some(Side::Short)
                } else if dev < -s.threshold {
                    Some(Side::Long)
                } else {
                    None
                }
            }
            Persona::House | Persona::Whale => {
                let past = *h.get(h.len().checked_sub(s.lookback + 1)?)? as f64;
                Some(if px >= past { Side::Long } else { Side::Short })
            }
            _ => None,
        }
    }

    async fn act_taker(&mut self, i: usize) {
        // Mean-reversion keeps an EMA of the oracle
        let px = self.px as f64;
        let a = &mut self.agents[i];
        let alpha = 2.0 / (a.s.lookback.max(1) as f64 + 1.0);
        a.ema = if a.ema == 0.0 { px } else { a.ema + alpha * (px - a.ema) };

        self.manage_exits(i).await;
        let s = self.agents[i].s.clone();
        let persona = self.agents[i].persona;
        if self.paused || self.ledger.open_of(i).len() >= s.max_positions || self.tick < self.agents[i].last_entry_tick + 3 {
            return;
        }
        if !self.rng.gen_bool(s.activity) {
            return;
        }
        let side = match persona {
            Persona::Degen => {
                if self.rng.gen_bool(0.5) { Side::Long } else { Side::Short }
            }
            Persona::House | Persona::Whale => {
                let side = self.signal(i);
                match side {
                    Some(sd) if self.rng.gen_bool(0.6) => sd,
                    Some(sd) => opposite(sd),
                    None => return,
                }
            }
            _ => match self.signal(i) {
                Some(sd) => sd,
                None => return,
            },
        };
        // A degen's first trade asks for its 15-20x; after the rejection it uses the cap
        let probe = persona == Persona::Degen && !self.agents[i].probed;
        let (leverage, clamped) = if probe {
            self.agents[i].probed = true;
            ((s.leverage_x * 1_000.0) as u32, false)
        } else {
            self.leverage_for(s.leverage_x).await
        };
        if clamped {
            self.metrics.leverage_clamps += 1;
        }
        let free = self.user_of(i).await.collateral_balance;
        let size = self.contracts_for(free, s.fraction, leverage);
        if size < CONTRACT {
            return;
        }
        // degen-1 and the takers fill MM quotes; degen-2 and house traders open at the oracle
        let house = persona == Persona::House || (persona == Persona::Degen && self.agents[i].name.ends_with('2'));
        let opened = if house {
            self.open_house(i, side, size, leverage, if probe { "open_position_probe" } else { "open_position" }).await
        } else if persona == Persona::Whale {
            self.whale_entry(i, side, size, leverage).await
        } else {
            self.take_quote(i, side, size, leverage, if probe { "fill_quote_probe" } else { "fill_quote" }).await
        };
        if opened {
            self.agents[i].last_entry_tick = self.tick;
        }
    }

    // --- limit traders -----------------------------------------------------------------

    async fn act_limit(&mut self, i: usize) {
        self.manage_exits(i).await;
        self.limit_refill_allowances(i).await;
        let owner = self.agents[i].t.key();
        let kp = self.agents[i].t.kp.insecure_clone();
        // Cancel stale orders: old, or far from the oracle
        for o in 0..self.ledger.orders.len() {
            let order = &self.ledger.orders[o];
            if !order.live || order.owner != i || self.now > order.expires_at {
                continue;
            }
            let away = (order.price as f64 / self.px as f64 - 1.0).abs();
            if self.tick - order.placed_tick >= 8 || away > 0.006 {
                let address = order.address;
                if !exists(self.chain.base(), address).await {
                    self.ledger.orders[o].live = false;
                    continue;
                }
                let ix = self.cancel_order_ix(&owner, address);
                if self.run("cancel_order", &[ix], &[&kp], None).await.is_ok() {
                    self.ledger.orders[o].live = false;
                    self.ledger.orders[o].end = Some(crate::ledger::OrderEnd::Cancelled);
                    self.metrics.orders_cancelled += 1;
                }
            }
        }
        let s = self.agents[i].s.clone();
        let live = self.ledger.orders.iter().filter(|o| o.live && o.owner == i).count();
        if self.paused || live >= 3 || self.ledger.open_of(i).len() >= s.max_positions || !self.rng.gen_bool(s.activity) {
            return;
        }
        let side = if self.rng.gen_bool(0.5) { OrderSide::Bid } else { OrderSide::Ask };
        // Mostly passive near the mid; sometimes crossing it
        let bps = if self.rng.gen_bool(s.cross) { -self.rng.gen_range(0.0..15.0) } else { self.rng.gen_range(1.0..20.0) };
        let px = self.px as f64;
        let price = match side {
            OrderSide::Bid => px * (1.0 - bps / 10_000.0),
            OrderSide::Ask => px * (1.0 + bps / 10_000.0),
        }
        .round() as u64;
        let (leverage, _) = self.leverage_for(s.leverage_x).await;
        let free = self.user_of(i).await.collateral_balance;
        let size = match s.order_contracts {
            (0, _) => self.contracts_for(free, s.fraction, leverage),
            (lo, hi) => self.rng.gen_range(lo..=hi) * CONTRACT,
        };
        if size < CONTRACT {
            return;
        }
        let ttl = self.rng.gen_range(1_200..5_400);
        let params = PlaceOrderParams {
            side, order_type: OrderType::Limit, price, size, leverage, expires_at: self.now + ttl, max_fills: s.max_fills, min_fill_size: s.min_fill_size,
        };
        let seq = load::<OrderBook>(self.chain.base(), self.book).await.sequence_number;
        let (ix, address) = self.place_order_ix(&owner, seq, params);
        if self.run("place_order", &[ix], &[&kp], None).await.is_ok() {
            self.ledger.orders.push(crate::ledger::TrackedOrder {
                address, owner: i, price, placed_tick: self.tick, expires_at: self.now + ttl, live: true, fills: 0, capped_fills: 0, end: None,
            });
            self.metrics.orders_placed += 1;
            self.agents[i].stats.orders += 1;
        }
    }

    async fn limit_refill_allowances(&mut self, i: usize) {
        let owner = self.agents[i].t.key();
        let budget = self.agents[i].deposit;
        for role in [ROLE_TAKER, ROLE_MAKER] {
            let address = approval_pda(&owner, &self.book_venue, role);
            let approval: VenueApproval = load(self.chain.base(), address).await;
            if approval.allowance >= budget * 3 / 10 && approval.expires_at > self.now + 86_400 {
                continue;
            }
            let params = VenueApprovalParams { expires_at: self.now + 30 * 86_400, ..approval_params(budget, self.now) };
            let ix = update_approval_ix(&self.chain.base, &owner, &self.book_venue, role, params);
            let kp = self.agents[i].t.kp.insecure_clone();
            if self.run("update_venue_approval", &[ix], &[&kp], None).await.is_ok() {
                self.ledger.track_approval(address, budget);
            }
        }
    }

    // --- stress: the whale and the gap holders ----------------------------------------

    /// The whale wants more than one fill may carry. Now and then it sends the
    /// whole clip unsplit, which perps-core must refuse (FillTooLarge); then it
    /// sweeps the book in SDK-sized fills.
    async fn whale_entry(&mut self, i: usize, side: Side, size: u64, leverage: u32) -> bool {
        let is_buy = side == Side::Long;
        if self.rng.gen_bool(0.3) {
            if let Some((mm, quote, q)) = self.best_quote(i, is_buy).await {
                let (price, remaining) = if is_buy { (q.ask_price, q.ask_remaining) } else { (q.bid_price, q.bid_remaining) };
                let clip = size.min(remaining);
                let cap = crate::world::mm_venue_params().max_fill_notional as i128;
                if model::notional(clip, price) > cap {
                    let limit = if is_buy { price + price / 2_000 } else { price - price / 2_000 };
                    let approval = self.sdk_approval(side, clip, price, limit, leverage).await;
                    let take = crate::actions::Take { agent: i, mm, quote, size: clip, is_buy, limit, leverage, approval: Some(approval), label: "fill_quote_probe", collusion: false };
                    self.send_take(take).await;
                }
            }
        }
        let mut left = size;
        let mut any = false;
        for _ in 0..3 {
            if left < CONTRACT {
                break;
            }
            let before = self.ledger.positions.len();
            if !self.take_quote(i, side, left, leverage, "fill_quote").await {
                break;
            }
            any = true;
            let filled = self.ledger.positions.get(before).map_or(0, |p| p.state.size);
            left = left.saturating_sub(filled.max(CONTRACT));
        }
        any
    }

    /// A gap holder opens at the highest leverage the market allows in the
    /// half hour before a daily break or the weekly close, and closes after
    /// the reopen, unless the keeper liquidates it first.
    async fn act_gapper(&mut self, i: usize) {
        for idx in self.ledger.open_of(i) {
            let opened = self.ledger.positions[idx].opened_tick;
            let carried = opened < self.last_reopen_tick;
            let close = if carried { self.rng.gen_bool(0.5) || self.tick >= self.last_reopen_tick + 6 } else { self.tick >= opened + 36 };
            if close {
                self.close_tracked(idx, "close_position").await;
            }
        }
        let (weekday, secs) = schedule::eastern(self.now);
        let before_close = (1..=5).contains(&weekday) && (16 * 3_600 + 1_800..17 * 3_600).contains(&secs);
        if self.paused || !before_close || !self.ledger.open_of(i).is_empty() || self.tick < self.agents[i].last_entry_tick + 6 {
            return;
        }
        let s = self.agents[i].s.clone();
        let (leverage, _) = self.leverage_for(s.leverage_x).await;
        let free = self.user_of(i).await.collateral_balance;
        let size = self.contracts_for(free, s.fraction, leverage);
        if size < CONTRACT {
            return;
        }
        let side = if self.rng.gen_bool(0.5) { Side::Long } else { Side::Short };
        let opened = if self.agents[i].name.ends_with('1') {
            self.open_house(i, side, size, leverage, "open_position").await
        } else {
            self.take_quote(i, side, size, leverage, "fill_quote").await
        };
        if opened {
            self.agents[i].last_entry_tick = self.tick;
            self.hit("gapper.entry");
        }
    }
}
