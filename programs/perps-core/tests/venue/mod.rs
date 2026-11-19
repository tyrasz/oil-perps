//! venue_fill end-to-end, driven through a relay venue: a builtin that forwards
//! any venue_fill data and accounts and signs with its `[b"perps_venue",
//! market]` PDA. It models a fully compromised venue, so every bound tested
//! here is one perps-core enforces by itself.
//!
//! The relay is added with `add_builtin_program`, so it also runs when
//! `SBF_OUT_DIR` points perps-core at its SBF binary.

use crate::support::*;
use anchor_lang::error::ErrorCode as AnchorError;
use perps_core::errors::PerpsError;
use perps_core::instructions::{VenueApprovalParams, VenueFillParams, VenueParams};
use perps_core::state::{ExecutionSource, Market, Position, PositionStatus, Side, UserAccount, VenueApproval};
use solana_program_test::processor;
use solana_sdk::account_info::AccountInfo;
use solana_sdk::entrypoint::ProgramResult;

pub const RELAY: Pubkey = Pubkey::new_from_array([0x7e; 32]);
/// A second relay, registered as a separate venue (or not at all).
pub const RELAY2: Pubkey = Pubkey::new_from_array([0x7f; 32]);

/// accounts: `[perps_core program, ..venue_fill accounts in order]`;
/// data: venue_fill instruction data, forwarded verbatim. Signer flags are
/// forwarded as received, plus this program's venue authority.
fn relay(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let (perps, rest) = accounts.split_first().unwrap();
    // venue_fill passes the market 4th; other forwarded instructions may have fewer accounts
    let market = rest.get(3).map(|a| *a.key).unwrap_or_default();
    let (authority, bump) = venue_authority(&market, program_id);
    let metas = rest
        .iter()
        .map(|a| AccountMeta { pubkey: *a.key, is_signer: a.is_signer || *a.key == authority, is_writable: a.is_writable })
        .collect();
    let ix = Instruction { program_id: *perps.key, accounts: metas, data: data.to_vec() };
    solana_sdk::program::invoke_signed(&ix, rest, &[&[b"perps_venue", market.as_ref(), &[bump]]])
}

fn perr(e: PerpsError) -> u32 {
    perps_err(e)
}

fn aerr(e: AnchorError) -> u32 {
    anchor_err(e)
}

const DEPOSIT: u64 = 10_000 * USDC;
const ALLOWANCE: u64 = 100_000 * USDC;

/// 100 contracts at $75.03, taker long, both at 10x.
fn fill_params(position_nonce: u64) -> VenueFillParams {
    VenueFillParams { size: 100 * CONTRACT, price: 75_030_000, taker_side: 0, taker_leverage: 10_000, maker_leverage: 10_000, position_nonce }
}

/// The relay's instruction for `fa`, with taker/maker marked as signers if
/// their keys are in `signers`.
fn relay_ix(relay_program: &Pubkey, fa: &FillAccounts, payer: &Pubkey, params: VenueFillParams, signers: &[&Keypair]) -> Instruction {
    let mut metas = vec![AccountMeta::new_readonly(perps_core::ID, false)];
    for mut m in anchor_lang::ToAccountMetas::to_account_metas(&fa.perps_accounts(payer), None) {
        // The relay signs for its PDA itself; the tx can't.
        if m.pubkey == fa.venue_authority {
            m.is_signer = false;
        }
        if signers.iter().any(|k| k.pubkey() == m.pubkey) {
            m.is_signer = true;
        }
        metas.push(m);
    }
    let data = anchor_lang::InstructionData::data(&perps_core::instruction::VenueFill { params });
    Instruction { program_id: *relay_program, accounts: metas, data }
}

async fn relay_fill(base: &mut Base, relay_program: &Pubkey, fa: &FillAccounts, params: VenueFillParams, signers: &[&Keypair]) -> Result<(), BanksClientError> {
    let payer = base.ctx.payer.pubkey();
    let ix = relay_ix(relay_program, fa, &payer, params, signers);
    send(&mut base.ctx, &[ix], signers).await
}

struct Env {
    base: Base,
    venue: Pubkey,
    taker: Trader,
    maker: Trader,
    keeper: Trader,
    next_nonce: u64,
}

impl Env {
    /// Relay registered (band 50 bps, 15 s oracle, min_signers 0); taker and
    /// maker with $10,000 each and $100,000 approvals in their roles.
    async fn new() -> Env {
        let mut pt = ProgramTest::default();
        // add_program with prefer_bpf off is add_builtin_program; processor!
        // can't be unwrapped here without naming solana-program-runtime's
        // fn-pointer type. perps-core still loads from SBF_OUT_DIR when set.
        pt.prefer_bpf(false);
        pt.add_program("relay", RELAY, processor!(relay));
        pt.add_program("relay2", RELAY2, processor!(relay));
        pt.prefer_bpf(is_sbf());
        let mut base = start(pt).await;
        let venue = register_venue(&mut base, &RELAY, default_venue_params()).await;
        let taker = trader(&mut base, DEPOSIT).await;
        let maker = trader(&mut base, DEPOSIT).await;
        let keeper = trader(&mut base, 0).await;
        let t = now(&mut base).await;
        approve(&mut base, &taker.kp, &venue, ROLE_TAKER, approval_params(ALLOWANCE, t)).await.unwrap();
        approve(&mut base, &maker.kp, &venue, ROLE_MAKER, approval_params(ALLOWANCE, t)).await.unwrap();
        Env { base, venue, taker, maker, keeper, next_nonce: 0 }
    }

    fn nonce(&mut self) -> u64 {
        self.next_nonce += 1;
        self.next_nonce.wrapping_mul(0x9e37_79b9_7f4a_7c15)
    }

    fn accounts(&self, taker: &Pubkey, maker: &Pubkey, nonce: u64) -> FillAccounts {
        fill_accounts(&self.base, &RELAY, taker, maker, nonce)
    }

    /// taker vs maker through the relay with fresh accounts.
    fn std_accounts(&mut self) -> (FillAccounts, u64) {
        let n = self.nonce();
        (self.accounts(&self.taker.key(), &self.maker.key(), n), n)
    }

    async fn fill(&mut self, fa: &FillAccounts, params: VenueFillParams) -> Result<(), BanksClientError> {
        relay_fill(&mut self.base, &RELAY, fa, params, &[]).await
    }

    /// The standard 100 @ $75.03 fill; returns (taker position, maker position).
    async fn std_fill(&mut self) -> (Pubkey, Pubkey) {
        let (fa, n) = self.std_accounts();
        self.fill(&fa, fill_params(n)).await.unwrap();
        (fa.taker_position, fa.maker_position)
    }

