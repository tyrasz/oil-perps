//! The Pyth Receiver `PriceUpdateV2` oracle end-to-end (docs/lp-pool.md
//! D47): every check `read_price` makes, at every instruction that reads a
//! price, the feed validation of `initialize_market`, and the timelocked
//! feed rotation (`propose_oracle_feed` / `apply_oracle_feed` /
//! `cancel_oracle_feed`), on the shared harness. Natively and on SBF.

use crate::support::*;
use perps_core::errors::PerpsError;
use perps_core::events::{OracleFeedApplied, OracleFeedCancelled, OracleFeedProposed};
use perps_core::instructions::{MarketConfigParams, ProtocolRolesParams};
use perps_core::pyth_receiver::PriceUpdateV2View;
use perps_core::state::PoolParams;

fn perr(e: PerpsError) -> u32 {
    perps_err(e)
}

/// A Full update for the OIL feed at $75, published at `t`.
fn good(t: i64) -> PriceUpdateV2View {
    PriceUpdateV2View::new(OIL_FEED_ID, pyth_dollars(OIL_PRICE), 0, -8, t)
}

fn with_disc_byte_flipped(mut d: Vec<u8>) -> Vec<u8> {
    d[0] ^= 1;
    d
}

/// A legacy Pyth v2 price account header (what the OIL market used to point
/// at): magic, version 2, price type, in the old 3_312 bytes.
fn legacy_v2_account() -> Vec<u8> {
    let mut d = vec![0u8; 3_312];
    d[0..4].copy_from_slice(&0xa1b2_c3d4u32.to_le_bytes());
    d[4..8].copy_from_slice(&2u32.to_le_bytes());
    d[8..12].copy_from_slice(&3u32.to_le_bytes());
    d
}

/// `(what, data, owner, expected error)` for each way the market's pinned
/// account can be refused, at clock `t`.
fn refusals(t: i64) -> Vec<(&'static str, Vec<u8>, Pubkey, u32)> {
    let g = good(t);
    let invalid_account = perr(PerpsError::InvalidOracleAccount);
    vec![
        ("wrong owner", g.to_bytes(), Pubkey::new_unique(), invalid_account),
        ("system-owned", g.to_bytes(), system_program::ID, invalid_account),
        ("wrong discriminator", with_disc_byte_flipped(g.to_bytes()), PYTH_PROGRAM, invalid_account),
        ("legacy v2 account", legacy_v2_account(), PYTH_PROGRAM, invalid_account),
        ("truncated", g.to_bytes()[..132].to_vec(), PYTH_PROGRAM, invalid_account),
        ("empty", vec![], PYTH_PROGRAM, invalid_account),
        ("unknown verification", { let mut d = g.to_bytes(); d[40] = 2; d }, PYTH_PROGRAM, invalid_account),
        ("partial", g.to_partial_bytes(13), PYTH_PROGRAM, perr(PerpsError::OracleNotFullyVerified)),
        ("other feed id", PriceUpdateV2View { feed_id: [7; 32], ..g }.to_bytes(), PYTH_PROGRAM, perr(PerpsError::OracleFeedIdMismatch)),
        ("61 s old", PriceUpdateV2View { publish_time: t - 61, ..g }.to_bytes(), PYTH_PROGRAM, perr(PerpsError::StaleOraclePrice)),
        ("61 s ahead", PriceUpdateV2View { publish_time: t + 61, ..g }.to_bytes(), PYTH_PROGRAM, perr(PerpsError::StaleOraclePrice)),
        // $75 ± 1.0001%
        ("wide confidence", PriceUpdateV2View { conf: 75_010_000, ..g }.to_bytes(), PYTH_PROGRAM, perr(PerpsError::OracleConfidenceTooWide)),
        ("zero price", PriceUpdateV2View { price: 0, ..g }.to_bytes(), PYTH_PROGRAM, perr(PerpsError::InvalidOraclePrice)),
        ("negative price", PriceUpdateV2View { price: -3_763_000_000, ..g }.to_bytes(), PYTH_PROGRAM, perr(PerpsError::InvalidOraclePrice)),
        // i64::MAX at expo 0 overflows the 6-dp u64
        ("overflow", PriceUpdateV2View { price: i64::MAX, exponent: 0, ..g }.to_bytes(), PYTH_PROGRAM, perr(PerpsError::InvalidOraclePrice)),
        ("huge exponent", PriceUpdateV2View { price: 1, exponent: i32::MAX, ..g }.to_bytes(), PYTH_PROGRAM, perr(PerpsError::InvalidOraclePrice)),
        // Rounds to zero at 6 dp
        ("tiny exponent", PriceUpdateV2View { exponent: i32::MIN, ..g }.to_bytes(), PYTH_PROGRAM, perr(PerpsError::InvalidOraclePrice)),
        ("sub-micro price", PriceUpdateV2View { price: 99, exponent: -8, ..g }.to_bytes(), PYTH_PROGRAM, perr(PerpsError::InvalidOraclePrice)),
        // Order: owner before layout, verification before feed id, feed id before age, age before price
        ("wrong owner and partial", g.to_partial_bytes(1), Pubkey::new_unique(), invalid_account),
        ("partial and other feed id", PriceUpdateV2View { feed_id: [7; 32], ..g }.to_partial_bytes(1), PYTH_PROGRAM, perr(PerpsError::OracleNotFullyVerified)),
        ("other feed id and stale", PriceUpdateV2View { feed_id: [7; 32], publish_time: t - 600, ..g }.to_bytes(), PYTH_PROGRAM, perr(PerpsError::OracleFeedIdMismatch)),
        ("stale and negative", PriceUpdateV2View { price: -1, publish_time: t - 600, ..g }.to_bytes(), PYTH_PROGRAM, perr(PerpsError::StaleOraclePrice)),
    ]
}

