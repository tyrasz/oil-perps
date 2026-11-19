//! End-to-end tests for the MM registry, run in-process with perps-core loaded
//! alongside (registries belong to perps-core markets and are created by its
//! admin). Fills settle into perps-core positions through `venue_fill`.

#[allow(dead_code)]
#[path = "../../perps-core/tests/support/market_env.rs"]
mod support;

use anchor_lang::AnchorDeserialize;
use mm_registry::instructions::{FillQuoteParams, FillQuoteResult, InitializeRegistryParams, PostQuoteParams, UpdateQuoteParams};
use mm_registry::{MarketMaker, MmRegistry, MmRegistryError, MmStatus, TwoSidedQuote};
use perps_core::errors::PerpsError;
use perps_core::instructions::VenueApprovalParams;
use perps_core::state::{venue_margin, ExecutionSource, Market, Position, PositionStatus, Side, VenueApproval};
use solana_program_test::{processor, ProgramTest};
use support::*;

fn mm_process(program_id: &Pubkey, accounts: &[solana_sdk::account_info::AccountInfo], data: &[u8]) -> solana_sdk::entrypoint::ProgramResult {
    let accounts = Box::leak(Box::new(accounts.to_vec()));
    mm_registry::entry(program_id, accounts, data)
}

fn mm_error(e: MmRegistryError) -> u32 {
    e.into()
}

// The spec's quote: $74.97 / $75.03, 10 contracts a side, 10x, five minutes
const BID: u64 = 74_970_000;
const ASK: u64 = 75_030_000;
const LEV: u32 = 10_000;
const SIGNATURE_FEE: u64 = 5_000;

fn quote_params() -> PostQuoteParams {
    PostQuoteParams { bid_price: BID, bid_size: 10 * CONTRACT, ask_price: ASK, ask_size: 10 * CONTRACT, min_fill_size: CONTRACT, expires_in: 300, leverage: LEV }
}

fn no_update() -> UpdateQuoteParams {
    UpdateQuoteParams { bid_price: None, bid_size: None, ask_price: None, ask_size: None, leverage: None, expires_in: None }
}

/// A taker's side of a fill: price limit, leverage and size.
#[derive(Clone, Copy)]
struct Req {
    is_buy: bool,
    size: u64,
    limit: u64,
    leverage: u32,
    allow_partial: bool,
}

fn buy(contracts: u64) -> Req {
    Req { is_buy: true, size: contracts * CONTRACT, limit: ASK, leverage: LEV, allow_partial: false }
}

fn sell(contracts: u64) -> Req {
    Req { is_buy: false, size: contracts * CONTRACT, limit: BID, leverage: LEV, allow_partial: false }
}

/// A registered market maker: perps-core trader (wallet, UserAccount) plus
/// its MarketMaker PDA and bond vault.
struct Mm {
    t: Trader,
    market_maker: Pubkey,
    bond: Pubkey,
}

impl Mm {
    fn key(&self) -> Pubkey {
        self.t.key()
    }
}

async fn create_mint(base: &mut Base) -> Pubkey {
    let payer = base.ctx.payer.insecure_clone();
    let mint = Keypair::new();
    let rent = base.ctx.banks_client.get_rent().await.unwrap();
    send(&mut base.ctx, &[
        system_instruction::create_account(&payer.pubkey(), &mint.pubkey(), rent.minimum_balance(spl_token::state::Mint::LEN), spl_token::state::Mint::LEN as u64, &spl_token::ID),
        spl_token::instruction::initialize_mint(&spl_token::ID, &mint.pubkey(), &payer.pubkey(), None, 6).unwrap(),
    ], &[&mint]).await.unwrap();
    mint.pubkey()
}

/// Send with `payer` paying the fee, so its lamport change is exact.
async fn send_as(ctx: &mut ProgramTestContext, payer: &Keypair, ixs: &[Instruction], others: &[&Keypair]) -> Result<(), BanksClientError> {
    let mut all: Vec<&Keypair> = vec![payer];
    all.extend_from_slice(others);
    let blockhash = ctx.get_new_latest_blockhash().await.unwrap();
    ctx.banks_client.process_transaction(Transaction::new_signed_with_payer(ixs, Some(&payer.pubkey()), &all, blockhash)).await
}

async fn rent(base: &mut Base, len: usize) -> u64 {
    base.ctx.banks_client.get_rent().await.unwrap().minimum_balance(len)
}

/// perps-core config + OIL market with the mm-registry program loaded, no registry yet.
async fn bare() -> Base {
    start(ProgramTest::new("mm_registry", mm_registry::ID, processor!(mm_process))).await
}

fn registry_pda(base: &Base) -> Pubkey {
    pda(&[b"mm_registry", base.market.as_ref()], &mm_registry::ID)
}

async fn init_registry(base: &mut Base, signer: &Keypair, mint: Pubkey) -> Result<(), BanksClientError> {
    let accounts = mm_registry::accounts::InitializeRegistry {
        authority: signer.pubkey(), config: config_pda(), registry: registry_pda(base), market: base.market,
        collateral_mint: mint, system_program: system_program::ID, token_program: spl_token::ID,
    };
    let params = InitializeRegistryParams { min_collateral: 100 * USDC, max_spread: 100, min_quote_size: CONTRACT, max_quote_size: 1_000 * CONTRACT, mm_fee: 10 };
    send(&mut base.ctx, &[ix(mm_registry::ID, accounts, mm_registry::instruction::InitializeRegistry { params })], &[signer]).await
}