    /// Assert the fill fails with `code` and changes nothing.
    async fn rejects(&mut self, fa: &FillAccounts, params: VenueFillParams, signers: &[&Keypair], code: u32) {
        let before = fa.snapshot(&mut self.base).await;
        let result = relay_fill(&mut self.base, &RELAY, fa, params, signers).await;
        assert_eq!(custom_error(result), code);
        assert_eq!(fa.snapshot(&mut self.base).await, before);
        assert_eq!(before.positions, [false, false]);
    }

    async fn approval(&mut self, address: Pubkey) -> VenueApproval {
        load(&mut self.base, address).await
    }

    async fn now(&mut self) -> i64 {
        now(&mut self.base).await
    }
}

// --- happy paths ---------------------------------------------------------------

#[tokio::test]
async fn venue_fill_opens_matched_positions() {
    let mut env = Env::new().await;
    let vault_before = vault_token_balance(&mut env.base).await;
    let market_before: Market = market(&mut env.base).await;
    let (fa, n) = env.std_accounts();
    assert_eq!(fa.taker_position, vposition(&env.taker.key(), &env.base.market, n));
    assert_eq!(fa.maker_position, vmaker(&fa.taker_position));
    env.fill(&fa, fill_params(n)).await.unwrap();

    let t: Position = load(&mut env.base, fa.taker_position).await;
    let m: Position = load(&mut env.base, fa.maker_position).await;
    // Taker: max($750.30 base, $750 IM at the oracle) + $3 adverse; MM: $750.30
    assert_eq!((t.collateral, m.collateral), (753_300_000, 750_300_000));
    assert!(t.side == Side::Long && m.side == Side::Short);
    assert_eq!((t.owner, m.owner), (env.taker.key(), env.maker.key()));
    for p in [&t, &m] {
        assert_eq!((p.size, p.entry_price, p.leverage), (100 * CONTRACT, 75_030_000, 10_000));
        assert!(p.execution_source == ExecutionSource::Venue && p.status == PositionStatus::Open);
        assert_eq!((p.market, p.opened_at, p.last_updated_at, p.realized_pnl), (env.base.market, WEDNESDAY, WEDNESDAY, 0));
        assert_eq!(p.last_funding_payment, market_before.cumulative_funding);
    }
    // Backstop: the taker is born at exactly 10% IM, the MM just above
    assert_eq!((t.margin_ratio(75_000_000, 0), m.margin_ratio(75_000_000, 0)), (1_000, 1_003));

    let tu: UserAccount = user(&mut env.base, &env.taker.key()).await;
    let mu: UserAccount = user(&mut env.base, &env.maker.key()).await;
    assert_eq!((tu.collateral_balance, mu.collateral_balance), (DEPOSIT - 753_300_000, DEPOSIT - 750_300_000));
    assert_eq!((tu.total_positions, tu.total_trades, mu.total_positions, mu.total_trades), (1, 1, 1, 1));
    let ta = env.approval(fa.taker_approval).await;
    let ma = env.approval(fa.maker_approval).await;
    assert_eq!((ta.allowance, ta.fills, ta.collateral_committed), (ALLOWANCE - 753_300_000, 1, 753_300_000));
    assert_eq!((ma.allowance, ma.fills, ma.collateral_committed), (ALLOWANCE - 750_300_000, 1, 750_300_000));
    assert_eq!((ta.role, ma.role), (ROLE_TAKER, ROLE_MAKER));

    let after: Market = market(&mut env.base).await;
    assert_eq!(after.long_open_interest, market_before.long_open_interest + 100 * CONTRACT);
    assert_eq!(after.short_open_interest, market_before.short_open_interest + 100 * CONTRACT);
    // open_position's PDA nonce is untouched; the trade counter moves
    assert_eq!(after.total_positions, market_before.total_positions);
    assert_eq!(after.total_trades, market_before.total_trades + 1);
    let venue = load_venue(&mut env.base, &RELAY).await;
    assert_eq!((venue.total_fills, venue.total_notional), (1, 7_503 * USDC));
    // No tokens move
    assert_eq!(vault_token_balance(&mut env.base).await, vault_before);
    assert_conservation(&mut env.base, 75_000_000, &[env.taker.key(), env.maker.key()], &[fa.taker_position, fa.maker_position]).await;
}

#[tokio::test]
async fn round_trip_is_zero_sum() {
    let mut env = Env::new().await;
    let (tp, mp) = env.std_fill().await;
    set_price(&mut env.base, 78.0).await;
    let owners = [env.taker.key(), env.maker.key()];
    assert_conservation(&mut env.base, 78_000_000, &owners, &[tp, mp]).await;

    close(&mut env.base, &env.taker, tp).await.unwrap();
    // One leg closed at the mark: the other is still marked against it
    assert_conservation(&mut env.base, 78_000_000, &owners, &[tp, mp]).await;
    close(&mut env.base, &env.maker, mp).await.unwrap();

    // PnL +-$297, fee 0.05% of $7,503 = $3.7515 each
    let taker_paid = token_balance(&mut env.base, env.taker.tokens).await;
    let maker_paid = token_balance(&mut env.base, env.maker.tokens).await;
    assert_eq!((taker_paid, maker_paid), (1_046_548_500, 449_548_500));
    assert_eq!(taker_paid + maker_paid, 753_300_000 + 750_300_000 - 7_503_000);
    assert_eq!(market(&mut env.base).await.insurance_fund, 7_503_000);
    assert_conservation(&mut env.base, 78_000_000, &owners, &[tp, mp]).await;
}

#[tokio::test]
async fn round_trip_with_funding_is_zero_sum() {
    let mut env = Env::new().await;
    let (tp, mp) = env.std_fill().await;
    // A one-sided open_position long makes funding non-zero (longs pay)
    let carol = trader(&mut env.base, 1_000 * USDC).await;
    open(&mut env.base, &carol, 0, 10, 5).await.unwrap();
    set_time(&mut env.base, WEDNESDAY + 3_600).await;
    update_funding(&mut env.base).await.unwrap();

    let cumulative = market(&mut env.base).await.cumulative_funding;
    assert!(cumulative > 0);
    let t: Position = load(&mut env.base, tp).await;
    let m: Position = load(&mut env.base, mp).await;
    let (funding_t, funding_m) = (t.accrued_funding(cumulative), m.accrued_funding(cumulative));
    assert!(funding_t < 0);
    assert_eq!(funding_t, -funding_m);

    set_price(&mut env.base, 78.0).await;
    close(&mut env.base, &env.taker, tp).await.unwrap();
    close(&mut env.base, &env.maker, mp).await.unwrap();
    let taker_paid = token_balance(&mut env.base, env.taker.tokens).await;
    let maker_paid = token_balance(&mut env.base, env.maker.tokens).await;
    assert_eq!(taker_paid as i64, 1_046_548_500 + funding_t);
    assert_eq!(maker_paid as i64, 449_548_500 + funding_m);
    // Funding moves between the legs only
    assert_eq!(taker_paid + maker_paid, 1_496_097_000);
}