/// Updates `read_price` accepts at `t`, and the 6-dp price it returns.
fn acceptances(t: i64) -> Vec<(&'static str, PriceUpdateV2View, u64)> {
    let g = good(t);
    vec![
        ("60 s old", PriceUpdateV2View { publish_time: t - 60, ..g }, 75_000_000),
        ("60 s ahead", PriceUpdateV2View { publish_time: t + 60, ..g }, 75_000_000),
        // Exactly 1% confidence
        ("1% confidence", PriceUpdateV2View { conf: 75_000_000, ..g }, 75_000_000),
        ("expo 0", PriceUpdateV2View { price: 75, exponent: 0, ..g }, 75_000_000),
        ("positive expo", PriceUpdateV2View { price: 7, exponent: 1, conf: 0, ..g }, 70_000_000),
        ("expo -12", PriceUpdateV2View { price: 75_123_456_789_012, exponent: -12, ..g }, 75_123_456),
        // The 134th byte is not looked at
        ("Partial-length tail", g, 75_000_000),
        // Prev publish time, EMA, posted slot and authority are not used
        ("other metadata", PriceUpdateV2View { prev_publish_time: 0, ema_price: -9, ema_conf: u64::MAX, posted_slot: 1, write_authority: [3; 32], ..g }, 75_000_000),
    ]
}

/// Every refusal at `open_position`, then the accepted updates open at the
/// price they carry.
#[tokio::test]
async fn read_price_refuses_every_bad_update() {
    let mut base = start(ProgramTest::default()).await;
    let alice = trader(&mut base, 10_000 * USDC).await;
    let t = now(&mut base).await;
    for (what, data, owner, code) in refusals(t) {
        set_feed(&mut base, data, owner).await;
        let (ix, _) = open_ix(&mut base, &alice.key(), 0, CONTRACT, 5_000).await;
        assert_eq!(custom_error(send(&mut base.ctx, &[ix], &[&alice.kp]).await), code, "{}", what);
    }
    for (what, v, price) in acceptances(t) {
        let mut data = v.to_bytes();
        if what == "Partial-length tail" {
            data[133] = 0xff;
        }
        set_feed(&mut base, data, PYTH_PROGRAM).await;
        let position = open(&mut base, &alice, 0, 1, 5).await.unwrap_or_else(|e| panic!("{}: {:?}", what, e));
        let p: perps_core::state::Position = load(&mut base, position).await;
        // The house fills at the oracle price plus its skew premium; the
        // premium is 0 at the pool's neutral params
        assert_eq!(p.entry_price, price, "{}", what);
    }
}

