//! Engine actions with their effects predicted by `model` (and their
//! rejections by `predict`), checked against what the engine actually did
//! (invariants 4, 8, 9, 9b and 10). Every action records a failed transaction
//! in the error histogram.

use crate::chain::{bad_debt_log, TxFailure};
use crate::checks::Class;
use crate::ledger::{Route, Settlement};
use crate::model;
use crate::predict::{self, Leg, Rej, Terms};
use crate::support::*;
use crate::world::Sim;
use mm_registry::TwoSidedQuote;
use perps_core::instructions::VenueApprovalParams;
use perps_core::state::{ExecutionSource, Market, Position, PositionStatus, Side, VenueApproval};

/// A landed transaction: its program logs.
pub struct TxOk {
    pub logs: Vec<String>,
}

/// A fill as the agent expects it to land.
pub struct FillPlan {
    pub route: Route,
    pub taker: usize,
    pub maker: usize,
    pub taker_side: Side,
    pub size: u64,
    pub price: u64,
    pub taker_leverage: u32,
    pub maker_leverage: u32,
    pub collusion: bool,
}

/// State a fill's accounting is checked against.
pub struct FillPre {
    pub market: Market,
    pub taker_free: u64,
    pub maker_free: u64,
    /// Allowance the fill draws from: the grant written in the same
    /// transaction, else what the ledger says the approval holds
    pub taker_allowance: u64,
    pub maker_allowance: u64,
}

/// One mm-registry taker transaction, as the SDK builds it (spec 5.3).
pub struct Take {
    pub agent: usize,
    pub mm: usize,
    pub quote: Pubkey,
    pub size: u64,
    pub is_buy: bool,
    pub limit: u64,
    pub leverage: u32,
    /// The taker approval the transaction writes first (None: use what is there)
    pub approval: Option<VenueApprovalParams>,
    pub label: &'static str,
    pub collusion: bool,
}

/// The SDK's per-fill notional, under the venues' $50k cap.
pub const SDK_FILL_CAP: u64 = 45_000 * USDC;

impl Sim {
    /// Execute with the model's prediction of how it must fail (None: it must
    /// land) and record the outcome.
    pub async fn run(&mut self, label: &str, ixs: &[Instruction], signers: &[&Keypair], predicted: Option<Rej>) -> Result<TxOk, TxFailure> {
        let out = self.chain.exec(label, ixs, signers).await;
        let (tick, now) = (self.tick, self.now);
        match out.result {
            Ok(()) => {
                self.checks.landed(tick, now, label, predicted);
                Ok(TxOk { logs: out.logs })
            }
            Err(f) => {
                if self.checks.failure(tick, now, label, &f, predicted) == Class::Expected {
                    self.hit_rejection(predicted.unwrap());
                }
                Err(f)
            }
        }
    }

    /// Whether a transaction with this prediction is one the sim sends: only
    /// rejections in the label's allowed set are ever sent on purpose.
    pub fn should_send(&mut self, label: &str, predicted: Option<Rej>) -> bool {
        match predicted {
            None => true,
            Some(r) if predict::allowed_for(label).contains(&r.1) => true,
            Some(r) => {
                *self.metrics.skips.entry(format!("{}:{}", label, r.1)).or_default() += 1;
                false
            }
        }
    }

    pub fn agent_of(&self, key: &Pubkey) -> usize {
        self.agents.iter().position(|a| a.t.key() == *key).expect("a known agent")
    }

    /// Deposit everything in the agent's wallet back into perps-core.
    pub async fn redeposit(&mut self, agent: usize) {
        let amount = self.wallet(agent).await;
        if amount == 0 {
            return;
        }
        let kp = self.agents[agent].t.kp.insecure_clone();
        let ix = self.deposit_ix(agent, amount);
        if self.run("deposit", &[ix], &[&kp], None).await.is_ok() {
            self.vault_expected += amount as i128;
        }
    }

