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
use crate::actions::open_secs_between;
use crate::checks::PROBES;
use crate::pool_math as pm;
use crate::schedule;
use perps_core::events::{Adl, FundingUpdated};
use perps_core::state::{PoolParams, Position};
use rand::Rng;
use std::collections::HashSet;

/// Which cap `probe_caps` pushes against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapProbe {
    Net,
    WeekendInW,
    /// Open and net-raising close outside W (D43)
    WeekendOutsideW,
    /// Net-raising closes to the cap, then lp-bound's claim outside W (D43)
    WeekendWithdraw,
    /// Net-raising closes to the production net cap, then lp-buffer's claim
    /// inside the exit buffer (D45)
    NetWithdraw,
}

/// lp-bound's deposit: its whole request is claimed against the weekend bound (D43).
const LP_BOUND: u64 = 200_000 * USDC;
/// lp-buffer's deposit: its whole request fits the exit buffer at the net cap
/// (about 20% of B with production params, D45; B is at least the $1M seed).
const LP_BUFFER: u64 = 50_000 * USDC;

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
    /// Positions the probes hold: the keeper's ADL passes leave them alone
    pub held: HashSet<usize>,
    /// Probes that have run (or can no longer run)
    pub done: HashSet<&'static str>,
    /// The pair carried over the forced gap, and how far its probes got
    pub gap: Option<GapPlan>,
    /// When the run's trading ends (the unwind starts)
    pub end_time: i64,
    /// Funding probes so far: LP gates, opens and closes across an overdue period
    pub funding_gates: u32,
    pub funding_opens: u32,
    pub funding_closes: u32,
    /// The prober's position held across an overdue funding period
    pub funding_held: Option<usize>,
    /// The sandwiching LP's deposit before settlement: (lp, shares it minted)
    pub sweep_sandwich: Option<(usize, u64)>,
    /// Ticks left for the F7 probe to find a position in profit
    pub mode1_retries: Option<u32>,
}

/// The pair carried over the forced gap: gap-a's longs go bankrupt at the
/// reopen, gap-b's shorts are the opposite-side winners the probes use.
pub struct GapPlan {
    pub longs: Vec<usize>,
    pub shorts: Vec<usize>,
    /// The oracle before the break
    pub pre_px: u64,
    /// When the gapped session opened (0: not yet)
    pub reopened_at: i64,
    /// 1 built, 2 reopened, 3 after the quiet time, 4 at the next open, 5 done
    pub phase: u8,
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
        self.adversary.end_time = self.now + (total_ticks as i64 - self.tick as i64) * self.cfg.tick_secs;
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
        // The pool's probes go first in the tick: some need the state the
        // session opened with, before the keeper has run
        self.probes_step().await;
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
                let mut st = self.st().await;
                model::pause_market(&mut st.m);
                if self.run("pause_market", &[ix], &[&guardian], None).await.is_ok() {
                    // Predicted, not copied: only is_paused moves (§4.11)
                    self.expect_st("pause_market", &st).await;
                    self.paused = true;
                    self.adversary.paused_at = Some(self.tick);
                    self.hit("pause_market");
                }
            }
            Some(at) if !self.adversary.unpaused && self.tick >= at + PAUSE_TICKS => {
                let accounts = perps_core::accounts::UpdateMarketConfig { authority: admin.pubkey(), config: config_pda(), market };
                let params = cme_config();
                let ix = ix(perps_core::ID, accounts, perps_core::instruction::UpdateMarketConfig { params });
                let mut st = self.st().await;
                let predicted = model::update_market_config(&mut st.m, &params).err();
                if self.run("update_market_config", &[ix], &[&admin], predicted).await.is_ok() {
                    // cme_config() passes max_open_interest 0: the cap must be kept (§4.12, F19)
                    self.expect_st("update_market_config", &st).await;
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
        let mut st = self.st().await;
        let (predicted, funding) = match model::settle_market(&mut st.m, settlement_price, self.now) {
            Ok(ev) => (None, ev),
            Err(r) => (Some(r), None),
        };
        let Ok(ok) = self.run("settle_market", &[ix], &[&admin], predicted).await else {
            return;
        };
        // Predicted, not copied: the funding due (D42), then settlement_price,
        // settled_at and is_paused; nothing else in Market or LpPool moves
        self.check_events("settle_market", &ok.logs, funding, &[]);
        self.expect_st("settle_market", &st).await;
        self.settled = true;
        self.paused = true;
        self.hit("settle_market");
        self.probe_settled_winner().await;
        self.probe_settled_lp_withdraw().await;
        if let Some(&idx) = self.ledger.open.first() {
            let _ = self.liquidate_one(idx, "liquidate_probe").await;
        }
        let p = self.agent_named("prober");
        self.open_house(p, Side::Long, 10 * CONTRACT, 5_000, "open_position_probe").await;
        // Funding stops at settlement, due or not
        let mut m = self.market().await;
        let predicted: Option<Rej> = model::update_funding(&mut m, &self.ov(), self.now).err();
        let ix = self.update_funding_ix();
        let kp = self.keeper.kp.insecure_clone();
        let _ = self.run("update_funding_probe", &[ix], &[&kp], predicted).await;
    }
}

// --- the pool's probes (docs/lp-pool.md §16.7 and the round-2 fixes) ---------------------
//
// Each probe sets up the state its rule is about, sends the transaction, and
// checks the on-chain outcome the spec requires (invariant 13), apart from
// the model's own prediction (which invariants 6, 9 and 10 check as for any
// transaction). A probe whose state could not be reached in a run is
// counted as not reached, with why, and is not a violation.

/// The forced reopen gap (down): a 10x long loses three times its margin.
const GAP: f64 = 0.30;
/// The sandwiching LP's deposit
const SANDWICH: u64 = 50_000 * USDC;
/// Fills per orderbook order (orderbook::MAX_FILLS)
const ORDER_FILLS: u64 = 16;

impl Sim {
    fn lp_named(&self, name: &str) -> Option<usize> {
        self.lps.iter().position(|l| l.name == name)
    }

    /// Probe `name` could not be checked this time, and why.
    pub fn probe_skip(&mut self, name: &str, why: &str) {
        *self.metrics.probes.entry(name.into()).or_default().not_reached.entry(why.into()).or_default() += 1;
    }

    /// Invariant 13: probe `name`'s on-chain outcome is the one the spec requires.
    pub fn probe_check(&mut self, name: &str, ok: bool, detail: impl FnOnce() -> String) -> bool {
        let d = detail();
        let stat = self.metrics.probes.entry(name.into()).or_default();
        stat.checked += 1;
        if ok {
            stat.ok += 1;
        }
        stat.last = d.clone();
        if ok {
            self.hit(format!("probe.{}", name));
        }
        let (tick, now) = (self.tick, self.now);
        self.checks.check(tick, now, PROBES, ok, || format!("{}: {}", name, d))
    }

    /// The probe's transaction (the last one sent since `from`) must have
    /// been refused with `code`.
    pub fn probe_rejected(&mut self, name: &str, from: u64, code: &str) -> bool {
        if self.chain.transactions == from {
            self.probe_skip(name, "not sent: the model predicts a rejection the label never sends");
            return false;
        }
        let got = self.last_failure.as_ref().map(|f| f.key());
        let want = format!("perps_core:{}", code);
        self.probe_check(name, got.as_deref() == Some(want.as_str()), || format!("required {}, chain: {}", want, got.clone().unwrap_or_else(|| "landed".into())))
    }

    /// In session and within `secs` of the session's open (the sim's own schedule).
    fn within_open(&self, secs: i64) -> bool {
        schedule::is_open(self.now) && !schedule::is_open(self.now - secs)
    }

    fn in_w(&self) -> bool {
        schedule::in_pre_close_window(self.now, crate::chain::pool_params().weekend_cap_window_secs)
    }

    /// Seconds until the session next closes (daily break or weekly close).
    fn secs_to_close(&self) -> i64 {
        let mut t = self.now;
        while schedule::is_open(t) && t < self.now + 2 * 86_400 {
            t += 60;
        }
        t - self.now
    }

    async fn pending_total(&mut self) -> u64 {
        let p = self.pool().await;
        p.pending_bad_debt_long + p.pending_bad_debt_short
    }

    /// What the model says a close of `idx` now would do.
    async fn predict_close(&mut self, idx: usize) -> Result<model::CloseOut, Rej> {
        let address = self.ledger.positions[idx].address;
        let user = self.agents[self.ledger.positions[idx].owner].t.user;
        let (mut st, mut u) = (self.st().await, self.user_at(user).await);
        let mut p: Position = load(self.chain.base(), address).await;
        model::close_position(&mut st, &mut p, &mut u, &self.ov(), self.now, 0)
    }