/// Collateral a fill of `req` at `price` can post, at the worst oracle the
/// venue band allows: the taker's one-shot allowance. venue_margin is convex
/// in the oracle, so the band's ends bound it.
fn worst_case_margin(m: &Market, req: Req, price: u64, band_bps: u16, now: i64) -> u64 {
    let (p, b) = (price as u128, band_bps as u128);
    let lo = ((p * 10_000 + 10_000 + b - 1) / (10_000 + b)) as u64;
    let hi = (p * 10_000 / (10_000 - b)) as u64;
    let side = if req.is_buy { Side::Long } else { Side::Short };
    [lo, price, hi].iter().map(|&o| venue_margin(m, side, req.size, price, o, req.leverage, now).unwrap_or(0)).max().unwrap()
}

struct Env {
    base: Base,
    registry: Pubkey,
    venue: Pubkey,
    mm: Mm,
    taker: Trader,
    nonce: u64,
}

impl Env {
    /// Registry, the mm-registry venue (min_signers 1), one MM ($1,000 bond,
    /// $1,000 in perps-core, maker approval with a 30 bps deviation bound) and
    /// a taker with $1,000 in perps-core. No quote yet.
    async fn new() -> Env {
        let mut base = bare().await;
        let admin = base.admin.insecure_clone();
        let mint = base.mint;
        init_registry(&mut base, &admin, mint).await.unwrap();
        let venue = register_venue(&mut base, &mm_registry::ID, mm_registry_venue_params()).await;
        let registry = registry_pda(&base);
        let mm = add_mm(&mut base, registry, venue).await;
        let taker = trader(&mut base, 1_000 * USDC).await;
        Env { base, registry, venue, mm, taker, nonce: 0x5eed_0000 }
    }

    fn next_nonce(&mut self) -> u64 {
        self.nonce += 1;
        self.nonce
    }

    async fn post_quote(&mut self, mm: &Keypair, params: PostQuoteParams) -> Result<Pubkey, BanksClientError> {
        let market_maker = pda(&[b"market_maker", self.registry.as_ref(), mm.pubkey().as_ref()], &mm_registry::ID);
        let index = load::<MarketMaker>(&mut self.base, market_maker).await.quotes_posted;
        let quote = pda(&[b"quote", market_maker.as_ref(), &index.to_le_bytes()], &mm_registry::ID);
        let accounts = mm_registry::accounts::PostQuote { owner: mm.pubkey(), registry: self.registry, market_maker, quote, system_program: system_program::ID };
        send(&mut self.base.ctx, &[ix(mm_registry::ID, accounts, mm_registry::instruction::PostQuote { params })], &[mm]).await?;
        Ok(quote)
    }

    async fn quote(&mut self) -> Pubkey {
        let mm = self.mm.t.kp.insecure_clone();
        self.post_quote(&mm, quote_params()).await.unwrap()
    }

    async fn update_quote(&mut self, mm: &Keypair, quote: Pubkey, params: UpdateQuoteParams) -> Result<(), BanksClientError> {
        let market_maker = pda(&[b"market_maker", self.registry.as_ref(), mm.pubkey().as_ref()], &mm_registry::ID);
        let accounts = mm_registry::accounts::UpdateQuote { owner: mm.pubkey(), registry: self.registry, market_maker, quote };
        send(&mut self.base.ctx, &[ix(mm_registry::ID, accounts, mm_registry::instruction::UpdateQuote { params })], &[mm]).await
    }

    async fn cancel_quote(&mut self, mm: &Keypair, quote: Pubkey) -> Result<(), BanksClientError> {
        let market_maker = pda(&[b"market_maker", self.registry.as_ref(), mm.pubkey().as_ref()], &mm_registry::ID);
        let accounts = mm_registry::accounts::CancelQuote { owner: mm.pubkey(), registry: self.registry, market_maker, quote };
        send(&mut self.base.ctx, &[ix(mm_registry::ID, accounts, mm_registry::instruction::CancelQuote {})], &[mm]).await
    }

    /// The taker's one-shot approval for `req`: approve_venue the first time,
    /// update_venue_approval after, as the SDK builds it.
    async fn taker_approval_ix(&mut self, taker: &Pubkey, quote: Pubkey, req: Req) -> Instruction {
        let now = now(&mut self.base).await;
        let m = market(&mut self.base).await;
        let band = load_venue(&mut self.base, &mm_registry::ID).await.max_price_deviation_bps;
        // A sell fills at the bid, which may be above the limit
        let price = match try_load::<TwoSidedQuote>(&mut self.base, quote).await {
            Some(q) if !req.is_buy => q.bid_price.max(req.limit),
            _ => req.limit,
        };
        let params = VenueApprovalParams {
            allowance: worst_case_margin(&m, req, price, band, now),
            max_leverage: req.leverage.clamp(1, m.max_leverage),
            max_buy_price: if req.is_buy { req.limit } else { 0 },
            min_sell_price: if req.is_buy { 0 } else { req.limit },
            max_price_deviation_bps: 0,
            expires_at: now + 120,
        };
        if exists(&mut self.base, approval_pda(taker, &self.venue, ROLE_TAKER)).await {
            update_approval_ix(&self.base, taker, &self.venue, ROLE_TAKER, params)
        } else {
            approve_ix(&self.base, taker, &self.venue, ROLE_TAKER, params)
        }
    }