    /// withdraw_collateral of `amount`, checked: free collateral and wallet move by exactly it.
    pub async fn withdraw(&mut self, agent: usize, amount: u64, label: &str) -> bool {
        let free = self.user_of(agent).await.collateral_balance;
        let vault = vault_token_balance(self.chain.base()).await;
        let predicted = predict::withdraw(free, amount, vault);
        let wallet = self.wallet(agent).await;
        let kp = self.agents[agent].t.kp.insecure_clone();
        let ix = self.withdraw_ix(agent, amount);
        if self.run(label, &[ix], &[&kp], predicted).await.is_err() {
            return false;
        }
        let after_free = self.user_of(agent).await.collateral_balance;
        let paid = self.wallet(agent).await as i128 - wallet as i128;
        let (tick, now) = (self.tick, self.now);
        self.checks.check(tick, now, "9_settlement_accounting", paid == amount as i128 && after_free + amount == free, || {
            format!("withdraw of {} from {}: wallet +{}, free {} -> {}", amount, free, paid, free, after_free)
        });
        self.vault_expected -= amount as i128;
        if !self.ledger.open_of(agent).is_empty() {
            self.hit("withdraw.with_open_positions");
        }
        true
    }

    pub async fn withdraw_all(&mut self, agent: usize) -> bool {
        let amount = self.user_of(agent).await.collateral_balance;
        amount == 0 || self.withdraw(agent, amount, "withdraw").await
    }

    /// add_margin to the tracked position `idx`, checked.
    pub async fn add_margin(&mut self, idx: usize, amount: u64, label: &str) -> bool {
        let owner = self.ledger.positions[idx].owner;
        let address = self.ledger.positions[idx].address;
        let free = self.user_of(owner).await.collateral_balance;
        let before: Position = load(self.chain.base(), address).await;
        let predicted = predict::add_margin(free, amount);
        let t = &self.agents[owner].t;
        let accounts = perps_core::accounts::AddMargin { owner: t.key(), user_account: t.user, market: self.chain.base.market, position: address };
        let ix = ix(perps_core::ID, accounts, perps_core::instruction::AddMargin { amount });
        let kp = t.kp.insecure_clone();
        if self.run(label, &[ix], &[&kp], predicted).await.is_err() {
            return false;
        }
        let after: Position = load(self.chain.base(), address).await;
        let after_free = self.user_of(owner).await.collateral_balance;
        let (tick, now) = (self.tick, self.now);
        let ok = after.collateral == before.collateral + amount && after_free + amount == free
            && after.size == before.size && after.entry_price == before.entry_price && after.last_funding_payment == before.last_funding_payment;
        self.checks.check(tick, now, "9_settlement_accounting", ok, || {
            format!("add_margin {} to {}: collateral {} -> {}, free {} -> {}", amount, address, before.collateral, after.collateral, free, after_free)
        });
        self.ledger.update(idx, after);
        self.hit("add_margin");
        true
    }

    async fn approval_terms(&mut self, address: Pubkey) -> Option<Terms> {
        try_load::<VenueApproval>(self.chain.base(), address).await.map(|a| Terms::from(&a))
    }

    // --- house: open_position at the oracle -------------------------------------------

    pub async fn open_house(&mut self, agent: usize, side: Side, size: u64, leverage: u32, label: &str) -> bool {
        let m = self.market().await;
        let owner = self.agents[agent].t.key();
        let free = self.user_of(agent).await.collateral_balance;
        let predicted = predict::open_position(&m, &self.ov(), self.now, free, side, size, leverage);
        if !self.should_send(label, predicted) {
            return false;
        }
        let (ix, position) = open_ix(self.chain.base(), &owner, side as u8, size, leverage).await;
        let kp = self.agents[agent].t.kp.insecure_clone();
        if self.run(label, &[ix], &[&kp], predicted).await.is_err() {
            return false;
        }
        let (tick, now, px) = (self.tick, self.now, self.px);
        let p: Option<Position> = try_load(self.chain.base(), position).await;
        let user = self.user_of(agent).await;
        let after = self.market().await;
        let expected = model::open_margin(&m, size, px, leverage, now);
        let ok = match &p {
            Some(p) => {
                Some(p.collateral) == expected
                    && p.entry_price == px
                    && p.side == side
                    && p.size == size
                    && p.leverage == leverage
                    && p.status == PositionStatus::Open
                    && p.last_funding_payment == m.cumulative_funding
                    && user.collateral_balance + p.collateral == free
                    && open_interest(&after, side) == open_interest(&m, side) + size
                    && after.total_positions == m.total_positions + 1
            }
            None => false,
        };
        self.checks.check(tick, now, "8_fill_accounting", ok, || {
            format!("open_position {:?} {} @ {} {}x: expected collateral {:?}, got {:?}", side as u8, size, px, leverage, expected, p.as_ref().map(|p| (p.collateral, p.entry_price)))
        });
        if let Some(p) = p {
            self.ledger.add_position(position, agent, Route::House, tick, p);
            self.metrics.record_volume(Route::House, model::notional(size, px) as u64);
            self.agents[agent].stats.opens += 1;
            self.hit("open_position");
        }
        true
    }

