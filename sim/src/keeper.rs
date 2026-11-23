//! The keeper, every tick in session: match crossing orderbook orders (price,
//! then sequence, split under the venue's per-fill cap), crank funding when
//! an interval of open time is due, liquidate every position the model calls
//! liquidatable (stage-1 slices again until healthy), run deficit-mode ADL
//! while bad debt is pending and exposure-mode ADL while a trigger holds,
//! settle losers' PnL into pool cash, write off stale debt, sweep insurance
//! above its target, probe a few healthy positions near maintenance (the
//! engine must refuse them), and close expired orders. On a settled market
//! it closes every position at the settlement price, losers first, and
//! sweeps the fund once none is left. On the first tick after a close (a
//! session edge) it runs its pass as a keeper that has not noticed the close
//! would, and the engine must refuse with MarketClosed.

use crate::actions::{FillPlan, FillPre};
use crate::ledger::{OrderEnd, Route, Settlement};
use crate::model::{self, Deficit};
use crate::predict::{self, Leg, Rej, Terms};
use crate::scenario::Scenario;
use crate::support::*;
use crate::world::Sim;
use orderbook::{Order, OrderSide, OrderStatus};
use perps_core::state::{Position, Side, VenueApproval};
use serde_json::json;
use std::collections::{HashMap, HashSet};

/// Liquidations the keeper attempted and could not do this tick, with the error.
pub type LiquidationFailures = HashMap<usize, String>;

const MAX_MATCHES_PER_TICK: usize = 40;
/// Liquidations of one position per pass (stage-1 slices until it is healthy)
const MAX_LIQUIDATIONS_PER_POSITION: usize = 6;
/// ADL slices per pass
const MAX_ADL_PER_TICK: usize = 20;
/// settle_position_pnl: every this many ticks, the largest losers
const SETTLE_EVERY_TICKS: u64 = 3;
const SETTLE_PER_PASS: usize = 6;
/// Healthy positions probed with liquidate per tick, and how close to maintenance (bps)
const PROBES_PER_TICK: usize = 2;
const PROBE_WITHIN_BPS: i128 = 150;
/// Unsplit crosses over the per-fill cap the keeper sends to see FillTooLarge (stress)
const UNSPLIT_PROBES: u64 = 5;

impl Sim {
    pub async fn keeper_pass(&mut self) -> LiquidationFailures {
        let mut failed = LiquidationFailures::new();
        if self.settled {
            self.settled_pass().await;
        } else if self.in_session() {
            self.match_book().await;
            self.crank_funding().await;
            failed = self.liquidate_all().await;
            self.adl_pass().await;
            self.settle_pnl_pass(false).await;
            self.write_off_pass().await;
            self.sweep_pass().await;
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
            st: model::St { m, pool: self.pool().await },
        };
        let kp = self.keeper.kp.insecure_clone();
        let Ok(ok) = self.run(label, &[compute_budget_ix(400_000), ix], &[&kp], predicted).await else {
            *self.metrics.match_skips.entry("failed".into()).or_default() += 1;
            return false;
        };
        self.metrics.matches += 1;
        let plan = FillPlan {
            route: Route::Orderbook, taker, maker, taker_side: if taker_is_bid { Side::Long } else { Side::Short },
            size: fill, price: maker_order.price, taker_leverage: taker_order.leverage, maker_leverage: maker_order.leverage,
            collusion: self.adversary.colluding_orders(bid_addr, ask_addr),
        };
        self.settle_fill(plan, pre, fa, &ok.logs).await;
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

    /// update_funding when an interval of open time (plus the carry) has
    /// passed since the last crank (§8). Inline cranks by trades (D40) reset
    /// the clock too, so the keeper's crank is often not needed.
    pub async fn crank_funding(&mut self) {
        let m = self.market().await;
        if model::settled(&m) || model::funding_open_total(&m, self.now) < m.funding_interval {
            return;
        }
        let mut st = self.st().await;
        let res = model::update_funding(&mut st.m, &self.ov(), self.now);
        let predicted = res.err();
        if !self.should_send("update_funding", predicted) {
            return;
        }
        let ix = self.update_funding_ix();
        let kp = self.keeper.kp.insecure_clone();
        let Ok(ok) = self.run("update_funding", &[ix], &[&kp], predicted).await else { return };
        let Ok(ev) = res else {
            self.resync().await;
            return;
        };
        self.expect_st("update_funding", &st).await;
        self.check_events("update_funding", &ok.logs, Some(ev), &[]);
        self.hit(match ev.rate.signum() {
            1 => "update_funding.longs_pay",
            -1 => "update_funding.shorts_pay",
            _ => "update_funding.zero",
        });
        self.hit("update_funding");
        if ev.n > 1 {
            self.hit("update_funding.catch_up");
        }
        // Longs pay size x n x step; shorts receive it. The difference is the pool's carry (§2.3.5)
        let total_step = ev.n as i128 * ev.step as i128;
        let paid_by_longs = m.long_open_interest as i128 * total_step / 1_000_000;
        let paid_to_shorts = m.short_open_interest as i128 * total_step / 1_000_000;
        self.metrics.funding_imbalance += paid_by_longs - paid_to_shorts;
    }

    // --- liquidations ------------------------------------------------------------------

    pub async fn liquidate_all(&mut self) -> LiquidationFailures {
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
            // A stage-1 slice leaves the position open; one whose haircut
            // left it under maintenance is liquidated again (§6.3)
            for _ in 0..MAX_LIQUIDATIONS_PER_POSITION {
                if let Err(why) = self.liquidate_one(idx, "liquidate").await {
                    *self.metrics.liquidation_failures.entry(why.clone()).or_default() += 1;
                    failed.insert(idx, why);
                    break;
                }
                let m = self.market().await;
                let p = &self.ledger.positions[idx];
                if !p.is_open() || !model::liquidatable(&m, &p.state, self.px, self.now) {
                    break;
                }
            }
        }
        failed
    }

