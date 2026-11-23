//! Invariants checked after the keeper every tick (1, 2, 2b, 3, 5, 11) and
//! after everyone unwinds (7). 4, 6, 8, 9, 9b, 10 and 12 are checked as
//! transactions land (checks.rs, actions.rs, keeper.rs).

use crate::keeper::LiquidationFailures;
use crate::model;
use crate::support::*;
use crate::world::Sim;
use perps_core::state::{Position, PositionStatus, UserAccount, VenueApproval};
use serde_json::json;

/// Why a liquidatable position may legitimately still be open after the
/// keeper's pass: its liquidation was refused by a gate, not by the margin check.
const EXEMPT: [&str; 5] = [
    "perps_core:MarketClosed",
    "perps_core:StaleOraclePrice",
    "perps_core:OracleConfidenceTooWide",
    "perps_core:OraclePriceBelowFloor",
    "perps_core:MarketSettled",
];

impl Sim {
    pub async fn check_invariants(&mut self, liquidation_failures: &LiquidationFailures) {
        let (tick, now, px) = (self.tick, self.now, self.px);
        let m = self.market().await;

        // 11. The engine's session and weekend-margin schedule agrees with the model's
        let engine_open = perps_core::session::is_open(m.session_schedule, now);
        let engine_window = perps_core::session::in_pre_close_window(m.session_schedule, now, m.pre_close_window_secs);
        let (model_open, model_window) = (model::in_session(&m, now), crate::schedule::market_pre_close(m.session_schedule, now, m.pre_close_window_secs));
        self.checks.check(tick, now, "11_session_model", engine_open == model_open && engine_window == model_window, || {
            format!("at {}: engine open {} pre-close {}, model open {} pre-close {}", now, engine_open, engine_window, model_open, model_window)
        });

        // Re-read every open position. Only close/liquidate/add_margin may
        // change one, and those update the ledger as they land.
        for idx in self.ledger.open.clone() {
            let address = self.ledger.positions[idx].address;
            let fresh: Option<Position> = try_load(self.chain.base(), address).await;
            let cached = &self.ledger.positions[idx].state;
            let same = fresh.as_ref().map_or(false, |f| {
                f.status == PositionStatus::Open && f.owner == cached.owner && f.side == cached.side && f.size == cached.size
                    && f.collateral == cached.collateral && f.entry_price == cached.entry_price && f.last_funding_payment == cached.last_funding_payment
            });
            self.checks.check(tick, now, "9_settlement_accounting", same, || {
                format!("open position {} changed outside its own close/liquidation/add_margin: {:?}", address, fresh.as_ref().map(|f| (f.status as u8, f.size, f.collateral)))
            });
            if let Some(f) = fresh {
                self.ledger.update(idx, f);
            }
        }

        // 1. Open interest is the sum over open positions
        let (mut long, mut short) = (0u64, 0u64);
        for &idx in &self.ledger.open {
            let p = &self.ledger.positions[idx].state;
            match p.side {
                perps_core::state::Side::Long => long += p.size,
                perps_core::state::Side::Short => short += p.size,
            }
        }
        self.checks.check(tick, now, "1_open_interest", long == m.long_open_interest && short == m.short_open_interest, || {
            format!("market OI long {} short {}, open positions sum to long {} short {}", m.long_open_interest, m.short_open_interest, long, short)
        });

        // 2. Solvency at the mark (non-fatal unless --strict-solvency: see checks::SOLVENCY)
        let vault = vault_token_balance(self.chain.base()).await as i128;
        let mut free = 0i128;
        for i in 0..self.agents.len() {
            free += self.user_of(i).await.collateral_balance as i128;
        }
        let keeper_user: UserAccount = load(self.chain.base(), self.keeper.user).await;
        free += keeper_user.collateral_balance as i128;
        let owed: i128 = self.ledger.open.iter().map(|&idx| model::equity(&self.ledger.positions[idx].state, px, m.cumulative_funding).max(0)).sum();
        let surplus = vault - free - owed;
        self.checks.check(tick, now, crate::checks::SOLVENCY, surplus >= 0, || {
            format!("vault {} < free collateral {} + open equity {} (short by {})", vault, free, owed, -surplus)
        });
        if surplus < 0 {
            self.checks.insolvent_ticks += 1;
        }
        if surplus < m.insurance_fund as i128 {
            self.checks.unbacked_insurance_ticks += 1;
        }
        let gap = surplus - m.insurance_fund as i128;
        self.checks.min_surplus = Some(self.checks.min_surplus.map_or(surplus, |s| s.min(surplus)));
        self.checks.min_surplus_minus_insurance = Some(self.checks.min_surplus_minus_insurance.map_or(gap, |s| s.min(gap)));

        let (net, pnl, count) = self.ledger.vault_exposure(px, m.cumulative_funding);
        self.metrics.peak_unpaired_net = self.metrics.peak_unpaired_net.max(net.abs());
        self.metrics.peak_unpaired_pnl = self.metrics.peak_unpaired_pnl.max(pnl);
        self.metrics.peak_unpaired_count = self.metrics.peak_unpaired_count.max(count);
        if surplus < 0 && self.metrics.first_insolvent.is_none() {
            self.metrics.first_insolvent = Some(json!({
                "tick": tick, "time": now, "short_usdc": -surplus as f64 / 1e6, "insurance_usdc": m.insurance_fund as f64 / 1e6,
                "unpaired_positions": count, "unpaired_net_contracts": net as f64 / 1e6, "vault_owes_unpaired_usdc": pnl as f64 / 1e6,
            }));
        }

        // 2b. Every token flow in or out of the vault, as the model predicted it
        let expected = self.vault_expected;
        self.checks.check(tick, now, "2b_vault_ledger", vault == expected, || format!("vault holds {}, model predicts {}", vault, expected));

        // 3. The engine's own liquidation predicate, on every open position:
        // it must agree with the model, and nothing it calls liquidatable may
        // survive the keeper's pass unless a gate refused the liquidation.
        let (_, engine_maintenance) = m.margin_ratios(now);
        for &idx in &self.ledger.open {
            let p = &self.ledger.positions[idx];
            let engine = p.state.is_liquidatable(px, p.state.accrued_funding(m.cumulative_funding), engine_maintenance);
            let modelled = model::liquidatable(&m, &p.state, px, now);
            let (address, ratio) = (p.address, model::margin_ratio_bps(&p.state, px, m.cumulative_funding));
            self.checks.check(tick, now, "3_liquidation_predicate", engine == modelled, || {
                format!("position {} at {} bps: engine liquidatable {}, model {} (maintenance engine {}, model {})", address, ratio, engine, modelled, engine_maintenance, model::margin_ratios(&m, now).1)
            });
            let why = liquidation_failures.get(&idx);
            let exempt = !model_open || why.map_or(false, |w| EXEMPT.contains(&w.as_str()));
            self.checks.check(tick, now, "3_no_unliquidated_positions", !engine || exempt, || {
                format!("position {} at {} bps (maintenance {}) is liquidatable and still open; keeper: {:?}", address, ratio, engine_maintenance, why)
            });
        }

        // 5. An approval holds exactly its owner's last grant minus every fill's margin since
        for a in 0..self.ledger.approvals.len() {
            let (address, cap, exists) = (self.ledger.approvals[a].address, self.ledger.approvals[a].cap, self.ledger.approvals[a].exists);
            let approval = try_load::<VenueApproval>(self.chain.base(), address).await;
            let ok = match (&approval, exists) {
                (Some(x), true) => x.allowance == cap,
                (None, false) => true,
                _ => false,
            };
            self.checks.check(tick, now, "5_allowance_tracked", ok, || {
                format!("approval {} holds {:?} (exists {}), but its owner's grants minus fills since come to {} (exists {})",
                    address, approval.as_ref().map(|x| x.allowance), approval.is_some(), cap, exists)
            });
            if let Some(x) = approval {
                if x.allowance > cap && exists {
                    self.hit("violation.allowance_increase");
                }
            }
        }

        // Hourly sample
        if tick % (3_600 / self.cfg.tick_secs.max(1)).max(1) as u64 == 0 {
            self.sample(&m, vault, free, owed, surplus).await;
        }
    }