    // --- mm-registry: the SDK's one-shot taker transaction ------------------------------

    /// The best fillable quote for a taker going `side`, other than its own.
    pub async fn best_quote(&mut self, agent: usize, is_buy: bool) -> Option<(usize, Pubkey, TwoSidedQuote)> {
        let mut best: Option<(usize, Pubkey, TwoSidedQuote)> = None;
        for mm in 0..self.agents.len() {
            let Some(quote) = self.agents[mm].mm.as_ref().and_then(|b| b.quote) else { continue };
            if mm == agent || self.agents[mm].persona == crate::agents::Persona::Colluder {
                continue;
            }
            let Some(q) = try_load::<TwoSidedQuote>(self.chain.base(), quote).await else { continue };
            let remaining = if is_buy { q.ask_remaining } else { q.bid_remaining };
            if !q.is_active || self.now > q.expires_at || remaining == 0 {
                continue;
            }
            let better = match &best {
                None => true,
                Some((_, _, b)) => if is_buy { q.ask_price < b.ask_price } else { q.bid_price > b.bid_price },
            };
            if better {
                best = Some((mm, quote, q));
            }
        }
        best
    }

    /// The SDK's taker approval for a fill of `size` at `price` (spec 5.3):
    /// worst-case margin over the venue band, the side asked for only, two
    /// minutes, limit = the taker's limit.
    pub async fn sdk_approval(&mut self, side: Side, size: u64, price: u64, limit: u64, leverage: u32) -> VenueApprovalParams {
        let m = self.market().await;
        let band = load_venue(self.chain.base(), &mm_registry::ID).await.max_price_deviation_bps;
        let is_buy = side == Side::Long;
        let margin_price = if is_buy { limit } else { price.max(limit) };
        let allowance = worst_case_margin(&m, side, size, margin_price, leverage, band, self.now);
        VenueApprovalParams {
            allowance,
            max_leverage: leverage.clamp(1, m.max_leverage),
            max_buy_price: if is_buy { limit } else { 0 },
            min_sell_price: if is_buy { 0 } else { limit },
            max_price_deviation_bps: 0,
            expires_at: self.now + 120,
            allowed_sides: if is_buy { SIDE_LONG } else { SIDE_SHORT },
        }
    }

    /// Fill the best MM quote for `size` on `side` (capped per fill as the SDK
    /// does). Returns false if there was nothing to fill or it did not land.
    pub async fn take_quote(&mut self, agent: usize, side: Side, size: u64, leverage: u32, label: &'static str) -> bool {
        let is_buy = side == Side::Long;
        let Some((mm, quote, q)) = self.best_quote(agent, is_buy).await else {
            self.metrics.no_liquidity += 1;
            return false;
        };
        let price = if is_buy { q.ask_price } else { q.bid_price };
        // 5 bps slippage tolerance; the fill is at the quoted price
        let limit = if is_buy { price + price / 2_000 } else { price - price / 2_000 };
        let cap = SDK_FILL_CAP as u128 * 1_000_000 / price as u128;
        let size = (size as u128).min(cap) as u64 / 1_000 * 1_000;
        if size < CONTRACT {
            return false;
        }
        let approval = self.sdk_approval(side, size, price, limit, leverage).await;
        let take = Take { agent, mm, quote, size, is_buy, limit, leverage, approval: Some(approval), label, collusion: false };
        self.send_take(take).await.unwrap_or(false)
    }

