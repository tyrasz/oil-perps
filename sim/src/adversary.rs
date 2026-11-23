//! The adversarial scenario's scripted actors, run each in-session tick after
//! the agents and before the keeper:
//!
//! - collusion: colluder-a and colluder-b cross at the band edge at the
//!   highest leverage the market allows, on mm-registry (a quotes, b fills)
//!   and on the orderbook (matched by a matcher that serves them), then close
//!   both legs together. Spec 2.1/2.2: "no position is born below IM, so
//!   collusion creates no bad debt" (invariants 8 and 12);
//! - wash trading: the same pair crossing at the oracle and closing at once;
//! - the prober: a taker trying the side its approval excludes, an expired
//!   approval, approval leverage and limit bounds, a fill against an MM's
//!   exhausted (mass-cancelled) and revoked maker approval, self-trades on
//!   both venues, an expired quote, a fill worse than its limit, add_margin
//!   and withdrawals beyond free collateral and a withdrawal with positions open;
//! - the guardian pauses the market mid-run (opens and fills must fail with
//!   MarketPaused; closes, liquidations and funding go on), and the admin
//!   unpauses it two hours later. settle_market at the end is in main.rs.

use crate::actions::Take;
use crate::agents::Persona;
use crate::model;
use crate::predict::{self, Rej};
use crate::support::*;
use crate::world::{maker_approval, Sim};
use mm_registry::instructions::PostQuoteParams;
use mm_registry::{MarketMaker, TwoSidedQuote};
use orderbook::{Order, OrderBook, OrderSide, OrderType, PlaceOrderParams};
use perps_core::instructions::VenueApprovalParams;
use perps_core::state::{Side, VenueApproval};
use rand::Rng;
use std::collections::HashSet;

#[derive(Default)]
pub struct AdversaryState {
    /// Orders the colluders placed for each other
    pub orders: HashSet<Pubkey>,
    pub steps: u64,
    pub paused_at: Option<u64>,
    pub unpaused: bool,
    /// The prober's short-lived approval, waiting to expire
    pub expiring_approval: Option<i64>,
    /// colluder-a's short-lived quote, waiting to expire
    pub expiring_quote: Option<(Pubkey, i64)>,
}

impl AdversaryState {
    pub fn colluding_orders(&self, bid: Pubkey, ask: Pubkey) -> bool {
        self.orders.contains(&bid) && self.orders.contains(&ask)
    }
}

const PAUSE_TICKS: u64 = 24;

impl Sim {
    fn agent_named(&self, name: &str) -> usize {
        self.agents.iter().position(|a| a.name == name).expect("scenario agent")
    }

    pub async fn adversary_step(&mut self, total_ticks: u64) {
        self.adversary.steps += 1;
        self.guardian(total_ticks).await;
        let m = self.market().await;
        if !self.oracle_ok(&m, 15) {
            // Rounds and probes need a fresh oracle to mean anything
            return;
        }
        if self.paused {
            self.probe_paused().await;
            return;
        }
        match self.adversary.steps % 6 {
            0 => self.collusion_mm(false).await,
            1 => self.probe_approvals().await,
            2 => self.collusion_book(false).await,
            3 => self.probe_makers().await,
            4 => {
                if self.rng.gen_bool(0.5) {
                    self.collusion_book(true).await
                } else {
                    self.collusion_mm(true).await
                }
            }
            _ => self.probe_misc().await,
        }
    }

    // --- the guardian and the admin ----------------------------------------------------

