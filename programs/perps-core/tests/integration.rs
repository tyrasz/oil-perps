//! End-to-end tests: the real perps-core program runs in-process under
//! solana-program-test, with SPL Token and fake Pyth Receiver `PriceUpdateV2`
//! accounts whose price and publish time the tests control.

use anchor_lang::{AccountDeserialize, InstructionData, ToAccountMetas};
use perps_core::errors::PerpsError;
use perps_core::instructions::{InitializeMarketParams, MarketConfigParams, OpenPositionParams, ProtocolRolesParams};
use perps_core::session::SCHEDULE_CME_GLOBEX;
use perps_core::state::{LpPool, Market, PoolParams, Position, PositionStatus, UserAccount};
use perps_core::pyth_receiver::{set_publish_times, PriceUpdateV2View};
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

#[allow(dead_code)]
#[path = "support/market_env.rs"]
mod support;
// venue_fill, driven through a relay venue on the shared harness
#[path = "venue/mod.rs"]
mod venue;
// The LP pool's own lifecycle, on the shared harness
#[path = "lp/mod.rs"]
mod lp;
// House trading against the pool: caps, liquidation, ADL, funding
#[path = "trading/mod.rs"]
mod trading;
// The PriceUpdateV2 oracle: read_price's checks and the feed rotation (D47)
#[path = "oracle/mod.rs"]
mod oracle;

const USDC: u64 = 1_000_000;
/// Stands in for the Pyth program: owner of every test price feed.
const PYTH_PROGRAM: Pubkey = Pubkey::new_from_array([0x50; 32]);
const CONTRACT: u64 = 1_000_000;

// Fri 2026-11-13 17:00 ET (22:00 UTC) is a weekly close.
const FRIDAY_CLOSE: i64 = 1_794_607_200;
const WEDNESDAY: i64 = 1_794_416_400; // Wed 12:00 ET
const FRIDAY_PRE_CLOSE: i64 = FRIDAY_CLOSE - 3_600; // inside a 2h window
const SATURDAY: i64 = FRIDAY_CLOSE + 86_400;

fn process(program_id: &Pubkey, accounts: &[solana_sdk::account_info::AccountInfo], data: &[u8]) -> solana_sdk::entrypoint::ProgramResult {
    // Anchor's entrypoint wants one lifetime for the slice and its elements.
    // Anchor 1.2's entry wants one lifetime for the id, the accounts and the data
    let program_id: &Pubkey = Box::leak(Box::new(*program_id));
    let data: &[u8] = Box::leak(data.to_vec().into_boxed_slice());
    let accounts = Box::leak(Box::new(accounts.to_vec()));
    perps_core::entry(program_id, accounts, data)
}

#[derive(Clone, Copy)]
struct MarketKeys {
    market: Pubkey,
    vault: Pubkey,
    vault_token: Pubkey,
    oracle: Pubkey,
}

struct Trader {
    keypair: Keypair,
    token_account: Pubkey,
}

struct Env {
    ctx: ProgramTestContext,
    /// Upgrade authority, and so the initial admin
    authority: Keypair,
    guardian: Keypair,
    mint: Pubkey,
    oil: MarketKeys,
    gold: MarketKeys,
    alice: Trader,
    keeper: Trader,
}

fn market_keys(mint: &Pubkey, commodity: &str, oracle: Pubkey) -> MarketKeys {
    let id = perps_core::ID;
    let (market, _) = Pubkey::find_program_address(&[b"market", mint.as_ref(), commodity.as_bytes()], &id);
    let (vault, _) = Pubkey::find_program_address(&[b"vault", market.as_ref()], &id);
    let (vault_token, _) = Pubkey::find_program_address(&[b"vault_token", market.as_ref()], &id);
    MarketKeys { market, vault, vault_token, oracle }
}

fn user_referral_pda(user: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"user_referral", user.as_ref()], &perps_core::ID).0
}

fn referral_code_pda(code: &[u8; 8]) -> Pubkey {
    Pubkey::find_program_address(&[b"referral_code", code.as_ref()], &perps_core::ID).0
}

fn user_account(owner: &Pubkey, market: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"user", owner.as_ref(), market.as_ref()], &perps_core::ID).0
}

/// Pyth feed ids: OIL is USOILSPOT's (support::OIL_FEED_ID), GOLD XAU/USD's.
fn feed_id(commodity: &str) -> [u8; 32] {
    match commodity {
        "GOLD" => GOLD_FEED_ID,
        _ => support::OIL_FEED_ID,
    }
}

/// XAU/USD: 765d2ba9…4bb2
const GOLD_FEED_ID: [u8; 32] = [
    0x76, 0x5d, 0x2b, 0xa9, 0x06, 0xdb, 0xc3, 0x2c, 0xa1, 0x7c, 0xc1, 0x1f, 0x53, 0x10, 0xa8, 0x9e,
    0x9e, 0xe1, 0xf6, 0x42, 0x05, 0x08, 0xc6, 0x38, 0x61, 0xf2, 0xf8, 0xba, 0x4e, 0xe3, 0x4b, 0xb2,
];

/// A Full Pyth Receiver `PriceUpdateV2` for `commodity`'s feed id:
/// `price * 10^expo` with confidence `conf`, published at `timestamp`.
fn pyth_account(commodity: &str, price: i64, conf: u64, expo: i32, timestamp: i64) -> Account {
    support::feed_account(PriceUpdateV2View::new(feed_id(commodity), price, conf, expo, timestamp).to_bytes(), PYTH_PROGRAM)
}

fn dollars(d: f64) -> i64 {
    (d * 1e8).round() as i64 // Pyth expo -8
}

fn oil_params(commodity: &str) -> InitializeMarketParams {
    InitializeMarketParams {
        commodity: commodity.to_string(),
        max_leverage: 20_000,
        maintenance_margin_ratio: 500,
        initial_margin_ratio: 1_000,
        taker_fee: 5,
        maker_fee: 2,
        liquidation_fee: 100,
        max_open_interest: 1_000_000 * CONTRACT,
        funding_interval: 3_600,
        pyth_feed_id: feed_id(commodity),
    }
}

fn weekend_config() -> MarketConfigParams {
    MarketConfigParams {
        is_paused: false,
        session_schedule: SCHEDULE_CME_GLOBEX,
        pre_close_window_secs: 7_200,
        weekend_initial_margin_ratio: 2_000,
        weekend_maintenance_margin_ratio: 1_000,
        max_confidence_bps: 100,
        min_oracle_price: 0,
        max_open_interest: 0,
    }
}

fn code(e: PerpsError) -> u32 {
    e.into()
}

fn custom_error(result: Result<(), BanksClientError>) -> u32 {
    match result.expect_err("transaction should fail").unwrap() {
        TransactionError::InstructionError(_, InstructionError::Custom(c)) => c,
        other => panic!("unexpected error: {:?}", other),
    }
}

fn config_pda() -> Pubkey {
    Pubkey::find_program_address(&[b"config"], &perps_core::ID).0
}

impl Env {
    /// Admin config created by the upgrade authority, and an OIL market.
    async fn new() -> Env {
        let mut env = Env::bare().await;
        let authority = env.authority.insecure_clone();
        env.init_config(&authority).await.unwrap();
        env.init_market("OIL").await.unwrap();
        env
    }

