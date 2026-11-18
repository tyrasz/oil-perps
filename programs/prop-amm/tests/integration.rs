//! End-to-end checks for the LP vault, run in-process with solana-program-test.

use anchor_lang::{InstructionData, ToAccountMetas};
use prop_amm::instructions::{ClosePositionParams, InitializeVaultParams, OpenPositionParams};
use prop_amm::PropAmmError;
use pyth_sdk_solana::state::{AccountType, PriceAccount, PriceInfo, PriceStatus, MAGIC, VERSION_2};
use solana_program_test::{processor, BanksClientError, ProgramTest, ProgramTestContext};
use solana_sdk::{
    account::Account,
    bpf_loader_upgradeable::{self, UpgradeableLoaderState},
    clock::Clock,
    instruction::{Instruction, InstructionError},
    program_pack::Pack,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    system_instruction, system_program, sysvar,
    transaction::{Transaction, TransactionError},
};

const USDC: u64 = 1_000_000;
const NOW: i64 = 1_794_416_400;
/// Stands in for the Pyth program: owner of the test price feed.
const PYTH_PROGRAM: Pubkey = Pubkey::new_from_array([0x50; 32]);

fn process(program_id: &Pubkey, accounts: &[solana_sdk::account_info::AccountInfo], data: &[u8]) -> solana_sdk::entrypoint::ProgramResult {
    let accounts = Box::leak(Box::new(accounts.to_vec()));
    prop_amm::entry(program_id, accounts, data)
}

fn pyth_account(price: i64, owner: Pubkey) -> Account {
    let mut acct = PriceAccount { magic: MAGIC, ver: VERSION_2, atype: AccountType::Price as u32, expo: -8, timestamp: NOW, ..Default::default() };
    acct.agg = PriceInfo { price, conf: 0, status: PriceStatus::Trading, ..Default::default() };
    Account { lamports: 1_000_000_000, data: bytemuck::bytes_of(&acct).to_vec(), owner, executable: false, rent_epoch: 0 }
}

fn config_pda() -> Pubkey {
    Pubkey::find_program_address(&[b"config"], &prop_amm::ID).0
}