    async fn guardian(&mut self, total_ticks: u64) {
        let base = &self.chain.base;
        let (admin, guardian, market) = (base.admin.insecure_clone(), base.guardian.insecure_clone(), base.market);
        match self.adversary.paused_at {
            None if self.tick >= total_ticks * 2 / 5 => {
                let accounts = perps_core::accounts::PauseMarket { signer: guardian.pubkey(), config: config_pda(), market };
                let ix = ix(perps_core::ID, accounts, perps_core::instruction::PauseMarket {});
                if self.run("pause_market", &[ix], &[&guardian], None).await.is_ok() {
                    self.paused = true;
                    self.adversary.paused_at = Some(self.tick);
                    self.hit("pause_market");
                }
            }
            Some(at) if !self.adversary.unpaused && self.tick >= at + PAUSE_TICKS => {
                let accounts = perps_core::accounts::UpdateMarketConfig { authority: admin.pubkey(), config: config_pda(), market };
                let ix = ix(perps_core::ID, accounts, perps_core::instruction::UpdateMarketConfig { params: cme_config() });
                if self.run("update_market_config", &[ix], &[&admin], None).await.is_ok() {
                    self.paused = false;
                    self.adversary.unpaused = true;
                    self.hit("unpause_market");
                }
            }
            _ => {}
        }
    }

    /// While paused: opening at the oracle and venue fills must fail with MarketPaused.
    async fn probe_paused(&mut self) {
        let p = self.agent_named("prober");
        self.open_house(p, Side::Long, 10 * CONTRACT, 5_000, "open_position_probe").await;
        self.prober_take(true, 10 * CONTRACT, 5_000, |a| a).await;
    }

    // --- collusion and wash trading ----------------------------------------------------

    /// Colluder-a's maker approval: its whole deposit, no adverse bound.
    async fn colluder_refill(&mut self, a: usize) {
        let owner = self.agents[a].t.key();
        let address = approval_pda(&owner, &self.mm_venue, ROLE_MAKER);
        let budget = self.agents[a].deposit;
        let params = VenueApprovalParams { max_price_deviation_bps: 0, ..maker_approval(budget, self.now) };
        let ix = update_approval_ix(&self.chain.base, &owner, &self.mm_venue, ROLE_MAKER, params);
        let kp = self.agents[a].t.kp.insecure_clone();
        if self.run("update_venue_approval", &[ix], &[&kp], None).await.is_ok() {
            self.ledger.track_approval(address, budget);
        }
    }

    /// The largest size both colluders can margin at `price`, under the per-fill cap.
    fn collusion_size(&self, price: u64, notional: u64) -> u64 {
        (notional as u128 * 1_000_000 / price as u128) as u64 / 1_000 * 1_000
    }

    /// Price at the band's edge, on the side that hands b the immediate gain
    /// when `b_buys`; at the oracle for a wash trade.
    fn edge_price(&self, band_bps: u16, b_buys: bool, wash: bool) -> u64 {
        if wash {
            return self.px;
        }
        let d = (self.px as u128 * band_bps as u128 / 10_000) as u64;
        if b_buys {
            self.px - d
        } else {
            self.px + d
        }
    }

    /// After a colluding fill: the losing leg must not be liquidatable, then both close together.
    async fn finish_collusion(&mut self, pair: usize, wash: bool) {
        let (t, k) = (self.ledger.pairs[pair].taker, self.ledger.pairs[pair].maker);
        let m = self.market().await;
        let loser = if model::equity(&self.ledger.positions[t].state, self.px, m.cumulative_funding) - self.ledger.positions[t].state.collateral as i128 <= 0 { t } else { k };
        let _ = self.liquidate_one(loser, "liquidate_probe").await;
        for idx in [t, k] {
            if self.ledger.positions[idx].is_open() {
                self.close_tracked(idx, "close_position").await;
            }
        }
        self.hit(if wash { "wash.round" } else { "collusion.round" });
    }

