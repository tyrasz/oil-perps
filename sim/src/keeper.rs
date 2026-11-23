//! The keeper, every tick in session: match crossing orderbook orders (price,
//! then sequence, split under the venue's per-fill cap), crank funding when
//! due, liquidate every position the model calls liquidatable, probe a few
//! healthy ones near maintenance (the engine must refuse them), and close
//! expired orders. On the first tick after a close (a session edge) it runs
//! its pass as a keeper that has not noticed the close would, and the engine
//! must refuse with MarketClosed.

use crate::actions::{FillPlan, FillPre};
use crate::ledger::{OrderEnd, Route, Settlement};
use crate::model;
use crate::predict::{self, Leg, Rej, Terms};
use crate::scenario::Scenario;
use crate::support::*;
use crate::world::Sim;
use orderbook::{Order, OrderSide, OrderStatus};
use perps_core::state::{Position, PositionStatus, Side, VenueApproval};
use serde_json::json;
use std::collections::{HashMap, HashSet};

/// Liquidations the keeper attempted and could not do this tick, with the error.
pub type LiquidationFailures = HashMap<usize, String>;

const MAX_MATCHES_PER_TICK: usize = 40;
/// Healthy positions probed with liquidate per tick, and how close to maintenance (bps)
const PROBES_PER_TICK: usize = 2;
const PROBE_WITHIN_BPS: i128 = 150;
/// Unsplit crosses over the per-fill cap the keeper sends to see FillTooLarge (stress)
const UNSPLIT_PROBES: u64 = 5;

impl Sim {
    pub async fn keeper_pass(&mut self) -> LiquidationFailures {
        let mut failed = LiquidationFailures::new();
        if self.in_session() {
            self.match_book().await;
            self.crank_funding().await;
            failed = self.liquidate_all().await;
            self.probe_healthy().await;
        } else if self.at_edge {
            // The keeper has not noticed the close: everything it sends now must fail with MarketClosed
            self.crank_funding().await;
            failed = self.liquidate_all().await;
            self.probe_edge().await;
        }
        self.close_expired_orders().await;
        failed
    }

    // --- matching ---------------------------------------------------------------------

    async fn live_orders(&mut self) -> Vec<(usize, Order)> {
        let mut out = Vec::new();
        for o in 0..self.ledger.orders.len() {
            if !self.ledger.orders[o].live {
                continue;
            }
            let address = self.ledger.orders[o].address;
            match try_load::<Order>(self.chain.base(), address).await {
                Some(order) => out.push((o, order)),
                // Every close of an order is tracked where it happens
                None => {
                    self.ledger.orders[o].live = false;
                    let (tick, now) = (self.tick, self.now);
                    self.checks.check(tick, now, "8_fill_accounting", false, || format!("order {} vanished without a tracked close", address));
                }
            }
        }
        out
    }

    fn fillable(&self, o: &Order) -> bool {
        (o.status == OrderStatus::Open || o.status == OrderStatus::PartiallyFilled) && self.now <= o.expires_at && o.fills < o.max_fills
    }

    async fn leg_of(&mut self, o: &Order, role: u8) -> Leg {
        let agent = self.agent_of(&o.owner);
        let approval = approval_pda(&o.owner, &self.book_venue, role);
        let terms = try_load::<VenueApproval>(self.chain.base(), approval).await.map(|a| Terms::from(&a));
        Leg {
            side: if o.side == OrderSide::Bid { Side::Long } else { Side::Short },
            leverage: o.leverage,
            free: self.user_of(agent).await.collateral_balance,
            approval: terms,
        }
    }