    /// No protocol config and no markets yet.
    async fn bare() -> Env {
        let authority = Keypair::new();
        let mut pt = ProgramTest::new("perps_core", perps_core::ID, processor!(process));
        // The program's ProgramData account, naming `authority` as upgrade authority
        let (program_data, _) = Pubkey::find_program_address(&[perps_core::ID.as_ref()], &bpf_loader_upgradeable::id());
        let state = UpgradeableLoaderState::ProgramData { slot: 0, upgrade_authority_address: Some(authority.pubkey()) };
        pt.add_account(program_data, Account {
            lamports: 1_000_000_000,
            data: bincode::serialize(&state).unwrap(),
            owner: bpf_loader_upgradeable::id(),
            executable: false,
            rent_epoch: 0,
        });
        let oil_oracle = Pubkey::new_unique();
        let gold_oracle = Pubkey::new_unique();
        pt.add_account(oil_oracle, pyth_account("OIL", dollars(75.0), 0, -8, WEDNESDAY));
        pt.add_account(gold_oracle, pyth_account("GOLD", dollars(2_000.0), 0, -8, WEDNESDAY));
        let mut ctx = pt.start_with_context().await;
        support::install_native_stubs();

        let guardian = Keypair::new();
        let mint = Keypair::new();
        let alice = Keypair::new();
        let keeper = Keypair::new();
        let payer = ctx.payer.insecure_clone();
        for who in [&authority, &guardian, &alice, &keeper] {
            let ix = system_instruction::transfer(&payer.pubkey(), &who.pubkey(), 10_000_000_000);
            send(&mut ctx, &[ix], &[&payer]).await.unwrap();
        }

        // USDC-like mint (6 decimals) and token accounts
        let rent = ctx.banks_client.get_rent().await.unwrap();
        let ixs = vec![
            system_instruction::create_account(&payer.pubkey(), &mint.pubkey(), rent.minimum_balance(spl_token::state::Mint::LEN), spl_token::state::Mint::LEN as u64, &spl_token::ID),
            spl_token::instruction::initialize_mint(&spl_token::ID, &mint.pubkey(), &payer.pubkey(), None, 6).unwrap(),
        ];
        send(&mut ctx, &ixs, &[&payer, &mint]).await.unwrap();

        let mut env = Env {
            oil: market_keys(&mint.pubkey(), "OIL", oil_oracle),
            gold: market_keys(&mint.pubkey(), "GOLD", gold_oracle),
            alice: Trader { token_account: Pubkey::default(), keypair: alice },
            keeper: Trader { token_account: Pubkey::default(), keypair: keeper },
            mint: mint.pubkey(),
            authority,
            guardian,
            ctx,
        };
        env.alice.token_account = env.token_account(&env.alice.keypair.pubkey(), 10_000 * USDC).await;
        env.keeper.token_account = env.token_account(&env.keeper.keypair.pubkey(), 0).await;

        env.set_time(WEDNESDAY).await;
        env
    }

    async fn funded_keypair(&mut self) -> Keypair {
        let k = Keypair::new();
        let payer = self.ctx.payer.insecure_clone();
        let ix = system_instruction::transfer(&payer.pubkey(), &k.pubkey(), 10_000_000_000);
        send(&mut self.ctx, &[ix], &[]).await.unwrap();
        k
    }

    async fn init_config(&mut self, signer: &Keypair) -> Result<(), BanksClientError> {
        let accounts = perps_core::accounts::InitializeConfig {
            upgrade_authority: signer.pubkey(),
            config: config_pda(),
            program_data: Pubkey::find_program_address(&[perps_core::ID.as_ref()], &bpf_loader_upgradeable::id()).0,
            system_program: system_program::ID,
        };
        let params = ProtocolRolesParams { guardian: self.guardian.pubkey(), oracle_program: PYTH_PROGRAM };
        self.exec(accounts, perps_core::instruction::InitializeConfig { params }, &[signer]).await
    }

    async fn pause(&mut self, commodity: &str, signer: &Keypair) -> Result<(), BanksClientError> {
        let accounts = perps_core::accounts::PauseMarket { signer: signer.pubkey(), config: config_pda(), market: self.keys(commodity).market };
        self.exec(accounts, perps_core::instruction::PauseMarket {}, &[signer]).await
    }

    async fn propose_admin(&mut self, signer: &Keypair, new_admin: Pubkey) -> Result<(), BanksClientError> {
        let accounts = perps_core::accounts::AdminOnly { admin: signer.pubkey(), config: config_pda() };
        self.exec(accounts, perps_core::instruction::ProposeAdmin { new_admin }, &[signer]).await
    }

    async fn accept_admin(&mut self, signer: &Keypair) -> Result<(), BanksClientError> {
        let accounts = perps_core::accounts::AcceptAdmin { new_admin: signer.pubkey(), config: config_pda() };
        self.exec(accounts, perps_core::instruction::AcceptAdmin {}, &[signer]).await
    }

    async fn token_account(&mut self, owner: &Pubkey, amount: u64) -> Pubkey {
        let account = Keypair::new();
        let payer = self.ctx.payer.insecure_clone();
        let rent = self.ctx.banks_client.get_rent().await.unwrap();
        let len = spl_token::state::Account::LEN;
        let mut ixs = vec![
            system_instruction::create_account(&payer.pubkey(), &account.pubkey(), rent.minimum_balance(len), len as u64, &spl_token::ID),
            spl_token::instruction::initialize_account(&spl_token::ID, &account.pubkey(), &self.mint, owner).unwrap(),
        ];
        if amount > 0 {
            ixs.push(spl_token::instruction::mint_to(&spl_token::ID, &self.mint, &account.pubkey(), &payer.pubkey(), &[], amount).unwrap());
        }
        send(&mut self.ctx, &ixs, &[&payer, &account]).await.unwrap();
        account.pubkey()
    }

    fn keys(&self, commodity: &str) -> &MarketKeys {
        if commodity == "OIL" { &self.oil } else { &self.gold }
    }

    /// Move the cluster clock and refresh oracle timestamps so prices stay fresh.
    async fn set_time(&mut self, unix_timestamp: i64) {
        let mut clock: Clock = self.ctx.banks_client.get_sysvar().await.unwrap();
        clock.unix_timestamp = unix_timestamp;
        self.ctx.set_sysvar(&clock);
        for oracle in [self.oil.oracle, self.gold.oracle] {
            let mut acct = self.ctx.banks_client.get_account(oracle).await.unwrap().unwrap();
            set_publish_times(&mut acct.data, unix_timestamp);
            self.ctx.set_account(&oracle, &AccountSharedData::from(acct));
        }
    }

    async fn now(&mut self) -> i64 {
        self.ctx.banks_client.get_sysvar::<Clock>().await.unwrap().unix_timestamp
    }

    async fn set_price(&mut self, commodity: &str, price: i64, conf: u64) {
        let now = self.now().await;
        let oracle = self.keys(commodity).oracle;
        self.ctx.set_account(&oracle, &AccountSharedData::from(pyth_account(commodity, price, conf, -8, now)));
    }

    async fn init_market(&mut self, commodity: &str) -> Result<(), BanksClientError> {
        self.init_market_with(commodity, oil_params(commodity)).await
    }

    async fn init_market_with(&mut self, commodity: &str, params: InitializeMarketParams) -> Result<(), BanksClientError> {
        let authority = self.authority.insecure_clone();
        let oracle = self.keys(commodity).oracle;
        self.init_market_as(&authority, commodity, params, oracle).await
    }

    /// initialize_market by `signer`, with the LP pool bootstrap in the same
    /// transaction (D11): `initialize_pool(PoolParams::neutral())`, the
    /// signer's LpPosition and a `support::LP_SEED` deposit from a wallet it owns.
    async fn init_market_as(&mut self, signer: &Keypair, commodity: &str, params: InitializeMarketParams, oracle: Pubkey) -> Result<(), BanksClientError> {
        self.init_market_seeded(signer, commodity, params, oracle, support::LP_SEED).await
    }