    pub async fn probes_step(&mut self) {
        if self.settled {
            return;
        }
        if !self.adversary.done.contains("lp_params") {
            self.probe_lp_params().await;
        }
        if !self.adversary.done.contains("adl_unauthorized") {
            self.probe_adl_unauthorized().await;
        }
        self.probe_funding().await;
        if self.adversary.mode1_retries.is_some() {
            self.retry_request_then_mode1().await;
            return;
        }
        // Reopen-time probes come before anything else in the tick
        self.gap_step().await;
        self.probe_lp_withdraw_gates().await;
        let pending = self.pending_total().await;
        let q = crate::chain::pool_params().lp_reopen_quiet_secs as i64;
        let calm = pending == 0 && !self.within_open(q) && self.secs_to_close() >= 7_200;
        let gap_over = self.adversary.gap.as_ref().map_or(true, |g| g.phase >= 5);
        // D43, outside W: the LP-withdraw half takes its own slot once
        // lp-bound's request is claimable with no debt pending (in a two-day
        // run the forced gap's debt covers that time, so it is reached in
        // longer runs), and does not hold up the chain below
        self.lp_probe_prepare("lp-bound", LP_BOUND, "weekend_bound_withdraw").await;
        if !self.adversary.done.contains("weekend_bound_withdraw") && calm && !self.in_w() && self.lp_probe_claimable("lp-bound").await {
            self.probe_caps(CapProbe::WeekendWithdraw).await;
            return;
        }
        // D45, the other side: at the production net cap a claim within the
        // exit buffer lands
        self.lp_probe_prepare("lp-buffer", LP_BUFFER, "net_buffer_withdraw").await;
        if !self.adversary.done.contains("net_buffer_withdraw") && calm && !self.in_w() && self.lp_probe_claimable("lp-buffer").await {
            self.probe_caps(CapProbe::NetWithdraw).await;
            return;
        }
        if !self.adversary.done.contains("donation") {
            if calm {
                self.probe_donation().await;
            }
        } else if !self.adversary.done.contains("flash") {
            if calm && !self.in_w() {
                self.probe_flash().await;
            }
        } else if !self.adversary.done.contains("net_cap") {
            if calm && !self.in_w() {
                self.probe_caps(CapProbe::Net).await;
            }
        } else if !self.adversary.done.contains("weekend_bound") {
            if calm && !self.in_w() {
                self.probe_caps(CapProbe::WeekendOutsideW).await;
            }
        } else if !self.adversary.done.contains("weekend_cap") && gap_over && calm && self.in_w() {
            self.probe_caps(CapProbe::WeekendInW).await;
        }
    }

    // --- params (D38) and signers (D16) ------------------------------------------------------

    /// LP params whose W and Q together would shut lp_withdraw for whole
    /// weeks must be refused (InvalidPoolParams), and the boundary (one CME
    /// week of open time exactly) accepted, then cancelled.
    async fn probe_lp_params(&mut self) {
        self.adversary.done.insert("lp_params");
        let admin = self.chain.base.admin.insecure_clone();
        let cur = crate::chain::pool_params();
        let with = |w: u32, q: u32| PoolParams {
            weekend_cap_window_secs: w,
            lp_reopen_quiet_secs: q,
            // Long enough that only the weekly bound can fail
            withdrawal_claim_window_secs: cur.withdrawal_claim_window_secs.max((pm::min_claim_window_secs(crate::session::SCHEDULE_CME_GLOBEX, w, q)).min(u32::MAX as u64) as u32),
            ..cur
        };
        let propose = |s: &Sim, params: PoolParams| {
            let b = &s.chain.base;
            ix(perps_core::ID, perps_core::accounts::ProposePoolParams { admin: b.admin.pubkey(), config: config_pda(), market: b.market, lp_pool: b.lp_pool },
                perps_core::instruction::ProposePoolParams { params })
        };
        // W = 4 days with Q = 1 day, and Q = 23 h alone (D38's examples)
        for (name, w, q) in [("lp_params_w4d_q1d_rejected", 4 * 86_400, 86_400), ("lp_params_q23h_rejected", cur.weekend_cap_window_secs, 82_800)] {
            let n0 = self.chain.transactions;
            let ix = propose(self, with(w, q));
            let _ = self.run("propose_pool_params_probe", &[ix], &[&admin], Some(crate::predict::core("InvalidPoolParams"))).await;
            self.probe_rejected(name, n0, "InvalidPoolParams");
        }
        // W + 5·Q + 1 day = one CME week of open time: allowed (then cancelled)
        let q = ((pm::WEEK_OPEN_SECS - pm::LP_UNGATED_SECS - cur.weekend_cap_window_secs as u64) / 5) as u32;
        let params = with(cur.weekend_cap_window_secs, q);
        let boundary = propose(self, params);
        let mut st = self.st().await;
        model::propose_pool_params(&mut st.pool, params, self.now);
        let landed = self.run("propose_pool_params", &[boundary], &[&admin], None).await.is_ok();
        if landed {
            // Only pending_params and pending_params_at move (D32)
            self.expect_st("propose_pool_params", &st).await;
        }
        self.probe_check("lp_params_boundary_accepted", landed, || format!("W {} + 5 x Q {} + 1 day = {} open s: landed {}", cur.weekend_cap_window_secs, q, pm::WEEK_OPEN_SECS, landed));
        if landed {
            let b = &self.chain.base;
            let accounts = perps_core::accounts::PoolAdminOnly { admin: admin.pubkey(), config: config_pda(), lp_pool: b.lp_pool };
            let cancel = ix(perps_core::ID, accounts, perps_core::instruction::CancelPoolParams {});
            let mut st = self.st().await;
            let predicted = model::cancel_pool_params(&mut st.pool).err();
            if self.run("cancel_pool_params", &[cancel], &[&admin], predicted).await.is_ok() {
                self.expect_st("cancel_pool_params", &st).await;
            }
        }
    }

    /// Mode-1 (exposure) ADL signed by anyone but adl_keeper or the admin: AdlUnauthorized.
    async fn probe_adl_unauthorized(&mut self) {
        let Some(&idx) = self.ledger.open.first() else { return };
        self.adversary.done.insert("adl_unauthorized");
        let p = self.agent_named("prober");
        let kp = self.agents[p].t.kp.insecure_clone();
        let n0 = self.chain.transactions;
        self.adl_by(idx, model::ADL_EXPOSURE, 0, "adl_probe", &kp).await;
        self.probe_rejected("adl_mode1_unauthorized", n0, "AdlUnauthorized");
    }

    // --- funding (D17 / F12, D40) --------------------------------------------------------------

    /// When a funding period is due and not yet cranked (the adversary acts
    /// before the keeper): LP flows must be refused (FundingStale), a house
    /// open must crank first and start from the cranked index, and a close
    /// of a position held across the period must pay it.
    async fn probe_funding(&mut self) {
        let m = self.market().await;
        if model::settled(&m) || model::funding_current(&m, self.now) {
            return;
        }
        let q = crate::chain::pool_params().lp_reopen_quiet_secs as i64;
        let open = open_secs_between(m.last_funding_time, self.now) + m.funding_carry_secs as i64;
        let due = (open / m.funding_interval).min(pm::MAX_FUNDING_CATCHUP_PERIODS as i64) as u64;
        if self.adversary.funding_gates < 3 && !self.within_open(q) {
            self.adversary.funding_gates += 1;
            if let Some(l) = self.lp_named("lp-sandwich") {
                let n0 = self.chain.transactions;
                self.lp_deposit(l, 1_000 * USDC, 0, "lp_deposit_probe").await;
                self.probe_rejected("lp_deposit_funding_stale", n0, "FundingStale");
                let pos = self.lp_position(l).await;
                if pos.shares > 0 && pos.pending_shares == 0 {
                    let n0 = self.chain.transactions;
                    self.lp_request(l, 1, "lp_request_probe").await;
                    self.probe_rejected("lp_request_funding_stale", n0, "FundingStale");
                }
            }
        }
        let p = self.agent_named("prober");
        let held = self.adversary.funding_held.filter(|&i| self.ledger.positions[i].is_open());
        if let Some(idx) = held {
            if self.adversary.funding_closes >= 3 {
                return;
            }
            if self.predict_close(idx).await.is_err() {
                return;
            }
            self.adversary.funding_closes += 1;
            self.adversary.funding_held = None;
            if self.close_tracked(idx, "close_position").await {
                let ev = crate::chain::events::<FundingUpdated>(&self.last_logs);
                let after = self.market().await;
                let ok = ev.len() == 1 && ev[0].periods == due && ev[0].cumulative_funding == after.cumulative_funding;
                self.probe_check("funding_catchup_close", ok, || format!("{} periods due: the close cranked {:?}", due, ev.iter().map(|e| (e.periods, e.cumulative_funding)).collect::<Vec<_>>()));
            }
            return;
        }
        if self.adversary.funding_opens >= 3 {
            return;
        }
        self.adversary.funding_opens += 1;
        // The side that lowers |net|: no pool cap or freeze applies to it
        let side = if model::net(&m) > 0 { Side::Short } else { Side::Long };
        let n = self.ledger.positions.len();
        if !self.open_house(p, side, 10 * CONTRACT, 5_000, "open_position").await || self.ledger.positions.len() == n {
            self.probe_skip("funding_catchup_open", "the open did not land");
            return;
        }
        let idx = n;
        let ev = crate::chain::events::<FundingUpdated>(&self.last_logs);
        let after = self.market().await;
        let basis = self.ledger.positions[idx].state.last_funding_payment;
        let ok = ev.len() == 1 && ev[0].periods == due && basis == after.cumulative_funding;
        self.probe_check("funding_catchup_open", ok, || format!("{} periods due: cranked {:?}, new position's basis {} (market {})", due, ev.iter().map(|e| e.periods).collect::<Vec<_>>(), basis, after.cumulative_funding));
        self.adversary.funding_held = Some(idx);
    }

