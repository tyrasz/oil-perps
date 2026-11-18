//! End-to-end checks for the LP vault, run in-process with solana-program-test.

use anchor_lang::{InstructionData, ToAccountMetas};
use prop_amm::instructions::{ClosePositionParams, InitializeVaultParams, OpenPositionParams};
use prop_amm::PropAmmError;
use pyth_sdk_solana::state::{AccountType, PriceAccount, PriceInfo, PriceStatus, MAGIC, VERSION_2};
use solana_program_test::{processor, BanksClientError, ProgramTest, ProgramTestContext};
use solana_sdk::{
    account::Account,
    clock::Clock,
    instruction::{Instruction, InstructionError},
    program_pack::Pack,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    system_instruction, system_program, sysvar,
    transaction::{Transaction, TransactionError},
};

const USDC: u64 = 1_000_000;

fn process(program_id: &Pubkey, accounts: &[solana_sdk::account_info::AccountInfo], data: &[u8]) -> solana_sdk::entrypoint::ProgramResult {
    let accounts = Box::leak(Box::new(accounts.to_vec()));
    prop_amm::entry(program_id, accounts, data)
}

fn pyth_account(price: i64, timestamp: i64) -> Account {
    let mut acct = PriceAccount { magic: MAGIC, ver: VERSION_2, atype: AccountType::Price as u32, expo: -8, timestamp, ..Default::default() };
    acct.agg = PriceInfo { price, conf: 0, status: PriceStatus::Trading, ..Default::default() };
    Account { lamports: 1_000_000_000, data: bytemuck::bytes_of(&acct).to_vec(), owner: Pubkey::new_unique(), executable: false, rent_epoch: 0 }
}

async fn send(ctx: &mut ProgramTestContext, ixs: &[Instruction], signers: &[&Keypair]) -> Result<(), BanksClientError> {
    let payer = ctx.payer.insecure_clone();
    let mut all: Vec<&Keypair> = vec![&payer];
    all.extend_from_slice(signers);
    let blockhash = ctx.get_new_latest_blockhash().await.unwrap();
    ctx.banks_client.process_transaction(Transaction::new_signed_with_payer(ixs, Some(&payer.pubkey()), &all, blockhash)).await
}

async fn balance(ctx: &mut ProgramTestContext, account: Pubkey) -> u64 {
    let acct = ctx.banks_client.get_account(account).await.unwrap().unwrap();
    spl_token::state::Account::unpack(&acct.data).unwrap().amount
}

fn custom_error(result: Result<(), BanksClientError>) -> u32 {
    match result.expect_err("transaction should fail").unwrap() {
        TransactionError::InstructionError(_, InstructionError::Custom(c)) => c,
        other => panic!("unexpected error: {:?}", other),
    }
}

