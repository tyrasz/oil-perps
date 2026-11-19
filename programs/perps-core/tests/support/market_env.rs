//! Shared harness for perps-core's venue tests and the programs that sit beside
//! perps-core (orderbook, vamm, mm-registry): a perps-core config + OIL market
//! whose admin is the upgrade authority, with a separate guardian, a fresh
//! oracle and helpers for traders, venues, approvals and positions.
//! Included with `#[path]` from those crates' integration tests.

use anchor_lang::{AccountDeserialize, AccountSerialize, InstructionData, ToAccountMetas};
use perps_core::errors::PerpsError;
use perps_core::instructions::{
    InitializeMarketParams, MarketConfigParams, OpenPositionParams, ProtocolRolesParams, VenueApprovalParams, VenueParams,
};
use perps_core::state::{Market, Position, PositionStatus, UserAccount, Venue};
use pyth_sdk_solana::state::{AccountType, PriceAccount, PriceInfo, PriceStatus, MAGIC, VERSION_2};
pub use solana_program_test::{BanksClientError, ProgramTest, ProgramTestContext};
#[allow(unused_imports)]
pub use solana_sdk::{
    account::{Account, AccountSharedData},
    bpf_loader_upgradeable::{self, UpgradeableLoaderState},
    clock::Clock,
    compute_budget::ComputeBudgetInstruction,
    instruction::{AccountMeta, Instruction, InstructionError},
    program_pack::Pack,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    system_instruction, system_program, sysvar,
    transaction::{Transaction, TransactionError},
};

pub const PYTH_PROGRAM: Pubkey = Pubkey::new_from_array([0x50; 32]);
pub const USDC: u64 = 1_000_000;
pub const CONTRACT: u64 = 1_000_000;
pub const OIL_PRICE: f64 = 75.0;

// Fri 2026-11-13 17:00 ET (22:00 UTC) is a weekly close on the CME schedule.
pub const FRIDAY_CLOSE: i64 = 1_794_607_200;
/// Wed 2026-11-11 12:00 ET: in session, outside any pre-close window. `start` sets the clock here.
pub const WEDNESDAY: i64 = 1_794_416_400;
pub const SATURDAY: i64 = FRIDAY_CLOSE + 86_400;

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

pub fn user_pda(owner: &Pubkey, market: &Pubkey) -> Pubkey {
    pda(&[b"user", owner.as_ref(), market.as_ref()], &perps_core::ID)
}

/// `[b"position", owner, market, market.total_positions]`: open_position's PDA.
pub fn position_pda(owner: &Pubkey, market: &Pubkey, index: u64) -> Pubkey {
    pda(&[b"position", owner.as_ref(), market.as_ref(), &index.to_le_bytes()], &perps_core::ID)
}

pub fn vault_pda(market: &Pubkey) -> Pubkey {
    pda(&[b"vault", market.as_ref()], &perps_core::ID)
}

pub fn vault_token_pda(market: &Pubkey) -> Pubkey {
    pda(&[b"vault_token", market.as_ref()], &perps_core::ID)
}

/// The upgradeable loader's ProgramData address for `program`.
pub fn program_data_pda(program: &Pubkey) -> Pubkey {
    pda(&[program.as_ref()], &bpf_loader_upgradeable::id())
}

/// perps-core's Venue record for `program` in `market`.
pub fn venue_pda(market: &Pubkey, program: &Pubkey) -> Pubkey {
    pda(&[b"venue", market.as_ref(), program.as_ref()], &perps_core::ID)
}

/// The PDA `program` signs venue_fill with: `[b"perps_venue", market]` under `program`.
pub fn venue_authority(market: &Pubkey, program: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[b"perps_venue", market.as_ref()], program)
}

pub const ROLE_TAKER: u8 = perps_core::state::ROLE_TAKER;
pub const ROLE_MAKER: u8 = perps_core::state::ROLE_MAKER;

pub fn approval_pda(owner: &Pubkey, venue: &Pubkey, role: u8) -> Pubkey {
    pda(&[b"venue_approval", owner.as_ref(), venue.as_ref(), &[role]], &perps_core::ID)
}

/// Taker position of a venue fill: `[b"vposition", taker, market, nonce]`.
pub fn vposition(owner: &Pubkey, market: &Pubkey, nonce: u64) -> Pubkey {
    pda(&[b"vposition", owner.as_ref(), market.as_ref(), &nonce.to_le_bytes()], &perps_core::ID)
}

/// Maker position of a venue fill: `[b"vmaker", taker_position]`.
pub fn vmaker(taker_position: &Pubkey) -> Pubkey {
    pda(&[b"vmaker", taker_position.as_ref()], &perps_core::ID)
}