    /// Predict, send and check one taker transaction. None: not sent (the
    /// prediction is a rejection the label never sends); Some(landed).
    pub async fn send_take(&mut self, t: Take) -> Option<bool> {
        let q = try_load::<TwoSidedQuote>(self.chain.base(), t.quote).await?;
        let m = self.market().await;
        let now = self.now;
        let taker_key = self.agents[t.agent].t.key();
        let mm_key = self.agents[t.mm].t.key();
        let taker_approval = approval_pda(&taker_key, &self.mm_venue, ROLE_TAKER);
        let maker_approval = approval_pda(&mm_key, &self.mm_venue, ROLE_MAKER);
        let taker_free = self.user_of(t.agent).await.collateral_balance;
        let maker_free = self.user_of(t.mm).await.collateral_balance;
        let side = if t.is_buy { Side::Long } else { Side::Short };
        let written = t.approval.as_ref().map(Terms::from);
        let taker_terms = match written {
            Some(w) => Some(w),
            None => self.approval_terms(taker_approval).await,
        };
        let maker_terms = self.approval_terms(maker_approval).await;
        let venue = load_venue(self.chain.base(), &mm_registry::ID).await;

        let mut predicted = written.and_then(|w| predict::approval_params(&m, &w, now));
        let mut fill = None;
        if predicted.is_none() {
            match predict::fill_quote(&q, now, t.agent == t.mm, t.size, t.is_buy, t.limit) {
                Err(r) => predicted = Some(r),
                Ok((size, price)) => {
                    let taker = Leg { side, leverage: t.leverage, free: taker_free, approval: taker_terms };
                    let maker = Leg { side: opposite(side), leverage: q.leverage, free: maker_free, approval: maker_terms };
                    if t.label == "fill_quote" && taker_terms.is_some() {
                        // The SDK checks the taker's own leg before sending
                        if let Err(r) = predict::leg(&m, &taker, size, price, self.px, now) {
                            *self.metrics.skips.entry(format!("fill_quote:taker_{}", r.1)).or_default() += 1;
                            return None;
                        }
                    }
                    // mm-registry's taker is a signer (it pays): one signer
                    match predict::venue_fill(&m, false, &venue, &self.ov(), now, 1, false, size, price, &taker, &maker) {
                        Err(r) => predicted = Some(r),
                        Ok(margins) => fill = Some((size, price, margins)),
                    }
                }
            }
        }
        if !self.should_send(t.label, predicted) {
            return None;
        }
        let approval_ix = t.approval.map(|params| {
            if exists_sync(&self.ledger, &taker_approval) {
                update_approval_ix(&self.chain.base, &taker_key, &self.mm_venue, ROLE_TAKER, params)
            } else {
                approve_ix(&self.chain.base, &taker_key, &self.mm_venue, ROLE_TAKER, params)
            }
        });
        let nonce = self.nonce();
        let (fill_ix, fa) = self.fill_quote_ix(&taker_key, &mm_key, t.quote, t.size, t.is_buy, t.limit, t.leverage, nonce);
        let pre = FillPre {
            taker_free,
            maker_free,
            taker_allowance: written.map(|w| w.allowance).or(self.ledger.approval_cap(&taker_approval)).unwrap_or(0),
            maker_allowance: self.ledger.approval_cap(&maker_approval).unwrap_or(0),
            market: m,
        };
        let mut ixs = vec![compute_budget_ix(300_000)];
        ixs.extend(approval_ix);
        ixs.push(fill_ix);
        let kp = self.agents[t.agent].t.kp.insecure_clone();
        let result = self.run(t.label, &ixs, &[&kp], predicted).await;
        if result.is_err() {
            return Some(false);
        }
        if let Some(w) = written {
            // The owner's own grant, in this transaction
            self.ledger.track_approval(taker_approval, w.allowance);
        }
        let (size, price) = match fill {
            Some((size, price, _)) => (size, price),
            // Landed against the prediction (invariant 10 already flagged it): check what the engine did
            None => (t.size.min(if t.is_buy { q.ask_remaining } else { q.bid_remaining }), if t.is_buy { q.ask_price } else { q.bid_price }),
        };
        let plan = FillPlan {
            route: Route::MmRegistry, taker: t.agent, maker: t.mm, taker_side: side, size, price,
            taker_leverage: t.leverage, maker_leverage: q.leverage, collusion: t.collusion,
        };
        self.settle_fill(plan, pre, fa).await;
        self.ledger.record_quote_fill(t.quote);
        if model::notional(size, price) > SDK_FILL_CAP as i128 {
            self.hit("fill.over_sdk_cap");
        }
        Some(true)
    }