    /// Healthy positions closest to maintenance (on mark notional): liquidate must refuse them.
    async fn probe_healthy(&mut self) {
        let m = self.market().await;
        let (_, maintenance) = model::margin_ratios(&m, self.now);
        let mut near: Vec<(i128, usize)> = self
            .ledger
            .open
            .iter()
            .map(|&idx| (model::margin_ratio_mark_bps(&self.ledger.positions[idx].state, self.px, m.cumulative_funding), idx))
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
        let weakest = self.ledger.open.iter().copied().min_by_key(|&idx| model::margin_ratio_mark_bps(&self.ledger.positions[idx].state, self.px, m.cumulative_funding));
        if let Some(idx) = weakest {
            let _ = self.liquidate_one(idx, "liquidate_probe").await;
        }
    }

    /// Liquidate tracked position `idx` with the model's prediction, checking
    /// the accounting if it lands. Err: the failure's `program:code`.
    pub async fn liquidate_one(&mut self, idx: usize, label: &str) -> Result<(), String> {
        let (kp, tokens) = (self.keeper.kp.insecure_clone(), self.keeper.tokens);
        self.liquidate_by(idx, label, &kp, tokens).await.map(|_| ())
    }

    /// `liquidate_one` with `liquidator` signing and its reward paid to
    /// `reward_to`. Ok: the model's outcome (None if it landed against a
    /// predicted rejection).
    pub async fn liquidate_by(&mut self, idx: usize, label: &str, liquidator: &Keypair, reward_to: Pubkey) -> Result<Option<model::LiqOut>, String> {
        let address = self.ledger.positions[idx].address;
        let owner = self.ledger.positions[idx].owner;
        let owner_key = self.agents[owner].t.key();
        let user = self.agents[owner].t.user;
        let before: Position = load(self.chain.base(), address).await;
        let (mut st, mut u, mut p) = (self.st().await, self.user_at(user).await, before.clone());
        let m0 = st.m.clone();
        let m0_target = st.pool.params.insurance_target;
        let res = model::liquidate(&mut st, &mut p, &mut u, &self.ov(), self.now);
        let predicted: Option<Rej> = res.as_ref().err().copied();
        if !self.should_send(label, predicted) {
            return Err(predicted.map_or("skipped".into(), |(a, b)| format!("{}:{}", a, b)));
        }
        let wallet = token_balance(self.chain.base(), reward_to).await;
        let ix = self.liquidate_ix(&liquidator.pubkey(), reward_to, &owner_key, address);
        let ok = self.run(label, &[ix], &[liquidator], predicted).await.map_err(|f| f.key())?;
        let reward = token_balance(self.chain.base(), reward_to).await as i128 - wallet as i128;
        let Ok(out) = res else {
            self.resync().await;
            self.vault_expected -= reward;
            let after: Position = load(self.chain.base(), address).await;
            self.ledger.update(idx, after);
            return Ok(None);
        };
        let (tick, now, px) = (self.tick, self.now, self.px);
        self.expect_st(label, &st).await;
        self.expect_user(label, user, &u).await;
        let after_p = self.expect_position(label, address, &p).await;
        let deficits: Vec<Deficit> = out.deficit.into_iter().collect();
        self.check_events(label, &ok.logs, out.funding, &deficits);
        self.checks.check(tick, now, "9_settlement_accounting", reward == out.keeper_reward as i128, || {
            format!("liquidation of {}: keeper got {}, model {} ({:?})", address, reward, out.keeper_reward, out)
        });
        // 2b: the vault pays what the model says it pays
        self.vault_expected -= out.keeper_reward as i128;
        self.ledger.update(idx, after_p.clone());
        // One settlement piece: what left the position, against what it realized
        let released = before.collateral as i128 - if out.size_remaining > 0 { after_p.collateral as i128 } else { 0 };
        let c_r = before.collateral as i128 + out.r;
        let fees = match (out.stage, out.deficit) {
            (1, _) => (out.penalty - out.keeper_reward) as i128,
            // Stage 2: what the protocol kept, measured on chain (the change
            // in the insurance fund plus the top-up it paid), plus the model's
            // overflow to LP cash past the insurance target (0 below it). The
            // identity then checks the chain-measured reward and insurance
            // flows against the realized PnL (third review), instead of
            // being c_r − (reward − top-up) by construction.
            (_, None) => {
                let ins_after = self.shadow.as_ref().map_or(m0.insurance_fund, |s| s.m.insurance_fund) as i128;
                let y = (c_r - (out.keeper_reward - out.top_up) as i128).max(0) as u64;
                let (_, overflow) = crate::pool_math::insurance_split(m0.insurance_fund, m0_target, y);
                ins_after - m0.insurance_fund as i128 + out.top_up as i128 + overflow as i128
            }
            _ => 0,
        };
        let piece = Settlement {
            liquidated: true, paid: reward + out.owner_leftover as i128, fees, top_up: out.top_up as i128, collateral: released,
            realized: out.realized, haircut: (out.solvency_hc + out.bad_debt_hc + out.cash_hc) as i128,
            deficit: out.deficit.map_or(0, |d| d.d as i128), insurance_draw: out.top_up as i128 + out.deficit.map_or(0, |d| d.ins_paid as i128),
            mark: px, oracle: px, cumulative_funding: st.m.cumulative_funding, pieces: 1, done: out.size_remaining == 0,
        };
        self.checks.check(tick, now, "4_pair_settlement", piece.balanced(), || {
            format!("liquidation of {}: paid {} + fees {} - top-up {} != collateral {} + realized {} - haircut {} + deficit {}", address, piece.paid, piece.fees, piece.top_up, piece.collateral, piece.realized, piece.haircut, piece.deficit)
        });
        self.ledger.settle(idx, piece);
        self.pool_stats.haircut("solvency", out.solvency_hc);
        self.pool_stats.haircut("bad_debt", out.bad_debt_hc);
        self.pool_stats.haircut("cash", out.cash_hc);
        self.check_pair(idx).await;
        let route = self.ledger.positions[idx].route;
        let (_, maintenance) = model::margin_ratios(&m0, now);
        let kind = match (out.stage, out.size_remaining > 0) {
            (1, true) => "stage1_partial",
            (1, false) => "stage1_full",
            _ => "stage2",
        };
        *self.pool_stats.liquidation_stages.entry(kind.into()).or_default() += 1;
        self.metrics.liquidation_events.push(json!({
            "time": now, "tick": tick, "owner": self.agents[owner].name, "route": route.name(), "side": before.side as u8, "stage": kind,
            "size_contracts": before.size as f64 / 1e6, "closed_contracts": out.size_closed as f64 / 1e6, "leverage_x": before.leverage as f64 / 1e3,
            "held_ticks": tick - self.ledger.positions[idx].opened_tick,
            "margin_bps": model::margin_ratio_mark_bps(&before, px, m0.cumulative_funding), "maintenance_bps": maintenance,
            "pre_close_window": maintenance != m0.maintenance_margin_ratio, "reward_usdc": out.keeper_reward as f64 / 1e6,
            "penalty_usdc": out.penalty as f64 / 1e6, "owner_leftover_usdc": out.owner_leftover as f64 / 1e6,
            "deficit_usdc": out.deficit.map_or(0.0, |d| d.d as f64 / 1e6), "reward_top_up_usdc": out.top_up as f64 / 1e6,
        }));
        self.metrics.liquidations += 1;
        *self.metrics.liquidations_by_route.entry(route).or_default() += 1;
        self.metrics.liquidation_rewards += out.keeper_reward;
        self.hit("liquidate");
        self.hit(format!("liquidate.{}", kind));
        if out.top_up > 0 {
            self.hit("liquidate.reward_top_up");
        }
        if let Some(d) = out.deficit {
            self.hit("liquidate.deficit");
            self.record_bad_debt("liquidation", idx, &d);
        }
        if out.size_remaining == 0 {
            self.agents[owner].stats.liquidated += 1;
        }
        Ok(Some(out))
    }