#[tokio::test]
async fn venue_positions_liquidate() {
    let mut env = Env::new().await;
    let (tp, mp) = env.std_fill().await;
    // MM short from $75.03 at $79: equity $353.30 on $7,503 = 4.7% < 5%
    set_price(&mut env.base, 79.0).await;
    let maker_key = env.maker.key();
    let taker_key = env.taker.key();
    assert_eq!(custom_error(liquidate(&mut env.base, &env.keeper, &taker_key, tp).await), perr(PerpsError::NotLiquidatable));
    liquidate(&mut env.base, &env.keeper, &maker_key, mp).await.unwrap();
    // 1% of the 5% maintenance margin on $7,503
    assert_eq!(token_balance(&mut env.base, env.keeper.tokens).await, 3_751_500);
    let m: Position = load(&mut env.base, mp).await;
    assert!(m.status == PositionStatus::Liquidated);

    // The taker is paid in full: $753.30 + $397 - $3.7515 fee
    close(&mut env.base, &env.taker, tp).await.unwrap();
    assert_eq!(token_balance(&mut env.base, env.taker.tokens).await, 1_146_548_500);
}

#[tokio::test]
async fn not_born_liquidatable() {
    let mut env = Env::new().await;
    // Band edges: $75 +- 0.5%
    for (price, side) in [(75_375_000, 0u8), (74_625_000, 1u8), (75_375_000, 1u8), (74_625_000, 0u8)] {
        let (fa, n) = env.std_accounts();
        let params = VenueFillParams { price, taker_side: side, ..fill_params(n) };
        env.fill(&fa, params).await.unwrap();
        let taker_key = env.taker.key();
        let maker_key = env.maker.key();
        for (owner, pos) in [(taker_key, fa.taker_position), (maker_key, fa.maker_position)] {
            assert_eq!(custom_error(liquidate(&mut env.base, &env.keeper, &owner, pos).await), perr(PerpsError::NotLiquidatable));
            let p: Position = load(&mut env.base, pos).await;
            assert!(p.margin_ratio(75_000_000, 0) >= 1_000);
        }
        close(&mut env.base, &env.taker, fa.taker_position).await.unwrap();
        close(&mut env.base, &env.maker, fa.maker_position).await.unwrap();
    }
}

#[tokio::test]
async fn concurrent_fills_do_not_race() {
    let mut env = Env::new().await;
    let carol = trader(&mut env.base, 1_000 * USDC).await;

    // An open_position landing between deriving a fill and sending it
    let (fa, n) = env.std_accounts();
    open(&mut env.base, &carol, 0, 1, 5).await.unwrap();
    env.fill(&fa, fill_params(n)).await.unwrap();

    // A fill landing between deriving an open_position and sending it
    let (open_carol, carol_pos) = open_ix(&mut env.base, &carol.key(), 1, CONTRACT, 5_000).await;
    env.std_fill().await;
    send(&mut env.base.ctx, &[open_carol], &[&carol.kp]).await.unwrap();
    assert!(exists(&mut env.base, carol_pos).await);

    // W = the maker builds a taker fill, then is filled as maker first
    let t = env.now().await;
    approve(&mut env.base, &env.maker.kp, &env.venue, ROLE_TAKER, approval_params(10_000 * USDC, t)).await.unwrap();
    approve(&mut env.base, &carol.kp, &env.venue, ROLE_MAKER, approval_params(10_000 * USDC, t)).await.unwrap();
    let w_nonce = env.nonce();
    let w_as_taker = env.accounts(&env.maker.key(), &carol.key(), w_nonce);
    let small = |n| VenueFillParams { size: CONTRACT, ..fill_params(n) };
    let (fa, n) = env.std_accounts();
    env.fill(&fa, small(n)).await.unwrap();
    env.fill(&w_as_taker, small(w_nonce)).await.unwrap();

    // Two takers against the same maker
    let dave = trader(&mut env.base, 1_000 * USDC).await;
    approve(&mut env.base, &dave.kp, &env.venue, ROLE_TAKER, approval_params(10_000 * USDC, t)).await.unwrap();
    let (n1, n2) = (env.nonce(), env.nonce());
    let a = env.accounts(&env.taker.key(), &env.maker.key(), n1);
    let b = env.accounts(&dave.key(), &env.maker.key(), n2);
    env.fill(&a, small(n1)).await.unwrap();
    env.fill(&b, small(n2)).await.unwrap();

    // A position account derived for another nonce fails its seeds
    let (n3, n4) = (env.nonce(), env.nonce());
    let wrong = env.accounts(&env.taker.key(), &env.maker.key(), n3);
    env.rejects(&wrong, small(n4), &[], aerr(AnchorError::ConstraintSeeds)).await;
    // A reused nonce: init finds the account in use (system program AccountAlreadyInUse)
    let before: UserAccount = user(&mut env.base, &env.taker.key()).await;
    assert_eq!(custom_error(env.fill(&a, small(n1)).await), 0);
    assert_eq!(user(&mut env.base, &env.taker.key()).await.collateral_balance, before.collateral_balance);
}

// --- rejections -------------------------------------------------------------------

