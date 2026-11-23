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
        let multi = self.ledger.orders.iter().filter(|o| o.fills > 1).count() as u64;
        if multi > 0 {
            self.metrics.paths.insert("order.fills>1".into(), multi);
        }
        let missing: Vec<&str> = self.cfg.scenario.required_paths().iter().copied().filter(|p| !self.metrics.paths.contains_key(*p)).collect();
        let fatal = self.checks.fatal_count();
        let solvency_failures = self.checks.results.get(crate::checks::SOLVENCY).map_or(0, |r| r.1);
        let m = &self.metrics;
        let funding_paid: f64 = m.funding_updates.iter().map(|f| f["paid_by_longs_usdc"].as_f64().unwrap_or(0.0)).sum();
        let funding_received: f64 = m.funding_updates.iter().map(|f| f["paid_to_shorts_usdc"].as_f64().unwrap_or(0.0)).sum();
        json!({
            "config": { "scenario": self.cfg.scenario.name(), "strict_solvency": self.cfg.strict_solvency, "seed": self.cfg.seed, "days": self.cfg.days, "tick_secs": self.cfg.tick_secs, "mode": info.mode, "start_time": info.start_time, "end_time": self.now, "ticks": info.ticks },
            "runtime": { "wall_secs": info.wall_secs, "transactions": self.chain.transactions, "failed_transactions": self.chain.failed_transactions },
            "oracle": {
                "start": 75.0, "end": self.px as f64 / 1e6, "min": info.price_min as f64 / 1e6, "max": info.price_max as f64 / 1e6,
                "jumps": self.oracle.jumps.iter().map(|(t, j)| json!({ "time": t, "move_pct": j })).collect::<Vec<_>>(), "outages": self.oracle.outages, "wide_confidence": self.oracle.wide_conf,
                "gaps": self.oracle.gaps.iter().map(|(t, g)| json!({ "time": t, "move_pct": g })).collect::<Vec<_>>(),
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
                "updates": m.funding_updates.len(), "paid_by_longs_usdc": funding_paid, "paid_to_shorts_usdc": funding_received,
                "kept_by_vault_usdc": usdc(m.funding_imbalance), "events": m.funding_updates,
            },
            "insurance": {
                "final_usdc": usdc(m.final_insurance), "final_vault_usdc": usdc(m.final_vault),
                "vault_minus_insurance_usdc": usdc(m.final_vault as i128 - m.final_insurance as i128),
                "hourly": m.insurance_series.iter().map(|(t, f)| json!([t, *f as f64 / 1e6])).collect::<Vec<_>>(),
            },
            "solvency": {
                "finding": "house positions and orphaned venue legs have the shared vault as counterparty, backed only by the insurance fund (engine design issue, being escalated); non-fatal unless --strict-solvency",
                "failed_checks": solvency_failures, "insolvent_ticks": self.checks.insolvent_ticks,
                "stuck_free_collateral_usdc": usdc(m.stuck_free),
                "min_surplus_usdc": self.checks.min_surplus.map(usdc), "min_surplus_minus_insurance_usdc": self.checks.min_surplus_minus_insurance.map(usdc),
                "ticks_insurance_not_backed": self.checks.unbacked_insurance_ticks,
                "first_insolvent_tick": m.first_insolvent,
                "vault_counterparty_peak": {
                    "positions": m.peak_unpaired_count, "net_contracts": m.peak_unpaired_net as f64 / 1e6, "vault_owes_usdc": usdc(m.peak_unpaired_pnl),
                },
            },
            "agents": agents,
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
        bd.iter().map(|e| f(&e["reward_top_up_usdc"])).sum::<f64>(), f(&r["bad_debt"]["uncovered_usdc"]));
    let fu = &r["funding"];
    println!("funding: {} updates, longs paid ${:.2}, shorts received ${:.2}, vault kept ${:.2}", fu["updates"], f(&fu["paid_by_longs_usdc"]), f(&fu["paid_to_shorts_usdc"]), f(&fu["kept_by_vault_usdc"]));
    let ins = &r["insurance"];
    println!("insurance fund ${:.2}, vault at end ${:.2} (vault - insurance ${:.2}); close fees ${:.2}", f(&ins["final_usdc"]), f(&ins["final_vault_usdc"]), f(&ins["vault_minus_insurance_usdc"]), f(&r["closes"]["fees_usdc"]));
    let s = &r["solvency"];
    let vc = &s["vault_counterparty_peak"];
    println!();
    println!("!!! FINDING (solvency, invariant 2; {}): {} failed checks, insolvent at the mark on {} ticks, min surplus ${:.2},",
        if r["config"]["strict_solvency"].as_bool() == Some(true) { "fatal: --strict-solvency" } else { "non-fatal by default" },
        s["failed_checks"], s["insolvent_ticks"], f(&s["min_surplus_usdc"]));
    println!("!!!   min surplus - insurance ${:.2}, {} ticks with the insurance fund not backed, ${:.2} free collateral stuck after unwind;",
        f(&s["min_surplus_minus_insurance_usdc"]), s["ticks_insurance_not_backed"], f(&s["stuck_free_collateral_usdc"]));
    println!("!!!   cause: house positions and orphaned venue legs have the shared vault as counterparty (peak {} positions, |net| {:.1} contracts, vault owing ${:.2})",
        vc["positions"], f(&vc["net_contracts"]), f(&vc["vault_owes_usdc"]));
    println!("!!!   first insolvent: {}", s["first_insolvent_tick"]);

    println!("\nPnL by agent:");
    for a in r["agents"].as_array().unwrap() {
        let stuck = f(&a["stuck_in_perps_usdc"]);
        println!("  {:<11} {:<15} ${:>11.2} ({:>+7.2}%)  opens {:>4} maker {:>4} closes {:>4} liq {:>2} fees ${:.2}{}",
            a["name"].as_str().unwrap(), a["persona"].as_str().unwrap(), f(&a["pnl_usdc"]), f(&a["pnl_pct"]), a["opens"], a["maker_fills"], a["closes"], a["liquidated"], f(&a["close_fees_usdc"]),
            if stuck > 0.0 { format!("  (${:.2} stuck: withdraw failed)", stuck) } else { String::new() });
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
        println!("\nno violations (solvency is reported above as a finding)");
    }
}
