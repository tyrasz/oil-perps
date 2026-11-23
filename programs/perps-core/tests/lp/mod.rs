//! LP pool lifecycle end-to-end (docs/lp-pool.md §5, §9, §10; test plan
//! §17.2 #2, #3, #16, #17, #18): deposits, withdrawal requests and their
//! gates, donations, the insurance sweep, the treasury, settled-market closes
//! and the params timelock, on the shared harness (pool on
//! `PoolParams::neutral()`, seeded with `LP_SEED`).
//!
//! Some states that need specific pool numbers (pending debt, cash losses,
//! insurance) are written with `patch` and documented where they are.

use crate::support::*;
use perps_core::errors::PerpsError;
use perps_core::state::{LpPool, Market, PoolParams, Position, PositionStatus, LP_DEAD_SHARES};

const HOUR: i64 = 3_600;
const DAY: i64 = 86_400;
/// Thu 2026-11-12 12:00 ET
const THURSDAY_NOON: i64 = FRIDAY_CLOSE - 29 * HOUR;
/// Fri 2026-11-13 10:00 ET: inside a 24 h pool pre-close window
const FRIDAY_MORNING: i64 = FRIDAY_CLOSE - 7 * HOUR;
/// Sun 2026-11-15 18:00 ET: the weekly reopen
const SUNDAY_OPEN: i64 = FRIDAY_CLOSE + 2 * DAY + HOUR;

fn perr(e: PerpsError) -> u32 {
    perps_err(e)
}

async fn base_with(params: PoolParams) -> Base {
    start_with(ProgramTest::default(), params).await
}

fn seed_lp(base: &Base) -> Lp {
    Lp { kp: base.seed_lp.kp.insecure_clone(), tokens: base.seed_lp.tokens, position: base.seed_lp.position }
}

fn update_funding_ix(base: &Base) -> Instruction {
    let accounts = perps_core::accounts::UpdateFunding { keeper: base.ctx.payer.pubkey(), market: base.market, pyth_price_feed: base.oracle };
    ix(perps_core::ID, accounts, perps_core::instruction::UpdateFunding {})
}

/// Send `ix`, preceded by an update_funding crank when funding is not
/// current (LP flows require it, §8).
async fn send_current(base: &mut Base, ix: Instruction, signer: &Keypair) -> Result<(), BanksClientError> {
    let t = now(base).await;
    let mut ixs = Vec::new();
    if !market(base).await.funding_current(t) {
        ixs.push(update_funding_ix(base));
    }
    ixs.push(ix);
    send(&mut base.ctx, &ixs, &[signer]).await
}

/// An LP with `usdc` in its wallet that deposits all of it.
async fn deposited_lp(base: &mut Base, usdc: u64) -> Lp {
    let l = lp(base, usdc).await;
    let ix = lp_deposit_ix(base, &l, usdc, 0);
    send_current(base, ix, &l.kp).await.unwrap();
    l
}

/// floor(a·b/c) in u128.
fn mul_div(a: u64, b: u64, c: u64) -> u64 {
    (a as u128 * b as u128 / c as u128) as u64
}

async fn patch_pool(base: &mut Base, f: impl FnOnce(&mut LpPool)) {
    let p = base.lp_pool;
    patch::<LpPool>(base, p, f).await;
}

async fn patch_market(base: &mut Base, f: impl FnOnce(&mut Market)) {
    let m = base.market;
    patch::<Market>(base, m, f).await;
}

/// Mint tokens straight into the vault, to back a bucket a test writes.
async fn fund_vault(base: &mut Base, amount: u64) {
    let vt = base.vault_token;
    mint_to(base, vt, amount).await;
}

// ---------------------------------------------------------------------------
// Deposits (§9.1)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn first_deposit_locks_the_dead_shares() {
    let mut base = start_seeded(ProgramTest::default(), PoolParams::neutral(), 0).await;
    let seed = seed_lp(&base);
    let admin = base.admin.insecure_clone();
    assert_eq!(pool(&mut base).await.lp_shares, 0);

    // Nothing to donate to or sweep into before the seed
    assert_eq!(custom_error(donate(&mut base, &admin, seed.tokens, USDC).await), perr(PerpsError::PoolHasNoLps));
    assert_eq!(custom_error(sweep(&mut base).await), perr(PerpsError::PoolHasNoLps));

    // Before the seed, params apply at once
    let p = PoolParams { withdrawal_delay_secs: 3_600, ..PoolParams::neutral() };
    propose_pool_params(&mut base, &admin, p).await.unwrap();
    let pl = pool(&mut base).await;
    assert_eq!((pl.params, pl.pending_params_at), (p, 0));

    assert_eq!(custom_error(lp_deposit(&mut base, &seed, 0, 0).await), perr(PerpsError::InvalidLpAmount));
    assert_eq!(custom_error(lp_deposit(&mut base, &seed, LP_DEAD_SHARES, 0).await), perr(PerpsError::InvalidLpAmount));
    assert_eq!(custom_error(lp_deposit(&mut base, &seed, 5 * USDC, 4 * USDC + 1).await), perr(PerpsError::SlippageExceeded));
    lp_deposit(&mut base, &seed, 5 * USDC, 4 * USDC).await.unwrap();

    // 5 USDC mints 5e6 shares, LP_DEAD_SHARES of them owned by nobody
    let pl = pool(&mut base).await;
    assert_eq!((pl.lp_shares, pl.cash), (5 * USDC, 5 * USDC));
    let pos = lp_position(&mut base, &seed).await;
    assert_eq!((pos.shares, pos.deposited_cum), (5 * USDC - LP_DEAD_SHARES, 5 * USDC));

    // Later deposits mint floor(amount·shares / NAV_mint), NAV_mint = cash + 1
    let l2 = lp(&mut base, 3 * USDC).await;
    lp_deposit(&mut base, &l2, 3 * USDC, 0).await.unwrap();
    assert_eq!(lp_position(&mut base, &l2).await.shares, 2_999_999);
    assert_eq!(pool(&mut base).await.lp_shares, 5 * USDC + 2_999_999);

    // With LPs in the pool, a proposal waits for the timelock
    propose_pool_params(&mut base, &admin, PoolParams::neutral()).await.unwrap();
    assert_ne!(pool(&mut base).await.pending_params_at, 0);
    assert_eq!(pool(&mut base).await.params, p);
}

