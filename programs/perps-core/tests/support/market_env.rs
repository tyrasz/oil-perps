//! Shared harness for programs that sit beside perps-core (orderbook, vamm):
//! a perps-core config + OIL market whose admin is the upgrade authority.
//! Included with `#[path]` from those crates' integration tests.

use anchor_lang::{InstructionData, ToAccountMetas};
use perps_core::instructions::{InitializeMarketParams, ProtocolRolesParams};
use pyth_sdk_solana::state::{AccountType, PriceAccount, PriceInfo, PriceStatus, MAGIC, VERSION_2};
pub use solana_program_test::{BanksClientError, ProgramTest, ProgramTestContext};
pub use solana_sdk::{
    account::Account,
    bpf_loader_upgradeable::{self, UpgradeableLoaderState},
    instruction::{Instruction, InstructionError},
    program_pack::Pack,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    system_instruction, system_program, sysvar,
    transaction::{Transaction, TransactionError},
};

pub const PYTH_PROGRAM: Pubkey = Pubkey::new_from_array([0x50; 32]);

pub fn perps_process(program_id: &Pubkey, accounts: &[solana_sdk::account_info::AccountInfo], data: &[u8]) -> solana_sdk::entrypoint::ProgramResult {
    let accounts = Box::leak(Box::new(accounts.to_vec()));
    perps_core::entry(program_id, accounts, data)
}

pub fn pda(seeds: &[&[u8]], program: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(seeds, program).0
}

pub fn config_pda() -> Pubkey {
    pda(&[b"config"], &perps_core::ID)
}

pub fn ix(program_id: Pubkey, accounts: impl ToAccountMetas, data: impl InstructionData) -> Instruction {
    Instruction { program_id, accounts: accounts.to_account_metas(None), data: data.data() }
}

pub async fn send(ctx: &mut ProgramTestContext, ixs: &[Instruction], signers: &[&Keypair]) -> Result<(), BanksClientError> {
    let payer = ctx.payer.insecure_clone();
    let mut all: Vec<&Keypair> = vec![&payer];
    all.extend_from_slice(signers);
    let blockhash = ctx.get_new_latest_blockhash().await.unwrap();
    ctx.banks_client.process_transaction(Transaction::new_signed_with_payer(ixs, Some(&payer.pubkey()), &all, blockhash)).await
}

pub fn custom_error(result: Result<(), BanksClientError>) -> u32 {
    match result.expect_err("transaction should fail").unwrap() {
        TransactionError::InstructionError(_, InstructionError::Custom(c)) => c,
        other => panic!("unexpected error: {:?}", other),
    }
}

pub struct Base {
    pub ctx: ProgramTestContext,
    pub admin: Keypair,
    pub market: Pubkey,
}

/// `pt` must already have the program under test added.
pub async fn start(mut pt: ProgramTest) -> Base {
    let admin = Keypair::new();
    let oracle = Pubkey::new_unique();
    pt.add_program("perps_core", perps_core::ID, solana_program_test::processor!(perps_process));
    let mut feed = PriceAccount { magic: MAGIC, ver: VERSION_2, atype: AccountType::Price as u32, expo: -8, ..Default::default() };
    feed.agg = PriceInfo { price: 7_500_000_000, conf: 0, status: PriceStatus::Trading, ..Default::default() };
    pt.add_account(oracle, Account { lamports: 1_000_000_000, data: bytemuck::bytes_of(&feed).to_vec(), owner: PYTH_PROGRAM, executable: false, rent_epoch: 0 });
    let state = UpgradeableLoaderState::ProgramData { slot: 0, upgrade_authority_address: Some(admin.pubkey()) };
    let program_data = pda(&[perps_core::ID.as_ref()], &bpf_loader_upgradeable::id());
    pt.add_account(program_data, Account { lamports: 1_000_000_000, data: bincode::serialize(&state).unwrap(), owner: bpf_loader_upgradeable::id(), executable: false, rent_epoch: 0 });
    let mut ctx = pt.start_with_context().await;
    let payer = ctx.payer.insecure_clone();
    fund(&mut ctx, &admin.pubkey()).await;

    let mint = Keypair::new();
    let rent = ctx.banks_client.get_rent().await.unwrap();
    send(&mut ctx, &[
        system_instruction::create_account(&payer.pubkey(), &mint.pubkey(), rent.minimum_balance(spl_token::state::Mint::LEN), spl_token::state::Mint::LEN as u64, &spl_token::ID),
        spl_token::instruction::initialize_mint(&spl_token::ID, &mint.pubkey(), &payer.pubkey(), None, 6).unwrap(),
    ], &[&mint]).await.unwrap();
    let mint = mint.pubkey();

    send(&mut ctx, &[ix(perps_core::ID, perps_core::accounts::InitializeConfig {
        upgrade_authority: admin.pubkey(), config: config_pda(), program_data, system_program: system_program::ID,
    }, perps_core::instruction::InitializeConfig { params: ProtocolRolesParams { guardian: admin.pubkey(), oracle_program: PYTH_PROGRAM } })], &[&admin]).await.unwrap();
    let market = pda(&[b"market", mint.as_ref(), b"OIL"], &perps_core::ID);
    let params = InitializeMarketParams {
        commodity: "OIL".into(), max_leverage: 20_000, maintenance_margin_ratio: 500, initial_margin_ratio: 1_000,
        taker_fee: 5, maker_fee: 2, liquidation_fee: 100, max_open_interest: 1_000_000_000_000, funding_interval: 3_600,
    };
    send(&mut ctx, &[ix(perps_core::ID, perps_core::accounts::InitializeMarket {
        authority: admin.pubkey(), config: config_pda(), market,
        vault: pda(&[b"vault", market.as_ref()], &perps_core::ID),
        vault_token_account: pda(&[b"vault_token", market.as_ref()], &perps_core::ID),
        collateral_mint: mint, pyth_price_feed: oracle, system_program: system_program::ID, token_program: spl_token::ID, rent: sysvar::rent::ID,
    }, perps_core::instruction::InitializeMarket { params })], &[&admin]).await.unwrap();
    Base { ctx, admin, market }
}

pub async fn fund(ctx: &mut ProgramTestContext, who: &Pubkey) {
    let payer = ctx.payer.insecure_clone();
    send(ctx, &[system_instruction::transfer(&payer.pubkey(), who, 10_000_000_000)], &[]).await.unwrap();
}

/// Hand the perps-core admin role from `base.admin` to a new key, returning it.
pub async fn rotate_admin(base: &mut Base) -> Keypair {
    let new_admin = Keypair::new();
    fund(&mut base.ctx, &new_admin.pubkey()).await;
    let old = base.admin.insecure_clone();
    send(&mut base.ctx, &[ix(perps_core::ID, perps_core::accounts::AdminOnly { admin: old.pubkey(), config: config_pda() },
        perps_core::instruction::ProposeAdmin { new_admin: new_admin.pubkey() })], &[&old]).await.unwrap();
    send(&mut base.ctx, &[ix(perps_core::ID, perps_core::accounts::AcceptAdmin { new_admin: new_admin.pubkey(), config: config_pda() },
        perps_core::instruction::AcceptAdmin {})], &[&new_admin]).await.unwrap();
    new_admin
}