    fn fill_ix(&self, taker: &Pubkey, mm: &Pubkey, quote: Pubkey, req: Req, nonce: u64, tweak: impl FnOnce(&mut mm_registry::accounts::FillQuote)) -> (Instruction, FillAccounts) {
        let fa = fill_accounts(&self.base, &mm_registry::ID, taker, mm, nonce);
        let market_maker = pda(&[b"market_maker", self.registry.as_ref(), mm.as_ref()], &mm_registry::ID);
        let mut accounts = mm_registry::accounts::FillQuote {
            taker: *taker, registry: self.registry, market_maker, quote, mm_owner: fa.maker,
            venue_authority: fa.venue_authority, perps_config: fa.config, perps_venue: fa.venue,
            venue_program_data: fa.venue_program_data, market: fa.market, pyth_price_feed: fa.pyth_price_feed,
            taker_user: fa.taker_user, taker_approval: fa.taker_approval, taker_position: fa.taker_position,
            mm_user: fa.maker_user, mm_approval: fa.maker_approval, mm_position: fa.maker_position,
            perps_core_program: perps_core::ID, system_program: system_program::ID,
        };
        tweak(&mut accounts);
        let params = FillQuoteParams {
            size: req.size, is_buy: req.is_buy, limit_price: req.limit, taker_leverage: req.leverage,
            allow_partial: req.allow_partial, position_nonce: nonce,
        };
        (ix(mm_registry::ID, accounts, mm_registry::instruction::FillQuote { params }), fa)
    }

    /// The SDK's taker transaction: compute budget, one-shot approval, fill_quote.
    async fn fill_tx(&mut self, taker: &Pubkey, mm: &Pubkey, quote: Pubkey, req: Req, tweak: impl FnOnce(&mut mm_registry::accounts::FillQuote)) -> (Vec<Instruction>, FillAccounts) {
        let approval = self.taker_approval_ix(taker, quote, req).await;
        let nonce = self.next_nonce();
        let (fill, fa) = self.fill_ix(taker, mm, quote, req, nonce, tweak);
        (vec![compute_budget_ix(300_000), approval, fill], fa)
    }

    /// The env's taker fills the env MM's `quote`.
    async fn fill(&mut self, quote: Pubkey, req: Req) -> Result<FillAccounts, BanksClientError> {
        let taker = self.taker.kp.insecure_clone();
        let mm = self.mm.key();
        let (ixs, fa) = self.fill_tx(&taker.pubkey(), &mm, quote, req, |_| {}).await;
        send_as(&mut self.base.ctx, &taker, &ixs, &[]).await?;
        Ok(fa)
    }

    /// Send `ixs` signed by `taker` and assert it fails with `code` without
    /// touching either party's perps-core state or the quote.
    async fn assert_rejected(&mut self, taker: &Keypair, ixs: &[Instruction], fa: &FillAccounts, quote: Pubkey, code: u32) {
        let before = fa.snapshot(&mut self.base).await;
        let quote_before = raw(&mut self.base, quote).await.map(|a| a.data);
        let result = send_as(&mut self.base.ctx, taker, ixs, &[]).await;
        assert_eq!(custom_error(result), code);
        assert_eq!(fa.snapshot(&mut self.base).await, before);
        assert_eq!(before.positions, [false, false]);
        assert_eq!(raw(&mut self.base, quote).await.map(|a| a.data), quote_before);
    }

    /// The env taker fills `req` against the env MM's `quote` and must fail with `code`.
    async fn reject(&mut self, quote: Pubkey, req: Req, code: u32) {
        let taker = self.taker.kp.insecure_clone();
        let mm = self.mm.key();
        let (ixs, fa) = self.fill_tx(&taker.pubkey(), &mm, quote, req, |_| {}).await;
        self.assert_rejected(&taker, &ixs, &fa, quote, code).await;
    }
}

/// mm-registry's deploy defaults (section 7). min_signers 1: the taker signs
/// as payer, so a venue bug cannot match two non-signing parties.
fn mm_registry_venue_params() -> perps_core::instructions::VenueParams {
    perps_core::instructions::VenueParams { min_signers: 1, ..default_venue_params() }
}

/// A market maker with a $1,000 bond, $1,000 in perps-core and a maker
/// approval for `venue` ($1,000 allowance, 30 bps adverse deviation).
async fn add_mm(base: &mut Base, registry: Pubkey, venue: Pubkey) -> Mm {
    let t = trader(base, 1_000 * USDC).await;
    mint_to(base, t.tokens, 1_000 * USDC).await;
    let market_maker = pda(&[b"market_maker", registry.as_ref(), t.key().as_ref()], &mm_registry::ID);
    let bond = pda(&[b"mm_collateral", market_maker.as_ref()], &mm_registry::ID);
    let accounts = mm_registry::accounts::RegisterMm {
        owner: t.key(), registry, market_maker, mm_collateral_account: bond, owner_token_account: t.tokens,
        collateral_mint: base.mint, system_program: system_program::ID, token_program: spl_token::ID, rent: sysvar::rent::ID,
    };
    send(&mut base.ctx, &[ix(mm_registry::ID, accounts, mm_registry::instruction::RegisterMm { initial_collateral: 1_000 * USDC })], &[&t.kp]).await.unwrap();
    let now = now(base).await;
    let params = VenueApprovalParams { max_price_deviation_bps: 30, ..approval_params(1_000 * USDC, now) };
    approve(base, &t.kp, &venue, ROLE_MAKER, params).await.unwrap();
    Mm { t, market_maker, bond }
}

async fn position(base: &mut Base, address: Pubkey) -> Position {
    load(base, address).await
}

// --- registry and bond -----------------------------------------------------------

#[tokio::test]
async fn only_the_perps_admin_creates_registries_with_the_market_mint() {
    let mut base = bare().await;
    let stranger = funded_keypair(&mut base).await;
    let admin = base.admin.insecure_clone();
    let mint = base.mint;
    assert_eq!(custom_error(init_registry(&mut base, &stranger, mint).await), mm_error(MmRegistryError::Unauthorized));
    let other = create_mint(&mut base).await;
    assert_eq!(custom_error(init_registry(&mut base, &admin, other).await), mm_error(MmRegistryError::InvalidCollateralMint));
    init_registry(&mut base, &admin, mint).await.unwrap();
}