    /// The market and its pool, but no LP deposit: trading needs a seed first.
    async fn init_market_unseeded(&mut self, commodity: &str) -> Result<(), BanksClientError> {
        let authority = self.authority.insecure_clone();
        let oracle = self.keys(commodity).oracle;
        self.init_market_seeded(&authority, commodity, oil_params(commodity), oracle, 0).await
    }

    async fn init_market_seeded(&mut self, signer: &Keypair, commodity: &str, params: InitializeMarketParams, oracle: Pubkey, seed: u64) -> Result<(), BanksClientError> {
        let k = *self.keys(commodity);
        let tokens = self.token_account(&signer.pubkey(), seed).await;
        let lp_pool = support::lp_pool_pda(&k.market);
        let seed_lp = support::Lp { kp: signer.insecure_clone(), tokens, position: support::lp_position_pda(&lp_pool, &signer.pubkey(), 0) };
        let accounts = perps_core::accounts::InitializeMarket {
            authority: signer.pubkey(),
            config: config_pda(),
            market: k.market,
            vault: k.vault,
            vault_token_account: k.vault_token,
            collateral_mint: self.mint,
            pyth_price_feed: oracle,
            system_program: system_program::ID,
            token_program: spl_token::ID,
            rent: sysvar::rent::ID,
        };
        let data = perps_core::instruction::InitializeMarket { params };
        let mut ixs = vec![Instruction { program_id: perps_core::ID, accounts: accounts.to_account_metas(None), data: data.data() }];
        ixs.extend(support::pool_bootstrap_ixs(&signer.pubkey(), k.market, oracle, &seed_lp, PoolParams::neutral(), seed));
        send(&mut self.ctx, &ixs, &[signer]).await
    }

    /// initialize_market alone: no pool.
    async fn init_market_only(&mut self, commodity: &str) -> Result<(), BanksClientError> {
        let k = *self.keys(commodity);
        let signer = self.authority.insecure_clone();
        let accounts = perps_core::accounts::InitializeMarket {
            authority: signer.pubkey(),
            config: config_pda(),
            market: k.market,
            vault: k.vault,
            vault_token_account: k.vault_token,
            collateral_mint: self.mint,
            pyth_price_feed: k.oracle,
            system_program: system_program::ID,
            token_program: spl_token::ID,
            rent: sysvar::rent::ID,
        };
        self.exec(accounts, perps_core::instruction::InitializeMarket { params: oil_params(commodity) }, &[&signer]).await
    }

    async fn init_pool(&mut self, signer: &Keypair, commodity: &str, params: PoolParams) -> Result<(), BanksClientError> {
        let market = self.keys(commodity).market;
        let accounts = perps_core::accounts::InitializePool {
            admin: signer.pubkey(),
            config: config_pda(),
            market,
            lp_pool: support::lp_pool_pda(&market),
            system_program: system_program::ID,
        };
        self.exec(accounts, perps_core::instruction::InitializePool { params, adl_keeper: signer.pubkey() }, &[signer]).await
    }

    async fn patch_market(&mut self, commodity: &str, f: impl FnOnce(&mut Market)) {
        let address = self.keys(commodity).market;
        let mut acct = self.ctx.banks_client.get_account(address).await.unwrap().unwrap();
        let mut m = Market::try_deserialize(&mut acct.data.as_slice()).unwrap();
        f(&mut m);
        let mut data = Vec::new();
        anchor_lang::AccountSerialize::try_serialize(&m, &mut data).unwrap();
        acct.data[..data.len()].copy_from_slice(&data);
        self.ctx.set_account(&address, &AccountSharedData::from(acct));
        support::rebaseline_buckets(&mut self.ctx).await;
    }

    async fn pool(&mut self, commodity: &str) -> LpPool {
        let address = support::lp_pool_pda(&self.keys(commodity).market);
        self.load(address).await
    }

    async fn configure(&mut self, commodity: &str, params: MarketConfigParams) -> Result<(), BanksClientError> {
        let authority = self.authority.insecure_clone();
        self.configure_as(&authority, commodity, params).await
    }

    async fn configure_as(&mut self, signer: &Keypair, commodity: &str, params: MarketConfigParams) -> Result<(), BanksClientError> {
        let accounts = perps_core::accounts::UpdateMarketConfig { authority: signer.pubkey(), config: config_pda(), market: self.keys(commodity).market };
        self.exec(accounts, perps_core::instruction::UpdateMarketConfig { params }, &[signer]).await
    }

    async fn settle(&mut self, commodity: &str, signer: &Keypair, price: u64) -> Result<(), BanksClientError> {
        let accounts = perps_core::accounts::SettleMarket { authority: signer.pubkey(), config: config_pda(), market: self.keys(commodity).market };
        self.exec(accounts, perps_core::instruction::SettleMarket { settlement_price: price }, &[signer]).await
    }

    async fn init_user(&mut self, commodity: &str) -> Result<(), BanksClientError> {
        let owner = self.alice.keypair.insecure_clone();
        let market = self.keys(commodity).market;
        let accounts = perps_core::accounts::InitializeUser {
            owner: owner.pubkey(),
            market,
            user_account: user_account(&owner.pubkey(), &market),
            system_program: system_program::ID,
        };
        self.exec(accounts, perps_core::instruction::InitializeUser {}, &[&owner]).await
    }

    async fn deposit(&mut self, commodity: &str, amount: u64) -> Result<(), BanksClientError> {
        let owner = self.alice.keypair.insecure_clone();
        let k = *self.keys(commodity);
        let accounts = perps_core::accounts::DepositCollateral {
            owner: owner.pubkey(),
            user_account: user_account(&owner.pubkey(), &k.market),
            market: k.market,
            vault: k.vault,
            vault_token_account: k.vault_token,
            user_token_account: self.alice.token_account,
            token_program: spl_token::ID,
            lp_pool: support::lp_pool_pda(&k.market),
        };
        self.exec(accounts, perps_core::instruction::DepositCollateral { amount }, &[&owner]).await
    }

    /// Withdraw from `commodity`'s vault using the user account of `account_market`.
    async fn withdraw_via(&mut self, commodity: &str, account_market: &str, amount: u64) -> Result<(), BanksClientError> {
        let owner = self.alice.keypair.insecure_clone();
        let k = *self.keys(commodity);
        let accounts = perps_core::accounts::WithdrawCollateral {
            owner: owner.pubkey(),
            user_account: user_account(&owner.pubkey(), &self.keys(account_market).market),
            market: k.market,
            vault: k.vault,
            vault_token_account: k.vault_token,
            user_token_account: self.alice.token_account,
            token_program: spl_token::ID,
            lp_pool: support::lp_pool_pda(&k.market),
        };
        self.exec(accounts, perps_core::instruction::WithdrawCollateral { amount }, &[&owner]).await
    }

    async fn open(&mut self, commodity: &str, side: u8, contracts: u64, leverage_x: u32) -> Result<Pubkey, BanksClientError> {
        let owner = self.alice.keypair.insecure_clone();
        let k = *self.keys(commodity);
        let index = self.market(commodity).await.total_positions;
        let (position, _) = Pubkey::find_program_address(
            &[b"position", owner.pubkey().as_ref(), k.market.as_ref(), &index.to_le_bytes()],
            &perps_core::ID,
        );
        let accounts = perps_core::accounts::OpenPosition {
            owner: owner.pubkey(),
            user_account: user_account(&owner.pubkey(), &k.market),
            market: k.market,
            position,
            pyth_price_feed: k.oracle,
            system_program: system_program::ID,
            lp_pool: support::lp_pool_pda(&k.market),
        };
        let params = OpenPositionParams { side, size: contracts * CONTRACT, leverage: leverage_x * 1_000, acceptable_price: 0 };
        self.exec(accounts, perps_core::instruction::OpenPosition { params }, &[&owner]).await?;
        Ok(position)
    }