#[tokio::test]
async fn harness_seed_and_later_deposits_floor() {
    let mut base = base_with(PoolParams::neutral()).await;
    let seed = seed_lp(&base);
    let pl = pool(&mut base).await;
    assert_eq!((pl.lp_shares, pl.cash, pl.params), (LP_SEED, LP_SEED, PoolParams::neutral()));
    assert_eq!(lp_position(&mut base, &seed).await.shares, LP_SEED - LP_DEAD_SHARES);
    assert_eq!(vault_token_balance(&mut base).await, LP_SEED);

    let l2 = lp(&mut base, 1_000 * USDC).await;
    // floor(1e9·1e12 / (1e12 + 1))
    let expected = 999_999_999;
    assert_eq!(custom_error(lp_deposit(&mut base, &l2, 1_000 * USDC, expected + 1).await), perr(PerpsError::SlippageExceeded));
    lp_deposit(&mut base, &l2, 1_000 * USDC, expected).await.unwrap();
    assert_eq!(lp_position(&mut base, &l2).await.shares, expected);
    assert_eq!(token_balance(&mut base, l2.tokens).await, 0);
    assert_eq!(vault_token_balance(&mut base).await, LP_SEED + 1_000 * USDC);
    // Rounding is in the pool's favour: per-share NAV did not fall
    let pl = pool(&mut base).await;
    assert!(pl.cash as u128 * LP_SEED as u128 >= LP_SEED as u128 * pl.lp_shares as u128);
}

#[tokio::test]
async fn max_pool_cash_caps_lp_nav() {
    let mut base = base_with(PoolParams::neutral()).await;
    let admin = base.admin.insecure_clone();
    let stranger = funded_keypair(&mut base).await;
    assert_eq!(custom_error(set_max_pool_cash(&mut base, &stranger, 1).await), perr(PerpsError::Unauthorized));

    // NAV_mint = cash + 1 (slack)
    set_max_pool_cash(&mut base, &admin, LP_SEED + 1 + 100 * USDC).await.unwrap();
    let l2 = lp(&mut base, 200 * USDC).await;
    lp_deposit(&mut base, &l2, 100 * USDC, 0).await.unwrap();
    assert_eq!(custom_error(lp_deposit(&mut base, &l2, 1, 0).await), perr(PerpsError::PoolCashCapExceeded));
    set_max_pool_cash(&mut base, &admin, 0).await.unwrap();
    // 1 unit mints 0 shares
    assert_eq!(custom_error(lp_deposit(&mut base, &l2, 1, 0).await), perr(PerpsError::SlippageExceeded));
    lp_deposit(&mut base, &l2, 100 * USDC, 0).await.unwrap();
}

#[tokio::test]
async fn deposit_gates() {
    let mut base = base_with(PoolParams { lp_reopen_quiet_secs: 1_800, ..PoolParams::neutral() }).await;
    let guardian = base.guardian.insecure_clone();
    let l2 = lp(&mut base, 1_000 * USDC).await;

    pause(&mut base, &guardian).await.unwrap();
    assert_eq!(custom_error(lp_deposit(&mut base, &l2, USDC, 0).await), perr(PerpsError::MarketPaused));
    configure(&mut base, cme_config()).await.unwrap(); // unpauses, CME hours

    set_time(&mut base, SATURDAY).await;
    assert_eq!(custom_error(lp_deposit(&mut base, &l2, USDC, 0).await), perr(PerpsError::MarketClosed));

    // The reopen quiet time, even with funding cranked
    set_time(&mut base, SUNDAY_OPEN + 600).await;
    let dep = lp_deposit_ix(&base, &l2, USDC, 0);
    assert_eq!(custom_error(send_current(&mut base, dep, &l2.kp).await), perr(PerpsError::LpFlowWindowClosed));

    // After it: funding must be current, and a crank in the same transaction does it
    set_time(&mut base, SUNDAY_OPEN + 1_801).await;
    assert_eq!(custom_error(lp_deposit(&mut base, &l2, USDC, 0).await), perr(PerpsError::FundingStale));
    let dep = lp_deposit_ix(&base, &l2, USDC, 0);
    send_current(&mut base, dep, &l2.kp).await.unwrap();

    // A stale oracle
    let t = now(&mut base).await;
    set_price_raw(&mut base, pyth_dollars(OIL_PRICE), 0, t - 61).await;
    assert_eq!(custom_error(lp_deposit(&mut base, &l2, USDC, 0).await), perr(PerpsError::StaleOraclePrice));
}