/// The floor and the configured confidence limit come from the market.
#[tokio::test]
async fn market_limits_apply_to_the_update() {
    let mut base = start(ProgramTest::default()).await;
    let alice = trader(&mut base, 10_000 * USDC).await;
    let t = now(&mut base).await;
    configure(&mut base, MarketConfigParams { min_oracle_price: usd(80.0), max_confidence_bps: 300, ..Default::default() }).await.unwrap();
    set_feed(&mut base, good(t).to_bytes(), PYTH_PROGRAM).await;
    assert_eq!(custom_error(open(&mut base, &alice, 0, 1, 5).await.map(|_| ())), perr(PerpsError::OraclePriceBelowFloor));
    // $81 ± 2.9%: inside the 3% limit
    let v = PriceUpdateV2View { price: pyth_dollars(81.0), conf: pyth_dollars(81.0 * 0.029) as u64, ..good(t) };
    set_feed(&mut base, v.to_bytes(), PYTH_PROGRAM).await;
    open(&mut base, &alice, 0, 1, 5).await.unwrap();
    let v = PriceUpdateV2View { conf: pyth_dollars(81.0 * 0.031) as u64, ..v };
    set_feed(&mut base, v.to_bytes(), PYTH_PROGRAM).await;
    assert_eq!(custom_error(open(&mut base, &alice, 0, 1, 5).await.map(|_| ())), perr(PerpsError::OracleConfidenceTooWide));
}

/// Another account in the feed slot fails the Accounts constraint
/// (ConstraintRaw) before read_price runs, even a valid update for the same
/// feed id owned by the oracle program.
#[tokio::test]
async fn only_the_pinned_account_is_read() {
    let mut base = start(ProgramTest::default()).await;
    let alice = trader(&mut base, 10_000 * USDC).await;
    let t = now(&mut base).await;
    let other = Pubkey::new_unique();
    base.ctx.set_account(&other, &AccountSharedData::from(pyth_account(pyth_dollars(70.0), 0, -8, t)));
    let pinned = base.oracle;
    base.oracle = other;
    let (ix, _) = open_ix(&mut base, &alice.key(), 0, CONTRACT, 5_000).await;
    assert_eq!(custom_error(send(&mut base.ctx, &[ix], &[&alice.kp]).await), anchor_err(anchor_lang::error::ErrorCode::ConstraintRaw));
    let ix = update_funding_ix(&base);
    assert_eq!(custom_error(send(&mut base.ctx, &[ix], &[]).await), anchor_err(anchor_lang::error::ErrorCode::ConstraintRaw));
    base.oracle = pinned;
    open(&mut base, &alice, 0, 1, 5).await.unwrap();
}

fn update_funding_ix(base: &Base) -> Instruction {
    let accounts = perps_core::accounts::UpdateFunding { keeper: base.ctx.payer.pubkey(), market: base.market, pyth_price_feed: base.oracle };
    ix(perps_core::ID, accounts, perps_core::instruction::UpdateFunding {})
}

/// Every instruction that reads a price goes through the same checks:
/// open, close, liquidate, update_funding, lp_deposit,
/// lp_request_withdrawal, lp_withdraw, settle_position_pnl and adl (venue_fill
/// is in `venue_fill_reads_the_feed_like_open_position`).
#[tokio::test]
async fn every_price_reader_runs_the_checks() {
    let mut base = start(ProgramTest::default()).await;
    let alice = trader(&mut base, 10_000 * USDC).await;
    let keeper = trader(&mut base, 0).await;
    let position = open(&mut base, &alice, 0, 10, 5).await.unwrap();
    let l2 = lp(&mut base, 1_000 * USDC).await;
    lp_deposit(&mut base, &l2, 1_000 * USDC, 0).await.unwrap();
    let l3 = lp(&mut base, 1_000 * USDC).await;
    lp_deposit(&mut base, &l3, 1_000 * USDC, 0).await.unwrap();
    // l3 has a request to claim (neutral params: no delay)
    let shares = lp_position(&mut base, &l3).await.shares;
    lp_request(&mut base, &l3, shares).await.unwrap();
    let t0 = now(&mut base).await;
    // Funding falls due an hour on, so update_funding reaches the oracle
    set_time(&mut base, t0 + 3_600).await;
    let t = now(&mut base).await;

    let payer = base.ctx.payer.pubkey();
    let (open_ix_, _) = open_ix(&mut base, &alice.key(), 0, CONTRACT, 5_000).await;
    let readers: Vec<(&str, Instruction, Vec<&Keypair>)> = vec![
        ("open_position", open_ix_, vec![&alice.kp]),
        ("close_position", close_ix(&base, &alice, position), vec![&alice.kp]),
        ("liquidate", liquidate_ix(&base, &keeper, &alice.key(), position), vec![&keeper.kp]),
        ("update_funding", update_funding_ix(&base), vec![]),
        ("lp_deposit", lp_deposit_ix(&base, &l2, USDC, 0), vec![&l2.kp]),
        ("lp_request_withdrawal", lp_request_ix(&base, &l2, 1), vec![&l2.kp]),
        ("lp_withdraw", lp_withdraw_ix(&base, &l3), vec![&l3.kp]),
        ("settle_position_pnl", settle_pnl_ix(&base, &payer, position), vec![]),
        ("adl", adl_ix(&base, &payer, &alice.key(), position, 0, 0), vec![]),
    ];
    let checks = ["wrong owner", "wrong discriminator", "truncated", "partial", "other feed id", "61 s old", "61 s ahead", "wide confidence", "negative price"];
    for (what, data, owner, code) in refusals(t).into_iter().filter(|r| checks.contains(&r.0)) {
        set_feed(&mut base, data, owner).await;
        for (name, ix, signers) in &readers {
            assert_eq!(custom_error(send(&mut base.ctx, &[ix.clone()], signers).await), code, "{} at {}", what, name);
        }
    }
    // With a good update every one of them gets past the oracle (the ones
    // that then refuse do so for their own reasons)
    set_feed(&mut base, good(t).to_bytes(), PYTH_PROGRAM).await;
    update_funding(&mut base).await.unwrap();
    lp_withdraw(&mut base, &l3).await.unwrap();
    close(&mut base, &alice, position).await.unwrap();
}