    /// colluder-a quotes one side at the mm-registry band edge; colluder-b fills it at max leverage.
    async fn collusion_mm(&mut self, wash: bool) {
        let (a, b) = (self.agent_named("colluder-a"), self.agent_named("colluder-b"));
        self.colluder_refill(a).await;
        let m = self.market().await;
        let leverage = if wash { 5_000 } else { model::max_leverage(&m, self.now) };
        let band = load_venue(self.chain.base(), &mm_registry::ID).await.max_price_deviation_bps;
        let b_buys = self.rng.gen_bool(0.5);
        let price = self.edge_price(band, b_buys, wash);
        let size = self.collusion_size(price, 30_000 * USDC).min(crate::world::MAX_QUOTE_SIZE);
        // One real side; the other is the registry's minimum, far enough not to matter
        let (bid, ask, bid_size, ask_size) = if b_buys { (price - 1, price, CONTRACT, size) } else { (price, price + 1, size, CONTRACT) };
        let owner = self.agents[a].t.key();
        let kp = self.agents[a].t.kp.insecure_clone();
        let market_maker = self.agents[a].mm.as_ref().unwrap().market_maker;
        let index = load::<MarketMaker>(self.chain.base(), market_maker).await.quotes_posted;
        let params = PostQuoteParams { bid_price: bid, bid_size, ask_price: ask, ask_size, min_fill_size: CONTRACT, expires_in: 600, leverage };
        let (ix, quote) = self.post_quote_ix(&owner, index, params);
        if self.run("post_quote", &[ix], &[&kp], None).await.is_err() {
            return;
        }
        self.ledger.record_quote(quote);
        let side = if b_buys { Side::Long } else { Side::Short };
        let approval = self.sdk_approval(side, size, price, price, leverage).await;
        let pairs = self.ledger.pairs.len();
        let take = Take { agent: b, mm: a, quote, size, is_buy: b_buys, limit: price, leverage, approval: Some(approval), label: "fill_quote", collusion: true };
        let landed = self.send_take(take).await == Some(true);
        if try_load::<TwoSidedQuote>(self.chain.base(), quote).await.map_or(false, |q| q.is_active) {
            let ix = self.cancel_quote_ix(&owner, quote);
            let _ = self.run("cancel_quote", &[ix], &[&kp], None).await;
        }
        if landed && self.ledger.pairs.len() > pairs {
            self.finish_collusion(pairs, wash).await;
        }
    }

    /// The pair crosses on the orderbook at the band edge; a matcher serving them matches it at once.
    async fn collusion_book(&mut self, wash: bool) {
        let (a, b) = (self.agent_named("colluder-a"), self.agent_named("colluder-b"));
        for who in [a, b] {
            self.limit_refill(who).await;
        }
        let m = self.market().await;
        let leverage = if wash { 5_000 } else { model::max_leverage(&m, self.now) };
        let band = load_venue(self.chain.base(), &orderbook::ID).await.max_price_deviation_bps;
        let b_buys = self.rng.gen_bool(0.5);
        let price = self.edge_price(band, b_buys, wash);
        let size = self.collusion_size(price, 30_000 * USDC);
        // a rests first (the maker, at its price); b crosses it
        let a_side = if b_buys { OrderSide::Ask } else { OrderSide::Bid };
        let b_side = if b_buys { OrderSide::Bid } else { OrderSide::Ask };
        let Some(oa) = self.place(a, a_side, price, size, leverage, 1).await else { return };
        let Some(ob) = self.place(b, b_side, price, size, leverage, 1).await else {
            self.cancel(a, oa).await;
            return;
        };
        self.adversary.orders.insert(self.ledger.orders[oa].address);
        self.adversary.orders.insert(self.ledger.orders[ob].address);
        let (bid, ask) = if b_buys { (ob, oa) } else { (oa, ob) };
        let pairs = self.ledger.pairs.len();
        let landed = self.match_tracked(bid, ask, "match_orders").await;
        for o in [oa, ob] {
            if self.ledger.orders[o].live {
                self.cancel(self.ledger.orders[o].owner, o).await;
            }
        }
        if landed && self.ledger.pairs.len() > pairs {
            self.finish_collusion(pairs, wash).await;
        }
    }

    // --- orderbook helpers -----------------------------------------------------------