    // --- LP sandwiches (D18, F6, F11) ------------------------------------------------------------

    /// The sandwiching LP's deposit; Some(shares minted).
    async fn sandwich_deposit(&mut self, l: usize, amount: u64) -> Option<u64> {
        let before = self.lp_position(l).await.shares;
        if !self.lp_deposit(l, amount, 0, "lp_deposit").await {
            return None;
        }
        Some(self.lp_position(l).await.shares - before)
    }

    /// Request `shares`, read the requested value, cancel: Some(value).
    async fn sandwich_exit(&mut self, l: usize, shares: u64) -> Option<u64> {
        if !self.lp_request(l, shares, "lp_request").await {
            return None;
        }
        let value = self.lp_position(l).await.requested_value;
        self.lp_cancel(l, "lp_cancel").await;
        Some(value)
    }

    /// Deposit, a donation, request: the request is worth no more than the
    /// deposit (the donation vests into NAV_redeem, D18).
    async fn probe_donation(&mut self) {
        if !model::funding_current(&self.market().await, self.now) {
            return;
        }
        self.adversary.done.insert("donation");
        let Some(l) = self.lp_named("lp-sandwich") else { return };
        let Some(shares) = self.sandwich_deposit(l, SANDWICH).await else {
            self.probe_skip("lp_sandwich_donation", "deposit refused");
            return;
        };
        let p = self.agent_named("prober");
        let lp_shares = self.pool().await.lp_shares;
        let Some((_, vesting)) = self.donate(p, 20_000 * USDC, "donate").await else {
            self.probe_skip("lp_sandwich_donation", "donation refused");
            return;
        };
        if let Some(value) = self.sandwich_exit(l, shares).await {
            let captured = vesting as u128 * shares as u128 / lp_shares as u128;
            self.probe_check("lp_sandwich_donation", value <= SANDWICH, || {
                format!("deposited {}, requested {} right after a {} donation (its share unvested: {})", SANDWICH, value, 20_000 * USDC, captured)
            });
        }
    }

    /// A flash move inside the session that makes the prober's 10x long
    /// stage-1 liquidatable: the prober liquidating itself must come out
    /// worse than closing (validate rule, F42), and an LP that deposits just
    /// before the keeper's liquidations and requests just after gains nothing.
    async fn probe_flash(&mut self) {
        self.adversary.done.insert("flash");
        if !model::funding_current(&self.market().await, self.now) {
            self.crank_funding().await;
        }
        let p = self.agent_named("prober");
        let m = self.market().await;
        let lev = model::max_leverage(&m, self.now);
        let size = (40_000 * USDC) as u128 * 1_000_000 / self.px as u128 / CONTRACT as u128 * CONTRACT as u128;
        let n = self.ledger.positions.len();
        if !self.open_house(p, Side::Long, size as u64, lev, "open_position").await || self.ledger.positions.len() == n {
            self.probe_skip("self_liquidation_loses", "the prober's open did not land");
            return;
        }
        let idx = n;
        let pos = self.ledger.positions[idx].state.clone();
        // The mark at which equity is 4.2% of mark notional (between ⅔·MM and MM)
        let (c, q, e) = (pos.collateral as f64, pos.size as f64 / 1e6, pos.entry_price as f64);
        let target = ((e * q - c) / (q * (1.0 - 0.042))).round() as u64;
        let st = self.st().await;
        let (_, mmr) = model::margin_ratios(&st.m, self.now);
        let eq = model::equity(&pos, target, st.m.cumulative_funding);
        let stage1 = eq > 0 && eq as u128 * 10_000 * 10_000 >= mmr as u128 * st.pool.params.backstop_threshold_bps as u128 * model::mark_notional(pos.size, target);
        if target >= self.px || !model::liquidatable(&st.m, &pos, target, self.now) || !stage1 {
            self.probe_skip("self_liquidation_loses", "no flash price makes the position stage-1 liquidatable");
            return;
        }
        self.oracle.shock_to(self.now, target);
        self.publish(target, target / 5_000).await;
        self.history.push(target);
        self.hit("oracle.flash_move");
        // The LP sandwich: in before the liquidations
        let l = self.lp_named("lp-sandwich");
        let minted = match l {
            Some(l) => self.sandwich_deposit(l, SANDWICH).await,
            None => None,
        };
        let liquidations = self.metrics.liquidations;
        // Self-liquidation against a close, both valued at this mark
        let user = self.agents[p].t.user;
        let (mut st, mut u, mut pp) = (self.st().await, self.user_at(user).await, pos.clone());
        let close = model::close_position(&mut st, &mut pp, &mut u, &self.ov(), self.now, 0);
        let (kp, tokens) = (self.agents[p].t.kp.insecure_clone(), self.agents[p].t.tokens);
        match (close, self.liquidate_by(idx, "liquidate", &kp, tokens).await) {
            (Ok(c), Ok(Some(out))) => {
                let rest = if out.size_remaining > 0 { self.predict_close(idx).await.map(|o| o.payout).ok() } else { Some(0) };
                match rest {
                    Some(rest) => {
                        let value = out.keeper_reward + out.owner_leftover + rest;
                        self.probe_check("self_liquidation_loses", out.stage == 1 && value < c.payout, || {
                            format!("stage {}: reward {} + leftover {} + remainder {} = {} vs closing {}", out.stage, out.keeper_reward, out.owner_leftover, rest, value, c.payout)
                        });
                    }
                    None => self.probe_skip("self_liquidation_loses", "the remainder cannot be valued by a close"),
                }
            }
            (Err(r), _) => self.probe_skip("self_liquidation_loses", &format!("a close would fail with {}", r.1)),
            (_, other) => self.probe_skip("self_liquidation_loses", &format!("the self-liquidation did not land: {:?}", other.err())),
        }
        if self.ledger.positions[idx].is_open() {
            self.close_tracked(idx, "close_position").await;
        }
        self.liquidate_all().await;
        let (Some(l), Some(shares)) = (l, minted) else {
            self.probe_skip("lp_sandwich_liquidation", "the deposit before the liquidations was refused");
            return;
        };
        let n_liq = self.metrics.liquidations - liquidations;
        if let Some(value) = self.sandwich_exit(l, shares).await {
            if n_liq == 0 {
                self.probe_skip("lp_sandwich_liquidation", "no liquidation between the deposit and the request");
            } else {
                self.probe_check("lp_sandwich_liquidation", value <= SANDWICH, || format!("deposited {}, {} liquidations, requested {}", SANDWICH, n_liq, value));
            }
        }
    }

    // --- caps (D14, F1, F7) -------------------------------------------------------------------