#[tokio::test]
async fn unregistered_or_impersonated_venues_cannot_fill() {
    let mut env = Env::new().await;
    // RELAY2 has no Venue record
    let n = env.nonce();
    let fa = fill_accounts(&env.base, &RELAY2, &env.taker.key(), &env.maker.key(), n);
    let before = fa.snapshot(&mut env.base).await;
    let result = relay_fill(&mut env.base, &RELAY2, &fa, fill_params(n), &[]).await;
    assert_eq!(custom_error(result), aerr(AnchorError::AccountNotInitialized));
    assert_eq!(fa.snapshot(&mut env.base).await, before);

    // RELAY2 passing RELAY's Venue: it can only sign for its own PDA
    let (mut fa, n) = env.std_accounts();
    fa.venue_authority = venue_authority(&env.base.market, &RELAY2).0;
    let before = fa.snapshot(&mut env.base).await;
    let result = relay_fill(&mut env.base, &RELAY2, &fa, fill_params(n), &[]).await;
    assert_eq!(custom_error(result), perr(PerpsError::VenueUnauthorized));
    assert_eq!(fa.snapshot(&mut env.base).await, before);

    // A keypair calling perps-core directly as the venue authority
    let impostor = funded_keypair(&mut env.base).await;
    let (mut fa, n) = env.std_accounts();
    fa.venue_authority = impostor.pubkey();
    let payer = env.base.ctx.payer.pubkey();
    let direct = ix(perps_core::ID, fa.perps_accounts(&payer), perps_core::instruction::VenueFill { params: fill_params(n) });
    assert_eq!(custom_error(send(&mut env.base.ctx, &[direct], &[&impostor]).await), perr(PerpsError::VenueUnauthorized));
    // Nor can the admin
    let admin = env.base.admin.insecure_clone();
    fa.venue_authority = admin.pubkey();
    let direct = ix(perps_core::ID, fa.perps_accounts(&payer), perps_core::instruction::VenueFill { params: fill_params(n) });
    assert_eq!(custom_error(send(&mut env.base.ctx, &[direct], &[&admin]).await), perr(PerpsError::VenueUnauthorized));
}

#[tokio::test]
async fn guardian_stops_fills_but_cannot_restart_them() {
    let mut env = Env::new().await;
    let guardian = env.base.guardian.insecure_clone();
    let admin = env.base.admin.insecure_clone();
    let stranger = funded_keypair(&mut env.base).await;

    assert_eq!(custom_error(disable_venue(&mut env.base, &stranger, &RELAY).await), perr(PerpsError::Unauthorized));
    disable_venue(&mut env.base, &guardian, &RELAY).await.unwrap();
    let (fa, n) = env.std_accounts();
    env.rejects(&fa, fill_params(n), &[], perr(PerpsError::VenueDisabled)).await;
    let slot = program_upgrade_authority(&mut env.base, &RELAY).await;
    assert_eq!(custom_error(update_venue(&mut env.base, &guardian, &RELAY, default_venue_params(), true, slot).await), perr(PerpsError::Unauthorized));
    update_venue(&mut env.base, &admin, &RELAY, default_venue_params(), true, slot).await.unwrap();
    env.fill(&fa, fill_params(n)).await.unwrap();

    assert_eq!(custom_error(set_venue_fills_paused(&mut env.base, &stranger, true).await), perr(PerpsError::Unauthorized));
    set_venue_fills_paused(&mut env.base, &guardian, true).await.unwrap();
    let (fa, n) = env.std_accounts();
    env.rejects(&fa, fill_params(n), &[], perr(PerpsError::VenueFillsPaused)).await;
    assert_eq!(custom_error(set_venue_fills_paused(&mut env.base, &guardian, false).await), perr(PerpsError::Unauthorized));
    assert_eq!(custom_error(set_venue_fills_paused(&mut env.base, &stranger, false).await), perr(PerpsError::Unauthorized));
    // Pausing doesn't stop ordinary trading, closes or approvals
    open(&mut env.base, &env.taker, 0, 1, 5).await.unwrap();
    set_venue_fills_paused(&mut env.base, &admin, false).await.unwrap();
    env.fill(&fa, fill_params(n)).await.unwrap();
}

#[tokio::test]
async fn venue_fills_follow_the_pinned_upgrade_authority() {
    let mut env = Env::new().await;
    let admin = env.base.admin.insecure_clone();
    let (fa, n) = env.std_accounts();
    // A new deploy slot alone doesn't halt fills: loader ExtendProgram needs no
    // authority and rewrites the slot, so pinning it would let anyone halt a venue
    bump_program_slot(&mut env.base, &RELAY).await;
    env.fill(&fa, fill_params(n)).await.unwrap();

    // Another program's ProgramData, with the same authority, fails the address pin
    add_venue_program_data(&mut env.base, &RELAY2, 1).await;
    let (fa, n) = env.std_accounts();
    let mut swapped = fa;
    swapped.venue_program_data = program_data_pda(&RELAY2);
    env.rejects(&swapped, fill_params(n), &[], perr(PerpsError::VenueProgramChanged)).await;

    // A change of upgrade authority halts fills until the admin re-attests
    let rogue = Pubkey::new_unique();
    set_upgrade_authority(&mut env.base, &RELAY, Some(rogue)).await;
    env.rejects(&fa, fill_params(n), &[], perr(PerpsError::VenueProgramChanged)).await;
    // Making the venue immutable is a change too
    set_upgrade_authority(&mut env.base, &RELAY, None).await;
    env.rejects(&fa, fill_params(n), &[], perr(PerpsError::VenueProgramChanged)).await;

    // The admin must name the authority it re-attests
    assert_eq!(custom_error(update_venue(&mut env.base, &admin, &RELAY, default_venue_params(), true, admin.pubkey()).await), perr(PerpsError::InvalidVenueParams));
    update_venue(&mut env.base, &admin, &RELAY, default_venue_params(), true, Pubkey::default()).await.unwrap();
    assert_eq!(load_venue(&mut env.base, &RELAY).await.upgrade_authority, Pubkey::default());
    env.fill(&fa, fill_params(n)).await.unwrap();
}

#[tokio::test]
async fn fill_price_must_be_inside_the_band() {
    let mut env = Env::new().await;
    for price in [75_375_001, 74_624_999] {
        let (fa, n) = env.std_accounts();
        for side in [0u8, 1u8] {
            env.rejects(&fa, VenueFillParams { price, taker_side: side, ..fill_params(n) }, &[], perr(PerpsError::FillPriceOutOfBand)).await;
        }
    }
}

#[tokio::test]
async fn venue_oracle_age_is_tighter_than_the_default() {
    let mut env = Env::new().await;
    let t = env.now().await;
    // 20 s old: too old for the venue's 15 s, fine for open_position's 60 s
    set_price_raw(&mut env.base, pyth_dollars(75.0), 0, t - 20).await;
    let (fa, n) = env.std_accounts();
    env.rejects(&fa, fill_params(n), &[], perr(PerpsError::StaleOraclePrice)).await;
    open(&mut env.base, &env.taker, 0, 1, 5).await.unwrap();
    set_price_raw(&mut env.base, pyth_dollars(75.0), 0, t - 15).await;
    env.fill(&fa, fill_params(n)).await.unwrap();
}