    async fn limit_refill(&mut self, i: usize) {
        let owner = self.agents[i].t.key();
        let budget = self.agents[i].deposit;
        for role in [ROLE_TAKER, ROLE_MAKER] {
            let address = approval_pda(&owner, &self.book_venue, role);
            let params = VenueApprovalParams { expires_at: self.now + 30 * 86_400, ..approval_params(budget, self.now) };
            let ix = update_approval_ix(&self.chain.base, &owner, &self.book_venue, role, params);
            let kp = self.agents[i].t.kp.insecure_clone();
            if self.run("update_venue_approval", &[ix], &[&kp], None).await.is_ok() {
                self.ledger.track_approval(address, budget);
            }
        }
    }

    /// place_order for agent `i`; returns the ledger index of the order.
    async fn place(&mut self, i: usize, side: OrderSide, price: u64, size: u64, leverage: u32, max_fills: u8) -> Option<usize> {
        let owner = self.agents[i].t.key();
        let kp = self.agents[i].t.kp.insecure_clone();
        let expires_at = self.now + 600;
        let params = PlaceOrderParams { side, order_type: OrderType::Limit, price, size, leverage, expires_at, max_fills, min_fill_size: CONTRACT };
        let seq = load::<OrderBook>(self.chain.base(), self.book).await.sequence_number;
        let (ix, address) = self.place_order_ix(&owner, seq, params);
        self.run("place_order", &[ix], &[&kp], None).await.ok()?;
        self.ledger.orders.push(crate::ledger::TrackedOrder {
            address, owner: i, price, placed_tick: self.tick, expires_at, live: true, fills: 0, capped_fills: 0, end: None,
        });
        self.metrics.orders_placed += 1;
        Some(self.ledger.orders.len() - 1)
    }

    async fn cancel(&mut self, i: usize, o: usize) {
        let (owner, kp) = (self.agents[i].t.key(), self.agents[i].t.kp.insecure_clone());
        let ix = self.cancel_order_ix(&owner, self.ledger.orders[o].address);
        if self.run("cancel_order", &[ix], &[&kp], None).await.is_ok() {
            self.ledger.orders[o].live = false;
            self.ledger.orders[o].end = Some(crate::ledger::OrderEnd::Cancelled);
        }
    }

    /// match_orders on two tracked orders, as the keeper would (prediction and checks included).
    async fn match_tracked(&mut self, bid: usize, ask: usize, label: &'static str) -> bool {
        let (Some(b), Some(a)) = (
            try_load::<Order>(self.chain.base(), self.ledger.orders[bid].address).await,
            try_load::<Order>(self.chain.base(), self.ledger.orders[ask].address).await,
        ) else {
            return false;
        };
        let venue = load_venue(self.chain.base(), &orderbook::ID).await;
        let price = if b.sequence_number < a.sequence_number { b.price } else { a.price };
        let max_fill = (venue.max_fill_notional as u128 * 1_000_000 / price as u128) as u64;
        let fill = predict::match_orders(&b, &a, self.now, max_fill).unwrap_or((b.size - b.filled_size).min(a.size - a.filled_size));
        self.match_pair(bid, &b, ask, &a, fill, max_fill, label).await
    }

    // --- the prober --------------------------------------------------------------------

    /// One fill_quote_probe by the prober against the best quote, its approval
    /// being the SDK's with `tweak` applied (None: send no approval).
    async fn prober_take(&mut self, is_buy: bool, size: u64, leverage: u32, tweak: impl FnOnce(VenueApprovalParams) -> VenueApprovalParams) -> Option<bool> {
        let p = self.agent_named("prober");
        let (mm, quote, q) = self.best_quote(p, is_buy).await?;
        self.prober_take_from(mm, quote, &q, is_buy, size, leverage, None, Some(tweak)).await
    }