    /// Self-cross `size` contracts at the oracle on the orderbook, `long`
    /// buying from `short`, one fill per venue cap. Returns the new legs.
    async fn build_cross(&mut self, long: usize, short: usize, size: u64) -> (Vec<usize>, Vec<usize>) {
        for who in [long, short] {
            self.limit_refill(who).await;
        }
        let m = self.market().await;
        let leverage = model::max_leverage(&m, self.now);
        let venue = load_venue(self.chain.base(), &orderbook::ID).await;
        let price = self.px;
        let fill = (venue.max_fill_notional as u128 * 1_000_000 / price as u128) as u64 / CONTRACT * CONTRACT;
        let (mut longs, mut shorts) = (Vec::new(), Vec::new());
        let mut left = size / CONTRACT * CONTRACT;
        while left >= CONTRACT {
            let chunk = left.min(fill * ORDER_FILLS);
            let Some(ob) = self.place(long, OrderSide::Bid, price, chunk, leverage, ORDER_FILLS as u8).await else { break };
            let Some(oa) = self.place(short, OrderSide::Ask, price, chunk, leverage, ORDER_FILLS as u8).await else {
                self.cancel(long, ob).await;
                break;
            };
            let mut filled = 0;
            for _ in 0..ORDER_FILLS {
                let n = self.ledger.positions.len();
                if !self.match_tracked(ob, oa, "match_orders").await {
                    break;
                }
                for i in n..self.ledger.positions.len() {
                    let pos = &self.ledger.positions[i];
                    if pos.owner == long {
                        filled += pos.state.size;
                        longs.push(i);
                    } else if pos.owner == short {
                        shorts.push(i);
                    }
                }
                if !self.ledger.orders[ob].live || filled >= chunk {
                    break;
                }
            }
            for o in [ob, oa] {
                if self.ledger.orders[o].live && exists(self.chain.base(), self.ledger.orders[o].address).await {
                    self.cancel(self.ledger.orders[o].owner, o).await;
                }
            }
            if filled == 0 {
                break;
            }
            left = left.saturating_sub(filled);
        }
        (longs, shorts)
    }

    /// Close every open position of `who` the model lets close now, those
    /// that lower |net| first (a few rounds: one leg's close frees the next).
    async fn unwind_probe_positions(&mut self, who: &[usize]) {
        for _ in 0..4 {
            let m = self.market().await;
            let net = model::net(&m);
            let mut legs: Vec<(bool, usize)> = self.ledger.open.iter().copied().filter(|&i| who.contains(&self.ledger.positions[i].owner))
                .map(|i| (model::sign(self.ledger.positions[i].side()) * net <= 0, i)).collect();
            if legs.is_empty() {
                return;
            }
            legs.sort();
            for (_, idx) in legs {
                if self.ledger.positions[idx].is_open() && self.predict_close(idx).await.is_ok() {
                    self.close_tracked(idx, "close_position").await;
                }
            }
        }
    }

    /// Push the net to the net cap (`Net`: k_net·B, outside W) or the weekend
    /// cap (α·B/G: `WeekendInW` inside W, `WeekendOutsideW` outside it, D43)
    /// by self-crossing a venue pair and closing the legs on the side that
    /// raises |net| (F1, D14): every close that lands fits the cap; the one
    /// past it is refused with PoolExposureLimit / PoolWeekendExposureLimit.
    /// A house open past it is refused the same way. Outside W, a free
    /// withdrawal request then must not make exposure ADL eligible (F7:
    /// triggers use B, not capNAV).
    ///
    /// Production k_net (0.4×) is under the α/G = 0.5× weekend line (D45),
    /// so the net cap, checked first, would answer. The weekend probes
    /// therefore raise the weekend IM first (G 20% → about 36%, as
    /// `update_market_config` may at any time, unseen by the pool): the
    /// always-on weekend cap must tighten below the net cap on its own.
    /// Outside W, lp-bound's matured withdrawal must then be refused
    /// (PoolCapWouldBreach): the capital behind the net cannot leave below
    /// the bound, and closes capped on B leave no buffer under it. The
    /// weekend IM is put back before the probe returns, so nothing else sees
    /// it. `NetWithdraw` keeps the production G: with the net at the 0.4× net
    /// cap, lp-buffer's claim inside the 20% exit buffer must land (D45).
    async fn probe_caps(&mut self, kind: CapProbe) {
        let weekend = !matches!(kind, CapProbe::Net | CapProbe::NetWithdraw);
        let (name, open_name, code) = match kind {
            CapProbe::Net => ("selfcross_close_net_cap", "house_open_net_cap", "PoolExposureLimit"),
            CapProbe::NetWithdraw => ("selfcross_close_net_cap_lp", "", "PoolExposureLimit"),
            CapProbe::WeekendInW => ("selfcross_close_weekend_cap", "house_open_weekend_cap", "PoolWeekendExposureLimit"),
            CapProbe::WeekendOutsideW => ("selfcross_close_weekend_bound", "house_open_weekend_bound", "PoolWeekendExposureLimit"),
            CapProbe::WeekendWithdraw => ("selfcross_close_weekend_bound_lp", "", "PoolWeekendExposureLimit"),
        };
        self.adversary.done.insert(match kind {
            CapProbe::Net => "net_cap",
            CapProbe::WeekendInW => "weekend_cap",
            CapProbe::WeekendOutsideW => "weekend_bound",
            CapProbe::WeekendWithdraw => "weekend_bound_withdraw",
            CapProbe::NetWithdraw => "net_buffer_withdraw",
        });
        let restore = if weekend {
            // G well above α·1e4/(0.7·k_net), so the weekend cap stays under
            // the net cap even on capNAV with pending withdrawals (≤ 30%),
            // and 10% above α·1e4·B/(k_net·capNAV) for the pending share now
            // (the probe LPs' matured requests can take it past 30%): the net
            // is pushed to the weekend cap on B by closes, and a house open
            // past it is checked against the net cap on capNAV first
            let m = self.market().await;
            let k = crate::chain::pool_params();
            let g0 = m.weekend_initial_margin_ratio.max(m.initial_margin_ratio);
            let mut floor = (k.weekend_lp_loss_bps as u64 * 10_000 * 10 / (7 * k.k_net_bps.max(1) as u64)) as u32 + 1;
            let st = self.st().await;
            if let Ok(v) = model::view(&st.pool, &st.m, self.px, self.now) {
                if v.cap_nav > 0 {
                    let need = k.weekend_lp_loss_bps as u128 * 10_000 * 11 * v.b / (10 * k.k_net_bps.max(1) as u128 * v.cap_nav) + 1;
                    floor = floor.max(need.min(10_000) as u32);
                }
            }
            let raised = (g0 + 500).max(floor).min(10_000);
            if !self.set_weekend_im(raised).await {
                self.probe_skip(name, "update_market_config refused");
                return;
            }
            Some(m.weekend_initial_margin_ratio)
        } else {
            None
        };
        let st = self.st().await;
        let Ok(v) = model::view(&st.pool, &st.m, self.px, self.now) else {
            if let Some(w) = restore {
                self.set_weekend_im(w).await;
            }
            return;
        };
        let params = st.pool.params;
        let g = model::g_bps(&st.m, &params) as u128;
        let cap = |b: u128| if weekend { b * params.weekend_lp_loss_bps as u128 / g } else { b * params.k_net_bps as u128 / 10_000 };
        let dir: i128 = if v.net >= 0 { 1 } else { -1 };
        let px = self.px as u128;
        let fill = 45_000 * USDC as u128 * 1_000_000 / px;
        // check_close_caps tests the net cap on B first. After a gap or a
        // fall in B the net can already be over k_net·B, or within one leg
        // of it (more often since D45 put k_net 20% under α/G), and a
        // raising close would then cross both caps and get the net cap's
        // answer. The weekend probes first close legs on the side that
        // lowers |net| (never capped) until two legs ($90k) fit under the
        // net cap on B, so the pair is built that much larger. NetWithdraw
        // does the same (D45 review): with the net left over k_net·B (a
        // price rise or a fall in B since it was put on) the room is
        // legitimately under the promised buffer, and the probe would
        // measure that state instead of the one at the cap
        let legs = 2 * 45_000 * USDC as u128;
        let excess = if weekend || kind == CapProbe::NetWithdraw {
            (v.net_notional + legs).saturating_sub(v.b * params.k_net_bps as u128 / 10_000) * 1_000_000 / px
        } else {
            0
        };
        let need = (cap(v.b) * 1_000_000 / px).saturating_sub(v.net.unsigned_abs()) + 2 * fill + excess;
        let (a, b) = (self.agent_named("cross-a"), self.agent_named("cross-b"));
        let (longs, shorts) = self.build_cross(a, b, need as u64).await;
        let (raise, mut lower) = if dir > 0 { (shorts, longs) } else { (longs, shorts) };
        if excess > 0 {
            let mut rest = Vec::new();
            for idx in lower {
                let st = self.st().await;
                let under = model::view(&st.pool, &st.m, self.px, self.now).map_or(true, |v| pm::net_cap_ok(v.net_notional + legs, params.k_net_bps, v.b));
                if under || !self.ledger.positions[idx].is_open() || !self.close_tracked(idx, "close_position").await {
                    rest.push(idx);
                }
            }
            lower = rest;
        }
        let mut refused = false;
        for idx in raise {
            if !self.ledger.positions[idx].is_open() {
                continue;
            }
            let pre = self.st().await;
            let Ok(vb) = model::view(&pre.pool, &pre.m, self.px, self.now) else { break };
            match self.predict_close(idx).await {
                Ok(_) => {
                    if !self.close_tracked(idx, "close_position").await {
                        continue;
                    }
                    // A close that raised |net| fits the cap on B before it
                    let m = self.market().await;
                    let nn = (model::net(&m).unsigned_abs() * px + 999_999) / 1_000_000;
                    let limit = cap(vb.b);
                    if nn > limit {
                        self.probe_check(name, false, || format!("a close landed with net notional {} over the cap {}", nn, limit));
                    }
                }
                Err(r) if r.1 == code || r.1 == "PoolExposureLimit" => {
                    let n0 = self.chain.transactions;
                    self.close_bounded(idx, 0, "close_position_probe").await;
                    self.probe_rejected(name, n0, code);
                    refused = true;
                    break;
                }
                Err(_) => continue,
            }
        }
        if !refused {
            self.probe_skip(name, "the cap was not reached with the pair built");
        } else if kind == CapProbe::WeekendWithdraw {
            self.probe_lp_bound_withdraw().await;
        } else if kind == CapProbe::NetWithdraw {
            self.probe_lp_buffer_withdraw().await;
        } else {
            // A house open past the same cap (capNAV ≤ B): refused at the cap, before the per-account limit.
            // check_open_caps tests the net cap on capNAV first, and net put
            // on before LPs requested (or by closes, capped on B) can
            // already be over k_net·capNAV (D45: production k_net is 20%
            // under α/G, so pending requests get there sooner than under
            // D43). For the weekend probes, close legs of the pair on the
            // side that lowers |net| (never capped) until the net is back
            // under the net cap on capNAV, so the weekend cap is what answers
            if weekend {
                for idx in lower {
                    let st = self.st().await;
                    let Ok(v) = model::view(&st.pool, &st.m, self.px, self.now) else { break };
                    if pm::net_cap_ok(v.net_notional * 20 / 19, params.k_net_bps, v.cap_nav) {
                        break;
                    }
                    if self.ledger.positions[idx].is_open() {
                        self.close_tracked(idx, "close_position").await;
                    }
                }
            }
            let st = self.st().await;
            let p = self.agent_named("prober");
            let free = self.user_of(p).await.collateral_balance;
            if let Ok(v) = model::view(&st.pool, &st.m, self.px, self.now) {
                let dd = model::dd_now(&st.pool, &v);
                let room = cap(v.cap_nav).saturating_sub(v.net_notional);
                let size = ((room + 1_000 * USDC as u128) * 1_000_000 / px) as u64 / CONTRACT * CONTRACT + CONTRACT;
                let lev = model::max_leverage(&st.m, self.now);
                let margin = model::notional(size, self.px) as u64 / (lev as u64 / 1_000).max(1);
                if dd >= params.dd_freeze_bps as u64 && params.dd_freeze_bps > 0 {
                    self.probe_skip(open_name, "drawdown freeze in force");
                } else if margin * 11 / 10 > free {
                    self.probe_skip(open_name, "the prober cannot margin an open past the cap");
                } else {
                    let side = if dir > 0 { Side::Long } else { Side::Short };
                    let n0 = self.chain.transactions;
                    // Still over the net cap on capNAV (the lowering legs
                    // ran out): the net cap is what the program must answer
                    let want = if weekend && !pm::net_cap_ok(v.net_notional, params.k_net_bps, v.cap_nav) { "PoolExposureLimit" } else { code };
                    self.open_house(p, side, size, lev, "open_position_probe").await;
                    self.probe_rejected(open_name, n0, want);
                }
            }
            if kind == CapProbe::Net && !self.probe_request_then_mode1().await {
                // Hold the net at the cap for a few ticks until some position is in profit
                self.adversary.mode1_retries = Some(6);
                return;
            }
        }
        if let Some(w) = restore {
            self.set_weekend_im(w).await;
        }
        self.unwind_probe_positions(&[a, b]).await;
    }