/// A settled market prices at the settlement price, so it does not need
/// the oracle at all: closes work whatever the feed holds (the path for a
/// negative or unpriceable WTI).
#[tokio::test]
async fn settled_markets_do_not_read_the_feed() {
    let mut base = start(ProgramTest::default()).await;
    let alice = trader(&mut base, 10_000 * USDC).await;
    let position = open(&mut base, &alice, 0, 10, 5).await.unwrap();
    let t = now(&mut base).await;
    set_feed(&mut base, PriceUpdateV2View { price: -3_763_000_000, ..good(t) }.to_bytes(), PYTH_PROGRAM).await;
    assert_eq!(custom_error(close(&mut base, &alice, position).await), perr(PerpsError::InvalidOraclePrice));
    settle(&mut base, usd(70.0)).await.unwrap();
    set_feed(&mut base, good(t).to_partial_bytes(1), Pubkey::new_unique()).await;
    close(&mut base, &alice, position).await.unwrap();
}

// --- initialize_market ----------------------------------------------------------

/// The market records the feed id and the oracle program it was created
/// with (docs/lp-pool.md D47).
#[tokio::test]
async fn market_records_the_feed_id_and_oracle_program() {
    let mut base = start(ProgramTest::default()).await;
    let m = market(&mut base).await;
    assert_eq!((m.pyth_price_feed, m.pyth_feed_id, m.oracle_program), (base.oracle, OIL_FEED_ID, PYTH_PROGRAM));
    assert_eq!((m.pending_price_feed, m.pending_feed_id, m.pending_oracle_at), (Pubkey::default(), [0; 32], 0));
    let key = base.market;
    assert_eq!(raw(&mut base, key).await.unwrap().data.len(), perps_core::state::Market::LEN);
    assert_eq!(perps_core::state::Market::LEN, 434);
}

// --- feed rotation -----------------------------------------------------------------

/// A second feed account at `key`, owned by the oracle program, for
/// `feed_id`, published now.
async fn second_feed(base: &mut Base, feed_id: [u8; 32], dollars: f64) -> Pubkey {
    let key = Pubkey::new_unique();
    let t = now(base).await;
    let v = PriceUpdateV2View::new(feed_id, pyth_dollars(dollars), 0, -8, t);
    base.ctx.set_account(&key, &AccountSharedData::from(feed_account(v.to_bytes(), PYTH_PROGRAM)));
    key
}

const XAU: [u8; 32] = [0x76; 32];

