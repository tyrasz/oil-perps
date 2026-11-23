//! The run report: a stdout summary and a JSON document.

use crate::agents::Persona;
use crate::checks::Class;
use crate::world::Sim;
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub struct RunInfo {
    pub mode: String,
    pub wall_secs: f64,
    pub start_time: i64,
    pub ticks: u64,
    pub price_min: u64,
    pub price_max: u64,
}

fn usdc(x: impl Into<i128>) -> f64 {
    x.into() as f64 / 1e6
}

impl Sim {
    pub async fn report(&mut self, info: &RunInfo) -> Value {
        let mut agents = Vec::new();
        let mut personas: BTreeMap<Persona, (i128, usize)> = BTreeMap::new();
        for i in 0..self.agents.len() {
            let tokens = self.wallet(i).await as i128;
            let stuck = self.user_of(i).await.collateral_balance;
            let a = &self.agents[i];
            let pnl = tokens - a.minted as i128;
            let e = personas.entry(a.persona).or_default();
            e.0 += pnl;
            e.1 += 1;
            agents.push(json!({
                "name": a.name, "persona": a.persona.name(), "deposit_usdc": usdc(a.deposit), "minted_usdc": usdc(a.minted),
                "final_tokens_usdc": usdc(tokens), "stuck_in_perps_usdc": usdc(stuck), "pnl_usdc": usdc(pnl), "pnl_pct": pnl as f64 / a.minted as f64 * 100.0,
                "opens": a.stats.opens, "maker_fills": a.stats.maker_fills, "closes": a.stats.closes,
                "liquidated": a.stats.liquidated, "close_fees_usdc": usdc(a.stats.fees_paid), "orders": a.stats.orders,
            }));
        }
        let keeper_tokens = crate::support::token_balance(self.chain.base(), self.keeper.tokens).await;
        let mut lps = Vec::new();
        for l in 0..self.lps.len() {
            let tokens = crate::support::token_balance(self.chain.base(), self.lps[l].lp.tokens).await as i128;
            let pos = self.lp_position(l).await;
            let w = &self.lps[l];
            lps.push(json!({
                "name": w.name, "persona": w.persona.name(), "minted_usdc": usdc(w.minted), "final_tokens_usdc": usdc(tokens), "pnl_usdc": usdc(tokens - w.minted as i128),
                "deposited_usdc": usdc(pos.deposited_cum), "withdrawn_usdc": usdc(pos.withdrawn_cum), "shares_left": pos.shares,
                "own_flows": w.tries,
            }));
        }
        let persona_json: Vec<Value> = personas
            .iter()
            .map(|(p, (pnl, n))| json!({ "persona": p.name(), "agents": n, "pnl_usdc": usdc(*pnl) }))
            .collect();

        let mut mms = Vec::new();
        for i in 0..self.agents.len() {
            let a = &self.agents[i];
            let Some(b) = a.mm.as_ref() else { continue };
            let e = &b.econ;
            let pnl = agents[i]["pnl_usdc"].as_f64().unwrap_or(0.0);
            mms.push(json!({
                "name": a.name, "half_spread_bps": a.s.half_spread_bps, "quote_contracts": a.s.quote_contracts,
                "fills": e.fills, "volume_usdc": e.volume as f64 / 1e6,
                "spread_captured_usdc": usdc(e.spread_captured), "close_fees_usdc": usdc(e.close_fees),
                "spread_minus_close_fees_usdc": usdc(e.spread_captured - e.close_fees as i128),
                "spread_captured_bps_of_volume": if e.volume > 0 { e.spread_captured as f64 / e.volume as f64 * 1e4 } else { 0.0 },
                "close_fees_bps_of_volume": if e.volume > 0 { e.close_fees as f64 / e.volume as f64 * 1e4 } else { 0.0 },
                "pnl_usdc": pnl, "pair_closes": e.pair_closes, "inventory_closes": e.inventory_closes,
                "allowance_refills": e.allowance_refills, "quote_reposts": e.reposts,
                "peak_gross_contracts": e.peak_gross_contracts, "peak_locked_usdc": usdc(e.peak_locked),
                "avg_locked_usdc": e.locked_sum / e.samples.max(1) as f64,
                "avg_net_only_margin_usdc": e.net_margin_sum / e.samples.max(1) as f64,
                "avg_gross_notional_usdc": e.gross_notional_sum / e.samples.max(1) as f64,
                "avg_locked_vs_net_only_margin": if e.net_margin_sum > 0.0 { e.locked_sum / e.net_margin_sum } else { 0.0 },
            }));
        }

        let errors: Vec<Value> = self
            .checks
            .errors
            .iter()
            .map(|((label, key), e)| json!({
                "instruction": label, "error": key, "class": if e.class == Some(Class::Expected) { "expected" } else { "unexpected" },
                "count": e.count, "first_tick": e.first_tick, "example": e.example,
            }))
            .collect();
        let invariants: serde_json::Map<String, Value> = self
            .checks
            .results
            .iter()
            .map(|(k, (n, f))| (k.to_string(), json!({ "checks": n, "failures": f, "ok": *f == 0 })))
            .collect();
        // Fatal violations first; solvency findings after, capped
        let fatal_first = self.checks.violations.iter().filter(|v| self.checks.fatal(v.invariant)).take(150);
        let findings = self.checks.violations.iter().filter(|v| !self.checks.fatal(v.invariant)).take(50);
        let violations: Vec<Value> = fatal_first
            .chain(findings)
            .map(|v| json!({ "tick": v.tick, "time": v.time, "invariant": v.invariant, "detail": v.detail }))
            .collect();
        let compute: serde_json::Map<String, Value> = self
            .chain
            .compute
            .iter()
            .filter(|(_, c)| c.count > 0)
            .map(|(k, c)| (k.clone(), json!({ "count": c.count, "mean": c.total / c.count, "max": c.max })))
            .collect();
        // Coverage: fills per order and per quote, engine paths reached, and what each scenario is for
        let mut order_hist: BTreeMap<String, u64> = BTreeMap::new();
        let mut order_ends: BTreeMap<String, u64> = BTreeMap::new();
        let mut capped = 0u64;
        for o in &self.ledger.orders {
            *order_hist.entry(format!("{}", o.fills)).or_default() += 1;
            *order_ends.entry(o.end.map_or("open_at_end".to_string(), |e| format!("{:?}", e))).or_default() += 1;
            capped += o.capped_fills as u64;
        }
        let mut quote_hist: BTreeMap<String, u64> = BTreeMap::new();
        for (_, n) in &self.ledger.quote_fills {
            *quote_hist.entry(format!("{}", n)).or_default() += 1;
        }
        let missing: Vec<String> = self.cfg.scenario.required_paths().iter().copied().filter(|p| !self.metrics.paths.contains_key(*p)).map(String::from).collect();
        let fatal = self.checks.fatal_count();
        let solvency_failures = self.checks.results.get(crate::checks::SOLVENCY).map_or(0, |r| r.1);
        let pool = self.pool_report().await;
        let m = &self.metrics;
        json!({
            "config": { "scenario": self.cfg.scenario.name(), "strict_solvency": self.cfg.strict_solvency, "seed": self.cfg.seed, "days": self.cfg.days, "tick_secs": self.cfg.tick_secs, "mode": info.mode, "start_time": info.start_time, "end_time": self.now, "ticks": info.ticks },
            "runtime": { "wall_secs": info.wall_secs, "transactions": self.chain.transactions, "failed_transactions": self.chain.failed_transactions, "account_in_use_retries": self.chain.lock_retries },
            "oracle": {
                "start": 75.0, "end": self.px as f64 / 1e6, "min": info.price_min as f64 / 1e6, "max": info.price_max as f64 / 1e6,
                "jumps": self.oracle.jumps.iter().map(|(t, j)| json!({ "time": t, "move_pct": j })).collect::<Vec<_>>(), "outages": self.oracle.outages, "wide_confidence": self.oracle.wide_conf,
                "gaps": self.oracle.gaps.iter().map(|(t, g)| json!({ "time": t, "move_pct": g })).collect::<Vec<_>>(),
                "adversary_moves": self.oracle.shocks.iter().map(|(t, g)| json!({ "time": t, "move_pct": g })).collect::<Vec<_>>(),
            },
            "volume": m.volume_json(),
            "orderbook": { "orders_placed": m.orders_placed, "orders_cancelled": m.orders_cancelled, "expired_closed": m.expired_orders_closed, "matches": m.matches, "match_skips": m.match_skips },
            "quotes": { "posts": m.quote_posts, "updates": m.quote_updates, "taker_found_no_quote": m.no_liquidity },
            "closes": { "count": m.closes, "fees_usdc": usdc(m.close_fees), "deficit_usdc": usdc(m.close_bad_debt) },
            "liquidations": {
                "count": m.liquidations, "by_route": m.liquidations_by_route.iter().map(|(r, n)| (r.name().to_string(), json!(n))).collect::<serde_json::Map<_, _>>(),
                "rewards_usdc": usdc(m.liquidation_rewards), "keeper_tokens_usdc": usdc(keeper_tokens), "failed_attempts": m.liquidation_failures,
                "in_pre_close_window": m.liquidation_events.iter().filter(|e| e["pre_close_window"].as_bool() == Some(true)).count(),
                "events": m.liquidation_events,
            },
            "bad_debt": { "events": m.bad_debt_events, "uncovered_usdc": usdc(m.uncovered_bad_debt) },
            "funding": {
                "updates": m.funding_updates.len(), "periods": m.funding_updates.iter().map(|f| f["periods"].as_u64().unwrap_or(0)).sum::<u64>(),
                "periods_dropped": m.funding_periods_dropped, "pool_carry_from_keeper_cranks_usdc": usdc(m.funding_imbalance), "events": m.funding_updates,
            },
            "insurance": {
                "final_usdc": usdc(m.final_insurance), "final_vault_usdc": usdc(m.final_vault),
                "vault_minus_insurance_usdc": usdc(m.final_vault as i128 - m.final_insurance as i128),
                "hourly": m.insurance_series.iter().map(|(t, f)| json!([t, *f as f64 / 1e6])).collect::<Vec<_>>(),
            },
            "solvency": {
                "rule": "vault - free collateral - every open position's claim at the solvency haircut >= 0, or bad debt pending (docs/lp-pool.md §16.7); fatal unless --lenient-solvency",
                "failed_checks": solvency_failures, "insolvent_ticks": self.checks.insolvent_ticks,
                "stuck_free_collateral_usdc": usdc(m.stuck_free),
                "min_surplus_usdc": self.checks.min_surplus.map(usdc), "min_surplus_minus_insurance_usdc": self.checks.min_surplus_minus_insurance.map(usdc),
                "ticks_insurance_not_backed": self.checks.unbacked_insurance_ticks,
                "first_insolvent_tick": m.first_insolvent,
            },
            "pool": pool,
            "probes": self.metrics.probes.iter().map(|(k, p)| (k.clone(), json!({
                "checked": p.checked, "ok": p.ok, "not_reached": p.not_reached, "last": p.last,
            }))).collect::<serde_json::Map<_, _>>(),
            "agents": agents,
            "lps": lps,
            "personas": persona_json,
            "market_makers": mms,
            "leverage_clamped_entries": m.leverage_clamps,
            "unwind_failures": m.unwind_failures,
            "errors": errors,
            "invariants": invariants,
            "violations": violations,
            "violation_count": self.checks.violations.len(),
            "fatal_violation_count": fatal,
            "coverage": {
                "engine_paths": m.paths, "skipped_by_prediction": m.skips, "missing_required_paths": missing,
                "order_fill_histogram": order_hist, "order_ends": order_ends, "capped_fills": capped,
                "quote_fill_histogram": quote_hist, "funding_periods_dropped": m.funding_periods_dropped,
            },
            "compute_units": compute,
            "samples": m.samples,
        })
    }
}