    // --- ADL (§7) ------------------------------------------------------------------------

    /// Deficit-mode ADL while bad debt is pending: the highest-score
    /// opposite-side winner the decaying floor admits, sliced by the engine
    /// to what the pending needs (D37). Exposure mode while a trigger holds:
    /// the highest-score net-side winner, sized to land on the target.
    pub async fn adl_pass(&mut self) {
        for _ in 0..MAX_ADL_PER_TICK {
            let st = self.st().await;
            let required = st.pool.pending_bad_debt_long > 0 || st.pool.pending_bad_debt_short > 0;
            let mode = if required {
                model::ADL_DEFICIT
            } else {
                let Ok(v) = model::view(&st.pool, &st.m, self.px, self.now) else { return };
                if model::settled(&st.m) || model::exposure_triggers(&st.m, &st.pool, &v, self.now).0 == 0 {
                    return;
                }
                model::ADL_EXPOSURE
            };
            let Some((idx, size)) = self.adl_candidate(&st, mode).await else {
                if required {
                    self.hit("adl.no_eligible_candidate");
                }
                return;
            };
            if !self.adl_one(idx, mode, size, "adl").await {
                return;
            }
        }
    }

    /// The best eligible candidate for `mode` and the size to send, by the model.
    async fn adl_candidate(&mut self, st: &model::St, mode: u8) -> Option<(usize, u64)> {
        let o = self.ov();
        let mut best: Option<(u128, usize, u64)> = None;
        for &idx in &self.ledger.open {
            // Positions the adversarial scenario holds for its probes: the
            // keeper picks among the eligible, and leaves these alone
            if self.adversary.held.contains(&idx) {
                continue;
            }
            let p = self.ledger.positions[idx].state.clone();
            let size = if mode == model::ADL_DEFICIT { 0 } else { self.exposure_size(st, &p)? };
            if size == u64::MAX {
                continue;
            }
            let (mut s2, mut p2, mut u2) = (st.clone(), p.clone(), perps_core::state::UserAccount { owner: p.owner, ..Default::default() });
            // House notional only matters for the effects, not eligibility
            u2.house_entry_notional = u64::MAX / 2;
            let Ok(out) = model::adl(&mut s2, &mut p2, &mut u2, mode, size, true, &o, self.now) else { continue };
            if best.map_or(true, |(score, _, _)| out.score > score) {
                best = Some((out.score, idx, size));
            }
        }
        best.map(|(_, idx, size)| (idx, size))
    }