/// Before any LP (no shares) a new feed applies at once, with both events.
#[tokio::test]
async fn rotation_applies_at_once_before_the_seed() {
    let mut base = start_seeded(ProgramTest::default(), PoolParams::neutral(), 0).await;
    assert_eq!(pool(&mut base).await.lp_shares, 0);
    let new = second_feed(&mut base, XAU, 80.0).await;
    let admin = base.admin.insecure_clone();
    let t = now(&mut base).await;
    let ix = propose_oracle_feed_ix(&base, &admin.pubkey(), new, XAU);
    let proposed: Vec<OracleFeedProposed> = send_events(&mut base.ctx, &[ix.clone()], &[&admin]).await.unwrap();
    assert_eq!(proposed.len(), 1);
    assert_eq!((proposed[0].market, proposed[0].price_feed, proposed[0].feed_id, proposed[0].effective_at), (base.market, new, XAU, t));
    let m = market(&mut base).await;
    assert_eq!((m.pyth_price_feed, m.pyth_feed_id, m.oracle_program, m.pending_oracle_at), (new, XAU, PYTH_PROGRAM, 0));
    // The same proposal again emits Applied too
    let applied: Vec<OracleFeedApplied> = send_events(&mut base.ctx, &[ix], &[&admin]).await.unwrap();
    assert_eq!(applied.len(), 1);
    assert_eq!((applied[0].price_feed, applied[0].feed_id, applied[0].effective_at), (new, XAU, t));

    // The old account is no longer the market's; the new one prices
    let seed = Lp { kp: base.seed_lp.kp.insecure_clone(), tokens: base.seed_lp.tokens, position: base.seed_lp.position };
    let ix = lp_deposit_ix(&base, &seed, LP_SEED, 0);
    assert_eq!(custom_error(send(&mut base.ctx, &[ix], &[&seed.kp]).await), anchor_err(anchor_lang::error::ErrorCode::ConstraintRaw));
    base.oracle = new;
    lp_deposit(&mut base, &seed, LP_SEED, 0).await.unwrap();
    let alice = trader(&mut base, 10_000 * USDC).await;
    let p = open(&mut base, &alice, 0, 1, 5).await.unwrap();
    assert_eq!(load::<perps_core::state::Position>(&mut base, p).await.entry_price, 80_000_000);
}

/// With LPs in the pool the change waits `params_timelock_secs(d, d, W, W)`
/// of the pool's params: 4 d + 1 d + 5 d = 10 d at production params.
/// Until then the old feed prices; apply is refused before the time, then
/// anyone applies it.
#[tokio::test]
async fn rotation_is_timelocked_once_lps_are_in() {
    let mut base = start_with(ProgramTest::default(), PoolParams::production()).await;
    let admin = base.admin.insecure_clone();
    let new = second_feed(&mut base, XAU, 80.0).await;
    let t = now(&mut base).await;
    let ix = propose_oracle_feed_ix(&base, &admin.pubkey(), new, XAU);
    let ev: Vec<OracleFeedProposed> = send_events(&mut base.ctx, &[ix], &[&admin]).await.unwrap();
    let eta = t + 864_000;
    assert_eq!((ev.len(), ev[0].price_feed, ev[0].feed_id, ev[0].effective_at), (1, new, XAU, eta));
    let m = market(&mut base).await;
    assert_eq!((m.pyth_price_feed, m.pyth_feed_id), (base.oracle, OIL_FEED_ID));
    assert_eq!((m.pending_price_feed, m.pending_feed_id, m.pending_oracle_at), (new, XAU, eta));

    // The old feed still prices in the meantime
    let alice = trader(&mut base, 10_000 * USDC).await;
    open(&mut base, &alice, 0, 1, 5).await.unwrap();
    assert_eq!(custom_error(apply_oracle_feed(&mut base, new).await), perr(PerpsError::OracleChangeTimelocked));
    set_time(&mut base, eta - 1).await;
    assert_eq!(custom_error(apply_oracle_feed(&mut base, new).await), perr(PerpsError::OracleChangeTimelocked));
    set_time(&mut base, eta).await;
    // Only the pending account
    let old = base.oracle;
    assert_eq!(custom_error(apply_oracle_feed(&mut base, old).await), perr(PerpsError::InvalidOracleAccount));
    // Anyone (the payer) applies; the new feed's age is not checked here
    let ix = apply_oracle_feed_ix(&base, new);
    let ev: Vec<OracleFeedApplied> = send_events(&mut base.ctx, &[ix], &[]).await.unwrap();
    assert_eq!((ev.len(), ev[0].market, ev[0].price_feed, ev[0].feed_id, ev[0].effective_at), (1, base.market, new, XAU, eta));
    let m = market(&mut base).await;
    assert_eq!((m.pyth_price_feed, m.pyth_feed_id, m.oracle_program), (new, XAU, PYTH_PROGRAM));
    assert_eq!((m.pending_price_feed, m.pending_feed_id, m.pending_oracle_at), (Pubkey::default(), [0; 32], 0));
    // Applied once: nothing pending now
    assert_eq!(custom_error(apply_oracle_feed(&mut base, Pubkey::default()).await), perr(PerpsError::OracleChangeTimelocked));
    // The new feed prices (set_time restamps it as base.oracle)
    base.oracle = new;
    set_time(&mut base, eta).await;
    update_funding(&mut base).await.unwrap();
}