pub fn ix(program_id: Pubkey, accounts: impl ToAccountMetas, data: impl InstructionData) -> Instruction {
    Instruction { program_id, accounts: accounts.to_account_metas(None), data: data.data() }
}

fn perps_ix(accounts: impl ToAccountMetas, data: impl InstructionData) -> Instruction {
    ix(perps_core::ID, accounts, data)
}

pub fn compute_budget_ix(units: u32) -> Instruction {
    ComputeBudgetInstruction::set_compute_unit_limit(units)
}

/// Whether the tests run against SBF binaries (`SBF_OUT_DIR` set).
pub fn is_sbf() -> bool {
    std::env::var("SBF_OUT_DIR").is_ok() || std::env::var("BPF_OUT_DIR").is_ok()
}

pub async fn send(ctx: &mut ProgramTestContext, ixs: &[Instruction], signers: &[&Keypair]) -> Result<(), BanksClientError> {
    let payer = ctx.payer.insecure_clone();
    let mut all: Vec<&Keypair> = vec![&payer];
    all.extend_from_slice(signers);
    let blockhash = ctx.get_new_latest_blockhash().await.unwrap();
    ctx.banks_client.process_transaction(Transaction::new_signed_with_payer(ixs, Some(&payer.pubkey()), &all, blockhash)).await
}

/// Simulate, returning the result and the compute units consumed.
pub async fn simulate(ctx: &mut ProgramTestContext, ixs: &[Instruction], signers: &[&Keypair]) -> (Result<(), TransactionError>, u64) {
    let payer = ctx.payer.insecure_clone();
    let mut all: Vec<&Keypair> = vec![&payer];
    all.extend_from_slice(signers);
    let blockhash = ctx.get_new_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(ixs, Some(&payer.pubkey()), &all, blockhash);
    let sim = ctx.banks_client.simulate_transaction(tx).await.unwrap();
    let units = sim.simulation_details.map(|d| d.units_consumed).unwrap_or(0);
    (sim.result.expect("simulation ran"), units)
}

pub fn custom_error(result: Result<(), BanksClientError>) -> u32 {
    match result.expect_err("transaction should fail").unwrap() {
        TransactionError::InstructionError(_, InstructionError::Custom(c)) => c,
        other => panic!("unexpected error: {:?}", other),
    }
}

pub fn perps_err(e: PerpsError) -> u32 {
    e.into()
}

pub fn anchor_err(e: anchor_lang::error::ErrorCode) -> u32 {
    e.into()
}

/// A Pyth v2 price account: `price * 10^expo` with confidence `conf`, published at `timestamp`.
pub fn pyth_account(price: i64, conf: u64, expo: i32, timestamp: i64) -> Account {
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
    Account { lamports: 1_000_000_000, data: bytemuck::bytes_of(&acct).to_vec(), owner: PYTH_PROGRAM, executable: false, rent_epoch: 0 }
}

/// Dollars as a Pyth price with expo -8.
pub fn pyth_dollars(d: f64) -> i64 {
    (d * 1e8).round() as i64
}

/// Dollars in the program's 6-decimal format.
pub fn usd(d: f64) -> u64 {
    (d * 1e6).round() as u64
}

pub struct Base {
    pub ctx: ProgramTestContext,
    /// perps-core upgrade authority and admin
    pub admin: Keypair,
    /// Can pause markets and venues, nothing else
    pub guardian: Keypair,
    pub market: Pubkey,
    pub oracle: Pubkey,
    pub mint: Pubkey,
    pub vault: Pubkey,
    pub vault_token: Pubkey,
}