#[tokio::test]
async fn only_the_vault_operator_can_trade_against_lp_funds() {
    let now = 1_794_416_400;
    let oracle = Pubkey::new_unique();
    let mut pt = ProgramTest::new("prop_amm", prop_amm::ID, processor!(process));
    pt.add_account(oracle, pyth_account(7_500_000_000, now));
    let mut ctx = pt.start_with_context().await;
    let mut clock: Clock = ctx.banks_client.get_sysvar().await.unwrap();
    clock.unix_timestamp = now;
    ctx.set_sysvar(&clock);

    let payer = ctx.payer.insecure_clone();
    let operator = Keypair::new();
    let attacker = Keypair::new();
    let mint = Keypair::new();
    for who in [&operator, &attacker] {
        send(&mut ctx, &[system_instruction::transfer(&payer.pubkey(), &who.pubkey(), 5_000_000_000)], &[]).await.unwrap();
    }
    let rent = ctx.banks_client.get_rent().await.unwrap();
    send(&mut ctx, &[
        system_instruction::create_account(&payer.pubkey(), &mint.pubkey(), rent.minimum_balance(spl_token::state::Mint::LEN), spl_token::state::Mint::LEN as u64, &spl_token::ID),
        spl_token::instruction::initialize_mint(&spl_token::ID, &mint.pubkey(), &payer.pubkey(), None, 6).unwrap(),
    ], &[&mint]).await.unwrap();

    let attacker_tokens = Keypair::new();
    let len = spl_token::state::Account::LEN;
    send(&mut ctx, &[
        system_instruction::create_account(&payer.pubkey(), &attacker_tokens.pubkey(), rent.minimum_balance(len), len as u64, &spl_token::ID),
        spl_token::instruction::initialize_account(&spl_token::ID, &attacker_tokens.pubkey(), &mint.pubkey(), &attacker.pubkey()).unwrap(),
    ], &[&attacker_tokens]).await.unwrap();

    let (vault, _) = Pubkey::find_program_address(&[b"lp_vault", mint.pubkey().as_ref()], &prop_amm::ID);
    let (vault_token, _) = Pubkey::find_program_address(&[b"vault_token", vault.as_ref()], &prop_amm::ID);
    let init = Instruction {
        program_id: prop_amm::ID,
        accounts: prop_amm::accounts::InitializeVault {
            authority: operator.pubkey(),
            vault,
            vault_token_account: vault_token,
            collateral_mint: mint.pubkey(),
            market: Pubkey::new_unique(),
            pyth_price_feed: oracle,
            system_program: system_program::ID,
            token_program: spl_token::ID,
            rent: sysvar::rent::ID,
        }
        .to_account_metas(None),
        data: prop_amm::instruction::InitializeVault {
            params: InitializeVaultParams {
                max_exposure: 1_000_000 * USDC,
                max_utilization: 8_000,
                max_position_size: 100_000 * USDC,
                base_spread: 5,
                max_skew_spread: 50,
                trading_fee: 5,
                lp_fee_share: 7_000,
                withdrawal_delay: 86_400,
            },
        }
        .data(),
    };
    send(&mut ctx, &[init], &[&operator]).await.unwrap();
    // $1M of LP money in the vault
    send(&mut ctx, &[spl_token::instruction::mint_to(&spl_token::ID, &mint.pubkey(), &vault_token, &payer.pubkey(), &[], 1_000_000 * USDC).unwrap()], &[]).await.unwrap();

    let trade_accounts = |trader: Pubkey| (vault, vault_token, attacker_tokens.pubkey(), oracle, trader);
    let (v, vt, tt, o, t) = trade_accounts(attacker.pubkey());

    // Closing a position that never existed, with entry price 0, would pay ~$974k
    let close = Instruction {
        program_id: prop_amm::ID,
        accounts: prop_amm::accounts::ClosePositionAmm { trader: t, vault: v, vault_token_account: vt, trader_token_account: tt, pyth_price_feed: o, token_program: spl_token::ID }.to_account_metas(None),
        data: prop_amm::instruction::ClosePosition { params: ClosePositionParams { size: 13_000 * USDC, is_long: true, entry_price: 0, min_price: 0, max_price: 0 } }.data(),
    };
    let unauthorized: u32 = PropAmmError::Unauthorized.into();
    assert_eq!(custom_error(send(&mut ctx, &[close], &[&attacker]).await), unauthorized);

    let open = Instruction {
        program_id: prop_amm::ID,
        accounts: prop_amm::accounts::OpenPositionAmm { trader: t, vault: v, vault_token_account: vt, trader_token_account: tt, pyth_price_feed: o, token_program: spl_token::ID }.to_account_metas(None),
        data: prop_amm::instruction::OpenPosition { params: OpenPositionParams { size: USDC, is_long: true, max_price: u64::MAX, min_price: 0 } }.data(),
    };
    assert_eq!(custom_error(send(&mut ctx, &[open], &[&attacker]).await), unauthorized);

    assert_eq!(balance(&mut ctx, vault_token).await, 1_000_000 * USDC);
    assert_eq!(balance(&mut ctx, attacker_tokens.pubkey()).await, 0);
}
