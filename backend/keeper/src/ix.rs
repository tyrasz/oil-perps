//! perps-core instructions the keeper sends. Account order follows the
//! `#[derive(Accounts)]` structs in programs/perps-core/src/instructions
//! (and target/idl/perps_core.json once generated).

use crate::state::anchor_discriminator;
use crate::{get_lp_pool_pda, get_user_account_pda, get_vault_pda, get_vault_token_pda};
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    hash::Hash,
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::{Keypair, Signature, Signer},
    transaction::Transaction,
};

pub type KeeperResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// `PerpsError::PoolCashInsufficient` (Anchor custom error 6057 = 0x17a9).
pub const ERR_POOL_CASH_INSUFFICIENT: u32 = 6057;
/// `PerpsError::NotLiquidatable` (6007 = 0x1777).
pub const ERR_NOT_LIQUIDATABLE: u32 = 6007;
/// `PerpsError::PositionAlreadyClosed` (6012 = 0x177c).
pub const ERR_POSITION_ALREADY_CLOSED: u32 = 6012;
/// `PerpsError::NothingToSettle` (6072) and `SettleWouldBankrupt` (6073).
pub const ERR_NOTHING_TO_SETTLE: u32 = 6072;
pub const ERR_SETTLE_WOULD_BANKRUPT: u32 = 6073;
/// `PerpsError::FundingNotDue` (6082 = 0x17c2).
pub const ERR_FUNDING_NOT_DUE: u32 = 6082;

/// A settle refused only because the position changed since the scan: the
/// owner closed it, the price moved it back to a profit, or the loss grew
/// past its collateral (the liquidator's job now).
pub fn is_settle_race(err: &str) -> bool {
    [ERR_POSITION_ALREADY_CLOSED, ERR_NOTHING_TO_SETTLE, ERR_SETTLE_WOULD_BANKRUPT].iter().any(|&c| has_program_error(err, c))
}

/// Whether an RPC error (simulation or confirmation) carries custom program
/// error `code`; matches both the "custom program error: 0x.." text and the
/// "Error Number: N" log line Anchor prints.
pub fn has_program_error(err: &str, code: u32) -> bool {
    err.contains(&format!("custom program error: {:#x}", code)) || err.contains(&format!("Error Number: {}.", code))
}

pub fn config_pda(program_id: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"config"], program_id).0
}

/// `update_funding()`: keeper, market, pyth.
pub fn update_funding(program_id: &Pubkey, keeper: &Pubkey, market: &Pubkey, pyth: &Pubkey) -> Instruction {
    Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new(*keeper, true),
            AccountMeta::new(*market, false),
            AccountMeta::new_readonly(*pyth, false),
        ],
        data: anchor_discriminator("global", "update_funding").to_vec(),
    }
}

/// `liquidate()` (§4.6): 11 accounts, `lp_pool` last.
#[allow(clippy::too_many_arguments)]
pub fn liquidate(
    program_id: &Pubkey,
    liquidator: &Pubkey,
    owner: &Pubkey,
    market: &Pubkey,
    position: &Pubkey,
    liquidator_token_account: &Pubkey,
    pyth: &Pubkey,
) -> Instruction {
    Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new(*liquidator, true),
            AccountMeta::new_readonly(*owner, false),
            AccountMeta::new(get_user_account_pda(program_id, owner, market).0, false),
            AccountMeta::new(*market, false),
            AccountMeta::new(*position, false),
            AccountMeta::new(get_vault_pda(program_id, market).0, false),
            AccountMeta::new(get_vault_token_pda(program_id, market).0, false),
            AccountMeta::new(*liquidator_token_account, false),
            AccountMeta::new_readonly(*pyth, false),
            AccountMeta::new_readonly(spl_token::id(), false),
            AccountMeta::new(get_lp_pool_pda(program_id, market).0, false),
        ],
        data: anchor_discriminator("global", "liquidate").to_vec(),
    }
}

