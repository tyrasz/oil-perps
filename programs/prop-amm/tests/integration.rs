//! End-to-end checks for the LP vault, run in-process with solana-program-test.

use anchor_lang::{InstructionData, ToAccountMetas};
use prop_amm::instructions::{ClosePositionParams, InitializeVaultParams, OpenPositionParams};
use prop_amm::PropAmmError;
use prop_amm::oracle::pyth_receiver::PriceUpdateV2View;
use prop_amm::LpVault;
use anchor_lang::{AccountDeserialize, AccountSerialize};
use solana_program_test::{processor, BanksClientError, ProgramTest, ProgramTestContext};
use solana_sdk::{
    account::{Account, AccountSharedData},
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
    // Anchor 1.2's entry wants one lifetime for the id, the accounts and the data
    let program_id: &Pubkey = Box::leak(Box::new(*program_id));
    let data: &[u8] = Box::leak(data.to_vec().into_boxed_slice());
    let accounts = Box::leak(Box::new(accounts.to_vec()));
    prop_amm::entry(program_id, accounts, data)
}

/// The vault's Pyth feed id (USOILSPOT's, as perps-core's OIL test market; a test value, D53).
const FEED_ID: [u8; 32] = [
    0x92, 0x5c, 0xa9, 0x2f, 0xf0, 0x05, 0xae, 0x94, 0x3c, 0x15, 0x8e, 0x35, 0x63, 0xf5, 0x96, 0x98,
    0xce, 0x7e, 0x75, 0xc5, 0xa8, 0xc8, 0xdd, 0x43, 0x30, 0x3a, 0x0a, 0x15, 0x48, 0x87, 0xb3, 0xe6,
];

/// A Full Pyth Receiver `PriceUpdateV2` for `FEED_ID` at `price` (expo -8), published at `NOW`.
fn pyth_account(price: i64, owner: Pubkey) -> Account {
    feed(PriceUpdateV2View::new(FEED_ID, price, 0, -8, NOW).to_bytes(), owner)
}

fn feed(data: Vec<u8>, owner: Pubkey) -> Account {
    Account { lamports: 1_000_000_000, data, owner, executable: false, rent_epoch: 0 }
}