    /// Check a landed venue fill against the model and record both legs.
    pub async fn settle_fill(&mut self, plan: FillPlan, pre: FillPre, fa: FillAccounts) -> Option<usize> {
        let (tick, now, px) = (self.tick, self.now, self.px);
        let maker_side = opposite(plan.taker_side);
        let m = &pre.market;
        let c_taker = model::venue_margin(m, plan.taker_side, plan.size, plan.price, px, plan.taker_leverage, now);
        let c_maker = model::venue_margin(m, maker_side, plan.size, plan.price, px, plan.maker_leverage, now);
        let taker_pos: Option<Position> = try_load(self.chain.base(), fa.taker_position).await;
        let maker_pos: Option<Position> = try_load(self.chain.base(), fa.maker_position).await;
        let taker_free = self.user_of(plan.taker).await.collateral_balance;
        let maker_free = self.user_of(plan.maker).await.collateral_balance;
        let after = self.market().await;

        let leg_ok = |p: &Option<Position>, owner: Pubkey, side: Side, lev: u32, c: Option<u64>| match p {
            Some(p) => {
                p.owner == owner && p.side == side && p.size == plan.size && p.entry_price == plan.price && p.leverage == lev
                    && Some(p.collateral) == c && p.status == PositionStatus::Open && p.execution_source == ExecutionSource::Venue
                    && p.last_funding_payment == m.cumulative_funding && p.market == fa.market
            }
            None => false,
        };
        // Spec 2.2: no position is born below IM at the oracle (model ratio, both legs)
        let (im, _) = model::margin_ratios(m, now);
        let born_ok = |p: &Option<Position>| p.as_ref().map_or(false, |p| model::margin_ratio_bps(p, px, m.cumulative_funding) >= im as i128);
        // A3: venue_fill leaves market.total_positions (open_position's PDA nonce) alone
        let ok = leg_ok(&taker_pos, fa.taker, plan.taker_side, plan.taker_leverage, c_taker)
            && leg_ok(&maker_pos, fa.maker, maker_side, plan.maker_leverage, c_maker)
            && c_taker.map_or(false, |c| pre.taker_free.checked_sub(c) == Some(taker_free))
            && c_maker.map_or(false, |c| pre.maker_free.checked_sub(c) == Some(maker_free))
            && born_ok(&taker_pos) && born_ok(&maker_pos)
            && after.long_open_interest == m.long_open_interest + plan.size
            && after.short_open_interest == m.short_open_interest + plan.size
            && after.total_positions == m.total_positions;
        self.checks.check(tick, now, "8_fill_accounting", ok, || {
            format!(
                "{} fill {} {} @ {} (oracle {}): expected margins {:?}/{:?}, positions {:?}/{:?}, free {}->{} / {}->{}",
                plan.route.name(), plan.taker_side as u8, plan.size, plan.price, px, c_taker, c_maker,
                taker_pos.as_ref().map(|p| (p.collateral, p.size, p.entry_price)), maker_pos.as_ref().map(|p| (p.collateral, p.size, p.entry_price)),
                pre.taker_free, taker_free, pre.maker_free, maker_free
            )
        });
        // Invariant 5's caps: the allowance drawn from, minus exactly the margin committed
        for (address, before, c) in [(fa.taker_approval, pre.taker_allowance, c_taker), (fa.maker_approval, pre.maker_allowance, c_maker)] {
            match c {
                Some(c) => self.ledger.track_approval(address, before.saturating_sub(c)),
                None => {
                    let held = self.approval_terms(address).await.map_or(0, |t| t.allowance);
                    self.ledger.track_approval(address, held);
                }
            }
        }
        let (Some(tp), Some(mp)) = (taker_pos, maker_pos) else { return None };
        let notional = model::notional(plan.size, plan.price) as u64;
        // Each side's edge at the oracle: what the maker captured from the spread
        let maker_edge = model::pnl(maker_side, plan.size, plan.price, px);
        let t = self.ledger.add_position(fa.taker_position, plan.taker, plan.route, tick, tp);
        let mk = self.ledger.add_position(fa.maker_position, plan.maker, plan.route, tick, mp);
        let pair = self.ledger.add_pair(t, mk, plan.collusion);
        self.metrics.record_volume(plan.route, notional);
        self.hit(format!("fill.{}", plan.route.name()));
        self.agents[plan.taker].stats.opens += 1;
        self.agents[plan.maker].stats.maker_fills += 1;
        for who in [plan.taker, plan.maker] {
            if let Some(book) = self.agents[who].mm.as_mut() {
                let edge = if who == plan.maker { maker_edge } else { -maker_edge };
                book.econ.fills += 1;
                book.econ.volume += notional as u128;
                book.econ.spread_captured += edge;
            }
        }
        Some(pair)
    }

    // --- close ---------------------------------------------------------------------------