/// `adl(mode, size)` (§7.1, `AdlPosition`).
#[allow(clippy::too_many_arguments)]
pub fn adl(program_id: &Pubkey, signer: &Pubkey, market: &Pubkey, position: &Pubkey, owner: &Pubkey, pyth: &Pubkey, mode: u8, size: u64) -> Instruction {
    let mut data = anchor_discriminator("global", "adl").to_vec();
    data.push(mode);
    data.extend_from_slice(&size.to_le_bytes());
    Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new_readonly(*signer, true),
            AccountMeta::new_readonly(config_pda(program_id), false),
            AccountMeta::new(*market, false),
            AccountMeta::new(get_lp_pool_pda(program_id, market).0, false),
            AccountMeta::new(*position, false),
            AccountMeta::new_readonly(*owner, false),
            AccountMeta::new(get_user_account_pda(program_id, owner, market).0, false),
            AccountMeta::new_readonly(*pyth, false),
        ],
        data,
    }
}

/// `settle_position_pnl()` (§6.6): the market is read-only.
pub fn settle_position_pnl(program_id: &Pubkey, signer: &Pubkey, market: &Pubkey, position: &Pubkey, pyth: &Pubkey) -> Instruction {
    Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new_readonly(*signer, true),
            AccountMeta::new_readonly(*market, false),
            AccountMeta::new(get_lp_pool_pda(program_id, market).0, false),
            AccountMeta::new(*position, false),
            AccountMeta::new_readonly(*pyth, false),
        ],
        data: anchor_discriminator("global", "settle_position_pnl").to_vec(),
    }
}

/// `close_settled_position()` (§10.5).
pub fn close_settled_position(program_id: &Pubkey, signer: &Pubkey, market: &Pubkey, position: &Pubkey, owner: &Pubkey) -> Instruction {
    Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new_readonly(*signer, true),
            AccountMeta::new(*market, false),
            AccountMeta::new(get_lp_pool_pda(program_id, market).0, false),
            AccountMeta::new(*position, false),
            AccountMeta::new_readonly(*owner, false),
            AccountMeta::new(get_user_account_pda(program_id, owner, market).0, false),
        ],
        data: anchor_discriminator("global", "close_settled_position").to_vec(),
    }
}

/// `sweep_insurance_excess()` (§10.1).
pub fn sweep_insurance_excess(program_id: &Pubkey, signer: &Pubkey, market: &Pubkey) -> Instruction {
    Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new_readonly(*signer, true),
            AccountMeta::new(*market, false),
            AccountMeta::new(get_lp_pool_pda(program_id, market).0, false),
        ],
        data: anchor_discriminator("global", "sweep_insurance_excess").to_vec(),
    }
}

/// `write_off_bad_debt()` (§7.4): the market is read-only.
pub fn write_off_bad_debt(program_id: &Pubkey, signer: &Pubkey, market: &Pubkey) -> Instruction {
    Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new_readonly(*signer, true),
            AccountMeta::new_readonly(*market, false),
            AccountMeta::new(get_lp_pool_pda(program_id, market).0, false),
        ],
        data: anchor_discriminator("global", "write_off_bad_debt").to_vec(),
    }
}

/// Sign with the keeper, send and confirm one instruction.
pub fn send(client: &RpcClient, keypair: &Keypair, ix: Instruction) -> KeeperResult<Signature> {
    let blockhash = client.get_latest_blockhash()?;
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&keypair.pubkey()), &[keypair], blockhash);
    Ok(client.send_and_confirm_transaction(&tx)?)
}