#[tokio::test]
async fn pending_bad_debt_and_insolvency_block_deposits_until_a_donation() {
    let mut base = base_with(PoolParams::neutral()).await;
    let seed = seed_lp(&base);
    let l2 = lp(&mut base, 1_000 * USDC).await;
    let donor = funded_keypair(&mut base).await;
    let donor_tokens = token_account(&mut base, &donor.pubkey(), 2_000_000 * USDC).await;
    let t = now(&mut base).await;

    // Bad debt booked on the long side (as liquidate will)
    patch_pool(&mut base, |p| {
        p.pending_bad_debt_long = 30 * USDC;
        p.pending_since_long = t;
    })
    .await;
    assert_eq!(custom_error(lp_deposit(&mut base, &l2, 100 * USDC, 0).await), perr(PerpsError::PoolAdlPending));
    // Requests are never gated; withdrawals are
    lp_request(&mut base, &seed, USDC).await.unwrap();
    assert_eq!(custom_error(lp_withdraw(&mut base, &seed).await), perr(PerpsError::PoolAdlPending));
    lp_cancel(&mut base, &seed).await.unwrap();

    assert_eq!(custom_error(donate(&mut base, &donor, donor_tokens, 0).await), perr(PerpsError::InvalidLpAmount));
    donate(&mut base, &donor, donor_tokens, 100 * USDC).await.unwrap();
    let p = pool(&mut base).await;
    assert!(!p.adl_required());
    assert_eq!((p.pending_since_long, p.bad_debt_recovered_cum, p.donated_cum), (0, 30 * USDC, 100 * USDC));
    // The rest vests (over max(withdrawal_delay_secs, 1) = 1 s)
    assert_eq!((p.vest_amount, p.vest_start, p.vest_end), (70 * USDC, t, t + 1));
    assert_eq!(p.cash, LP_SEED + 100 * USDC);
    lp_deposit(&mut base, &l2, 100 * USDC, 0).await.unwrap();

    // Traders up $1.5M on an unhedged pool (20k contracts long at entry 0): NAV_mint < 0
    patch_market(&mut base, |m| m.long_open_interest = 20_000 * CONTRACT).await;
    assert_eq!(custom_error(lp_deposit(&mut base, &l2, 100 * USDC, 0).await), perr(PerpsError::PoolInsolvent));
    // A donation, not a cheap mint, recapitalises it (red-team F34)
    donate(&mut base, &donor, donor_tokens, 600_000 * USDC).await.unwrap();
    lp_deposit(&mut base, &l2, 100 * USDC, 0).await.unwrap();
}

// ---------------------------------------------------------------------------
// Withdrawals (§9.2–9.5)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn withdrawal_delay_is_fixed_at_request_time() {
    let delay = 4 * DAY as u32;
    let mut base = base_with(PoolParams { withdrawal_delay_secs: delay, ..PoolParams::neutral() }).await;
    let admin = base.admin.insecure_clone();
    let l2 = deposited_lp(&mut base, 1_000 * USDC).await;
    let s2 = lp_position(&mut base, &l2).await.shares;

    assert_eq!(custom_error(lp_request(&mut base, &l2, 0).await), perr(PerpsError::InsufficientShares));
    assert_eq!(custom_error(lp_request(&mut base, &l2, s2 + 1).await), perr(PerpsError::InsufficientShares));
    lp_request(&mut base, &l2, s2 / 2).await.unwrap();
    assert_eq!(custom_error(lp_request(&mut base, &l2, 1).await), perr(PerpsError::WithdrawalPending));
    assert_eq!(pool(&mut base).await.pending_withdraw_shares, s2 / 2);
    lp_cancel(&mut base, &l2).await.unwrap();
    assert_eq!(custom_error(lp_cancel(&mut base, &l2).await), perr(PerpsError::NoWithdrawalPending));
    assert_eq!(custom_error(lp_withdraw(&mut base, &l2).await), perr(PerpsError::NoWithdrawalPending));
    let pos = lp_position(&mut base, &l2).await;
    assert_eq!((pos.pending_shares, pos.requested_value, pos.unlock_at), (0, 0, 0));
    assert_eq!(pool(&mut base).await.pending_withdraw_shares, 0);

    // Propose an 8-day delay: timelock max(1 d, 8 d + 0 + 5 d) = 13 d
    let t0 = now(&mut base).await;
    propose_pool_params(&mut base, &admin, PoolParams { withdrawal_delay_secs: 2 * delay, ..PoolParams::neutral() }).await.unwrap();
    assert_eq!(pool(&mut base).await.pending_params_at, t0 + 13 * DAY);

    // A request a day before it lands keeps the 4-day delay it was made under
    set_time(&mut base, t0 + 12 * DAY).await;
    let req = lp_request_ix(&base, &l2, s2);
    send_current(&mut base, req, &l2.kp).await.unwrap();
    let pos = lp_position(&mut base, &l2).await;
    assert_eq!((pos.requested_at, pos.unlock_at, pos.claim_window_secs), (t0 + 12 * DAY, t0 + 16 * DAY, 0));
    set_time(&mut base, t0 + 13 * DAY).await;
    apply_pool_params(&mut base).await.unwrap();
    assert_eq!(pool(&mut base).await.params.withdrawal_delay_secs, 2 * delay);

    set_time(&mut base, t0 + 16 * DAY - 1).await;
    let w = lp_withdraw_ix(&base, &l2);
    assert_eq!(custom_error(send_current(&mut base, w, &l2.kp).await), perr(PerpsError::WithdrawalDelayNotElapsed));
    set_time(&mut base, t0 + 16 * DAY).await;
    let before = pool(&mut base).await;
    let w = lp_withdraw_ix(&base, &l2);
    send_current(&mut base, w, &l2.kp).await.unwrap();

    let value = token_balance(&mut base, l2.tokens).await;
    assert_eq!(value, pos.requested_value);
    assert!(value < 1_000 * USDC && value + 2 >= 1_000 * USDC, "{}", value);
    let after = pool(&mut base).await;
    assert_eq!((after.cash, after.lp_shares, after.pending_withdraw_shares), (before.cash - value, before.lp_shares - s2, 0));
    let pos = lp_position(&mut base, &l2).await;
    assert_eq!((pos.shares, pos.pending_shares, pos.withdrawn_cum), (0, 0, value));
    assert_eq!(vault_token_balance(&mut base).await, LP_SEED + 1_000 * USDC - value);
}