#[tokio::test]
async fn market_makers_get_their_collateral_back() {
    let mut env = Env::new().await;
    let mm = env.mm.t.kp.insecure_clone();
    let (registry, market_maker, bond, tokens) = (env.registry, env.mm.market_maker, env.mm.bond, env.mm.t.tokens);
    mint_to(&mut env.base, tokens, 1_000 * USDC).await;
    let bond_ix = |deposit: bool, amount: u64| {
        if deposit {
            let accounts = mm_registry::accounts::DepositCollateral {
                owner: mm.pubkey(), registry, market_maker, mm_collateral_account: bond, owner_token_account: tokens, token_program: spl_token::ID,
            };
            ix(mm_registry::ID, accounts, mm_registry::instruction::DepositCollateral { amount })
        } else {
            let accounts = mm_registry::accounts::WithdrawCollateral {
                owner: mm.pubkey(), registry, market_maker, mm_collateral_account: bond, owner_token_account: tokens, token_program: spl_token::ID,
            };
            ix(mm_registry::ID, accounts, mm_registry::instruction::WithdrawCollateral { amount })
        }
    };
    send(&mut env.base.ctx, &[bond_ix(true, 500 * USDC)], &[&mm]).await.unwrap();
    assert_eq!(token_balance(&mut env.base, bond).await, 1_500 * USDC);

    // Withdrawals used to sign as the vault itself and always failed
    send(&mut env.base.ctx, &[bond_ix(false, 200 * USDC)], &[&mm]).await.unwrap();
    assert_eq!(token_balance(&mut env.base, bond).await, 1_300 * USDC);
    assert_eq!(token_balance(&mut env.base, tokens).await, 700 * USDC);

    // Deregistering returns everything and closes the vault
    deregister(&mut env, &mm).await.unwrap();
    assert_eq!(token_balance(&mut env.base, tokens).await, 2_000 * USDC);
    assert!(!exists(&mut env.base, bond).await);
}

async fn deregister(env: &mut Env, mm: &Keypair) -> Result<(), BanksClientError> {
    let accounts = mm_registry::accounts::DeregisterMm {
        owner: mm.pubkey(), registry: env.registry, market_maker: env.mm.market_maker, mm_collateral_account: env.mm.bond,
        owner_token_account: env.mm.t.tokens, token_program: spl_token::ID,
    };
    send(&mut env.base.ctx, &[ix(mm_registry::ID, accounts, mm_registry::instruction::DeregisterMm {})], &[mm]).await
}

// --- quotes ------------------------------------------------------------------------

#[tokio::test]
async fn quotes_can_be_posted_after_a_cancel() {
    let mut env = Env::new().await;
    let mm = env.mm.t.kp.insecure_clone();
    let q0 = env.quote().await;
    env.quote().await;
    env.cancel_quote(&mm, q0).await.unwrap();
    // Seeding by the live count reused index 1, which quote 1 still occupies
    env.quote().await;
}

#[tokio::test]
async fn quotes_need_a_leverage_and_a_bounded_expiry_and_lock_nothing() {
    let mut env = Env::new().await;
    let mm = env.mm.t.kp.insecure_clone();
    let invalid = mm_error(MmRegistryError::InvalidQuoteParams);
    for params in [
        PostQuoteParams { leverage: 0, ..quote_params() },
        PostQuoteParams { expires_in: 0, ..quote_params() },
        PostQuoteParams { expires_in: -1, ..quote_params() },
        PostQuoteParams { expires_in: mm_registry::MAX_QUOTE_TTL_SECS + 1, ..quote_params() },
    ] {
        assert_eq!(custom_error(env.post_quote(&mm, params).await.map(|_| ())), invalid);
    }
    let quote = env.post_quote(&mm, PostQuoteParams { expires_in: mm_registry::MAX_QUOTE_TTL_SECS, ..quote_params() }).await.unwrap();
    let q: TwoSidedQuote = load(&mut env.base, quote).await;
    let now = now(&mut env.base).await;
    assert_eq!((q.leverage, q.expires_at, q.collateral_locked), (LEV, now + mm_registry::MAX_QUOTE_TTL_SECS, 0));
    let m: MarketMaker = load(&mut env.base, env.mm.market_maker).await;
    assert_eq!((m.collateral_locked, m.collateral_available), (0, 1_000 * USDC));

    for params in [
        UpdateQuoteParams { leverage: Some(0), ..no_update() },
        UpdateQuoteParams { expires_in: Some(0), ..no_update() },
        UpdateQuoteParams { expires_in: Some(mm_registry::MAX_QUOTE_TTL_SECS + 1), ..no_update() },
    ] {
        assert_eq!(custom_error(env.update_quote(&mm, quote, params).await), invalid);
    }
    env.update_quote(&mm, quote, UpdateQuoteParams { leverage: Some(5_000), expires_in: Some(60), ..no_update() }).await.unwrap();
    let q: TwoSidedQuote = load(&mut env.base, quote).await;
    assert_eq!((q.leverage, q.expires_at), (5_000, now + 60));
}

#[tokio::test]
async fn update_quote_cannot_wrap_the_spread_check() {
    let mut env = Env::new().await;
    let mm = env.mm.t.kp.insecure_clone();
    let quote = env.quote().await;
    let prices = |bid: u64, ask: u64| UpdateQuoteParams { bid_price: Some(bid), ask_price: Some(ask), ..no_update() };
    // (ask - bid) * 10000 / bid == 2^32 bps, which used to truncate to 0 as u32
    let result = env.update_quote(&mm, quote, prices(1_000_000, 1_000_000 + (1u64 << 32) * 100)).await;
    assert_eq!(custom_error(result), mm_error(MmRegistryError::SpreadTooWide));
    // A sane update still goes through
    env.update_quote(&mm, quote, prices(74_960_000, 75_000_000)).await.unwrap();
}

// --- fills ---------------------------------------------------------------------------

