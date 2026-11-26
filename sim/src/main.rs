//! Agent-based simulation of the OIL perps market.
//!
//! Two market makers quote on mm-registry, retail traders take those quotes,
//! rest orderbook orders or open at the oracle, and a keeper matches the book,
//! cranks funding, liquidates and closes expired orders, over several
//! simulated days of the CME session schedule (weekend close and Sunday
//! reopen included). Invariants are checked as transactions land and after
//! every tick; any violation or unexpected engine error makes the run exit
//! non-zero.
//!
//!   cargo run -p sim --release -- --seed 1 --days 2
//!   cargo run -p sim --release -- --scenario stress --seed 2
//!   SBF_OUT_DIR=$PWD/target/deploy cargo run -p sim --release -- --seed 1   # the real SBF binaries
//!
//! Options: --seed N (1), --days D (5), --tick-secs S (300), --report path.json,
//! --scenario normal|stress|adversarial (normal; see scenario.rs). Solvency
//! (invariant 2) is fatal by default; --lenient-solvency reports it as a
//! finding instead (--strict-solvency is accepted and does nothing).

// (deprecated: its base64::decode, as in perps-core's own tests)
#[allow(dead_code, deprecated)]
#[path = "../../programs/perps-core/tests/support/market_env.rs"]
mod support;

// perps-core's dependency-free pool math and session helpers, shared with the
// keeper (docs/lp-pool.md §2.7)
#[allow(dead_code)]
#[path = "../../programs/perps-core/src/pool_math.rs"]
mod pool_math;
#[allow(dead_code)]
#[path = "../../programs/perps-core/src/session.rs"]
mod session;

mod actions;
mod adversary;
mod agents;
mod chain;
#[cfg(test)]
mod differential;
mod checks;
mod invariants;
mod keeper;
mod ledger;
mod metrics;
mod model;
mod oracle;
mod predict;
mod report;
mod scenario;
mod schedule;
mod world;

use rand::seq::SliceRandom;
use std::process::ExitCode;
use std::time::Instant;
use scenario::Scenario;
use support::*;
use world::{Config, Sim};

fn usage() -> ! {
    eprintln!("usage: sim [--seed N] [--days D] [--tick-secs S] [--report path.json] [--scenario normal|stress|adversarial] [--lenient-solvency]");
    std::process::exit(2)
}

fn parse_args() -> Config {
    let mut cfg = Config { seed: 1, days: 5.0, tick_secs: 300, report: None, scenario: Scenario::Normal, strict_solvency: true };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| usage());
        match arg.as_str() {
            "--seed" => cfg.seed = value().parse().unwrap_or_else(|_| usage()),
            "--days" => cfg.days = value().parse().unwrap_or_else(|_| usage()),
            "--tick-secs" => cfg.tick_secs = value().parse().unwrap_or_else(|_| usage()),
            "--report" => cfg.report = Some(value()),
            "--scenario" => cfg.scenario = Scenario::parse(&value()).unwrap_or_else(|| usage()),
            // The default since the LP pool (docs/lp-pool.md §16.7); kept for old command lines
            "--strict-solvency" => {}
            "--lenient-solvency" => cfg.strict_solvency = false,
            "-h" | "--help" => usage(),
            _ => usage(),
        }
    }
    if cfg.days <= 0.0 || cfg.tick_secs <= 0 || cfg.tick_secs > 3_600 {
        usage();
    }
    cfg
}

/// Which binaries run: the SBF .so files from SBF_OUT_DIR, or the programs compiled in natively.
fn mode() -> String {
    match std::env::var("SBF_OUT_DIR").or_else(|_| std::env::var("BPF_OUT_DIR")) {
        Ok(dir) => {
            for so in ["perps_core.so", "mm_registry.so", "orderbook.so"] {
                let path = std::path::Path::new(&dir).join(so);
                if !path.exists() {
                    eprintln!("error: {} not found; run scripts/build-sbf.sh", path.display());
                    std::process::exit(2);
                }
            }
            format!("SBF ({})", dir)
        }
        Err(_) => "native".into(),
    }
}