    /// Exposure mode: the slice of `p` that brings |net| down to the target
    /// (u64::MAX: not on the net side). The engine refuses an overshoot.
    fn exposure_size(&self, st: &model::St, p: &Position) -> Option<u64> {
        let v = model::view(&st.pool, &st.m, self.px, self.now).ok()?;
        let (_, target) = model::exposure_triggers(&st.m, &st.pool, &v, self.now);
        let on_side = (v.net > 0 && p.side == Side::Long) || (v.net < 0 && p.side == Side::Short);
        if !on_side {
            return Some(u64::MAX);
        }
        // Smallest |net| whose ceil notional still reaches the target
        let keep = if target == 0 { 0 } else { (target - 1) * 1_000_000 / self.px as u128 + 1 };
        let room = v.net.unsigned_abs().saturating_sub(keep);
        if room == 0 {
            return Some(u64::MAX);
        }
        Some((room.min(p.size as u128)) as u64)
    }

    /// adl of tracked position `idx`, predicted and checked by the model.
    pub async fn adl_one(&mut self, idx: usize, mode: u8, size: u64, label: &str) -> bool {
        // Mode 1 is signed by the pool's adl_keeper, the admin key here
        let signer = if mode == model::ADL_EXPOSURE { self.chain.base.admin.insecure_clone() } else { self.keeper.kp.insecure_clone() };
        self.adl_by(idx, mode, size, label, &signer).await.is_some()
    }