    /// update_market_config as the admin with the weekend IM changed (the
    /// rest as it is), checked against the model.
    async fn set_weekend_im(&mut self, weekend_im: u32) -> bool {
        let m = self.market().await;
        let params = perps_core::instructions::MarketConfigParams {
            is_paused: m.is_paused,
            session_schedule: m.session_schedule,
            pre_close_window_secs: m.pre_close_window_secs,
            weekend_initial_margin_ratio: weekend_im,
            weekend_maintenance_margin_ratio: m.weekend_maintenance_margin_ratio,
            max_confidence_bps: m.max_confidence_bps,
            min_oracle_price: m.min_oracle_price,
            max_open_interest: 0,
        };
        let admin = self.chain.base.admin.insecure_clone();
        let accounts = perps_core::accounts::UpdateMarketConfig { authority: admin.pubkey(), config: config_pda(), market: self.chain.base.market };
        let ix = ix(perps_core::ID, accounts, perps_core::instruction::UpdateMarketConfig { params });
        let mut st = self.st().await;
        let predicted = model::update_market_config(&mut st.m, &params).err();
        if self.run("update_market_config", &[ix], &[&admin], predicted).await.is_ok() {
            self.expect_st("update_market_config", &st).await;
            true
        } else {
            false
        }
    }

    /// LP `name` deposits `amount`, then requests everything, so a request
    /// is claimable a day into the run for the withdrawal probe `done_key`
    /// (lp-bound: D43; lp-buffer: D45).
    async fn lp_probe_prepare(&mut self, name: &str, amount: u64, done_key: &str) {
        let Some(l) = self.lp_named(name) else { return };
        if self.adversary.done.contains(done_key) {
            return;
        }
        let pos = self.lp_position(l).await;
        if pos.shares == 0 && pos.deposited_cum == 0 {
            if !self.within_open(crate::chain::pool_params().lp_reopen_quiet_secs as i64) && !self.in_w() {
                self.lp_deposit(l, amount, 0, "lp_deposit").await;
            }
        } else if pos.pending_shares == 0 && pos.shares > 0 && pos.requested_at == 0 {
            self.lp_request(l, pos.shares, "lp_request").await;
        }
    }

    /// LP `name`'s request can be claimed now (gates aside from the caps).
    async fn lp_probe_claimable(&mut self, name: &str) -> bool {
        let Some(l) = self.lp_named(name) else { return false };
        let pos = self.lp_position(l).await;
        if pos.pending_shares == 0 || self.now < pos.unlock_at {
            return false;
        }
        let p = crate::chain::pool_params();
        let window = pm::effective_claim_window_secs(crate::session::SCHEDULE_CME_GLOBEX, pos.claim_window_secs, p.weekend_cap_window_secs, p.lp_reopen_quiet_secs);
        let live = window == 0 || open_secs_between(pos.unlock_at, self.now) as u64 <= window;
        live && schedule::is_open(self.now) && !self.within_open(p.lp_reopen_quiet_secs as i64)
    }

    /// D43: with the net at the weekend cap, lp-bound's claim would leave
    /// G·net over α·B': refused with PoolCapWouldBreach, and the model must
    /// say so for that reason (the hard-net line alone would let it out).
    async fn probe_lp_bound_withdraw(&mut self) {
        let name = "lp_withdraw_weekend_bound";
        let Some(l) = self.lp_named("lp-bound") else { return };
        self.crank_funding().await;
        let st = self.st().await;
        let pos = self.lp_position(l).await;
        let Ok(v) = model::view(&st.pool, &st.m, self.px, self.now) else { return };
        let p = st.pool.params;
        let value = pm::redeem_value(pos.pending_shares, st.pool.lp_shares, v.nav_redeem).min(pos.requested_value);
        let b_after = (v.nav_redeem - value as i128).max(0) as u128;
        let g = model::g_bps(&st.m, &p);
        if !pm::hard_net_ok(v.net_notional, p.hard_net_bps, b_after) {
            self.probe_skip(name, "the hard-net line refuses it first");
            return;
        }
        if pm::weekend_cap_ok(v.net_notional, g, p.weekend_lp_loss_bps, b_after) {
            self.probe_skip(name, "the claim would not breach the weekend bound");
            return;
        }
        let n0 = self.chain.transactions;
        self.lp_withdraw(l, "lp_withdraw_probe").await;
        self.probe_rejected(name, n0, "PoolCapWouldBreach");
    }