/// 8.3 #1
#[tokio::test]
async fn fills_open_matched_perps_positions_and_move_no_tokens() {
    let mut env = Env::new().await;
    let quote = env.quote().await;
    let (taker, mm) = (env.taker.key(), env.mm.key());
    let vault_before = vault_token_balance(&mut env.base).await;
    let position_rent = rent(&mut env.base, Position::LEN).await;
    let approval_rent = rent(&mut env.base, VenueApproval::LEN).await;

    // Buy: taker long / MM short at the ask. Read the instruction's return data too.
    let lamports_before = lamports(&mut env.base, taker).await;
    let taker_kp = env.taker.kp.insecure_clone();
    let (ixs, fa) = env.fill_tx(&taker, &mm, quote, buy(10), |_| {}).await;
    let blockhash = env.base.ctx.get_new_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(&ixs, Some(&taker), &[&taker_kp], blockhash);
    let out = env.base.ctx.banks_client.process_transaction_with_metadata(tx).await.unwrap();
    out.result.unwrap();
    let mut data = out.metadata.unwrap().return_data.expect("fill_quote returns its result").data;
    // The runtime strips trailing zero bytes from return data; fee is the last field
    data.resize(24, 0);
    let result = FillQuoteResult::try_from_slice(&data).unwrap();
    assert_eq!((result.fill_price, result.fill_size, result.fee), (ASK, 10 * CONTRACT, 0));
    // First fill also creates the taker's approval
    assert_eq!(lamports_before - lamports(&mut env.base, taker).await, 2 * position_rent + approval_rent + SIGNATURE_FEE);

    let long = position(&mut env.base, fa.taker_position).await;
    let short = position(&mut env.base, fa.maker_position).await;
    assert_eq!((long.owner, long.side as u8, long.size, long.entry_price), (taker, Side::Long as u8, 10 * CONTRACT, ASK));
    assert_eq!((short.owner, short.side as u8, short.size, short.entry_price), (mm, Side::Short as u8, 10 * CONTRACT, ASK));
    assert_eq!((long.leverage, short.leverage), (LEV, LEV));
    assert!(long.execution_source == ExecutionSource::Venue && short.execution_source == ExecutionSource::Venue);
    // max(base, IM at the oracle) + the taker's 0.03/contract loss at the $75 oracle
    assert_eq!((long.collateral, short.collateral), (75_330_000, 75_030_000));
    assert_eq!(user(&mut env.base, &taker).await.collateral_balance, 1_000 * USDC - 75_330_000);
    assert_eq!(user(&mut env.base, &mm).await.collateral_balance, 1_000 * USDC - 75_030_000);
    let mm_approval: VenueApproval = load(&mut env.base, fa.maker_approval).await;
    assert_eq!((mm_approval.allowance, mm_approval.fills), (1_000 * USDC - 75_030_000, 1));

    let q: TwoSidedQuote = load(&mut env.base, quote).await;
    assert_eq!((q.ask_remaining, q.bid_remaining), (0, 10 * CONTRACT));
    let m: MarketMaker = load(&mut env.base, env.mm.market_maker).await;
    assert_eq!((m.total_volume, m.total_fills, m.inventory, m.total_fees_paid), (750_300_000, 1, 0, 0));
    let r: MmRegistry = load(&mut env.base, env.registry).await;
    assert_eq!((r.total_volume, r.total_fees), (750_300_000, 0));
    let mkt = market(&mut env.base).await;
    assert_eq!((mkt.long_open_interest, mkt.short_open_interest), (10 * CONTRACT, 10 * CONTRACT));

    // Sell: taker short / MM long at the bid
    let lamports_before = lamports(&mut env.base, taker).await;
    let fa = env.fill(quote, sell(10)).await.unwrap();
    assert_eq!(lamports_before - lamports(&mut env.base, taker).await, 2 * position_rent + SIGNATURE_FEE);
    let short = position(&mut env.base, fa.taker_position).await;
    let long = position(&mut env.base, fa.maker_position).await;
    assert_eq!((short.owner, short.side as u8, short.entry_price, short.collateral), (taker, Side::Short as u8, BID, 75_300_000));
    assert_eq!((long.owner, long.side as u8, long.entry_price, long.collateral), (mm, Side::Long as u8, BID, 75_000_000));

    // Margin stays in perps-core: no SPL balance moved
    assert_eq!(vault_token_balance(&mut env.base).await, vault_before);
    assert_eq!(token_balance(&mut env.base, env.mm.bond).await, 1_000 * USDC);
    assert_eq!(token_balance(&mut env.base, env.taker.tokens).await, 0);
    assert_eq!(token_balance(&mut env.base, env.mm.t.tokens).await, 0);
}