    /// adl signed by `signer`, predicted and checked by the model. Some:
    /// it landed (with the model's outcome, None if the model predicted a
    /// rejection).
    pub async fn adl_by(&mut self, idx: usize, mode: u8, size: u64, label: &str, signer: &Keypair) -> Option<Option<model::AdlOut>> {
        let address = self.ledger.positions[idx].address;
        let owner = self.ledger.positions[idx].owner;
        let owner_key = self.agents[owner].t.key();
        let user = self.agents[owner].t.user;
        let before: Position = load(self.chain.base(), address).await;
        let (mut st, mut u, mut p) = (self.st().await, self.user_at(user).await, before.clone());
        // Mode 1: the pool's adl_keeper or the admin
        let authorized = signer.pubkey() == st.pool.adl_keeper || signer.pubkey() == self.chain.base.admin.pubkey();
        let res = model::adl(&mut st, &mut p, &mut u, mode, size, authorized, &self.ov(), self.now);
        let predicted = res.as_ref().err().copied();
        if !self.should_send(label, predicted) {
            return None;
        }
        let ix = self.adl_ix(&signer.pubkey(), &owner_key, address, mode, size);
        let Ok(ok) = self.run(label, &[ix], &[signer], predicted).await else { return None };
        let Ok(out) = res else {
            self.resync().await;
            let after: Position = load(self.chain.base(), address).await;
            self.ledger.update(idx, after);
            return Some(None);
        };
        let (tick, now) = (self.tick, self.now);
        self.expect_st(label, &st).await;
        self.expect_user(label, user, &u).await;
        let after = self.expect_position(label, address, &p).await;
        self.check_events(label, &ok.logs, out.funding, &[]);
        let got: Vec<(u8, u64, u64, u64, u64)> = crate::chain::events::<perps_core::events::Adl>(&ok.logs).iter().map(|e| (e.mode, e.size_closed, e.profit, e.bad_debt_hc, e.fee)).collect();
        let want = vec![(out.mode, out.x, out.paid, out.bad_debt_hc, out.fee)];
        self.checks.check(tick, now, "9_settlement_accounting", got == want, || format!("adl of {}: event (mode, size, profit, bad-debt hc, fee) {:?}, model {:?}", address, got, want));
        self.ledger.update(idx, after.clone());
        let released = before.collateral as i128 - if out.x < before.size { after.collateral as i128 } else { 0 };
        let piece = Settlement {
            liquidated: false, paid: out.credit as i128, fees: out.fee as i128, top_up: 0, collateral: released, realized: out.profit as i128,
            haircut: (out.solvency_hc + out.bad_debt_hc) as i128, deficit: 0, insurance_draw: 0, mark: out.mark, oracle: out.mark,
            cumulative_funding: st.m.cumulative_funding, pieces: 1, done: out.x == before.size,
        };
        self.checks.check(tick, now, "4_pair_settlement", piece.balanced(), || format!("adl of {}: {:?} does not balance", address, piece));
        self.ledger.settle(idx, piece);
        self.pool_stats.haircut("solvency", out.solvency_hc);
        self.pool_stats.haircut("bad_debt", out.bad_debt_hc);
        self.check_pair(idx).await;
        let kind = format!("mode{}.triggers{}", out.mode, out.triggers);
        *self.pool_stats.adl_by_kind.entry(kind.clone()).or_default() += 1;
        self.pool_stats.adl_events.push(json!({
            "time": now, "tick": tick, "kind": kind, "owner": self.agents[owner].name, "size_closed": out.x as f64 / 1e6, "full": out.x == before.size,
            "profit_usdc": out.paid as f64 / 1e6, "bad_debt_hc_usdc": out.bad_debt_hc as f64 / 1e6, "fee_usdc": out.fee as f64 / 1e6,
            "score": out.score as f64, "floor": out.floor, "net_before": out.net_before as f64 / 1e6, "net_after": out.net_after as f64 / 1e6,
        }));
        self.hit(format!("adl.mode{}", out.mode));
        if out.x < before.size {
            self.hit("adl.partial");
        }
        Some(Some(out))
    }

