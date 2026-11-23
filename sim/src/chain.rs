//! Transaction execution on the in-process bank, with the failure decoded from
//! the program logs and the compute units recorded per instruction label.
//!
//! The harness's `send` waits for a fresh blockhash before every transaction
//! (up to one simulated slot, 400 ms), which would make a multi-day run take
//! hours. Here a transaction uses the latest blockhash and only waits for a new
//! one when it would otherwise repeat a signature already sent with it.

use crate::support::{Base, Instruction, Keypair, Pubkey, Signer, Transaction, TransactionError};
use perps_core::state::PoolParams;
use solana_program_test::ProgramTestBanksClientExt;
use solana_sdk::hash::Hash;
use solana_sdk::instruction::InstructionError;
use solana_sdk::signature::Signature;
use std::collections::{BTreeMap, HashSet};

/// Why a transaction failed.
#[derive(Clone, Debug)]
pub struct TxFailure {
    /// Program that raised the error (innermost failing program), or "runtime".
    pub program: String,
    /// Anchor error name when the logs carry one, else the runtime error.
    pub code: String,
    /// The transaction error as the runtime reported it.
    pub raw: String,
    /// A program panicked (overflow, unwrap, ...).
    pub panic: bool,
    /// The error log line, for the report's examples.
    pub detail: String,
}

impl TxFailure {
    /// `program:code`, the error histogram key.
    pub fn key(&self) -> String {
        format!("{}:{}", self.program, self.code)
    }
}

pub struct TxOutcome {
    pub result: Result<(), TxFailure>,
    /// Program logs (msg! lines included), for the engine's own accounting messages
    pub logs: Vec<String>,
}

#[derive(Default, Clone, Copy)]
pub struct CuStat {
    pub count: u64,
    pub total: u64,
    pub max: u64,
}

pub struct Chain {
    pub base: Base,
    blockhash: Hash,
    sent: HashSet<Signature>,
    pub compute: BTreeMap<String, CuStat>,
    pub transactions: u64,
    pub failed_transactions: u64,
    /// Resends after the test bank's AccountInUse lock race (reported)
    pub lock_retries: u64,
    /// Program id -> short name, for decoding logs
    names: Vec<(Pubkey, &'static str)>,
}

impl Chain {
    pub fn new(base: Base) -> Chain {
        let blockhash = base.ctx.last_blockhash;
        let names = vec![
            (perps_core::ID, "perps_core"),
            (mm_registry::ID, "mm_registry"),
            (orderbook::ID, "orderbook"),
            (spl_token::ID, "spl_token"),
            (solana_sdk::system_program::ID, "system"),
            (solana_sdk::compute_budget::ID, "compute_budget"),
        ];
        Chain { base, blockhash, sent: HashSet::new(), compute: BTreeMap::new(), transactions: 0, failed_transactions: 0, lock_retries: 0, names }
    }

    fn program_name(&self, id: &str) -> String {
        self.names.iter().find(|(k, _)| k.to_string() == id).map(|(_, n)| n.to_string()).unwrap_or_else(|| id.to_string())
    }

    /// Sign `ixs` with the context payer plus `signers`, execute, and record
    /// the compute units under `label`. A transport failure (the bank task
    /// died, e.g. a native-mode program panic) is fatal and panics here.
    pub async fn exec(&mut self, label: &str, ixs: &[Instruction], signers: &[&Keypair]) -> TxOutcome {
        let payer = self.base.ctx.payer.insecure_clone();
        let mut all: Vec<&Keypair> = vec![&payer];
        all.extend_from_slice(signers);
        let latest = self.base.ctx.banks_client.get_latest_blockhash().await.expect("bank reachable");
        if latest != self.blockhash {
            self.blockhash = latest;
            self.sent.clear();
        }
        let mut tx = Transaction::new_signed_with_payer(ixs, Some(&payer.pubkey()), &all, self.blockhash);
        if self.sent.contains(&tx.signatures[0]) {
            // Identical to a transaction already sent with this blockhash
            self.blockhash = self.base.ctx.banks_client.get_new_latest_blockhash(&self.blockhash).await.expect("new blockhash");
            self.sent.clear();
            tx = Transaction::new_signed_with_payer(ixs, Some(&payer.pubkey()), &all, self.blockhash);
        }
        self.sent.insert(tx.signatures[0]);
        let mut out = self
            .base
            .ctx
            .banks_client
            .process_transaction_with_metadata(tx)
            .await
            .unwrap_or_else(|e| panic!("transport error in {}: {:?} (a native program may have panicked)", label, e));
        // AccountInUse is the test bank's account-lock race, not an outcome of
        // the programs: the transaction was never executed (locks are taken
        // before it runs), so it is resent on a fresh blockhash (seen once,
        // under load, in a 2-day adversarial run: a donate right after a mint_to)
        for _ in 0..5 {
            if !matches!(out.result, Err(TransactionError::AccountInUse)) {
                break;
            }
            self.blockhash = self.base.ctx.banks_client.get_new_latest_blockhash(&self.blockhash).await.expect("new blockhash");
            self.sent.clear();
            let retry = Transaction::new_signed_with_payer(ixs, Some(&payer.pubkey()), &all, self.blockhash);
            self.sent.insert(retry.signatures[0]);
            self.lock_retries += 1;
            out = self
                .base
                .ctx
                .banks_client
                .process_transaction_with_metadata(retry)
                .await
                .unwrap_or_else(|e| panic!("transport error in {}: {:?} (a native program may have panicked)", label, e));
        }
        let (units, logs) = out.metadata.map(|m| (m.compute_units_consumed, m.log_messages)).unwrap_or((0, Vec::new()));
        self.transactions += 1;
        let stat = self.compute.entry(label.to_string()).or_default();
        let result = match out.result {
            Ok(()) => {
                stat.count += 1;
                stat.total += units;
                stat.max = stat.max.max(units);
                Ok(())
            }
            Err(e) => {
                self.failed_transactions += 1;
                Err(self.decode(&e, &logs))
            }
        };
        TxOutcome { result, logs }
    }