#[tokio::test]
async fn matured_requests_lapse_after_the_claim_window_in_open_seconds() {
    let p = PoolParams { withdrawal_delay_secs: DAY as u32, withdrawal_claim_window_secs: DAY as u32, ..PoolParams::neutral() };
    let mut base = base_with(p).await;
    configure(&mut base, cme_config()).await.unwrap();
    let a = deposited_lp(&mut base, 1_000 * USDC).await;
    let b = deposited_lp(&mut base, 1_000 * USDC).await;
    let (sa, sb) = (lp_position(&mut base, &a).await.shares, lp_position(&mut base, &b).await.shares);

    // Requested Thursday noon: unlock Friday noon. A day of open time is
    // Fri 12:00–17:00 (5 h) plus Sun 18:00 – Mon 13:00 (19 h).
    set_time(&mut base, THURSDAY_NOON).await;
    for (l, s) in [(&a, sa), (&b, sb)] {
        let req = lp_request_ix(&base, l, s);
        send_current(&mut base, req, &l.kp).await.unwrap();
    }
    assert_eq!(lp_position(&mut base, &a).await.claim_window_secs, DAY as u32);
    let monday_1pm = FRIDAY_CLOSE + 68 * HOUR;
    set_time(&mut base, monday_1pm).await;
    let w = lp_withdraw_ix(&base, &a);
    send_current(&mut base, w, &a.kp).await.unwrap();

    set_time(&mut base, monday_1pm + 1).await;
    let w = lp_withdraw_ix(&base, &b);
    assert_eq!(custom_error(send_current(&mut base, w, &b.kp).await), perr(PerpsError::WithdrawalExpired));
    // Cancel and re-request: the delay restarts. From Tue 13:00:01 a day of
    // open time ends Wed 14:00:01 (the 17:00 break does not count).
    lp_cancel(&mut base, &b).await.unwrap();
    let req = lp_request_ix(&base, &b, sb);
    send_current(&mut base, req, &b.kp).await.unwrap();
    let unlock = monday_1pm + 1 + DAY;
    assert_eq!(lp_position(&mut base, &b).await.unlock_at, unlock);
    set_time(&mut base, unlock + 25 * HOUR).await;
    let w = lp_withdraw_ix(&base, &b);
    send_current(&mut base, w, &b.kp).await.unwrap();
    assert!(token_balance(&mut base, b.tokens).await > 0);
    assert_eq!(perps_core::session::open_seconds_between(1, unlock, unlock + 25 * HOUR), DAY);
}

/// Review finding (D38): a request keeps the claim window in force when it
/// was made, but a later apply that widens `W` or `Q` (patched here) extends
/// it to `W + 5·Q + 1 day`, so the request cannot lapse inside time the new
/// gates close.
#[tokio::test]
async fn wider_gates_extend_a_stored_claim_window() {
    let p = PoolParams { withdrawal_delay_secs: DAY as u32, withdrawal_claim_window_secs: DAY as u32, ..PoolParams::neutral() };
    let mut base = base_with(p).await;
    configure(&mut base, cme_config()).await.unwrap();
    let b = deposited_lp(&mut base, 1_000 * USDC).await;
    let sb = lp_position(&mut base, &b).await.shares;
    set_time(&mut base, THURSDAY_NOON).await;
    let req = lp_request_ix(&base, &b, sb);
    send_current(&mut base, req, &b.kp).await.unwrap();
    assert_eq!(lp_position(&mut base, &b).await.claim_window_secs, DAY as u32);
    // One open second past the stored day (as in the lapse test above)
    let monday_1pm = FRIDAY_CLOSE + 68 * HOUR;
    set_time(&mut base, monday_1pm + 1).await;
    patch_pool(&mut base, |p| p.params.weekend_cap_window_secs = HOUR as u32).await;
    let w = lp_withdraw_ix(&base, &b);
    send_current(&mut base, w, &b.kp).await.unwrap();
    assert!(token_balance(&mut base, b.tokens).await > 0);
}

#[tokio::test]
async fn pre_close_window_and_reopen_quiet_time_block_exits() {
    let p = PoolParams { weekend_cap_window_secs: DAY as u32, lp_reopen_quiet_secs: 1_800, ..PoolParams::neutral() };
    let mut base = base_with(p).await;
    configure(&mut base, cme_config()).await.unwrap();
    let l2 = deposited_lp(&mut base, 1_000 * USDC).await;
    lp_request(&mut base, &l2, 1_000).await.unwrap();

    // Inside W: no exits, but deposits are fine
    set_time(&mut base, FRIDAY_MORNING).await;
    let w = lp_withdraw_ix(&base, &l2);
    assert_eq!(custom_error(send_current(&mut base, w, &l2.kp).await), perr(PerpsError::LpFlowWindowClosed));
    deposited_lp(&mut base, 500 * USDC).await;

    set_time(&mut base, SATURDAY).await;
    assert_eq!(custom_error(lp_withdraw(&mut base, &l2).await), perr(PerpsError::MarketClosed));

    // The quiet time after the reopen blocks both directions
    set_time(&mut base, SUNDAY_OPEN + 1_799).await;
    let w = lp_withdraw_ix(&base, &l2);
    assert_eq!(custom_error(send_current(&mut base, w, &l2.kp).await), perr(PerpsError::LpFlowWindowClosed));
    let l3 = lp(&mut base, 10 * USDC).await;
    let d = lp_deposit_ix(&base, &l3, 10 * USDC, 0);
    assert_eq!(custom_error(send_current(&mut base, d, &l3.kp).await), perr(PerpsError::LpFlowWindowClosed));

    set_time(&mut base, SUNDAY_OPEN + 1_800).await;
    let w = lp_withdraw_ix(&base, &l2);
    send_current(&mut base, w, &l2.kp).await.unwrap();
}