    // --- settle_position_pnl, write-off, sweep, settled closes ---------------------------

    /// Every few ticks, or whenever the winners' unrealized profit is more
    /// than half the pool's cash (D3, R5): move the largest losers' losses
    /// into cash. `force` runs it now (the unwind).
    pub async fn settle_pnl_pass(&mut self, force: bool) {
        let m = self.market().await;
        let cash = self.pool().await.cash as i128;
        let mark = predict::close_mark(&m, &self.ov());
        let mut winners = 0i128;
        let mut losers: Vec<(i128, usize)> = Vec::new();
        for &idx in &self.ledger.open {
            let p = &self.ledger.positions[idx].state;
            let r = model::realized(p, mark, m.cumulative_funding);
            if r > 0 {
                winners += r;
            } else if r < 0 && -r < p.collateral as i128 {
                losers.push((r, idx));
            }
        }
        if !force && self.tick % SETTLE_EVERY_TICKS != 0 && winners * 2 <= cash {
            return;
        }
        losers.sort();
        for (_, idx) in losers.into_iter().take(if force { usize::MAX } else { SETTLE_PER_PASS }) {
            self.settle_pnl_one(idx, "settle_pnl").await;
        }
    }

    pub async fn settle_pnl_one(&mut self, idx: usize, label: &str) -> bool {
        let address = self.ledger.positions[idx].address;
        let before: Position = load(self.chain.base(), address).await;
        let (mut st, mut p) = (self.st().await, before.clone());
        let res = model::settle_pnl(&mut st, &mut p, &self.ov(), self.now);
        let predicted = res.as_ref().err().copied();
        if !self.should_send(label, predicted) {
            return false;
        }
        let ix = self.settle_pnl_ix(address);
        let kp = self.keeper.kp.insecure_clone();
        if self.run(label, &[ix], &[&kp], predicted).await.is_err() {
            return false;
        }
        let Ok(loss) = res else {
            self.resync().await;
            let after: Position = load(self.chain.base(), address).await;
            self.ledger.update(idx, after);
            return true;
        };
        self.expect_st(label, &st).await;
        let after = self.expect_position(label, address, &p).await;
        self.ledger.update(idx, after);
        let piece = Settlement {
            collateral: loss as i128, realized: -(loss as i128), mark: p.entry_price, oracle: p.entry_price,
            cumulative_funding: st.m.cumulative_funding, pieces: 1, ..Default::default()
        };
        self.ledger.settle(idx, piece);
        self.pool_stats.settle_pnl.0 += 1;
        self.pool_stats.settle_pnl.1 += loss;
        self.hit("settle_position_pnl");
        true
    }

    async fn write_off_pass(&mut self) {
        let mut st = self.st().await;
        if !model::write_off_due(&st, self.now) {
            return;
        }
        let res = model::write_off(&mut st, self.now);
        let predicted = res.as_ref().err().copied();
        if !self.should_send("write_off_bad_debt", predicted) {
            return;
        }
        let ix = self.write_off_ix();
        let kp = self.keeper.kp.insecure_clone();
        if self.run("write_off_bad_debt", &[ix], &[&kp], predicted).await.is_err() {
            return;
        }
        match res {
            Ok(w) => {
                self.expect_st("write_off_bad_debt", &st).await;
                self.pool_stats.written_off += w;
                self.hit("write_off_bad_debt");
            }
            Err(_) => self.resync().await,
        }
    }