impl Sim {
    /// The report's pool section (docs/lp-pool.md §16.7): NAV path,
    /// drawdown, capital use, haircuts, ADL, funding drops and the weekend
    /// criterion per weekly close.
    async fn pool_report(&mut self) -> Value {
        let st = self.st().await;
        let v = crate::model::view(&st.pool, &st.m, crate::predict::close_mark(&st.m, &self.ov()), self.now).ok();
        let p = &self.pool_stats;
        let first = p.series.first().map(|s| s["pool"].clone()).unwrap_or(Value::Null);
        json!({
            "params": format!("{:?}", st.pool.params),
            "nav_redeem_start_usdc": first["nav_redeem_usdc"], "nps_start": first["nps"],
            "nav_redeem_end_usdc": p.before_redeem.as_ref().map(|b| usdc(b["nav_redeem"].as_i64().unwrap_or(0))),
            "nps_end": p.before_redeem.as_ref().map(|b| b["nps"].clone()),
            "after_redemption": { "nav_redeem_usdc": v.map(|v| usdc(v.nav_redeem)), "cash_usdc": usdc(st.pool.cash), "lp_shares": st.pool.lp_shares },
            "cash_end_usdc": usdc(st.pool.cash),
            "treasury_usdc": usdc(st.pool.treasury), "lp_shares_end": st.pool.lp_shares,
            "min_nav_redeem_usdc": p.min_nav_redeem.map(usdc), "peak_drawdown_bps": p.peak_dd_bps,
            "peak_net_notional_usdc": usdc(p.peak_net_notional as i128), "peak_net_over_cap_nav": p.peak_net_over_cap_nav, "peak_net_over_b": p.peak_net_over_b,
            "house_opens": p.house_opens, "open_fees_usdc": usdc(p.open_fees),
            "haircuts": p.haircuts.iter().map(|(k, (a, n))| (k.clone(), json!({ "usdc": usdc(*a), "count": n }))).collect::<serde_json::Map<_, _>>(),
            "pool_cash_insufficient": p.cash_insufficient,
            "liquidation_stages": p.liquidation_stages,
            "settle_pnl": { "count": p.settle_pnl.0, "usdc": usdc(p.settle_pnl.1) },
            "deficits": {
                "total_usdc": usdc(p.deficit_total), "beyond_insurance_usdc": usdc(p.deficit_beyond_insurance),
                "lp_absorbed_usdc": usdc(p.deficit_lp_absorbed), "pending_usdc": usdc(p.deficit_pending), "events": p.deficits,
            },
            "bad_debt_cum_usdc": usdc(st.pool.bad_debt_cum), "bad_debt_recovered_usdc": usdc(st.pool.bad_debt_recovered_cum),
            "written_off_usdc": usdc(p.written_off), "swept_usdc": usdc(p.swept),
            "adl": { "by_kind": p.adl_by_kind, "events": p.adl_events },
            "funding_periods_dropped": self.metrics.funding_periods_dropped,
            "weekly_closes": p.weekly_closes,
            "lp_flows": p.lp_flows.len(),
            "lp_flow_events": p.lp_flows,
            "hourly": p.series,
        })
    }
}