    /// D45: with the net pushed to the production net cap (`k_net·B` by
    /// net-raising closes, at the production G), the weekend withdraw line
    /// `G·net_notional ≤ α·B'` still leaves `B − ⌈G·net_notional/α⌉`, at
    /// least the exit buffer `(1 − k_net·G/α)·B` (20%; more by up to one leg
    /// when the last close stopped short of the cap). lp-buffer's claim fits
    /// it: the model's post-checks must pass, the claim must land, and one
    /// unit past the room must fail the weekend line in the model's math.
    async fn probe_lp_buffer_withdraw(&mut self) {
        let name = "lp_withdraw_within_buffer";
        let Some(l) = self.lp_named("lp-buffer") else { return };
        self.crank_funding().await;
        let st = self.st().await;
        let pos = self.lp_position(l).await;
        let Ok(v) = model::view(&st.pool, &st.m, self.px, self.now) else { return };
        let p = st.pool.params;
        let g = model::g_bps(&st.m, &p);
        let alpha = p.weekend_lp_loss_bps as u128;
        if g == 0 || alpha == 0 || v.net_notional == 0 {
            self.probe_skip(name, "no weekend line to measure the buffer against");
            return;
        }
        let nav = v.nav_redeem.max(0) as u128;
        let need = (v.net_notional * g as u128 + alpha - 1) / alpha;
        let room = nav.saturating_sub(need);
        let buffer_bps = 10_000u128.saturating_sub(p.k_net_bps as u128 * g as u128 / alpha);
        let promised = nav * buffer_bps / 10_000;
        let value = pm::redeem_value(pos.pending_shares, st.pool.lp_shares, v.nav_redeem).min(pos.requested_value) as u128;
        let detail = format!(
            "net_notional {} on B {}: room {} ({} bps of B; buffer {} bps promises {}), claim {}",
            v.net_notional,
            nav,
            room,
            if nav > 0 { room * 10_000 / nav } else { 0 },
            buffer_bps,
            promised,
            value
        );
        // The room is exactly the edge of the weekend line (0 when the net
        // is already past it). It is at least the promised buffer (up to
        // rounding) only while the net is within the net cap on B: a price
        // rise or a fall in B since the net was put on (or a cap-exempt
        // liquidation of the minority side) leaves it over k_net·B, and then
        // the room is smaller by design (§3.1, exit-liveness cost; D45 review)
        let at_edge = if need >= nav {
            room == 0
        } else {
            pm::weekend_cap_ok(v.net_notional, g, p.weekend_lp_loss_bps, nav - room)
                && (room == 0 || !pm::weekend_cap_ok(v.net_notional, g, p.weekend_lp_loss_bps, nav - room - 1))
        };
        let within_cap = pm::net_cap_ok(v.net_notional, p.k_net_bps, nav);
        if !self.probe_check("exit_buffer_at_net_cap", at_edge && (!within_cap || room + 2 >= promised), || detail.clone()) {
            return;
        }
        if !within_cap {
            self.probe_skip(name, "the net is over the net cap on B (price or B moved), so the buffer is not promised");
            return;
        }
        if value > room {
            self.probe_skip(name, "the claim is larger than the buffer left");
            return;
        }
        if !pm::hard_net_ok(v.net_notional, p.hard_net_bps, nav - value) {
            self.probe_skip(name, "the hard-net line refuses it first");
            return;
        }
        let got = self.lp_withdraw(l, "lp_withdraw_probe").await;
        self.probe_check(name, got.is_some(), || format!("{}; chain: {}", detail, got.map_or("refused".into(), |x| format!("paid {}", x))));
    }

    /// F7: the seed LP requests half its shares (capNAV falls, B does not);
    /// exposure ADL must still be refused with AdlNotRequired.
    /// Returns false when no position is in profit to target (the caller
    /// keeps the net at the cap and tries again a tick later).
    async fn probe_request_then_mode1(&mut self) -> bool {
        let name = "request_then_mode1_adl";
        self.crank_funding().await;
        // ADL refuses an unprofitable target before it looks at the triggers
        let f = self.market().await.cumulative_funding;
        let candidate = self.ledger.open.iter().copied().filter(|i| !self.adversary.held.contains(i)).find(|&i| {
            let p = &self.ledger.positions[i].state;
            model::slice_profit(p, p.size, self.px, f) > 0 && model::equity(p, self.px, f) > 0
        });
        if candidate.is_none() {
            return false;
        }
        let shares = self.lp_position(0).await.shares / 2;
        if !self.lp_request(0, shares, "lp_request").await {
            self.probe_skip(name, "the seed's request was refused");
            return true;
        }
        let st = self.st().await;
        let Ok(v) = model::view(&st.pool, &st.m, self.px, self.now) else { return true };
        let (triggers, _) = model::exposure_triggers(&st.m, &st.pool, &v, self.now);
        let hard = st.pool.params.hard_net_bps as u128;
        let on_cap_nav = hard > 0 && v.net_notional * 10_000 > hard * v.cap_nav;
        match (triggers, candidate) {
            (0, Some(idx)) => {
                let admin = self.chain.base.admin.insecure_clone();
                let n0 = self.chain.transactions;
                self.adl_by(idx, model::ADL_EXPOSURE, 0, "adl_probe", &admin).await;
                if self.probe_rejected(name, n0, "AdlNotRequired") && !on_cap_nav {
                    self.probe_skip(name, "checked, but the hard-net line would not have held on capNAV either");
                }
            }
            (0, None) => self.probe_skip(name, "no profitable position to target"),
            _ => self.probe_skip(name, "an exposure trigger already holds on B"),
        }
        self.lp_cancel(0, "lp_cancel").await;
        true
    }

    /// The F7 probe waiting for a position in profit, with the net still at the cap.
    async fn retry_request_then_mode1(&mut self) {
        let Some(left) = self.adversary.mode1_retries else { return };
        let done = self.probe_request_then_mode1().await;
        if done || left <= 1 {
            if !done {
                self.probe_skip("request_then_mode1_adl", "no profitable position to target");
            }
            self.adversary.mode1_retries = None;
            let (a, b) = (self.agent_named("cross-a"), self.agent_named("cross-b"));
            self.unwind_probe_positions(&[a, b]).await;
        } else {
            self.adversary.mode1_retries = Some(left - 1);
        }
    }

    // --- the forced gap: pending bad debt and the D13/D23/D37 rules ----------------------------

    async fn gap_step(&mut self) {
        let phase = self.adversary.gap.as_ref().map_or(0, |g| g.phase);
        match phase {
            0 => self.gap_build().await,
            1 => self.gap_reopen().await,
            2 => self.gap_after_quiet().await,
            3 => self.gap_next_open().await,
            4 => self.gap_dust().await,
            _ => {}
        }
    }

    /// Mon-Thu in the hour before the daily break, with a day of the run
    /// left: gap-a buys from gap-b at 10x, enough that a 30% gap at the
    /// reopen leaves bad debt pending beyond insurance and the LP window cap,
    /// and more than the other winners can repay before the next open.
    async fn gap_build(&mut self) {
        if self.adversary.done.contains("gap") || !self.adversary.done.contains("net_cap") {
            return;
        }
        let to_close = self.secs_to_close();
        let reopen = self.now + to_close + 3_600;
        let daily = to_close <= 3_600 && to_close >= 900 && schedule::is_open(reopen);
        if !daily || self.adversary.end_time < reopen + 25 * 3_600 || self.pending_total().await > 0 {
            return;
        }
        self.adversary.done.insert("gap");
        let st = self.st().await;
        let Ok(v) = model::view(&st.pool, &st.m, self.px, self.now) else { return };
        let params = st.pool.params;
        let lp_room = if st.pool.loss_window_start != 0 && reopen < st.pool.loss_window_start + pm::LOSS_WINDOW_SECS {
            pm::lp_window_cap(st.pool.loss_window_nav, params.lp_loss_cap_window_bps, st.pool.lp_absorbed_window)
        } else {
            (v.nav_redeem.max(0) as u128 * params.lp_loss_cap_window_bps as u128 / 10_000) as u64
        };
        let gapped = (self.px as f64 * (1.0 - GAP)) as u64;
        // What the other winners could repay: every other short's profit at the gap
        let (a, b) = (self.agent_named("gap-a"), self.agent_named("gap-b"));
        let others: i128 = self.ledger.open.iter().map(|&i| &self.ledger.positions[i]).filter(|p| p.side() == Side::Short && p.owner != b)
            .map(|p| model::pnl(Side::Short, p.state.size, p.state.entry_price, gapped).max(0)).sum();
        let target = st.m.insurance_fund as i128 + lp_room as i128 + (others * 3 / 2).max(0) + 80_000 * USDC as i128;
        let pre_px = self.px;
        let (mut longs, mut shorts) = (Vec::new(), Vec::new());
        let fill = 45_000 * USDC as u128 * 1_000_000 / pre_px as u128;
        for _ in 0..4 {
            let (l, s) = self.build_cross(a, b, (fill * ORDER_FILLS as u128) as u64).await;
            if l.is_empty() {
                break;
            }
            longs.extend(l);
            shorts.extend(s);
            let deficit: i128 = longs.iter().map(|&i| -model::equity(&self.ledger.positions[i].state, gapped, st.m.cumulative_funding)).filter(|&d| d > 0).sum();
            if deficit >= target {
                break;
            }
        }
        if longs.is_empty() {
            self.probe_skip("gap", "the pair could not be built");
            return;
        }
        self.oracle.forced_gap = Some(-GAP);
        self.hit("oracle.forced_gap");
        self.adversary.gap = Some(GapPlan { longs, shorts, pre_px, reopened_at: 0, phase: 1 });
    }