/// The neutral params' timelock is the 5 d margin (`d = W = 0`), never under 1 d.
#[tokio::test]
async fn rotation_timelock_follows_the_pool_params() {
    let mut base = start(ProgramTest::default()).await;
    let admin = base.admin.insecure_clone();
    let new = second_feed(&mut base, XAU, 80.0).await;
    let t = now(&mut base).await;
    propose_oracle_feed(&mut base, &admin, new, XAU).await.unwrap();
    assert_eq!(market(&mut base).await.pending_oracle_at, t + 5 * 86_400);
    assert_eq!(perps_core::pool_math::params_timelock_secs(0, 0, 0, 0), 5 * 86_400);
    assert_eq!(perps_core::pool_math::params_timelock_secs(345_600, 345_600, 86_400, 86_400), 864_000);
}

/// apply re-validates the feed and the configured oracle program as they
/// are at apply time.
#[tokio::test]
async fn apply_revalidates_the_pending_feed() {
    let mut base = start(ProgramTest::default()).await;
    let admin = base.admin.insecure_clone();
    let new = second_feed(&mut base, XAU, 80.0).await;
    let t = now(&mut base).await;
    propose_oracle_feed(&mut base, &admin, new, XAU).await.unwrap();
    let eta = market(&mut base).await.pending_oracle_at;
    set_time(&mut base, eta).await;
    let v = PriceUpdateV2View::new(XAU, pyth_dollars(80.0), 0, -8, t);
    for (data, owner, code) in [
        (v.to_partial_bytes(3), PYTH_PROGRAM, perr(PerpsError::OracleNotFullyVerified)),
        (PriceUpdateV2View { feed_id: OIL_FEED_ID, ..v }.to_bytes(), PYTH_PROGRAM, perr(PerpsError::OracleFeedIdMismatch)),
        (v.to_bytes(), Pubkey::new_unique(), perr(PerpsError::InvalidOracleAccount)),
        (with_disc_byte_flipped(v.to_bytes()), PYTH_PROGRAM, perr(PerpsError::InvalidOracleAccount)),
    ] {
        base.ctx.set_account(&new, &AccountSharedData::from(feed_account(data, owner)));
        assert_eq!(custom_error(apply_oracle_feed(&mut base, new).await), code);
    }
    base.ctx.set_account(&new, &AccountSharedData::from(feed_account(v.to_bytes(), PYTH_PROGRAM)));
    // The admin moves the oracle program: the pending feed is no longer its
    let new_program = Pubkey::new_unique();
    update_roles(&mut base, &admin, new_program).await.unwrap();
    assert_eq!(custom_error(apply_oracle_feed(&mut base, new).await), perr(PerpsError::InvalidOracleAccount));
    update_roles(&mut base, &admin, PYTH_PROGRAM).await.unwrap();
    apply_oracle_feed(&mut base, new).await.unwrap();
}

/// The market keeps the oracle program it was set up with: an
/// update_roles change reaches it only through a rotation.
#[tokio::test]
async fn oracle_program_changes_reach_markets_only_through_rotation() {
    let mut base = start_seeded(ProgramTest::default(), PoolParams::neutral(), 0).await;
    let admin = base.admin.insecure_clone();
    let new_program = Pubkey::new_unique();
    update_roles(&mut base, &admin, new_program).await.unwrap();
    // The market still reads its PYTH_PROGRAM feed
    let seed = Lp { kp: base.seed_lp.kp.insecure_clone(), tokens: base.seed_lp.tokens, position: base.seed_lp.position };
    lp_deposit(&mut base, &seed, LP_SEED / 2, 0).await.unwrap();
    // A feed owned by the new program cannot be read until rotated in
    let t = now(&mut base).await;
    let moved = PriceUpdateV2View::new(OIL_FEED_ID, pyth_dollars(75.0), 0, -8, t).to_bytes();
    set_feed(&mut base, moved.clone(), new_program).await;
    assert_eq!(custom_error(lp_deposit(&mut base, &seed, USDC, 0).await), perr(PerpsError::InvalidOracleAccount));
    // Rotating to the same account under the new program: timelocked now
    // (LPs are in), then applied
    let oracle = base.oracle;
    propose_oracle_feed(&mut base, &admin, oracle, OIL_FEED_ID).await.unwrap();
    let eta = market(&mut base).await.pending_oracle_at;
    set_time(&mut base, eta).await;
    apply_oracle_feed(&mut base, oracle).await.unwrap();
    assert_eq!(market(&mut base).await.oracle_program, new_program);
    update_funding(&mut base).await.unwrap();
    lp_deposit(&mut base, &seed, USDC, 0).await.unwrap();
}

