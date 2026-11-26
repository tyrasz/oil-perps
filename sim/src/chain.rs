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
use solana_hash::Hash;
use solana_sdk::instruction::InstructionError;
use solana_signature::Signature;
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
                    // Natively there is no "Error Code: <Name>." line to read (D48)
                    InstructionError::Custom(c) if !crate::support::is_sbf() => {
                        native_error_name(&program, *c).unwrap_or_else(|| format!("Custom({})", c))
                    }
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

/// Native runs only: an Anchor 1.2 program run natively logs no `msg!` lines,
/// so no "Error Code: <Name>." line names its error (docs/lp-pool.md D48).
/// Name the custom code the way the SBF log would: from the failing program's
/// `#[error_code]` enum (variants numbered from 6000 in source order, read
/// from the program's own source so the table cannot drift), or below 6000
/// from Anchor's framework codes the programs' account checks raise.
fn native_error_name(program: &str, code: u32) -> Option<String> {
    use std::sync::LazyLock;
    static PERPS: LazyLock<Vec<&'static str>> = LazyLock::new(|| error_variants(include_str!("../../programs/perps-core/src/errors.rs")));
    static MM: LazyLock<Vec<&'static str>> = LazyLock::new(|| error_variants(include_str!("../../programs/mm-registry/src/errors.rs")));
    static BOOK: LazyLock<Vec<&'static str>> = LazyLock::new(|| error_variants(include_str!("../../programs/orderbook/src/lib.rs")));
    let names: &[&str] = match program {
        "perps_core" => &PERPS,
        "mm_registry" => &MM,
        "orderbook" => &BOOK,
        _ => return None,
    };
    if code < anchor_lang::error::ERROR_CODE_OFFSET {
        use anchor_lang::error::ErrorCode as E;
        let framework = [
            E::InstructionMissing, E::InstructionFallbackNotFound, E::InstructionDidNotDeserialize, E::InstructionDidNotSerialize,
            E::ConstraintMut, E::ConstraintHasOne, E::ConstraintSigner, E::ConstraintRaw, E::ConstraintOwner,
            E::ConstraintRentExempt, E::ConstraintSeeds, E::ConstraintExecutable, E::ConstraintState, E::ConstraintAssociated,
            E::ConstraintAssociatedInit, E::ConstraintClose, E::ConstraintAddress, E::ConstraintZero, E::ConstraintTokenMint,
            E::ConstraintTokenOwner, E::ConstraintMintMintAuthority, E::ConstraintMintFreezeAuthority, E::ConstraintMintDecimals,
            E::ConstraintSpace, E::ConstraintAccountIsNone, E::ConstraintTokenTokenProgram, E::ConstraintMintTokenProgram,
            E::ConstraintAssociatedTokenTokenProgram, E::ConstraintDuplicateMutableAccount,
            E::RequireViolated, E::RequireEqViolated, E::RequireKeysEqViolated, E::RequireNeqViolated, E::RequireKeysNeqViolated,
            E::RequireGtViolated, E::RequireGteViolated,
            E::AccountDiscriminatorAlreadySet, E::AccountDiscriminatorNotFound, E::AccountDiscriminatorMismatch,
            E::AccountDidNotDeserialize, E::AccountDidNotSerialize, E::AccountNotEnoughKeys, E::AccountNotMutable,
            E::AccountOwnedByWrongProgram, E::InvalidProgramId, E::InvalidProgramExecutable, E::AccountNotSigner,
            E::AccountNotSystemOwned, E::AccountNotInitialized, E::AccountNotProgramData, E::AccountNotAssociatedTokenAccount,
            E::AccountSysvarMismatch, E::AccountReallocExceedsLimit, E::AccountDuplicateReallocs,
            E::DeclaredProgramIdMismatch, E::TryingToInitPayerAsProgramAccount, E::InvalidNumericConversion,
        ];
        return framework.iter().find(|e| u32::from(**e) == code).map(|e| e.name());
    }
    names.get((code - anchor_lang::error::ERROR_CODE_OFFSET) as usize).map(|n| n.to_string())
}