/// `pt` must already have the program under test added. Leaves the clock at
/// `WEDNESDAY` with a fresh $75 oracle price.
pub async fn start(mut pt: ProgramTest) -> Base {
    let admin = Keypair::new();
    let guardian = Keypair::new();
    let oracle = Pubkey::new_unique();
    pt.add_program("perps_core", perps_core::ID, solana_program_test::processor!(perps_process));
    pt.add_account(oracle, pyth_account(pyth_dollars(OIL_PRICE), 0, -8, 0));
    let state = UpgradeableLoaderState::ProgramData { slot: 0, upgrade_authority_address: Some(admin.pubkey()) };
    let program_data = program_data_pda(&perps_core::ID);
    pt.add_account(program_data, Account { lamports: 1_000_000_000, data: bincode::serialize(&state).unwrap(), owner: bpf_loader_upgradeable::id(), executable: false, rent_epoch: 0 });
    let mut ctx = pt.start_with_context().await;
    // Before the market exists, so its funding clock starts here too
    let mut clock: Clock = ctx.banks_client.get_sysvar().await.unwrap();
    clock.unix_timestamp = WEDNESDAY;
    ctx.set_sysvar(&clock);
    let payer = ctx.payer.insecure_clone();
    fund(&mut ctx, &admin.pubkey()).await;
    fund(&mut ctx, &guardian.pubkey()).await;

    let mint = Keypair::new();
    let rent = ctx.banks_client.get_rent().await.unwrap();
    send(&mut ctx, &[
        system_instruction::create_account(&payer.pubkey(), &mint.pubkey(), rent.minimum_balance(spl_token::state::Mint::LEN), spl_token::state::Mint::LEN as u64, &spl_token::ID),
        spl_token::instruction::initialize_mint(&spl_token::ID, &mint.pubkey(), &payer.pubkey(), None, 6).unwrap(),
    ], &[&mint]).await.unwrap();
    let mint = mint.pubkey();

    send(&mut ctx, &[perps_ix(perps_core::accounts::InitializeConfig {
        upgrade_authority: admin.pubkey(), config: config_pda(), program_data, system_program: system_program::ID,
    }, perps_core::instruction::InitializeConfig { params: ProtocolRolesParams { guardian: guardian.pubkey(), oracle_program: PYTH_PROGRAM } })], &[&admin]).await.unwrap();
    let market = pda(&[b"market", mint.as_ref(), b"OIL"], &perps_core::ID);
    let params = InitializeMarketParams {
        commodity: "OIL".into(), max_leverage: 20_000, maintenance_margin_ratio: 500, initial_margin_ratio: 1_000,
        taker_fee: 5, maker_fee: 2, liquidation_fee: 100, max_open_interest: 1_000_000_000_000, funding_interval: 3_600,
    };
    let (vault, vault_token) = (vault_pda(&market), vault_token_pda(&market));
    send(&mut ctx, &[perps_ix(perps_core::accounts::InitializeMarket {
        authority: admin.pubkey(), config: config_pda(), market, vault, vault_token_account: vault_token,
        collateral_mint: mint, pyth_price_feed: oracle, system_program: system_program::ID, token_program: spl_token::ID, rent: sysvar::rent::ID,
    }, perps_core::instruction::InitializeMarket { params })], &[&admin]).await.unwrap();
    let mut base = Base { ctx, admin, guardian, market, oracle, mint, vault, vault_token };
    set_time(&mut base, WEDNESDAY).await;
    base
}

pub async fn fund(ctx: &mut ProgramTestContext, who: &Pubkey) {
    let payer = ctx.payer.insecure_clone();
    send(ctx, &[system_instruction::transfer(&payer.pubkey(), who, 10_000_000_000)], &[]).await.unwrap();
}

/// A new keypair holding 10 SOL.
pub async fn funded_keypair(base: &mut Base) -> Keypair {
    let k = Keypair::new();
    fund(&mut base.ctx, &k.pubkey()).await;
    k
}

/// Hand the perps-core admin role from `base.admin` to a new key, returning it.
pub async fn rotate_admin(base: &mut Base) -> Keypair {
    let new_admin = Keypair::new();
    fund(&mut base.ctx, &new_admin.pubkey()).await;
    let old = base.admin.insecure_clone();
    send(&mut base.ctx, &[perps_ix(perps_core::accounts::AdminOnly { admin: old.pubkey(), config: config_pda() },
        perps_core::instruction::ProposeAdmin { new_admin: new_admin.pubkey() })], &[&old]).await.unwrap();
    send(&mut base.ctx, &[perps_ix(perps_core::accounts::AcceptAdmin { new_admin: new_admin.pubkey(), config: config_pda() },
        perps_core::instruction::AcceptAdmin {})], &[&new_admin]).await.unwrap();
    new_admin
}

// --- clock and oracle ---------------------------------------------------------

pub async fn now(base: &mut Base) -> i64 {
    base.ctx.banks_client.get_sysvar::<Clock>().await.unwrap().unix_timestamp
}

/// Warp the clock to `unix_timestamp` and restamp the oracle so its price stays fresh.
pub async fn set_time(base: &mut Base, unix_timestamp: i64) {
    let mut clock: Clock = base.ctx.banks_client.get_sysvar().await.unwrap();
    clock.unix_timestamp = unix_timestamp;
    base.ctx.set_sysvar(&clock);
    let mut acct = base.ctx.banks_client.get_account(base.oracle).await.unwrap().unwrap();
    let mut pa: PriceAccount = *bytemuck::from_bytes(&acct.data);
    pa.timestamp = unix_timestamp;
    pa.prev_timestamp = unix_timestamp;
    acct.data = bytemuck::bytes_of(&pa).to_vec();
    base.ctx.set_account(&base.oracle, &AccountSharedData::from(acct));
}