    /// The first tick of the gapped session, inside the profit-close quiet
    /// time and before the keeper: the winner's close is refused
    /// (ProfitCloseQuiet, D23/F48), an LP deposit is refused (Q), the
    /// bankrupt longs are liquidated (pending), and a mode-0 ADL of a winner
    /// whose score is under the fresh floor is refused (AdlScoreBelowFloor).
    async fn gap_reopen(&mut self) {
        if !schedule::is_open(self.now) || self.tick != self.last_reopen_tick {
            return;
        }
        let g = self.adversary.gap.as_mut().unwrap();
        g.phase = 2;
        g.reopened_at = self.now;
        let (pre_px, longs, shorts) = (g.pre_px, g.longs.clone(), g.shorts.clone());
        if (self.px as f64) > pre_px as f64 * (1.0 - GAP * 0.7) {
            self.probe_skip("gap", "the reopen did not gap");
            self.adversary.gap.as_mut().unwrap().phase = 5;
            return;
        }
        let quiet = crate::chain::pool_params().profit_close_quiet_secs as i64;
        let q = crate::chain::pool_params().lp_reopen_quiet_secs as i64;
        let m = self.market().await;
        let winner = |s: &Sim, i: usize| {
            let p = &s.ledger.positions[i];
            p.is_open() && model::pnl(p.side(), p.state.size, p.state.entry_price, s.px) > 0
        };
        // F48: the winner tries to close before its bankrupt counterparty is liquidated
        if let Some(&s1) = shorts.iter().find(|&&i| winner(self, i)) {
            if self.within_open(quiet) && self.oracle_ok(&m, 60) {
                let n0 = self.chain.transactions;
                self.close_bounded(s1, 0, "close_position_probe").await;
                self.probe_rejected("winner_close_in_quiet", n0, "ProfitCloseQuiet");
            } else {
                self.probe_skip("winner_close_in_quiet", "not inside the quiet time");
            }
        }
        // Q: LP deposits wait for the keeper's liquidations to reprice NAV
        if let Some(l) = self.lp_named("lp-sandwich") {
            if self.within_open(q) {
                let n0 = self.chain.transactions;
                self.lp_deposit(l, 1_000 * USDC, 0, "lp_deposit_probe").await;
                self.probe_rejected("lp_deposit_in_q", n0, "LpFlowWindowClosed");
            }
        }
        for &i in &longs {
            if self.ledger.positions[i].is_open() {
                let _ = self.liquidate_one(i, "liquidate").await;
            }
        }
        let pool = self.pool().await;
        if pool.pending_bad_debt_long == 0 {
            self.probe_skip("gap", "the gap left no bad debt pending");
            self.adversary.gap.as_mut().unwrap().phase = 5;
            return;
        }
        self.hit("gap.pending");
        // The opposite-side winners stay out of the keeper's ADL until the probes are done
        for &i in &shorts {
            self.adversary.held.insert(i);
        }
        // D16: a mode-0 ADL under the floor, which starts at 100x and decays over 6 open hours
        let st = self.st().await;
        let below = shorts.iter().copied().find(|&i| {
            let p = &self.ledger.positions[i].state;
            self.ledger.positions[i].is_open() && model::slice_profit(p, p.size, self.px, st.m.cumulative_funding) > 0
                && model::equity(p, self.px, st.m.cumulative_funding) > 0
                && model::adl_score(&st.m, p, self.px) < model::adl_floor(&st.m, &st.pool, Side::Short, self.now) as u128
        });
        match below {
            Some(i) => {
                let kp = self.agents[self.agent_named("prober")].t.kp.insecure_clone();
                let n0 = self.chain.transactions;
                self.adl_by(i, model::ADL_DEFICIT, 0, "adl_probe", &kp).await;
                self.probe_rejected("adl_mode0_below_floor", n0, "AdlScoreBelowFloor");
            }
            None => self.probe_skip("adl_mode0_below_floor", "no winner under the floor"),
        }
    }

    /// Past the quiet time, pending > 0: an opposite-side winner's close is
    /// haircut by min(pending, profit) (D13/F4).
    async fn gap_after_quiet(&mut self) {
        let quiet = crate::chain::pool_params().profit_close_quiet_secs as i64;
        if self.within_open(quiet) || !schedule::is_open(self.now) {
            return;
        }
        self.adversary.gap.as_mut().unwrap().phase = 3;
        let shorts = self.adversary.gap.as_ref().unwrap().shorts.clone();
        let before = self.pool().await;
        if before.pending_bad_debt_long == 0 {
            self.probe_skip("opposite_winner_haircut", "the pending was repaid before the quiet time ended");
            return;
        }
        for i in shorts {
            if !self.ledger.positions[i].is_open() {
                continue;
            }
            let Ok(out) = self.predict_close(i).await else { continue };
            if out.p <= 0 {
                continue;
            }
            self.adversary.held.remove(&i);
            if !self.close_tracked(i, "close_position").await {
                continue;
            }
            let after = self.pool().await;
            let hc = before.pending_bad_debt_long - after.pending_bad_debt_long;
            let want = (before.pending_bad_debt_long as i128).min(out.p - out.solvency_hc as i128).max(0) as u64;
            self.probe_check("opposite_winner_haircut", hc > 0 && hc == want, || {
                format!("profit {} (solvency haircut {}), pending {} -> {}: haircut {}, required {}", out.p, out.solvency_hc, before.pending_bad_debt_long, after.pending_bad_debt_long, hc, want)
            });
            return;
        }
        self.probe_skip("opposite_winner_haircut", "no winner could close");
    }

    /// At the next session open, still inside its quiet time and with the
    /// pending old enough that the floor is 0: the winner's own mode-0 ADL
    /// is refused (ProfitCloseQuiet, D23 second review).
    async fn gap_next_open(&mut self) {
        let quiet = crate::chain::pool_params().profit_close_quiet_secs as i64;
        let reopened_at = self.adversary.gap.as_ref().unwrap().reopened_at;
        if self.now < reopened_at + 3_600 || !self.within_open(quiet) || self.tick != self.last_reopen_tick {
            if self.now >= self.adversary.end_time - self.cfg.tick_secs {
                self.probe_skip("self_adl_in_quiet", "the run ended before the next open");
            }
            return;
        }
        self.adversary.gap.as_mut().unwrap().phase = 4;
        let st = self.st().await;
        if st.pool.pending_bad_debt_long == 0 {
            self.probe_skip("self_adl_in_quiet", "the pending was repaid before the next open");
            return;
        }
        let shorts = self.adversary.gap.as_ref().unwrap().shorts.clone();
        let floor = model::adl_floor(&st.m, &st.pool, Side::Short, self.now) as u128;
        let f = st.m.cumulative_funding;
        let pick = shorts.into_iter().find(|&i| {
            let p = &self.ledger.positions[i].state;
            self.ledger.positions[i].is_open() && model::slice_profit(p, p.size, self.px, f) > 0 && model::equity(p, self.px, f) > 0
                && model::adl_score(&st.m, p, self.px) >= floor
        });
        let Some(i) = pick else {
            self.probe_skip("self_adl_in_quiet", "no winner over the floor");
            return;
        };
        let owner = self.agents[self.agent_named("gap-b")].t.kp.insecure_clone();
        let n0 = self.chain.transactions;
        self.adl_by(i, model::ADL_DEFICIT, 0, "adl_probe", &owner).await;
        self.probe_rejected("self_adl_in_quiet", n0, "ProfitCloseQuiet");
    }