#[tokio::test]
async fn approvals_are_required_and_scoped_to_venue_and_role() {
    let mut env = Env::new().await;
    let erin = trader(&mut env.base, 1_000 * USDC).await;
    let t = env.now().await;

    // No approval at all
    let n = env.nonce();
    let fa = env.accounts(&erin.key(), &env.maker.key(), n);
    env.rejects(&fa, fill_params(n), &[], aerr(AnchorError::AccountNotInitialized)).await;

    // Approved for another venue only
    let venue2 = register_venue(&mut env.base, &RELAY2, default_venue_params()).await;
    let other = approve(&mut env.base, &erin.kp, &venue2, ROLE_TAKER, approval_params(10_000 * USDC, t)).await.unwrap();
    let mut fa2 = fa;
    fa2.taker_approval = other;
    env.rejects(&fa2, fill_params(n), &[], aerr(AnchorError::ConstraintSeeds)).await;

    // Approved here, but as maker: a maker approval never lets a venue make her the taker
    let as_maker = approve(&mut env.base, &erin.kp, &env.venue, ROLE_MAKER, approval_params(10_000 * USDC, t)).await.unwrap();
    let mut fa3 = fa;
    fa3.taker_approval = as_maker;
    env.rejects(&fa3, fill_params(n), &[], aerr(AnchorError::ConstraintSeeds)).await;

    // Expired
    let mut short = approval_params(10_000 * USDC, t);
    short.expires_at = t + 100;
    approve(&mut env.base, &erin.kp, &env.venue, ROLE_TAKER, short).await.unwrap();
    set_time(&mut env.base, t + 100).await;
    env.fill(&fa, VenueFillParams { size: CONTRACT, ..fill_params(n) }).await.unwrap();
    set_time(&mut env.base, t + 101).await;
    let n = env.nonce();
    let fa = env.accounts(&erin.key(), &env.maker.key(), n);
    env.rejects(&fa, fill_params(n), &[], perr(PerpsError::ApprovalExpired)).await;
}

#[tokio::test]
async fn allowance_is_spent_down() {
    let mut env = Env::new().await;
    let t = env.now().await;
    let (taker, venue) = (env.taker.kp.insecure_clone(), env.venue);
    update_approval(&mut env.base, &taker, &venue, ROLE_TAKER, approval_params(753_300_000 - 1, t)).await.unwrap();
    let (fa, n) = env.std_accounts();
    env.rejects(&fa, fill_params(n), &[], perr(PerpsError::ApprovalAllowanceExceeded)).await;
    update_approval(&mut env.base, &taker, &venue, ROLE_TAKER, approval_params(753_300_000, t)).await.unwrap();
    env.fill(&fa, fill_params(n)).await.unwrap();
    let a = env.approval(fa.taker_approval).await;
    assert_eq!((a.allowance, a.fills), (0, 1));
    // Nothing left: even a one-contract fill fails. Updating keeps the stats.
    let (fa, n) = env.std_accounts();
    env.rejects(&fa, VenueFillParams { size: CONTRACT, ..fill_params(n) }, &[], perr(PerpsError::ApprovalAllowanceExceeded)).await;
    update_approval(&mut env.base, &taker, &venue, ROLE_TAKER, approval_params(USDC, t)).await.unwrap();
    let a = env.approval(fa.taker_approval).await;
    assert_eq!((a.allowance, a.fills, a.collateral_committed), (USDC, 1, 753_300_000));
}

#[tokio::test]
async fn leverage_is_bounded_by_approval_and_initial_margin() {
    let mut env = Env::new().await;
    let t = env.now().await;
    let (taker, venue) = (env.taker.kp.insecure_clone(), env.venue);
    let mut p = approval_params(10_000 * USDC, t);
    p.max_leverage = 5_000;
    update_approval(&mut env.base, &taker, &venue, ROLE_TAKER, p).await.unwrap();
    let (fa, n) = env.std_accounts();
    env.rejects(&fa, fill_params(n), &[], perr(PerpsError::ApprovalLeverageExceeded)).await;
    env.rejects(&fa, VenueFillParams { maker_leverage: 15_000, taker_leverage: 5_000, ..fill_params(n) }, &[], perr(PerpsError::ExcessiveLeverage)).await;
    for lev in [0, 15_000, 20_001] {
        env.rejects(&fa, VenueFillParams { taker_leverage: 5_000, maker_leverage: lev, ..fill_params(n) }, &[], perr(PerpsError::ExcessiveLeverage)).await;
    }
    env.fill(&fa, VenueFillParams { taker_leverage: 5_000, ..fill_params(n) }).await.unwrap();
    let tp: Position = load(&mut env.base, fa.taker_position).await;
    // $1,500.60 at 5x + $3 adverse
    assert_eq!((tp.leverage, tp.collateral), (5_000, 1_503_600_000));
}

#[tokio::test]
async fn limit_prices_bind_each_side() {
    let mut env = Env::new().await;
    let t = env.now().await;
    let (taker, maker, venue) = (env.taker.kp.insecure_clone(), env.maker.kp.insecure_clone(), env.venue);
    let mut p = approval_params(10_000 * USDC, t);
    p.max_buy_price = 75_029_999;
    update_approval(&mut env.base, &taker, &venue, ROLE_TAKER, p).await.unwrap();
    let (fa, n) = env.std_accounts();
    env.rejects(&fa, fill_params(n), &[], perr(PerpsError::ApprovalLimitPrice)).await;
    // max_buy_price says nothing about selling; allowed_sides does
    env.fill(&fa, VenueFillParams { taker_side: 1, ..fill_params(n) }).await.unwrap();
    let buy_only = VenueApprovalParams { allowed_sides: SIDE_LONG, ..p };
    update_approval(&mut env.base, &taker, &venue, ROLE_TAKER, buy_only).await.unwrap();
    let (fa, n) = env.std_accounts();
    env.rejects(&fa, VenueFillParams { taker_side: 1, price: 75_000_000, ..fill_params(n) }, &[], perr(PerpsError::ApprovalSideNotAllowed)).await;
    let sell_only = VenueApprovalParams { allowed_sides: SIDE_SHORT, max_buy_price: 0, ..p };
    update_approval(&mut env.base, &taker, &venue, ROLE_TAKER, sell_only).await.unwrap();
    env.rejects(&fa, VenueFillParams { taker_side: 0, price: 75_000_000, ..fill_params(n) }, &[], perr(PerpsError::ApprovalSideNotAllowed)).await;
    env.fill(&fa, VenueFillParams { taker_side: 1, price: 75_000_000, ..fill_params(n) }).await.unwrap();
    update_approval(&mut env.base, &taker, &venue, ROLE_TAKER, p).await.unwrap();

    p.max_buy_price = 75_030_000;
    update_approval(&mut env.base, &taker, &venue, ROLE_TAKER, p).await.unwrap();
    let mut q = approval_params(10_000 * USDC, t);
    q.min_sell_price = 75_030_001;
    update_approval(&mut env.base, &maker, &venue, ROLE_MAKER, q).await.unwrap();
    let (fa, n) = env.std_accounts();
    env.rejects(&fa, fill_params(n), &[], perr(PerpsError::ApprovalLimitPrice)).await;
    q.min_sell_price = 75_030_000;
    update_approval(&mut env.base, &maker, &venue, ROLE_MAKER, q).await.unwrap();
    env.fill(&fa, fill_params(n)).await.unwrap();
}