    async fn sample(&mut self, m: &perps_core::state::Market, vault: i128, free: i128, owed: i128, surplus: i128) {
        let mut mms = Vec::new();
        for i in 0..self.agents.len() {
            if self.agents[i].persona != crate::agents::Persona::MarketMaker {
                continue;
            }
            let (net, gross, locked) = self.inventory(i);
            let free = self.user_of(i).await.collateral_balance;
            let (im, _) = model::margin_ratios(m, self.now);
            // Margin a netting engine would need for the same exposure
            let net_margin = (net.unsigned_abs() as f64 / 1e6) * (self.px as f64 / 1e6) * im as f64 / 10_000.0;
            let book = self.agents[i].mm.as_mut().unwrap();
            book.econ.peak_gross_contracts = book.econ.peak_gross_contracts.max(gross as f64 / 1e6);
            book.econ.peak_locked = book.econ.peak_locked.max(locked);
            book.econ.locked_sum += locked as f64 / 1e6;
            book.econ.net_margin_sum += net_margin;
            book.econ.gross_notional_sum += gross as f64 / 1e6 * self.px as f64 / 1e6;
            book.econ.samples += 1;
            mms.push(json!({
                "name": self.agents[i].name, "net_contracts": net as f64 / 1e6, "gross_contracts": gross as f64 / 1e6,
                "locked_usdc": locked as f64 / 1e6, "free_usdc": free as f64 / 1e6, "net_only_margin_usdc": net_margin,
            }));
        }
        let unpaired = self.ledger.vault_exposure(self.px, m.cumulative_funding);
        self.metrics.insurance_series.push((self.now, m.insurance_fund));
        self.metrics.samples.push(json!({
            "time": self.now, "tick": self.tick, "oracle": self.px as f64 / 1e6, "in_session": self.in_session(),
            "insurance_usdc": m.insurance_fund as f64 / 1e6, "vault_usdc": vault as f64 / 1e6, "free_usdc": free as f64 / 1e6,
            "open_equity_usdc": owed as f64 / 1e6, "surplus_usdc": surplus as f64 / 1e6,
            "long_oi": m.long_open_interest as f64 / 1e6, "short_oi": m.short_open_interest as f64 / 1e6,
            "open_positions": self.ledger.open.len(), "cumulative_funding": m.cumulative_funding, "market_makers": mms,
            "vault_counterparty": { "positions": unpaired.2, "net_contracts": unpaired.0 as f64 / 1e6, "vault_owes_usdc": unpaired.1 as f64 / 1e6 },
        }));
    }