async fn update_roles(base: &mut Base, admin: &Keypair, oracle_program: Pubkey) -> Result<(), BanksClientError> {
    let guardian = base.guardian.pubkey();
    let ix = ix(
        perps_core::ID,
        perps_core::accounts::AdminOnly { admin: admin.pubkey(), config: config_pda() },
        perps_core::instruction::UpdateRoles { params: ProtocolRolesParams { guardian, oracle_program } },
    );
    send(&mut base.ctx, &[ix], &[admin]).await
}

#[tokio::test]
async fn propose_checks_signer_feed_and_market() {
    let mut base = start(ProgramTest::default()).await;
    let admin = base.admin.insecure_clone();
    let guardian = base.guardian.insecure_clone();
    let new = second_feed(&mut base, XAU, 80.0).await;
    for who in [&guardian, &Keypair::new()] {
        fund(&mut base.ctx, &who.pubkey()).await;
        assert_eq!(custom_error(propose_oracle_feed(&mut base, who, new, XAU).await), perr(PerpsError::Unauthorized));
        assert_eq!(custom_error(cancel_oracle_feed(&mut base, who).await), perr(PerpsError::Unauthorized));
    }
    assert_eq!(custom_error(propose_oracle_feed(&mut base, &admin, new, [0; 32]).await), perr(PerpsError::InvalidMarketConfig));
    assert_eq!(custom_error(propose_oracle_feed(&mut base, &admin, new, OIL_FEED_ID).await), perr(PerpsError::OracleFeedIdMismatch));
    let t = now(&mut base).await;
    let v = PriceUpdateV2View::new(XAU, pyth_dollars(80.0), 0, -8, t);
    for (data, owner, code) in [
        (v.to_bytes(), Pubkey::new_unique(), perr(PerpsError::InvalidOracleAccount)),
        (v.to_partial_bytes(5), PYTH_PROGRAM, perr(PerpsError::OracleNotFullyVerified)),
        (v.to_bytes()[..100].to_vec(), PYTH_PROGRAM, perr(PerpsError::InvalidOracleAccount)),
    ] {
        let k = Pubkey::new_unique();
        base.ctx.set_account(&k, &AccountSharedData::from(feed_account(data, owner)));
        assert_eq!(custom_error(propose_oracle_feed(&mut base, &admin, k, XAU).await), code);
    }
    // A stale update is fine to propose (the market may be closed)
    let stale = Pubkey::new_unique();
    let old = PriceUpdateV2View { publish_time: t - 86_400, ..v };
    base.ctx.set_account(&stale, &AccountSharedData::from(feed_account(old.to_bytes(), PYTH_PROGRAM)));
    propose_oracle_feed(&mut base, &admin, stale, XAU).await.unwrap();
    // A new proposal replaces it and restarts the clock
    set_time(&mut base, t + 100).await;
    propose_oracle_feed(&mut base, &admin, new, XAU).await.unwrap();
    let m = market(&mut base).await;
    assert_eq!((m.pending_price_feed, m.pending_oracle_at), (new, t + 100 + 5 * 86_400));
    // Settled markets keep their feed
    settle(&mut base, usd(70.0)).await.unwrap();
    assert_eq!(custom_error(propose_oracle_feed(&mut base, &admin, new, XAU).await), perr(PerpsError::MarketSettled));
}