    pub async fn sweep_pass(&mut self) {
        let mut st = self.st().await;
        let target = st.pool.params.insurance_target;
        let all = model::settled(&st.m) && st.pool.open_positions == 0;
        if st.m.insurance_fund == 0 || (!all && st.m.insurance_fund <= target) {
            return;
        }
        let res = model::sweep(&mut st, self.now);
        let predicted = res.as_ref().err().copied();
        if !self.should_send("sweep_insurance_excess", predicted) {
            return;
        }
        // The adversarial scenario's LP sandwich around the settled whole-fund sweep
        let sandwich = if all { self.sweep_sandwich_value().await } else { None };
        let ix = self.sweep_ix();
        let kp = self.keeper.kp.insecure_clone();
        if self.run("sweep_insurance_excess", &[ix], &[&kp], predicted).await.is_err() {
            return;
        }
        match res {
            Ok((x, recovered, _)) => {
                self.expect_st("sweep_insurance_excess", &st).await;
                self.pool_stats.swept += x;
                self.hit(if all { "sweep.settled_all" } else { "sweep.excess" });
                if let Some((value, lp_shares)) = sandwich {
                    self.check_sweep_sandwich(value, recovered, lp_shares).await;
                }
            }
            Err(_) => self.resync().await,
        }
    }

    /// Settled market: close every open position at the settlement price,
    /// losers first (winners are paid from the cash they bring), then ADL
    /// and the sweep.
    pub async fn settled_pass(&mut self) {
        let m = self.market().await;
        let mark = m.settlement_price;
        let mut order: Vec<(i128, usize)> = self.ledger.open.iter().map(|&idx| (model::realized(&self.ledger.positions[idx].state, mark, m.cumulative_funding), idx)).collect();
        order.sort();
        for (_, idx) in order {
            self.close_settled_one(idx).await;
        }
        self.adl_pass().await;
        self.write_off_pass().await;
        self.sweep_pass().await;
    }

    pub async fn close_settled_one(&mut self, idx: usize) -> bool {
        let label = "close_settled_position";
        let address = self.ledger.positions[idx].address;
        let owner = self.ledger.positions[idx].owner;
        let owner_key = self.agents[owner].t.key();
        let user = self.agents[owner].t.user;
        let before: Position = load(self.chain.base(), address).await;
        let (mut st, mut u, mut p) = (self.st().await, self.user_at(user).await, before.clone());
        let res = model::close_settled(&mut st, &mut p, &mut u, self.now);
        let predicted = res.as_ref().err().copied();
        if !self.should_send(label, predicted) {
            return false;
        }
        let free_before = self.user_at(user).await.collateral_balance;
        let ix = self.close_settled_ix(&owner_key, address);
        let kp = self.keeper.kp.insecure_clone();
        let Ok(ok) = self.run(label, &[ix], &[&kp], predicted).await else { return false };
        let Ok(out) = res else {
            self.resync().await;
            let after: Position = load(self.chain.base(), address).await;
            self.ledger.update(idx, after);
            return true;
        };
        self.expect_st(label, &st).await;
        self.expect_user(label, user, &u).await;
        let after = self.expect_position(label, address, &p).await;
        let deficits: Vec<Deficit> = out.deficit.into_iter().collect();
        self.check_events(label, &ok.logs, None, &deficits);
        let paid = self.user_at(user).await.collateral_balance as i128 - free_before as i128;
        self.ledger.update(idx, after);
        self.record_close_piece(idx, &before, &out, paid, st.m.cumulative_funding, true);
        self.check_pair(idx).await;
        self.metrics.record_close(out.fee, out.deficit.map_or(0, |d| d.d));
        self.hit("close_settled_position");
        if let Some(d) = out.deficit {
            self.record_bad_debt("settled_close", idx, &d);
        }
        true
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