    #[allow(clippy::too_many_arguments)]
    async fn prober_take_from(
        &mut self, mm: usize, quote: Pubkey, q: &TwoSidedQuote, is_buy: bool, size: u64, leverage: u32, limit: Option<u64>,
        tweak: Option<impl FnOnce(VenueApprovalParams) -> VenueApprovalParams>,
    ) -> Option<bool> {
        let p = self.agent_named("prober");
        let price = if is_buy { q.ask_price } else { q.bid_price };
        let limit = limit.unwrap_or(if is_buy { price + price / 2_000 } else { price - price / 2_000 });
        let side = if is_buy { Side::Long } else { Side::Short };
        let approval = match tweak {
            Some(f) => Some(f(self.sdk_approval(side, size, price, limit, leverage).await)),
            None => None,
        };
        let take = Take { agent: p, mm, quote, size, is_buy, limit, leverage, approval, label: "fill_quote_probe", collusion: false };
        self.send_take(take).await
    }

    /// The taker's own approval bounds: excluded side, leverage, limit price; and an expired approval.
    async fn probe_approvals(&mut self) {
        let lev = 5_000;
        let size = 20 * CONTRACT;
        // Buying with an approval that only allows selling
        self.prober_take(true, size, lev, |a| VenueApprovalParams { allowed_sides: SIDE_SHORT, max_buy_price: 0, min_sell_price: 0, ..a }).await;
        // Selling with an approval that only allows buying
        self.prober_take(false, size, lev, |a| VenueApprovalParams { allowed_sides: SIDE_LONG, max_buy_price: 0, min_sell_price: 0, ..a }).await;
        // Leverage over the approval's
        self.prober_take(true, size, lev, |a| VenueApprovalParams { max_leverage: 2_000, ..a }).await;
        // Fill above the approval's buy limit (mm-registry's own limit is fine)
        self.prober_take(true, size, lev, |a| VenueApprovalParams { max_buy_price: 1, ..a }).await;
        // A standing approval that has expired
        let p = self.agent_named("prober");
        match self.adversary.expiring_approval {
            Some(expires) if self.now > expires => {
                if let Some((mm, quote, q)) = self.best_quote(p, true).await {
                    self.prober_take_from(mm, quote, &q, true, size, lev, None, None::<fn(VenueApprovalParams) -> VenueApprovalParams>).await;
                }
                self.adversary.expiring_approval = None;
            }
            Some(_) => {}
            None => {
                let owner = self.agents[p].t.key();
                let address = approval_pda(&owner, &self.mm_venue, ROLE_TAKER);
                let params = VenueApprovalParams { expires_at: self.now + 60, ..approval_params(5_000 * USDC, self.now) };
                let ix = if self.ledger.approval_cap(&address).is_some() {
                    update_approval_ix(&self.chain.base, &owner, &self.mm_venue, ROLE_TAKER, params)
                } else {
                    approve_ix(&self.chain.base, &owner, &self.mm_venue, ROLE_TAKER, params)
                };
                let kp = self.agents[p].t.kp.insecure_clone();
                if self.run("update_venue_approval", &[ix], &[&kp], None).await.is_ok() {
                    self.ledger.track_approval(address, params.allowance);
                    self.adversary.expiring_approval = Some(params.expires_at);
                }
            }
        }
    }

    async fn mm_quote_of(&mut self, mm: usize) -> Option<(Pubkey, TwoSidedQuote)> {
        let quote = self.agents[mm].mm.as_ref()?.quote?;
        let q = try_load::<TwoSidedQuote>(self.chain.base(), quote).await?;
        (q.is_active && self.now <= q.expires_at && q.ask_remaining > 0).then_some((quote, q))
    }