/// Set the oracle to `dollars`, published now.
pub async fn set_price(base: &mut Base, dollars: f64) {
    let t = now(base).await;
    set_price_raw(base, pyth_dollars(dollars), 0, t).await;
}

/// Set the oracle to a raw expo -8 price and confidence, published at `timestamp`.
pub async fn set_price_raw(base: &mut Base, price: i64, conf: u64, timestamp: i64) {
    base.ctx.set_account(&base.oracle, &AccountSharedData::from(pyth_account(price, conf, -8, timestamp)));
}

// --- accounts -----------------------------------------------------------------

pub async fn raw(base: &mut Base, address: Pubkey) -> Option<Account> {
    base.ctx.banks_client.get_account(address).await.unwrap()
}

pub async fn exists(base: &mut Base, address: Pubkey) -> bool {
    raw(base, address).await.map(|a| a.lamports > 0).unwrap_or(false)
}

pub async fn lamports(base: &mut Base, address: Pubkey) -> u64 {
    raw(base, address).await.map(|a| a.lamports).unwrap_or(0)
}

pub async fn load<T: AccountDeserialize>(base: &mut Base, address: Pubkey) -> T {
    try_load(base, address).await.expect("account exists")
}

pub async fn try_load<T: AccountDeserialize>(base: &mut Base, address: Pubkey) -> Option<T> {
    let acct = raw(base, address).await?;
    T::try_deserialize(&mut acct.data.as_slice()).ok()
}

/// Rewrite an Anchor account in place (e.g. to fake legacy state or an OI level).
pub async fn patch<T: AccountDeserialize + AccountSerialize>(base: &mut Base, address: Pubkey, f: impl FnOnce(&mut T)) {
    let mut acct = raw(base, address).await.expect("account exists");
    let mut value = T::try_deserialize(&mut acct.data.as_slice()).unwrap();
    f(&mut value);
    let mut data = Vec::with_capacity(acct.data.len());
    value.try_serialize(&mut data).unwrap();
    acct.data[..data.len()].copy_from_slice(&data);
    base.ctx.set_account(&address, &AccountSharedData::from(acct));
}

pub async fn market(base: &mut Base) -> Market {
    let m = base.market;
    load(base, m).await
}

pub async fn user(base: &mut Base, owner: &Pubkey) -> UserAccount {
    let u = user_pda(owner, &base.market);
    load(base, u).await
}

pub async fn token_balance(base: &mut Base, account: Pubkey) -> u64 {
    let acct = raw(base, account).await.expect("token account exists");
    spl_token::state::Account::unpack(&acct.data).unwrap().amount
}

pub async fn vault_token_balance(base: &mut Base) -> u64 {
    let v = base.vault_token;
    token_balance(base, v).await
}

// --- traders --------------------------------------------------------------------

/// A wallet with 10 SOL, a USDC token account and a perps-core UserAccount.
pub struct Trader {
    pub kp: Keypair,
    pub tokens: Pubkey,
    pub user: Pubkey,
}

impl Trader {
    pub fn key(&self) -> Pubkey {
        self.kp.pubkey()
    }
}

/// A token account for `owner` holding `amount` newly minted USDC.
pub async fn token_account(base: &mut Base, owner: &Pubkey, amount: u64) -> Pubkey {
    let payer = base.ctx.payer.insecure_clone();
    let account = Keypair::new();
    let rent = base.ctx.banks_client.get_rent().await.unwrap();
    let len = spl_token::state::Account::LEN;
    let mut ixs = vec![
        system_instruction::create_account(&payer.pubkey(), &account.pubkey(), rent.minimum_balance(len), len as u64, &spl_token::ID),
        spl_token::instruction::initialize_account(&spl_token::ID, &account.pubkey(), &base.mint, owner).unwrap(),
    ];
    if amount > 0 {
        ixs.push(spl_token::instruction::mint_to(&spl_token::ID, &base.mint, &account.pubkey(), &payer.pubkey(), &[], amount).unwrap());
    }
    send(&mut base.ctx, &ixs, &[&account]).await.unwrap();
    account.pubkey()
}

pub async fn mint_to(base: &mut Base, tokens: Pubkey, amount: u64) {
    let payer = base.ctx.payer.insecure_clone();
    let ix = spl_token::instruction::mint_to(&spl_token::ID, &base.mint, &tokens, &payer.pubkey(), &[], amount).unwrap();
    send(&mut base.ctx, &[ix], &[]).await.unwrap();
}