#[tokio::test]
async fn stale_quote_is_bounded_by_the_makers_deviation_limit() {
    let mut env = Env::new().await;
    let t = env.now().await;
    let (maker, venue) = (env.maker.kp.insecure_clone(), env.venue);
    let mut q = approval_params(10_000 * USDC, t);
    q.max_price_deviation_bps = 30;
    update_approval(&mut env.base, &maker, &venue, ROLE_MAKER, q).await.unwrap();
    // The oracle ran to $75.30; selling at the old $75.03 ask is 36 bps under it
    set_price(&mut env.base, 75.30).await;
    let (fa, n) = env.std_accounts();
    env.rejects(&fa, fill_params(n), &[], perr(PerpsError::ApprovalPriceDeviation)).await;
    // The same deviation in the MM's favour is fine
    env.fill(&fa, VenueFillParams { taker_side: 1, ..fill_params(n) }).await.unwrap();
}

#[tokio::test]
async fn self_trade_is_rejected() {
    let mut env = Env::new().await;
    let t = env.now().await;
    let (taker, venue) = (env.taker.kp.insecure_clone(), env.venue);
    approve(&mut env.base, &taker, &venue, ROLE_MAKER, approval_params(10_000 * USDC, t)).await.unwrap();
    let n = env.nonce();
    let fa = env.accounts(&env.taker.key(), &env.taker.key(), n);
    env.rejects(&fa, fill_params(n), &[], perr(PerpsError::SelfTrade)).await;
}

#[tokio::test]
async fn min_signers_counts_parties_the_venue_marks_as_signers() {
    let mut env = Env::new().await;
    let admin = env.base.admin.insecure_clone();
    let slot = program_upgrade_authority(&mut env.base, &RELAY).await;
    let one = VenueParams { min_signers: 1, ..default_venue_params() };
    update_venue(&mut env.base, &admin, &RELAY, one, true, slot).await.unwrap();
    let (fa, n) = env.std_accounts();
    env.rejects(&fa, fill_params(n), &[], perr(PerpsError::MissingSigner)).await;
    let taker = env.taker.kp.insecure_clone();
    let maker = env.maker.kp.insecure_clone();
    relay_fill(&mut env.base, &RELAY, &fa, fill_params(n), &[&taker]).await.unwrap();

    let two = VenueParams { min_signers: 2, ..default_venue_params() };
    update_venue(&mut env.base, &admin, &RELAY, two, true, slot).await.unwrap();
    let (fa, n) = env.std_accounts();
    env.rejects(&fa, fill_params(n), &[&maker], perr(PerpsError::MissingSigner)).await;
    relay_fill(&mut env.base, &RELAY, &fa, fill_params(n), &[&taker, &maker]).await.unwrap();
}

#[tokio::test]
async fn market_state_gates_fills() {
    let mut env = Env::new().await;
    let guardian = env.base.guardian.insecure_clone();
    // Saturday on the CME schedule
    configure(&mut env.base, cme_config()).await.unwrap();
    set_time(&mut env.base, SATURDAY).await;
    let (fa, n) = env.std_accounts();
    env.rejects(&fa, fill_params(n), &[], perr(PerpsError::MarketClosed)).await;
    set_time(&mut env.base, WEDNESDAY).await;
    env.fill(&fa, fill_params(n)).await.unwrap();

    pause(&mut env.base, &guardian).await.unwrap();
    let (fa, n) = env.std_accounts();
    env.rejects(&fa, fill_params(n), &[], perr(PerpsError::MarketPaused)).await;
    // A settled market is also paused
    settle(&mut env.base, 75 * USDC).await.unwrap();
    env.rejects(&fa, fill_params(n), &[], perr(PerpsError::MarketPaused)).await;
}

#[tokio::test]
async fn open_interest_cap_applies_to_both_sides() {
    let mut env = Env::new().await;
    let market_key = env.base.market;
    for long_side in [true, false] {
        patch::<Market>(&mut env.base, market_key, |m| {
            let near_cap = m.max_open_interest - 50 * CONTRACT;
            if long_side { m.long_open_interest = near_cap } else { m.short_open_interest = near_cap }
        }).await;
        let (fa, n) = env.std_accounts();
        for side in [0u8, 1u8] {
            env.rejects(&fa, VenueFillParams { taker_side: side, ..fill_params(n) }, &[], perr(PerpsError::OpenInterestCapExceeded)).await;
        }
        env.fill(&fa, VenueFillParams { size: 50 * CONTRACT, ..fill_params(n) }).await.unwrap();
        patch::<Market>(&mut env.base, market_key, |m| { m.long_open_interest = 0; m.short_open_interest = 0 }).await;
    }
}

#[tokio::test]
async fn each_side_needs_its_own_collateral() {
    let mut env = Env::new().await;
    let poor = trader(&mut env.base, 750_300_000 - 1).await;
    let t = env.now().await;
    approve(&mut env.base, &poor.kp, &env.venue, ROLE_MAKER, approval_params(10_000 * USDC, t)).await.unwrap();
    approve(&mut env.base, &poor.kp, &env.venue, ROLE_TAKER, approval_params(10_000 * USDC, t)).await.unwrap();
    let n = env.nonce();
    let fa = env.accounts(&env.taker.key(), &poor.key(), n);
    env.rejects(&fa, fill_params(n), &[], perr(PerpsError::InsufficientCollateral)).await;
    let fa = env.accounts(&poor.key(), &env.maker.key(), n);
    env.rejects(&fa, fill_params(n), &[], perr(PerpsError::InsufficientCollateral)).await;
    mint_to(&mut env.base, poor.tokens, 1).await;
    deposit(&mut env.base, &poor, 1).await.unwrap();
    let fa = env.accounts(&env.taker.key(), &poor.key(), n);
    env.fill(&fa, fill_params(n)).await.unwrap();
    assert_eq!(user(&mut env.base, &poor.key()).await.collateral_balance, 0);
}