/// 8.3 #2
#[tokio::test]
async fn partial_fills_and_exhaustion() {
    let mut env = Env::new().await;
    let mm_kp = env.mm.t.kp.insecure_clone();
    let quote = env.post_quote(&mm_kp, PostQuoteParams { min_fill_size: 3 * CONTRACT, ..quote_params() }).await.unwrap();
    let quote_rent = lamports(&mut env.base, quote).await;

    let mut fills = Vec::new();
    for contracts in [4, 4] {
        fills.push((env.fill(quote, buy(contracts)).await.unwrap(), contracts));
    }
    // Below min_fill_size and not the whole remainder
    env.reject(quote, buy(1), mm_error(MmRegistryError::FillSizeTooSmall)).await;
    // The 2-contract tail is below min_fill_size but is all that is left
    fills.push((env.fill(quote, buy(2)).await.unwrap(), 2));
    env.reject(quote, buy(1), mm_error(MmRegistryError::FillSizeExceedsRemaining)).await;

    fills.push((env.fill(quote, sell(7)).await.unwrap(), 7));
    env.reject(quote, sell(5), mm_error(MmRegistryError::FillSizeExceedsRemaining)).await;
    let mm_before = lamports(&mut env.base, env.mm.market_maker).await;
    let fa = env.fill(quote, Req { allow_partial: true, ..sell(5) }).await.unwrap();
    fills.push((fa, 3));

    // Every fill made its own pair of positions
    for (fa, contracts) in fills {
        let t = position(&mut env.base, fa.taker_position).await;
        let m = position(&mut env.base, fa.maker_position).await;
        assert_eq!((t.size, m.size, t.status as u8, m.status as u8), (contracts * CONTRACT, contracts * CONTRACT, PositionStatus::Open as u8, PositionStatus::Open as u8));
        assert!(t.side != m.side);
    }
    // Exhausted: closed into the market_maker PDA, counters down
    assert!(!exists(&mut env.base, quote).await);
    assert_eq!(lamports(&mut env.base, env.mm.market_maker).await, mm_before + quote_rent);
    let m: MarketMaker = load(&mut env.base, env.mm.market_maker).await;
    assert_eq!((m.active_quotes, m.total_fills), (0, 5));
    let r: MmRegistry = load(&mut env.base, env.registry).await;
    assert_eq!(r.active_quotes, 0);
}

/// 8.3 #3: the taker's limit protects against an update_quote landing first.
#[tokio::test]
async fn limit_price_guards_against_a_repriced_quote() {
    let mut env = Env::new().await;
    let mm = env.mm.t.kp.insecure_clone();
    let quote = env.quote().await;
    env.update_quote(&mm, quote, UpdateQuoteParams { ask_price: Some(75_500_000), bid_price: Some(74_900_000), ..no_update() }).await.unwrap();
    env.reject(quote, buy(1), mm_error(MmRegistryError::PriceWorseThanLimit)).await;
    env.reject(quote, sell(1), mm_error(MmRegistryError::PriceWorseThanLimit)).await;
    env.reject(quote, Req { limit: 0, ..sell(1) }, mm_error(MmRegistryError::InvalidPrice)).await;
    // A limit that accepts the new bid fills at the bid
    let fa = env.fill(quote, Req { limit: 74_900_000, ..sell(1) }).await.unwrap();
    assert_eq!(position(&mut env.base, fa.taker_position).await.entry_price, 74_900_000);
}

/// 8.3 #4: the MM's approval bounds how far a stale quote can be picked off.
#[tokio::test]
async fn stale_quote_hits_the_mm_deviation_bound() {
    let mut env = Env::new().await;
    let quote = env.quote().await;
    // Inside the 50 bps venue band, but the MM shorts 36 bps under the oracle
    set_price(&mut env.base, 75.30).await;
    env.reject(quote, buy(10), perps_err(PerpsError::ApprovalPriceDeviation)).await;
    // The bid is 44 bps under: the MM buys cheap, which the bound allows
    let fa = env.fill(quote, sell(10)).await.unwrap();
    assert_eq!(position(&mut env.base, fa.maker_position).await.side as u8, Side::Long as u8);
}

/// 8.3 #5
#[tokio::test]
async fn fill_rejections() {
    let mut env = Env::new().await;
    let mm_kp = env.mm.t.kp.insecure_clone();
    let quote = env.quote().await;

    // Expired
    let t = now(&mut env.base).await;
    set_time(&mut env.base, t + 301).await;
    env.reject(quote, buy(1), mm_error(MmRegistryError::QuoteExpired)).await;
    set_time(&mut env.base, t).await;

    // Legacy quotes: no leverage, or no expiry
    patch::<TwoSidedQuote>(&mut env.base, quote, |q| q.leverage = 0).await;
    env.reject(quote, buy(1), mm_error(MmRegistryError::QuoteNotFillable)).await;
    patch::<TwoSidedQuote>(&mut env.base, quote, |q| { q.leverage = LEV; q.expires_at = 0 }).await;
    env.reject(quote, buy(1), mm_error(MmRegistryError::QuoteNotFillable)).await;
    patch::<TwoSidedQuote>(&mut env.base, quote, |q| q.expires_at = t + 300).await;

    // MM not active; trading disabled
    let market_maker = env.mm.market_maker;
    patch::<MarketMaker>(&mut env.base, market_maker, |m| m.status = MmStatus::Suspended).await;
    env.reject(quote, buy(1), mm_error(MmRegistryError::MarketMakerNotActive)).await;
    patch::<MarketMaker>(&mut env.base, market_maker, |m| m.status = MmStatus::Active).await;
    let registry = env.registry;
    patch::<MmRegistry>(&mut env.base, registry, |r| r.is_trading_enabled = false).await;
    env.reject(quote, buy(1), mm_error(MmRegistryError::TradingDisabled)).await;
    patch::<MmRegistry>(&mut env.base, registry, |r| r.is_trading_enabled = true).await;

    // A quote of the same MM recorded under another registry
    patch::<TwoSidedQuote>(&mut env.base, quote, |q| q.registry = Pubkey::new_unique()).await;
    env.reject(quote, buy(1), mm_error(MmRegistryError::Unauthorized)).await;
    patch::<TwoSidedQuote>(&mut env.base, quote, |q| q.registry = registry).await;

    // Another market account
    let taker = env.taker.kp.insecure_clone();
    let mm = env.mm.key();
    let other_market = env.base.oracle;
    let (ixs, fa) = env.fill_tx(&taker.pubkey(), &mm, quote, buy(1), |a| a.market = other_market).await;
    env.assert_rejected(&taker, &ixs, &fa, quote, mm_error(MmRegistryError::WrongMarket)).await;

    // The MM takes its own quote
    let (ixs, fa) = env.fill_tx(&mm, &mm, quote, buy(1), |_| {}).await;
    env.assert_rejected(&mm_kp, &ixs, &fa, quote, mm_error(MmRegistryError::SelfTrade)).await;

    // MM's perps-core balance too low
    withdraw(&mut env.base, &env.mm.t, 950 * USDC).await.unwrap();
    env.reject(quote, buy(10), perps_err(PerpsError::InsufficientCollateral)).await;
    env.fill(quote, buy(1)).await.unwrap();

    // MM not approved
    let venue = env.venue;
    revoke(&mut env.base, &mm_kp, &venue, ROLE_MAKER).await.unwrap();
    env.reject(quote, buy(1), anchor_err(anchor_lang::error::ErrorCode::AccountNotInitialized)).await;
    // ... and the quote itself is still cancellable
    env.cancel_quote(&mm_kp, quote).await.unwrap();
}