#[tokio::test]
async fn withdrawal_pays_the_lower_of_request_and_now() {
    let mut base = base_with(PoolParams::neutral()).await;
    let donor = funded_keypair(&mut base).await;
    let donor_tokens = token_account(&mut base, &donor.pubkey(), 10_000 * USDC).await;

    // NAV rises during the delay: the request's value is paid, not more
    let l2 = deposited_lp(&mut base, 1_000 * USDC).await;
    let s2 = lp_position(&mut base, &l2).await.shares;
    lp_request(&mut base, &l2, s2).await.unwrap();
    let requested = lp_position(&mut base, &l2).await.requested_value;
    let p = pool(&mut base).await;
    assert_eq!(requested, mul_div(s2, p.cash - 1, p.lp_shares));
    donate(&mut base, &donor, donor_tokens, 10_000 * USDC).await.unwrap();
    let t = now(&mut base).await;
    set_time(&mut base, t + 1).await; // fully vested
    lp_withdraw(&mut base, &l2).await.unwrap();
    assert_eq!(token_balance(&mut base, l2.tokens).await, requested);

    // NAV falls during the delay: the leaver bears it (a $100k pool loss)
    let l3 = deposited_lp(&mut base, 1_000 * USDC).await;
    let s3 = lp_position(&mut base, &l3).await.shares;
    lp_request(&mut base, &l3, s3).await.unwrap();
    let requested = lp_position(&mut base, &l3).await.requested_value;
    patch_pool(&mut base, |p| p.cash -= 100_000 * USDC).await;
    let p = pool(&mut base).await;
    let expected = mul_div(s3, p.cash - 1, p.lp_shares);
    lp_withdraw(&mut base, &l3).await.unwrap();
    assert_eq!(token_balance(&mut base, l3.tokens).await, expected);
    assert!(expected < requested);
}

#[tokio::test]
async fn withdrawal_needs_cash_behind_it() {
    let mut base = base_with(PoolParams::neutral()).await;
    let seed = seed_lp(&base);
    // Traders down $750k (10k contracts short at entry 0): NAV_redeem counts
    // h = 50% of it, more than the cash on hand
    patch_market(&mut base, |m| m.short_open_interest = 10_000 * CONTRACT).await;
    lp_request(&mut base, &seed, LP_SEED - LP_DEAD_SHARES).await.unwrap();
    assert!(lp_position(&mut base, &seed).await.requested_value > LP_SEED);
    assert_eq!(custom_error(lp_withdraw(&mut base, &seed).await), perr(PerpsError::PoolCashInsufficient));
}

#[tokio::test]
async fn hard_net_post_check_counts_other_pending_shares() {
    let p = PoolParams { k_net_bps: 10_000, hard_net_bps: 15_000, ..PoolParams::neutral() };
    let mut base = base_with(p).await;
    let seed = seed_lp(&base);
    let l2 = deposited_lp(&mut base, 1_000_000 * USDC).await;
    let s2 = lp_position(&mut base, &l2).await.shares;

    // 16k contracts long at $75 (U = 0): $1.2M of net the pool is short
    let n = 16_000 * CONTRACT;
    patch_market(&mut base, |m| m.long_open_interest = n).await;
    patch_pool(&mut base, |p| {
        p.long_entry_sum = n as u128 * usd(75.0) as u128;
        p.open_positions = 1;
    })
    .await;

    // Both LPs ask to leave. The first exit leaves B' ≈ $1M ≥ $1.2M / 1.5,
    // counting the seed's pending shares; the second would leave nothing.
    lp_request(&mut base, &seed, LP_SEED - LP_DEAD_SHARES).await.unwrap();
    lp_request(&mut base, &l2, s2).await.unwrap();
    lp_withdraw(&mut base, &l2).await.unwrap();
    assert_eq!(custom_error(lp_withdraw(&mut base, &seed).await), perr(PerpsError::PoolCapWouldBreach));
}

#[tokio::test]
async fn drawdown_block_stops_withdrawals_when_enabled() {
    let mut base = base_with(PoolParams { dd_withdraw_block_bps: 1_000, ..PoolParams::neutral() }).await;
    let seed = seed_lp(&base);
    // The next pool view sets the peak at nps = 1.0
    deposited_lp(&mut base, 1_000 * USDC).await;
    assert_eq!(pool(&mut base).await.nps_peak, 1_000_000);
    // A 20% loss of LP cash
    patch_pool(&mut base, |p| p.cash = p.cash / 5 * 4).await;
    lp_request(&mut base, &seed, USDC).await.unwrap();
    assert_eq!(custom_error(lp_withdraw(&mut base, &seed).await), perr(PerpsError::PoolWithdrawalsBlocked));
    // After a week the peak resets and the block lifts
    let t = now(&mut base).await;
    set_time(&mut base, t + 7 * DAY).await;
    let w = lp_withdraw_ix(&base, &seed);
    send_current(&mut base, w, &seed.kp).await.unwrap();
}

// ---------------------------------------------------------------------------
// Insurance, treasury, donations (§10.1–10.4)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sweep_recovers_pending_then_vests_the_excess() {
    let delay = 4 * DAY as u32;
    let p = PoolParams { insurance_target: 100 * USDC, withdrawal_delay_secs: delay, ..PoolParams::neutral() };
    let mut base = base_with(p).await;
    let seed = seed_lp(&base);
    assert_eq!(custom_error(sweep(&mut base).await), perr(PerpsError::NothingToSweep));

    // $300 of fees in the fund (as closes will credit it), $50 of long bad debt pending
    fund_vault(&mut base, 300 * USDC).await;
    patch_market(&mut base, |m| m.insurance_fund = 300 * USDC).await;
    let t = now(&mut base).await;
    patch_pool(&mut base, |p| {
        p.pending_bad_debt_long = 50 * USDC;
        p.pending_since_long = t - 10;
    })
    .await;
    sweep(&mut base).await.unwrap();
    assert_eq!(market(&mut base).await.insurance_fund, 100 * USDC);
    let pl = pool(&mut base).await;
    assert_eq!((pl.cash, pl.pending_bad_debt_long, pl.pending_since_long, pl.bad_debt_recovered_cum), (LP_SEED + 200 * USDC, 0, 0, 50 * USDC));
    assert_eq!((pl.vest_amount, pl.vest_start, pl.vest_end), (150 * USDC, t, t + delay as i64));
    assert_eq!(custom_error(sweep(&mut base).await), perr(PerpsError::NothingToSweep));

    // A deposit just before a sweep pays NAV_mint, which counts unvested
    // value at once, and a request just after is valued without it:
    // depositing around a sweep earns nothing from it (red-team F11)
    let l2 = deposited_lp(&mut base, 1_000 * USDC).await;
    fund_vault(&mut base, 300 * USDC).await;
    patch_market(&mut base, |m| m.insurance_fund += 300 * USDC).await;
    sweep(&mut base).await.unwrap();
    let s2 = lp_position(&mut base, &l2).await.shares;
    lp_request(&mut base, &l2, s2).await.unwrap();
    assert!(lp_position(&mut base, &l2).await.requested_value < 1_000 * USDC);
    // The second sweep merged into the vesting schedule
    let pl = pool(&mut base).await;
    assert_eq!((pl.vest_amount, pl.vest_start, pl.vest_end), (450 * USDC, t, t + delay as i64));

    // Half-way through the delay half of it has vested into NAV_redeem
    let half = t + delay as i64 / 2;
    set_time(&mut base, half).await;
    let req = lp_request_ix(&base, &seed, LP_SEED - LP_DEAD_SHARES);
    send_current(&mut base, req, &seed.kp).await.unwrap();
    let pl = pool(&mut base).await;
    assert_eq!(pl.unvested(half), 225 * USDC);
    let nav_redeem = pl.cash - 225 * USDC - 1;
    assert_eq!(lp_position(&mut base, &seed).await.requested_value, mul_div(LP_SEED - LP_DEAD_SHARES, nav_redeem, pl.lp_shares));
}