#[tokio::test]
async fn fill_size_is_capped_per_fill() {
    let mut env = Env::new().await;
    let admin = env.base.admin.insecure_clone();
    let slot = program_upgrade_authority(&mut env.base, &RELAY).await;
    let capped = VenueParams { max_fill_notional: 7_503 * USDC - 1, ..default_venue_params() };
    update_venue(&mut env.base, &admin, &RELAY, capped, true, slot).await.unwrap();
    let (fa, n) = env.std_accounts();
    env.rejects(&fa, fill_params(n), &[], perr(PerpsError::FillTooLarge)).await;
    let exact = VenueParams { max_fill_notional: 7_503 * USDC, ..default_venue_params() };
    update_venue(&mut env.base, &admin, &RELAY, exact, true, slot).await.unwrap();
    env.fill(&fa, fill_params(n)).await.unwrap();
}

#[tokio::test]
async fn degenerate_fill_params_are_rejected() {
    let mut env = Env::new().await;
    let (fa, n) = env.std_accounts();
    env.rejects(&fa, VenueFillParams { size: 0, ..fill_params(n) }, &[], perr(PerpsError::InvalidFillParams)).await;
    env.rejects(&fa, VenueFillParams { price: 0, ..fill_params(n) }, &[], perr(PerpsError::InvalidFillParams)).await;
    env.rejects(&fa, VenueFillParams { taker_side: 2, ..fill_params(n) }, &[], perr(PerpsError::InvalidPositionSide)).await;
    // Notional rounds to zero: one base unit at a $0.0001 oracle
    set_price(&mut env.base, 0.0001).await;
    env.rejects(&fa, VenueFillParams { size: 1, price: 100, ..fill_params(n) }, &[], perr(PerpsError::InvalidFillParams)).await;
}

#[tokio::test]
async fn revoked_approval_refunds_rent_and_stops_fills() {
    let mut env = Env::new().await;
    let (maker, venue) = (env.maker.kp.insecure_clone(), env.venue);
    let approval = approval_pda(&maker.pubkey(), &venue, ROLE_MAKER);
    let rent = lamports(&mut env.base, approval).await;
    let before = lamports(&mut env.base, maker.pubkey()).await;
    // Only the owner can revoke
    let stranger = funded_keypair(&mut env.base).await;
    let forged = Instruction {
        program_id: perps_core::ID,
        accounts: vec![AccountMeta::new(stranger.pubkey(), true), AccountMeta::new(approval, false)],
        data: anchor_lang::InstructionData::data(&perps_core::instruction::RevokeVenueApproval {}),
    };
    assert_eq!(custom_error(send(&mut env.base.ctx, &[forged], &[&stranger]).await), aerr(AnchorError::ConstraintSeeds));

    revoke(&mut env.base, &maker, &venue, ROLE_MAKER).await.unwrap();
    assert!(!exists(&mut env.base, approval).await);
    // The test payer pays the fee, so the owner gets exactly the rent back
    assert_eq!(lamports(&mut env.base, maker.pubkey()).await, before + rent);
    let (fa, n) = env.std_accounts();
    env.rejects(&fa, fill_params(n), &[], aerr(AnchorError::AccountNotInitialized)).await;
}

// --- admin ------------------------------------------------------------------------

#[tokio::test]
async fn only_the_admin_registers_and_updates_venues() {
    let mut env = Env::new().await;
    let guardian = env.base.guardian.insecure_clone();
    let admin = env.base.admin.insecure_clone();
    add_venue_program_data(&mut env.base, &RELAY2, 7).await;
    let ua = admin.pubkey();
    let p = default_venue_params();
    assert_eq!(custom_error(try_register_venue(&mut env.base, &guardian, &RELAY2, p, ua).await), perr(PerpsError::Unauthorized));
    // The admin must name the venue's upgrade authority
    assert_eq!(custom_error(try_register_venue(&mut env.base, &admin, &RELAY2, p, Pubkey::new_unique()).await), perr(PerpsError::InvalidVenueParams));
    try_register_venue(&mut env.base, &admin, &RELAY2, p, ua).await.unwrap();
    assert!(try_register_venue(&mut env.base, &admin, &RELAY2, p, ua).await.is_err(), "a venue is registered once");

    let v = load_venue(&mut env.base, &RELAY2).await;
    assert_eq!(v.authority, venue_authority(&env.base.market, &RELAY2).0);
    assert_eq!((v.market, v.program, v.program_data, v.upgrade_authority), (env.base.market, RELAY2, program_data_pda(&RELAY2), ua));
    assert_eq!((v.max_price_deviation_bps, v.max_oracle_age_secs, v.min_signers, v.max_fill_notional), (50, 15, 0, 50_000 * USDC));
    assert!(v.enabled && v.total_fills == 0);

    assert_eq!(custom_error(update_venue(&mut env.base, &guardian, &RELAY2, p, false, ua).await), perr(PerpsError::Unauthorized));
    let wide = VenueParams { max_price_deviation_bps: 100, max_oracle_age_secs: 30, ..p };
    update_venue(&mut env.base, &admin, &RELAY2, wide, false, ua).await.unwrap();
    let v = load_venue(&mut env.base, &RELAY2).await;
    assert_eq!((v.max_price_deviation_bps, v.max_oracle_age_secs, v.enabled, v.program, v.market), (100, 30, false, RELAY2, env.base.market));

    // The admin handover moves venue control too
    let new_admin = rotate_admin(&mut env.base).await;
    assert_eq!(custom_error(update_venue(&mut env.base, &admin, &RELAY2, p, true, ua).await), perr(PerpsError::Unauthorized));
    update_venue(&mut env.base, &new_admin, &RELAY2, p, true, ua).await.unwrap();
}