    async fn close(&mut self, commodity: &str, position: Pubkey) -> Result<(), BanksClientError> {
        self.close_with_referral(commodity, position, None).await
    }

    /// Close passing (referral_code, referrer_token_account) when given.
    async fn close_with_referral(&mut self, commodity: &str, position: Pubkey, referral: Option<(Pubkey, Pubkey)>) -> Result<(), BanksClientError> {
        let owner = self.alice.keypair.insecure_clone();
        let k = *self.keys(commodity);
        let accounts = perps_core::accounts::ClosePosition {
            owner: owner.pubkey(),
            user_account: user_account(&owner.pubkey(), &k.market),
            market: k.market,
            position,
            vault: k.vault,
            vault_token_account: k.vault_token,
            user_token_account: self.alice.token_account,
            pyth_price_feed: k.oracle,
            token_program: spl_token::ID,
            user_referral: referral.map(|_| user_referral_pda(&owner.pubkey())),
            referral_code: referral.map(|(code, _)| code),
            referrer_token_account: referral.map(|(_, account)| account),
            lp_pool: support::lp_pool_pda(&k.market),
        };
        self.exec(accounts, perps_core::instruction::ClosePosition { acceptable_price: 0 }, &[&owner]).await
    }

    async fn liquidate(&mut self, commodity: &str, position: Pubkey) -> Result<(), BanksClientError> {
        let keeper = self.keeper.keypair.insecure_clone();
        let owner = self.alice.keypair.pubkey();
        let k = *self.keys(commodity);
        let accounts = perps_core::accounts::Liquidate {
            liquidator: keeper.pubkey(),
            position_owner: owner,
            user_account: user_account(&owner, &k.market),
            market: k.market,
            position,
            vault: k.vault,
            vault_token_account: k.vault_token,
            liquidator_token_account: self.keeper.token_account,
            pyth_price_feed: k.oracle,
            token_program: spl_token::ID,
            lp_pool: support::lp_pool_pda(&k.market),
        };
        self.exec(accounts, perps_core::instruction::Liquidate {}, &[&keeper]).await
    }

    async fn update_funding(&mut self, commodity: &str) -> Result<(), BanksClientError> {
        let keeper = self.keeper.keypair.insecure_clone();
        let k = *self.keys(commodity);
        let accounts = perps_core::accounts::UpdateFunding { keeper: keeper.pubkey(), market: k.market, pyth_price_feed: k.oracle };
        self.exec(accounts, perps_core::instruction::UpdateFunding {}, &[&keeper]).await
    }

    /// The keeper wallet creates referral code KEEP; alice applies it.
    async fn refer_alice(&mut self) -> Pubkey {
        let code = *b"KEEP\0\0\0\0";
        let referrer = self.keeper.keypair.insecure_clone();
        let alice = self.alice.keypair.insecure_clone();
        let code_pda = referral_code_pda(&code);
        let accounts = perps_core::accounts::CreateReferralCode {
            owner: referrer.pubkey(),
            referral_code: code_pda,
            system_program: system_program::ID,
        };
        let params = perps_core::instructions::CreateReferralCodeParams { code };
        self.exec(accounts, perps_core::instruction::CreateReferralCode { params }, &[&referrer]).await.unwrap();
        let accounts = perps_core::accounts::ApplyReferralCode {
            user: alice.pubkey(),
            referral_code: code_pda,
            user_referral: user_referral_pda(&alice.pubkey()),
            system_program: system_program::ID,
        };
        self.exec(accounts, perps_core::instruction::ApplyReferralCode {}, &[&alice]).await.unwrap();
        code_pda
    }

    async fn exec(&mut self, accounts: impl ToAccountMetas, data: impl InstructionData, signers: &[&Keypair]) -> Result<(), BanksClientError> {
        let ix = Instruction { program_id: perps_core::ID, accounts: accounts.to_account_metas(None), data: data.data() };
        send(&mut self.ctx, &[ix], signers).await
    }

    async fn load<T: AccountDeserialize>(&mut self, address: Pubkey) -> T {
        let acct = self.ctx.banks_client.get_account(address).await.unwrap().expect("account exists");
        T::try_deserialize(&mut acct.data.as_slice()).unwrap()
    }

    async fn market(&mut self, commodity: &str) -> Market {
        let address = self.keys(commodity).market;
        self.load(address).await
    }

    async fn user(&mut self, commodity: &str) -> UserAccount {
        let address = user_account(&self.alice.keypair.pubkey(), &self.keys(commodity).market);
        self.load(address).await
    }

    async fn token_balance(&mut self, account: Pubkey) -> u64 {
        let acct = self.ctx.banks_client.get_account(account).await.unwrap().unwrap();
        spl_token::state::Account::unpack(&acct.data).unwrap().amount
    }

    /// Alice with `amount` USDC deposited in `commodity`'s market.
    async fn funded(mut self, commodity: &str, amount: u64) -> Env {
        self.init_user(commodity).await.unwrap();
        self.deposit(commodity, amount).await.unwrap();
        self
    }
}

async fn send(ctx: &mut ProgramTestContext, ixs: &[Instruction], signers: &[&Keypair]) -> Result<(), BanksClientError> {
    let payer = ctx.payer.insecure_clone();
    let mut all: Vec<&Keypair> = vec![&payer];
    all.extend_from_slice(signers);
    let blockhash = ctx.get_new_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(ixs, Some(&payer.pubkey()), &all, blockhash);
    let result = ctx.banks_client.process_transaction(tx).await;
    if result.is_ok() {
        // §11 after every transaction (docs/lp-pool.md §17.2)
        support::watch_buckets(ctx, ixs).await;
    }
    result
}

// ---------------------------------------------------------------------------

#[tokio::test]
async fn each_commodity_has_its_own_market() {
    let mut env = Env::new().await;
    env.init_market("GOLD").await.unwrap();

    assert_ne!(env.oil.market, env.gold.market);
    assert_eq!(env.market("OIL").await.commodity_seed(), b"OIL");
    assert_eq!(env.market("GOLD").await.commodity_seed(), b"GOLD");
    assert!(env.init_market("OIL").await.is_err(), "a market can only be created once");
}

#[tokio::test]
async fn collateral_is_isolated_per_market() {
    let mut env = Env::new().await.funded("OIL", 1_000 * USDC).await;
    env.init_market("GOLD").await.unwrap();
    env.init_user("GOLD").await.unwrap();
    // GOLD's vault holds someone else's money: send tokens straight to it
    let payer = env.ctx.payer.insecure_clone();
    let ix = spl_token::instruction::mint_to(&spl_token::ID, &env.mint, &env.gold.vault_token, &payer.pubkey(), &[], 5_000 * USDC).unwrap();
    send(&mut env.ctx, &[ix], &[&payer]).await.unwrap();

    // Alice's GOLD account holds nothing
    assert_eq!(custom_error(env.withdraw_via("GOLD", "GOLD", 100 * USDC).await), code(PerpsError::InsufficientCollateral));
    // Her OIL account can't be used against GOLD's vault: the user-account PDA check rejects it
    let constraint_seeds: u32 = anchor_lang::error::ErrorCode::ConstraintSeeds.into();
    assert_eq!(custom_error(env.withdraw_via("GOLD", "OIL", 100 * USDC).await), constraint_seeds);
    // GOLD's vault also holds its pool's LP seed
    assert_eq!(env.token_balance(env.gold.vault_token).await, support::LP_SEED + 5_000 * USDC);
    assert_eq!(env.user("OIL").await.collateral_balance, 1_000 * USDC);

    env.withdraw_via("OIL", "OIL", 100 * USDC).await.unwrap();
    assert_eq!(env.user("OIL").await.collateral_balance, 900 * USDC);
}