fn program_data_pda() -> Pubkey {
    Pubkey::find_program_address(&[prop_amm::ID.as_ref()], &bpf_loader_upgradeable::id()).0
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

fn unauthorized() -> u32 {
    PropAmmError::Unauthorized.into()
}

struct Env {
    ctx: ProgramTestContext,
    /// Upgrade authority, and so the initial admin
    deployer: Keypair,
    attacker: Keypair,
    mint: Pubkey,
    oracle: Pubkey,
    fake_oracle: Pubkey,
    vault: Pubkey,
    vault_token: Pubkey,
    attacker_tokens: Pubkey,
}

impl Env {
    async fn new() -> Env {
        let deployer = Keypair::new();
        let oracle = Pubkey::new_unique();
        let fake_oracle = Pubkey::new_unique();
        let mut pt = ProgramTest::new("prop_amm", prop_amm::ID, processor!(process));
        pt.add_account(oracle, pyth_account(7_500_000_000, PYTH_PROGRAM));
        pt.add_account(fake_oracle, pyth_account(7_500_000_000, Pubkey::new_unique()));
        let state = UpgradeableLoaderState::ProgramData { slot: 0, upgrade_authority_address: Some(deployer.pubkey()) };
        pt.add_account(program_data_pda(), Account {
            lamports: 1_000_000_000,
            data: bincode::serialize(&state).unwrap(),
            owner: bpf_loader_upgradeable::id(),
            executable: false,
            rent_epoch: 0,
        });
        let mut ctx = pt.start_with_context().await;
        let mut clock: Clock = ctx.banks_client.get_sysvar().await.unwrap();
        clock.unix_timestamp = NOW;
        ctx.set_sysvar(&clock);

        let payer = ctx.payer.insecure_clone();
        let attacker = Keypair::new();
        let mint = Keypair::new();
        for who in [&deployer, &attacker] {
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
        Env { ctx, deployer, attacker, mint: mint.pubkey(), oracle, fake_oracle, vault, vault_token, attacker_tokens: attacker_tokens.pubkey() }
    }

    async fn exec(&mut self, accounts: impl ToAccountMetas, data: impl InstructionData, signer: &Keypair) -> Result<(), BanksClientError> {
        let ix = Instruction { program_id: prop_amm::ID, accounts: accounts.to_account_metas(None), data: data.data() };
        send(&mut self.ctx, &[ix], &[signer]).await
    }

    async fn init_config(&mut self, signer: &Keypair) -> Result<(), BanksClientError> {
        let accounts = prop_amm::accounts::InitializeConfig {
            upgrade_authority: signer.pubkey(),
            config: config_pda(),
            program_data: program_data_pda(),
            system_program: system_program::ID,
        };
        self.exec(accounts, prop_amm::instruction::InitializeConfig { oracle_program: PYTH_PROGRAM }, signer).await
    }

    async fn init_vault(&mut self, signer: &Keypair, oracle: Pubkey) -> Result<(), BanksClientError> {
        let accounts = prop_amm::accounts::InitializeVault {
            authority: signer.pubkey(),
            config: config_pda(),
            vault: self.vault,
            vault_token_account: self.vault_token,
            collateral_mint: self.mint,
            market: Pubkey::new_unique(),
            pyth_price_feed: oracle,
            system_program: system_program::ID,
            token_program: spl_token::ID,
            rent: sysvar::rent::ID,
        };
        let params = InitializeVaultParams {
            max_exposure: 1_000_000 * USDC,
            max_utilization: 8_000,
            max_position_size: 100_000 * USDC,
            base_spread: 5,
            max_skew_spread: 50,
            trading_fee: 5,
            lp_fee_share: 7_000,
            withdrawal_delay: 86_400,
        };
        self.exec(accounts, prop_amm::instruction::InitializeVault { params }, signer).await
    }

    /// Close a position that never existed: entry price 0 would pay ~$974k.
    async fn phantom_close(&mut self, trader: &Keypair) -> Result<(), BanksClientError> {
        let accounts = prop_amm::accounts::ClosePositionAmm {
            trader: trader.pubkey(),
            config: config_pda(),
            vault: self.vault,
            vault_token_account: self.vault_token,
            trader_token_account: self.attacker_tokens,
            pyth_price_feed: self.oracle,
            token_program: spl_token::ID,
        };
        let params = ClosePositionParams { size: 13_000 * USDC, is_long: true, entry_price: 0, min_price: 0, max_price: 0 };
        self.exec(accounts, prop_amm::instruction::ClosePosition { params }, trader).await
    }

    async fn open(&mut self, trader: &Keypair) -> Result<(), BanksClientError> {
        let accounts = prop_amm::accounts::OpenPositionAmm {
            trader: trader.pubkey(),
            config: config_pda(),
            vault: self.vault,
            vault_token_account: self.vault_token,
            trader_token_account: self.attacker_tokens,
            pyth_price_feed: self.oracle,
            token_program: spl_token::ID,
        };
        let params = OpenPositionParams { size: USDC, is_long: true, max_price: u64::MAX, min_price: 0 };
        self.exec(accounts, prop_amm::instruction::OpenPosition { params }, trader).await
    }

    /// Config and vault set up by the deployer, holding $1M of LP money.
    async fn with_funded_vault(mut self) -> Env {
        let deployer = self.deployer.insecure_clone();
        self.init_config(&deployer).await.unwrap();
        let oracle = self.oracle;
        self.init_vault(&deployer, oracle).await.unwrap();
        let payer = self.ctx.payer.insecure_clone();
        let ix = spl_token::instruction::mint_to(&spl_token::ID, &self.mint, &self.vault_token, &payer.pubkey(), &[], 1_000_000 * USDC).unwrap();
        send(&mut self.ctx, &[ix], &[]).await.unwrap();
        self
    }
}

#[tokio::test]
async fn only_the_upgrade_authority_can_set_up_and_only_the_admin_creates_vaults() {
    let mut env = Env::new().await;
    let attacker = env.attacker.insecure_clone();
    let deployer = env.deployer.insecure_clone();

    assert_eq!(custom_error(env.init_config(&attacker).await), unauthorized());
    env.init_config(&deployer).await.unwrap();

    // The canonical vault can't be taken by someone else, or created with a fake feed
    let oracle = env.oracle;
    assert_eq!(custom_error(env.init_vault(&attacker, oracle).await), unauthorized());
    let fake = env.fake_oracle;
    let invalid_oracle: u32 = PropAmmError::InvalidOracleAccount.into();
    assert_eq!(custom_error(env.init_vault(&deployer, fake).await), invalid_oracle);
    env.init_vault(&deployer, oracle).await.unwrap();
}

#[tokio::test]
async fn only_the_admin_can_trade_against_lp_funds() {
    let mut env = Env::new().await.with_funded_vault().await;
    let attacker = env.attacker.insecure_clone();

    assert_eq!(custom_error(env.phantom_close(&attacker).await), unauthorized());
    assert_eq!(custom_error(env.open(&attacker).await), unauthorized());

    assert_eq!(balance(&mut env.ctx, env.vault_token).await, 1_000_000 * USDC);
    assert_eq!(balance(&mut env.ctx, env.attacker_tokens).await, 0);
}

#[tokio::test]
async fn admin_handover_moves_trading_rights() {
    let mut env = Env::new().await.with_funded_vault().await;
    let deployer = env.deployer.insecure_clone();
    let new_admin = env.attacker.insecure_clone(); // any key the deployer hands over to

    let propose = prop_amm::accounts::AdminOnly { admin: deployer.pubkey(), config: config_pda() };
    env.exec(propose, prop_amm::instruction::ProposeAdmin { new_admin: new_admin.pubkey() }, &deployer).await.unwrap();
    // Not admin until accepted
    assert_eq!(custom_error(env.open(&new_admin).await), unauthorized());

    let accept = prop_amm::accounts::AcceptAdmin { new_admin: new_admin.pubkey(), config: config_pda() };
    env.exec(accept, prop_amm::instruction::AcceptAdmin {}, &new_admin).await.unwrap();
    // The new admin gets past authorization; the trade then stops at the
    // exposure check because the test vault has no recorded LP deposits.
    let exposure: u32 = PropAmmError::ExposureLimitExceeded.into();
    assert_eq!(custom_error(env.open(&new_admin).await), exposure);
    assert_eq!(custom_error(env.open(&deployer).await), unauthorized());
}