impl Sim {
    /// Move the clock one tick and publish the oracle (unless the feed is out).
    async fn advance(&mut self, dt: i64) {
        self.now += dt;
        let base = self.chain.base();
        let mut clock: Clock = base.ctx.banks_client.get_sysvar().await.unwrap();
        clock.unix_timestamp = self.now;
        base.ctx.set_sysvar(&clock);
        let open = self.in_session();
        self.at_edge = self.was_open && !open;
        self.just_reopened = open && !self.was_open;
        if open && !self.was_open {
            self.last_reopen_tick = self.tick;
        }
        self.was_open = open;
        let publication = self.oracle.step(&mut self.oracle_rng, self.now, dt, open);
        if publication.published {
            self.publish(publication.price, publication.conf).await;
        }
        if open && publication.published {
            self.history.push(self.px);
        }
    }

    async fn publish(&mut self, price: u64, conf: u64) {
        let now = self.now;
        set_price_raw(self.chain.base(), price as i64 * 100, conf * 100, now).await;
        self.px = price;
        self.conf = conf;
        self.published_at = now;
    }

    async fn set_clock(&mut self) {
        let now = self.now;
        let base = self.chain.base();
        let mut clock: Clock = base.ctx.banks_client.get_sysvar().await.unwrap();
        clock.unix_timestamp = now;
        base.ctx.set_sysvar(&clock);
    }

    /// Move the clock to `t`, republishing the last price fresh and tight
    /// (the unwind's warps: no price moves, only time).
    async fn warp_to(&mut self, t: i64) {
        self.now = t;
        self.set_clock().await;
        let px = self.px;
        self.publish(px, px / 5_000).await;
        // Session edges, so a weekly close and its reopen crossed by the
        // unwind are recorded for the §17.4 criterion (at a zero gap: the
        // warps republish the same price)
        let open = self.in_session();
        self.at_edge = self.was_open && !open;
        self.just_reopened = open && !self.was_open;
        self.was_open = open;
    }

    /// After an unwind warp: the keeper and the invariants run in session
    /// (or settled), and at a weekly close so it is recorded.
    async fn after_warp(&mut self) {
        if self.in_session() || self.settled {
            let failures = self.keeper_pass().await;
            self.check_invariants(&failures).await;
        } else if self.at_edge {
            self.check_invariants(&Default::default()).await;
        }
    }

    /// Warp tick by tick until `ok(self)` holds (at most `limit` ticks), running
    /// the keeper (its settled pass on a settled market) along the way.
    async fn warp_until(&mut self, limit: u64, ok: impl Fn(&Sim) -> bool) -> bool {
        for _ in 0..limit {
            if ok(self) {
                return true;
            }
            let t = self.now + self.cfg.tick_secs;
            self.warp_to(t).await;
            self.after_warp().await;
        }
        ok(self)
    }

    /// In session, past the profit-close quiet time after the open (or after
    /// settlement) and, for LP exits, outside the pool's W and Q gates.
    fn can_close(&self) -> bool {
        let q = crate::chain::pool_params().profit_close_quiet_secs as i64;
        if self.settled {
            return true;
        }
        self.in_session() && session::seconds_since_open(1, self.now).map_or(true, |t| t >= q)
    }

    fn lp_in_w(&self) -> bool {
        crate::schedule::in_pre_close_window(self.now, crate::chain::pool_params().weekend_cap_window_secs)
    }