#[tokio::test]
async fn treasury_withdrawal_is_admin_only_and_bounded() {
    let mut base = base_with(PoolParams::neutral()).await;
    let admin = base.admin.insecure_clone();
    let stranger = funded_keypair(&mut base).await;
    // $50 of protocol revenue (as the fee split will credit it)
    fund_vault(&mut base, 50 * USDC).await;
    patch_pool(&mut base, |p| p.treasury = 50 * USDC).await;
    let dest = token_account(&mut base, &admin.pubkey(), 0).await;

    assert_eq!(custom_error(withdraw_treasury(&mut base, &stranger, dest, USDC).await), perr(PerpsError::Unauthorized));
    assert_eq!(custom_error(withdraw_treasury(&mut base, &admin, dest, 50 * USDC + 1).await), perr(PerpsError::InsufficientTreasury));
    // Not to the vault itself: a self-transfer would take the treasury off
    // the books and leave its tokens stranded in the vault (review finding)
    let vault_token = base.vault_token;
    assert_eq!(custom_error(withdraw_treasury(&mut base, &admin, vault_token, 10 * USDC).await), perr(PerpsError::VaultInvariantViolated));
    assert_eq!(pool(&mut base).await.treasury, 50 * USDC);
    withdraw_treasury(&mut base, &admin, dest, 50 * USDC).await.unwrap();
    assert_eq!(token_balance(&mut base, dest).await, 50 * USDC);
    assert_eq!(pool(&mut base).await.treasury, 0);
    assert_eq!(vault_token_balance(&mut base).await, LP_SEED);
}

#[tokio::test]
async fn buckets_must_stay_backed() {
    let mut base = base_with(PoolParams::neutral()).await;
    let admin = base.admin.insecure_clone();
    // A treasury the vault does not hold: paying it out would break §11
    patch_pool(&mut base, |p| p.treasury = 50 * USDC).await;
    let dest = token_account(&mut base, &admin.pubkey(), 0).await;
    assert_eq!(custom_error(withdraw_treasury(&mut base, &admin, dest, 50 * USDC).await), perr(PerpsError::VaultInvariantViolated));
}

// ---------------------------------------------------------------------------
// Settled market (§10.5, red-team F15, F26)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn settled_market_positions_are_closed_by_anyone_and_lps_exit() {
    let mut base = base_with(PoolParams::neutral()).await;
    configure(&mut base, cme_config()).await.unwrap();
    let seed = seed_lp(&base);
    let alice = trader(&mut base, 1_000 * USDC).await;
    let bob = trader(&mut base, 1_000 * USDC).await;
    let carol = trader(&mut base, 1_000 * USDC).await;
    // Longs: alice 10 @ 5x ($150), carol 10 @ 10x ($75). Short: bob 20 @ 5x ($300).
    let pa = open(&mut base, &alice, 0, 10, 5).await.unwrap();
    let pc = open(&mut base, &carol, 0, 10, 10).await.unwrap();
    let pb = open(&mut base, &bob, 1, 20, 5).await.unwrap();
    assert_eq!(custom_error(close_settled(&mut base, &bob.key(), pb).await), perr(PerpsError::MarketNotSettled));

    // Settled at $60: alice loses all $150, carol is $75 under water, bob is up $300
    settle(&mut base, usd(60.0)).await.unwrap();
    set_time(&mut base, SATURDAY).await; // no session needed from here on
    // The owner must match the position
    assert!(close_settled(&mut base, &alice.key(), pc).await.is_err());

    close_settled(&mut base, &carol.key(), pc).await.unwrap();
    let pl = pool(&mut base).await;
    // $75 deficit: no insurance yet, so the LP absorbs it (cap = 100% of the window NAV)
    assert_eq!((pl.bad_debt_cum, pl.lp_absorbed_window, pl.pending_bad_debt_long), (75 * USDC, 75 * USDC, 0));
    assert_eq!(pl.cash, LP_SEED + 75 * USDC);
    assert_eq!(custom_error(close_settled(&mut base, &carol.key(), pc).await), perr(PerpsError::PositionAlreadyClosed));

    close_settled(&mut base, &alice.key(), pa).await.unwrap();
    close_settled(&mut base, &bob.key(), pb).await.unwrap();
    // Bob: $300 + $300 - 0.05% of $1,500 credited to free collateral
    assert_eq!(user(&mut base, &bob.key()).await.collateral_balance, 700 * USDC + 599_250_000);
    assert_eq!(user(&mut base, &alice.key()).await.collateral_balance, 850 * USDC);
    assert_eq!(user(&mut base, &carol.key()).await.collateral_balance, 925 * USDC);
    let pb_state: Position = load(&mut base, pb).await;
    assert!(pb_state.status == PositionStatus::Closed);
    assert_eq!(pb_state.realized_pnl, 300 * USDC as i64 - 750_000);

    let m = market(&mut base).await;
    let pl = pool(&mut base).await;
    assert_eq!((m.long_open_interest, m.short_open_interest, m.insurance_fund), (0, 0, 750_000));
    assert_eq!((pl.open_positions, pl.long_entry_sum, pl.short_entry_sum, pl.position_collateral), (0, 0, 0, 0));
    // Every trader's free balance: deposits now feed the bucket too
    assert_eq!(pl.free_collateral, 850 * USDC + 925 * USDC + 700 * USDC + 599_250_000);
    assert_buckets(&mut base).await;
    assert_eq!(pl.cash, LP_SEED - 75 * USDC);
    for u in [&alice, &bob, &carol] {
        assert_eq!(user(&mut base, &u.key()).await.house_entry_notional, 0);
    }

    // Settled and empty: the whole fund sweeps to LPs
    sweep(&mut base).await.unwrap();
    assert_eq!(market(&mut base).await.insurance_fund, 0);
    assert_eq!(pool(&mut base).await.cash, LP_SEED - 75 * USDC + 750_000);
    assert_eq!(custom_error(sweep(&mut base).await), perr(PerpsError::NothingToSweep));

    // The seed LP leaves on a Saturday: settled markets need no session
    let seed_shares = LP_SEED - LP_DEAD_SHARES;
    set_time(&mut base, SATURDAY + 1).await; // the swept value has vested
    assert_eq!(custom_error(lp_deposit(&mut base, &seed, USDC, 0).await), perr(PerpsError::MarketPaused));
    lp_request(&mut base, &seed, seed_shares).await.unwrap();
    let cash = pool(&mut base).await.cash;
    let expected = mul_div(seed_shares, cash - 1, LP_SEED);
    assert_eq!(lp_position(&mut base, &seed).await.requested_value, expected);
    lp_withdraw(&mut base, &seed).await.unwrap();
    assert_eq!(token_balance(&mut base, seed.tokens).await, expected);
    assert_eq!(pool(&mut base).await.lp_shares, LP_DEAD_SHARES);
}