fn perr(e: PropAmmError) -> u32 {
    e.into()
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
        // Lets the native (non-SBF) program reach program-test's clock and CPI stubs
        solana_sdk::native_stubs::install();
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
        self.init_vault_for(signer, oracle, FEED_ID).await
    }

    async fn init_vault_for(&mut self, signer: &Keypair, oracle: Pubkey, pyth_feed_id: [u8; 32]) -> Result<(), BanksClientError> {
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
            pyth_feed_id,
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
        let tokens = self.attacker_tokens;
        self.open_with(trader, tokens).await
    }

    async fn open_with(&mut self, trader: &Keypair, trader_token_account: Pubkey) -> Result<(), BanksClientError> {
        let accounts = prop_amm::accounts::OpenPositionAmm {
            trader: trader.pubkey(),
            config: config_pda(),
            vault: self.vault,
            vault_token_account: self.vault_token,
            trader_token_account,
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

impl Env {
    /// `with_funded_vault`, with the $1M recorded as LP assets (so a trade
    /// passes the utilization check and reaches the oracle) and a token
    /// account of the deployer's. Returns that account.
    async fn tradable(&mut self) -> Pubkey {
        let mut acct = self.ctx.banks_client.get_account(self.vault).await.unwrap().unwrap();
        let mut v = LpVault::try_deserialize(&mut acct.data.as_slice()).unwrap();
        v.total_assets = 1_000_000 * USDC;
        let mut data = Vec::new();
        v.try_serialize(&mut data).unwrap();
        acct.data[..data.len()].copy_from_slice(&data);
        self.ctx.set_account(&self.vault, &AccountSharedData::from(acct));

        let payer = self.ctx.payer.insecure_clone();
        let tokens = Keypair::new();
        let rent = self.ctx.banks_client.get_rent().await.unwrap();
        let len = spl_token::state::Account::LEN;
        send(&mut self.ctx, &[
            system_instruction::create_account(&payer.pubkey(), &tokens.pubkey(), rent.minimum_balance(len), len as u64, &spl_token::ID),
            spl_token::instruction::initialize_account(&spl_token::ID, &tokens.pubkey(), &self.mint, &self.deployer.pubkey()).unwrap(),
        ], &[&tokens]).await.unwrap();
        tokens.pubkey()
    }

    fn set_feed(&mut self, data: Vec<u8>, owner: Pubkey) {
        let oracle = self.oracle;
        self.ctx.set_account(&oracle, &AccountSharedData::from(feed(data, owner)));
    }

    async fn set_time(&mut self, ts: i64) {
        let mut clock: Clock = self.ctx.banks_client.get_sysvar().await.unwrap();
        clock.unix_timestamp = ts;
        self.ctx.set_sysvar(&clock);
    }
}

/// open/close read the pinned feed with perps-core's PriceUpdateV2 checks
/// (docs/lp-pool.md D47): owner = the vault's snapshot of
/// config.oracle_program (D52), Full, the vault's feed id, at most 60 s old
/// or ahead, then the shared price rules.
#[tokio::test]
async fn trades_read_the_vaults_price_update() {
    let mut env = Env::new().await.with_funded_vault().await;
    let deployer = env.deployer.insecure_clone();
    let tokens = env.tradable().await;
    env.open_with(&deployer, tokens).await.unwrap();
    let v: LpVault = {
        let acct = env.ctx.banks_client.get_account(env.vault).await.unwrap().unwrap();
        LpVault::try_deserialize(&mut acct.data.as_slice()).unwrap()
    };
    assert_eq!((v.total_long_size, v.pyth_feed_id), (USDC, FEED_ID));

    let good = PriceUpdateV2View::new(FEED_ID, 7_500_000_000, 0, -8, NOW);
    let cases: Vec<(Vec<u8>, Pubkey, u32)> = vec![
        // Published 61 s before or after the clock
        (PriceUpdateV2View { publish_time: NOW - 61, ..good }.to_bytes(), PYTH_PROGRAM, perr(PropAmmError::StaleOraclePrice)),
        (PriceUpdateV2View { publish_time: NOW + 61, ..good }.to_bytes(), PYTH_PROGRAM, perr(PropAmmError::StaleOraclePrice)),
        (PriceUpdateV2View { feed_id: [7; 32], ..good }.to_bytes(), PYTH_PROGRAM, perr(PropAmmError::OracleFeedIdMismatch)),
        (good.to_partial_bytes(13), PYTH_PROGRAM, perr(PropAmmError::OracleNotFullyVerified)),
        (good.to_bytes(), Pubkey::new_unique(), perr(PropAmmError::InvalidOracleAccount)),
        (good.to_bytes()[..132].to_vec(), PYTH_PROGRAM, perr(PropAmmError::InvalidOracleAccount)),
        ({ let mut d = good.to_bytes(); d[0] ^= 1; d }, PYTH_PROGRAM, perr(PropAmmError::InvalidOracleAccount)),
        (PriceUpdateV2View { conf: 75_000_001, ..good }.to_bytes(), PYTH_PROGRAM, perr(PropAmmError::OracleConfidenceTooWide)),
        (PriceUpdateV2View { price: -1, ..good }.to_bytes(), PYTH_PROGRAM, perr(PropAmmError::InvalidOraclePrice)),
    ];
    for (i, (data, owner, code)) in cases.into_iter().enumerate() {
        env.set_feed(data, owner);
        assert_eq!(custom_error(env.open_with(&deployer, tokens).await), code, "case {}", i);
    }
    // Inside the age limit either way
    for t in [NOW - 60, NOW + 60] {
        env.set_feed(PriceUpdateV2View { publish_time: t, ..good }.to_bytes(), PYTH_PROGRAM);
        env.open_with(&deployer, tokens).await.unwrap();
    }
    // The clock moves on: stale
    env.set_feed(good.to_bytes(), PYTH_PROGRAM);
    env.set_time(NOW + 61).await;
    assert_eq!(custom_error(env.open_with(&deployer, tokens).await), perr(PropAmmError::StaleOraclePrice));
}

#[tokio::test]
async fn vaults_need_a_full_update_for_their_feed_id() {
    let mut env = Env::new().await;
    let deployer = env.deployer.insecure_clone();
    env.init_config(&deployer).await.unwrap();
    let oracle = env.oracle;
    assert_eq!(custom_error(env.init_vault_for(&deployer, oracle, [0; 32]).await), perr(PropAmmError::InvalidParameters));
    assert_eq!(custom_error(env.init_vault_for(&deployer, oracle, [7; 32]).await), perr(PropAmmError::OracleFeedIdMismatch));
    env.set_feed(PriceUpdateV2View::new(FEED_ID, 7_500_000_000, 0, -8, NOW).to_partial_bytes(1), PYTH_PROGRAM);
    assert_eq!(custom_error(env.init_vault(&deployer, oracle).await), perr(PropAmmError::OracleNotFullyVerified));
    // A stale update is accepted at creation (no age check)
    env.set_feed(PriceUpdateV2View::new(FEED_ID, 7_500_000_000, 0, -8, NOW - 86_400).to_bytes(), PYTH_PROGRAM);
    env.init_vault(&deployer, oracle).await.unwrap();
}

impl Env {
    async fn set_oracle_program(&mut self, oracle_program: Pubkey) -> Result<(), BanksClientError> {
        let deployer = self.deployer.insecure_clone();
        let accounts = prop_amm::accounts::AdminOnly { admin: deployer.pubkey(), config: config_pda() };
        self.exec(accounts, prop_amm::instruction::SetOracleProgram { oracle_program }, &deployer).await
    }
}

/// The vault snapshots the oracle program at creation (docs/lp-pool.md
/// D52): a later set_oracle_program reaches new vaults only, so existing
/// vaults keep trading on their feed.
#[tokio::test]
async fn vaults_keep_the_oracle_program_they_were_created_with() {
    let mut env = Env::new().await.with_funded_vault().await;
    let deployer = env.deployer.insecure_clone();
    let tokens = env.tradable().await;
    let v: LpVault = {
        let acct = env.ctx.banks_client.get_account(env.vault).await.unwrap().unwrap();
        LpVault::try_deserialize(&mut acct.data.as_slice()).unwrap()
    };
    assert_eq!(v.oracle_program, PYTH_PROGRAM);
    env.set_oracle_program(Pubkey::new_unique()).await.unwrap();
    env.open_with(&deployer, tokens).await.unwrap();
    // A feed owned by the new program is not the vault's
    let good = PriceUpdateV2View::new(FEED_ID, 7_500_000_000, 0, -8, NOW).to_bytes();
    env.set_feed(good, Pubkey::new_unique());
    assert_eq!(custom_error(env.open_with(&deployer, tokens).await), perr(PropAmmError::InvalidOracleAccount));
}

/// Under a Pyth Receiver the vault's feed must be the push oracle's shard-0
/// account for its feed id (D52): an account anyone can post into is refused.
#[tokio::test]
async fn vaults_under_a_pyth_receiver_pin_the_push_feed_account() {
    use prop_amm::oracle::pyth_push::{push_feed_address, PYTH_PUSH_ORACLE_LEGACY, PYTH_PUSH_ORACLE_PRO, PYTH_RECEIVER_PRO};
    let mut env = Env::new().await;
    let deployer = env.deployer.insecure_clone();
    env.init_config(&deployer).await.unwrap();
    env.set_oracle_program(PYTH_RECEIVER_PRO).await.unwrap();
    let data = PriceUpdateV2View::new(FEED_ID, 7_500_000_000, 0, -8, NOW).to_bytes();
    // A keypair account and the other pair's PDA, both owned by the Receiver
    for key in [Pubkey::new_unique(), push_feed_address(&PYTH_PUSH_ORACLE_LEGACY, &FEED_ID)] {
        env.ctx.set_account(&key, &AccountSharedData::from(feed(data.clone(), PYTH_RECEIVER_PRO)));
        assert_eq!(custom_error(env.init_vault(&deployer, key).await), perr(PropAmmError::InvalidOracleAccount));
    }
    let pda = push_feed_address(&PYTH_PUSH_ORACLE_PRO, &FEED_ID);
    env.ctx.set_account(&pda, &AccountSharedData::from(feed(data, PYTH_RECEIVER_PRO)));
    env.init_vault(&deployer, pda).await.unwrap();
    let acct = env.ctx.banks_client.get_account(env.vault).await.unwrap().unwrap();
    let v = LpVault::try_deserialize(&mut acct.data.as_slice()).unwrap();
    assert_eq!((v.pyth_price_feed, v.oracle_program), (pda, PYTH_RECEIVER_PRO));
}
