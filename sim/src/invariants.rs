//! Invariants checked after the keeper every tick (1, 2, 2a, 2b, 2c, 3, 5,
//! 11) and after everyone unwinds (7). 4, 6, 8, 9, 9b, 10 and 12 are checked
//! as transactions land (checks.rs, actions.rs, keeper.rs).

use crate::keeper::LiquidationFailures;
use crate::model;
use crate::pool_math as pm;
use crate::support::*;
use crate::world::Sim;
use perps_core::state::{ExecutionSource, Position, PositionStatus, Side, UserAccount, VenueApproval};
use serde_json::json;

/// Why a liquidatable position may legitimately still be open after the
/// keeper's pass: its liquidation was refused by a gate, not by the margin
/// check. Liquidations never revert on pool cash (§2.8), so
/// PoolCashInsufficient is not here.
const EXEMPT: [&str; 5] = [
    "perps_core:MarketClosed",
    "perps_core:StaleOraclePrice",
    "perps_core:OracleConfidenceTooWide",
    "perps_core:OraclePriceBelowFloor",
    "perps_core:MarketSettled",
];


/// Invariant 16: predicted-rejection skips allowed per label:rejection pair
/// and simulated day (the 2-day CI runs see at most ~6 a pair), and the floor.
pub const SKIP_LIMIT_PER_DAY: f64 = 25.0;
pub const SKIP_LIMIT_MIN: u64 = 50;


/// Invariant 2: an open position's claim on the vault at the mark: its
/// collateral plus its realized PnL, a profit cut by the solvency haircut
/// `φ = cash/U` the pool would apply when `U > cash` (§2.8), floored at 0.
pub fn open_claim(collateral: u64, r: i128, u: i128, cash: i128) -> i128 {
    let r = if r > 0 && u > 0 && u > cash { r * cash / u } else { r };
    (collateral as i128 + r).max(0)
}

impl Sim {
    /// Σ collateral_balance over every UserAccount the sim created (agents and keeper).
    pub async fn free_total(&mut self) -> i128 {
        let mut free = 0i128;
        for i in 0..self.agents.len() {
            free += self.user_of(i).await.collateral_balance as i128;
        }
        let keeper_user: UserAccount = load(self.chain.base(), self.keeper.user).await;
        free + keeper_user.collateral_balance as i128
    }