/// A trader with `usdc` deposited in the market (and nothing left in the wallet).
pub async fn trader(base: &mut Base, usdc: u64) -> Trader {
    let kp = funded_keypair(base).await;
    let tokens = token_account(base, &kp.pubkey(), usdc).await;
    let user = user_pda(&kp.pubkey(), &base.market);
    send(&mut base.ctx, &[perps_ix(perps_core::accounts::InitializeUser {
        owner: kp.pubkey(), market: base.market, user_account: user, system_program: system_program::ID,
    }, perps_core::instruction::InitializeUser {})], &[&kp]).await.unwrap();
    let t = Trader { kp, tokens, user };
    if usdc > 0 {
        deposit(base, &t, usdc).await.unwrap();
    }
    t
}

pub async fn deposit(base: &mut Base, t: &Trader, amount: u64) -> Result<(), BanksClientError> {
    let accounts = perps_core::accounts::DepositCollateral {
        owner: t.key(), user_account: t.user, market: base.market, vault: base.vault,
        vault_token_account: base.vault_token, user_token_account: t.tokens, token_program: spl_token::ID,
    };
    send(&mut base.ctx, &[perps_ix(accounts, perps_core::instruction::DepositCollateral { amount })], &[&t.kp]).await
}

pub async fn withdraw(base: &mut Base, t: &Trader, amount: u64) -> Result<(), BanksClientError> {
    let accounts = perps_core::accounts::WithdrawCollateral {
        owner: t.key(), user_account: t.user, market: base.market, vault: base.vault,
        vault_token_account: base.vault_token, user_token_account: t.tokens, token_program: spl_token::ID,
    };
    send(&mut base.ctx, &[perps_ix(accounts, perps_core::instruction::WithdrawCollateral { amount })], &[&t.kp]).await
}

// --- market admin ------------------------------------------------------------------

pub async fn pause(base: &mut Base, signer: &Keypair) -> Result<(), BanksClientError> {
    let accounts = perps_core::accounts::PauseMarket { signer: signer.pubkey(), config: config_pda(), market: base.market };
    send(&mut base.ctx, &[perps_ix(accounts, perps_core::instruction::PauseMarket {})], &[signer]).await
}

/// update_market_config as the admin.
pub async fn configure(base: &mut Base, params: MarketConfigParams) -> Result<(), BanksClientError> {
    let admin = base.admin.insecure_clone();
    let accounts = perps_core::accounts::UpdateMarketConfig { authority: admin.pubkey(), config: config_pda(), market: base.market };
    send(&mut base.ctx, &[perps_ix(accounts, perps_core::instruction::UpdateMarketConfig { params })], &[&admin]).await
}

/// CME Globex hours with 20% / 10% weekend margins from two hours before the weekly close.
pub fn cme_config() -> MarketConfigParams {
    MarketConfigParams {
        is_paused: false,
        session_schedule: perps_core::session::SCHEDULE_CME_GLOBEX,
        pre_close_window_secs: 7_200,
        weekend_initial_margin_ratio: 2_000,
        weekend_maintenance_margin_ratio: 1_000,
        max_confidence_bps: 100,
        min_oracle_price: 0,
    }
}

pub async fn settle(base: &mut Base, settlement_price: u64) -> Result<(), BanksClientError> {
    let admin = base.admin.insecure_clone();
    let accounts = perps_core::accounts::SettleMarket { authority: admin.pubkey(), config: config_pda(), market: base.market };
    send(&mut base.ctx, &[perps_ix(accounts, perps_core::instruction::SettleMarket { settlement_price })], &[&admin]).await
}

/// update_funding, cranked by the test payer.
pub async fn update_funding(base: &mut Base) -> Result<(), BanksClientError> {
    let keeper = base.ctx.payer.pubkey();
    let accounts = perps_core::accounts::UpdateFunding { keeper, market: base.market, pyth_price_feed: base.oracle };
    send(&mut base.ctx, &[perps_ix(accounts, perps_core::instruction::UpdateFunding {})], &[]).await
}

// --- positions ----------------------------------------------------------------------

/// open_position for `owner` at index `market.total_positions` as of now.
/// Returns the instruction and the position it creates, for sending later.
pub async fn open_ix(base: &mut Base, owner: &Pubkey, side: u8, size: u64, leverage: u32) -> (Instruction, Pubkey) {
    let index = market(base).await.total_positions;
    let position = position_pda(owner, &base.market, index);
    let accounts = perps_core::accounts::OpenPosition {
        owner: *owner, user_account: user_pda(owner, &base.market), market: base.market, position,
        pyth_price_feed: base.oracle, system_program: system_program::ID,
    };
    (perps_ix(accounts, perps_core::instruction::OpenPosition { params: OpenPositionParams { side, size, leverage } }), position)
}