#[tokio::test]
async fn venue_params_are_validated() {
    let mut env = Env::new().await;
    let admin = env.base.admin.insecure_clone();
    add_venue_program_data(&mut env.base, &RELAY2, 1).await;
    let ua = admin.pubkey();
    let p = default_venue_params();
    let bad = [
        VenueParams { max_price_deviation_bps: 501, ..p },
        VenueParams { max_oracle_age_secs: 0, ..p },
        VenueParams { max_oracle_age_secs: 61, ..p },
        VenueParams { min_signers: 3, ..p },
        VenueParams { max_fill_notional: 0, ..p },
    ];
    for params in bad {
        assert_eq!(custom_error(try_register_venue(&mut env.base, &admin, &RELAY2, params, ua).await), perr(PerpsError::InvalidVenueParams));
        assert_eq!(custom_error(update_venue(&mut env.base, &admin, &RELAY, params, true, ua).await), perr(PerpsError::InvalidVenueParams));
    }
    let edges = VenueParams { max_price_deviation_bps: 500, max_oracle_age_secs: 60, min_signers: 2, max_fill_notional: 1 };
    try_register_venue(&mut env.base, &admin, &RELAY2, edges, ua).await.unwrap();

    // perps-core itself and non-executable accounts can't be venues
    assert_eq!(custom_error(try_register_venue(&mut env.base, &admin, &perps_core::ID, p, ua).await), perr(PerpsError::InvalidVenueParams));
    let wallet = funded_keypair(&mut env.base).await.pubkey();
    add_venue_program_data(&mut env.base, &wallet, 1).await;
    assert_eq!(custom_error(try_register_venue(&mut env.base, &admin, &wallet, p, ua).await), perr(PerpsError::InvalidVenueParams));
}

#[tokio::test]
async fn registering_in_a_settled_market_fails() {
    let mut env = Env::new().await;
    let admin = env.base.admin.insecure_clone();
    add_venue_program_data(&mut env.base, &RELAY2, 1).await;
    settle(&mut env.base, 75 * USDC).await.unwrap();
    assert_eq!(custom_error(try_register_venue(&mut env.base, &admin, &RELAY2, default_venue_params(), admin.pubkey()).await), perr(PerpsError::MarketSettled));
}

#[tokio::test]
async fn approval_params_are_validated() {
    let mut env = Env::new().await;
    let erin = trader(&mut env.base, 0).await;
    let t = env.now().await;
    let ok = approval_params(USDC, t);
    let bad = [
        VenueApprovalParams { max_leverage: 0, ..ok },
        VenueApprovalParams { max_leverage: 20_001, ..ok },
        VenueApprovalParams { max_price_deviation_bps: 10_001, ..ok },
        VenueApprovalParams { expires_at: t, ..ok },
        VenueApprovalParams { expires_at: t + 90 * 86_400 + 1, ..ok },
        VenueApprovalParams { allowed_sides: 0, ..ok },
        VenueApprovalParams { allowed_sides: 4, ..ok },
    ];
    let venue = env.venue;
    for params in bad {
        assert_eq!(custom_error(approve(&mut env.base, &erin.kp, &venue, ROLE_TAKER, params).await.map(|_| ())), perr(PerpsError::InvalidApprovalParams));
    }
    assert_eq!(custom_error(approve(&mut env.base, &erin.kp, &venue, 2, ok).await.map(|_| ())), perr(PerpsError::InvalidApprovalParams));
    let edge = VenueApprovalParams { max_leverage: 20_000, max_price_deviation_bps: 10_000, expires_at: t + 90 * 86_400, ..ok };
    let approval = approve(&mut env.base, &erin.kp, &venue, ROLE_TAKER, edge).await.unwrap();
    let a = env.approval(approval).await;
    assert_eq!((a.owner, a.venue, a.market, a.role, a.allowance, a.expires_at), (erin.key(), venue, env.base.market, ROLE_TAKER, USDC, t + 90 * 86_400));
    for params in bad {
        assert_eq!(custom_error(update_approval(&mut env.base, &erin.kp, &venue, ROLE_TAKER, params).await), perr(PerpsError::InvalidApprovalParams));
    }
    // Someone else can't update it
    let stranger = funded_keypair(&mut env.base).await;
    let mut forged = update_approval_ix(&env.base, &stranger.pubkey(), &venue, ROLE_TAKER, ok);
    forged.accounts[3].pubkey = approval;
    assert_eq!(custom_error(send(&mut env.base.ctx, &[forged], &[&stranger]).await), aerr(AnchorError::ConstraintSeeds));
}

#[tokio::test]
async fn fill_compute_units() {
    let mut env = Env::new().await;
    let (fa, n) = env.std_accounts();
    let payer = env.base.ctx.payer.pubkey();
    let ix = relay_ix(&RELAY, &fa, &payer, fill_params(n), &[]);
    let (result, units) = simulate(&mut env.base.ctx, &[ix], &[]).await;
    result.unwrap();
    println!("venue_fill via relay: {} CU", units);
    if is_sbf() {
        assert!(units > 0 && units < 100_000, "{} CU", units);
    }
}

/// The relay's instruction forwarding an arbitrary perps-core instruction.
fn relay_wrap(relay_program: &Pubkey, inner: Instruction) -> Instruction {
    let mut accounts = vec![AccountMeta::new_readonly(perps_core::ID, false)];
    accounts.extend(inner.accounts);
    Instruction { program_id: *relay_program, accounts, data: inner.data }
}

#[tokio::test]
async fn a_venue_cannot_widen_a_signing_takers_approval() {
    // The taker signs the transaction that calls the venue, and a signer stays
    // a signer in every CPI. Approvals must only be writable at top level, or
    // a compromised venue could raise the taker's allowance and then fill.
    let mut env = Env::new().await;
    let t = env.now().await;
    let (taker, venue) = (env.taker.kp.insecure_clone(), env.venue);
    let wide = approval_params(1_000_000 * USDC, t);
    let inner = update_approval_ix(&env.base, &taker.pubkey(), &venue, ROLE_TAKER, wide);
    let result = send(&mut env.base.ctx, &[relay_wrap(&RELAY, inner)], &[&taker]).await;
    assert_eq!(custom_error(result), perr(PerpsError::ApprovalViaCpi));

    let fresh = trader(&mut env.base, DEPOSIT).await;
    let inner = approve_ix(&env.base, &fresh.key(), &venue, ROLE_TAKER, wide);
    let result = send(&mut env.base.ctx, &[relay_wrap(&RELAY, inner)], &[&fresh.kp]).await;
    assert_eq!(custom_error(result), perr(PerpsError::ApprovalViaCpi));

    // The same writes at top level still work, and revoking works from a CPI
    update_approval(&mut env.base, &taker, &venue, ROLE_TAKER, wide).await.unwrap();
    let revoke = relay_wrap(&RELAY, revoke_ix(&taker.pubkey(), &venue, ROLE_TAKER));
    send(&mut env.base.ctx, &[revoke], &[&taker]).await.unwrap();
    assert!(!exists(&mut env.base, approval_pda(&taker.pubkey(), &venue, ROLE_TAKER)).await);
}