    /// close_position for the tracked position `idx`, checked against the model.
    pub async fn close_tracked(&mut self, idx: usize, label: &str) -> bool {
        let address = self.ledger.positions[idx].address;
        let owner = self.ledger.positions[idx].owner;
        let p: Position = load(self.chain.base(), address).await;
        let m = self.market().await;
        let predicted = predict::close_position(&m, &self.ov(), self.now);
        if !self.should_send(label, predicted) {
            return false;
        }
        let mark = predict::close_mark(&m, &self.ov());
        let wallet = self.wallet(owner).await;
        let ix = close_ix(&self.chain.base, &self.agents[owner].t, address);
        let kp = self.agents[owner].t.kp.insecure_clone();
        let Ok(ok) = self.run(label, &[ix], &[&kp], predicted).await else { return false };
        let (tick, now) = (self.tick, self.now);
        let expected = model::close(&m, &p, mark);
        let after_p: Position = load(self.chain.base(), address).await;
        let after_m = self.market().await;
        let paid = self.wallet(owner).await as i128 - wallet as i128;
        let fine = paid == expected.payout as i128
            && after_m.insurance_fund == expected.fund_after
            && after_p.status == PositionStatus::Closed
            && after_p.realized_pnl as i128 == expected.realized
            && open_interest(&after_m, p.side) == open_interest(&m, p.side) - p.size;
        self.checks.check(tick, now, "9_settlement_accounting", fine, || {
            format!("close of {} ({:?} {} @ {}, collateral {}) at {}: expected {:?}, paid {}, realized {}, fund {} -> {}", address, p.side as u8, p.size, p.entry_price, p.collateral, mark, expected, paid, after_p.realized_pnl, m.insurance_fund, after_m.insurance_fund)
        });
        self.check_bad_debt_log("close", &ok.logs, expected.deficit, expected.uncovered);
        // 2b: the vault pays what the model says it pays
        self.vault_expected -= expected.payout as i128;
        self.ledger.update(idx, after_p);
        let settlement = Settlement {
            liquidated: false, paid, insurance_delta: after_m.insurance_fund as i128 - m.insurance_fund as i128,
            collateral: p.collateral, mark, cumulative_funding: m.cumulative_funding, uncovered: expected.uncovered,
        };
        self.ledger.settle(idx, settlement);
        self.check_pair(idx).await;
        self.metrics.record_close(&expected);
        self.hit("close");
        if m.settlement_price > 0 {
            self.hit("close.at_settlement");
        }
        if expected.deficit > 0 {
            self.hit("close.deficit");
            self.record_bad_debt("close", idx, expected.deficit, expected.uncovered, 0, after_m.insurance_fund);
        }
        let a = &mut self.agents[owner];
        a.stats.closes += 1;
        a.stats.fees_paid += expected.fee;
        if let Some(book) = a.mm.as_mut() {
            book.econ.close_fees += expected.fee;
        }
        true
    }

    /// 9b: the engine's own "Bad debt: deficit=, uncovered=" message matches the model exactly.
    pub fn check_bad_debt_log(&mut self, what: &str, logs: &[String], deficit: u64, uncovered: u64) {
        let logged = bad_debt_log(logs);
        let ok = if deficit > 0 { logged == Some((deficit, uncovered)) } else { logged.is_none() };
        let (tick, now) = (self.tick, self.now);
        self.checks.check(tick, now, "9b_bad_debt_accounting", ok, || {
            format!("{}: model deficit {} uncovered {}, engine logged {:?}", what, deficit, uncovered, logged)
        });
    }

    pub fn record_bad_debt(&mut self, kind: &str, idx: usize, deficit: u64, uncovered: u64, top_up: u64, fund_after: u64) {
        if uncovered > 0 {
            self.hit("bad_debt.uncovered");
        }
        self.metrics.uncovered_bad_debt += uncovered;
        let owner = self.ledger.positions[idx].owner;
        self.metrics.bad_debt_events.push(serde_json::json!({
            "kind": kind, "time": self.now, "tick": self.tick, "owner": self.agents[owner].name, "route": self.ledger.positions[idx].route.name(),
            "deficit_usdc": deficit as f64 / 1e6, "uncovered_usdc": uncovered as f64 / 1e6, "reward_top_up_usdc": top_up as f64 / 1e6,
            "insurance_after_usdc": fund_after as f64 / 1e6,
        }));
    }