    /// After that quiet time: a donation repays all but one unit of the
    /// pending, and a mode-0 ADL asking for a whole winner closes only the
    /// slice the dust needs (D37), paying the close fee on that slice only
    /// (D22). Then gap-b closes its winners.
    async fn gap_dust(&mut self) {
        let quiet = crate::chain::pool_params().profit_close_quiet_secs as i64;
        if self.within_open(quiet) || !schedule::is_open(self.now) {
            return;
        }
        self.adversary.gap.as_mut().unwrap().phase = 5;
        let shorts = self.adversary.gap.as_ref().unwrap().shorts.clone();
        let pending = self.pool().await.pending_bad_debt_long;
        let p = self.agent_named("prober");
        if pending > 1 {
            self.donate(p, pending - 1, "donate").await;
        }
        let st = self.st().await;
        if st.pool.pending_bad_debt_long != 1 {
            self.probe_skip("dust_pending_slice", "no dust pending");
        } else {
            let floor = model::adl_floor(&st.m, &st.pool, Side::Short, self.now) as u128;
            let f = st.m.cumulative_funding;
            let pick = shorts.iter().copied().find(|&i| {
                let q = &self.ledger.positions[i].state;
                self.ledger.positions[i].is_open() && model::slice_profit(q, q.size, self.px, f) > 0 && model::equity(q, self.px, f) > 0
                    && model::adl_score(&st.m, q, self.px) >= floor && model::notional(q.size, self.px) > 1_000 * USDC as i128
            });
            match pick {
                Some(i) => {
                    let before = self.ledger.positions[i].state.clone();
                    let kp = self.agents[p].t.kp.insecure_clone();
                    let landed = self.adl_by(i, model::ADL_DEFICIT, 0, "adl", &kp).await.is_some();
                    let ev = crate::chain::events::<Adl>(&self.last_logs);
                    let after = self.pool().await;
                    let fee = |x: u64| model::slice_close_fee(&before, x, st.m.taker_fee);
                    let ok = landed && ev.len() == 1 && ev[0].size_closed < before.size && ev[0].fee == fee(ev[0].size_closed)
                        && self.ledger.positions[i].is_open() && after.pending_bad_debt_long == 0;
                    let still_open = self.ledger.positions[i].is_open();
                    let detail = format!("1 unit pending, whole position of {} asked: closed {:?} (fee {:?}, slice fee {}), pending after {}, still open {}", before.size,
                        ev.first().map(|e| e.size_closed), ev.first().map(|e| e.fee), ev.first().map_or(0, |e| fee(e.size_closed)), after.pending_bad_debt_long, still_open);
                    self.probe_check("dust_pending_slice", ok, || detail);
                }
                None => self.probe_skip("dust_pending_slice", "no winner over the floor"),
            }
        }
        for &i in &shorts {
            self.adversary.held.remove(&i);
        }
        let b = self.agent_named("gap-b");
        let a = self.agent_named("gap-a");
        self.unwind_probe_positions(&[a, b]).await;
    }

    // --- LP withdrawal gates (§9.4, D17, F5) ------------------------------------------------

    /// lp-probe deposits and requests at once, so its request matures a
    /// day later; then a withdrawal inside W, inside Q alone, and while bad
    /// debt is pending (outside both) must each be refused.
    async fn probe_lp_withdraw_gates(&mut self) {
        let Some(l) = self.lp_named("lp-probe") else { return };
        let pos = self.lp_position(l).await;
        if pos.shares == 0 && pos.deposited_cum == 0 {
            self.lp_deposit(l, 50_000 * USDC, 0, "lp_deposit").await;
            return;
        }
        if pos.pending_shares == 0 {
            if pos.shares > 0 && pos.requested_at == 0 {
                self.lp_request(l, pos.shares, "lp_request").await;
            }
            return;
        }
        let p = crate::chain::pool_params();
        let window = pm::effective_claim_window_secs(crate::session::SCHEDULE_CME_GLOBEX, pos.claim_window_secs, p.weekend_cap_window_secs, p.lp_reopen_quiet_secs);
        if self.now < pos.unlock_at || (window > 0 && open_secs_between(pos.unlock_at, self.now) as u64 > window) || !schedule::is_open(self.now) {
            return;
        }
        let (in_w, in_q) = (self.in_w(), self.within_open(p.lp_reopen_quiet_secs as i64));
        let pending = self.pending_total().await;
        let (name, code) = if in_w {
            ("lp_withdraw_in_w", "LpFlowWindowClosed")
        } else if in_q {
            ("lp_withdraw_in_q", "LpFlowWindowClosed")
        } else if pending > 0 {
            ("lp_withdraw_adl_required", "PoolAdlPending")
        } else {
            return;
        };
        if self.adversary.done.contains(name) {
            return;
        }
        self.adversary.done.insert(name);
        if code == "PoolAdlPending" {
            // Funding stale would be refused first
            self.crank_funding().await;
        }
        let n0 = self.chain.transactions;
        self.lp_withdraw(l, "lp_withdraw_probe").await;
        self.probe_rejected(name, n0, code);
    }

    // --- settlement (D35, D18) ----------------------------------------------------------------

    /// Before settlement: the sandwiching LP deposits, to request right after
    /// the settled market's whole-fund sweep (sweep_pass checks it).
    pub async fn arm_sweep_sandwich(&mut self) {
        let Some(l) = self.lp_named("lp-sandwich") else { return };
        self.crank_funding().await;
        if self.pending_total().await > 0 || !self.in_session() {
            self.probe_skip("lp_sandwich_sweep", "no deposit before settlement");
            return;
        }
        match self.sandwich_deposit(l, SANDWICH).await {
            Some(shares) => self.adversary.sweep_sandwich = Some((l, shares)),
            None => self.probe_skip("lp_sandwich_sweep", "the deposit before settlement was refused"),
        }
    }

    /// Right after settlement: a matured LP request is refused inside the
    /// settled quiet time (D41), so the losers' deficits are booked before
    /// an LP exits at a NAV that still counts their losses.
    async fn probe_settled_lp_withdraw(&mut self) {
        let m = self.market().await;
        let p = crate::chain::pool_params();
        let quiet = p.profit_close_quiet_secs.max(p.lp_reopen_quiet_secs) as i64;
        if m.settled_at == 0 || self.now - m.settled_at >= quiet {
            self.probe_skip("settled_lp_withdraw_quiet", "past the quiet time");
            return;
        }
        let mut matured = None;
        for l in 0..self.lps.len() {
            let pos = self.lp_position(l).await;
            if pos.pending_shares > 0 && self.now >= pos.unlock_at {
                matured = Some(l);
                break;
            }
        }
        let Some(l) = matured else {
            self.probe_skip("settled_lp_withdraw_quiet", "no matured request at settlement");
            return;
        };
        let n0 = self.chain.transactions;
        self.lp_withdraw(l, "lp_withdraw_probe").await;
        self.probe_rejected("settled_lp_withdraw_quiet", n0, "LpFlowWindowClosed");
    }

    /// Right after settlement: a winner's settled close and its owner's own
    /// close are both refused inside the quiet time (D35), so the keeper
    /// books the losers' deficits first.
    async fn probe_settled_winner(&mut self) {
        let m = self.market().await;
        let quiet = crate::chain::pool_params().profit_close_quiet_secs as i64;
        let winner = self.ledger.open.iter().copied().map(|i| (model::realized(&self.ledger.positions[i].state, m.settlement_price, m.cumulative_funding), i))
            .filter(|&(r, _)| r > 0).max();
        let Some((_, idx)) = winner else {
            self.probe_skip("settled_winner_close_quiet", "no winner at the settlement price");
            return;
        };
        if m.settled_at == 0 || self.now - m.settled_at >= quiet {
            self.probe_skip("settled_winner_close_quiet", "past the quiet time");
            return;
        }
        let n0 = self.chain.transactions;
        self.close_settled_one(idx).await;
        self.probe_rejected("settled_winner_close_quiet", n0, "ProfitCloseQuiet");
        let n0 = self.chain.transactions;
        self.close_bounded(idx, 0, "close_position_probe").await;
        self.probe_rejected("settled_owner_close_quiet", n0, "ProfitCloseQuiet");
    }

    /// The sweep sandwich's check, after the settled market's whole-fund
    /// sweep: the LP's request is worth no more than its shares were just
    /// before the sweep, plus their share of the pending the sweep repaid
    /// (the rest vests, D18).
    pub async fn check_sweep_sandwich(&mut self, value_before: u64, recovered: u64, lp_shares: u64) {
        let Some((l, shares)) = self.adversary.sweep_sandwich.take() else { return };
        let Some(value) = self.sandwich_exit(l, shares).await else {
            self.probe_skip("lp_sandwich_sweep", "the request after the sweep was refused");
            return;
        };
        let allowed = value_before as u128 + (recovered as u128 * shares as u128 + lp_shares as u128) / lp_shares.max(1) as u128 + 1;
        self.probe_check("lp_sandwich_sweep", (value as u128) <= allowed, || format!("worth {} before the sweep (+{} recovered pending), requested {} after", value_before, recovered, value));
    }

    /// The sandwiching LP's shares at NAV_redeem now, if a sweep sandwich is armed.
    pub async fn sweep_sandwich_value(&mut self) -> Option<(u64, u64)> {
        let (_, shares) = self.adversary.sweep_sandwich?;
        let st = self.st().await;
        let v = model::view(&st.pool, &st.m, st.m.settlement_price.max(1), self.now).ok()?;
        Some((pm::redeem_value(shares, st.pool.lp_shares, v.nav_redeem), st.pool.lp_shares))
    }
}
