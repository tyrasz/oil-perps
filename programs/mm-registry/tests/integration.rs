//! End-to-end tests for the MM registry, run in-process with perps-core loaded
//! alongside (registries belong to perps-core markets and are created by its
//! admin).

use anchor_lang::{InstructionData, ToAccountMetas};
use mm_registry::instructions::{FillQuoteParams, InitializeRegistryParams, PostQuoteParams};
use mm_registry::MmRegistryError;
use perps_core::instructions::{InitializeMarketParams, ProtocolRolesParams};
use pyth_sdk_solana::state::{AccountType, PriceAccount, PriceInfo, PriceStatus, MAGIC, VERSION_2};
use solana_program_test::{processor, BanksClientError, ProgramTest, ProgramTestContext};
use solana_sdk::{
    account::Account,
    bpf_loader_upgradeable::{self, UpgradeableLoaderState},
    instruction::{Instruction, InstructionError},
    program_pack::Pack,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    system_instruction, system_program, sysvar,
    transaction::{Transaction, TransactionError},
};

const USDC: u64 = 1_000_000;
const CONTRACT: u64 = 1_000_000;
const PYTH_PROGRAM: Pubkey = Pubkey::new_from_array([0x50; 32]);

fn perps_process(program_id: &Pubkey, accounts: &[solana_sdk::account_info::AccountInfo], data: &[u8]) -> solana_sdk::entrypoint::ProgramResult {
    let accounts = Box::leak(Box::new(accounts.to_vec()));
    perps_core::entry(program_id, accounts, data)
}

fn mm_process(program_id: &Pubkey, accounts: &[solana_sdk::account_info::AccountInfo], data: &[u8]) -> solana_sdk::entrypoint::ProgramResult {
    let accounts = Box::leak(Box::new(accounts.to_vec()));
    mm_registry::entry(program_id, accounts, data)
}

fn pda(seeds: &[&[u8]], program: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(seeds, program).0
}

async fn send(ctx: &mut ProgramTestContext, ixs: &[Instruction], signers: &[&Keypair]) -> Result<(), BanksClientError> {
    let payer = ctx.payer.insecure_clone();
    let mut all: Vec<&Keypair> = vec![&payer];
    all.extend_from_slice(signers);
    let blockhash = ctx.get_new_latest_blockhash().await.unwrap();
    ctx.banks_client.process_transaction(Transaction::new_signed_with_payer(ixs, Some(&payer.pubkey()), &all, blockhash)).await
}

fn ix(program_id: Pubkey, accounts: impl ToAccountMetas, data: impl InstructionData) -> Instruction {
    Instruction { program_id, accounts: accounts.to_account_metas(None), data: data.data() }
}

fn custom_error(result: Result<(), BanksClientError>) -> u32 {
    match result.expect_err("transaction should fail").unwrap() {
        TransactionError::InstructionError(_, InstructionError::Custom(c)) => c,
        other => panic!("unexpected error: {:?}", other),
    }
}

fn mm_error(e: MmRegistryError) -> u32 {
    e.into()
}

struct Env {
    ctx: ProgramTestContext,
    admin: Keypair,
    mm: Keypair,
    taker: Keypair,
    mint: Pubkey,
    other_mint: Pubkey,
    market: Pubkey,
    registry: Pubkey,
    market_maker: Pubkey,
    vault: Pubkey,
    mm_tokens: Pubkey,
    taker_tokens: Pubkey,
}

async fn create_mint(ctx: &mut ProgramTestContext) -> Pubkey {
    let payer = ctx.payer.insecure_clone();
    let mint = Keypair::new();
    let rent = ctx.banks_client.get_rent().await.unwrap();
    send(ctx, &[
        system_instruction::create_account(&payer.pubkey(), &mint.pubkey(), rent.minimum_balance(spl_token::state::Mint::LEN), spl_token::state::Mint::LEN as u64, &spl_token::ID),
        spl_token::instruction::initialize_mint(&spl_token::ID, &mint.pubkey(), &payer.pubkey(), None, 6).unwrap(),
    ], &[&mint]).await.unwrap();
    mint.pubkey()
}

