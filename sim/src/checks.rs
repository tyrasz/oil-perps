//! Failure classification (invariants 6 and 10) and the record of invariant results.
//!
//! Every transaction is sent with the model's prediction of how it must fail
//! (predict.rs), or None when it must land. A failure is expected only when it
//! is exactly the predicted error and that error is in its instruction label's
//! allowed set; anything else, including any panic, is unexpected. A predicted
//! rejection that lands anyway is a violation of invariant 10.

use crate::chain::TxFailure;
use crate::predict::{allowed_for, Rej};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Expected,
    Unexpected,
}

pub fn classify(label: &str, f: &TxFailure, predicted: Option<Rej>) -> Class {
    let matches = predicted.map_or(false, |(program, code)| program == f.program && code == f.code);
    if !f.panic && matches && allowed_for(label).contains(&f.code.as_str()) {
        Class::Expected
    } else {
        Class::Unexpected
    }
}

#[derive(Default)]
pub struct ErrorEntry {
    pub count: u64,
    pub class: Option<Class>,
    pub first_tick: u64,
    pub example: String,
}

#[derive(Default)]
pub struct Violation {
    pub tick: u64,
    pub time: i64,
    pub invariant: &'static str,
    pub detail: String,
}

/// Solvency (docs/lp-pool.md §16.7): the vault covers free collateral and
/// every open position's claim at the solvency haircut, or bad debt is
/// pending for ADL. Fatal by default now that the LP pool is the house
/// counterparty; `--lenient-solvency` reports it as a finding instead.
pub const SOLVENCY: &str = "2_solvency";

/// The outcome a probe (adversary.rs, the LP agents' gate attempts) must
/// have on-chain by the spec: a named rejection, or a landing with a stated
/// effect. Checked against the chain itself, apart from the model's prediction.
pub const PROBES: &str = "13_probe_outcomes";
/// An LP flow that landed passed every gate (docs/lp-pool.md §9.5), each
/// recomputed from the schedule and the accounts, not from the model.
pub const LP_GATES: &str = "14_lp_gates";
/// §17.4: across each weekend whose reopen gap is at most G, the per-share
/// NAV drawdown is at most G·net/B at the close plus the deficits beyond
/// insurance booked meanwhile, over B.
pub const WEEKEND: &str = "15_weekend_criterion";
/// Coverage (third review): every required scenario path is reached (runs
/// of at least two days), and no label has its transactions skipped by a
/// predicted rejection more than `SKIP_LIMIT_PER_DAY` times a day.
pub const COVERAGE: &str = "16_coverage";

pub const INVARIANTS: [&str; 21] = [
    "1_open_interest",
    SOLVENCY,
    "2a_bucket_identity",
    "2b_vault_ledger",
    "2c_pool_aggregates",
    "3_liquidation_predicate",
    "3_no_unliquidated_positions",
    "4_pair_settlement",
    "5_allowance_tracked",
    "6_unexpected_errors",
    "7_final_conservation",
    "8_fill_accounting",
    "9_settlement_accounting",
    "9b_bad_debt_accounting",
    "10_predicted_rejections",
    "11_session_model",
    "12_collusion_no_bad_debt",
    PROBES,
    LP_GATES,
    WEEKEND,
    COVERAGE,
];

#[derive(Default)]
pub struct CheckBook {
    pub strict_solvency: bool,
    /// (instruction label, program:code) -> entry
    pub errors: BTreeMap<(String, String), ErrorEntry>,
    pub violations: Vec<Violation>,
    /// invariant -> (checks run, failures)
    pub results: BTreeMap<&'static str, (u64, u64)>,
    /// Ticks the solvency surplus was below the insurance fund
    pub unbacked_insurance_ticks: u64,
    pub insolvent_ticks: u64,
    pub min_surplus: Option<i128>,
    pub min_surplus_minus_insurance: Option<i128>,
}

impl CheckBook {
    pub fn new(strict_solvency: bool) -> CheckBook {
        let mut b = CheckBook { strict_solvency, ..CheckBook::default() };
        for i in INVARIANTS {
            b.results.insert(i, (0, 0));
        }
        b
    }

    pub fn fatal(&self, invariant: &str) -> bool {
        invariant != SOLVENCY || self.strict_solvency
    }

    /// Count one evaluation of `invariant`; record a violation when `ok` is false.
    pub fn check(&mut self, tick: u64, time: i64, invariant: &'static str, ok: bool, detail: impl FnOnce() -> String) -> bool {
        let r = self.results.entry(invariant).or_insert((0, 0));
        r.0 += 1;
        if !ok {
            r.1 += 1;
            let detail = detail();
            let shown = self.violations.iter().filter(|v| v.invariant == invariant).count();
            if shown < if self.fatal(invariant) { 50 } else { 3 } {
                let tag = if self.fatal(invariant) { "VIOLATION" } else { "finding" };
                eprintln!("{} [{}] tick {}: {}", tag, invariant, tick, detail);
            }
            self.violations.push(Violation { tick, time, invariant, detail });
        }
        ok
    }

    /// Record a failed transaction against its prediction.
    pub fn failure(&mut self, tick: u64, time: i64, label: &str, f: &TxFailure, predicted: Option<Rej>) -> Class {
        let class = classify(label, f, predicted);
        let e = self.errors.entry((label.to_string(), f.key())).or_default();
        if e.count == 0 {
            e.first_tick = tick;
            e.example = f.detail.chars().take(300).collect();
            e.class = Some(class);
        }
        if class == Class::Unexpected {
            e.class = Some(class);
        }
        e.count += 1;
        let detail = || {
            format!("{} failed with {} ({}), predicted {:?}: {}", label, f.key(), f.raw, predicted.map(|(p, c)| format!("{}:{}", p, c)), f.detail)
        };
        self.check(tick, time, "6_unexpected_errors", class == Class::Expected, detail);
        class
    }

    /// A transaction landed: it must not have been predicted to fail.
    pub fn landed(&mut self, tick: u64, time: i64, label: &str, predicted: Option<Rej>) {
        self.check(tick, time, "10_predicted_rejections", predicted.is_none(), || {
            format!("{} landed, but the model predicts it fails with {:?}", label, predicted)
        });
    }

    /// A violation of an invariant that fails the run.
    pub fn violated(&self) -> bool {
        self.violations.iter().any(|v| self.fatal(v.invariant))
    }

    pub fn fatal_count(&self) -> usize {
        self.violations.iter().filter(|v| self.fatal(v.invariant)).count()
    }
}