#[tokio::test]
async fn open_and_close_long_at_a_profit() {
    let mut env = Env::new().await.funded("OIL", 1_000 * USDC).await;
    let before = env.token_balance(env.alice.token_account).await;

    // 10 contracts at $75 = $750 notional; 5x posts $150
    let position = env.open("OIL", 0, 10, 5).await.unwrap();
    let p: Position = env.load(position).await;
    assert_eq!((p.collateral, p.entry_price), (150 * USDC, 75 * USDC));
    assert_eq!(env.user("OIL").await.collateral_balance, 850 * USDC);

    env.set_price("OIL", dollars(80.0), 0).await;
    env.close("OIL", position).await.unwrap();

    // $150 collateral + $50 profit - 0.05% fee on $750 ($0.375)
    let received = env.token_balance(env.alice.token_account).await - before;
    assert_eq!(received, 199_625_000);
    let p: Position = env.load(position).await;
    assert!(p.status == PositionStatus::Closed);
}

#[tokio::test]
async fn leverage_beyond_initial_margin_is_rejected() {
    let mut env = Env::new().await.funded("OIL", 1_000 * USDC).await;
    // 10% initial margin caps OIL at 10x despite a 20x max
    assert_eq!(custom_error(env.open("OIL", 0, 10, 20).await.map(|_| ())), code(PerpsError::ExcessiveLeverage));
    env.open("OIL", 0, 10, 10).await.unwrap();
    assert_eq!(env.user("OIL").await.collateral_balance, 925 * USDC);
}

#[tokio::test]
async fn wide_oracle_confidence_blocks_trading() {
    let mut env = Env::new().await.funded("OIL", 1_000 * USDC).await;
    env.set_price("OIL", dollars(75.0), dollars(1.5) as u64).await; // +/- 2%
    assert_eq!(custom_error(env.open("OIL", 0, 10, 5).await.map(|_| ())), code(PerpsError::OracleConfidenceTooWide));
}

#[tokio::test]
async fn closed_market_blocks_price_dependent_actions() {
    let mut env = Env::new().await.funded("OIL", 1_000 * USDC).await;
    env.configure("OIL", weekend_config()).await.unwrap();
    let position = env.open("OIL", 0, 10, 5).await.unwrap();

    env.set_time(SATURDAY).await;
    assert_eq!(custom_error(env.open("OIL", 0, 10, 5).await.map(|_| ())), code(PerpsError::MarketClosed));
    assert_eq!(custom_error(env.close("OIL", position).await), code(PerpsError::MarketClosed));
    assert_eq!(custom_error(env.liquidate("OIL", position).await), code(PerpsError::MarketClosed));
    assert_eq!(custom_error(env.update_funding("OIL").await), code(PerpsError::MarketClosed));
    // Moving collateral doesn't need a price
    env.deposit("OIL", 10 * USDC).await.unwrap();

    env.set_time(SATURDAY + 86_400 + 3_600).await; // Sunday 19:00 ET, reopened
    env.close("OIL", position).await.unwrap();
}

#[tokio::test]
async fn weekend_margins_liquidate_thin_positions_before_the_close() {
    let mut env = Env::new().await.funded("OIL", 1_000 * USDC).await;
    env.configure("OIL", weekend_config()).await.unwrap();

    // 10x long: $75 collateral on $750
    let position = env.open("OIL", 0, 10, 10).await.unwrap();
    // -2%: equity $60 = 8% margin, above the normal 5% maintenance
    env.set_price("OIL", dollars(73.5), 0).await;
    assert_eq!(custom_error(env.liquidate("OIL", position).await), code(PerpsError::NotLiquidatable));

    // An hour before Friday's close the weekend maintenance margin (10%) applies
    env.set_time(FRIDAY_PRE_CLOSE).await;
    assert_eq!(custom_error(env.open("OIL", 0, 1, 10).await.map(|_| ())), code(PerpsError::ExcessiveLeverage));
    env.liquidate("OIL", position).await.unwrap();

    let p: Position = env.load(position).await;
    assert!(p.status == PositionStatus::Liquidated);
    // Reward: 1% of the maintenance margin in force (the weekend 10%) on
    // $750 = $0.75 (docs/lp-pool.md §6.4, D8; was 375_000 on the 5% MMR)
    assert_eq!(env.token_balance(env.keeper.token_account).await, 750_000);
}

#[tokio::test]
async fn settlement_closes_positions_when_the_oracle_breaks() {
    let mut env = Env::new().await.funded("OIL", 1_000 * USDC).await;
    let position = env.open("OIL", 0, 10, 5).await.unwrap();
    let abandoned = env.open("OIL", 0, 10, 5).await.unwrap();
    let before = env.token_balance(env.alice.token_account).await;

    // April 2020: the price goes negative and the oracle can no longer be used
    env.set_price("OIL", dollars(-37.63), 0).await;
    assert_eq!(custom_error(env.close("OIL", position).await), code(PerpsError::InvalidOraclePrice));

    let stranger = Keypair::new();
    assert_eq!(custom_error(env.settle("OIL", &stranger, 70 * USDC).await), code(PerpsError::Unauthorized));
    let authority = env.authority.insecure_clone();
    env.settle("OIL", &authority, 70 * USDC).await.unwrap();

    assert_eq!(custom_error(env.liquidate("OIL", position).await), code(PerpsError::MarketSettled));

    // Closes at $70: $150 - $50 loss - $0.375 fee
    env.close("OIL", position).await.unwrap();
    assert_eq!(env.token_balance(env.alice.token_account).await - before, 99_625_000);

    // Anyone closes a position its owner abandoned, at the settlement price;
    // the equity goes to the owner's free collateral (docs/lp-pool.md §10.5)
    let keeper = env.keeper.keypair.insecure_clone();
    let k = env.oil;
    let accounts = perps_core::accounts::CloseSettledPosition {
        signer: keeper.pubkey(),
        market: k.market,
        lp_pool: support::lp_pool_pda(&k.market),
        position: abandoned,
        position_owner: env.alice.keypair.pubkey(),
        user_account: user_account(&env.alice.keypair.pubkey(), &k.market),
    };
    env.exec(accounts, perps_core::instruction::CloseSettledPosition {}, &[&keeper]).await.unwrap();
    assert_eq!(env.user("OIL").await.collateral_balance, 700 * USDC + 99_625_000);
    assert!(env.load::<Position>(abandoned).await.status == PositionStatus::Closed);

    assert_eq!(custom_error(env.open("OIL", 0, 1, 5).await.map(|_| ())), code(PerpsError::MarketPaused));
    assert_eq!(custom_error(env.settle("OIL", &authority, 71 * USDC).await), code(PerpsError::MarketSettled));
}

#[tokio::test]
async fn funding_is_charged_on_notional() {
    let mut env = Env::new().await.funded("OIL", 10_000 * USDC).await;
    let before = env.token_balance(env.alice.token_account).await;
    // 100 longs, no shorts: maximum imbalance -> +0.01% for the period
    let position = env.open("OIL", 0, 100, 5).await.unwrap();

    let t = env.now().await;
    env.set_time(t + 3_600).await;
    env.update_funding("OIL").await.unwrap();
    env.close("OIL", position).await.unwrap();

    // $1,500 collateral - 0.01% of $7,500 funding ($0.75) - 0.05% fee ($3.75)
    assert_eq!(env.token_balance(env.alice.token_account).await - before, 1_495_500_000);
}