async fn token_account(ctx: &mut ProgramTestContext, mint: &Pubkey, owner: &Pubkey, amount: u64) -> Pubkey {
    let payer = ctx.payer.insecure_clone();
    let account = Keypair::new();
    let rent = ctx.banks_client.get_rent().await.unwrap();
    let len = spl_token::state::Account::LEN;
    send(ctx, &[
        system_instruction::create_account(&payer.pubkey(), &account.pubkey(), rent.minimum_balance(len), len as u64, &spl_token::ID),
        spl_token::instruction::initialize_account(&spl_token::ID, &account.pubkey(), mint, owner).unwrap(),
        spl_token::instruction::mint_to(&spl_token::ID, mint, &account.pubkey(), &payer.pubkey(), &[], amount).unwrap(),
    ], &[&account]).await.unwrap();
    account.pubkey()
}

async fn balance(ctx: &mut ProgramTestContext, account: Pubkey) -> Option<u64> {
    let acct = ctx.banks_client.get_account(account).await.unwrap()?;
    Some(spl_token::state::Account::unpack(&acct.data).unwrap().amount)
}

impl Env {
    /// perps-core config + an OIL market (admin = upgrade authority), no registry yet.
    async fn new() -> Env {
        let admin = Keypair::new();
        let oracle = Pubkey::new_unique();
        let mut pt = ProgramTest::new("perps_core", perps_core::ID, processor!(perps_process));
        pt.add_program("mm_registry", mm_registry::ID, processor!(mm_process));
        let mut feed = PriceAccount { magic: MAGIC, ver: VERSION_2, atype: AccountType::Price as u32, expo: -8, ..Default::default() };
        feed.agg = PriceInfo { price: 7_500_000_000, conf: 0, status: PriceStatus::Trading, ..Default::default() };
        pt.add_account(oracle, Account { lamports: 1_000_000_000, data: bytemuck::bytes_of(&feed).to_vec(), owner: PYTH_PROGRAM, executable: false, rent_epoch: 0 });
        let state = UpgradeableLoaderState::ProgramData { slot: 0, upgrade_authority_address: Some(admin.pubkey()) };
        let program_data = pda(&[perps_core::ID.as_ref()], &bpf_loader_upgradeable::id());
        pt.add_account(program_data, Account { lamports: 1_000_000_000, data: bincode::serialize(&state).unwrap(), owner: bpf_loader_upgradeable::id(), executable: false, rent_epoch: 0 });
        let mut ctx = pt.start_with_context().await;

        let payer = ctx.payer.insecure_clone();
        let mm = Keypair::new();
        let taker = Keypair::new();
        for who in [&admin, &mm, &taker] {
            send(&mut ctx, &[system_instruction::transfer(&payer.pubkey(), &who.pubkey(), 10_000_000_000)], &[]).await.unwrap();
        }
        let mint = create_mint(&mut ctx).await;
        let other_mint = create_mint(&mut ctx).await;
        let mm_tokens = token_account(&mut ctx, &mint, &mm.pubkey(), 10_000 * USDC).await;
        let taker_tokens = token_account(&mut ctx, &mint, &taker.pubkey(), 10_000 * USDC).await;

        // perps-core: protocol config, then the OIL market
        let config = pda(&[b"config"], &perps_core::ID);
        send(&mut ctx, &[ix(perps_core::ID, perps_core::accounts::InitializeConfig {
            upgrade_authority: admin.pubkey(),
            config,
            program_data,
            system_program: system_program::ID,
        }, perps_core::instruction::InitializeConfig { params: ProtocolRolesParams { guardian: admin.pubkey(), oracle_program: PYTH_PROGRAM } })], &[&admin]).await.unwrap();
        let market = pda(&[b"market", mint.as_ref(), b"OIL"], &perps_core::ID);
        let vault = pda(&[b"vault", market.as_ref()], &perps_core::ID);
        let vault_token = pda(&[b"vault_token", market.as_ref()], &perps_core::ID);
        let params = InitializeMarketParams {
            commodity: "OIL".into(), max_leverage: 20_000, maintenance_margin_ratio: 500, initial_margin_ratio: 1_000,
            taker_fee: 5, maker_fee: 2, liquidation_fee: 100, max_open_interest: 1_000_000 * CONTRACT, funding_interval: 3_600,
        };
        send(&mut ctx, &[ix(perps_core::ID, perps_core::accounts::InitializeMarket {
            authority: admin.pubkey(), config, market, vault, vault_token_account: vault_token, collateral_mint: mint,
            pyth_price_feed: oracle, system_program: system_program::ID, token_program: spl_token::ID, rent: sysvar::rent::ID,
        }, perps_core::instruction::InitializeMarket { params })], &[&admin]).await.unwrap();

        let registry = pda(&[b"mm_registry", market.as_ref()], &mm_registry::ID);
        let market_maker = pda(&[b"market_maker", registry.as_ref(), mm.pubkey().as_ref()], &mm_registry::ID);
        let mm_vault = pda(&[b"mm_collateral", market_maker.as_ref()], &mm_registry::ID);
        Env { ctx, admin, mm, taker, mint, other_mint, market, registry, market_maker, vault: mm_vault, mm_tokens, taker_tokens }
    }