/// A legacy quote (no leverage or expiry) can be cancelled.
#[tokio::test]
async fn legacy_quotes_are_cancellable() {
    let mut env = Env::new().await;
    let mm = env.mm.t.kp.insecure_clone();
    let quote = env.quote().await;
    patch::<TwoSidedQuote>(&mut env.base, quote, |q| { q.leverage = 0; q.expires_at = 0 }).await;
    env.reject(quote, buy(1), mm_error(MmRegistryError::QuoteNotFillable)).await;
    env.cancel_quote(&mm, quote).await.unwrap();
    assert!(!exists(&mut env.base, quote).await);
}

/// 8.3 #6: the maker side is the quote's MM and no one else.
#[tokio::test]
async fn maker_accounts_cannot_be_substituted() {
    let mut env = Env::new().await;
    let quote = env.quote().await;
    // W is approved as a maker for this venue too, with money in perps-core
    let (registry, venue) = (env.registry, env.venue);
    let w = add_mm(&mut env.base, registry, venue).await;
    let taker = env.taker.kp.insecure_clone();
    let mm = env.mm.key();
    let (w_key, w_user) = (w.key(), w.t.user);
    let w_approval = approval_pda(&w_key, &venue, ROLE_MAKER);

    let (ixs, fa) = env.fill_tx(&taker.pubkey(), &mm, quote, buy(1), |a| a.mm_owner = w_key).await;
    env.assert_rejected(&taker, &ixs, &fa, quote, mm_error(MmRegistryError::Unauthorized)).await;
    let seeds = anchor_err(anchor_lang::error::ErrorCode::ConstraintSeeds);
    let (ixs, fa) = env.fill_tx(&taker.pubkey(), &mm, quote, buy(1), |a| a.mm_user = w_user).await;
    env.assert_rejected(&taker, &ixs, &fa, quote, seeds).await;
    let (ixs, fa) = env.fill_tx(&taker.pubkey(), &mm, quote, buy(1), |a| a.mm_approval = w_approval).await;
    env.assert_rejected(&taker, &ixs, &fa, quote, seeds).await;
    // Every W account at once still fails on the MM's wallet
    let (ixs, fa) = env.fill_tx(&taker.pubkey(), &mm, quote, buy(1), |a| {
        a.mm_owner = w_key;
        a.mm_user = w_user;
        a.mm_approval = w_approval;
    }).await;
    env.assert_rejected(&taker, &ixs, &fa, quote, mm_error(MmRegistryError::Unauthorized)).await;
    assert_eq!(user(&mut env.base, &w_key).await.collateral_balance, 1_000 * USDC);
}

/// 8.3 #7: MM positions are ordinary perps-core positions.
#[tokio::test]
async fn mm_positions_close_and_liquidate_like_any_other() {
    let mut env = Env::new().await;
    let quote = env.quote().await;
    let keeper = trader(&mut env.base, 0).await;

    // MM long 10 @ $74.97 with 75_000_000; closing at $75 pays collateral + $0.30 - 5 bps of $749.70
    let fa = env.fill(quote, sell(10)).await.unwrap();
    let mm_tokens = env.mm.t.tokens;
    close(&mut env.base, &env.mm.t, fa.maker_position).await.unwrap();
    assert_eq!(token_balance(&mut env.base, mm_tokens).await, 75_000_000 + 300_000 - 374_850);
    assert_eq!(position(&mut env.base, fa.maker_position).await.status as u8, PositionStatus::Closed as u8);

    // MM short 10 @ $75.03 at 10x; at $79 it is under maintenance margin
    let fa = env.fill(quote, buy(10)).await.unwrap();
    set_price(&mut env.base, 79.0).await;
    let mm = env.mm.key();
    liquidate(&mut env.base, &keeper, &mm, fa.maker_position).await.unwrap();
    assert_eq!(position(&mut env.base, fa.maker_position).await.status as u8, PositionStatus::Liquidated as u8);
    // liquidation_fee (1%) of the maintenance margin (5%) on $750.30
    assert_eq!(token_balance(&mut env.base, keeper.tokens).await, 375_150);
    // The taker's winning leg still closes, paid from the vault
    close(&mut env.base, &env.taker, fa.taker_position).await.unwrap();
    assert!(token_balance(&mut env.base, env.taker.tokens).await > 75_330_000);
}

/// 8.3 #8: the bond is not tied to open positions.
#[tokio::test]
async fn deregister_with_open_positions_refunds_the_bond() {
    let mut env = Env::new().await;
    let mm_kp = env.mm.t.kp.insecure_clone();
    let quote = env.quote().await;
    let fa = env.fill(quote, buy(5)).await.unwrap();
    env.cancel_quote(&mm_kp, quote).await.unwrap();
    deregister(&mut env, &mm_kp).await.unwrap();
    assert_eq!(token_balance(&mut env.base, env.mm.t.tokens).await, 1_000 * USDC);
    assert!(!exists(&mut env.base, env.mm.market_maker).await);

    close(&mut env.base, &env.mm.t, fa.maker_position).await.unwrap();
    assert_eq!(position(&mut env.base, fa.maker_position).await.status as u8, PositionStatus::Closed as u8);
}