#[tokio::test]
async fn each_funding_period_is_charged_at_its_own_price() {
    let mut env = Env::new().await.funded("OIL", 10_000 * USDC).await;
    let before = env.token_balance(env.alice.token_account).await;
    let position = env.open("OIL", 0, 100, 5).await.unwrap();

    // 0.01% at $75, then the price doubles and another 0.01% at $150
    let t = env.now().await;
    env.set_time(t + 3_600).await;
    env.update_funding("OIL").await.unwrap();
    env.set_price("OIL", dollars(150.0), 0).await;
    env.set_time(t + 7_200).await;
    env.update_funding("OIL").await.unwrap();
    assert_eq!(env.market("OIL").await.funding_rate, 100); // latest period's rate, for display

    env.close("OIL", position).await.unwrap();
    // $1,500 collateral + $7,500 profit - funding ($0.75 + $1.50) - fee ($3.75).
    // Charging both periods at the closing price would have cost $3.00.
    assert_eq!(env.token_balance(env.alice.token_account).await - before, 8_994_000_000);
}

#[tokio::test]
async fn referral_reward_is_paid_to_the_referrer_at_close() {
    let mut env = Env::new().await.funded("OIL", 1_000 * USDC).await;
    let referral_code = env.refer_alice().await;
    let before = env.token_balance(env.alice.token_account).await;
    let position = env.open("OIL", 0, 10, 5).await.unwrap();

    // Someone else's token account can't be passed as the referrer's
    let wrong = env.alice.token_account;
    assert_eq!(custom_error(env.close_with_referral("OIL", position, Some((referral_code, wrong))).await), code(PerpsError::InvalidReferralParams));

    let referrer = env.keeper.token_account;
    env.close_with_referral("OIL", position, Some((referral_code, referrer))).await.unwrap();
    // Fee $0.375 on $750, 10% discount -> $0.3375; referrer gets 20% -> $0.0675
    assert_eq!(env.token_balance(env.alice.token_account).await - before, 150 * USDC - 337_500);
    assert_eq!(env.token_balance(referrer).await, 67_500);
    assert_eq!(env.market("OIL").await.insurance_fund, 337_500 - 67_500);
}

#[tokio::test]
async fn underwater_close_pays_no_referral_reward() {
    let mut env = Env::new().await.funded("OIL", 1_000 * USDC).await;
    let referral_code = env.refer_alice().await;
    let position = env.open("OIL", 0, 10, 10).await.unwrap();
    // -20%: a $150 loss on $75 collateral
    env.set_price("OIL", dollars(60.0), 0).await;
    let referrer = env.keeper.token_account;
    env.close_with_referral("OIL", position, Some((referral_code, referrer))).await.unwrap();
    assert_eq!(env.token_balance(referrer).await, 0);
}

#[tokio::test]
async fn liquidation_feeds_and_draws_on_the_insurance_fund() {
    let mut env = Env::new().await.funded("OIL", 1_000 * USDC).await;
    // Build up an insurance fund from fees: $750 notional at 0.05% = $0.375 per close
    for _ in 0..4 {
        let p = env.open("OIL", 0, 10, 5).await.unwrap();
        env.close("OIL", p).await.unwrap();
    }
    assert_eq!(env.market("OIL").await.insurance_fund, 1_500_000);

    // 10x long ($75 collateral) gapped 15% down: $112.50 loss, $37.50 bad debt
    let position = env.open("OIL", 0, 10, 10).await.unwrap();
    env.set_price("OIL", dollars(63.75), 0).await;
    env.liquidate("OIL", position).await.unwrap();

    // Reward $0.375 is paid from the fund, which also absorbs as much bad debt as it can
    assert_eq!(env.token_balance(env.keeper.token_account).await, 375_000);
    assert_eq!(env.market("OIL").await.insurance_fund, 0);
}

#[tokio::test]
async fn market_fees_are_bounded() {
    let mut env = Env::new().await;
    let params = InitializeMarketParams { taker_fee: 1_000_000, ..oil_params("GOLD") };
    assert_eq!(custom_error(env.init_market_with("GOLD", params).await), code(PerpsError::InvalidMarketConfig));
}

#[tokio::test]
async fn user_accounts_cannot_be_used_in_another_market() {
    let mut env = Env::new().await.funded("OIL", 1_000 * USDC).await;
    env.init_market("GOLD").await.unwrap();
    let alice = env.alice.keypair.insecure_clone();
    let oil_account = user_account(&alice.pubkey(), &env.oil.market);
    let gold = env.gold;
    let constraint_seeds: u32 = anchor_lang::error::ErrorCode::ConstraintSeeds.into();

    // Depositing into GOLD's vault while crediting the OIL account
    let deposit = perps_core::accounts::DepositCollateral {
        owner: alice.pubkey(),
        user_account: oil_account,
        market: gold.market,
        vault: gold.vault,
        vault_token_account: gold.vault_token,
        user_token_account: env.alice.token_account,
        token_program: spl_token::ID,
        lp_pool: support::lp_pool_pda(&gold.market),
    };
    let result = env.exec(deposit, perps_core::instruction::DepositCollateral { amount: 10 * USDC }, &[&alice]).await;
    assert_eq!(custom_error(result), constraint_seeds);

    // Opening a GOLD position paid for by OIL collateral
    let (position, _) = Pubkey::find_program_address(
        &[b"position", alice.pubkey().as_ref(), gold.market.as_ref(), &0u64.to_le_bytes()],
        &perps_core::ID,
    );
    let open = perps_core::accounts::OpenPosition {
        owner: alice.pubkey(),
        user_account: oil_account,
        market: gold.market,
        position,
        pyth_price_feed: gold.oracle,
        system_program: system_program::ID,
        lp_pool: support::lp_pool_pda(&gold.market),
    };
    let params = OpenPositionParams { side: 0, size: CONTRACT / 100, leverage: 5_000, acceptable_price: 0 };
    let result = env.exec(open, perps_core::instruction::OpenPosition { params }, &[&alice]).await;
    assert_eq!(custom_error(result), constraint_seeds);

    assert_eq!(env.user("OIL").await.collateral_balance, 1_000 * USDC);
    // Only GOLD's pool seed is in its vault
    assert_eq!(env.token_balance(gold.vault_token).await, support::LP_SEED);
}

// --- protocol roles ----------------------------------------------------------

fn unpaused() -> MarketConfigParams {
    MarketConfigParams { is_paused: false, ..Default::default() }
}

#[tokio::test]
async fn only_the_upgrade_authority_can_set_up_the_protocol() {
    let mut env = Env::bare().await;
    let stranger = env.funded_keypair().await;
    assert_eq!(custom_error(env.init_config(&stranger).await), code(PerpsError::Unauthorized));

    let authority = env.authority.insecure_clone();
    env.init_config(&authority).await.unwrap();
    assert!(env.init_config(&authority).await.is_err(), "config can only be created once");
}

#[tokio::test]
async fn only_the_admin_creates_markets_with_a_genuine_feed() {
    let mut env = Env::new().await;
    let stranger = env.funded_keypair().await;
    let gold_oracle = env.gold.oracle;
    assert_eq!(
        custom_error(env.init_market_as(&stranger, "GOLD", oil_params("GOLD"), gold_oracle).await),
        code(PerpsError::Unauthorized)
    );

    // A feed account not owned by the configured oracle program is refused
    let fake_feed = Pubkey::new_unique();
    let mut fake = pyth_account("GOLD", dollars(2_000.0), 0, -8, WEDNESDAY);
    fake.owner = Pubkey::new_unique();
    env.ctx.set_account(&fake_feed, &AccountSharedData::from(fake));
    let authority = env.authority.insecure_clone();
    assert_eq!(
        custom_error(env.init_market_as(&authority, "GOLD", oil_params("GOLD"), fake_feed).await),
        code(PerpsError::InvalidOracleAccount)
    );

    env.init_market("GOLD").await.unwrap();
}