// ---------------------------------------------------------------------------
// Params (§5, §17.2 #18)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pool_params_are_timelocked_once_lps_exist() {
    let mut base = base_with(PoolParams::neutral()).await;
    let admin = base.admin.insecure_clone();
    let stranger = funded_keypair(&mut base).await;
    let t0 = now(&mut base).await;
    let p1 = PoolParams { withdrawal_delay_secs: 2 * DAY as u32, weekend_cap_window_secs: DAY as u32, ..PoolParams::neutral() };

    assert_eq!(custom_error(propose_pool_params(&mut base, &stranger, p1).await), perr(PerpsError::Unauthorized));
    let bad = PoolParams { k_net_bps: 100_001, ..PoolParams::neutral() };
    assert_eq!(custom_error(propose_pool_params(&mut base, &admin, bad).await), perr(PerpsError::InvalidPoolParams));
    assert_eq!(custom_error(apply_pool_params(&mut base).await), perr(PerpsError::ParamsTimelocked));
    assert_eq!(custom_error(cancel_pool_params(&mut base, &admin).await), perr(PerpsError::NoPendingParams));

    // max(1 d, 2 d + 1 d + 5 d) = 8 d
    propose_pool_params(&mut base, &admin, p1).await.unwrap();
    let pl = pool(&mut base).await;
    assert_eq!((pl.pending_params, pl.pending_params_at, pl.params), (p1, t0 + 8 * DAY, PoolParams::neutral()));
    set_time(&mut base, t0 + 8 * DAY - 1).await;
    assert_eq!(custom_error(apply_pool_params(&mut base).await), perr(PerpsError::ParamsTimelocked));
    set_time(&mut base, t0 + 8 * DAY).await;
    apply_pool_params(&mut base).await.unwrap(); // permissionless (the test payer)
    let pl = pool(&mut base).await;
    assert_eq!((pl.params, pl.pending_params_at), (p1, 0));

    // Cutting the delay and window still waits for the current ones
    let t1 = now(&mut base).await;
    propose_pool_params(&mut base, &admin, PoolParams::neutral()).await.unwrap();
    assert_eq!(pool(&mut base).await.pending_params_at, t1 + 8 * DAY);
    assert_eq!(custom_error(cancel_pool_params(&mut base, &stranger).await), perr(PerpsError::Unauthorized));
    cancel_pool_params(&mut base, &admin).await.unwrap();
    assert_eq!(pool(&mut base).await.pending_params_at, 0);
    set_time(&mut base, t1 + 9 * DAY).await;
    assert_eq!(custom_error(apply_pool_params(&mut base).await), perr(PerpsError::ParamsTimelocked));

    // Re-validated against the market at apply: production params need
    // liq_penalty·1e4 < MMR·backstop, which a 2.5% MMR breaks
    let prod = PoolParams::production();
    let t2 = now(&mut base).await;
    propose_pool_params(&mut base, &admin, prod).await.unwrap();
    // max(1 d, max(2 d, 4 d) + max(1 d, 1 d) + 5 d) = 10 d
    assert_eq!(pool(&mut base).await.pending_params_at, t2 + 10 * DAY);
    patch_market(&mut base, |m| m.maintenance_margin_ratio = 250).await;
    set_time(&mut base, t2 + 10 * DAY).await;
    assert_eq!(custom_error(apply_pool_params(&mut base).await), perr(PerpsError::InvalidPoolParams));
    patch_market(&mut base, |m| m.maintenance_margin_ratio = 500).await;
    apply_pool_params(&mut base).await.unwrap();
    assert_eq!(pool(&mut base).await.params, prod);
}

