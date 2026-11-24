//! Engine actions with their effects predicted by `model` (and their
//! rejections by `model` and `predict`), checked against what the engine
//! actually did (invariants 4, 8, 9, 9b and 10): after every landed
//! transaction that touches the market or the LP pool, the engine's Market,
//! LpPool, Position, UserAccount and LpPosition must equal the model's field
//! by field, its events must be the model's, and its token flows go into the
//! vault ledger (2b). Every action records a failed transaction in the error
//! histogram.

use crate::chain::{events, TxFailure};
use crate::checks::Class;
use crate::ledger::{Route, Settlement};
use crate::model::{self, Deficit, FundingEv, St};
use crate::predict::{self, Leg, Rej, Terms};
use crate::support::*;
use crate::world::Sim;
use mm_registry::TwoSidedQuote;
use perps_core::events::{BadDebt, FundingUpdated};
use perps_core::instructions::VenueApprovalParams;
use perps_core::state::{ExecutionSource, LpPosition, Market, Position, PositionStatus, Side, UserAccount, VenueApproval};
use serde_json::json;

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
    /// The market and pool before the fill
    pub st: St,
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
        // The probes (adversary.rs) read how their transaction ended
        self.last_failure = out.result.as_ref().err().cloned();
        self.last_logs = out.logs.clone();
        match out.result {
            Ok(()) => {
                self.checks.landed(tick, now, label, predicted);
                // Every crank, inline or not, for the report (§17.4: none may drop periods)
                for e in events::<FundingUpdated>(&out.logs) {
                    self.metrics.funding_periods_dropped += e.periods_dropped;
                    self.metrics.funding_updates.push(json!({
                        "time": now, "by": label, "periods": e.periods, "periods_dropped": e.periods_dropped, "rate": e.rate,
                        "step": e.step, "cumulative": e.cumulative_funding, "carry_secs": e.carry_secs, "price": self.px,
                    }));
                }
                Ok(TxOk { logs: out.logs })
            }
            Err(f) => {
                if f.code == "PoolCashInsufficient" {
                    *self.pool_stats.cash_insufficient.entry(label.to_string()).or_default() += 1;
                }
                if self.checks.failure(tick, now, label, &f, predicted) == Class::Expected {
                    self.hit_rejection(predicted.unwrap());
                }
                Err(f)
            }
        }
    }

    // --- checking a landed transaction against the model ----------------------------------

    /// The engine's market and pool after a landed transaction must equal the
    /// model's (invariant 9); the shadow (invariants 2a, 2c) moves to the
    /// engine's state either way, so one difference is reported once.
    pub async fn expect_st(&mut self, label: &str, model: &St) -> bool {
        let chain = self.st().await;
        let mut diffs = model::diff_market(&chain.m, &model.m);
        diffs.extend(model::diff_pool(&chain.pool, &model.pool));
        let (tick, now) = (self.tick, self.now);
        let ok = self.checks.check(tick, now, "9_settlement_accounting", diffs.is_empty(), || {
            format!("{}: market/pool differ from the model: {}", label, diffs.join("; "))
        });
        let pre = self.shadow.take();
        self.bound_step(label, pre.as_ref(), &chain);
        self.shadow = Some(chain);
        ok
    }

    /// The shadow follows the chain after a transaction the model does not
    /// price field by field (a landed rejection, already flagged).
    pub async fn resync(&mut self) {
        let chain = self.st().await;
        let pre = self.shadow.take();
        self.bound_step("unmodelled", pre.as_ref(), &chain);
        self.shadow = Some(chain);
    }

    /// A position after a landed transaction must equal the model's.
    pub async fn expect_position(&mut self, label: &str, address: Pubkey, model: &Position) -> Position {
        let chain: Position = load(self.chain.base(), address).await;
        let diffs = model::diff_position(&chain, model);
        let (tick, now) = (self.tick, self.now);
        self.checks.check(tick, now, "9_settlement_accounting", diffs.is_empty(), || {
            format!("{}: position {} differs from the model: {}", label, address, diffs.join("; "))
        });
        chain
    }

    /// A UserAccount after a landed transaction must equal the model's.
    pub async fn expect_user(&mut self, label: &str, address: Pubkey, model: &UserAccount) {
        let chain: UserAccount = load(self.chain.base(), address).await;
        let diffs = model::diff_user(&chain, model);
        let (tick, now) = (self.tick, self.now);
        self.checks.check(tick, now, "9_settlement_accounting", diffs.is_empty(), || {
            format!("{}: user {} differs from the model: {}", label, address, diffs.join("; "))
        });
    }

    pub async fn user_at(&mut self, address: Pubkey) -> UserAccount {
        load(self.chain.base(), address).await
    }

    /// The transaction's FundingUpdated and BadDebt events are exactly the
    /// model's crank (if any) and deficits (9, 9b).
    pub fn check_events(&mut self, label: &str, logs: &[String], funding: Option<FundingEv>, deficits: &[Deficit]) {
        let (tick, now) = (self.tick, self.now);
        let got: Vec<(u64, u64, i64, i64, u32)> = events::<FundingUpdated>(logs).iter().map(|e| (e.periods, e.periods_dropped, e.rate, e.step, e.carry_secs)).collect();
        let want: Vec<(u64, u64, i64, i64, u32)> = funding.iter().map(|e| (e.n, e.dropped, e.rate, e.step, e.carry)).collect();
        self.checks.check(tick, now, "9_settlement_accounting", got == want, || {
            format!("{}: FundingUpdated (periods, dropped, rate, step, carry) {:?}, model {:?}", label, got, want)
        });
        let got: Vec<(u8, u64, u64, u64, u64)> = events::<BadDebt>(logs).iter().map(|e| (e.side, e.deficit, e.insurance_paid, e.lp_absorbed, e.pending_added)).collect();
        let want: Vec<(u8, u64, u64, u64, u64)> = deficits.iter().map(|d| (d.side as u8, d.d, d.ins_paid, d.lp_absorbed, d.pending)).collect();
        self.checks.check(tick, now, "9b_bad_debt_accounting", got == want, || {
            format!("{}: BadDebt (side, deficit, insurance, lp, pending) {:?}, model {:?}", label, got, want)
        });
        for d in deficits {
            self.pool_stats.deficit_total += d.d;
            self.pool_stats.deficit_beyond_insurance += d.d - d.ins_paid;
            self.pool_stats.deficit_lp_absorbed += d.lp_absorbed;
            self.pool_stats.deficit_pending += d.pending;
            self.pool_stats.deficits.push(json!({
                "time": now, "tick": tick, "by": label, "side": d.side as u8, "deficit_usdc": d.d as f64 / 1e6,
                "insurance_usdc": d.ins_paid as f64 / 1e6, "lp_absorbed_usdc": d.lp_absorbed as f64 / 1e6, "pending_usdc": d.pending as f64 / 1e6,
            }));
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
        let user = self.agents[agent].t.user;
        let (mut st, mut u) = (self.st().await, self.user_at(user).await);
        let predicted = model::deposit(&mut st, &mut u, amount).err();
        if self.run("deposit", &[ix], &[&kp], predicted).await.is_ok() {
            self.vault_expected += amount as i128;
            self.expect_st("deposit", &st).await;
            self.expect_user("deposit", user, &u).await;
        }
    }

    /// withdraw_collateral of `amount`, checked: free collateral and wallet move by exactly it.
    pub async fn withdraw(&mut self, agent: usize, amount: u64, label: &str) -> bool {
        let free = self.user_of(agent).await.collateral_balance;
        let vault = vault_token_balance(self.chain.base()).await;
        let user = self.agents[agent].t.user;
        let (mut st, mut u) = (self.st().await, self.user_at(user).await);
        let predicted = predict::withdraw(free, amount, vault).or(model::withdraw(&mut st, &mut u, amount).err());
        let wallet = self.wallet(agent).await;
        let kp = self.agents[agent].t.kp.insecure_clone();
        let ix = self.withdraw_ix(agent, amount);
        if self.run(label, &[ix], &[&kp], predicted).await.is_err() {
            return false;
        }
        self.expect_st(label, &st).await;
        self.expect_user(label, user, &u).await;
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
        let user = self.agents[owner].t.user;
        let (mut st, mut u, mut p) = (self.st().await, self.user_at(user).await, before.clone());
        let predicted = model::add_margin(&mut st, &mut u, &mut p, amount, self.now).err();
        let t = &self.agents[owner].t;
        let accounts = perps_core::accounts::AddMargin { owner: t.key(), user_account: t.user, market: self.chain.base.market, position: address, lp_pool: self.chain.base.lp_pool };
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
        self.expect_st(label, &st).await;
        self.expect_user(label, user, &u).await;
        let after = self.expect_position(label, address, &p).await;
        self.ledger.update(idx, after);
        self.hit("add_margin");
        true
    }

    async fn approval_terms(&mut self, address: Pubkey) -> Option<Terms> {
        try_load::<VenueApproval>(self.chain.base(), address).await.map(|a| Terms::from(&a))
    }

    // --- house: open_position at the oracle -------------------------------------------

    pub async fn open_house(&mut self, agent: usize, side: Side, size: u64, leverage: u32, label: &str) -> bool {
        self.open_house_bounded(agent, side, size, leverage, 0, label).await
    }

    /// open_position against the pool at the skew price, `acceptable_price`
    /// bounding it (0: none), predicted and checked by the model (§4.4).
    pub async fn open_house_bounded(&mut self, agent: usize, side: Side, size: u64, leverage: u32, acceptable_price: u64, label: &str) -> bool {
        let user = self.agents[agent].t.user;
        let owner = self.agents[agent].t.key();
        let (mut st, mut u) = (self.st().await, self.user_at(user).await);
        let res = model::open(&mut st, &mut u, &self.ov(), self.now, side, size, leverage, acceptable_price);
        let predicted = res.as_ref().err().copied();
        if !self.should_send(label, predicted) {
            return false;
        }
        let (ix, position) = open_ix_with(self.chain.base(), &owner, side as u8, size, leverage, acceptable_price).await;
        let kp = self.agents[agent].t.kp.insecure_clone();
        let Ok(ok) = self.run(label, &[ix], &[&kp], predicted).await else { return false };
        let (tick, now) = (self.tick, self.now);
        let Ok(out) = res else {
            // Landed against the prediction (invariant 10 has it): track what the engine did
            self.resync().await;
            if let Some(p) = try_load::<Position>(self.chain.base(), position).await {
                self.ledger.add_position(position, agent, Route::House, tick, p);
            }
            return true;
        };
        self.expect_st(label, &st).await;
        self.expect_user(label, user, &u).await;
        let p = self.expect_position(label, position, &out.position).await;
        self.check_events(label, &ok.logs, out.funding, &[]);
        let m = &st.m;
        self.checks.check(tick, now, "8_fill_accounting", p.market == self.chain.base.market && p.execution_source == ExecutionSource::OrderBook, || {
            format!("open_position {}: wrong market or source", position)
        });
        // No house position is born below IM at the oracle either
        let (im, _) = model::margin_ratios(m, now);
        self.checks.check(tick, now, "8_fill_accounting", model::margin_ratio_bps(&p, self.px, m.cumulative_funding) >= im as i128, || {
            format!("open_position {}: born below IM at the oracle ({} bps)", position, model::margin_ratio_bps(&p, self.px, m.cumulative_funding))
        });
        self.ledger.add_position(position, agent, Route::House, tick, p);
        self.metrics.record_volume(Route::House, out.notional);
        self.pool_stats.open_fees += out.fee;
        self.pool_stats.house_opens += 1;
        self.agents[agent].stats.opens += 1;
        self.hit("open_position");
        if out.exec != self.px {
            self.hit("open_position.skew_impact");
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
            st: model::St { m, pool: self.pool().await },
        };
        let mut ixs = vec![compute_budget_ix(300_000)];
        ixs.extend(approval_ix);
        ixs.push(fill_ix);
        let kp = self.agents[t.agent].t.kp.insecure_clone();
        let result = self.run(t.label, &ixs, &[&kp], predicted).await;
        let Ok(ok) = result else {
            return Some(false);
        };
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
        self.settle_fill(plan, pre, fa, &ok.logs).await;
        self.ledger.record_quote_fill(t.quote);
        if model::notional(size, price) > SDK_FILL_CAP as i128 {
            self.hit("fill.over_sdk_cap");
        }
        Some(true)
    }

    /// Check a landed venue fill against the model and record both legs.
    pub async fn settle_fill(&mut self, plan: FillPlan, pre: FillPre, fa: FillAccounts, logs: &[String]) -> Option<usize> {
        let (tick, now, px) = (self.tick, self.now, self.px);
        let maker_side = opposite(plan.taker_side);
        let m = &pre.st.m;
        let c_taker = model::venue_margin(m, plan.taker_side, plan.size, plan.price, px, plan.taker_leverage, now);
        let c_maker = model::venue_margin(m, maker_side, plan.size, plan.price, px, plan.maker_leverage, now);
        let taker_pos: Option<Position> = try_load(self.chain.base(), fa.taker_position).await;
        let maker_pos: Option<Position> = try_load(self.chain.base(), fa.maker_position).await;
        let taker_free = self.user_of(plan.taker).await.collateral_balance;
        let maker_free = self.user_of(plan.maker).await.collateral_balance;
        let after = self.market().await;
        // The pool's side: the inline crank, both legs into the aggregates (§4.8)
        let mut st = pre.st.clone();
        let fill = match (c_taker, c_maker) {
            (Some(tc), Some(mc)) => model::fill(&mut st, plan.taker_side, plan.size, plan.price, tc, mc, px, now).ok(),
            _ => None,
        };
        match fill {
            Some(ev) => {
                // A matched pair leaves U where it was (§2.3)
                let u_before = model::view(&pre.st.pool, &pre.st.m, px, now).map(|v| v.u);
                let mut cranked = pre.st.clone();
                model::crank(&mut cranked.m, px, now);
                let u_cranked = model::view(&cranked.pool, &cranked.m, px, now).map(|v| v.u);
                let u_after = model::view(&st.pool, &st.m, px, now).map(|v| v.u);
                self.checks.check(tick, now, "2c_pool_aggregates", u_cranked.is_ok() && u_cranked == u_after, || {
                    format!("{} fill moved U: {:?} (before the crank {:?}) -> {:?}", plan.route.name(), u_cranked, u_before, u_after)
                });
                self.expect_st(plan.route.name(), &st).await;
                self.check_events(plan.route.name(), logs, ev, &[]);
            }
            None => self.resync().await,
        }
        let m = &st.m;

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
            && after.long_open_interest == pre.st.m.long_open_interest + plan.size
            && after.short_open_interest == pre.st.m.short_open_interest + plan.size
            && after.total_positions == pre.st.m.total_positions;
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
        self.close_bounded(idx, 0, label).await
    }

    /// close_position with an `acceptable_price` bound (0: none), §4.5.
    pub async fn close_bounded(&mut self, idx: usize, acceptable_price: u64, label: &str) -> bool {
        let address = self.ledger.positions[idx].address;
        let owner = self.ledger.positions[idx].owner;
        let user = self.agents[owner].t.user;
        let before: Position = load(self.chain.base(), address).await;
        let (mut st, mut u, mut p) = (self.st().await, self.user_at(user).await, before.clone());
        let res = model::close_position(&mut st, &mut p, &mut u, &self.ov(), self.now, acceptable_price);
        let predicted = res.as_ref().err().copied();
        if !self.should_send(label, predicted) {
            return false;
        }
        let wallet = self.wallet(owner).await;
        let ix = close_ix_with(&self.chain.base, &self.agents[owner].t, address, acceptable_price);
        let kp = self.agents[owner].t.kp.insecure_clone();
        let Ok(ok) = self.run(label, &[ix], &[&kp], predicted).await else { return false };
        let paid = self.wallet(owner).await as i128 - wallet as i128;
        let Ok(out) = res else {
            self.resync().await;
            self.vault_expected -= paid;
            let after: Position = load(self.chain.base(), address).await;
            self.ledger.update(idx, after);
            return true;
        };
        let (tick, now) = (self.tick, self.now);
        self.expect_st(label, &st).await;
        self.expect_user(label, user, &u).await;
        let after = self.expect_position(label, address, &p).await;
        let deficits: Vec<Deficit> = out.deficit.into_iter().collect();
        self.check_events(label, &ok.logs, out.funding, &deficits);
        self.checks.check(tick, now, "9_settlement_accounting", paid == out.payout as i128, || {
            format!("close of {}: paid {}, model {} ({:?})", address, paid, out.payout, out)
        });
        // 2b: the vault pays what the model says it pays
        self.vault_expected -= out.payout as i128;
        self.ledger.update(idx, after);
        self.record_close_piece(idx, &before, &out, paid, st.m.cumulative_funding, false);
        self.check_pair(idx).await;
        self.metrics.record_close(out.fee, out.deficit.map_or(0, |d| d.d));
        self.hit("close");
        if st.m.settlement_price > 0 {
            self.hit("close.at_settlement");
        }
        if out.exec != out.mark {
            self.hit("close.skew_impact");
        }
        if out.solvency_hc + out.bad_debt_hc > 0 {
            self.hit("close.haircut");
        }
        if let Some(d) = out.deficit {
            self.hit("close.deficit");
            self.record_bad_debt("close", idx, &d);
        }
        let a = &mut self.agents[owner];
        a.stats.closes += 1;
        a.stats.fees_paid += out.fee;
        if let Some(book) = a.mm.as_mut() {
            book.econ.close_fees += out.fee;
        }
        true
    }

    /// Record a full close (close_position or close_settled_position) of `idx`.
    pub fn record_close_piece(&mut self, idx: usize, before: &Position, out: &model::CloseOut, paid: i128, cumulative_funding: i64, settled_close: bool) {
        let piece = Settlement {
            liquidated: false, paid, fees: out.fee as i128, top_up: 0, collateral: before.collateral as i128, realized: out.p,
            haircut: (out.solvency_hc + out.bad_debt_hc) as i128, deficit: out.deficit.map_or(0, |d| d.d as i128),
            insurance_draw: out.deficit.map_or(0, |d| d.ins_paid as i128), mark: out.exec, oracle: out.mark,
            cumulative_funding, pieces: 1, done: true,
        };
        self.pool_stats.haircut("solvency", out.solvency_hc);
        self.pool_stats.haircut("bad_debt", out.bad_debt_hc);
        self.ledger.settle(idx, piece);
        let (tick, now) = (self.tick, self.now);
        let s = self.ledger.positions[idx].settled;
        let address = self.ledger.positions[idx].address;
        self.checks.check(tick, now, "4_pair_settlement", piece.balanced(), || {
            format!("{} of {}: paid {} + fees {} != collateral {} + realized {} - haircut {} + deficit {}{}",
                if settled_close { "settled close" } else { "close" }, address, piece.paid, piece.fees, piece.collateral, piece.realized, piece.haircut, piece.deficit,
                if s.balanced() { "" } else { " (and over its life)" })
        });
    }

    /// A deficit was booked through the waterfall.
    pub fn record_bad_debt(&mut self, kind: &str, idx: usize, d: &Deficit) {
        if d.pending > 0 {
            self.hit("bad_debt.pending");
        }
        if d.lp_absorbed > 0 {
            self.hit("bad_debt.lp_absorbed");
        }
        self.metrics.uncovered_bad_debt += d.d - d.ins_paid;
        let owner = self.ledger.positions[idx].owner;
        self.metrics.bad_debt_events.push(serde_json::json!({
            "kind": kind, "time": self.now, "tick": self.tick, "owner": self.agents[owner].name, "route": self.ledger.positions[idx].route.name(),
            "deficit_usdc": d.d as f64 / 1e6, "insurance_usdc": d.ins_paid as f64 / 1e6, "lp_absorbed_usdc": d.lp_absorbed as f64 / 1e6,
            "pending_usdc": d.pending as f64 / 1e6,
        }));
    }

    /// Invariant 4, once both legs of `idx`'s pair are fully settled (closed,
    /// liquidated, ADL'd, in any number of pieces, at any marks): what left
    /// the pair (payouts, keeper rewards, fees and penalties, less insurance's
    /// reward top-ups) equals both legs' collateral plus what they realized,
    /// less haircuts, plus the deficits the waterfall absorbed. Settled each
    /// in one piece at the same price and funding index, the legs realized
    /// exactly opposite amounts (§2.3: matched pairs cancel).
    pub async fn check_pair(&mut self, idx: usize) {
        let Some(pair) = self.ledger.positions[idx].pair else { return };
        let (t, k) = (self.ledger.pairs[pair].taker, self.ledger.pairs[pair].maker);
        let (st, sk) = (self.ledger.positions[t].settled, self.ledger.positions[k].settled);
        if !(st.done && sk.done) {
            return;
        }
        let lhs = st.paid + st.fees - st.top_up + sk.paid + sk.fees - sk.top_up;
        let rhs = st.collateral + st.realized - st.haircut + st.deficit + sk.collateral + sk.realized - sk.haircut + sk.deficit;
        let one_piece = st.pieces == 1 && sk.pieces == 1;
        let same_mark = one_piece && st.mark == sk.mark && st.cumulative_funding == sk.cumulative_funding;
        let (tick, now) = (self.tick, self.now);
        let (ta, ka) = (self.ledger.positions[t].address, self.ledger.positions[k].address);
        self.checks.check(tick, now, "4_pair_settlement", lhs == rhs && (!same_mark || st.realized + sk.realized == 0), || {
            format!("pair {} / {}: out {} != collateral + realized - haircut + deficit {}; realized {} + {} (same mark {})", ta, ka, lhs, rhs, st.realized, sk.realized, same_mark)
        });
        self.hit(if same_mark { "pair.settled_same_mark" } else { "pair.settled_different_marks" });
        if self.ledger.pairs[pair].collusion {
            // 12: closed back to back at one oracle price, the colluders took
            // nothing from the pool and nothing from insurance: the skew
            // prices of the two closes favour the pool (§2.4)
            let together = one_piece && !st.liquidated && !sk.liquidated && st.oracle == sk.oracle && st.cumulative_funding == sk.cumulative_funding;
            let no_draw = st.deficit == 0 && sk.deficit == 0 && st.insurance_draw == 0 && sk.insurance_draw == 0;
            self.checks.check(tick, now, "12_collusion_no_bad_debt", !together || (no_draw && st.realized + sk.realized <= 0), || {
                format!("colluding pair {} / {} closed at {}: realized {} + {}, deficits {} + {}", ta, ka, st.oracle, st.realized, sk.realized, st.deficit, sk.deficit)
            });
        }
    }

    // --- LP flows (§9) ------------------------------------------------------------------------

    /// lp_deposit by LP wallet `lp`, checked against the model.
    pub async fn lp_deposit(&mut self, lp: usize, amount: u64, min_shares_out: u64, label: &str) -> bool {
        let (mut st, mut pos) = (self.st().await, self.lp_position(lp).await);
        let predicted = model::lp_deposit(&mut st, &mut pos, amount, min_shares_out, &self.ov(), self.now).err();
        if !self.should_send(label, predicted) {
            return false;
        }
        let ix = lp_deposit_ix(&self.chain.base, &self.lps[lp].lp, amount, min_shares_out);
        let kp = self.lps[lp].lp.kp.insecure_clone();
        let pre = self.st().await;
        if self.run(label, &[ix], &[&kp], predicted).await.is_err() {
            return false;
        }
        self.check_lp_gates(label, LpFlow::Deposit, &pre, None);
        self.vault_expected += amount as i128;
        self.after_lp(label, lp, &st, &pos, predicted).await;
        self.hit("lp_deposit");
        true
    }

    pub async fn lp_request(&mut self, lp: usize, shares: u64, label: &str) -> bool {
        let (mut st, mut pos) = (self.st().await, self.lp_position(lp).await);
        let predicted = model::lp_request(&mut st, &mut pos, shares, &self.ov(), self.now).err();
        if !self.should_send(label, predicted) {
            return false;
        }
        let ix = lp_request_ix(&self.chain.base, &self.lps[lp].lp, shares);
        let kp = self.lps[lp].lp.kp.insecure_clone();
        let pre = self.st().await;
        if self.run(label, &[ix], &[&kp], predicted).await.is_err() {
            return false;
        }
        self.check_lp_gates(label, LpFlow::Request, &pre, None);
        self.after_lp(label, lp, &st, &pos, predicted).await;
        self.hit("lp_request");
        true
    }

    pub async fn lp_cancel(&mut self, lp: usize, label: &str) -> bool {
        let (mut st, mut pos) = (self.st().await, self.lp_position(lp).await);
        let predicted = model::lp_cancel(&mut st, &mut pos).err();
        if !self.should_send(label, predicted) {
            return false;
        }
        let b = &self.chain.base;
        let l = &self.lps[lp].lp;
        let ix = ix(perps_core::ID, perps_core::accounts::LpCancelWithdrawal { owner: l.key(), lp_pool: b.lp_pool, lp_position: l.position },
            perps_core::instruction::LpCancelWithdrawal {});
        let kp = l.kp.insecure_clone();
        if self.run(label, &[ix], &[&kp], predicted).await.is_err() {
            return false;
        }
        self.after_lp(label, lp, &st, &pos, predicted).await;
        self.hit("lp_cancel");
        true
    }

    /// lp_withdraw by `lp`: the tokens it received (None: not sent or failed).
    pub async fn lp_withdraw(&mut self, lp: usize, label: &str) -> Option<u64> {
        let (mut st, mut pos) = (self.st().await, self.lp_position(lp).await);
        let res = model::lp_withdraw(&mut st, &mut pos, &self.ov(), self.now);
        let predicted = res.as_ref().err().copied();
        if !self.should_send(label, predicted) {
            return None;
        }
        let tokens = self.lps[lp].lp.tokens;
        let wallet = token_balance(self.chain.base(), tokens).await;
        let ix = lp_withdraw_ix(&self.chain.base, &self.lps[lp].lp);
        let kp = self.lps[lp].lp.kp.insecure_clone();
        let (pre, pre_pos) = (self.st().await, self.lp_position(lp).await);
        self.run(label, &[ix], &[&kp], predicted).await.ok()?;
        self.check_lp_gates(label, LpFlow::Withdraw, &pre, Some(&pre_pos));
        let got = token_balance(self.chain.base(), tokens).await - wallet;
        self.vault_expected -= got as i128;
        let (tick, now) = (self.tick, self.now);
        if let Ok(value) = res {
            self.checks.check(tick, now, "9_settlement_accounting", got == value, || format!("{}: LP received {}, model {}", label, got, value));
        }
        self.after_lp(label, lp, &st, &pos, predicted).await;
        self.hit("lp_withdraw");
        Some(got)
    }

    /// Invariant 14: an LP flow that landed passed every gate of §9 (D17,
    /// F5, F12), recomputed here from the sim's own schedule and the
    /// accounts as they were before it: in session (or settled), funding
    /// current, and for deposits and withdrawals no pending bad debt and not
    /// inside `Q`; for withdrawals also not inside `W`, past the delay and
    /// within the claim window, and once settled past the settled quiet time (D41).
    fn check_lp_gates(&mut self, label: &str, flow: LpFlow, pre: &St, pos: Option<&LpPosition>) {
        let (now, tick) = (self.now, self.tick);
        let p = &pre.pool.params;
        let settled = pre.m.settlement_price > 0;
        let open = crate::schedule::is_open(now);
        let mut broken: Vec<String> = Vec::new();
        if !settled {
            if !open {
                broken.push("market closed".into());
            }
            let since = open_secs_between(pre.m.last_funding_time, now) + pre.m.funding_carry_secs as i64;
            if since >= pre.m.funding_interval {
                broken.push(format!("funding stale ({} open s due)", since));
            }
            let quiet = open && p.lp_reopen_quiet_secs > 0 && !crate::schedule::is_open(now - p.lp_reopen_quiet_secs as i64);
            if quiet && flow != LpFlow::Request {
                broken.push("inside Q".into());
            }
            if flow == LpFlow::Withdraw && crate::schedule::in_pre_close_window(now, p.weekend_cap_window_secs) {
                broken.push("inside W".into());
            }
        }
        // D41: a settled withdrawal waits out the longer quiet time after settled_at
        let settled_quiet = p.profit_close_quiet_secs.max(p.lp_reopen_quiet_secs) as i64;
        if settled && flow == LpFlow::Withdraw && pre.m.settled_at > 0 && now - pre.m.settled_at < settled_quiet {
            broken.push("inside the settled quiet time".into());
        }
        let pending = pre.pool.pending_bad_debt_long + pre.pool.pending_bad_debt_short;
        if pending > 0 && flow != LpFlow::Request {
            broken.push(format!("{} bad debt pending", pending));
        }
        if let (LpFlow::Withdraw, Some(pos)) = (flow, pos) {
            if now < pos.unlock_at {
                broken.push("before the delay".into());
            }
            let window = crate::pool_math::effective_claim_window_secs(pos.claim_window_secs, p.weekend_cap_window_secs, p.lp_reopen_quiet_secs);
            if !settled && window > 0 && open_secs_between(pos.unlock_at, now) as u64 > window {
                broken.push("after the claim window".into());
            }
        }
        self.checks.check(tick, now, crate::checks::LP_GATES, broken.is_empty(), || format!("{} landed at {}: {}", label, now, broken.join(", ")));
    }

    /// donate_to_pool of `amount` by agent `donor` (minted to its wallet
    /// first), checked against the model: `(recovered, vesting)`.
    pub async fn donate(&mut self, donor: usize, amount: u64, label: &str) -> Option<(u64, u64)> {
        let (mut st, now) = (self.st().await, self.now);
        let res = model::donate(&mut st, amount, now);
        let predicted = res.as_ref().err().copied();
        if !self.should_send(label, predicted) {
            return None;
        }
        let tokens = self.agents[donor].t.tokens;
        mint_to(self.chain.base(), tokens, amount).await;
        self.total_minted += amount;
        self.agents[donor].minted += amount;
        let b = &self.chain.base;
        let accounts = perps_core::accounts::DonateToPool {
            donor: self.agents[donor].t.key(), market: b.market, lp_pool: b.lp_pool, vault_token_account: b.vault_token,
            donor_token_account: tokens, token_program: spl_token::ID,
        };
        let ix = ix(perps_core::ID, accounts, perps_core::instruction::DonateToPool { amount });
        let kp = self.agents[donor].t.kp.insecure_clone();
        let landed = self.run(label, &[ix], &[&kp], predicted).await.is_ok();
        if !landed {
            // The tokens stay in the donor's wallet
            return None;
        }
        self.vault_expected += amount as i128;
        match res {
            Ok(out) => {
                self.expect_st(label, &st).await;
                self.hit("donate_to_pool");
                Some(out)
            }
            Err(_) => {
                self.resync().await;
                None
            }
        }
    }

    async fn after_lp(&mut self, label: &str, lp: usize, st: &St, pos: &LpPosition, predicted: Option<Rej>) {
        if predicted.is_some() {
            self.resync().await;
            return;
        }
        self.expect_st(label, st).await;
        let chain = self.lp_position(lp).await;
        let diffs = model::diff_lp(&chain, pos);
        let (tick, now) = (self.tick, self.now);
        self.checks.check(tick, now, "9_settlement_accounting", diffs.is_empty(), || {
            format!("{}: LpPosition of {} differs from the model: {}", label, self.lps[lp].name, diffs.join("; "))
        });
        self.pool_stats.lp_flows.push(json!({ "time": now, "tick": tick, "lp": self.lps[lp].name, "what": label, "shares": chain.shares, "pending": chain.pending_shares }));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LpFlow {
    Deposit,
    Request,
    Withdraw,
}

/// Open seconds of the CME session in `[from, to)`, from the sim's own
/// schedule: sessions open and close on whole minutes, so one sample per
/// minute is exact.
pub fn open_secs_between(from: i64, to: i64) -> i64 {
    if to <= from {
        return 0;
    }
    let mut open = 0;
    let mut t = from;
    while t < to {
        let step = (60 - t.rem_euclid(60)).min(to - t);
        if crate::schedule::is_open(t) {
            open += step;
        }
        t += step;
    }
    open
}

/// The ledger tracks every approval the sim's agents created.
fn exists_sync(ledger: &crate::ledger::Ledger, address: &Pubkey) -> bool {
    ledger.approval_cap(address).is_some()
}

pub fn opposite(side: Side) -> Side {
    model::opposite(side)
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

#[cfg(test)]
mod tests {
    use super::open_secs_between;
    use crate::support::WEDNESDAY;

    /// The LP-gate check's open-seconds count (sim schedule) agrees with
    /// perps-core's session.rs over three weeks, DST change included.
    #[test]
    fn open_seconds_match_the_engine() {
        let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for _ in 0..400 {
            let a = WEDNESDAY - 10 * 86_400 + (next() % (21 * 86_400)) as i64 / 60 * 60;
            let b = a + (next() % (4 * 86_400)) as i64 / 60 * 60;
            assert_eq!(open_secs_between(a, b), crate::session::open_seconds_between(1, a, b), "{} .. {}", a, b);
        }
    }

    /// The gate check's reopen quiet test (open now, closed Q earlier) is
    /// the engine's `seconds_since_open < Q`.
    #[test]
    fn quiet_test_matches_the_engine() {
        let q = 1_800;
        let mut t = WEDNESDAY - 7 * 86_400;
        while t < WEDNESDAY + 7 * 86_400 {
            let sim = crate::schedule::is_open(t) && !crate::schedule::is_open(t - q);
            let engine = crate::session::seconds_since_open(1, t).map_or(false, |s| s < q);
            assert_eq!(sim, engine, "at {}", t);
            t += 300;
        }
    }
}