/// open_position of `contracts` whole contracts at `leverage_x`.
pub async fn open(base: &mut Base, t: &Trader, side: u8, contracts: u64, leverage_x: u32) -> Result<Pubkey, BanksClientError> {
    let (ix, position) = open_ix(base, &t.key(), side, contracts * CONTRACT, leverage_x * 1_000).await;
    send(&mut base.ctx, &[ix], &[&t.kp]).await?;
    Ok(position)
}

pub fn close_ix(base: &Base, t: &Trader, position: Pubkey) -> Instruction {
    let accounts = perps_core::accounts::ClosePosition {
        owner: t.key(), user_account: t.user, market: base.market, position, vault: base.vault,
        vault_token_account: base.vault_token, user_token_account: t.tokens, pyth_price_feed: base.oracle,
        token_program: spl_token::ID, user_referral: None, referral_code: None, referrer_token_account: None,
    };
    perps_ix(accounts, perps_core::instruction::ClosePosition {})
}

/// Close `position`; the payout goes to `t.tokens`.
pub async fn close(base: &mut Base, t: &Trader, position: Pubkey) -> Result<(), BanksClientError> {
    let ix = close_ix(base, t, position);
    send(&mut base.ctx, &[ix], &[&t.kp]).await
}

/// `keeper` liquidates `owner`'s `position`; the reward goes to `keeper.tokens`.
pub async fn liquidate(base: &mut Base, keeper: &Trader, owner: &Pubkey, position: Pubkey) -> Result<(), BanksClientError> {
    let accounts = perps_core::accounts::Liquidate {
        liquidator: keeper.key(), position_owner: *owner, user_account: user_pda(owner, &base.market), market: base.market,
        position, vault: base.vault, vault_token_account: base.vault_token, liquidator_token_account: keeper.tokens,
        pyth_price_feed: base.oracle, token_program: spl_token::ID,
    };
    send(&mut base.ctx, &[perps_ix(accounts, perps_core::instruction::Liquidate {})], &[&keeper.kp]).await
}

/// Vault tokens == free balances + open positions' equity at `mark` (collateral +
/// PnL + accrued funding) + insurance fund, within 1 unit per position.
///
/// Holds only while every open position's counterparty is another listed open
/// position (venue pairs, both legs open, or at the closing mark): an
/// open_position position, or one leg of a pair, trades against the vault.
pub async fn assert_conservation(base: &mut Base, mark: u64, owners: &[Pubkey], positions: &[Pubkey]) {
    let m = market(base).await;
    let mut expected: i128 = m.insurance_fund as i128;
    for owner in owners {
        expected += user(base, owner).await.collateral_balance as i128;
    }
    let mut open = 0i128;
    for &address in positions {
        let p: Position = load(base, address).await;
        if p.status == PositionStatus::Open {
            open += 1;
            expected += p.collateral as i128 + p.unrealized_pnl(mark) as i128 + p.accrued_funding(m.cumulative_funding) as i128;
        }
    }
    let vault = vault_token_balance(base).await as i128;
    assert!((vault - expected).abs() <= open, "vault {} != expected {} ({} open positions)", vault, expected, open);
}

// --- venues -----------------------------------------------------------------------

/// mm-registry's defaults from the spec: 50 bps band, 15 s oracle, $50k per fill.
/// `min_signers` is 0 here; set it per venue.
pub fn default_venue_params() -> VenueParams {
    VenueParams { max_price_deviation_bps: 50, max_oracle_age_secs: 15, min_signers: 0, max_fill_notional: 50_000 * USDC }
}

/// Write a fake upgradeable-loader ProgramData for `program` deployed at `slot`.
/// Works for any program id, including builtins and programs loaded by `add_program`.
pub async fn add_venue_program_data(base: &mut Base, program: &Pubkey, slot: u64) {
    let state = UpgradeableLoaderState::ProgramData { slot, upgrade_authority_address: Some(base.admin.pubkey()) };
    let account = Account { lamports: 1_000_000_000, data: bincode::serialize(&state).unwrap(), owner: bpf_loader_upgradeable::id(), executable: false, rent_epoch: 0 };
    base.ctx.set_account(&program_data_pda(program), &AccountSharedData::from(account));
}

/// The slot `program`'s fake ProgramData says it was deployed at.
pub async fn program_slot(base: &mut Base, program: &Pubkey) -> u64 {
    let acct = raw(base, program_data_pda(program)).await.expect("ProgramData exists");
    match bincode::deserialize(&acct.data).unwrap() {
        UpgradeableLoaderState::ProgramData { slot, .. } => slot,
        other => panic!("not ProgramData: {:?}", other),
    }
}