    /// Makers: a mass-cancelled (allowance 0) and a revoked approval; self-trades on both venues.
    async fn probe_makers(&mut self) {
        let size = 10 * CONTRACT;
        // mm-tight mass-cancels with update_venue_approval { allowance: 0 }, a fill is tried, then it restores
        let t = self.agent_named("mm-tight");
        if let Some((quote, q)) = self.mm_quote_of(t).await {
            let owner = self.agents[t].t.key();
            let address = approval_pda(&owner, &self.mm_venue, ROLE_MAKER);
            if let Some(current) = try_load::<VenueApproval>(self.chain.base(), address).await {
                let zero = VenueApprovalParams {
                    allowance: 0, max_leverage: current.max_leverage, max_buy_price: current.max_buy_price, min_sell_price: current.min_sell_price,
                    max_price_deviation_bps: current.max_price_deviation_bps, expires_at: current.expires_at, allowed_sides: current.allowed_sides,
                };
                let kp = self.agents[t].t.kp.insecure_clone();
                let ix = update_approval_ix(&self.chain.base, &owner, &self.mm_venue, ROLE_MAKER, zero);
                if self.run("update_venue_approval", &[ix], &[&kp], None).await.is_ok() {
                    self.ledger.track_approval(address, 0);
                    self.prober_take_from(t, quote, &q, true, size, 5_000, None, Some(|a| a)).await;
                    let budget = self.agents[t].mm.as_ref().unwrap().budget;
                    let ix = update_approval_ix(&self.chain.base, &owner, &self.mm_venue, ROLE_MAKER, maker_approval(budget, self.now));
                    if self.run("update_venue_approval", &[ix], &[&kp], None).await.is_ok() {
                        self.ledger.track_approval(address, budget);
                    }
                }
            }
        }
        // mm-wide revokes its maker approval, a fill is tried, then it approves again
        let w = self.agent_named("mm-wide");
        if let Some((quote, q)) = self.mm_quote_of(w).await {
            let owner = self.agents[w].t.key();
            let address = approval_pda(&owner, &self.mm_venue, ROLE_MAKER);
            let kp = self.agents[w].t.kp.insecure_clone();
            if self.run("revoke_venue_approval", &[revoke_ix(&owner, &self.mm_venue, ROLE_MAKER)], &[&kp], None).await.is_ok() {
                self.ledger.revoke_approval(&address);
                self.hit("revoke_venue_approval");
                self.prober_take_from(w, quote, &q, true, size, 5_000, None, Some(|a| a)).await;
                let budget = self.agents[w].mm.as_ref().unwrap().budget;
                let ix = approve_ix(&self.chain.base, &owner, &self.mm_venue, ROLE_MAKER, maker_approval(budget, self.now));
                if self.run("approve_venue", &[ix], &[&kp], None).await.is_ok() {
                    self.ledger.track_approval(address, budget);
                }
            }
        }
        // An MM taking its own quote (mm-registry refuses)
        if let Some((quote, q)) = self.mm_quote_of(t).await {
            let price = q.ask_price;
            let approval = self.sdk_approval(Side::Long, size, price, price + price / 2_000, 5_000).await;
            let take = Take { agent: t, mm: t, quote, size, is_buy: true, limit: price + price / 2_000, leverage: 5_000, approval: Some(approval), label: "fill_quote_probe", collusion: false };
            self.send_take(take).await;
        }
        // One owner's bid and ask crossing on the orderbook (orderbook refuses)
        let a = self.agent_named("colluder-a");
        self.limit_refill(a).await;
        let px = self.px;
        if let Some(ob) = self.place(a, OrderSide::Bid, px, size, 5_000, 1).await {
            if let Some(oa) = self.place(a, OrderSide::Ask, px, size, 5_000, 1).await {
                self.match_tracked(ob, oa, "match_orders_probe").await;
                self.cancel(a, oa).await;
            }
            self.cancel(a, ob).await;
        }
    }