    /// Invariant 4, once both legs of `idx`'s pair have settled (closed or
    /// liquidated, at any marks): everything the vault paid out for the pair,
    /// plus what the insurance fund gained from it, equals both legs'
    /// collateral, plus the vault's PnL from settling them at different marks
    /// and funding stamps, plus bad debt nobody absorbed. With equal marks and
    /// stamps that PnL is exactly zero (spec 2.2: "closing both pays out
    /// c_T + c_M - fee_T - fee_M").
    pub async fn check_pair(&mut self, idx: usize) {
        let Some(pair) = self.ledger.positions[idx].pair else { return };
        let (t, k) = (self.ledger.pairs[pair].taker, self.ledger.pairs[pair].maker);
        let (Some(st), Some(sk)) = (self.ledger.positions[t].settled, self.ledger.positions[k].settled) else { return };
        let leg = |i: usize, s: &Settlement| {
            let p = &self.ledger.positions[i].state;
            model::pnl(p.side, p.size, p.entry_price, s.mark) + model::funding(p.side, p.size, p.last_funding_payment, s.cumulative_funding)
        };
        let vault_pnl = leg(t, &st) + leg(k, &sk);
        let lhs = st.paid + st.insurance_delta + sk.paid + sk.insurance_delta;
        let rhs = st.collateral as i128 + sk.collateral as i128 + vault_pnl + st.uncovered as i128 + sk.uncovered as i128;
        let same_mark = st.mark == sk.mark && st.cumulative_funding == sk.cumulative_funding;
        let (tick, now) = (self.tick, self.now);
        let (ta, ka) = (self.ledger.positions[t].address, self.ledger.positions[k].address);
        self.checks.check(tick, now, "4_pair_settlement", lhs == rhs && (!same_mark || vault_pnl == 0), || {
            format!("pair {} / {}: paid + insurance {} != collateral + vault PnL {} + uncovered ({} = {} + {} + {} + {} + {}); marks {} / {}",
                ta, ka, lhs, vault_pnl, rhs, st.collateral, sk.collateral, vault_pnl, st.uncovered, sk.uncovered, st.mark, sk.mark)
        });
        self.hit(if same_mark { "pair.settled_same_mark" } else { "pair.settled_different_marks" });
        if self.ledger.pairs[pair].collusion {
            // 12: closed together, the colluders only moved value between themselves
            let closed_together = same_mark && !st.liquidated && !sk.liquidated;
            let no_draw = st.uncovered == 0 && sk.uncovered == 0 && st.insurance_delta >= 0 && sk.insurance_delta >= 0;
            let zero_sum = st.paid + sk.paid + st.insurance_delta + sk.insurance_delta == st.collateral as i128 + sk.collateral as i128;
            self.checks.check(tick, now, "12_collusion_no_bad_debt", !closed_together || (no_draw && zero_sum), || {
                format!("colluding pair {} / {} closed at {}: paid {} + {}, insurance {} + {}, collateral {} + {}", ta, ka, st.mark, st.paid, sk.paid, st.insurance_delta, sk.insurance_delta, st.collateral, sk.collateral)
            });
        }
    }
}

/// The ledger tracks every approval the sim's agents created.
fn exists_sync(ledger: &crate::ledger::Ledger, address: &Pubkey) -> bool {
    ledger.approval_cap(address).is_some()
}

pub fn opposite(side: Side) -> Side {
    match side {
        Side::Long => Side::Short,
        Side::Short => Side::Long,
    }
}

pub fn open_interest(m: &Market, side: Side) -> u64 {
    match side {
        Side::Long => m.long_open_interest,
        Side::Short => m.short_open_interest,
    }
}

/// The taker's one-shot allowance: the margin a fill at `price` posts at the
/// worst oracle the venue band allows. venue_margin is convex in the oracle,
/// so the band's ends bound it (as in mm-registry's tests). 0 when the
/// leverage is above what the market allows (the fill then fails on leverage).
pub fn worst_case_margin(m: &Market, side: Side, size: u64, price: u64, leverage: u32, band_bps: u16, now: i64) -> u64 {
    let (p, b) = (price as u128, band_bps as u128);
    let lo = ((p * 10_000 + 10_000 + b - 1) / (10_000 + b)) as u64;
    let hi = (p * 10_000 / (10_000 - b)) as u64;
    [lo, price, hi].iter().map(|&o| model::venue_margin(m, side, size, price, o, leverage, now).unwrap_or(0)).max().unwrap()
}