/// initialize_market takes a non-zero feed id and requires the account to
/// be a Full PriceUpdateV2 for it; the update's age is not checked
/// (docs/lp-pool.md D47).
#[tokio::test]
async fn markets_need_a_full_update_for_their_feed_id() {
    let mut env = Env::new().await;
    let authority = env.authority.insecure_clone();
    let gold_oracle = env.gold.oracle;
    let invalid_config = code(PerpsError::InvalidMarketConfig);
    assert_eq!(
        custom_error(env.init_market_as(&authority, "GOLD", InitializeMarketParams { pyth_feed_id: [0; 32], ..oil_params("GOLD") }, gold_oracle).await),
        invalid_config
    );
    // The OIL feed id against the GOLD account
    assert_eq!(
        custom_error(env.init_market_as(&authority, "GOLD", InitializeMarketParams { pyth_feed_id: feed_id("OIL"), ..oil_params("GOLD") }, gold_oracle).await),
        code(PerpsError::OracleFeedIdMismatch)
    );
    let v = PriceUpdateV2View::new(GOLD_FEED_ID, dollars(2_000.0), 0, -8, WEDNESDAY);
    let mut wrong_disc = v.to_bytes();
    wrong_disc[7] ^= 0xff;
    for (data, expected) in [
        (v.to_partial_bytes(13), code(PerpsError::OracleNotFullyVerified)),
        (wrong_disc, code(PerpsError::InvalidOracleAccount)),
        (v.to_bytes()[..132].to_vec(), code(PerpsError::InvalidOracleAccount)),
    ] {
        env.ctx.set_account(&gold_oracle, &AccountSharedData::from(support::feed_account(data, PYTH_PROGRAM)));
        assert_eq!(custom_error(env.init_market_as(&authority, "GOLD", oil_params("GOLD"), gold_oracle).await), expected);
    }
    // A day-old update is fine at creation (the market may open later)
    let stale = PriceUpdateV2View { publish_time: WEDNESDAY - 86_400, ..v };
    env.ctx.set_account(&gold_oracle, &AccountSharedData::from(support::feed_account(stale.to_bytes(), PYTH_PROGRAM)));
    env.init_market_seeded(&authority, "GOLD", oil_params("GOLD"), gold_oracle, 0).await.unwrap();
    let m: Market = env.load(env.gold.market).await;
    assert_eq!((m.pyth_price_feed, m.pyth_feed_id, m.oracle_program), (gold_oracle, GOLD_FEED_ID, PYTH_PROGRAM));
}

/// Under one of Pyth's Receivers the market's account must be the push
/// oracle's shard-0 PDA for its feed id (docs/lp-pool.md D52): a Receiver-
/// owned account anyone can post into, or the other pair's PDA, is refused.
#[tokio::test]
async fn markets_under_a_pyth_receiver_pin_the_push_feed_account() {
    use perps_core::pyth_push::{push_feed_address, PYTH_PUSH_ORACLE_LEGACY, PYTH_PUSH_ORACLE_PRO, PYTH_RECEIVER_PRO};
    let mut env = Env::new().await;
    let authority = env.authority.insecure_clone();
    let guardian = env.guardian.pubkey();
    let accounts = perps_core::accounts::AdminOnly { admin: authority.pubkey(), config: config_pda() };
    let params = ProtocolRolesParams { guardian, oracle_program: PYTH_RECEIVER_PRO };
    env.exec(accounts, perps_core::instruction::UpdateRoles { params }, &[&authority]).await.unwrap();
    let data = PriceUpdateV2View::new(GOLD_FEED_ID, dollars(2_000.0), 0, -8, WEDNESDAY).to_bytes();
    for key in [Pubkey::new_unique(), push_feed_address(&PYTH_PUSH_ORACLE_LEGACY, &GOLD_FEED_ID), push_feed_address(&PYTH_PUSH_ORACLE_PRO, &feed_id("OIL"))] {
        env.ctx.set_account(&key, &AccountSharedData::from(support::feed_account(data.clone(), PYTH_RECEIVER_PRO)));
        assert_eq!(
            custom_error(env.init_market_seeded(&authority, "GOLD", oil_params("GOLD"), key, 0).await),
            code(PerpsError::InvalidOracleAccount)
        );
    }
    let pda = push_feed_address(&PYTH_PUSH_ORACLE_PRO, &GOLD_FEED_ID);
    env.ctx.set_account(&pda, &AccountSharedData::from(support::feed_account(data, PYTH_RECEIVER_PRO)));
    env.init_market_seeded(&authority, "GOLD", oil_params("GOLD"), pda, 0).await.unwrap();
    let m: Market = env.load(env.gold.market).await;
    assert_eq!((m.pyth_price_feed, m.pyth_feed_id, m.oracle_program), (pda, GOLD_FEED_ID, PYTH_RECEIVER_PRO));
}

#[tokio::test]
async fn guardian_can_pause_but_not_unpause_or_settle() {
    let mut env = Env::new().await.funded("OIL", 1_000 * USDC).await;
    let guardian = env.guardian.insecure_clone();
    let stranger = env.funded_keypair().await;

    assert_eq!(custom_error(env.pause("OIL", &stranger).await), code(PerpsError::Unauthorized));
    env.pause("OIL", &guardian).await.unwrap();
    assert!(env.market("OIL").await.is_paused);
    assert_eq!(custom_error(env.open("OIL", 0, 1, 5).await.map(|_| ())), code(PerpsError::MarketPaused));

    assert_eq!(custom_error(env.configure_as(&guardian, "OIL", unpaused()).await), code(PerpsError::Unauthorized));
    assert_eq!(custom_error(env.settle("OIL", &guardian, 70 * USDC).await), code(PerpsError::Unauthorized));

    env.configure("OIL", unpaused()).await.unwrap();
    env.open("OIL", 0, 1, 5).await.unwrap();
}

#[tokio::test]
async fn admin_handover_takes_two_steps_and_covers_existing_markets() {
    let mut env = Env::new().await;
    let old_admin = env.authority.insecure_clone();
    let new_admin = env.funded_keypair().await;
    let stranger = env.funded_keypair().await;

    env.propose_admin(&old_admin, new_admin.pubkey()).await.unwrap();
    assert_eq!(custom_error(env.accept_admin(&stranger).await), code(PerpsError::Unauthorized));
    // Until accepted, the old admin is still in charge
    env.configure_as(&old_admin, "OIL", unpaused()).await.unwrap();

    env.accept_admin(&new_admin).await.unwrap();
    assert_eq!(custom_error(env.configure_as(&old_admin, "OIL", unpaused()).await), code(PerpsError::Unauthorized));
    assert_eq!(custom_error(env.propose_admin(&old_admin, old_admin.pubkey()).await), code(PerpsError::Unauthorized));

    // The new admin controls the market the old admin created, and can create markets
    env.configure_as(&new_admin, "OIL", unpaused()).await.unwrap();
    let gold_oracle = env.gold.oracle;
    env.init_market_as(&new_admin, "GOLD", oil_params("GOLD"), gold_oracle).await.unwrap();
}

// --- LP pool bootstrap and market config (docs/lp-pool.md §4.10, §4.12, §5) ---