    /// Name the failure from the logs: the program on top of the invoke stack
    /// when the first Anchor error (or panic) was logged.
    fn decode(&self, err: &TransactionError, logs: &[String]) -> TxFailure {
        let raw = format!("{:?}", err);
        let mut stack: Vec<String> = Vec::new();
        let mut failed_program: Option<String> = None;
        for line in logs {
            if let Some(rest) = line.strip_prefix("Program ") {
                let mut words = rest.split_whitespace();
                let id = words.next().unwrap_or("");
                match words.next() {
                    Some("invoke") => stack.push(self.program_name(id)),
                    Some("success") => {
                        stack.pop();
                    }
                    Some("failed:") => {
                        if failed_program.is_none() {
                            failed_program = Some(self.program_name(id));
                        }
                        stack.pop();
                    }
                    _ => {}
                }
            }
            let top = stack.last().cloned().unwrap_or_else(|| "runtime".into());
            if line.contains("panicked at") {
                return TxFailure { program: top, code: "Panic".into(), raw, panic: true, detail: line.clone() };
            }
            if let Some(code) = anchor_error_name(line) {
                return TxFailure { program: top, code, raw, panic: false, detail: line.clone() };
            }
        }
        let (program, code) = match err {
            TransactionError::InstructionError(_, ie) => {
                let program = failed_program.unwrap_or_else(|| "runtime".into());
                let code = match ie {
                    InstructionError::Custom(c) => format!("Custom({})", c),
                    other => format!("{:?}", other),
                };
                (program, code)
            }
            other => ("runtime".into(), format!("{:?}", other)),
        };
        let panic = code.contains("ProgramFailedToComplete");
        let detail = logs.iter().rev().find(|l| l.contains("failed") || l.contains("Error")).cloned().unwrap_or_default();
        TxFailure { program, code, raw, panic, detail }
    }

    pub fn base(&mut self) -> &mut Base {
        &mut self.base
    }
}

/// `Error Code: Name.` from an Anchor error log line. Skips the
/// `Custom(6002)` form a caller logs when it propagates a CPI error.
fn anchor_error_name(line: &str) -> Option<String> {
    let start = line.find("Error Code: ")? + "Error Code: ".len();
    let rest = &line[start..];
    let end = rest.find('.')?;
    let name = &rest[..end];
    if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        Some(name.to_string())
    } else {
        None
    }
}

/// The sim's pool parameters: production defaults (docs/lp-pool.md §15.1)
/// with a one-day withdrawal delay, so the end-of-run unwind fits (§16.7).
/// The harness applies them before the protocol's $1M seed, which goes in
/// with `initialize_market` and `initialize_pool` at t0 (D11).
pub fn pool_params() -> PoolParams {
    PoolParams { withdrawal_delay_secs: 86_400, ..PoolParams::production() }
}

/// Anchor events of type `T` a transaction emitted: "Program data: <base64>"
/// on SBF, and the same line as a program log in native mode
/// (quiet_events.rs), CPIs included, in emission order.
pub fn events<T: anchor_lang::Event>(logs: &[String]) -> Vec<T> {
    use anchor_lang::__private::base64;
    let mut out = Vec::new();
    for line in logs {
        let Some(b64) = line.strip_prefix("Program data: ").or_else(|| line.strip_prefix("Program log: Program data: ")) else { continue };
        let Ok(data) = base64::decode(b64.trim()) else { continue };
        if data.len() >= 8 && data[..8] == T::discriminator() {
            if let Ok(e) = T::try_from_slice(&data[8..]) {
                out.push(e);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{anchor_error_name, events, pool_params};
    use anchor_lang::{AnchorSerialize, Discriminator};
    use perps_core::events::FundingUpdated;

    #[test]
    fn reads_events_in_both_modes() {
        use anchor_lang::__private::base64;
        let ev = FundingUpdated { periods: 2, periods_dropped: 0, rate: 50, step: 4_000, cumulative_funding: 8_000, carry_secs: 17 };
        let mut data = FundingUpdated::discriminator().to_vec();
        data.extend(ev.try_to_vec().unwrap());
        let b64 = base64::encode(&data);
        let logs = vec![
            "Program log: Instruction: UpdateFunding".to_string(),
            format!("Program data: {}", b64),
            format!("Program log: Program data: {}", b64),
        ];
        let got = events::<FundingUpdated>(&logs);
        assert_eq!(got.len(), 2);
        assert_eq!((got[0].periods, got[1].carry_secs), (2, 17));
    }

    /// Production defaults with the one-day delay must pass the engine's own validation.
    #[test]
    fn sim_pool_params_validate() {
        let m = perps_core::state::Market { maintenance_margin_ratio: 500, initial_margin_ratio: 1_000, taker_fee: 5, ..Default::default() };
        assert!(pool_params().validate(&m));
        assert_eq!(pool_params().withdrawal_delay_secs, 86_400);
    }

    #[test]
    fn reads_anchor_error_names() {
        let l = "Program log: AnchorError thrown in programs/perps-core/src/instructions/venue_fill.rs:249. Error Code: InsufficientCollateral. Error Number: 6002. Error Message: Insufficient collateral for position.";
        assert_eq!(anchor_error_name(l).as_deref(), Some("InsufficientCollateral"));
        let l = "Program log: ProgramError occurred. Error Code: Custom(6002). Error Number: 6002.";
        assert_eq!(anchor_error_name(l), None);
    }
}