    /// Invariant 7 after everyone has closed, cancelled and withdrawn; the
    /// vault's leftover against the insurance fund is the realised side of
    /// solvency (invariant 2).
    pub async fn check_final(&mut self) {
        let (tick, now) = (self.tick, self.now);
        let m = self.market().await;
        let vault = vault_token_balance(self.chain.base()).await;
        let mut wallets = token_balance(self.chain.base(), self.keeper.tokens).await as u128;
        let mut leftover_free = 0u64;
        for i in 0..self.agents.len() {
            wallets += self.wallet(i).await as u128;
            leftover_free += self.user_of(i).await.collateral_balance;
            if let Some(book) = self.agents[i].mm.as_ref() {
                // A bond that was not returned still exists as a token account
                let bond = book.bond;
                if exists(self.chain.base(), bond).await {
                    wallets += token_balance(self.chain.base(), bond).await as u128;
                }
            }
        }
        let mint_address = self.chain.base.mint;
        let mint = raw(self.chain.base(), mint_address).await.expect("mint");
        let supply = spl_token::state::Mint::unpack(&mint.data).unwrap().supply;
        let minted = self.total_minted;
        let conserved = wallets + vault as u128 == supply as u128 && supply == minted;
        self.checks.check(tick, now, "7_final_conservation", conserved, || {
            format!("wallets {} + vault {} != minted {} (supply {})", wallets, vault, minted, supply)
        });
        let still_open = self.ledger.open.len();
        let flat = still_open == 0 && m.long_open_interest == 0 && m.short_open_interest == 0;
        self.checks.check(tick, now, "7_final_conservation", flat, || {
            format!("not fully unwound: {} positions open, OI {}/{}", still_open, m.long_open_interest, m.short_open_interest)
        });
        // Free collateral left behind can only be a withdraw the vault could not pay
        let diff = vault as i128 - m.insurance_fund as i128;
        self.checks.check(tick, now, crate::checks::SOLVENCY, diff >= 0 && leftover_free == 0, || {
            format!("after unwind: vault {} vs insurance fund {} (unbacked by {}), {} free collateral could not be withdrawn", vault, m.insurance_fund, (-diff).max(0), leftover_free)
        });
        self.metrics.final_vault = vault;
        self.metrics.final_insurance = m.insurance_fund;
        self.metrics.stuck_free = leftover_free;
    }
}