/// The variant names of the `#[error_code]` enum in `src`, in order. Panics on
/// anything it does not expect (an explicit discriminant, a second enum), so
/// a change to an error enum's shape fails the unit test instead of
/// misnaming codes.
fn error_variants(src: &'static str) -> Vec<&'static str> {
    let mut parts = src.split("#[error_code]");
    let body = parts.next().and(parts.next()).expect("an #[error_code] enum");
    assert!(parts.next().is_none(), "one #[error_code] enum per file");
    let open = body.find('{').expect("the enum body");
    let mut names = Vec::new();
    for line in body[open + 1..].lines() {
        let line = line.split("//").next().unwrap().trim();
        if line == "}" {
            return names;
        }
        if line.is_empty() || (line.starts_with("#[") && line.ends_with(")]")) {
            continue;
        }
        let name = line.strip_suffix(',').unwrap_or(line);
        assert!(name.chars().all(|c| c.is_ascii_alphanumeric()) && name.starts_with(|c: char| c.is_ascii_uppercase()), "unexpected line in an error enum: {:?}", line);
        names.push(name);
    }
    panic!("unterminated error enum")
}

/// The sim's pool parameters: production defaults (docs/lp-pool.md §15.1)
/// with a one-day withdrawal delay, so the end-of-run unwind fits (§16.7).
/// The harness applies them before the protocol's $1M seed, which goes in
/// with `initialize_market` and `initialize_pool` at t0 (D11).
pub fn pool_params() -> PoolParams {
    PoolParams { withdrawal_delay_secs: 86_400, ..PoolParams::production() }
}

/// Anchor events of type `T` a transaction emitted: "Program data: <base64>"
/// on SBF, and "Program log: Program data: <base64>" in native mode
/// (perps-core's `emit_event!`; natively mm-registry's and orderbook's events
/// are lost, D48), CPIs included, in emission order.
#[allow(deprecated)] // base64::decode, as the harness uses it
pub fn events<T: anchor_lang::Event>(logs: &[String]) -> Vec<T> {
    use anchor_lang::__private::base64;
    let mut out = Vec::new();
    for line in logs {
        let Some(b64) = line.strip_prefix("Program data: ").or_else(|| line.strip_prefix("Program log: Program data: ")) else { continue };
        let Ok(data) = base64::decode(b64.trim()) else { continue };
        if data.len() >= 8 && &data[..8] == T::DISCRIMINATOR {
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
    use perps_core::events::FundingUpdated;

    #[test]
    #[allow(deprecated)] // base64::encode
    fn reads_events_in_both_modes() {
        use anchor_lang::__private::base64;
        let ev = FundingUpdated { periods: 2, periods_dropped: 0, rate: 50, step: 4_000, cumulative_funding: 8_000, carry_secs: 17 };
        // The discriminator and the borsh body, as emit! writes them
        let data = anchor_lang::Event::data(&ev);
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

    /// The native error names are the ones the SBF log carries: every
    /// parsed variant, at its code, is the enum's own `name()`.
    #[test]
    fn native_error_names_match_the_enums() {
        // SAFETY: the enums are fieldless `#[repr(u32)]` (Anchor's
        // #[error_code], size asserted below) with discriminants 0..n, and
        // `i` stays below the parsed count, which is n: error_variants takes
        // only bare variant names between the enum's braces and panics on
        // anything else (a discriminant, a field, a stray line)
        fn check<E: Copy>(program: &str, src: &'static str, name: impl Fn(E) -> String) {
            let names = super::error_variants(src);
            assert!(!names.is_empty());
            for (i, n) in names.iter().enumerate() {
                let e: E = unsafe { std::mem::transmute_copy(&(i as u32)) };
                assert_eq!(&name(e), n);
                assert_eq!(super::native_error_name(program, 6_000 + i as u32).as_deref(), Some(*n));
            }
            assert_eq!(super::native_error_name(program, 6_000 + names.len() as u32), None);
        }
        assert_eq!(std::mem::size_of::<perps_core::errors::PerpsError>(), 4);
        assert_eq!(std::mem::size_of::<mm_registry::errors::MmRegistryError>(), 4);
        assert_eq!(std::mem::size_of::<orderbook::ErrorCode>(), 4);
        check::<perps_core::errors::PerpsError>("perps_core", include_str!("../../programs/perps-core/src/errors.rs"), |e| e.name());
        check::<mm_registry::errors::MmRegistryError>("mm_registry", include_str!("../../programs/mm-registry/src/errors.rs"), |e| e.name());
        check::<orderbook::ErrorCode>("orderbook", include_str!("../../programs/orderbook/src/lib.rs"), |e| e.name());
        // The codes a native run showed unnamed, and the framework's
        assert_eq!(super::native_error_name("perps_core", 6_001).as_deref(), Some("ExcessiveLeverage"));
        assert_eq!(super::native_error_name("perps_core", 6_092).as_deref(), Some("ProfitCloseQuiet"));
        assert_eq!(super::native_error_name("mm_registry", 3_012).as_deref(), Some("AccountNotInitialized"));
        assert_eq!(super::native_error_name("perps_core", 2_040).as_deref(), Some("ConstraintDuplicateMutableAccount"));
        assert_eq!(super::native_error_name("spl_token", 6_001), None);
        assert_eq!(super::native_error_name("spl_token", 3_012), None);
    }

    #[test]
    fn reads_anchor_error_names() {
        let l = "Program log: AnchorError thrown in programs/perps-core/src/instructions/venue_fill.rs:249. Error Code: InsufficientCollateral. Error Number: 6002. Error Message: Insufficient collateral for position.";
        assert_eq!(anchor_error_name(l).as_deref(), Some("InsufficientCollateral"));
        let l = "Program log: ProgramError occurred. Error Code: Custom(6002). Error Number: 6002.";
        assert_eq!(anchor_error_name(l), None);
    }
}
