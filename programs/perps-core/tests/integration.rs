//! End-to-end tests: the real perps-core program runs in-process under
//! solana-program-test, with SPL Token and a fake Pyth price account whose
//! price and timestamp the tests control.

use anchor_lang::{AccountDeserialize, InstructionData, ToAccountMetas};
use perps_core::errors::PerpsError;
use perps_core::instructions::{InitializeMarketParams, MarketConfigParams, OpenPositionParams};
use perps_core::session::SCHEDULE_CME_GLOBEX;
use perps_core::state::{Market, Position, PositionStatus, UserAccount};
use pyth_sdk_solana::state::{AccountType, PriceAccount, PriceInfo, PriceStatus, MAGIC, VERSION_2};
use solana_program_test::{processor, BanksClientError, ProgramTest, ProgramTestContext};
use solana_sdk::{
    account::{Account, AccountSharedData},
    clock::Clock,
    instruction::{Instruction, InstructionError},
    program_pack::Pack,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    system_instruction, system_program, sysvar,
    transaction::{Transaction, TransactionError},
};

const USDC: u64 = 1_000_000;
const CONTRACT: u64 = 1_000_000;

// Fri 2026-11-13 17:00 ET (22:00 UTC) is a weekly close.
const FRIDAY_CLOSE: i64 = 1_794_607_200;
const WEDNESDAY: i64 = 1_794_416_400; // Wed 12:00 ET
const FRIDAY_PRE_CLOSE: i64 = FRIDAY_CLOSE - 3_600; // inside a 2h window
const SATURDAY: i64 = FRIDAY_CLOSE + 86_400;

fn process(program_id: &Pubkey, accounts: &[solana_sdk::account_info::AccountInfo], data: &[u8]) -> solana_sdk::entrypoint::ProgramResult {
    // Anchor's entrypoint wants one lifetime for the slice and its elements.
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
    authority: Keypair,
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

fn user_account(owner: &Pubkey, market: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"user", owner.as_ref(), market.as_ref()], &perps_core::ID).0
}