#[tokio::test]
async fn adl_keeper_is_set_by_the_admin() {
    let mut base = base_with(PoolParams::neutral()).await;
    let admin = base.admin.insecure_clone();
    let stranger = funded_keypair(&mut base).await;
    assert_eq!(pool(&mut base).await.adl_keeper, admin.pubkey());
    assert_eq!(custom_error(set_adl_keeper(&mut base, &stranger, stranger.pubkey()).await), perr(PerpsError::Unauthorized));
    let keeper = Pubkey::new_unique();
    set_adl_keeper(&mut base, &admin, keeper).await.unwrap();
    assert_eq!(pool(&mut base).await.adl_keeper, keeper);
}

#[tokio::test]
async fn lp_positions_are_class_zero_only() {
    let mut base = base_with(PoolParams::neutral()).await;
    let kp = funded_keypair(&mut base).await;
    let position = lp_position_pda(&base.lp_pool, &kp.pubkey(), 1);
    let accounts = perps_core::accounts::InitLpPosition { owner: kp.pubkey(), lp_pool: base.lp_pool, lp_position: position, system_program: system_program::ID };
    let ix = ix(perps_core::ID, accounts, perps_core::instruction::InitLpPosition { class: 1 });
    assert_eq!(custom_error(send(&mut base.ctx, &[ix], &[&kp]).await), perr(PerpsError::InvalidLpClass));
}

// ---------------------------------------------------------------------------
// Compute (§14, §17.3): asserted against the SBF binary
// ---------------------------------------------------------------------------

#[tokio::test]
async fn lp_flows_fit_the_compute_budget() {
    let p = PoolParams { withdrawal_delay_secs: DAY as u32, withdrawal_claim_window_secs: DAY as u32, ..PoolParams::neutral() };
    let mut base = base_with(p).await;
    configure(&mut base, cme_config()).await.unwrap();
    let l2 = lp(&mut base, 1_000 * USDC).await;
    let dep = lp_deposit_ix(&base, &l2, 1_000 * USDC, 0);
    let (result, deposit_units) = simulate(&mut base.ctx, &[dep.clone()], &[&l2.kp]).await;
    result.unwrap();
    send(&mut base.ctx, &[dep], &[&l2.kp]).await.unwrap();

    // Worst case for lp_withdraw: not settled, CME hours, the claim window
    // and funding checks (open_seconds_between twice)
    set_time(&mut base, THURSDAY_NOON).await;
    update_funding(&mut base).await.unwrap();
    let s2 = lp_position(&mut base, &l2).await.shares;
    lp_request(&mut base, &l2, s2).await.unwrap();
    set_time(&mut base, FRIDAY_CLOSE + 68 * HOUR).await;
    update_funding(&mut base).await.unwrap();
    let w = lp_withdraw_ix(&base, &l2);
    let (result, withdraw_units) = simulate(&mut base.ctx, &[w], &[&l2.kp]).await;
    result.unwrap();
    if is_sbf() {
        assert!(deposit_units < 100_000, "lp_deposit used {} CU", deposit_units);
        assert!(withdraw_units < 100_000, "lp_withdraw used {} CU", withdraw_units);
    }
    println!("lp_deposit {} CU, lp_withdraw {} CU", deposit_units, withdraw_units);
}

// ---------------------------------------------------------------------------
// First-depositor capture (§9.1, red-team F14)
// ---------------------------------------------------------------------------

/// The classic share-inflation attack does not work: the first deposit
/// locks `LP_DEAD_SHARES`, tokens sent straight to the vault are not LP cash
/// (NAV ignores them), and a donation is shared by the dead shares and every
/// later depositor's fair mint, so the attacker's 1 share captures nothing.
#[tokio::test]
async fn first_deposit_capture_is_prevented() {
    let mut base = start_seeded(ProgramTest::default(), PoolParams::neutral(), 0).await;
    let attacker = lp(&mut base, 20_000 * USDC).await;
    // The smallest first deposit: 1 share of its own, LP_DEAD_SHARES locked
    lp_deposit(&mut base, &attacker, LP_DEAD_SHARES + 1, 0).await.unwrap();
    assert_eq!(lp_position(&mut base, &attacker).await.shares, 1);
    // Inflate by sending tokens straight to the vault: not cash, NAV unchanged
    let vt = base.vault_token;
    let t = spl_token::instruction::transfer(&spl_token::ID, &attacker.tokens, &vt, &attacker.key(), &[], 10_000 * USDC).unwrap();
    send(&mut base.ctx, &[t], &[&attacker.kp]).await.unwrap();
    assert_eq!(pool(&mut base).await.cash, LP_DEAD_SHARES + 1);
    // The victim mints at NAV_mint = cash + 1: floor(1e9·(1e6+1)/(1e6+2))
    let victim = lp(&mut base, 1_000 * USDC).await;
    lp_deposit(&mut base, &victim, 1_000 * USDC, 0).await.unwrap();
    let vs = lp_position(&mut base, &victim).await.shares;
    assert_eq!(vs, (1_000 * USDC as u128 * (LP_DEAD_SHARES + 1) as u128 / (LP_DEAD_SHARES + 2) as u128) as u64);
    lp_request(&mut base, &victim, vs).await.unwrap();
    assert!(lp_position(&mut base, &victim).await.requested_value + 2 >= 1_000 * USDC);
    lp_cancel(&mut base, &victim).await.unwrap();
    // A real donation goes to all shares pro rata; the attacker's one share
    // is worth a few units of it
    donate(&mut base, &attacker.kp, attacker.tokens, 9_000 * USDC).await.unwrap();
    let t = now(&mut base).await;
    set_time(&mut base, t + 1).await; // vested
    lp_request(&mut base, &attacker, 1).await.unwrap();
    assert!(lp_position(&mut base, &attacker).await.requested_value <= 20);
    lp_request(&mut base, &victim, vs).await.unwrap();
    // The victim holds ~99.9% of the shares: its $1,000 and ~$8,991 of the gift
    assert!(lp_position(&mut base, &victim).await.requested_value > 9_900 * USDC);
}