    async fn init_registry(&mut self, signer: &Keypair, mint: Pubkey) -> Result<(), BanksClientError> {
        let accounts = mm_registry::accounts::InitializeRegistry {
            authority: signer.pubkey(),
            config: pda(&[b"config"], &perps_core::ID),
            registry: self.registry,
            market: self.market,
            collateral_mint: mint,
            system_program: system_program::ID,
            token_program: spl_token::ID,
        };
        let params = InitializeRegistryParams { min_collateral: 100 * USDC, max_spread: 100, min_quote_size: CONTRACT, max_quote_size: 1_000 * CONTRACT, mm_fee: 10 };
        send(&mut self.ctx, &[ix(mm_registry::ID, accounts, mm_registry::instruction::InitializeRegistry { params })], &[signer]).await
    }

    async fn register(&mut self, amount: u64) {
        let mm = self.mm.insecure_clone();
        let accounts = mm_registry::accounts::RegisterMm {
            owner: mm.pubkey(), registry: self.registry, market_maker: self.market_maker, mm_collateral_account: self.vault,
            owner_token_account: self.mm_tokens, collateral_mint: self.mint, system_program: system_program::ID,
            token_program: spl_token::ID, rent: sysvar::rent::ID,
        };
        send(&mut self.ctx, &[ix(mm_registry::ID, accounts, mm_registry::instruction::RegisterMm { initial_collateral: amount })], &[&mm]).await.unwrap();
    }

    async fn deposit(&mut self, amount: u64) -> Result<(), BanksClientError> {
        let mm = self.mm.insecure_clone();
        let accounts = mm_registry::accounts::DepositCollateral {
            owner: mm.pubkey(), registry: self.registry, market_maker: self.market_maker, mm_collateral_account: self.vault,
            owner_token_account: self.mm_tokens, token_program: spl_token::ID,
        };
        send(&mut self.ctx, &[ix(mm_registry::ID, accounts, mm_registry::instruction::DepositCollateral { amount })], &[&mm]).await
    }

    async fn withdraw(&mut self, amount: u64) -> Result<(), BanksClientError> {
        let mm = self.mm.insecure_clone();
        let accounts = mm_registry::accounts::WithdrawCollateral {
            owner: mm.pubkey(), registry: self.registry, market_maker: self.market_maker, mm_collateral_account: self.vault,
            owner_token_account: self.mm_tokens, token_program: spl_token::ID,
        };
        send(&mut self.ctx, &[ix(mm_registry::ID, accounts, mm_registry::instruction::WithdrawCollateral { amount })], &[&mm]).await
    }

    fn quote(&self, index: u64) -> Pubkey {
        pda(&[b"quote", self.market_maker.as_ref(), &index.to_le_bytes()], &mm_registry::ID)
    }

    async fn post_quote(&mut self, index: u64) -> Result<(), BanksClientError> {
        let mm = self.mm.insecure_clone();
        let accounts = mm_registry::accounts::PostQuote {
            owner: mm.pubkey(), registry: self.registry, market_maker: self.market_maker, quote: self.quote(index), system_program: system_program::ID,
        };
        // $74.95 / $75.00 for 10 contracts each side: locks 10% of $750
        let params = PostQuoteParams { bid_price: 74_950_000, bid_size: 10 * CONTRACT, ask_price: 75_000_000, ask_size: 10 * CONTRACT, min_fill_size: CONTRACT, expires_in: 0 };
        send(&mut self.ctx, &[ix(mm_registry::ID, accounts, mm_registry::instruction::PostQuote { params })], &[&mm]).await
    }