    /// lp-lapse's request, never claimed: once its claim window of open
    /// time is over, lp_withdraw must refuse it (WithdrawalExpired, F5).
    /// The unwind warps there if the run ended first.
    async fn probe_lapsed_request(&mut self) {
        let Some(l) = self.lps.iter().position(|w| w.persona == agents::LpPersona::Lapse) else { return };
        let pos = self.lp_position(l).await;
        if pos.pending_shares == 0 {
            self.probe_skip("lp_withdraw_expired", "no request to lapse");
            return;
        }
        let p = crate::chain::pool_params();
        let window = pool_math::effective_claim_window_secs(crate::session::SCHEDULE_CME_GLOBEX, pos.claim_window_secs, p.weekend_cap_window_secs, p.lp_reopen_quiet_secs) as i64;
        let unlock = pos.unlock_at;
        if window == 0 || !self.warp_until(20_000, |s| actions::open_secs_between(unlock, s.now) > window).await {
            self.probe_skip("lp_withdraw_expired", "the claim window did not end");
            return;
        }
        let n0 = self.chain.transactions;
        self.lp_withdraw(l, "lp_withdraw_probe").await;
        self.probe_rejected("lp_withdraw_expired", n0, "WithdrawalExpired");
    }

    fn lp_window_open(&self) -> bool {
        let p = crate::chain::pool_params();
        if self.settled {
            // D41: the settled quiet time after settled_at
            let at = self.shadow.as_ref().map_or(0, |st| st.m.settled_at);
            return at == 0 || self.now - at >= p.profit_close_quiet_secs.max(p.lp_reopen_quiet_secs) as i64;
        }
        self.in_session()
            && !crate::schedule::in_pre_close_window(self.now, p.weekend_cap_window_secs)
            && session::seconds_since_open(1, self.now).map_or(true, |t| t >= p.lp_reopen_quiet_secs as i64)
    }