    /// Expired quote, price worse than limit, add_margin and withdrawals beyond free collateral, withdraw with positions open.
    async fn probe_misc(&mut self) {
        let size = 10 * CONTRACT;
        let a = self.agent_named("colluder-a");
        // colluder-a's short-lived quote, then a fill once it has expired
        match self.adversary.expiring_quote {
            Some((quote, expires)) if self.now > expires => {
                if let Some(q) = try_load::<TwoSidedQuote>(self.chain.base(), quote).await {
                    self.prober_take_from(a, quote, &q, true, size, 5_000, None, Some(|x| x)).await;
                    if q.is_active {
                        let (owner, kp) = (self.agents[a].t.key(), self.agents[a].t.kp.insecure_clone());
                        let ix = self.cancel_quote_ix(&owner, quote);
                        let _ = self.run("cancel_quote", &[ix], &[&kp], None).await;
                    }
                }
                self.adversary.expiring_quote = None;
            }
            Some(_) => {}
            None => {
                let (owner, kp) = (self.agents[a].t.key(), self.agents[a].t.kp.insecure_clone());
                let market_maker = self.agents[a].mm.as_ref().unwrap().market_maker;
                let index = load::<MarketMaker>(self.chain.base(), market_maker).await.quotes_posted;
                let px = self.px;
                let params = PostQuoteParams { bid_price: px - px / 200, bid_size: size, ask_price: px + px / 200, ask_size: size, min_fill_size: CONTRACT, expires_in: 60, leverage: 5_000 };
                let (ix, quote) = self.post_quote_ix(&owner, index, params);
                if self.run("post_quote", &[ix], &[&kp], None).await.is_ok() {
                    self.ledger.record_quote(quote);
                    self.adversary.expiring_quote = Some((quote, self.now + 60));
                }
            }
        }
        // A limit below the ask (mm-registry refuses before perps-core)
        let p = self.agent_named("prober");
        if let Some((mm, quote, q)) = self.best_quote(p, true).await {
            self.prober_take_from(mm, quote, &q, true, size, 5_000, Some(q.ask_price - 1), Some(|x| x)).await;
        }
        // Someone with positions open: withdraw half the free collateral, then more than is left
        let holder = (0..self.agents.len()).find(|&i| {
            !self.ledger.open_of(i).is_empty() && matches!(self.agents[i].persona, Persona::Momentum | Persona::MeanReversion | Persona::House | Persona::Limit)
        });
        if let Some(i) = holder {
            let free = self.user_of(i).await.collateral_balance;
            if free >= 2 {
                self.withdraw(i, free / 2, "withdraw").await;
                let left = self.user_of(i).await.collateral_balance;
                self.withdraw(i, left + 1, "withdraw_probe").await;
                // add_margin of a tenth of what is left, then beyond free collateral
                let idx = self.ledger.open_of(i)[0];
                if left >= 10 {
                    self.add_margin(idx, left / 10, "add_margin").await;
                }
                let left = self.user_of(i).await.collateral_balance;
                self.add_margin(idx, left + 1, "add_margin_probe").await;
                self.redeposit(i).await;
            }
        }
    }

    /// End of the adversarial run: the admin settles the market at
    /// `settlement_price`; liquidations and funding must then fail with
    /// MarketSettled and opens with MarketPaused, and closes pay at the
    /// settlement price, in session or not.
    pub async fn settle_and_probe(&mut self, settlement_price: u64) {
        let base = &self.chain.base;
        let (admin, market) = (base.admin.insecure_clone(), base.market);
        let accounts = perps_core::accounts::SettleMarket { authority: admin.pubkey(), config: config_pda(), market };
        let ix = ix(perps_core::ID, accounts, perps_core::instruction::SettleMarket { settlement_price });
        if self.run("settle_market", &[ix], &[&admin], None).await.is_err() {
            return;
        }
        self.settled = true;
        self.paused = true;
        self.hit("settle_market");
        if let Some(&idx) = self.ledger.open.first() {
            let _ = self.liquidate_one(idx, "liquidate_probe").await;
        }
        let p = self.agent_named("prober");
        self.open_house(p, Side::Long, 10 * CONTRACT, 5_000, "open_position_probe").await;
        let m = self.market().await;
        if self.now - m.last_funding_time >= m.funding_interval {
            let predicted: Option<Rej> = predict::update_funding(&m, &self.ov(), self.now);
            let ix = self.update_funding_ix();
            let kp = self.keeper.kp.insecure_clone();
            let _ = self.run("update_funding", &[ix], &[&kp], predicted).await;
        }
    }
}