/// Simulate an upgrade of `program`; returns the new slot.
pub async fn bump_program_slot(base: &mut Base, program: &Pubkey) -> u64 {
    let slot = program_slot(base, program).await + 1;
    add_venue_program_data(base, program, slot).await;
    slot
}

pub fn register_venue_ix(base: &Base, admin: &Pubkey, program: &Pubkey, params: VenueParams, expected_program_slot: u64) -> Instruction {
    let accounts = perps_core::accounts::RegisterVenue {
        admin: *admin, config: config_pda(), market: base.market, venue_program: *program,
        venue_program_data: program_data_pda(program), venue: venue_pda(&base.market, program), system_program: system_program::ID,
    };
    perps_ix(accounts, perps_core::instruction::RegisterVenue { params, expected_program_slot })
}

pub async fn try_register_venue(base: &mut Base, signer: &Keypair, program: &Pubkey, params: VenueParams, expected_program_slot: u64) -> Result<(), BanksClientError> {
    let ix = register_venue_ix(base, &signer.pubkey(), program, params, expected_program_slot);
    send(&mut base.ctx, &[ix], &[signer]).await
}

/// Register `program` as a venue of the market as the admin, creating its fake
/// ProgramData (slot 1) if there is none. Returns the Venue address.
pub async fn register_venue(base: &mut Base, program: &Pubkey, params: VenueParams) -> Pubkey {
    if !exists(base, program_data_pda(program)).await {
        add_venue_program_data(base, program, 1).await;
    }
    let slot = program_slot(base, program).await;
    let admin = base.admin.insecure_clone();
    try_register_venue(base, &admin, program, params, slot).await.unwrap();
    venue_pda(&base.market, program)
}

pub async fn update_venue(base: &mut Base, signer: &Keypair, program: &Pubkey, params: VenueParams, enabled: bool, expected_program_slot: u64) -> Result<(), BanksClientError> {
    let accounts = perps_core::accounts::UpdateVenue {
        admin: signer.pubkey(), config: config_pda(), venue: venue_pda(&base.market, program), venue_program_data: program_data_pda(program),
    };
    send(&mut base.ctx, &[perps_ix(accounts, perps_core::instruction::UpdateVenue { params, enabled, expected_program_slot })], &[signer]).await
}

pub async fn disable_venue(base: &mut Base, signer: &Keypair, program: &Pubkey) -> Result<(), BanksClientError> {
    let accounts = perps_core::accounts::DisableVenue { signer: signer.pubkey(), config: config_pda(), venue: venue_pda(&base.market, program) };
    send(&mut base.ctx, &[perps_ix(accounts, perps_core::instruction::DisableVenue {})], &[signer]).await
}

pub async fn set_venue_fills_paused(base: &mut Base, signer: &Keypair, paused: bool) -> Result<(), BanksClientError> {
    let accounts = perps_core::accounts::SetVenueFillsPaused { signer: signer.pubkey(), config: config_pda() };
    send(&mut base.ctx, &[perps_ix(accounts, perps_core::instruction::SetVenueFillsPaused { paused })], &[signer]).await
}

pub async fn load_venue(base: &mut Base, program: &Pubkey) -> Venue {
    let v = venue_pda(&base.market, program);
    load(base, v).await
}

// --- approvals ----------------------------------------------------------------------

/// No limit price or deviation bound, the market's max leverage, a day to expiry.
pub fn approval_params(allowance: u64, now: i64) -> VenueApprovalParams {
    VenueApprovalParams { allowance, max_leverage: 20_000, max_buy_price: 0, min_sell_price: 0, max_price_deviation_bps: 0, expires_at: now + 86_400 }
}

pub fn approve_ix(base: &Base, owner: &Pubkey, venue: &Pubkey, role: u8, params: VenueApprovalParams) -> Instruction {
    let accounts = perps_core::accounts::ApproveVenue {
        owner: *owner, market: base.market, venue: *venue, approval: approval_pda(owner, venue, role), system_program: system_program::ID,
    };
    perps_ix(accounts, perps_core::instruction::ApproveVenue { role, params })
}

/// Returns the approval address.
pub async fn approve(base: &mut Base, owner: &Keypair, venue: &Pubkey, role: u8, params: VenueApprovalParams) -> Result<Pubkey, BanksClientError> {
    let ix = approve_ix(base, &owner.pubkey(), venue, role, params);
    send(&mut base.ctx, &[ix], &[owner]).await?;
    Ok(approval_pda(&owner.pubkey(), venue, role))
}