    async fn match_book(&mut self) {
        let m = self.market().await;
        if m.is_paused {
            return;
        }
        let mut skip: HashSet<(usize, usize)> = HashSet::new();
        let venue = load_venue(self.chain.base(), &orderbook::ID).await;
        for _ in 0..MAX_MATCHES_PER_TICK {
            let orders = self.live_orders().await;
            let mut bids: Vec<&(usize, Order)> = orders.iter().filter(|(_, o)| o.side == OrderSide::Bid && self.fillable(o)).collect();
            let mut asks: Vec<&(usize, Order)> = orders.iter().filter(|(_, o)| o.side == OrderSide::Ask && self.fillable(o)).collect();
            // Price, then time (sequence)
            bids.sort_by(|a, b| b.1.price.cmp(&a.1.price).then(a.1.sequence_number.cmp(&b.1.sequence_number)));
            asks.sort_by(|a, b| a.1.price.cmp(&b.1.price).then(a.1.sequence_number.cmp(&b.1.sequence_number)));
            let mut chosen = None;
            'outer: for (bi, b) in &bids {
                for (ai, a) in &asks {
                    if b.price < a.price {
                        break;
                    }
                    if b.owner == a.owner || skip.contains(&(*bi, *ai)) {
                        continue;
                    }
                    chosen = Some(((*bi, b.clone()), (*ai, a.clone())));
                    break 'outer;
                }
            }
            let Some(((bi, b), (ai, a))) = chosen else { break };
            skip.insert((bi, ai));
            let maker_is_bid = b.sequence_number < a.sequence_number;
            let price = if maker_is_bid { b.price } else { a.price };
            // Simulate locally and skip what the venue would refuse
            if !model::within_band(price, self.px, venue.max_price_deviation_bps) {
                *self.metrics.match_skips.entry("outside_band".into()).or_default() += 1;
                continue;
            }
            let (rem_b, rem_a) = (b.size - b.filled_size, a.size - a.filled_size);
            let crossing = rem_b.min(rem_a);
            let max_fill = (venue.max_fill_notional as u128 * 1_000_000 / price as u128) as u64;
            let fill = match predict::match_orders(&b, &a, self.now, max_fill) {
                Ok(fill) => fill,
                Err(r) => {
                    *self.metrics.match_skips.entry(format!("predicted_{}", r.1)).or_default() += 1;
                    continue;
                }
            };
            if self.cfg.scenario == Scenario::Stress && crossing > max_fill && self.metrics.unsplit_probes < UNSPLIT_PROBES {
                // The cross is over the per-fill cap: unsplit, perps-core must refuse it
                self.metrics.unsplit_probes += 1;
                self.match_pair(bi, &b, ai, &a, crossing, 0, "match_orders_probe").await;
            }
            if !self.match_pair(bi, &b, ai, &a, fill, max_fill, "match_orders").await {
                continue;
            }
            if fill < crossing {
                self.hit("match.split_by_cap");
            }
        }
    }

    /// Predict, send and check one match. Returns whether it landed.
    #[allow(clippy::too_many_arguments)]
    pub async fn match_pair(&mut self, bi: usize, b: &Order, ai: usize, a: &Order, fill: u64, max_fill_size: u64, label: &'static str) -> bool {
        let m = self.market().await;
        let venue = load_venue(self.chain.base(), &orderbook::ID).await;
        let taker_is_bid = b.sequence_number > a.sequence_number;
        let (taker_order, maker_order) = if taker_is_bid { (b, a) } else { (a, b) };
        let taker_leg = self.leg_of(taker_order, ROLE_TAKER).await;
        let maker_leg = self.leg_of(maker_order, ROLE_MAKER).await;
        let mut predicted = predict::match_orders(b, a, self.now, max_fill_size).err();
        if predicted.is_none() {
            predicted = predict::venue_fill(&m, false, &venue, &self.ov(), self.now, 0, false, fill, maker_order.price, &taker_leg, &maker_leg).err();
        }
        let (bid_addr, ask_addr) = (self.ledger.orders[bi].address, self.ledger.orders[ai].address);
        if let Some(r) = predicted {
            // A predicted failure is sent once per pair, as the keeper's simulate step; then skipped
            if label == "match_orders" && !self.metrics.probed_pairs.insert((bid_addr, ask_addr)) {
                *self.metrics.match_skips.entry(format!("predicted_{}", r.1)).or_default() += 1;
                return false;
            }
        }
        if !self.should_send(label, predicted) {
            return false;
        }
        let nonce = self.nonce();
        let (ix, fa) = self.match_ix((&bid_addr, b), (&ask_addr, a), nonce, max_fill_size);
        let (taker, maker) = (self.agent_of(&fa.taker), self.agent_of(&fa.maker));
        let pre = FillPre {
            taker_free: taker_leg.free,
            maker_free: maker_leg.free,
            taker_allowance: self.ledger.approval_cap(&fa.taker_approval).unwrap_or(0),
            maker_allowance: self.ledger.approval_cap(&fa.maker_approval).unwrap_or(0),
            market: m,
        };
        let kp = self.keeper.kp.insecure_clone();
        if self.run(label, &[compute_budget_ix(400_000), ix], &[&kp], predicted).await.is_err() {
            *self.metrics.match_skips.entry("failed".into()).or_default() += 1;
            return false;
        }
        self.metrics.matches += 1;
        let plan = FillPlan {
            route: Route::Orderbook, taker, maker, taker_side: if taker_is_bid { Side::Long } else { Side::Short },
            size: fill, price: maker_order.price, taker_leverage: taker_order.leverage, maker_leverage: maker_order.leverage,
            collusion: self.adversary.colluding_orders(bid_addr, ask_addr),
        };
        self.settle_fill(plan, pre, fa).await;
        // A filled order, or one out of prepaid fills, is closed to its owner
        for (o, order) in [(bi, b), (ai, a)] {
            let filled = order.size - order.filled_size == fill;
            let exhausted = order.fills + 1 == order.max_fills;
            let address = self.ledger.orders[o].address;
            let gone = !exists(self.chain.base(), address).await;
            let (tick, now) = (self.tick, self.now);
            self.checks.check(tick, now, "8_fill_accounting", (filled || exhausted) == gone, || {
                format!("order {} after a {} fill: expected closed={}, closed={}", address, fill, filled || exhausted, gone)
            });
            let tracked = &mut self.ledger.orders[o];
            tracked.fills += 1;
            if fill < order.size - order.filled_size && max_fill_size > 0 && fill == max_fill_size {
                tracked.capped_fills += 1;
            }
            if gone {
                tracked.live = false;
                tracked.end = Some(if filled { OrderEnd::Filled } else { OrderEnd::MaxFills });
                let end = if filled { "order.closed_filled" } else { "order.closed_max_fills" };
                self.hit(end);
            }
        }
        true
    }

    // --- funding ------------------------------------------------------------------------

    async fn crank_funding(&mut self) {
        let m = self.market().await;
        if self.now - m.last_funding_time < m.funding_interval {
            return;
        }
        let predicted = predict::update_funding(&m, &self.ov(), self.now);
        if !self.should_send("update_funding", predicted) {
            return;
        }
        let ix = self.update_funding_ix();
        let kp = self.keeper.kp.insecure_clone();
        if self.run("update_funding", &[ix], &[&kp], predicted).await.is_err() {
            return;
        }
        let after = self.market().await;
        let rate = model::funding_rate(m.long_open_interest, m.short_open_interest);
        let step = model::funding_step(rate, self.px);
        let (tick, now) = (self.tick, self.now);
        self.checks.check(tick, now, "9_settlement_accounting", after.funding_rate == rate && after.cumulative_funding == m.cumulative_funding + step && after.last_funding_time == now, || {
            format!("update_funding: expected rate {} (+{} per contract), got rate {} cumulative {} -> {}", rate, step, after.funding_rate, m.cumulative_funding, after.cumulative_funding)
        });
        let elapsed = now - m.last_funding_time;
        // One period is applied per crank whatever the elapsed time
        self.metrics.funding_periods_dropped += (elapsed / m.funding_interval - 1).max(0) as u64;
        self.hit(match rate.signum() {
            1 => "update_funding.longs_pay",
            -1 => "update_funding.shorts_pay",
            _ => "update_funding.zero",
        });
        self.hit("update_funding");
        // Longs pay size x step; shorts receive it. With unequal OI the vault keeps the difference.
        let paid_by_longs = m.long_open_interest as i128 * step as i128 / 1_000_000;
        let paid_to_shorts = m.short_open_interest as i128 * step as i128 / 1_000_000;
        self.metrics.funding_imbalance += paid_by_longs - paid_to_shorts;
        self.metrics.funding_updates.push(json!({
            "time": now, "rate": rate, "price": self.px, "elapsed_secs": elapsed,
            "long_oi": m.long_open_interest, "short_oi": m.short_open_interest,
            "paid_by_longs_usdc": paid_by_longs as f64 / 1e6, "paid_to_shorts_usdc": paid_to_shorts as f64 / 1e6,
        }));
    }

    // --- liquidations ------------------------------------------------------------------

    async fn liquidate_all(&mut self) -> LiquidationFailures {
        let mut failed = LiquidationFailures::new();
        let m = self.market().await;
        let candidates: Vec<usize> = self
            .ledger
            .open
            .iter()
            .copied()
            .filter(|&idx| model::liquidatable(&m, &self.ledger.positions[idx].state, self.px, self.now))
            .collect();
        for idx in candidates {
            if let Err(why) = self.liquidate_one(idx, "liquidate").await {
                *self.metrics.liquidation_failures.entry(why.clone()).or_default() += 1;
                failed.insert(idx, why);
            }
        }
        failed
    }

    /// Healthy positions closest to maintenance: liquidate must refuse them.
    async fn probe_healthy(&mut self) {
        let m = self.market().await;
        let (_, maintenance) = model::margin_ratios(&m, self.now);
        let mut near: Vec<(i128, usize)> = self
            .ledger
            .open
            .iter()
            .map(|&idx| (model::margin_ratio_bps(&self.ledger.positions[idx].state, self.px, m.cumulative_funding), idx))
            .filter(|&(r, idx)| r >= maintenance as i128 && r < maintenance as i128 + PROBE_WITHIN_BPS && self.metrics.last_probe.get(&idx).map_or(true, |&t| t + 12 <= self.tick))
            .collect();
        near.sort();
        for (_, idx) in near.into_iter().take(PROBES_PER_TICK) {
            self.metrics.last_probe.insert(idx, self.tick);
            let _ = self.liquidate_one(idx, "liquidate_probe").await;
        }
    }

    /// At a session edge: one liquidation of the weakest position, which must fail with MarketClosed.
    async fn probe_edge(&mut self) {
        let m = self.market().await;
        let weakest = self.ledger.open.iter().copied().min_by_key(|&idx| model::margin_ratio_bps(&self.ledger.positions[idx].state, self.px, m.cumulative_funding));
        if let Some(idx) = weakest {
            let _ = self.liquidate_one(idx, "liquidate_probe").await;
        }
    }

    /// Liquidate tracked position `idx` with the model's prediction, checking
    /// the accounting if it lands. Err: the failure's `program:code`.
    pub async fn liquidate_one(&mut self, idx: usize, label: &str) -> Result<(), String> {
        let address = self.ledger.positions[idx].address;
        let owner = self.ledger.positions[idx].owner;
        let owner_key = self.agents[owner].t.key();
        let p: Position = load(self.chain.base(), address).await;
        let m = self.market().await;
        let predicted: Option<Rej> = predict::liquidate(&m, &self.ov(), self.now, &p);
        if !self.should_send(label, predicted) {
            return Err(predicted.map_or("skipped".into(), |(a, b)| format!("{}:{}", a, b)));
        }
        let wallet = token_balance(self.chain.base(), self.keeper.tokens).await;
        let ix = self.liquidate_ix(&owner_key, address);
        let kp = self.keeper.kp.insecure_clone();
        let ok = self.run(label, &[ix], &[&kp], predicted).await.map_err(|f| f.key())?;
        let (tick, now, px) = (self.tick, self.now, self.px);
        let expected = model::liquidation(&m, &p, px);
        let after_p: Position = load(self.chain.base(), address).await;
        let after_m = self.market().await;
        let reward = token_balance(self.chain.base(), self.keeper.tokens).await as i128 - wallet as i128;
        let side_oi = |mk: &perps_core::state::Market| if p.side == Side::Long { mk.long_open_interest } else { mk.short_open_interest };
        let fine = reward == expected.reward as i128
            && after_m.insurance_fund == expected.fund_after
            && after_p.status == PositionStatus::Liquidated
            && after_p.realized_pnl as i128 == expected.realized
            && side_oi(&after_m) == side_oi(&m) - p.size;
        self.checks.check(tick, now, "9_settlement_accounting", fine, || {
            format!("liquidation of {} ({:?} {} @ {}, collateral {}) at {}: expected {:?}, reward {}, realized {}, fund {} -> {}", address, p.side as u8, p.size, p.entry_price, p.collateral, px, expected, reward, after_p.realized_pnl, m.insurance_fund, after_m.insurance_fund)
        });
        self.check_bad_debt_log("liquidate", &ok.logs, expected.deficit, expected.uncovered);
        // 2b: the vault pays what the model says it pays
        self.vault_expected -= expected.reward as i128;
        self.ledger.update(idx, after_p);
        let settlement = Settlement {
            liquidated: true, paid: reward, insurance_delta: after_m.insurance_fund as i128 - m.insurance_fund as i128,
            collateral: p.collateral, mark: px, cumulative_funding: m.cumulative_funding, uncovered: expected.uncovered,
        };
        self.ledger.settle(idx, settlement);
        self.check_pair(idx).await;
        let route = self.ledger.positions[idx].route;
        let (_, maintenance) = model::margin_ratios(&m, now);
        self.metrics.liquidation_events.push(json!({
            "time": now, "tick": tick, "owner": self.agents[owner].name, "route": route.name(), "side": p.side as u8,
            "size_contracts": p.size as f64 / 1e6, "leverage_x": p.leverage as f64 / 1e3, "held_ticks": tick - self.ledger.positions[idx].opened_tick,
            "margin_bps": model::margin_ratio_bps(&p, px, m.cumulative_funding), "maintenance_bps": maintenance,
            "pre_close_window": maintenance != m.maintenance_margin_ratio, "reward_usdc": expected.reward as f64 / 1e6,
            "deficit_usdc": expected.deficit as f64 / 1e6, "reward_top_up_usdc": expected.top_up as f64 / 1e6,
        }));
        self.metrics.liquidations += 1;
        *self.metrics.liquidations_by_route.entry(route).or_default() += 1;
        self.metrics.liquidation_rewards += expected.reward;
        self.hit("liquidate");
        if expected.top_up > 0 {
            self.hit("liquidate.reward_top_up");
        }
        if expected.deficit > 0 {
            self.hit("liquidate.deficit");
            self.record_bad_debt("liquidation", idx, expected.deficit, expected.uncovered, expected.top_up, after_m.insurance_fund);
        }
        self.agents[owner].stats.liquidated += 1;
        Ok(())
    }

    // --- expired orders ------------------------------------------------------------------

    async fn close_expired_orders(&mut self) {
        for o in 0..self.ledger.orders.len() {
            let order = &self.ledger.orders[o];
            if !order.live || self.now <= order.expires_at {
                continue;
            }
            let (address, owner) = (order.address, self.agents[order.owner].t.key());
            // Permissionless: no signer but the fee payer
            let ix = self.close_expired_ix(&owner, address);
            if self.run("close_expired_order", &[ix], &[], None).await.is_ok() {
                self.ledger.orders[o].live = false;
                self.ledger.orders[o].end = Some(OrderEnd::Expired);
                self.metrics.expired_orders_closed += 1;
                self.hit("order.closed_expired");
            }
        }
    }
}