/// Sign each instruction as its own transaction (one blockhash), send them
/// all, then confirm them together. A burst of liquidations after a reopen gap
/// then lands within a slot or two instead of one confirmation round trip
/// (~0.5 s) each, during which owners close the positions first. Sending runs
/// the preflight simulation, so one the program would refuse (closed since
/// the scan) fails at once without landing. Results are in `ixs` order.
pub fn send_batch(client: &RpcClient, keypair: &Keypair, ixs: Vec<Instruction>, timeout: std::time::Duration) -> Vec<KeeperResult<Signature>> {
    let blockhash: Hash = match client.get_latest_blockhash() {
        Ok(b) => b,
        Err(e) => {
            let e = e.to_string();
            return ixs.iter().map(|_| Err(e.clone().into())).collect();
        }
    };
    let mut out: Vec<Option<KeeperResult<Signature>>> = Vec::with_capacity(ixs.len());
    let mut pending: Vec<(usize, Signature)> = Vec::new();
    for (i, ix) in ixs.into_iter().enumerate() {
        let tx = Transaction::new_signed_with_payer(&[ix], Some(&keypair.pubkey()), &[keypair], blockhash);
        match client.send_transaction(&tx) {
            Ok(sig) => {
                out.push(None);
                pending.push((i, sig));
            }
            Err(e) => out.push(Some(Err(e.into()))),
        }
    }
    let deadline = std::time::Instant::now() + timeout;
    while !pending.is_empty() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(300));
        let mut still = Vec::new();
        // getSignatureStatuses takes at most 256 signatures
        for chunk in pending.chunks(256) {
            let sigs: Vec<Signature> = chunk.iter().map(|p| p.1).collect();
            let Ok(statuses) = client.get_signature_statuses(&sigs) else {
                still.extend_from_slice(chunk);
                continue;
            };
            for (&(i, sig), status) in chunk.iter().zip(statuses.value) {
                match status {
                    Some(s) if s.err.is_some() => out[i] = Some(Err(format!("{}: {}", sig, s.err.unwrap()).into())),
                    Some(s) if s.satisfies_commitment(client.commitment()) => out[i] = Some(Ok(sig)),
                    _ => still.push((i, sig)),
                }
            }
        }
        pending = still;
    }
    for (i, sig) in pending {
        out[i] = Some(Err(format!("{} not confirmed within {:?}", sig, timeout).into()));
    }
    out.into_iter().map(|r| r.expect("every instruction has a result")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(ix: &Instruction) -> Vec<(bool, bool)> {
        ix.accounts.iter().map(|a| (a.is_signer, a.is_writable)).collect()
    }

    #[test]
    fn discriminators_match_anchor() {
        assert_eq!(anchor_discriminator("global", "liquidate"), [223, 179, 226, 125, 48, 46, 39, 74]);
        assert_eq!(anchor_discriminator("global", "update_funding"), [224, 66, 9, 70, 75, 81, 94, 88]);
        assert_eq!(anchor_discriminator("account", "Position"), [170, 188, 143, 228, 122, 64, 247, 208]);
    }

    #[test]
    fn liquidate_appends_the_pool() {
        let (program, k, o, m, p, t, f) =
            (Pubkey::new_unique(), Pubkey::new_unique(), Pubkey::new_unique(), Pubkey::new_unique(), Pubkey::new_unique(), Pubkey::new_unique(), Pubkey::new_unique());
        let ix = liquidate(&program, &k, &o, &m, &p, &t, &f);
        assert_eq!(ix.accounts.len(), 11);
        assert_eq!(ix.accounts[10].pubkey, get_lp_pool_pda(&program, &m).0);
        assert!(ix.accounts[10].is_writable);
        assert_eq!(ix.accounts[2].pubkey, get_user_account_pda(&program, &o, &m).0);
        assert_eq!(ix.accounts[9].pubkey, spl_token::id());
    }

    #[test]
    fn adl_encodes_mode_and_size() {
        let (program, s, m, p, o, f) = (Pubkey::new_unique(), Pubkey::new_unique(), Pubkey::new_unique(), Pubkey::new_unique(), Pubkey::new_unique(), Pubkey::new_unique());
        let ix = adl(&program, &s, &m, &p, &o, &f, 1, 0x0102_0304_0506_0708);
        assert_eq!(&ix.data[..8], anchor_discriminator("global", "adl"));
        assert_eq!(ix.data[8], 1);
        assert_eq!(&ix.data[9..], 0x0102_0304_0506_0708u64.to_le_bytes());
        let keys: Vec<Pubkey> = ix.accounts.iter().map(|a| a.pubkey).collect();
        assert_eq!(
            keys,
            vec![s, config_pda(&program), m, get_lp_pool_pda(&program, &m).0, p, o, get_user_account_pda(&program, &o, &m).0, f]
        );
        assert_eq!(
            names(&ix),
            vec![(true, false), (false, false), (false, true), (false, true), (false, true), (false, false), (false, true), (false, false)]
        );
    }

    #[test]
    fn pool_maintenance_instructions() {
        let (program, s, m, p, o, f) = (Pubkey::new_unique(), Pubkey::new_unique(), Pubkey::new_unique(), Pubkey::new_unique(), Pubkey::new_unique(), Pubkey::new_unique());
        let pool = get_lp_pool_pda(&program, &m).0;
        let ix = settle_position_pnl(&program, &s, &m, &p, &f);
        assert_eq!(ix.accounts.iter().map(|a| a.pubkey).collect::<Vec<_>>(), vec![s, m, pool, p, f]);
        assert_eq!(names(&ix), vec![(true, false), (false, false), (false, true), (false, true), (false, false)]);
        let ix = close_settled_position(&program, &s, &m, &p, &o);
        assert_eq!(
            ix.accounts.iter().map(|a| a.pubkey).collect::<Vec<_>>(),
            vec![s, m, pool, p, o, get_user_account_pda(&program, &o, &m).0]
        );
        assert_eq!(ix.data, anchor_discriminator("global", "close_settled_position"));
        let ix = sweep_insurance_excess(&program, &s, &m);
        assert_eq!(names(&ix), vec![(true, false), (false, true), (false, true)]);
        let ix = write_off_bad_debt(&program, &s, &m);
        assert_eq!(names(&ix), vec![(true, false), (false, false), (false, true)]);
        assert_eq!(ix.accounts[2].pubkey, pool);
    }

    #[test]
    fn recognises_program_errors() {
        assert!(has_program_error("Error processing Instruction 0: custom program error: 0x17a9", ERR_POOL_CASH_INSUFFICIENT));
        assert!(has_program_error("AnchorError ... Error Code: PoolCashInsufficient. Error Number: 6057. Error Message", ERR_POOL_CASH_INSUFFICIENT));
        assert!(!has_program_error("custom program error: 0x17aa", ERR_POOL_CASH_INSUFFICIENT));
        // ProfitCloseQuiet (6092)
        assert!(has_program_error("custom program error: 0x17cc", 6092));
        // A confirmed transaction's error as send_batch reports it, and a preflight refusal
        let landed = format!("sig: {}", solana_sdk::transaction::TransactionError::InstructionError(0, solana_sdk::instruction::InstructionError::Custom(ERR_POSITION_ALREADY_CLOSED)));
        assert!(has_program_error(&landed, ERR_POSITION_ALREADY_CLOSED));
        assert!(has_program_error("RPC response error -32002: Transaction simulation failed: Error processing Instruction 0: custom program error: 0x1777; 3 log messages", ERR_NOT_LIQUIDATABLE));
        // An LP's or a bot's crank landed first
        assert!(has_program_error("Error processing Instruction 0: custom program error: 0x17c2", ERR_FUNDING_NOT_DUE));
        // The owner closed the position between the scan and the settle
        assert!(is_settle_race("Error processing Instruction 0: custom program error: 0x177c"));
        assert!(is_settle_race("custom program error: 0x17b8") && is_settle_race("custom program error: 0x17b9"));
        assert!(!is_settle_race("custom program error: 0x178b")); // MarketClosed is not a race
    }
}