/// 8.3 #9
#[tokio::test]
async fn update_quote_migrates_a_legacy_locked_quote() {
    let mut env = Env::new().await;
    let mm_kp = env.mm.t.kp.insecure_clone();
    let quote = env.quote().await;
    // As post_quote used to leave it: 10% of the larger side locked, no leverage, no expiry
    patch::<TwoSidedQuote>(&mut env.base, quote, |q| { q.leverage = 0; q.expires_at = 0; q.collateral_locked = 75_030_000 }).await;
    patch::<MarketMaker>(&mut env.base, env.mm.market_maker, |m| { m.collateral_locked = 75_030_000; m.collateral_available = 1_000 * USDC - 75_030_000 }).await;
    env.reject(quote, buy(1), mm_error(MmRegistryError::QuoteNotFillable)).await;

    env.update_quote(&mm_kp, quote, UpdateQuoteParams { leverage: Some(LEV), expires_in: Some(300), ..no_update() }).await.unwrap();
    let q: TwoSidedQuote = load(&mut env.base, quote).await;
    assert_eq!((q.collateral_locked, q.leverage), (0, LEV));
    let m: MarketMaker = load(&mut env.base, env.mm.market_maker).await;
    assert_eq!((m.collateral_locked, m.collateral_available), (0, 1_000 * USDC));
    env.fill(quote, buy(1)).await.unwrap();
}

/// A4: fills are seeded by caller nonces, so a fill in which W is the maker
/// does not invalidate W's own taker fill built before it landed.
#[tokio::test]
async fn fills_do_not_invalidate_prebuilt_fills() {
    let mut env = Env::new().await;
    let quote = env.quote().await;
    let (registry, venue) = (env.registry, env.venue);
    let w = add_mm(&mut env.base, registry, venue).await;
    let w_kp = w.t.kp.insecure_clone();
    let w_quote = env.post_quote(&w_kp, quote_params()).await.unwrap();

    // W builds and signs a taker fill against the env MM ...
    let mm = env.mm.key();
    let (ixs, fa) = env.fill_tx(&w.key(), &mm, quote, buy(2), |_| {}).await;
    let blockhash = env.base.ctx.get_new_latest_blockhash().await.unwrap();
    let prebuilt = Transaction::new_signed_with_payer(&ixs, Some(&w.key()), &[&w_kp], blockhash);
    // ... then is the maker in another fill, and in an open_position landing first
    let taker = env.taker.kp.insecure_clone();
    let (ixs2, _) = env.fill_tx(&taker.pubkey(), &w.key(), w_quote, buy(2), |_| {}).await;
    send_as(&mut env.base.ctx, &taker, &ixs2, &[]).await.unwrap();
    open(&mut env.base, &w.t, 0, 1, 5).await.unwrap();
    env.base.ctx.banks_client.process_transaction(prebuilt).await.unwrap();
    assert_eq!(position(&mut env.base, fa.taker_position).await.owner, w.key());
}

/// Section 5.3: a two-quote sweep fits in one legacy transaction.
#[tokio::test]
async fn two_quote_sweep_fits_a_legacy_transaction() {
    let mut env = Env::new().await;
    let q1 = env.quote().await;
    let (registry, venue) = (env.registry, env.venue);
    let w = add_mm(&mut env.base, registry, venue).await;
    let w_kp = w.t.kp.insecure_clone();
    let q2 = env.post_quote(&w_kp, quote_params()).await.unwrap();

    let taker = env.taker.kp.insecure_clone();
    // One approval covering both fills
    let approval = env.taker_approval_ix(&taker.pubkey(), q1, buy(20)).await;
    let (n1, n2) = (env.next_nonce(), env.next_nonce());
    let mm = env.mm.key();
    let (f1, fa1) = env.fill_ix(&taker.pubkey(), &mm, q1, buy(10), n1, |_| {});
    let (f2, fa2) = env.fill_ix(&taker.pubkey(), &w.key(), q2, buy(10), n2, |_| {});
    let blockhash = env.base.ctx.get_new_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(&[compute_budget_ix(400_000), approval, f1, f2], Some(&taker.pubkey()), &[&taker], blockhash);
    let size = bincode::serialize(&tx).unwrap().len();
    println!("two-quote sweep: {} bytes", size);
    assert!(size <= 1_232, "sweep is {} bytes", size);
    env.base.ctx.banks_client.process_transaction(tx).await.unwrap();
    for fa in [fa1, fa2] {
        assert_eq!(position(&mut env.base, fa.taker_position).await.size, 10 * CONTRACT);
    }
    assert_eq!(user(&mut env.base, &taker.pubkey()).await.total_positions, 2);
}

/// 8.3 #10: compute and (via scripts/build-sbf.sh) stack. Units are only
/// meaningful against the SBF binaries.
#[tokio::test]
async fn fill_quote_compute_units() {
    let mut env = Env::new().await;
    let quote = env.quote().await;
    let taker = env.taker.kp.insecure_clone();
    let mm = env.mm.key();
    let approval = env.taker_approval_ix(&taker.pubkey(), quote, buy(10)).await;
    send_as(&mut env.base.ctx, &taker, &[approval], &[]).await.unwrap();
    let nonce = env.next_nonce();
    let (fill, _) = env.fill_ix(&taker.pubkey(), &mm, quote, buy(10), nonce, |_| {});
    let (result, units) = simulate(&mut env.base.ctx, &[compute_budget_ix(300_000), fill], &[&taker]).await;
    result.unwrap();
    println!("fill_quote compute units: {}", units);
    if is_sbf() {
        assert!(units < 200_000, "fill_quote used {} CU", units);
    }
}