/// A rotation left pending by settle_market cannot be applied: a settled
/// market's feed is final (D52). The admin can still cancel it.
#[tokio::test]
async fn apply_is_refused_once_settled() {
    let mut base = start(ProgramTest::default()).await;
    let admin = base.admin.insecure_clone();
    let new = second_feed(&mut base, XAU, 80.0).await;
    propose_oracle_feed(&mut base, &admin, new, XAU).await.unwrap();
    let eta = market(&mut base).await.pending_oracle_at;
    settle(&mut base, usd(70.0)).await.unwrap();
    set_time(&mut base, eta).await;
    assert_eq!(custom_error(apply_oracle_feed(&mut base, new).await), perr(PerpsError::MarketSettled));
    let m = market(&mut base).await;
    assert_eq!((m.pyth_price_feed, m.pyth_feed_id, m.pending_price_feed), (base.oracle, OIL_FEED_ID, new));
    cancel_oracle_feed(&mut base, &admin).await.unwrap();
    assert_eq!(market(&mut base).await.pending_oracle_at, 0);
}

/// Under one of Pyth's Receivers a rotation must name the push oracle's
/// shard-0 PDA for the feed id (D52), at propose and again at apply.
#[tokio::test]
async fn rotation_under_a_pyth_receiver_pins_the_push_feed_account() {
    use perps_core::pyth_push::{push_feed_address, PYTH_PUSH_ORACLE_LEGACY, PYTH_PUSH_ORACLE_PRO, PYTH_RECEIVER_PRO};
    let mut base = start(ProgramTest::default()).await;
    let admin = base.admin.insecure_clone();
    update_roles(&mut base, &admin, PYTH_RECEIVER_PRO).await.unwrap();
    let t = now(&mut base).await;
    let data = PriceUpdateV2View::new(XAU, pyth_dollars(80.0), 0, -8, t).to_bytes();
    for key in [Pubkey::new_unique(), push_feed_address(&PYTH_PUSH_ORACLE_LEGACY, &XAU), push_feed_address(&PYTH_PUSH_ORACLE_PRO, &OIL_FEED_ID)] {
        base.ctx.set_account(&key, &AccountSharedData::from(feed_account(data.clone(), PYTH_RECEIVER_PRO)));
        assert_eq!(custom_error(propose_oracle_feed(&mut base, &admin, key, XAU).await), perr(PerpsError::InvalidOracleAccount));
    }
    let pda = push_feed_address(&PYTH_PUSH_ORACLE_PRO, &XAU);
    base.ctx.set_account(&pda, &AccountSharedData::from(feed_account(data, PYTH_RECEIVER_PRO)));
    propose_oracle_feed(&mut base, &admin, pda, XAU).await.unwrap();
    let eta = market(&mut base).await.pending_oracle_at;
    set_time(&mut base, eta).await;
    apply_oracle_feed(&mut base, pda).await.unwrap();
    let m = market(&mut base).await;
    assert_eq!((m.pyth_price_feed, m.pyth_feed_id, m.oracle_program), (pda, XAU, PYTH_RECEIVER_PRO));
}

#[tokio::test]
async fn cancel_clears_the_pending_feed() {
    let mut base = start(ProgramTest::default()).await;
    let admin = base.admin.insecure_clone();
    assert_eq!(custom_error(cancel_oracle_feed(&mut base, &admin).await), perr(PerpsError::NoPendingOracleChange));
    let new = second_feed(&mut base, XAU, 80.0).await;
    propose_oracle_feed(&mut base, &admin, new, XAU).await.unwrap();
    let eta = market(&mut base).await.pending_oracle_at;
    let ix = cancel_oracle_feed_ix(&base, &admin.pubkey());
    let ev: Vec<OracleFeedCancelled> = send_events(&mut base.ctx, &[ix], &[&admin]).await.unwrap();
    assert_eq!((ev.len(), ev[0].market, ev[0].price_feed, ev[0].feed_id, ev[0].effective_at), (1, base.market, new, XAU, eta));
    let m = market(&mut base).await;
    assert_eq!((m.pyth_price_feed, m.pyth_feed_id), (base.oracle, OIL_FEED_ID));
    assert_eq!((m.pending_price_feed, m.pending_feed_id, m.pending_oracle_at), (Pubkey::default(), [0; 32], 0));
    assert_eq!(custom_error(cancel_oracle_feed(&mut base, &admin).await), perr(PerpsError::NoPendingOracleChange));
    // Nothing to apply at or after the old time
    set_time(&mut base, eta).await;
    assert_eq!(custom_error(apply_oracle_feed(&mut base, new).await), perr(PerpsError::InvalidOracleAccount));
}