    async fn cancel_quote(&mut self, index: u64) -> Result<(), BanksClientError> {
        let mm = self.mm.insecure_clone();
        let accounts = mm_registry::accounts::CancelQuote { owner: mm.pubkey(), registry: self.registry, market_maker: self.market_maker, quote: self.quote(index) };
        send(&mut self.ctx, &[ix(mm_registry::ID, accounts, mm_registry::instruction::CancelQuote {})], &[&mm]).await
    }
}

#[tokio::test]
async fn only_the_perps_admin_creates_registries_with_the_market_mint() {
    let mut env = Env::new().await;
    let taker = env.taker.insecure_clone();
    let admin = env.admin.insecure_clone();
    let mint = env.mint;
    assert_eq!(custom_error(env.init_registry(&taker, mint).await), mm_error(MmRegistryError::Unauthorized));
    let other = env.other_mint;
    assert_eq!(custom_error(env.init_registry(&admin, other).await), mm_error(MmRegistryError::InvalidCollateralMint));
    env.init_registry(&admin, mint).await.unwrap();
}

#[tokio::test]
async fn market_makers_get_their_collateral_back() {
    let mut env = Env::new().await;
    let admin = env.admin.insecure_clone();
    let mint = env.mint;
    env.init_registry(&admin, mint).await.unwrap();

    env.register(1_000 * USDC).await;
    env.deposit(500 * USDC).await.unwrap();
    assert_eq!(balance(&mut env.ctx, env.vault).await, Some(1_500 * USDC));

    // Withdrawals used to sign as the vault itself and always failed
    env.withdraw(200 * USDC).await.unwrap();
    assert_eq!(balance(&mut env.ctx, env.vault).await, Some(1_300 * USDC));
    assert_eq!(balance(&mut env.ctx, env.mm_tokens).await, Some(8_700 * USDC));

    // Deregistering returns everything and closes the vault
    let mm = env.mm.insecure_clone();
    let accounts = mm_registry::accounts::DeregisterMm {
        owner: mm.pubkey(), registry: env.registry, market_maker: env.market_maker, mm_collateral_account: env.vault,
        owner_token_account: env.mm_tokens, token_program: spl_token::ID,
    };
    send(&mut env.ctx, &[ix(mm_registry::ID, accounts, mm_registry::instruction::DeregisterMm {})], &[&mm]).await.unwrap();
    assert_eq!(balance(&mut env.ctx, env.mm_tokens).await, Some(10_000 * USDC));
    assert_eq!(balance(&mut env.ctx, env.vault).await, None);
}

#[tokio::test]
async fn quotes_can_be_posted_after_a_cancel() {
    let mut env = Env::new().await;
    let admin = env.admin.insecure_clone();
    let mint = env.mint;
    env.init_registry(&admin, mint).await.unwrap();
    env.register(1_000 * USDC).await;

    env.post_quote(0).await.unwrap();
    env.post_quote(1).await.unwrap();
    env.cancel_quote(0).await.unwrap();
    // Seeding by the live count reused index 1, which quote 1 still occupies
    env.post_quote(2).await.unwrap();
}

#[tokio::test]
async fn fills_are_disabled_and_move_no_funds() {
    let mut env = Env::new().await;
    let admin = env.admin.insecure_clone();
    let mint = env.mint;
    env.init_registry(&admin, mint).await.unwrap();
    env.register(1_000 * USDC).await;
    env.post_quote(0).await.unwrap();

    let taker = env.taker.insecure_clone();
    for is_buy in [false, true] {
        let accounts = mm_registry::accounts::FillQuote {
            taker: taker.pubkey(), registry: env.registry, market_maker: env.market_maker, quote: env.quote(0),
            mm_collateral_account: env.vault, taker_token_account: env.taker_tokens, token_program: spl_token::ID,
        };
        let params = FillQuoteParams { size: 5 * CONTRACT, is_buy };
        let result = send(&mut env.ctx, &[ix(mm_registry::ID, accounts, mm_registry::instruction::FillQuote { params })], &[&taker]).await;
        assert_eq!(custom_error(result), mm_error(MmRegistryError::FillsDisabled));
    }
    assert_eq!(balance(&mut env.ctx, env.vault).await, Some(1_000 * USDC));
    assert_eq!(balance(&mut env.ctx, env.taker_tokens).await, Some(10_000 * USDC));
}