pub fn update_approval_ix(base: &Base, owner: &Pubkey, venue: &Pubkey, role: u8, params: VenueApprovalParams) -> Instruction {
    let accounts = perps_core::accounts::UpdateVenueApproval {
        owner: *owner, market: base.market, venue: *venue, approval: approval_pda(owner, venue, role),
    };
    perps_ix(accounts, perps_core::instruction::UpdateVenueApproval { params })
}

pub async fn update_approval(base: &mut Base, owner: &Keypair, venue: &Pubkey, role: u8, params: VenueApprovalParams) -> Result<(), BanksClientError> {
    let ix = update_approval_ix(base, &owner.pubkey(), venue, role, params);
    send(&mut base.ctx, &[ix], &[owner]).await
}

pub fn revoke_ix(owner: &Pubkey, venue: &Pubkey, role: u8) -> Instruction {
    let accounts = perps_core::accounts::RevokeVenueApproval { owner: *owner, approval: approval_pda(owner, venue, role) };
    perps_ix(accounts, perps_core::instruction::RevokeVenueApproval {})
}

pub async fn revoke(base: &mut Base, owner: &Keypair, venue: &Pubkey, role: u8) -> Result<(), BanksClientError> {
    send(&mut base.ctx, &[revoke_ix(&owner.pubkey(), venue, role)], &[owner]).await
}

// --- venue fills ----------------------------------------------------------------------

/// Every account venue_fill takes, derived for `venue_program` in the market.
/// Venue programs forward these through their own CPI accounts.
#[derive(Clone, Copy, Debug)]
pub struct FillAccounts {
    pub venue_authority: Pubkey,
    pub venue_authority_bump: u8,
    pub config: Pubkey,
    pub market: Pubkey,
    pub venue: Pubkey,
    pub venue_program_data: Pubkey,
    pub pyth_price_feed: Pubkey,
    pub taker: Pubkey,
    pub taker_user: Pubkey,
    pub taker_approval: Pubkey,
    pub taker_position: Pubkey,
    pub maker: Pubkey,
    pub maker_user: Pubkey,
    pub maker_approval: Pubkey,
    pub maker_position: Pubkey,
}

pub fn fill_accounts(base: &Base, venue_program: &Pubkey, taker: &Pubkey, maker: &Pubkey, position_nonce: u64) -> FillAccounts {
    let market = base.market;
    let (venue_authority, venue_authority_bump) = venue_authority(&market, venue_program);
    let venue = venue_pda(&market, venue_program);
    let taker_position = vposition(taker, &market, position_nonce);
    FillAccounts {
        venue_authority,
        venue_authority_bump,
        config: config_pda(),
        market,
        venue,
        venue_program_data: program_data_pda(venue_program),
        pyth_price_feed: base.oracle,
        taker: *taker,
        taker_user: user_pda(taker, &market),
        taker_approval: approval_pda(taker, &venue, ROLE_TAKER),
        taker_position,
        maker: *maker,
        maker_user: user_pda(maker, &market),
        maker_approval: approval_pda(maker, &venue, ROLE_MAKER),
        maker_position: vmaker(&taker_position),
    }
}

impl FillAccounts {
    /// perps-core's account struct, in venue_fill's order.
    pub fn perps_accounts(&self, payer: &Pubkey) -> perps_core::accounts::VenueFill {
        perps_core::accounts::VenueFill {
            venue_authority: self.venue_authority,
            payer: *payer,
            config: self.config,
            market: self.market,
            venue: self.venue,
            venue_program_data: self.venue_program_data,
            pyth_price_feed: self.pyth_price_feed,
            taker: self.taker,
            taker_user: self.taker_user,
            taker_approval: self.taker_approval,
            taker_position: self.taker_position,
            maker: self.maker,
            maker_user: self.maker_user,
            maker_approval: self.maker_approval,
            maker_position: self.maker_position,
            system_program: system_program::ID,
        }
    }

    /// The perps-core state a fill reads or writes, for asserting a rejected
    /// fill changed nothing: both UserAccounts and approvals (raw bytes) and
    /// whether each position account exists.
    pub async fn snapshot(&self, base: &mut Base) -> FillSnapshot {
        let mut accounts = Vec::new();
        for a in [self.taker_user, self.maker_user, self.taker_approval, self.maker_approval] {
            accounts.push(raw(base, a).await.map(|a| a.data));
        }
        let positions = [exists(base, self.taker_position).await, exists(base, self.maker_position).await];
        FillSnapshot { accounts, positions }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct FillSnapshot {
    pub accounts: Vec<Option<Vec<u8>>>,
    pub positions: [bool; 2],
}