/// A Pyth v2 price account: `price * 10^expo` with confidence `conf`.
fn pyth_account(price: i64, conf: u64, expo: i32, timestamp: i64) -> Account {
    let mut acct = PriceAccount {
        magic: MAGIC,
        ver: VERSION_2,
        atype: AccountType::Price as u32,
        expo,
        timestamp,
        prev_timestamp: timestamp,
        prev_price: price,
        prev_conf: conf,
        ..Default::default()
    };
    acct.agg = PriceInfo { price, conf, status: PriceStatus::Trading, ..Default::default() };
    Account {
        lamports: 1_000_000_000,
        data: bytemuck::bytes_of(&acct).to_vec(),
        owner: Pubkey::new_unique(),
        executable: false,
        rent_epoch: 0,
    }
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

impl Env {
    async fn new() -> Env {
        let mut pt = ProgramTest::new("perps_core", perps_core::ID, processor!(process));
        let oil_oracle = Pubkey::new_unique();
        let gold_oracle = Pubkey::new_unique();
        pt.add_account(oil_oracle, pyth_account(dollars(75.0), 0, -8, WEDNESDAY));
        pt.add_account(gold_oracle, pyth_account(dollars(2_000.0), 0, -8, WEDNESDAY));
        let mut ctx = pt.start_with_context().await;

        let authority = Keypair::new();
        let mint = Keypair::new();
        let alice = Keypair::new();
        let keeper = Keypair::new();
        let payer = ctx.payer.insecure_clone();
        for who in [&authority, &alice, &keeper] {
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
            ctx,
        };
        env.alice.token_account = env.token_account(&env.alice.keypair.pubkey(), 10_000 * USDC).await;
        env.keeper.token_account = env.token_account(&env.keeper.keypair.pubkey(), 0).await;

        env.set_time(WEDNESDAY).await;
        env.init_market("OIL").await.unwrap();
        env
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
            let mut pa: PriceAccount = *bytemuck::from_bytes(&acct.data);
            pa.timestamp = unix_timestamp;
            pa.prev_timestamp = unix_timestamp;
            acct.data = bytemuck::bytes_of(&pa).to_vec();
            self.ctx.set_account(&oracle, &AccountSharedData::from(acct));
        }
    }

    async fn now(&mut self) -> i64 {
        self.ctx.banks_client.get_sysvar::<Clock>().await.unwrap().unix_timestamp
    }

    async fn set_price(&mut self, commodity: &str, price: i64, conf: u64) {
        let now = self.now().await;
        let oracle = self.keys(commodity).oracle;
        self.ctx.set_account(&oracle, &AccountSharedData::from(pyth_account(price, conf, -8, now)));
    }

    async fn init_market(&mut self, commodity: &str) -> Result<(), BanksClientError> {
        let k = *self.keys(commodity);
        let accounts = perps_core::accounts::InitializeMarket {
            authority: self.authority.pubkey(),
            market: k.market,
            vault: k.vault,
            vault_token_account: k.vault_token,
            collateral_mint: self.mint,
            pyth_price_feed: k.oracle,
            system_program: system_program::ID,
            token_program: spl_token::ID,
            rent: sysvar::rent::ID,
        };
        let data = perps_core::instruction::InitializeMarket { params: oil_params(commodity) };
        let authority = self.authority.insecure_clone();
        self.exec(accounts, data, &[&authority]).await
    }

    async fn configure(&mut self, commodity: &str, params: MarketConfigParams) -> Result<(), BanksClientError> {
        let accounts = perps_core::accounts::UpdateMarketConfig { authority: self.authority.pubkey(), market: self.keys(commodity).market };
        let authority = self.authority.insecure_clone();
        self.exec(accounts, perps_core::instruction::UpdateMarketConfig { params }, &[&authority]).await
    }

    async fn settle(&mut self, commodity: &str, signer: &Keypair, price: u64) -> Result<(), BanksClientError> {
        let accounts = perps_core::accounts::SettleMarket { authority: signer.pubkey(), market: self.keys(commodity).market };
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
        };
        let params = OpenPositionParams { side, size: contracts * CONTRACT, leverage: leverage_x * 1_000 };
        self.exec(accounts, perps_core::instruction::OpenPosition { params }, &[&owner]).await?;
        Ok(position)
    }

    async fn close(&mut self, commodity: &str, position: Pubkey) -> Result<(), BanksClientError> {
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
            user_referral: None,
            referral_code: None,
        };
        self.exec(accounts, perps_core::instruction::ClosePosition {}, &[&owner]).await
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
        };
        self.exec(accounts, perps_core::instruction::Liquidate {}, &[&keeper]).await
    }

    async fn update_funding(&mut self, commodity: &str) -> Result<(), BanksClientError> {
        let keeper = self.keeper.keypair.insecure_clone();
        let k = *self.keys(commodity);
        let accounts = perps_core::accounts::UpdateFunding { keeper: keeper.pubkey(), market: k.market, pyth_price_feed: k.oracle };
        self.exec(accounts, perps_core::instruction::UpdateFunding {}, &[&keeper]).await
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
    ctx.banks_client.process_transaction(tx).await
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

    // GOLD account holds nothing, so GOLD's vault can't be drained with OIL deposits
    assert_eq!(custom_error(env.withdraw_via("GOLD", "GOLD", 100 * USDC).await), code(PerpsError::InsufficientCollateral));
    // Passing the OIL user account against GOLD's market fails the PDA check
    assert!(env.withdraw_via("GOLD", "OIL", 100 * USDC).await.is_err());

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
    // Reward: 1% of the 5% maintenance margin on $750 = $0.375
    assert_eq!(env.token_balance(env.keeper.token_account).await, 375_000);
}

#[tokio::test]
async fn settlement_closes_positions_when_the_oracle_breaks() {
    let mut env = Env::new().await.funded("OIL", 1_000 * USDC).await;
    let position = env.open("OIL", 0, 10, 5).await.unwrap();
    let before = env.token_balance(env.alice.token_account).await;

    // April 2020: the price goes negative and the oracle can no longer be used
    env.set_price("OIL", dollars(-37.63), 0).await;
    assert_eq!(custom_error(env.close("OIL", position).await), code(PerpsError::InvalidOraclePrice));

    let stranger = Keypair::new();
    assert_eq!(custom_error(env.settle("OIL", &stranger, 70 * USDC).await), code(PerpsError::Unauthorized));
    let authority = env.authority.insecure_clone();
    env.settle("OIL", &authority, 70 * USDC).await.unwrap();

    // Closes at $70: $150 - $50 loss - $0.375 fee
    env.close("OIL", position).await.unwrap();
    assert_eq!(env.token_balance(env.alice.token_account).await - before, 99_625_000);

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
