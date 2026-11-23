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
//! --scenario normal|stress|adversarial (normal; see scenario.rs), and
//! --strict-solvency to make the solvency finding (invariant 2) fatal.

#[allow(dead_code)]
#[path = "../../programs/perps-core/tests/support/market_env.rs"]
mod support;

mod actions;
mod adversary;
mod agents;
mod chain;
mod checks;
mod invariants;
mod keeper;
mod ledger;
mod metrics;
mod model;
mod oracle;
mod predict;
mod quiet_events;
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
    eprintln!("usage: sim [--seed N] [--days D] [--tick-secs S] [--report path.json] [--scenario normal|stress|adversarial] [--strict-solvency]");
    std::process::exit(2)
}

fn parse_args() -> Config {
    let mut cfg = Config { seed: 1, days: 5.0, tick_secs: 300, report: None, scenario: Scenario::Normal, strict_solvency: false };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| usage());
        match arg.as_str() {
            "--seed" => cfg.seed = value().parse().unwrap_or_else(|_| usage()),
            "--days" => cfg.days = value().parse().unwrap_or_else(|_| usage()),
            "--tick-secs" => cfg.tick_secs = value().parse().unwrap_or_else(|_| usage()),
            "--report" => cfg.report = Some(value()),
            "--scenario" => cfg.scenario = Scenario::parse(&value()).unwrap_or_else(|| usage()),
            "--strict-solvency" => cfg.strict_solvency = true,
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

    /// Everyone closes every position, cancels orders and quotes, withdraws
    /// everything, and the MMs take their bonds back. Normally at the final
    /// price in session; in the adversarial scenario the admin settles the
    /// market first and everyone closes at the settlement price, out of session.
    async fn unwind(&mut self) {
        if self.cfg.scenario == Scenario::Adversarial {
            // 2% under the last price, so settlement PnL is not just the mark's
            let settlement = self.px - self.px / 50;
            self.settle_and_probe(settlement).await;
            while self.in_session() {
                self.now += self.cfg.tick_secs;
                self.set_clock().await;
            }
        } else {
            // close_position needs the session open and a fresh, tight oracle
            while !self.in_session() {
                self.now += self.cfg.tick_secs;
                self.set_clock().await;
            }
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
        for idx in self.ledger.open.clone() {
            if !self.close_tracked(idx, "close_position").await {
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