/// Human summary of the JSON report.
pub fn print_summary(r: &Value) {
    let f = |v: &Value| v.as_f64().unwrap_or(0.0);
    println!();
    println!("=== OIL perps simulation: {} scenario | seed {} | {} days | {} s ticks | {} ===", r["config"]["scenario"].as_str().unwrap_or(""), r["config"]["seed"], r["config"]["days"], r["config"]["tick_secs"], r["config"]["mode"].as_str().unwrap_or(""));
    println!("wall {:.1}s, {} transactions ({} failed)", f(&r["runtime"]["wall_secs"]), r["runtime"]["transactions"], r["runtime"]["failed_transactions"]);
    let o = &r["oracle"];
    println!("oracle ${:.2} -> ${:.2} (range ${:.2}-${:.2}), {} jumps, {} outages, {} wide-confidence ticks, reopen gaps over 1%: {}", f(&o["start"]), f(&o["end"]), f(&o["min"]), f(&o["max"]), o["jumps"].as_array().map_or(0, |j| j.len()), o["outages"], o["wide_confidence"],
        o["gaps"].as_array().map(|g| g.iter().filter(|x| f(&x["move_pct"]).abs() >= 1.0).map(|x| format!("{:+.2}%", f(&x["move_pct"]))).collect::<Vec<_>>().join(" ")).unwrap_or_default());
    println!("\nvolume:");
    if let Some(v) = r["volume"].as_object() {
        for (route, x) in v {
            println!("  {:<12} {:>6} fills  ${:>14.2}", route, x["fills"], f(&x["notional_usdc"]));
        }
    }
    println!("  orderbook: {} placed, {} matched, {} cancelled, {} expired-closed; skips {}", r["orderbook"]["orders_placed"], r["orderbook"]["matches"], r["orderbook"]["orders_cancelled"], r["orderbook"]["expired_closed"], r["orderbook"]["match_skips"]);
    let l = &r["liquidations"];
    println!("\nliquidations: {} {} ({} under the pre-close weekend margin), rewards ${:.2}; failed attempts {}", l["count"], l["by_route"], l["in_pre_close_window"], f(&l["rewards_usdc"]), l["failed_attempts"]);
    let bd = r["bad_debt"]["events"].as_array().cloned().unwrap_or_default();
    let kind = |k: &str| bd.iter().filter(|e| e["kind"].as_str() == Some(k)).count();
    println!("bad debt: {} close deficits (${:.2}), {} liquidation deficits, reward top-ups ${:.2}, uncovered ${:.2}", kind("close"), f(&r["closes"]["deficit_usdc"]), kind("liquidation"),
        reward_top_ups(r), f(&r["bad_debt"]["uncovered_usdc"]));
    let fu = &r["funding"];
    println!("funding: {} cranks, {} periods charged, {} dropped (must be 0)", fu["updates"], fu["periods"], fu["periods_dropped"]);
    let ins = &r["insurance"];
    println!("insurance fund ${:.2}, vault at end ${:.2} (vault - insurance ${:.2}); close fees ${:.2}", f(&ins["final_usdc"]), f(&ins["final_vault_usdc"]), f(&ins["vault_minus_insurance_usdc"]), f(&r["closes"]["fees_usdc"]));
    let s = &r["solvency"];
    println!("solvency (invariant 2, {}): {} failed checks, {} ticks short at the mark, min surplus ${:.2} (${:.2} over insurance), ${:.2} free collateral stuck after unwind",
        if r["config"]["strict_solvency"].as_bool() == Some(true) { "fatal" } else { "--lenient-solvency" },
        s["failed_checks"], s["insolvent_ticks"], f(&s["min_surplus_usdc"]), f(&s["min_surplus_minus_insurance_usdc"]), f(&s["stuck_free_collateral_usdc"]));
    let p = &r["pool"];
    println!("\nLP pool: NAV_redeem ${:.2} -> ${:.2} before the LPs redeem (min ${:.2}), per-share {} -> {}, peak drawdown {} bps; LP flows {}",
        f(&p["nav_redeem_start_usdc"]), f(&p["nav_redeem_end_usdc"]), f(&p["min_nav_redeem_usdc"]), p["nps_start"], p["nps_end"], p["peak_drawdown_bps"], p["lp_flows"]);
    println!("  capital use: peak net ${:.2}, {:.3} of the net cap (k_net x capNAV), {:.3} x B; house opens {}, open fees ${:.2}, treasury ${:.2}, cash at end ${:.2}",
        f(&p["peak_net_notional_usdc"]), f(&p["peak_net_over_cap_nav"]), f(&p["peak_net_over_b"]), p["house_opens"], f(&p["open_fees_usdc"]), f(&p["treasury_usdc"]), f(&p["cash_end_usdc"]));
    println!("  haircuts {} | PoolCashInsufficient {} | liquidations {} | settle_pnl {} (${:.2})",
        p["haircuts"], p["pool_cash_insufficient"], p["liquidation_stages"], p["settle_pnl"]["count"], f(&p["settle_pnl"]["usdc"]));
    println!("  deficits ${:.2} (${:.2} beyond insurance: ${:.2} LP-absorbed, ${:.2} pending), written off ${:.2}, swept ${:.2}; ADL {}",
        f(&p["deficits"]["total_usdc"]), f(&p["deficits"]["beyond_insurance_usdc"]), f(&p["deficits"]["lp_absorbed_usdc"]), f(&p["deficits"]["pending_usdc"]),
        f(&p["written_off_usdc"]), f(&p["swept_usdc"]), p["adl"]["by_kind"]);
    for w in p["weekly_closes"].as_array().cloned().unwrap_or_default() {
        println!("  weekly close {}{}: net ${:.2}, B ${:.2}, G·net/B {:.4}, carried excess ${:.2}; reopen gap {}%, weekend drawdown {} (bound {}, ok {}), deficits beyond insurance ${:.2}",
            w["close_time"], if w["in_unwind"].as_bool() == Some(true) { " (unwind)" } else { "" }, f(&w["net_notional_at_close_usdc"]), f(&w["b_at_close_usdc"]), f(&w["g_net_over_b"]),
            f(&w["carried_excess_usdc"]), w["reopen_gap_pct"], w["drawdown"], w["bound"], w["criterion_ok"], f(&w["deficits_beyond_insurance_usdc"]));
        let wk = &w["week"];
        println!("    week to this close: {} liquidations, ADL {} (mode 0) / {} (mode 1), haircuts ${:.2}, deficits ${:.2} (${:.2} beyond insurance), {} LP flows, peak drawdown {} bps, NAV_redeem ${:.2} -> ${:.2}",
            wk["liquidations"], wk["adl_mode0"], wk["adl_mode1"], f(&wk["haircuts_usdc"]), f(&wk["deficits_usdc"]), f(&wk["deficits_beyond_insurance_usdc"]), wk["lp_flows"], wk["peak_drawdown_bps"],
            f(&wk["nav_redeem_start_usdc"]), f(&wk["nav_redeem_at_close_usdc"]));
    }

    println!("\nPnL by agent:");
    for a in r["agents"].as_array().unwrap() {
        let stuck = f(&a["stuck_in_perps_usdc"]);
        println!("  {:<11} {:<15} ${:>11.2} ({:>+7.2}%)  opens {:>4} maker {:>4} closes {:>4} liq {:>2} fees ${:.2}{}",
            a["name"].as_str().unwrap(), a["persona"].as_str().unwrap(), f(&a["pnl_usdc"]), f(&a["pnl_pct"]), a["opens"], a["maker_fills"], a["closes"], a["liquidated"], f(&a["close_fees_usdc"]),
            if stuck > 0.0 { format!("  (${:.2} stuck: withdraw failed)", stuck) } else { String::new() });
    }
    for a in r["lps"].as_array().cloned().unwrap_or_default() {
        println!("  {:<11} {:<15} ${:>11.2} (deposited ${:.2}, withdrew ${:.2}, {} shares left) own flows {}",
            a["name"].as_str().unwrap_or(""), format!("lp-{}", a["persona"].as_str().unwrap_or("")), f(&a["pnl_usdc"]), f(&a["deposited_usdc"]), f(&a["withdrawn_usdc"]), a["shares_left"], a["own_flows"]);
    }
    println!("PnL by persona:");
    for p in r["personas"].as_array().unwrap() {
        println!("  {:<15} ${:>11.2} ({} agents)", p["persona"].as_str().unwrap(), f(&p["pnl_usdc"]), p["agents"]);
    }
    println!("\nmarket makers:");
    for m in r["market_makers"].as_array().unwrap() {
        println!("  {} ({} bps half-spread): {} fills ${:.0}; spread captured ${:.2} ({:.2} bps) vs close fees ${:.2} ({:.2} bps); PnL ${:.2}",
            m["name"].as_str().unwrap(), m["half_spread_bps"], m["fills"], f(&m["volume_usdc"]), f(&m["spread_captured_usdc"]), f(&m["spread_captured_bps_of_volume"]),
            f(&m["close_fees_usdc"]), f(&m["close_fees_bps_of_volume"]), f(&m["pnl_usdc"]));
        println!("    pair closes {}, inventory closes {}, allowance refills {}, reposts {}; peak gross {:.0} contracts, peak locked ${:.0}",
            m["pair_closes"], m["inventory_closes"], m["allowance_refills"], m["quote_reposts"], f(&m["peak_gross_contracts"]), f(&m["peak_locked_usdc"]));
        println!("    capital without netting: avg locked ${:.0} on avg gross ${:.0}, vs ${:.0} a netting engine would need for the net position ({:.1}x)",
            f(&m["avg_locked_usdc"]), f(&m["avg_gross_notional_usdc"]), f(&m["avg_net_only_margin_usdc"]), f(&m["avg_locked_vs_net_only_margin"]));
    }
    println!("\nerrors (instruction, program:error, class, count):");
    for e in r["errors"].as_array().unwrap() {
        println!("  {:<24} {:<44} {:<10} {}", e["instruction"].as_str().unwrap(), e["error"].as_str().unwrap(), e["class"].as_str().unwrap(), e["count"]);
    }
    let cov = &r["coverage"];
    println!("\ncoverage: engine paths reached");
    if let Some(p) = cov["engine_paths"].as_object() {
        let line: Vec<String> = p.iter().map(|(k, v)| format!("{}={}", k, v)).collect();
        for chunk in line.chunks(6) {
            println!("  {}", chunk.join("  "));
        }
    }
    println!("  order fills per order {} | order ends {} | capped fills {} | fills per quote {}", cov["order_fill_histogram"], cov["order_ends"], cov["capped_fills"], cov["quote_fill_histogram"]);
    println!("  not sent (predicted rejection): {}", cov["skipped_by_prediction"]);
    let missing = cov["missing_required_paths"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default();
    println!("  scenario paths not reached: {}", if missing.is_empty() { "none".to_string() } else { missing });
    if let Some(pr) = r["probes"].as_object().filter(|p| !p.is_empty()) {
        println!("\nprobes (invariant 13: the on-chain outcome the spec requires):");
        for (k, v) in pr {
            let nr = v["not_reached"].as_object().map(|m| m.iter().map(|(why, n)| format!("{} x{}", why, n)).collect::<Vec<_>>().join("; ")).unwrap_or_default();
            println!("  {:<32} {:>3} checked {:>3} ok{}", k, v["checked"], v["ok"], if nr.is_empty() { String::new() } else { format!("  not reached: {}", nr) });
        }
    }
    println!("\ninvariants:");
    let strict = r["config"]["strict_solvency"].as_bool() == Some(true);
    for (k, v) in r["invariants"].as_object().unwrap() {
        let ok = v["ok"].as_bool().unwrap_or(false);
        let status = if ok { "ok" } else if k == crate::checks::SOLVENCY && !strict { "FINDING (non-fatal)" } else { "FAIL" };
        println!("  {:<30} {:>8} checks  {:>5} failures  {}", k, v["checks"], v["failures"], status);
    }
    println!("\ncompute units (mean / max):");
    for (k, v) in r["compute_units"].as_object().unwrap() {
        println!("  {:<24} {:>8} / {:>8}  ({} tx)", k, v["mean"], v["max"], v["count"]);
    }
    let n = r["fatal_violation_count"].as_u64().unwrap_or(0);
    if n > 0 {
        println!("\n{} VIOLATIONS; first ones:", n);
        let strict = r["config"]["strict_solvency"].as_bool() == Some(true);
        for v in r["violations"].as_array().unwrap().iter().filter(|v| strict || v["invariant"].as_str() != Some(crate::checks::SOLVENCY)).take(10) {
            println!("  tick {} [{}] {}", v["tick"], v["invariant"].as_str().unwrap(), v["detail"].as_str().unwrap());
        }
    } else {
        println!("\nno violations");
    }
}

/// Insurance-paid keeper reward top-ups: only the liquidation events carry
/// them (keeper.rs), not the bad-debt events.
pub fn reward_top_ups(r: &Value) -> f64 {
    // + 0.0: an empty f64 sum is -0.0
    r["liquidations"]["events"].as_array().map_or(0.0, |ev| ev.iter().map(|e| e["reward_top_up_usdc"].as_f64().unwrap_or(0.0)).sum::<f64>()) + 0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reward_top_ups_sum_the_liquidation_events() {
        let r = json!({
            "liquidations": { "events": [ { "reward_top_up_usdc": 1.5 }, { "reward_top_up_usdc": 0.25 }, { "stage": 1 } ] },
            "bad_debt": { "events": [ { "kind": "liquidation", "deficit_usdc": 9.0 } ] },
        });
        assert_eq!(reward_top_ups(&r), 1.75);
    }
}