    pub async fn check_invariants(&mut self, liquidation_failures: &LiquidationFailures) {
        let (tick, now, px) = (self.tick, self.now, self.px);
        let st = self.st().await;
        let m = st.m.clone();
        let pool = st.pool.clone();

        // 11. The engine's session and weekend-margin schedule agrees with the model's
        let engine_open = perps_core::session::is_open(m.session_schedule, now);
        let engine_window = perps_core::session::in_pre_close_window(m.session_schedule, now, m.pre_close_window_secs);
        let (model_open, model_window) = (model::in_session(&m, now), crate::schedule::market_pre_close(m.session_schedule, now, m.pre_close_window_secs));
        let pool_window = perps_core::session::in_pre_close_window(m.session_schedule, now, pool.params.weekend_cap_window_secs);
        self.checks.check(tick, now, "11_session_model", engine_open == model_open && engine_window == model_window && pool_window == model::in_pool_window(&m, &pool.params, now), || {
            format!("at {}: engine open {} pre-close {} W {}, model open {} pre-close {}", now, engine_open, engine_window, pool_window, model_open, model_window)
        });

        // Re-read every open position. Only the transactions that settle,
        // slice or top one up may change it, and those update the ledger as they land.
        for idx in self.ledger.open.clone() {
            let address = self.ledger.positions[idx].address;
            let fresh: Option<Position> = try_load(self.chain.base(), address).await;
            let cached = &self.ledger.positions[idx].state;
            let same = fresh.as_ref().map_or(false, |f| {
                f.status == PositionStatus::Open && f.owner == cached.owner && f.side == cached.side && f.size == cached.size
                    && f.collateral == cached.collateral && f.entry_price == cached.entry_price && f.last_funding_payment == cached.last_funding_payment
            });
            self.checks.check(tick, now, "9_settlement_accounting", same, || {
                format!("open position {} changed outside a tracked transaction: {:?}", address, fresh.as_ref().map(|f| (f.status as u8, f.size, f.collateral)))
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
                Side::Long => long += p.size,
                Side::Short => short += p.size,
            }
        }
        self.checks.check(tick, now, "1_open_interest", long == m.long_open_interest && short == m.short_open_interest, || {
            format!("market OI long {} short {}, open positions sum to long {} short {}", m.long_open_interest, m.short_open_interest, long, short)
        });

        // 2a. The vault holds exactly the buckets (nobody sends tokens to it
        // directly here, so §11's ≥ is =), free collateral is every user's,
        // position collateral every open position's, and every bucket is the
        // model's (the shadow left by the last modelled transaction)
        let vault = vault_token_balance(self.chain.base()).await as i128;
        let buckets = model::bucket_total(&st) as i128;
        let free = self.free_total().await;
        let pos_col: i128 = self.ledger.open.iter().map(|&i| self.ledger.positions[i].state.collateral as i128).sum();
        self.checks.check(tick, now, "2a_bucket_identity", vault == buckets && free == pool.free_collateral as i128 && pos_col == pool.position_collateral as i128, || {
            format!("vault {} vs buckets {} (free {} = users {}?, position {} = positions {}?, insurance {}, cash {}, treasury {})",
                vault, buckets, pool.free_collateral, free, pool.position_collateral, pos_col, m.insurance_fund, pool.cash, pool.treasury)
        });
        if let Some(shadow) = self.shadow.clone() {
            let bucket_diff = [
                ("free_collateral", pool.free_collateral, shadow.pool.free_collateral),
                ("position_collateral", pool.position_collateral, shadow.pool.position_collateral),
                ("cash", pool.cash, shadow.pool.cash),
                ("treasury", pool.treasury, shadow.pool.treasury),
                ("insurance_fund", m.insurance_fund, shadow.m.insurance_fund),
            ];
            let bad: Vec<String> = bucket_diff.iter().filter(|(_, a, b)| a != b).map(|(n, a, b)| format!("{} {} (model {})", n, a, b)).collect();
            self.checks.check(tick, now, "2a_bucket_identity", bad.is_empty(), || format!("buckets moved without a modelled transaction: {}", bad.join(", ")));
            let mut rest = model::diff_pool(&pool, &shadow.pool);
            rest.extend(model::diff_market(&m, &shadow.m));
            self.checks.check(tick, now, "2c_pool_aggregates", rest.is_empty(), || format!("pool/market moved without a modelled transaction: {}", rest.join("; ")));
        }
        self.shadow = Some(st.clone());

        // 2c. The aggregates are the exact sums over open positions, each
        // user's house notional the sum of its open house positions'
        // opened notional, and the O(1) valuation brackets the realisable one
        let (mut les, mut ses, mut lfs, mut sfs) = (0u128, 0u128, 0i128, 0i128);
        let mut house: Vec<u64> = vec![0; self.agents.len()];
        let mut u_eng = 0i128;
        for &idx in &self.ledger.open {
            let t = &self.ledger.positions[idx];
            let p = &t.state;
            let (e, f) = (p.size as u128 * p.entry_price as u128, p.size as i128 * p.last_funding_payment as i128);
            match p.side {
                Side::Long => {
                    les += e;
                    lfs += f;
                }
                Side::Short => {
                    ses += e;
                    sfs += f;
                }
            }
            if p.execution_source == ExecutionSource::OrderBook {
                house[t.owner] += model::opened_notional(p, p.size);
            }
            u_eng += model::realized(p, px, m.cumulative_funding);
        }
        let count = self.ledger.open.len() as u64;
        self.checks.check(tick, now, "2c_pool_aggregates", (les, ses, lfs, sfs, count) == (pool.long_entry_sum, pool.short_entry_sum, pool.long_funding_sum, pool.short_funding_sum, pool.open_positions), || {
            format!("aggregates {:?}, sums over open positions {:?}", (pool.long_entry_sum, pool.short_entry_sum, pool.long_funding_sum, pool.short_funding_sum, pool.open_positions), (les, ses, lfs, sfs, count))
        });
        for i in 0..self.agents.len() {
            let held = self.user_of(i).await.house_entry_notional;
            let name = self.agents[i].name;
            self.checks.check(tick, now, "2c_pool_aggregates", held == house[i], || format!("{}: house_entry_notional {}, open house positions sum to {}", name, held, house[i]));
        }
        let mark = crate::predict::close_mark(&m, &self.ov());
        let v = model::view(&pool, &m, mark, now);
        if let Ok(v) = v {
            // U_eng at the same mark the view uses
            let u_eng = if mark == px { u_eng } else { self.ledger.open.iter().map(|&i| model::realized(&self.ledger.positions[i].state, mark, m.cumulative_funding)).sum() };
            let slack = 2 * pool.open_positions as i128 + 1;
            let real = pool.cash as i128 - u_eng;
            self.checks.check(tick, now, "2c_pool_aggregates", (u_eng - v.u).abs() <= slack && v.nav_mint >= real && real >= v.nav_redeem, || {
                format!("U {} vs Σ positions {} (slack {}); NAV_mint {} >= cash - U_eng {} >= NAV_redeem {}?", v.u, u_eng, slack, v.nav_mint, real, v.nav_redeem)
            });
            self.track_pool(&st, &v).await;
        } else {
            self.checks.check(tick, now, "2c_pool_aggregates", false, || "pool_view overflows".to_string());
        }

        // 2. Solvency, fatal by default: the vault covers free collateral
        // plus every open position's claim, a winner's profit at the
        // solvency haircut the pool would apply (φ = cash/U when U > cash),
        // unless bad debt is pending (the ADL keeper's job, §2.6)
        let v = model::view(&pool, &m, px, now).ok();
        let (u, cash) = (v.map_or(0, |v| v.u), pool.cash as i128);
        let mut owed = 0i128;
        for &idx in &self.ledger.open {
            let p = &self.ledger.positions[idx].state;
            owed += open_claim(p.collateral, model::realized(p, px, m.cumulative_funding), u, cash);
        }
        let surplus = vault - free - owed;
        let adl_required = pool.pending_bad_debt_long > 0 || pool.pending_bad_debt_short > 0;
        self.checks.check(tick, now, crate::checks::SOLVENCY, surplus >= 0 || adl_required, || {
            format!("vault {} < free collateral {} + open claims {} (short by {}), and no bad debt pending", vault, free, owed, -surplus)
        });
        if surplus < 0 {
            self.checks.insolvent_ticks += 1;
            if self.metrics.first_insolvent.is_none() {
                self.metrics.first_insolvent = Some(json!({
                    "tick": tick, "time": now, "short_usdc": -surplus as f64 / 1e6, "insurance_usdc": m.insurance_fund as f64 / 1e6,
                    "cash_usdc": pool.cash as f64 / 1e6, "u_usdc": u as f64 / 1e6, "adl_required": adl_required,
                }));
            }
        }
        if surplus < m.insurance_fund as i128 {
            self.checks.unbacked_insurance_ticks += 1;
        }
        let gap = surplus - m.insurance_fund as i128;
        self.checks.min_surplus = Some(self.checks.min_surplus.map_or(surplus, |s| s.min(surplus)));
        self.checks.min_surplus_minus_insurance = Some(self.checks.min_surplus_minus_insurance.map_or(gap, |s| s.min(gap)));

        // 2b. Every token flow in or out of the vault, as the model predicted it
        let expected = self.vault_expected;
        self.checks.check(tick, now, "2b_vault_ledger", vault == expected, || format!("vault holds {}, model predicts {}", vault, expected));

        // 3. The engine's own liquidation predicate (mark notional, §6.1), on
        // every open position: it must agree with the model, and nothing it
        // calls liquidatable may survive the keeper's pass unless a gate
        // refused the liquidation.
        let (_, engine_maintenance) = m.margin_ratios(now);
        for &idx in &self.ledger.open {
            let p = &self.ledger.positions[idx];
            let engine = p.state.is_liquidatable(px, p.state.accrued_funding(m.cumulative_funding), engine_maintenance);
            let modelled = model::liquidatable(&m, &p.state, px, now);
            let (address, ratio) = (p.address, model::margin_ratio_mark_bps(&p.state, px, m.cumulative_funding));
            self.checks.check(tick, now, "3_liquidation_predicate", engine == modelled, || {
                format!("position {} at {} bps of mark notional: engine liquidatable {}, model {} (maintenance engine {}, model {})", address, ratio, engine, modelled, engine_maintenance, model::margin_ratios(&m, now).1)
            });
            let why = liquidation_failures.get(&idx);
            let exempt = !model_open || self.settled || why.map_or(false, |w| EXEMPT.contains(&w.as_str()));
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

        self.weekly_close_record(&st).await;

        // Hourly sample
        if tick % (3_600 / self.cfg.tick_secs.max(1)).max(1) as u64 == 0 {
            self.sample(&m, vault, free, owed, surplus).await;
        }
    }

    /// Pool peaks for the report.
    async fn track_pool(&mut self, st: &model::St, v: &pm::PoolView) {
        let p = &st.pool;
        if p.lp_shares <= pm::LP_DEAD_SHARES {
            // Redeemed: only the dead shares are left
            return;
        }
        let s = &mut self.pool_stats;
        s.peak_dd_bps = s.peak_dd_bps.max(model::dd_now(p, v));
        s.week_peak_dd_bps = s.week_peak_dd_bps.max(model::dd_now(p, v));
        s.peak_net_notional = s.peak_net_notional.max(v.net_notional);
        let cap = p.params.k_net_bps as f64 / 1e4 * v.cap_nav as f64;
        if cap > 0.0 {
            s.peak_net_over_cap_nav = s.peak_net_over_cap_nav.max(v.net_notional as f64 / cap);
        }
        if v.b > 0 {
            s.peak_net_over_b = s.peak_net_over_b.max(v.net_notional as f64 / v.b as f64);
        }
        s.min_nav_redeem = Some(s.min_nav_redeem.map_or(v.nav_redeem, |x| x.min(v.nav_redeem)));
    }

    /// The weekend criterion (§17.4): net, capital and per-share NAV at the
    /// weekly close; the drawdown across the weekend once the reopen's
    /// liquidations and ADL have run.
    async fn weekly_close_record(&mut self, st: &model::St) {
        let (now, px) = (self.now, self.px);
        let weekly_close = self.at_edge && !crate::schedule::is_open(now + 3 * 3_600);
        let reopen = self.just_reopened && self.tick > 0;
        let Ok(v) = model::view(&st.pool, &st.m, px, now) else { return };
        let p = &st.pool.params;
        if weekly_close {
            let g = model::g_bps(&st.m, p) as f64;
            let b = v.b as f64;
            let alpha_cap = p.weekend_lp_loss_bps as f64 / g * b;
            // The week to this close: what happened since the last one (or the start)
            let ps = &self.pool_stats;
            let adl = |mode: &str| ps.adl_by_kind.iter().filter(|(k, _)| k.starts_with(mode)).map(|(_, n)| *n).sum::<u64>();
            let nowc = crate::metrics::WeekBase {
                liquidations: self.metrics.liquidations, adl_mode0: adl("mode0"), adl_mode1: adl("mode1"),
                haircuts: ps.haircuts.values().map(|(a, _)| *a).sum(), deficits: ps.deficit_total, beyond_insurance: ps.deficit_beyond_insurance,
                lp_flows: ps.lp_flows.len() as u64, nav_redeem: v.nav_redeem,
            };
            let first_nav = ps.series.first().and_then(|x| x["pool"]["nav_redeem_usdc"].as_f64()).map_or(nowc.nav_redeem, |n| (n * 1e6) as i128);
            let base = ps.week_base.unwrap_or(crate::metrics::WeekBase { nav_redeem: first_nav, ..Default::default() });
            let week = json!({
                "liquidations": nowc.liquidations - base.liquidations, "adl_mode0": nowc.adl_mode0 - base.adl_mode0, "adl_mode1": nowc.adl_mode1 - base.adl_mode1,
                "haircuts_usdc": (nowc.haircuts - base.haircuts) as f64 / 1e6, "deficits_usdc": (nowc.deficits - base.deficits) as f64 / 1e6,
                "deficits_beyond_insurance_usdc": (nowc.beyond_insurance - base.beyond_insurance) as f64 / 1e6, "lp_flows": nowc.lp_flows - base.lp_flows,
                "peak_drawdown_bps": ps.week_peak_dd_bps, "nav_redeem_start_usdc": base.nav_redeem as f64 / 1e6, "nav_redeem_at_close_usdc": v.nav_redeem as f64 / 1e6,
            });
            self.pool_stats.week_base = Some(nowc);
            self.pool_stats.week_peak_dd_bps = 0;
            self.pool_stats.weekly_closes.push(json!({
                "close_time": now, "in_unwind": self.unwinding, "g": g / 1e4, "net_contracts": v.net as f64 / 1e6, "net_notional_at_close_usdc": v.net_notional as f64 / 1e6,
                "b_at_close_usdc": b / 1e6, "g_net_over_b": g / 1e4 * v.net_notional as f64 / b.max(1.0),
                "carried_excess_usdc": ((v.net_notional as f64 - alpha_cap) / 1e6).max(0.0), "nps_at_close": v.nps,
                "pending_bad_debt_at_close_usdc": (st.pool.pending_bad_debt_long + st.pool.pending_bad_debt_short) as f64 / 1e6,
                "insurance_at_close_usdc": st.m.insurance_fund as f64 / 1e6,
                "deficit_beyond_insurance_before_usdc": self.pool_stats.deficit_beyond_insurance as f64 / 1e6,
                "week": week,
            }));
        } else if reopen {
            // The reopen's gap: the oracle's, or none across an unwind warp
            let gap_pct = if self.unwinding { Some(0.0) } else { self.oracle.gaps.last().filter(|g| g.0 == now).map(|g| g.1) };
            let mut verdict = None;
            if let Some(last) = self.pool_stats.weekly_closes.last_mut() {
                if last.get("reopen_time").is_none() {
                    let nps0 = last["nps_at_close"].as_u64().unwrap_or(0);
                    let dd = if nps0 > 0 && v.nps < nps0 { (nps0 - v.nps) as f64 / nps0 as f64 } else { 0.0 };
                    let b0 = last["b_at_close_usdc"].as_f64().unwrap_or(0.0) * 1e6;
                    let g_net = last["g_net_over_b"].as_f64().unwrap_or(0.0);
                    let extra = self.pool_stats.deficit_beyond_insurance as f64 - last["deficit_beyond_insurance_before_usdc"].as_f64().unwrap_or(0.0) * 1e6;
                    last["reopen_time"] = json!(now);
                    last["reopen_gap_pct"] = json!(gap_pct.map(|g| (g * 100.0).round() / 100.0));
                    last["reopen_price"] = json!(px as f64 / 1e6);
                    last["nps_at_reopen"] = json!(v.nps);
                    last["drawdown"] = json!(dd);
                    let bound = g_net + extra.max(0.0) / b0.max(1.0);
                    last["bound"] = json!(bound);
                    // §17.4: must hold for a gap of at most G (fatal, invariant 15)
                    let ok = dd <= bound + 1e-4;
                    let g = last["g"].as_f64().unwrap_or(0.0);
                    let applies = gap_pct.map_or(false, |p| p.abs() / 100.0 <= g + 1e-9);
                    last["criterion_ok"] = json!(ok);
                    last["criterion_applies"] = json!(applies);
                    last["deficits_beyond_insurance_usdc"] = json!(extra / 1e6);
                    verdict = Some((ok, applies, dd, bound, gap_pct, g));
                }
            }
            if let Some((ok, applies, dd, bound, gap, g)) = verdict {
                let tick = self.tick;
                self.checks.check(tick, now, crate::checks::WEEKEND, ok || !applies, || {
                    format!("weekend drawdown {:.4}% over the bound {:.4}% (reopen gap {:?}%, G {:.2}%)", dd * 100.0, bound * 100.0, gap, g * 100.0)
                });
            }
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
        let pool = self.pool().await;
        let v = model::view(&pool, m, self.px, self.now).ok();
        let pool_json = v.map(|v| json!({
            "cash_usdc": pool.cash as f64 / 1e6, "treasury_usdc": pool.treasury as f64 / 1e6, "u_usdc": v.u as f64 / 1e6,
            "nav_mint_usdc": v.nav_mint as f64 / 1e6, "nav_redeem_usdc": v.nav_redeem as f64 / 1e6, "b_usdc": v.b as f64 / 1e6,
            "cap_nav_usdc": v.cap_nav as f64 / 1e6, "nps": v.nps, "drawdown_bps": model::dd_now(&pool, &v),
            "net_contracts": v.net as f64 / 1e6, "net_notional_usdc": v.net_notional as f64 / 1e6,
            "net_over_cap": if v.cap_nav > 0 { v.net_notional as f64 * 1e4 / (pool.params.k_net_bps as f64 * v.cap_nav as f64) } else { 0.0 },
            "pending_bad_debt_usdc": (pool.pending_bad_debt_long + pool.pending_bad_debt_short) as f64 / 1e6,
        }));
        if let Some(p) = pool_json.clone() {
            self.pool_stats.series.push(json!({ "time": self.now, "pool": p }));
        }
        self.metrics.insurance_series.push((self.now, m.insurance_fund));
        self.metrics.samples.push(json!({
            "time": self.now, "tick": self.tick, "oracle": self.px as f64 / 1e6, "in_session": self.in_session(),
            "insurance_usdc": m.insurance_fund as f64 / 1e6, "vault_usdc": vault as f64 / 1e6, "free_usdc": free as f64 / 1e6,
            "open_claims_usdc": owed as f64 / 1e6, "surplus_usdc": surplus as f64 / 1e6,
            "long_oi": m.long_open_interest as f64 / 1e6, "short_oi": m.short_open_interest as f64 / 1e6,
            "open_positions": self.ledger.open.len(), "cumulative_funding": m.cumulative_funding, "market_makers": mms,
            "pool": pool_json,
        }));
    }

    /// Invariant 7 after everyone has closed, cancelled and withdrawn, and
    /// every LP has redeemed every share but the dead ones: tokens are
    /// conserved, nothing is open, no debt is pending, and the vault holds
    /// exactly insurance, treasury and the dead shares' cash.
    /// Invariant 16 (coverage): in a run of at least two days every required
    /// path of the scenario is reached, and no label:rejection pair was kept
    /// from the chain by a predicted rejection more than
    /// `SKIP_LIMIT_PER_DAY` times a day (min `SKIP_LIMIT_MIN`): a chain or
    /// model regression that predicts a whole class of transactions unsendable
    /// otherwise shows up only as a skip count.
    pub fn check_coverage(&mut self) {
        let (tick, now) = (self.tick, self.now);
        let multi = self.ledger.orders.iter().filter(|o| o.fills > 1).count() as u64;
        if multi > 0 {
            self.metrics.paths.insert("order.fills>1".into(), multi);
        }
        if self.cfg.days >= 2.0 {
            let missing: Vec<&str> = self.cfg.scenario.required_paths().iter().copied().filter(|p| !self.metrics.paths.contains_key(*p)).collect();
            self.checks.check(tick, now, crate::checks::COVERAGE, missing.is_empty(), || format!("required paths not reached: {}", missing.join(", ")));
        }
        let limit = ((SKIP_LIMIT_PER_DAY * self.cfg.days).ceil() as u64).max(SKIP_LIMIT_MIN);
        let over: Vec<String> = self.metrics.skips.iter().filter(|(_, &n)| n > limit).map(|(k, n)| format!("{} x{}", k, n)).collect();
        self.checks.check(tick, now, crate::checks::COVERAGE, over.is_empty(), || {
            format!("transactions not sent (predicted rejection) over {} per label: {}", limit, over.join(", "))
        });
    }

    pub async fn check_final(&mut self) {
        let (tick, now) = (self.tick, self.now);
        let st = self.st().await;
        let (m, pool) = (&st.m, &st.pool);
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
        let mut lp_left = Vec::new();
        for l in 0..self.lps.len() {
            let tokens = self.lps[l].lp.tokens;
            wallets += token_balance(self.chain.base(), tokens).await as u128;
            let pos = self.lp_position(l).await;
            if pos.shares > 0 {
                lp_left.push(format!("{} holds {} shares ({} pending)", self.lps[l].name, pos.shares, pos.pending_shares));
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
        let flat = still_open == 0 && m.long_open_interest == 0 && m.short_open_interest == 0 && pool.open_positions == 0 && pool.position_collateral == 0;
        self.checks.check(tick, now, "7_final_conservation", flat, || {
            format!("not fully unwound: {} positions open, OI {}/{}, pool counts {}, position collateral {}", still_open, m.long_open_interest, m.short_open_interest, pool.open_positions, pool.position_collateral)
        });
        let adl_required = pool.pending_bad_debt_long > 0 || pool.pending_bad_debt_short > 0;
        self.checks.check(tick, now, "7_final_conservation", !adl_required, || {
            format!("bad debt still pending after the unwind: long {} short {}", pool.pending_bad_debt_long, pool.pending_bad_debt_short)
        });
        self.checks.check(tick, now, "7_final_conservation", lp_left.is_empty() && pool.lp_shares == pm::LP_DEAD_SHARES && pool.pending_withdraw_shares == 0, || {
            format!("LP shares not withdrawn: {} (lp_shares {}, dead {})", lp_left.join(", "), pool.lp_shares, pm::LP_DEAD_SHARES)
        });
        self.checks.check(tick, now, "7_final_conservation", leftover_free == 0 && pool.free_collateral == 0, || {
            format!("free collateral left: users {}, pool bucket {}", leftover_free, pool.free_collateral)
        });
        // What stays in the vault: insurance, treasury and LP cash the dead shares own
        let kept = m.insurance_fund as u128 + pool.treasury as u128 + pool.cash as u128;
        self.checks.check(tick, now, "7_final_conservation", vault as u128 == kept, || {
            format!("after unwind: vault {} != insurance {} + treasury {} + cash {}", vault, m.insurance_fund, pool.treasury, pool.cash)
        });
        // Solvency, realised: nothing owed is left unpaid
        self.checks.check(tick, now, crate::checks::SOLVENCY, leftover_free == 0 && vault as u128 >= kept, || {
            format!("after unwind: vault {} vs insurance + treasury + cash {}, {} free collateral could not be withdrawn", vault, kept, leftover_free)
        });
        self.metrics.final_vault = vault;
        self.metrics.final_insurance = m.insurance_fund;
        self.metrics.stuck_free = leftover_free;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The φ branch, which no 2-day CI run reaches on SBF (U never exceeds
    /// cash there): the claim is exactly collateral + what the engine's own
    /// quote would pay with nothing pending, so a vault short of it fails.
    #[test]
    fn open_claim_takes_the_engines_solvency_haircut() {
        let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = || {
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            x.wrapping_mul(0x2545_f491_4f6c_dd1d)
        };
        let mut haircut = 0;
        for _ in 0..20_000 {
            let cash = (next() % 2_000_000_000_000) as i128;
            let u = (next() % 4_000_000_000_000) as i128 - 1_000_000_000_000;
            let p = next() % 500_000_000_000;
            let c = next() % 100_000_000_000;
            let q = pm::quote_profit_raw(u, cash as u64, 0, p, true).unwrap();
            assert_eq!(open_claim(c, p as i128, u, cash), c as i128 + q.paid as i128 + q.cash_hc as i128, "u {} cash {} p {}", u, cash, p);
            haircut += (q.solvency_hc > 0) as u32;
            // Losses are not haircut, and the claim never goes below 0
            assert_eq!(open_claim(c, -(p as i128), u, cash), (c as i128 - p as i128).max(0));
        }
        assert!(haircut > 1_000, "the φ branch was reached {} times", haircut);
        // Worked case: U = $300 against $100 of cash pays a third of a $90 profit
        assert_eq!(open_claim(50, 90, 300, 100), 50 + 30);
        assert_eq!(open_claim(50, 90, 100, 300), 50 + 90);
    }
}