#[tokio::test]
async fn initialize_pool_needs_the_admin_and_a_fresh_market() {
    let mut env = Env::new().await;
    env.init_market_only("GOLD").await.unwrap();
    let authority = env.authority.insecure_clone();
    let stranger = env.funded_keypair().await;
    let neutral = PoolParams::neutral();

    assert_eq!(custom_error(env.init_pool(&stranger, "GOLD", neutral).await), code(PerpsError::Unauthorized));
    let bad = PoolParams { backstop_threshold_bps: 0, ..neutral };
    assert_eq!(custom_error(env.init_pool(&authority, "GOLD", bad).await), code(PerpsError::InvalidPoolParams));
    for f in [
        (|m: &mut Market| m.insurance_fund = 1) as fn(&mut Market),
        |m: &mut Market| m.long_open_interest = 1,
        |m: &mut Market| m.short_open_interest = 1,
        |m: &mut Market| m.settlement_price = 1,
    ] {
        let fresh = env.market("GOLD").await;
        env.patch_market("GOLD", f).await;
        assert_eq!(custom_error(env.init_pool(&authority, "GOLD", neutral).await), code(PerpsError::MarketNotFresh));
        env.patch_market("GOLD", |m| *m = fresh).await;
    }

    // One unit sent straight to the vault does not block it (red-team F14)
    let payer = env.ctx.payer.insecure_clone();
    let ix = spl_token::instruction::mint_to(&spl_token::ID, &env.mint, &env.gold.vault_token, &payer.pubkey(), &[], 1).unwrap();
    send(&mut env.ctx, &[ix], &[&payer]).await.unwrap();
    env.init_pool(&authority, "GOLD", neutral).await.unwrap();
    let pool = env.pool("GOLD").await;
    assert_eq!((pool.market, pool.adl_keeper, pool.params, pool.lp_shares, pool.cash), (env.gold.market, authority.pubkey(), neutral, 0, 0));
    assert!(env.init_pool(&authority, "GOLD", neutral).await.is_err(), "a pool can only be created once");
}

#[tokio::test]
async fn markets_bootstrap_their_pool_in_one_transaction() {
    let mut env = Env::new().await;
    let pool = env.pool("OIL").await;
    assert_eq!((pool.lp_shares, pool.cash, pool.params), (support::LP_SEED, support::LP_SEED, PoolParams::neutral()));
    assert_eq!(env.token_balance(env.oil.vault_token).await, support::LP_SEED);

    // Without the seed the pool exists but has no LPs
    env.init_market_unseeded("GOLD").await.unwrap();
    let pool = env.pool("GOLD").await;
    assert_eq!((pool.lp_shares, pool.cash), (0, 0));
}

#[tokio::test]
async fn initialize_market_bounds_funding_interval_and_open_interest() {
    let mut env = Env::new().await;
    for params in [
        InitializeMarketParams { funding_interval: 59, ..oil_params("GOLD") },
        InitializeMarketParams { funding_interval: 86_401, ..oil_params("GOLD") },
        InitializeMarketParams { funding_interval: 0, ..oil_params("GOLD") },
        InitializeMarketParams { max_open_interest: perps_core::state::MAX_OPEN_INTEREST + 1, ..oil_params("GOLD") },
    ] {
        assert_eq!(custom_error(env.init_market_with("GOLD", params).await), code(PerpsError::InvalidMarketConfig));
    }
    let params = InitializeMarketParams { funding_interval: 60, max_open_interest: perps_core::state::MAX_OPEN_INTEREST, ..oil_params("GOLD") };
    env.init_market_with("GOLD", params).await.unwrap();
    assert_eq!(env.market("GOLD").await.funding_carry_secs, 0);
}

#[tokio::test]
async fn max_open_interest_is_editable_and_zero_keeps_it() {
    let mut env = Env::new().await.funded("OIL", 1_000 * USDC).await;
    let cap = MarketConfigParams { max_open_interest: 5 * CONTRACT, ..unpaused() };
    env.configure("OIL", cap).await.unwrap();
    assert_eq!(env.market("OIL").await.max_open_interest, 5 * CONTRACT);
    // unpaused() leaves it at 0: the cap is kept (red-team F19)
    env.configure("OIL", unpaused()).await.unwrap();
    assert_eq!(env.market("OIL").await.max_open_interest, 5 * CONTRACT);
    let too_big = MarketConfigParams { max_open_interest: perps_core::state::MAX_OPEN_INTEREST + 1, ..unpaused() };
    assert_eq!(custom_error(env.configure("OIL", too_big).await), code(PerpsError::InvalidMarketConfig));

    assert_eq!(custom_error(env.open("OIL", 0, 6, 5).await.map(|_| ())), code(PerpsError::OpenInterestCapExceeded));
    env.open("OIL", 0, 5, 5).await.unwrap();
    // Below the current OI it only blocks increases
    env.configure("OIL", MarketConfigParams { max_open_interest: CONTRACT, ..unpaused() }).await.unwrap();
    assert_eq!(custom_error(env.open("OIL", 0, 1, 5).await.map(|_| ())), code(PerpsError::OpenInterestCapExceeded));
}

/// The localnet-only test schedule (D46, docs/localnet.md): a default build
/// (what CI, scripts/build-sbf.sh and any production deploy produce) rejects
/// it with `InvalidMarketConfig` like any unknown schedule; a build with the
/// `localnet` feature accepts it. Run against SBF binaries, the test crate's
/// feature must match the binary's: `target/deploy` with the default
/// features, `target/deploy-localnet` with `--features localnet`.
#[tokio::test]
async fn test_schedule_needs_a_localnet_build() {
    use perps_core::session::SCHEDULE_TEST_HOURLY;
    let mut env = Env::new().await.funded("OIL", 1_000 * USDC).await;
    let test = MarketConfigParams { session_schedule: SCHEDULE_TEST_HOURLY, pre_close_window_secs: 300, ..weekend_config() };
    let result = env.configure("OIL", test).await;
    if !cfg!(feature = "localnet") {
        assert_eq!(custom_error(result), code(PerpsError::InvalidMarketConfig));
        assert_eq!(env.market("OIL").await.session_schedule, 0);
        // The production schedules are unaffected
        env.configure("OIL", weekend_config()).await.unwrap();
        assert_eq!(env.market("OIL").await.session_schedule, SCHEDULE_CME_GLOBEX);
        return;
    }
    result.unwrap();
    assert_eq!(env.market("OIL").await.session_schedule, SCHEDULE_TEST_HOURLY);
    // A test week starts at ts ≡ 0 (mod 3_600): open, daily break, weekend
    let week = 1_794_607_200 + 3_600;
    env.set_time(week + 100).await;
    let position = env.open("OIL", 0, 10, 5).await.unwrap();
    env.set_time(week + 760).await; // daily break [750, 810)
    assert_eq!(custom_error(env.open("OIL", 0, 10, 5).await.map(|_| ())), code(PerpsError::MarketClosed));
    env.set_time(week + 3_100).await; // weekend [3_000, 3_600)
    assert_eq!(custom_error(env.close("OIL", position).await), code(PerpsError::MarketClosed));
    assert_eq!(custom_error(env.update_funding("OIL").await), code(PerpsError::MarketClosed));
    env.set_time(week + 3_600 + 5).await; // reopened
    env.close("OIL", position).await.unwrap();
}

#[tokio::test]
async fn trading_needs_the_market_pool() {
    // docs/lp-pool.md §17.2 #1: every collateral and trading instruction
    // takes the pool, so a market without one cannot be used. The ones that
    // need an open position (close, add_margin, liquidate, venue_fill,
    // settle_position_pnl, adl) are in
    // venue::every_trading_instruction_needs_the_market_pool
    let mut env = Env::new().await;
    env.init_market_only("GOLD").await.unwrap();
    env.init_user("GOLD").await.unwrap();
    let not_init: u32 = anchor_lang::error::ErrorCode::AccountNotInitialized.into();
    assert_eq!(custom_error(env.deposit("GOLD", 10 * USDC).await), not_init);
    assert_eq!(custom_error(env.withdraw_via("GOLD", "GOLD", 0).await), not_init);
    assert_eq!(custom_error(env.open("GOLD", 0, 1, 5).await.map(|_| ())), not_init);
}