    /// Everyone closes every position, cancels orders and quotes, withdraws
    /// everything, and the MMs take their bonds back; then the keeper clears
    /// pending bad debt (ADL, else the write-off after 24 open hours) and every
    /// LP redeems every share it holds (invariant 7, docs/lp-pool.md §16.7):
    /// the unwind warps into open sessions past the withdrawal delay, outside
    /// W and Q, within the claim window. Normally at the final price in
    /// session; in the adversarial scenario the admin settles the market
    /// first and everything closes at the settlement price, out of session.
    async fn unwind(&mut self) {
        self.unwinding = true;
        // The keeper may ADL anything now, the probes' positions included
        self.adversary.held.clear();
        self.adversary.mode1_retries = None;
        if self.cfg.scenario == Scenario::Adversarial {
            // An LP deposits just before settlement, to request right after the whole-fund sweep
            self.arm_sweep_sandwich().await;
            // 2% under the last price, so settlement PnL is not just the mark's
            let settlement = self.px - self.px / 50;
            self.settle_and_probe(settlement).await;
            // Winners wait out the profit-close quiet time after settlement (D35)
            let quiet = crate::chain::pool_params().profit_close_quiet_secs as i64;
            let settled_at = self.market().await.settled_at;
            while self.in_session() || self.now < settled_at + quiet {
                self.now += self.cfg.tick_secs;
                self.set_clock().await;
            }
        } else {
            // close_position needs the session open, a fresh, tight oracle and
            // (for winners) the profit-close quiet time behind it
            self.warp_until(10_000, |s| s.can_close()).await;
            let px = self.px;
            self.publish(px, px / 5_000).await;
        }
        // Every quote still active (expired ones included: they count against deregistering)
        let quotes: Vec<_> = self.ledger.quote_fills.iter().map(|(q, _)| *q).collect();
        for address in quotes {
            let Some(q) = try_load::<mm_registry::TwoSidedQuote>(self.chain.base(), address).await else { continue };
            if !q.is_active {
                continue;
            }
            let Some(i) = self.agents.iter().position(|a| a.mm.as_ref().map(|b| b.market_maker) == Some(q.market_maker)) else { continue };
            let owner = self.agents[i].t.key();
            let kp = self.agents[i].t.kp.insecure_clone();
            let ix = self.cancel_quote_ix(&owner, address);
            let _ = self.run("cancel_quote", &[ix], &[&kp], None).await;
        }
        for o in 0..self.ledger.orders.len() {
            let (address, owner) = (self.ledger.orders[o].address, self.ledger.orders[o].owner);
            if !self.ledger.orders[o].live || !exists(self.chain.base(), address).await {
                continue;
            }
            let (key, kp) = (self.agents[owner].t.key(), self.agents[owner].t.kp.insecure_clone());
            let ix = self.cancel_order_ix(&key, address);
            if self.run("cancel_order", &[ix], &[&kp], None).await.is_ok() {
                self.ledger.orders[o].live = false;
                self.ledger.orders[o].end = Some(ledger::OrderEnd::Cancelled);
            }
        }
        // Close everything, losers first so winners find the cash they are
        // owed; a close that would raise |net| past the cap waits for the
        // others. The keeper liquidates, settles PnL and runs ADL between rounds.
        for round in 0..20 {
            if self.ledger.open.is_empty() {
                break;
            }
            // The owners close first; then the keeper (on a settled market it
            // closes whatever is left at the settlement price)
            if round > 0 || !self.settled {
                let failures = self.keeper_pass().await;
                self.check_invariants(&failures).await;
            }
            if !self.settled {
                self.settle_pnl_pass(true).await;
            }
            let m = self.market().await;
            let mark = predict::close_mark(&m, &self.ov());
            let mut order: Vec<(i128, usize)> = self.ledger.open.iter().map(|&i| (model::realized(&self.ledger.positions[i].state, mark, m.cumulative_funding), i)).collect();
            order.sort();
            for (_, idx) in order {
                if self.ledger.positions[idx].is_open() {
                    self.close_tracked(idx, "close_position").await;
                }
            }
            if !self.ledger.open.is_empty() && !self.settled {
                // Try again a tick later, in session
                let t = self.now + self.cfg.tick_secs;
                self.warp_to(t).await;
                self.warp_until(10_000, |s| s.can_close()).await;
            }
        }
        self.metrics.unwind_failures += self.ledger.open.len() as u64;
        // Pending bad debt: ADL whoever is left, else wait out the write-off
        let pending = |s: &Sim| s.shadow.as_ref().map_or(false, |st| st.pool.pending_bad_debt_long > 0 || st.pool.pending_bad_debt_short > 0);
        if pending(self) {
            self.hit("unwind.pending_bad_debt");
            if !self.warp_until(2_000, |s| !pending(s)).await {
                self.metrics.unwind_failures += 1;
            }
        }
        for i in 0..self.agents.len() {
            if !self.withdraw_all(i).await {
                self.metrics.unwind_failures += 1;
            }
            if self.agents[i].mm.is_some() {
                let kp = self.agents[i].t.kp.insecure_clone();
                let ix = self.deregister_ix(i);
                if self.run("deregister_mm", &[ix], &[&kp], None).await.is_err() {
                    self.metrics.unwind_failures += 1;
                }
            }
        }
        // A settled market with no positions left sweeps its whole fund to LP
        // cash (D19); it vests over the withdrawal delay (D18), so the LPs
        // wait for it before they request
        self.sweep_pass().await;
        let vest_end = self.pool().await.vest_end;
        self.warp_until(10_000, |s| s.now >= vest_end).await;
        // Every LP redeems every share: request, wait the delay, withdraw
        self.warp_until(10_000, |s| s.settled || s.in_session()).await;
        let st = self.st().await;
        if let Ok(v) = model::view(&st.pool, &st.m, predict::close_mark(&st.m, &self.ov()), self.now) {
            self.pool_stats.before_redeem = Some(serde_json::json!({ "time": self.now, "nav_redeem": v.nav_redeem as i64, "nps": v.nps, "cash": st.pool.cash, "lp_shares": st.pool.lp_shares }));
        }
        if !self.settled {
            self.probe_lapsed_request().await;
        }
        self.crank_funding().await;
        for l in 0..self.lps.len() {
            // A request still pending from the run is replaced by one for every share
            if self.lp_position(l).await.pending_shares > 0 && !self.lp_cancel(l, "lp_cancel").await {
                self.metrics.unwind_failures += 1;
            }
            let pos = self.lp_position(l).await;
            if pos.shares > 0 && !self.lp_request(l, pos.shares, "lp_request").await {
                self.metrics.unwind_failures += 1;
            }
        }
        let unlock = self.now + crate::chain::pool_params().withdrawal_delay_secs as i64;
        // On the way: the reopen's quiet time Q refuses a matured withdrawal (D17)
        let quiet = crate::chain::pool_params().lp_reopen_quiet_secs as i64;
        let mut probed_q = self.settled;
        for _ in 0..10_000 {
            // Past the delay and outside the gates; first through one reopen's Q
            if self.now >= unlock && self.lp_window_open() && probed_q {
                break;
            }
            let in_q = self.in_session() && !crate::schedule::is_open(self.now - quiet);
            if !probed_q && self.now >= unlock && in_q && !self.lp_in_w() {
                probed_q = true;
                self.crank_funding().await;
                let n0 = self.chain.transactions;
                self.lp_withdraw(0, "lp_withdraw_probe").await;
                self.probe_rejected("lp_withdraw_in_q", n0, "LpFlowWindowClosed");
            }
            let t = self.now + self.cfg.tick_secs;
            self.warp_to(t).await;
            self.after_warp().await;
        }
        self.crank_funding().await;
        for l in 0..self.lps.len() {
            if self.lp_position(l).await.pending_shares > 0 && self.lp_withdraw(l, "lp_withdraw").await.is_none() {
                self.metrics.unwind_failures += 1;
            }
        }
        self.check_invariants(&Default::default()).await;
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let cfg = parse_args();
    // Program logs at debug level would print every transaction's log
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "solana_program_test=info");
    }
    let mode = mode();
    eprintln!("sim: {} scenario, seed {}, {} days, {} s ticks, programs: {}", cfg.scenario.name(), cfg.seed, cfg.days, cfg.tick_secs, mode);
    let started = Instant::now();
    let report_path = cfg.report.clone();
    let ticks = (cfg.days * 86_400.0 / cfg.tick_secs as f64).round() as u64;
    let tick_secs = cfg.tick_secs;

    let mut sim = Sim::new(cfg).await;
    let start_time = sim.now;
    let (mut price_min, mut price_max) = (sim.px, sim.px);
    let mut order: Vec<usize> = (0..sim.agents.len()).collect();
    for tick in 1..=ticks {
        sim.tick = tick;
        sim.advance(tick_secs).await;
        price_min = price_min.min(sim.px);
        price_max = price_max.max(sim.px);
        // Random turn order: some takers act before the MMs refresh, against last tick's quotes
        order.shuffle(&mut sim.rng);
        for &i in &order {
            sim.act(i).await;
        }
        sim.act_lps().await;
        if sim.cfg.scenario == Scenario::Adversarial && sim.in_session() {
            sim.adversary_step(ticks).await;
        }
        let failures = sim.keeper_pass().await;
        sim.check_invariants(&failures).await;
        if tick % (86_400 / tick_secs as u64).max(1) == 0 || tick == ticks {
            let m = sim.market().await;
            eprintln!(
                "  day {:.1}: oracle ${:.2}, {} open positions, OI {:.0}/{:.0}, insurance ${:.2}, {} tx, {} violations, {:.0}s",
                tick as f64 * tick_secs as f64 / 86_400.0, sim.px as f64 / 1e6, sim.ledger.open.len(),
                m.long_open_interest as f64 / 1e6, m.short_open_interest as f64 / 1e6, m.insurance_fund as f64 / 1e6,
                sim.chain.transactions, sim.checks.fatal_count(), started.elapsed().as_secs_f64()
            );
        }
    }
    sim.tick = ticks + 1;
    sim.unwind().await;
    sim.check_final().await;
    sim.check_coverage();

    let info = report::RunInfo { mode, wall_secs: started.elapsed().as_secs_f64(), start_time, ticks, price_min, price_max };
    let report = sim.report(&info).await;
    report::print_summary(&report);
    if let Some(path) = report_path {
        std::fs::write(&path, serde_json::to_string_pretty(&report).unwrap()).expect("write the report");
        println!("report written to {}", path);
    }
    if sim.checks.violated() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
