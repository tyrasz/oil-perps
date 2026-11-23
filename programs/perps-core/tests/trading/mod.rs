//! House trading against the LP pool end-to-end (docs/lp-pool.md §4, §6,
//! §7, §8; test plan §17.2 #1, #7, #8, #10–#15, #19), on the shared harness.

use crate::support::*;
use perps_core::errors::PerpsError;
use perps_core::state::{LpPool, Market, PoolParams, Position, PositionStatus};

const HOUR: i64 = 3_600;
const DAY: i64 = 86_400;
/// Fri 2026-11-13 12:00 ET: in session, outside a 2 h margin window
const FRIDAY_NOON: i64 = FRIDAY_CLOSE - 5 * HOUR;
/// Fri 2026-11-13 10:00 ET: inside a 24 h pool pre-close window
const FRIDAY_MORNING: i64 = FRIDAY_CLOSE - 7 * HOUR;
/// Sun 2026-11-15 18:00 ET: the weekly reopen
const SUNDAY_OPEN: i64 = FRIDAY_CLOSE + 2 * DAY + HOUR;

fn perr(e: PerpsError) -> u32 {
    perps_err(e)
}

fn neutral() -> PoolParams {
    PoolParams::neutral()
}

async fn base_with(params: PoolParams) -> Base {
    start_with(ProgramTest::default(), params).await
}

fn seed_lp(base: &Base) -> Lp {
    Lp { kp: base.seed_lp.kp.insecure_clone(), tokens: base.seed_lp.tokens, position: base.seed_lp.position }
}

async fn patch_pool(base: &mut Base, f: impl FnOnce(&mut LpPool)) {
    let p = base.lp_pool;
    patch::<LpPool>(base, p, f).await;
}

async fn patch_market(base: &mut Base, f: impl FnOnce(&mut Market)) {
    let m = base.market;
    patch::<Market>(base, m, f).await;
}

/// No funding period falls due for the rest of the test, so tests that are
/// not about funding keep exact numbers although every open, close,
/// liquidation and ADL cranks funding inline (D40).
async fn freeze_funding(base: &mut Base) {
    patch_market(base, |m| m.funding_interval = 1 << 40).await;
}

/// The close fee of `x` contracts (6 dp) opened at $75 with 5 bps: D21, D22.
fn fee_at_75(x: u64) -> u64 {
    ((x as u128 * 75 * 5 + 9_999) / 10_000) as u64
}

async fn open_with(base: &mut Base, t: &Trader, side: u8, size: u64, leverage_x: u32, acceptable: u64) -> Result<Pubkey, BanksClientError> {
    let (ix, position) = open_ix_with(base, &t.key(), side, size, leverage_x * 1_000, acceptable).await;
    send(&mut base.ctx, &[ix], &[&t.kp]).await?;
    Ok(position)
}

async fn position(base: &mut Base, p: Pubkey) -> Position {
    load(base, p).await
}

// ---------------------------------------------------------------------------
// §17.2 #1: no trading without LPs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn opens_need_a_seeded_pool() {
    let mut base = start_seeded(ProgramTest::default(), neutral(), 0).await;
    let alice = trader(&mut base, 1_000 * USDC).await;
    assert_eq!(custom_error(open(&mut base, &alice, 0, 1, 5).await.map(|_| ())), perr(PerpsError::PoolHasNoLps));
    // Collateral moves are never gated by the pool
    withdraw(&mut base, &alice, 1_000 * USDC).await.unwrap();
    assert_eq!(pool(&mut base).await.free_collateral, 0);
    assert_buckets(&mut base).await;
}

// ---------------------------------------------------------------------------
// §17.2 #7: house open price, margin and fee
// ---------------------------------------------------------------------------

#[tokio::test]
async fn house_opens_pay_skew_impact_and_an_open_fee() {
    let params = PoolParams { skew_scale: 2_500_000_000_000, max_impact_bps: 100, open_fee_bps: 5, ..neutral() };
    let mut base = base_with(params).await;
    let alice = trader(&mut base, 20_000 * USDC).await;
    let bob = trader(&mut base, 20_000 * USDC).await;
    // 1,000 contracts from zero skew: premium 75·1000/(2·2.5M) = $0.015
    assert_eq!(
        custom_error(open_with(&mut base, &alice, 0, 1_000 * CONTRACT, 5, 75_014_999).await.map(|_| ())),
        perr(PerpsError::SlippageExceeded)
    );
    let pa = open_with(&mut base, &alice, 0, 1_000 * CONTRACT, 5, 75_015_000).await.unwrap();
    let p = position(&mut base, pa).await;
    assert_eq!((p.entry_price, p.opened_price), (75_015_000, 75_015_000));
    // Margin: $75,015 / 5 = $15,003, plus the $15 adverse PnL at the oracle
    assert_eq!(p.collateral, 15_018 * USDC);
    // Open fee: ceil(5 bps of $75,015) = $37.5075, to insurance (neutral target)
    assert_eq!(user(&mut base, &alice.key()).await.collateral_balance, 20_000 * USDC - 15_018 * USDC - 37_507_500);
    assert_eq!(market(&mut base).await.insurance_fund, 37_507_500);
    assert_eq!(user(&mut base, &alice.key()).await.house_entry_notional, 75_015 * USDC);

    // A sell that reduces the skew gets a better price than the oracle
    assert_eq!(
        custom_error(open_with(&mut base, &bob, 1, 1_000 * CONTRACT, 5, 75_015_001).await.map(|_| ())),
        perr(PerpsError::SlippageExceeded)
    );
    let pb = open_with(&mut base, &bob, 1, 1_000 * CONTRACT, 5, 75_015_000).await.unwrap();
    assert_eq!(position(&mut base, pb).await.entry_price, 75_015_000);
    assert_conservation(&mut base, 75_000_000, &[alice.key(), bob.key()], &[pa, pb]).await;

    // An open followed by a close in the same skew state nets to zero:
    // closing the long sells from skew 0 (after bob's short closes first)
    let ix = close_ix_with(&base, &bob, pb, 74_000_000);
    assert_eq!(custom_error(send(&mut base.ctx, &[ix], &[&bob.kp]).await), perr(PerpsError::SlippageExceeded));
    close(&mut base, &bob, pb).await.unwrap();
    // Skew +1000 again; selling 1000 brings it to 0 at the same $75.015
    let before = token_balance(&mut base, alice.tokens).await;
    let ix = close_ix_with(&base, &alice, pa, 75_015_001);
    assert_eq!(custom_error(send(&mut base.ctx, &[ix], &[&alice.kp]).await), perr(PerpsError::SlippageExceeded));
    close(&mut base, &alice, pa).await.unwrap();
    // No PnL; the close fee is ceil(5 bps of the opened $75,015)
    assert_eq!(token_balance(&mut base, alice.tokens).await - before, 15_018 * USDC - 37_507_500);
    assert_buckets(&mut base).await;
}

// ---------------------------------------------------------------------------
// §17.2 #8: caps
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_net_cap_binds_net_raising_opens_only() {
    // 0.1 × capNAV ≈ $100k of net
    let params = PoolParams { k_net_bps: 1_000, adl_net_target_bps: 0, ..neutral() };
    let mut base = base_with(params).await;
    let alice = trader(&mut base, 50_000 * USDC).await;
    let bob = trader(&mut base, 50_000 * USDC).await;
    // capNAV = $1M − 1 unit of slack: net·$75 rounded up must stay ≤ $99,999.9999999
    assert_eq!(custom_error(open_with(&mut base, &alice, 0, 1_333_333_334, 5, 0).await.map(|_| ())), perr(PerpsError::PoolExposureLimit));
    open_with(&mut base, &alice, 0, 1_333_333_333, 5, 0).await.unwrap();
    assert_eq!(custom_error(open_with(&mut base, &alice, 0, CONTRACT, 5, 0).await.map(|_| ())), perr(PerpsError::PoolExposureLimit));
    // LPs lose half the pool: net is now far beyond the cap, but an open
    // that lowers |net| is never capped
    patch_pool(&mut base, |p| p.cash /= 2).await;
    open_with(&mut base, &bob, 1, 500 * CONTRACT, 5, 0).await.unwrap();
    assert_eq!(custom_error(open_with(&mut base, &bob, 1, 2_000 * CONTRACT, 5, 0).await.map(|_| ())), perr(PerpsError::PoolExposureLimit));
}

#[tokio::test]
async fn pending_withdrawals_shrink_capnav_for_opens() {
    let params = PoolParams { k_net_bps: 1_000, adl_net_target_bps: 0, ..neutral() };
    let mut base = base_with(params).await;
    let alice = trader(&mut base, 50_000 * USDC).await;
    let seed = seed_lp(&base);
    // Half the shares pending: capNAV ≈ $500k, the net cap ≈ $50k
    lp_request(&mut base, &seed, LP_SEED / 2).await.unwrap();
    let v = pool_view_at(&mut base, 75_000_000).await;
    assert!(v.cap_nav < v.b / 2 + 1 && v.cap_nav * 2 + 2 >= v.b);
    assert_eq!(custom_error(open(&mut base, &alice, 0, 700, 5).await.map(|_| ())), perr(PerpsError::PoolExposureLimit));
    open(&mut base, &alice, 0, 600, 5).await.unwrap();
    // Cancelling restores it
    lp_cancel(&mut base, &seed).await.unwrap();
    open(&mut base, &alice, 0, 600, 5).await.unwrap();
}

#[tokio::test]
async fn the_weekend_cap_binds_inside_the_window() {
    // α = 10%, window 24 h; G derived from the 20% weekend IM: cap = 0.5 × capNAV
    let params = PoolParams { weekend_cap_window_secs: DAY as u32, weekend_lp_loss_bps: 1_000, ..neutral() };
    // (G override, time, $600k of net allowed)
    for (gap, t, ok) in [(0u32, FRIDAY_MORNING, false), (0, FRIDAY_MORNING - DAY, true), (1_000, FRIDAY_MORNING, true)] {
        let mut base = base_with(PoolParams { weekend_gap_bps: gap, ..params }).await;
        configure(&mut base, cme_config()).await.unwrap();
        let alice = trader(&mut base, 200_000 * USDC).await;
        set_time(&mut base, t).await;
        let r = open(&mut base, &alice, 0, 8_000, 5).await;
        if ok {
            r.unwrap();
        } else {
            assert_eq!(custom_error(r.map(|_| ())), perr(PerpsError::PoolWeekendExposureLimit));
            open(&mut base, &alice, 0, 6_000, 5).await.unwrap(); // $450k fits
        }
    }
}

#[tokio::test]
async fn the_per_account_cap_counts_opened_notional() {
    // 1% of a 1× net cap ≈ $10k per account
    let params = PoolParams { k_net_bps: 10_000, adl_net_target_bps: 0, per_account_cap_bps: 100, ..neutral() };
    let mut base = base_with(params).await;
    let alice = trader(&mut base, 10_000 * USDC).await;
    let p = open(&mut base, &alice, 0, 133, 5).await.unwrap(); // $9,975
    assert_eq!(custom_error(open(&mut base, &alice, 0, 1, 5).await.map(|_| ())), perr(PerpsError::AccountExposureLimit));
    // Settling its loss rewrites entry_price, not the house notional (F10, F30)
    set_price(&mut base, 74.0).await;
    let before = user(&mut base, &alice.key()).await.house_entry_notional;
    settle_pnl(&mut base, p).await.unwrap();
    assert_eq!(user(&mut base, &alice.key()).await.house_entry_notional, before);
    assert_eq!(before, 9_975 * USDC);
    assert_eq!(custom_error(open(&mut base, &alice, 0, 1, 5).await.map(|_| ())), perr(PerpsError::AccountExposureLimit));
    // Closing frees the headroom
    close(&mut base, &alice, p).await.unwrap();
    assert_eq!(user(&mut base, &alice.key()).await.house_entry_notional, 0);
    open(&mut base, &alice, 0, 1, 5).await.unwrap();
}

#[tokio::test]
async fn pending_debt_and_drawdown_freeze_net_raising_opens() {
    let params = PoolParams { dd_freeze_bps: 1_000, ..neutral() };
    let mut base = base_with(params).await;
    let alice = trader(&mut base, 10_000 * USDC).await;
    let bob = trader(&mut base, 10_000 * USDC).await;
    let pa = open(&mut base, &alice, 0, 10, 5).await.unwrap();
    patch_pool(&mut base, |p| { p.pending_bad_debt_long = USDC; p.pending_since_long = WEDNESDAY; }).await;
    assert_eq!(custom_error(open(&mut base, &alice, 0, 1, 5).await.map(|_| ())), perr(PerpsError::PoolAdlPending));
    // An open that lowers |net| is not frozen, nor is any close
    open(&mut base, &bob, 1, 5, 5).await.unwrap();
    close(&mut base, &alice, pa).await.unwrap();
    patch_pool(&mut base, |p| { p.pending_bad_debt_long = 0; p.pending_since_long = 0; }).await;
    // A 50% drawdown against a fresh peak
    let nps = pool_view_at(&mut base, 75_000_000).await.nps;
    let t = now(&mut base).await;
    patch_pool(&mut base, |p| { p.nps_peak = nps * 2; p.nps_peak_at = t; }).await;
    assert_eq!(custom_error(open(&mut base, &alice, 0, 20, 5).await.map(|_| ())), perr(PerpsError::PoolDrawdownFreeze));
    // The peak resets after DD_WINDOW_SECS
    set_time(&mut base, t + 7 * DAY).await;
    open(&mut base, &alice, 0, 20, 5).await.unwrap();
}

// ---------------------------------------------------------------------------
// §17.2 #10–#11: closes
// ---------------------------------------------------------------------------

/// Red-team F31: settled losses accumulate into the position's and the
/// user's realized PnL.
#[tokio::test]
async fn realized_pnl_accumulates_settled_losses() {
    let mut base = base_with(neutral()).await;
    let alice = trader(&mut base, 1_000 * USDC).await;
    set_price(&mut base, 100.0).await;
    let p = open(&mut base, &alice, 0, 20, 5).await.unwrap(); // $2,000, $400 margin
    set_price(&mut base, 85.0).await;
    settle_pnl(&mut base, p).await.unwrap();
    let s = position(&mut base, p).await;
    assert_eq!((s.collateral, s.entry_price, s.realized_pnl), (100 * USDC, 85 * USDC, -300 * USDC as i64));
    assert_eq!(custom_error(settle_pnl(&mut base, p).await), perr(PerpsError::NothingToSettle));
    set_price(&mut base, 85.5).await;
    close(&mut base, &alice, p).await.unwrap();
    // −$300 + $10 − the $1 fee on the opened $2,000
    assert_eq!(position(&mut base, p).await.realized_pnl, -291 * USDC as i64);
    assert_eq!(user(&mut base, &alice.key()).await.realized_pnl, -291 * USDC as i64);
    assert_eq!(token_balance(&mut base, alice.tokens).await, 109 * USDC);
    assert_buckets(&mut base).await;
}

/// Red-team F4, F25: an opposite-side winner who closes before any ADL pays
/// the pending bad debt first.
#[tokio::test]
async fn an_opposite_winner_closing_pays_pending_first() {
    let mut base = base_with(neutral()).await;
    let bob = trader(&mut base, 1_000 * USDC).await;
    let p = open(&mut base, &bob, 1, 10, 5).await.unwrap();
    set_price(&mut base, 70.0).await;
    // $30 of bankrupt-long debt is pending
    patch_pool(&mut base, |p| { p.pending_bad_debt_long = 30 * USDC; p.pending_since_long = WEDNESDAY; }).await;
    let cash = pool(&mut base).await.cash;
    close(&mut base, &bob, p).await.unwrap();
    // $150 + ($50 − $30) − $0.375
    assert_eq!(token_balance(&mut base, bob.tokens).await, 169_625_000);
    let pl = pool(&mut base).await;
    assert_eq!((pl.pending_bad_debt_long, pl.pending_since_long, pl.bad_debt_recovered_cum), (0, 0, 30 * USDC));
    assert_eq!(pl.cash, cash - 20 * USDC);
}

// ---------------------------------------------------------------------------
// §17.2 #12–#13: liquidation
// ---------------------------------------------------------------------------

fn staged() -> PoolParams {
    PoolParams {
        backstop_threshold_bps: 6_667,
        liq_penalty_bps: 200,
        liq_keeper_share_bps: 2_000,
        min_liq_slice_bps: 2_000,
        liq_min_remaining_notional: 100 * USDC,
        ..neutral()
    }
}

#[tokio::test]
async fn stage_one_slices_and_the_owner_keeps_the_rest() {
    let mut base = base_with(staged()).await;
    let alice = trader(&mut base, 1_000 * USDC).await;
    let keeper = trader(&mut base, 0).await;
    // 100 @ $75 at 10x ($750); at $71 equity is $350, 4.93% of mark notional
    let p = open(&mut base, &alice, 0, 100, 10).await.unwrap();
    set_price(&mut base, 71.0).await;
    let ins0 = market(&mut base).await.insurance_fund;
    liquidate(&mut base, &keeper, &alice.key(), p).await.unwrap();
    let s = position(&mut base, p).await;
    assert!(s.status == PositionStatus::Open && s.size < 100 * CONTRACT && s.size > 0);
    let x = 100 * CONTRACT - s.size;
    assert!(x >= 20 * CONTRACT);
    // Back above initial margin (within a unit), so a second call is refused
    assert!(s.equity(71 * USDC, 0) * 10_000 + 10_000 >= 1_000 * s.mark_notional(71 * USDC) as i128);
    assert_eq!(custom_error(liquidate(&mut base, &keeper, &alice.key(), p).await), perr(PerpsError::NotLiquidatable));
    // Penalty 2% of the closed notional: 20% to the keeper, 80% to insurance
    let pen = ((x as u128 * 71 * 200 + 9_999) / 10_000) as u64; // ceil
    let reward = token_balance(&mut base, keeper.tokens).await;
    assert_eq!(reward, pen / 5);
    assert_eq!(market(&mut base).await.insurance_fund - ins0, pen - pen / 5);
    assert_eq!(user(&mut base, &alice.key()).await.house_entry_notional, s.opened_notional(s.size));
    assert_conservation(&mut base, 71 * USDC, &[alice.key()], &[p]).await;

    // A small position closes in full at stage 1; the leftover goes to the
    // owner's free collateral, all of which can be withdrawn (F18)
    let bob = trader(&mut base, 100 * USDC).await;
    set_price(&mut base, 75.0).await;
    let q = open(&mut base, &bob, 0, 1, 10).await.unwrap(); // $7.50 margin
    let free = user(&mut base, &bob.key()).await.collateral_balance;
    set_price(&mut base, 71.0).await;
    liquidate(&mut base, &keeper, &bob.key(), q).await.unwrap();
    assert!(position(&mut base, q).await.status == PositionStatus::Liquidated);
    // $3.50 equity − 2% of $71 = $2.08
    let after = user(&mut base, &bob.key()).await.collateral_balance;
    assert_eq!(after - free, 2_080_000);
    withdraw(&mut base, &bob, after).await.unwrap();
    assert_buckets(&mut base).await;
}

/// A gap deficit runs insurance → LP window cap → pending, which freezes LP
/// flows and net-raising opens but not free-collateral withdrawals; deficit
/// ADL then clears it, permissionlessly, once its floor has decayed.
#[tokio::test]
async fn a_gap_deficit_goes_pending_and_deficit_adl_clears_it() {
    // LPs stop pursuing 1 bp of window NAV ($100) per loss window
    let mut base = base_with(PoolParams { lp_loss_cap_window_bps: 1, ..neutral() }).await;
    freeze_funding(&mut base).await;
    let alice = trader(&mut base, 1_000 * USDC).await;
    let bob = trader(&mut base, 2_000 * USDC).await;
    let carol = trader(&mut base, 1_000 * USDC).await;
    let keeper = trader(&mut base, 0).await;
    let pa = open(&mut base, &alice, 0, 100, 10).await.unwrap(); // $750
    let pb = open(&mut base, &bob, 1, 100, 5).await.unwrap(); // $1,500
    // Gap to $60: alice is $750 under water
    set_price(&mut base, 60.0).await;
    liquidate(&mut base, &keeper, &alice.key(), pa).await.unwrap();
    let pl = pool(&mut base).await;
    // The cap is 1 bp of the window-start NAV_redeem ($1M less slack)
    let cap = pl.loss_window_nav / 10_000;
    assert_eq!((pl.bad_debt_cum, pl.lp_absorbed_window, pl.ins_paid_window), (750 * USDC, cap, 0));
    assert_eq!(cap, 100 * USDC - 1);
    let pending = 750 * USDC - cap;
    assert_eq!(pl.pending_bad_debt_long, pending);
    assert!(pl.adl_required());
    assert_eq!(token_balance(&mut base, keeper.tokens).await, 0); // insurance was empty

    // Frozen: LP deposits, LP withdrawals and net-raising opens
    let seed = seed_lp(&base);
    assert_eq!(custom_error(lp_deposit(&mut base, &seed, USDC, 0).await), perr(PerpsError::PoolAdlPending));
    assert_eq!(custom_error(open(&mut base, &carol, 1, 1, 5).await.map(|_| ())), perr(PerpsError::PoolAdlPending));
    // Not frozen: free-collateral withdrawals
    withdraw(&mut base, &carol, 1_000 * USDC).await.unwrap();

    // Exposure mode is keeper-only; deficit mode is anyone's, but a
    // just-booked deficit's floor (100×) is above bob's 2.5× score
    let stranger = funded_keypair(&mut base).await;
    assert_eq!(custom_error(adl(&mut base, Some(&stranger), &bob.key(), pb, 1, 0).await), perr(PerpsError::AdlUnauthorized));
    assert_eq!(custom_error(adl(&mut base, Some(&stranger), &bob.key(), pb, 2, 0).await), perr(PerpsError::InvalidAdlMode));
    assert_eq!(custom_error(adl(&mut base, None, &bob.key(), pb, 0, 0).await), perr(PerpsError::AdlScoreBelowFloor));
    let t = now(&mut base).await;
    // floor(t) = 1e6·(21_600 − t)/21_600 ≤ 25_000 once t ≥ 21_060
    set_time(&mut base, t + 21_059).await;
    assert_eq!(custom_error(adl(&mut base, None, &bob.key(), pb, 0, 0).await), perr(PerpsError::AdlScoreBelowFloor));
    set_time(&mut base, t + 21_060).await;
    let ins = market(&mut base).await.insurance_fund;
    let ix = adl_ix(&base, &base.ctx.payer.pubkey(), &bob.key(), pb, 0, 0);
    let ev: Vec<perps_core::events::Adl> = send_events(&mut base.ctx, &[ix], &[]).await.unwrap();
    // Asked for the whole position, the ADL closes only the slice whose $15
    // a contract covers the pending debt (+4 units of slack, D37)
    let x = ((pending as u128 + 4) * 100 * CONTRACT as u128).div_ceil(1_500 * USDC as u128) as u64;
    assert_eq!((ev[0].size_closed, ev[0].bad_debt_hc), (x, pending));
    // Bob gets the slice's collateral and its profit less the pending debt
    // and the close fee (D22), to free collateral; the fee goes to insurance
    // like any close fee
    let fee = fee_at_75(x);
    let profit = x * 15;
    assert_eq!((ev[0].profit, ev[0].fee), (profit - pending, fee));
    let s = position(&mut base, pb).await;
    assert!(s.status == PositionStatus::Open);
    assert_eq!(s.size, 100 * CONTRACT - x);
    assert_eq!(s.realized_pnl, (profit - pending) as i64 - fee as i64);
    let c_x = (1_500 * USDC as u128 * x as u128 / (100 * CONTRACT) as u128) as u64;
    assert_eq!(user(&mut base, &bob.key()).await.collateral_balance, 500 * USDC + c_x + profit - pending - fee);
    assert_eq!(market(&mut base).await.insurance_fund, ins + fee);
    let pl = pool(&mut base).await;
    assert!(!pl.adl_required());
    assert_eq!((pl.bad_debt_recovered_cum, pl.open_positions), (pending, 1));
    assert_buckets(&mut base).await;
    // LP flows reopen (funding is frozen, so it is always current)
    let l2 = lp(&mut base, 10 * USDC).await;
    lp_deposit(&mut base, &l2, 10 * USDC, 0).await.unwrap();
}

/// Owner decision D22: every deficit-mode ADL slice pays the normal close
/// fee on its opened notional, through `distribute_fee`, after the haircuts.
#[tokio::test]
async fn deficit_adl_slices_pay_the_close_fee() {
    let mut base = base_with(PoolParams { lp_loss_cap_window_bps: 1, ..neutral() }).await;
    freeze_funding(&mut base).await;
    let alice = trader(&mut base, 1_000 * USDC).await;
    let bob = trader(&mut base, 2_000 * USDC).await;
    let keeper = trader(&mut base, 0).await;
    let pa = open(&mut base, &alice, 0, 100, 10).await.unwrap();
    let pb = open(&mut base, &bob, 1, 100, 5).await.unwrap(); // $1,500
    set_price(&mut base, 60.0).await;
    liquidate(&mut base, &keeper, &alice.key(), pa).await.unwrap();
    let pending = pool(&mut base).await.pending_bad_debt_long;
    assert!(pending > 600 * USDC);
    let t = now(&mut base).await;
    set_time(&mut base, t + 21_060).await;

    // A 40-contract slice: $600 of profit all repays pending, so the $1.50
    // fee (5 bps of $3,000 opened) comes out of its $600 of collateral
    let free0 = user(&mut base, &bob.key()).await.collateral_balance;
    let ins0 = market(&mut base).await.insurance_fund;
    let ix = adl_ix(&base, &base.ctx.payer.pubkey(), &bob.key(), pb, 0, 40 * CONTRACT);
    let ev: Vec<perps_core::events::Adl> = send_events(&mut base.ctx, &[ix], &[]).await.unwrap();
    assert_eq!(ev.len(), 1);
    assert_eq!((ev[0].size_closed, ev[0].profit, ev[0].bad_debt_hc, ev[0].fee), (40 * CONTRACT, 0, 600 * USDC, 1_500_000));
    assert_eq!(user(&mut base, &bob.key()).await.collateral_balance, free0 + 600 * USDC - 1_500_000);
    assert_eq!(market(&mut base).await.insurance_fund, ins0 + 1_500_000);
    // Asking for the rest closes only the slice the rest of pending needs
    // (D37); it pays the fee on that slice alone
    let rest = pending - 600 * USDC;
    let x = ((rest as u128 + 4) * 60 * CONTRACT as u128).div_ceil(900 * USDC as u128) as u64;
    let ix = adl_ix(&base, &base.ctx.payer.pubkey(), &bob.key(), pb, 0, 0);
    let ev: Vec<perps_core::events::Adl> = send_events(&mut base.ctx, &[ix], &[]).await.unwrap();
    let fee2 = fee_at_75(x);
    assert_eq!((ev[0].size_closed, ev[0].profit, ev[0].bad_debt_hc, ev[0].fee), (x, x * 15 - rest, rest, fee2));
    let c_x = (900 * USDC as u128 * x as u128 / (60 * CONTRACT) as u128) as u64;
    assert_eq!(user(&mut base, &bob.key()).await.collateral_balance, free0 + 600 * USDC - 1_500_000 + c_x + x * 15 - rest - fee2);
    let s = position(&mut base, pb).await;
    assert!(s.status == PositionStatus::Open);
    assert_eq!(s.size, 60 * CONTRACT - x);
    assert_eq!(s.realized_pnl, (600 * USDC + x * 15 - pending) as i64 - (1_500_000 + fee2) as i64);
    assert_eq!(market(&mut base).await.insurance_fund, ins0 + 1_500_000 + fee2);
    assert!(!pool(&mut base).await.adl_required());
    assert_buckets(&mut base).await;
}

/// Owner decision D23: a winner cannot close for `profit_close_quiet_secs`
/// after a session open, so the keeper liquidates its bankrupt matched
/// counterparty first and the winner then repays the deficit (D13). Losing
/// closes and liquidations are not held; settled markets are exempt.
#[tokio::test]
async fn winners_wait_out_the_profit_close_quiet_time() {
    let params = PoolParams { profit_close_quiet_secs: 300, lp_loss_cap_window_bps: 1, ..neutral() };
    let mut base = base_with(params).await;
    configure(&mut base, cme_config()).await.unwrap();
    freeze_funding(&mut base).await;
    let alice = trader(&mut base, 1_000 * USDC).await;
    let bob = trader(&mut base, 2_000 * USDC).await;
    let carol = trader(&mut base, 1_000 * USDC).await;
    let dave = trader(&mut base, 1_000 * USDC).await;
    let keeper = trader(&mut base, 0).await;
    let pa = open(&mut base, &alice, 0, 100, 10).await.unwrap(); // $750, bankrupt below $67.50
    let pb = open(&mut base, &bob, 1, 100, 5).await.unwrap(); // its matched winner
    let pc = open(&mut base, &carol, 0, 1, 2).await.unwrap(); // a small loser
    let pd = open(&mut base, &dave, 1, 1, 2).await.unwrap(); // a small winner

    // The weekend gaps oil to $60
    set_time(&mut base, SUNDAY_OPEN + 10).await;
    set_price(&mut base, 60.0).await;
    // Bob would be paid in full and the whole deficit left to the LPs
    assert_eq!(custom_error(close(&mut base, &bob, pb).await), perr(PerpsError::ProfitCloseQuiet));
    // Carol's losing close and the keeper's liquidation are not held
    close(&mut base, &carol, pc).await.unwrap();
    liquidate(&mut base, &keeper, &alice.key(), pa).await.unwrap();
    let pending = pool(&mut base).await.pending_bad_debt_long;
    assert!(pending > 600 * USDC);
    set_time(&mut base, SUNDAY_OPEN + 299).await;
    assert_eq!(custom_error(close(&mut base, &bob, pb).await), perr(PerpsError::ProfitCloseQuiet));
    // Once it is over, Bob's profit repays the pending deficit first
    set_time(&mut base, SUNDAY_OPEN + 300).await;
    let tokens = token_balance(&mut base, bob.tokens).await;
    close(&mut base, &bob, pb).await.unwrap();
    // $1,500 + ($1,500 − pending) − 5 bps of $7,500
    assert_eq!(token_balance(&mut base, bob.tokens).await - tokens, 3_000 * USDC - pending - 3_750_000);
    let pl = pool(&mut base).await;
    assert_eq!((pl.pending_bad_debt_long, pl.bad_debt_recovered_cum), (0, pending));

    // The daily 18:00 reopen starts a quiet time too
    set_time(&mut base, SUNDAY_OPEN + DAY + 10).await;
    assert_eq!(custom_error(close(&mut base, &dave, pd).await), perr(PerpsError::ProfitCloseQuiet));
    // Settlement starts a quiet time of its own (D35): the losers it makes
    // bankrupt are closed and booked before any winner is paid
    let t = SUNDAY_OPEN + DAY + 400;
    set_time(&mut base, t).await;
    settle(&mut base, 60 * USDC).await.unwrap();
    assert_eq!(market(&mut base).await.settled_at, t);
    set_time(&mut base, t + 299).await;
    assert_eq!(custom_error(close(&mut base, &dave, pd).await), perr(PerpsError::ProfitCloseQuiet));
    set_time(&mut base, t + 300).await;
    close(&mut base, &dave, pd).await.unwrap();
    assert!(position(&mut base, pd).await.status == PositionStatus::Closed);
    assert_buckets(&mut base).await;
}

#[tokio::test]
async fn adl_rejects_the_wrong_candidates() {
    let mut base = base_with(neutral()).await;
    let alice = trader(&mut base, 2_000 * USDC).await;
    let bob = trader(&mut base, 2_000 * USDC).await;
    let pa = open(&mut base, &alice, 0, 10, 5).await.unwrap();
    let pb = open(&mut base, &bob, 1, 10, 5).await.unwrap();
    let admin = base.admin.insecure_clone();
    set_price(&mut base, 76.0).await;
    // Nothing pending, no trigger
    assert_eq!(custom_error(adl(&mut base, None, &alice.key(), pa, 0, 0).await), perr(PerpsError::AdlNotRequired));
    assert_eq!(custom_error(adl(&mut base, Some(&admin), &alice.key(), pa, 1, 0).await), perr(PerpsError::AdlNotRequired));
    // Bob is losing
    assert_eq!(custom_error(adl(&mut base, None, &bob.key(), pb, 0, 0).await), perr(PerpsError::AdlCandidateNotProfitable));
    // Pending on the long side is repaid by shorts, not by long alice
    patch_pool(&mut base, |p| { p.pending_bad_debt_long = USDC; p.pending_since_long = 1; }).await;
    assert_eq!(custom_error(adl(&mut base, None, &alice.key(), pa, 0, 0).await), perr(PerpsError::AdlWrongSide));
    assert_eq!(custom_error(adl(&mut base, None, &alice.key(), pa, 0, 11 * CONTRACT).await), perr(PerpsError::InvalidPositionReduction));
    // Settled: deficit mode only
    settle(&mut base, 76 * USDC).await.unwrap();
    assert_eq!(custom_error(adl(&mut base, Some(&admin), &alice.key(), pa, 1, 0).await), perr(PerpsError::MarketSettled));
    patch_pool(&mut base, |p| { p.pending_bad_debt_long = 0; p.pending_bad_debt_short = USDC; }).await;
    adl(&mut base, None, &alice.key(), pa, 0, 0).await.unwrap();
    assert_eq!(pool(&mut base).await.pending_bad_debt_short, 0);
}

/// Exposure ADL on the hard-net trigger stops at its target.
#[tokio::test]
async fn exposure_adl_stops_at_the_net_target() {
    let params = PoolParams { k_net_bps: 10_000, hard_net_bps: 10_000, adl_net_target_bps: 5_000, ..neutral() };
    let mut base = base_with(params).await;
    let alice = trader(&mut base, 200_000 * USDC).await;
    let pa = open(&mut base, &alice, 0, 13_000, 10).await.unwrap(); // $975k of net
    let admin = base.admin.insecure_clone();
    set_price(&mut base, 75.1).await;
    assert_eq!(custom_error(adl(&mut base, Some(&admin), &alice.key(), pa, 1, 0).await), perr(PerpsError::AdlNotRequired));
    // LPs lose $100k: B ≈ $899k < net notional, so nTrig holds; target 1 × B
    patch_pool(&mut base, |p| p.cash -= 100_000 * USDC).await;
    let v = pool_view_at(&mut base, usd(75.1)).await;
    let target = v.b; // k_net = 1×
    // Net may fall to the target and no further
    let max_x = ((v.net_notional - target) * 1_000_000 / usd(75.1) as u128) as u64;
    assert_eq!(custom_error(adl(&mut base, Some(&admin), &alice.key(), pa, 1, max_x + 2 * CONTRACT).await), perr(PerpsError::AdlOvershoot));
    let free0 = user(&mut base, &alice.key()).await.collateral_balance;
    let c0 = position(&mut base, pa).await.collateral;
    let ins0 = market(&mut base).await.insurance_fund;
    let ix = adl_ix(&base, &admin.pubkey(), &alice.key(), pa, 1, max_x - CONTRACT);
    let ev: Vec<perps_core::events::Adl> = send_events(&mut base.ctx, &[ix], &[&admin]).await.unwrap();
    let s = position(&mut base, pa).await;
    assert_eq!(s.size, 13_000 * CONTRACT - (max_x - CONTRACT));
    assert!(s.status == PositionStatus::Open);
    // Exposure mode charges no close fee (D22): the owner gets C_x + profit
    assert_eq!((ev.len(), ev[0].fee), (1, 0));
    assert!(ev[0].profit > 0);
    let credited = user(&mut base, &alice.key()).await.collateral_balance - free0;
    assert_eq!(credited, (c0 - s.collateral) + ev[0].profit);
    assert_eq!(market(&mut base).await.insurance_fund, ins0);
    // The ADL keeper key can run it too, a stranger cannot
    let k = funded_keypair(&mut base).await;
    set_adl_keeper(&mut base, &admin, k.pubkey()).await.unwrap();
    adl(&mut base, Some(&k), &alice.key(), pa, 1, CONTRACT / 2).await.unwrap();
}

/// §7.4, red-team F27: the write-off clock counts open seconds.
#[tokio::test]
async fn bad_debt_is_written_off_after_a_day_of_session() {
    let mut base = base_with(neutral()).await;
    configure(&mut base, cme_config()).await.unwrap();
    assert_eq!(custom_error(write_off(&mut base).await), perr(PerpsError::AdlNotRequired));
    set_time(&mut base, FRIDAY_NOON).await;
    patch_pool(&mut base, |p| { p.pending_bad_debt_long = 100 * USDC; p.pending_since_long = FRIDAY_NOON; }).await;
    // Saturday: only 5 open hours have passed
    set_time(&mut base, SATURDAY).await;
    assert_eq!(custom_error(write_off(&mut base).await), perr(PerpsError::BadDebtNotStale));
    // 5 h on Friday + 19 h from the Sunday open
    set_time(&mut base, SUNDAY_OPEN + 19 * HOUR - 1).await;
    assert_eq!(custom_error(write_off(&mut base).await), perr(PerpsError::BadDebtNotStale));
    set_time(&mut base, SUNDAY_OPEN + 19 * HOUR).await;
    write_off(&mut base).await.unwrap();
    let pl = pool(&mut base).await;
    assert_eq!((pl.pending_bad_debt_long, pl.pending_since_long, pl.bad_debt_written_off_cum), (0, 0, 100 * USDC));
}

// ---------------------------------------------------------------------------
// §17.2 #15: funding
// ---------------------------------------------------------------------------

#[tokio::test]
async fn missed_funding_periods_are_caught_up_once() {
    let mut base = base_with(neutral()).await;
    let alice = trader(&mut base, 10_000 * USDC).await;
    let p = open(&mut base, &alice, 0, 100, 5).await.unwrap();
    assert_eq!(custom_error(update_funding(&mut base).await), perr(PerpsError::FundingNotDue));
    // 3 periods and 100 s: charged in one crank, the 100 s carried
    set_time(&mut base, WEDNESDAY + 3 * HOUR + 100).await;
    update_funding(&mut base).await.unwrap();
    let m = market(&mut base).await;
    assert_eq!((m.cumulative_funding, m.funding_carry_secs), (3 * 7_500, 100));
    set_time(&mut base, WEDNESDAY + 4 * HOUR - 1).await;
    assert_eq!(custom_error(update_funding(&mut base).await), perr(PerpsError::FundingNotDue));
    set_time(&mut base, WEDNESDAY + 4 * HOUR).await;
    update_funding(&mut base).await.unwrap();
    let m = market(&mut base).await;
    assert_eq!((m.cumulative_funding, m.funding_carry_secs), (4 * 7_500, 0));
    // The pool's carry lands in cash at the close: $3 of funding
    let cash = pool(&mut base).await.cash;
    close(&mut base, &alice, p).await.unwrap();
    assert_eq!(pool(&mut base).await.cash, cash + 3 * USDC);
}

#[tokio::test]
async fn the_weekend_accrues_no_funding() {
    let mut base = base_with(neutral()).await;
    configure(&mut base, cme_config()).await.unwrap();
    let alice = trader(&mut base, 10_000 * USDC).await;
    // Wed 12:00 → Fri 16:00 ET is 50 open hours (two daily breaks): no carry
    set_time(&mut base, FRIDAY_CLOSE - HOUR).await;
    update_funding(&mut base).await.unwrap();
    assert_eq!(market(&mut base).await.funding_carry_secs, 0);
    open(&mut base, &alice, 0, 100, 5).await.unwrap();
    let f0 = market(&mut base).await.cumulative_funding;
    set_time(&mut base, FRIDAY_CLOSE - 1).await;
    assert_eq!(custom_error(update_funding(&mut base).await), perr(PerpsError::FundingNotDue));
    // 49 closed hours later, one period is due, not 50
    set_time(&mut base, SUNDAY_OPEN - 1).await;
    assert_eq!(custom_error(update_funding(&mut base).await), perr(PerpsError::MarketClosed));
    set_time(&mut base, SUNDAY_OPEN).await;
    update_funding(&mut base).await.unwrap();
    let m = market(&mut base).await;
    assert_eq!((m.cumulative_funding - f0, m.funding_carry_secs), (7_500, 0));
}

// ---------------------------------------------------------------------------
// §17.2 #19: the bucket identity after every action (200 random actions
// across trading, liquidation, settle, ADL, write-off, LP flows, sweeps and
// donations; the harness also checks it after every transaction)
// ---------------------------------------------------------------------------

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// The per-transaction watcher bites: value written into a bucket without
/// tokens is caught by the next instruction.
#[tokio::test]
#[should_panic(expected = "§11")]
async fn the_bucket_watcher_catches_an_unbacked_bucket() {
    let mut base = base_with(neutral()).await;
    let alice = trader(&mut base, 1_000 * USDC).await;
    // Raw write, bypassing `patch` (which re-baselines)
    let address = base.lp_pool;
    let mut acct = raw(&mut base, address).await.unwrap();
    let mut p: LpPool = anchor_lang::AccountDeserialize::try_deserialize(&mut acct.data.as_slice()).unwrap();
    p.cash += 1;
    let mut data = Vec::new();
    anchor_lang::AccountSerialize::try_serialize(&p, &mut data).unwrap();
    acct.data[..data.len()].copy_from_slice(&data);
    base.ctx.set_account(&address, &AccountSharedData::from(acct));
    open(&mut base, &alice, 0, 1, 5).await.unwrap();
}

#[tokio::test]
async fn bucket_identity_property() {
    // §17.2 #19 across all instructions: the relay venue (venue/mod.rs) is
    // loaded so venue_fill runs too
    let mut pt = ProgramTest::default();
    pt.prefer_bpf(false);
    pt.add_program("relay", crate::venue::RELAY, solana_program_test::processor!(crate::venue::relay));
    pt.prefer_bpf(is_sbf());
    let params = PoolParams { skew_scale: 2_500_000_000_000, max_impact_bps: 100, open_fee_bps: 5, treasury_fee_bps: 1_000, insurance_target: 50 * USDC, ..staged() };
    let mut base = start_with(pt, params).await;
    let venue = register_venue(&mut base, &crate::venue::RELAY, default_venue_params()).await;
    let admin = base.admin.insecure_clone();
    let treasury_dest = token_account(&mut base, &admin.pubkey(), 0).await;
    let keeper = trader(&mut base, 0).await;
    let mut traders = Vec::new();
    for _ in 0..4 {
        let tr = trader(&mut base, 5_000 * USDC).await;
        let t0 = now(&mut base).await;
        for role in [ROLE_TAKER, ROLE_MAKER] {
            approve(&mut base, &tr.kp, &venue, role, approval_params(1_000_000 * USDC, t0 + 20 * DAY)).await.unwrap();
        }
        traders.push(tr);
    }
    let (mut fills, mut nonce) = (0u32, 0u64);
    let mut positions: Vec<(usize, Pubkey)> = Vec::new();
    let mut rng = Rng(0x1234_5678_9abc_def1);
    let mut price = 75.0f64;
    let mut t = WEDNESDAY;
    let owners: Vec<Pubkey> = traders.iter().map(|x| x.key()).collect();
    let lp_wallet = lp(&mut base, 50_000 * USDC).await;
    let donor = funded_keypair(&mut base).await;
    let donor_tokens = token_account(&mut base, &donor.pubkey(), 10_000 * USDC).await;
    for _ in 0..200 {
        let who = rng.below(traders.len() as u64) as usize;
        let tr = &traders[who];
        match rng.below(13) {
            0 => {
                let _ = deposit(&mut base, tr, rng.below(500) * USDC).await;
            }
            1 => {
                let free = user(&mut base, &tr.key()).await.collateral_balance;
                let _ = withdraw(&mut base, tr, rng.below(free + 1)).await;
            }
            2 | 3 => {
                let size = 1 + rng.below(50) * CONTRACT;
                if let Ok(p) = open_with(&mut base, tr, rng.below(2) as u8, size, 1 + rng.below(10) as u32, 0).await {
                    positions.push((who, p));
                }
            }
            4 => {
                if let Some(&(o, p)) = positions.get(rng.below(positions.len().max(1) as u64) as usize) {
                    let _ = close(&mut base, &traders[o], p).await;
                }
            }
            5 => {
                if let Some(&(o, p)) = positions.get(rng.below(positions.len().max(1) as u64) as usize) {
                    let owner = traders[o].key();
                    let _ = liquidate(&mut base, &keeper, &owner, p).await;
                    let _ = settle_pnl(&mut base, p).await;
                }
            }
            6 => {
                if let Some(&(o, p)) = positions.get(rng.below(positions.len().max(1) as u64) as usize) {
                    let _ = add_margin(&mut base, &traders[o], p, rng.below(20) * USDC).await;
                }
            }
            7 => {
                price = (price * (0.97 + rng.below(61) as f64 / 1_000.0)).max(40.0);
                set_price(&mut base, price).await;
            }
            8 => {
                // A venue fill between two traders at the oracle ±0.2%
                nonce += 1;
                let maker_i = (who + 1 + rng.below(3) as usize) % traders.len();
                let (taker, maker) = (traders[who].key(), traders[maker_i].key());
                let fa = fill_accounts(&base, &crate::venue::RELAY, &taker, &maker, nonce);
                let fill_price = usd(price * (0.998 + rng.below(5) as f64 / 1_000.0));
                let params = perps_core::instructions::VenueFillParams {
                    size: (1 + rng.below(50)) * CONTRACT,
                    price: fill_price,
                    taker_side: rng.below(2) as u8,
                    taker_leverage: 5_000,
                    maker_leverage: 5_000,
                    position_nonce: nonce,
                };
                if crate::venue::relay_fill(&mut base, &crate::venue::RELAY, &fa, params, &[]).await.is_ok() {
                    fills += 1;
                    positions.push((who, fa.taker_position));
                    positions.push((maker_i, fa.maker_position));
                }
            }
            9 => {
                // LP flows need funding current: crank in the same transaction
                let i = lp_deposit_ix(&base, &lp_wallet, 1 + rng.below(1_000) * USDC, 0);
                let f = funding_ix(&base);
                let _ = send(&mut base.ctx, &[f, i.clone()], &[&lp_wallet.kp]).await;
                let _ = send(&mut base.ctx, &[i], &[&lp_wallet.kp]).await;
            }
            10 => {
                let pos = lp_position(&mut base, &lp_wallet).await;
                if pos.pending_shares > 0 {
                    let i = lp_withdraw_ix(&base, &lp_wallet);
                    let f = funding_ix(&base);
                    if send(&mut base.ctx, &[f, i.clone()], &[&lp_wallet.kp]).await.is_err() {
                        let _ = send(&mut base.ctx, &[i], &[&lp_wallet.kp]).await;
                    }
                    let _ = lp_cancel(&mut base, &lp_wallet).await;
                } else if pos.shares > 0 {
                    let _ = lp_request(&mut base, &lp_wallet, 1 + rng.below(pos.shares)).await;
                }
            }
            11 => {
                let _ = sweep(&mut base).await;
                let _ = donate(&mut base, &donor, donor_tokens, rng.below(50) * USDC).await;
                let treasury = pool(&mut base).await.treasury;
                let _ = withdraw_treasury(&mut base, &admin, treasury_dest, rng.below(treasury + 1)).await;
            }
            12 => {
                if let Some(&(o, p)) = positions.get(rng.below(positions.len().max(1) as u64) as usize) {
                    let owner = traders[o].key();
                    let _ = adl(&mut base, None, &owner, p, 0, 0).await;
                    let _ = adl(&mut base, Some(&admin), &owner, p, 1, 0).await;
                }
                let _ = write_off(&mut base).await;
            }
            _ => {
                t += 1_800 + rng.below(3_600) as i64;
                set_time(&mut base, t).await;
                set_price(&mut base, price).await;
                let _ = update_funding(&mut base).await;
            }
        }
        assert_buckets(&mut base).await;
    }
    let all: Vec<Pubkey> = positions.iter().map(|x| x.1).collect();
    assert_conservation(&mut base, usd(price), &owners, &all).await;
    // The harness checked §11 and the §1.2 sums after every transaction too
    let lpp = lp_position(&mut base, &lp_wallet).await;
    assert!(lpp.deposited_cum > 0 && lpp.withdrawn_cum > 0, "LP flows ran: {:?}", (lpp.deposited_cum, lpp.withdrawn_cum));
    assert!(fills > 0, "venue fills ran");

    // Settle, close every position with close_settled_position (losers
    // first: winners wait for cash), sweep the whole fund, and let every LP out
    settle(&mut base, usd(price)).await.unwrap();
    t += DAY;
    set_time(&mut base, t).await;
    let mut settled_closes = 0;
    for _ in 0..3 {
        for &(o, p) in &positions {
            if position(&mut base, p).await.status == PositionStatus::Open && close_settled(&mut base, &traders[o].key(), p).await.is_ok() {
                settled_closes += 1;
            }
        }
    }
    let pl = pool(&mut base).await;
    assert!(settled_closes > 0);
    assert_eq!((pl.open_positions, pl.position_collateral, pl.long_entry_sum, pl.short_entry_sum), (0, 0, 0, 0));
    if market(&mut base).await.insurance_fund > 0 {
        sweep(&mut base).await.unwrap();
    }
    assert_eq!(market(&mut base).await.insurance_fund, 0);
    t += DAY; // what was swept has vested
    set_time(&mut base, t).await;
    let seed = seed_lp(&base);
    for l in [&lp_wallet, &seed] {
        let pos = lp_position(&mut base, l).await;
        if pos.pending_shares > 0 {
            lp_cancel(&mut base, l).await.unwrap();
        }
        let shares = lp_position(&mut base, l).await.shares;
        if shares > 0 {
            lp_request(&mut base, l, shares).await.unwrap();
            lp_withdraw(&mut base, l).await.unwrap();
        }
    }
    let pl = pool(&mut base).await;
    assert_eq!(pl.lp_shares, perps_core::state::LP_DEAD_SHARES);
    assert_buckets(&mut base).await;
    let (checks, sums) = buckets_watched(&base.lp_pool);
    assert!(checks >= 50 && sums, "{} checks, sums on: {}", checks, sums);
    assert_eq!(pool(&mut base).await.free_collateral, {
        let mut s = 0;
        for o in &owners {
            s += user(&mut base, o).await.collateral_balance;
        }
        s + user(&mut base, &keeper.key()).await.collateral_balance
    });
}

// ---------------------------------------------------------------------------
// §14, §17.3: compute, asserted against the SBF binary
// ---------------------------------------------------------------------------

#[tokio::test]
async fn trading_fits_the_compute_budget() {
    let mut base = base_with(PoolParams { skew_scale: 2_500_000_000_000, max_impact_bps: 100, ..staged() }).await;
    configure(&mut base, cme_config()).await.unwrap();
    let alice = trader(&mut base, 10_000 * USDC).await;
    let bob = trader(&mut base, 10_000 * USDC).await;
    let keeper = trader(&mut base, 0).await;
    let (open_i, pa) = open_ix(&mut base, &alice.key(), 0, 100 * CONTRACT, 10_000).await;
    let (r, open_units) = simulate(&mut base.ctx, &[open_i.clone()], &[&alice.kp]).await;
    r.unwrap();
    send(&mut base.ctx, &[open_i], &[&alice.kp]).await.unwrap();
    let pb = open(&mut base, &bob, 1, 100, 5).await.unwrap();

    set_time(&mut base, WEDNESDAY + 4 * HOUR).await;
    let accounts = perps_core::accounts::UpdateFunding { keeper: base.ctx.payer.pubkey(), market: base.market, pyth_price_feed: base.oracle };
    let fund_i = ix(perps_core::ID, accounts, perps_core::instruction::UpdateFunding {});
    let (r, funding_units) = simulate(&mut base.ctx, &[fund_i], &[]).await;
    r.unwrap();
    update_funding(&mut base).await.unwrap();

    let close = close_ix(&base, &bob, pb);
    let (r, close_units) = simulate(&mut base.ctx, &[close], &[&bob.kp]).await;
    r.unwrap();

    // Stage 1 at $71: a partial slice
    set_price(&mut base, 71.0).await;
    let liq = liquidate_ix(&base, &keeper, &alice.key(), pa);
    let (r, liq_units) = simulate(&mut base.ctx, &[liq], &[&keeper.kp]).await;
    r.unwrap();

    // Deficit ADL of bob's winning short
    patch_pool(&mut base, |p| { p.pending_bad_debt_long = USDC; p.pending_since_long = 1; }).await;
    let payer = base.ctx.payer.pubkey();
    let a = adl_ix(&base, &payer, &bob.key(), pb, 0, 0);
    let (r, adl_units) = simulate(&mut base.ctx, &[a], &[]).await;
    r.unwrap();

    println!("open {} close {} liquidate {} update_funding {} adl {} CU", open_units, close_units, liq_units, funding_units, adl_units);
    if is_sbf() {
        assert!(open_units < 120_000, "open_position used {} CU", open_units);
        assert!(close_units < 150_000, "close_position used {} CU", close_units);
        assert!(liq_units < 150_000, "liquidate used {} CU", liq_units);
        assert!(funding_units < 60_000, "update_funding used {} CU", funding_units);
        assert!(adl_units < 100_000, "adl used {} CU", adl_units);
    }
}

// ---------------------------------------------------------------------------
// §17.2 completion: LP pricing with open positions, fees, closes, the
// waterfall, ADL triggers, the write-off clock and funding catch-up
// ---------------------------------------------------------------------------

/// §9.1, §9.2: with unhedged positions open, deposits mint at NAV_mint (the
/// traders' whole loss counted) and requests are valued at NAV_redeem (only
/// h = 50% of it), so a round trip cannot gain.
#[tokio::test]
async fn lp_flows_price_at_nav_mint_and_nav_redeem() {
    let mut base = base_with(neutral()).await;
    let alice = trader(&mut base, 20_000 * USDC).await;
    open(&mut base, &alice, 0, 1_000, 5).await.unwrap();
    set_price(&mut base, 74.0).await;
    // Traders down $1,000; slack 2·1 + 1
    let v = pool_view_at(&mut base, 74 * USDC).await;
    assert_eq!(v.u, -1_000 * USDC as i128);
    assert_eq!(v.nav_mint, (LP_SEED + 1_000 * USDC + 3) as i128);
    assert_eq!(v.nav_redeem, (LP_SEED + 500 * USDC - 3) as i128);

    let l2 = lp(&mut base, 1_000 * USDC).await;
    let shares = (1_000 * USDC as u128 * LP_SEED as u128 / v.nav_mint as u128) as u64;
    lp_deposit(&mut base, &l2, 1_000 * USDC, shares).await.unwrap();
    assert_eq!(lp_position(&mut base, &l2).await.shares, shares);
    lp_request(&mut base, &l2, shares).await.unwrap();
    let pl = pool(&mut base).await;
    let redeem = pool_view_at(&mut base, 74 * USDC).await.nav_redeem;
    assert_eq!(redeem, (pl.cash + 500 * USDC - 3) as i128);
    let value = (shares as u128 * redeem as u128 / pl.lp_shares as u128) as u64;
    assert_eq!(lp_position(&mut base, &l2).await.requested_value, value);
    assert!(value < 1_000 * USDC);
    lp_withdraw(&mut base, &l2).await.unwrap();
    assert_eq!(token_balance(&mut base, l2.tokens).await, value);
}

/// §2.5, §17.2 #16: real fees split into treasury, insurance up to its
/// target, and LP cash; the treasury is withdrawn by the admin.
#[tokio::test]
async fn fees_split_into_treasury_insurance_and_cash() {
    let params = PoolParams { open_fee_bps: 5, treasury_fee_bps: 1_000, insurance_target: 10 * USDC, ..neutral() };
    let mut base = base_with(params).await;
    let alice = trader(&mut base, 20_000 * USDC).await;
    // Open fee: 5 bps of $75,000 = $37.50 → $3.75 treasury, $10 insurance, $23.75 cash
    let p = open(&mut base, &alice, 0, 1_000, 5).await.unwrap();
    let pl = pool(&mut base).await;
    assert_eq!((pl.treasury, pl.cash), (3_750_000, LP_SEED + 23_750_000));
    assert_eq!(market(&mut base).await.insurance_fund, 10 * USDC);
    // Close fee: the 5 bps taker fee on the opened $75,000; insurance is full
    close(&mut base, &alice, p).await.unwrap();
    let pl = pool(&mut base).await;
    assert_eq!((pl.treasury, pl.cash), (7_500_000, LP_SEED + 57_500_000));
    assert_eq!(market(&mut base).await.insurance_fund, 10 * USDC);
    assert_eq!(token_balance(&mut base, alice.tokens).await, 15_000 * USDC - 37_500_000);
    // At its target there is nothing to sweep
    assert_eq!(custom_error(sweep(&mut base).await), perr(PerpsError::NothingToSweep));
    let admin = base.admin.insecure_clone();
    let dest = token_account(&mut base, &admin.pubkey(), 0).await;
    withdraw_treasury(&mut base, &admin, dest, 7_500_000).await.unwrap();
    assert_eq!((token_balance(&mut base, dest).await, pool(&mut base).await.treasury), (7_500_000, 0));
}

/// §2.8, §17.2 #10: a close is haircut pro rata when traders' gains U exceed
/// LP cash.
#[tokio::test]
async fn closes_take_a_solvency_haircut_when_gains_exceed_cash() {
    let mut base = base_with(neutral()).await;
    let alice = trader(&mut base, 1_000 * USDC).await;
    let p = open(&mut base, &alice, 0, 10, 5).await.unwrap(); // $150 margin
    // LP cash falls to $20 (as if LPs had lost it)
    patch_pool(&mut base, |p| p.cash = 20 * USDC).await;
    set_price(&mut base, 80.0).await;
    // U = $50 > cash: the profit is paid at 20/50
    let ix = close_ix(&base, &alice, p);
    let ev: Vec<perps_core::events::PositionClosed> = send_events(&mut base.ctx, &[ix], &[&alice.kp]).await.unwrap();
    assert_eq!(ev.len(), 1);
    assert_eq!((ev[0].solvency_hc, ev[0].bad_debt_hc, ev[0].fee), (30 * USDC, 0, 375_000));
    assert_eq!(token_balance(&mut base, alice.tokens).await, 150 * USDC + 20 * USDC - 375_000);
    assert_eq!(pool(&mut base).await.cash, 0);
    assert_eq!(position(&mut base, p).await.realized_pnl, 20 * USDC as i64 - 375_000);
}

/// §6.4, §17.2 #13: a stage-2 residual fills insurance to its target and
/// overflows to LP cash.
#[tokio::test]
async fn stage_two_residual_fills_insurance_then_cash() {
    let mut base = base_with(PoolParams { insurance_target: USDC, ..neutral() }).await;
    let alice = trader(&mut base, 1_000 * USDC).await;
    let keeper = trader(&mut base, 0).await;
    let p = open(&mut base, &alice, 0, 100, 10).await.unwrap(); // $750
    // At $71 equity is $350 < 5% of $7,100
    set_price(&mut base, 71.0).await;
    let ix = liquidate_ix(&base, &keeper, &alice.key(), p);
    let ev: Vec<perps_core::events::PositionLiquidated> = send_events(&mut base.ctx, &[ix], &[&keeper.kp]).await.unwrap();
    assert_eq!((ev[0].stage, ev[0].size_closed, ev[0].deficit), (2, 100 * CONTRACT, 0));
    // Reward: 1% of the 5% MM on the opened $7,500 = $3.75; $346.25 left,
    // $1 of it to insurance, the rest and the $400 loss to cash
    assert_eq!(token_balance(&mut base, keeper.tokens).await, 3_750_000);
    assert_eq!(market(&mut base).await.insurance_fund, USDC);
    assert_eq!(pool(&mut base).await.cash, LP_SEED + 400 * USDC + 345_250_000);
    assert!(position(&mut base, p).await.status == PositionStatus::Liquidated);
}

/// Red-team F2, F4/F25 (§17.2 #11, #13): thirty bankrupt positions in one
/// loss window share one LP cap; the rest is pending, and an opposite
/// winner who closes before any ADL repays it from its profit.
#[tokio::test]
async fn bankrupt_positions_share_one_window_cap_and_a_winner_repays_pending() {
    // LPs absorb at most 1 bp of the window-start NAV (≈ $100)
    let mut base = base_with(PoolParams { lp_loss_cap_window_bps: 1, ..neutral() }).await;
    let keeper = trader(&mut base, 0).await;
    let bob = trader(&mut base, 3_000 * USDC).await;
    let pb = open(&mut base, &bob, 1, 300, 10).await.unwrap(); // $2,250
    let mut longs = Vec::new();
    for _ in 0..30 {
        let t = trader(&mut base, 100 * USDC).await;
        let p = open(&mut base, &t, 0, 10, 10).await.unwrap(); // $75 each
        longs.push((t, p));
    }
    // Gap to $60: each long is $75 under water; matched book, so U = 0
    set_price(&mut base, 60.0).await;
    for (t, p) in &longs {
        liquidate(&mut base, &keeper, &t.key(), *p).await.unwrap();
    }
    let pl = pool(&mut base).await;
    let cap = pl.loss_window_nav / 10_000;
    assert_eq!(pl.loss_window_nav, LP_SEED - 63); // NAV_redeem, slack 2·31 + 1
    assert_eq!((pl.bad_debt_cum, pl.lp_absorbed_window, pl.ins_paid_window), (2_250 * USDC, cap, 0));
    let pending = 2_250 * USDC - cap;
    assert_eq!(pl.pending_bad_debt_long, pending);
    assert!(pl.adl_required());

    // Bob's $4,500 profit repays the long side's pending first
    let ix = close_ix(&base, &bob, pb);
    let ev: Vec<perps_core::events::PositionClosed> = send_events(&mut base.ctx, &[ix], &[&bob.kp]).await.unwrap();
    assert_eq!((ev[0].bad_debt_hc, ev[0].solvency_hc), (pending, 0));
    assert_eq!(token_balance(&mut base, bob.tokens).await, 2_250 * USDC + 4_500 * USDC - pending - 11_250_000);
    let pl = pool(&mut base).await;
    assert_eq!((pl.pending_bad_debt_long, pl.pending_since_long, pl.bad_debt_recovered_cum), (0, 0, pending));
    assert!(!pl.adl_required());
    assert_eq!(pl.open_positions, 0);
}

/// Red-team F21 (§17.2 #13): a liquidation whose realized profit the pool
/// cannot pay still goes through, the shortfall reported as `cash_hc`.
#[tokio::test]
async fn liquidation_never_reverts_on_pool_cash() {
    let mut base = base_with(neutral()).await;
    let alice = trader(&mut base, 1_000 * USDC).await;
    let carol = trader(&mut base, 2_000 * USDC).await;
    let keeper = trader(&mut base, 0).await;
    let p = open(&mut base, &alice, 0, 100, 10).await.unwrap(); // $750
    open(&mut base, &carol, 0, 100, 5).await.unwrap();
    // Alice settles a $500 loss at $70 ($250 left, entry $70)
    set_price(&mut base, 70.0).await;
    settle_pnl(&mut base, p).await.unwrap();
    // At $70.50 she is $50 up since the settle, but $300 < 5% of $7,050;
    // carol's $450 loss keeps U < 0, so no solvency haircut. LP cash is gone.
    set_price(&mut base, 70.5).await;
    patch_pool(&mut base, |p| p.cash = 0).await;
    let ix = liquidate_ix(&base, &keeper, &alice.key(), p);
    let ev: Vec<perps_core::events::PositionLiquidated> = send_events(&mut base.ctx, &[ix], &[&keeper.kp]).await.unwrap();
    assert_eq!((ev[0].stage, ev[0].cash_hc, ev[0].solvency_hc, ev[0].deficit), (2, 50 * USDC, 0, 0));
    assert_eq!(ev[0].keeper_reward, 3_750_000);
    assert!(position(&mut base, p).await.status == PositionStatus::Liquidated);
    // $250 − $3.75 to insurance (neutral target), nothing from cash
    assert_eq!(pool(&mut base).await.cash, 0);
    assert_eq!(market(&mut base).await.insurance_fund, 246_250_000);
}

/// §7.2 dTrig: a drawdown past `dd_adl_bps` makes exposure ADL eligible,
/// down to `adl_net_target_bps` of B and no further.
#[tokio::test]
async fn drawdown_exposure_adl_stops_at_its_target() {
    let params = PoolParams { k_net_bps: 10_000, dd_adl_bps: 1_500, adl_net_target_bps: 5_000, ..neutral() };
    let mut base = base_with(params).await;
    let admin = base.admin.insecure_clone();
    let alice = trader(&mut base, 80_000 * USDC).await;
    let pa = open(&mut base, &alice, 0, 10_000, 10).await.unwrap(); // $750k, peak nps = 1.0
    set_price(&mut base, 76.0).await;
    assert_eq!(custom_error(adl(&mut base, Some(&admin), &alice.key(), pa, 1, 0).await), perr(PerpsError::AdlNotRequired));
    // LPs lose $150k elsewhere: mark NAV $840k, a 16% drawdown
    patch_pool(&mut base, |p| p.cash -= 150_000 * USDC).await;
    let v = pool_view_at(&mut base, 76 * USDC).await;
    let target = v.b * 5_000 / 10_000;
    let max_x = ((v.net_notional - target) * 1_000_000 / (76 * USDC) as u128) as u64;
    assert_eq!(custom_error(adl(&mut base, Some(&admin), &alice.key(), pa, 1, max_x + 2 * CONTRACT).await), perr(PerpsError::AdlOvershoot));
    let ix = adl_ix(&base, &admin.pubkey(), &alice.key(), pa, 1, max_x - CONTRACT);
    let ev: Vec<perps_core::events::Adl> = send_events(&mut base.ctx, &[ix], &[&admin]).await.unwrap();
    assert_eq!(ev[0].triggers, perps_core::pool_math::TRIGGER_DRAWDOWN);
    assert_eq!(position(&mut base, pa).await.size, 10_000 * CONTRACT - (max_x - CONTRACT));
}

/// §7.2 wTrig (D20): inside the weekend window, net above α·B/G is
/// force-reduced to that target; outside the window it is not.
#[tokio::test]
async fn weekend_force_reduce_stops_at_its_target() {
    // α = 10%, G derived 20%: target 0.5 × B
    let params = PoolParams { weekend_cap_window_secs: DAY as u32, weekend_lp_loss_bps: 1_000, ..neutral() };
    let mut base = base_with(params).await;
    configure(&mut base, cme_config()).await.unwrap();
    freeze_funding(&mut base).await;
    let admin = base.admin.insecure_clone();
    let alice = trader(&mut base, 100_000 * USDC).await;
    let pa = open(&mut base, &alice, 0, 8_000, 10).await.unwrap(); // $600k on Wednesday
    set_price(&mut base, 75.1).await;
    assert_eq!(custom_error(adl(&mut base, Some(&admin), &alice.key(), pa, 1, 0).await), perr(PerpsError::AdlNotRequired));
    set_time(&mut base, FRIDAY_MORNING).await;
    set_price(&mut base, 75.1).await;
    let v = pool_view_at(&mut base, usd(75.1)).await;
    let target = v.b * 1_000 / 2_000;
    let max_x = ((v.net_notional - target) * 1_000_000 / usd(75.1) as u128) as u64;
    assert_eq!(custom_error(adl(&mut base, Some(&admin), &alice.key(), pa, 1, max_x + 2 * CONTRACT).await), perr(PerpsError::AdlOvershoot));
    let ix = adl_ix(&base, &admin.pubkey(), &alice.key(), pa, 1, max_x - CONTRACT);
    let ev: Vec<perps_core::events::Adl> = send_events(&mut base.ctx, &[ix], &[&admin]).await.unwrap();
    assert_eq!(ev[0].triggers, perps_core::pool_math::TRIGGER_WEEKEND);
    let v = pool_view_at(&mut base, usd(75.1)).await;
    assert!(v.net_notional >= target && v.net_notional < target + 2 * 76 * USDC as u128);
}

/// Red-team F7, F33: a withdrawal request alone shrinks capNAV (opens) but
/// not B, so it never makes exposure ADL eligible; a real loss does.
#[tokio::test]
async fn a_withdrawal_request_alone_never_triggers_exposure_adl() {
    let params = PoolParams { k_net_bps: 10_000, hard_net_bps: 15_000, adl_net_target_bps: 5_000, ..neutral() };
    let mut base = base_with(params).await;
    let admin = base.admin.insecure_clone();
    let seed = seed_lp(&base);
    let alice = trader(&mut base, 200_000 * USDC).await;
    let pa = open(&mut base, &alice, 0, 13_000, 10).await.unwrap(); // $975k
    set_price(&mut base, 75.1).await;
    lp_request(&mut base, &seed, (LP_SEED - 1_000_000) / 10 * 9).await.unwrap();
    // capNAV ≈ $100k: opens are capped ...
    assert_eq!(custom_error(open(&mut base, &alice, 0, 1, 10).await.map(|_| ())), perr(PerpsError::PoolExposureLimit));
    // ... but B is unchanged, so the hard-net trigger does not fire
    assert_eq!(custom_error(adl(&mut base, Some(&admin), &alice.key(), pa, 1, 0).await), perr(PerpsError::AdlNotRequired));
    // A $400k LP loss does: net $976k > 1.5 × $600k
    patch_pool(&mut base, |p| p.cash -= 400_000 * USDC).await;
    adl(&mut base, Some(&admin), &alice.key(), pa, 1, 100 * CONTRACT).await.unwrap();
}

/// Red-team F27 (§17.2 #14): `pending_since_long` is debt-weighted, so a large
/// second deficit restarts the write-off clock almost fully.
#[tokio::test]
async fn a_large_second_deficit_restarts_the_write_off_clock() {
    let mut base = base_with(PoolParams { lp_loss_cap_window_bps: 1, ..neutral() }).await;
    freeze_funding(&mut base).await;
    let alice = trader(&mut base, 1_000 * USDC).await;
    let bob = trader(&mut base, 10_000 * USDC).await;
    let keeper = trader(&mut base, 0).await;
    let pa = open(&mut base, &alice, 0, 100, 10).await.unwrap(); // $750
    let pb = open(&mut base, &bob, 0, 1_000, 10).await.unwrap(); // $7,500
    set_price(&mut base, 60.0).await;
    let t0 = now(&mut base).await;
    liquidate(&mut base, &keeper, &alice.key(), pa).await.unwrap();
    let first = pool(&mut base).await;
    assert_eq!(first.pending_since_long, t0);
    let p1 = first.pending_bad_debt_long;
    assert!(p1 > 600 * USDC);
    // 20 h later (24/7 market: open seconds are wall seconds) bob's $7,500 deficit lands in the same window
    let t1 = t0 + 20 * HOUR;
    set_time(&mut base, t1).await;
    liquidate(&mut base, &keeper, &bob.key(), pb).await.unwrap();
    let pl = pool(&mut base).await;
    assert_eq!(pl.pending_bad_debt_long, p1 + 7_500 * USDC);
    let since = ((t0 as i128 * p1 as i128 + t1 as i128 * 7_500 * USDC as i128) / (p1 as i128 + 7_500 * USDC as i128)) as i64;
    assert_eq!(pl.pending_since_long, since);
    assert!(since > t1 - 2 * HOUR);
    // A day after the first deficit is not enough
    set_time(&mut base, t0 + DAY).await;
    assert_eq!(custom_error(write_off(&mut base).await), perr(PerpsError::BadDebtNotStale));
    set_time(&mut base, since + DAY - 1).await;
    assert_eq!(custom_error(write_off(&mut base).await), perr(PerpsError::BadDebtNotStale));
    set_time(&mut base, since + DAY).await;
    write_off(&mut base).await.unwrap();
    assert_eq!(pool(&mut base).await.bad_debt_written_off_cum, p1 + 7_500 * USDC);
}

fn funding_ix(base: &Base) -> Instruction {
    let accounts = perps_core::accounts::UpdateFunding { keeper: base.ctx.payer.pubkey(), market: base.market, pyth_price_feed: base.oracle };
    ix(perps_core::ID, accounts, perps_core::instruction::UpdateFunding {})
}

async fn crank(base: &mut Base) -> perps_core::events::FundingUpdated {
    let i = funding_ix(base);
    let mut ev: Vec<perps_core::events::FundingUpdated> = send_events(&mut base.ctx, &[i], &[]).await.unwrap();
    assert_eq!(ev.len(), 1);
    ev.remove(0)
}

/// §8, red-team F24: exactly 168 due keeps the remainder; 169 drops one
/// period and the carry.
#[tokio::test]
async fn funding_catch_up_is_capped_at_168_periods() {
    let mut base = base_with(neutral()).await;
    let alice = trader(&mut base, 10_000 * USDC).await;
    open(&mut base, &alice, 0, 100, 5).await.unwrap();
    let f0 = market(&mut base).await.cumulative_funding;
    set_time(&mut base, WEDNESDAY + 168 * HOUR + 100).await;
    let e = crank(&mut base).await;
    assert_eq!((e.periods, e.periods_dropped, e.carry_secs), (168, 0, 100));
    let m = market(&mut base).await;
    assert_eq!((m.cumulative_funding, m.funding_carry_secs), (f0 + 168 * e.step, 100));
    // 169 periods and 100 s since (with the carry): 168 charged, 1 dropped, no carry
    let t = now(&mut base).await;
    set_time(&mut base, t + 169 * HOUR).await;
    let e2 = crank(&mut base).await;
    assert_eq!((e2.periods, e2.periods_dropped, e2.carry_secs), (168, 1, 0));
    assert_eq!(market(&mut base).await.funding_carry_secs, 0);
}

/// §8: the 17:00–18:00 ET daily break accrues nothing.
#[tokio::test]
async fn funding_skips_the_daily_break() {
    let mut base = base_with(neutral()).await;
    configure(&mut base, cme_config()).await.unwrap();
    // Wed 12:00 → 18:30 ET: 5 h + 30 min of session
    set_time(&mut base, WEDNESDAY + 6 * HOUR + 1_800).await;
    let e = crank(&mut base).await;
    assert_eq!((e.periods, e.periods_dropped, e.carry_secs), (5, 0, 1_800));
}

// ---------------------------------------------------------------------------
// §17.3: liquidation worst cases on SBF
// ---------------------------------------------------------------------------

/// A stage-1 slice whose solvency haircut leaves `C' ≤ 0` and becomes a full
/// stage-1 close, and a stage-2 deficit that rolls the loss window.
#[tokio::test]
async fn liquidation_worst_cases_fit_the_compute_budget() {
    // Stage 1 → full close
    let mut base = base_with(staged()).await;
    configure(&mut base, cme_config()).await.unwrap();
    let alice = trader(&mut base, 1_000 * USDC).await;
    let keeper = trader(&mut base, 0).await;
    let p = open(&mut base, &alice, 0, 100, 10).await.unwrap(); // $750
    // Settle a $680 loss: $70 of collateral at entry $68.20
    set_price(&mut base, 68.2).await;
    settle_pnl(&mut base, p).await.unwrap();
    // At $70.30: E = $280 is 3.98% of $7,030 (stage 1); the slice's profit
    // is haircut to 0 (U > cash = 0), and its penalty exceeds $70
    set_price(&mut base, 70.3).await;
    patch_pool(&mut base, |p| p.cash = 0).await;
    let liq = liquidate_ix(&base, &keeper, &alice.key(), p);
    let (r, full_units) = simulate(&mut base.ctx, &[liq.clone()], &[&keeper.kp]).await;
    r.unwrap();
    let ev: Vec<perps_core::events::PositionLiquidated> = send_events(&mut base.ctx, &[liq], &[&keeper.kp]).await.unwrap();
    assert_eq!((ev[0].stage, ev[0].size_closed, ev[0].size_remaining, ev[0].penalty, ev[0].owner_leftover), (1, 100 * CONTRACT, 0, 70 * USDC, 0));
    assert_eq!(ev[0].keeper_reward, 14 * USDC);
    assert!(ev[0].solvency_hc > 0);

    // Stage 2 with a deficit, after the last loss window has expired
    let mut base = base_with(staged()).await;
    configure(&mut base, cme_config()).await.unwrap();
    let bob = trader(&mut base, 1_000 * USDC).await;
    let keeper = trader(&mut base, 0).await;
    let p = open(&mut base, &bob, 0, 100, 10).await.unwrap();
    let t = now(&mut base).await;
    patch_pool(&mut base, |p| { p.loss_window_start = t - 2 * DAY; p.loss_window_nav = 1; p.lp_absorbed_window = 1; }).await;
    set_price(&mut base, 60.0).await;
    let liq = liquidate_ix(&base, &keeper, &bob.key(), p);
    let (r, deficit_units) = simulate(&mut base.ctx, &[liq.clone()], &[&keeper.kp]).await;
    r.unwrap();
    send(&mut base.ctx, &[liq], &[&keeper.kp]).await.unwrap();
    let pl = pool(&mut base).await;
    assert_eq!((pl.loss_window_start, pl.lp_absorbed_window, pl.bad_debt_cum), (t, 750 * USDC, 750 * USDC));
    println!("liquidate: stage-1 full after haircut {} CU, stage-2 deficit with window roll {} CU", full_units, deficit_units);
    if is_sbf() {
        assert!(full_units < 150_000, "liquidate (stage 1 full) used {} CU", full_units);
        assert!(deficit_units < 150_000, "liquidate (stage 2 deficit) used {} CU", deficit_units);
    }
}

// ---------------------------------------------------------------------------
// Review round 2
// ---------------------------------------------------------------------------

/// Review finding (D23 bypass by self-ADL): old opposite pending must not
/// let a winner self-ADL in the quiet time after a gapped reopen, ahead of
/// the liquidation of its bankrupt counterparty. $1 of long pending booked
/// Friday 10:00 ET is 7 open hours old at the Sunday open (floor 0).
#[tokio::test]
async fn a_winner_cannot_self_adl_in_the_quiet_time() {
    let params = PoolParams { profit_close_quiet_secs: 300, lp_loss_cap_window_bps: 1, ..neutral() };
    let mut base = base_with(params).await;
    configure(&mut base, cme_config()).await.unwrap();
    freeze_funding(&mut base).await;
    let alice = trader(&mut base, 1_000 * USDC).await;
    let bob = trader(&mut base, 2_000 * USDC).await;
    let keeper = trader(&mut base, 0).await;
    let pa = open(&mut base, &alice, 0, 100, 10).await.unwrap();
    let pb = open(&mut base, &bob, 1, 100, 5).await.unwrap();
    patch_pool(&mut base, |p| { p.pending_bad_debt_long = USDC; p.pending_since_long = FRIDAY_MORNING; }).await;

    set_time(&mut base, SUNDAY_OPEN + 5).await;
    set_price(&mut base, 60.0).await;
    // Bob, the owner, signs a deficit ADL of his own position
    assert_eq!(custom_error(adl(&mut base, Some(&bob.kp), &bob.key(), pb, 0, 0).await), perr(PerpsError::ProfitCloseQuiet));
    set_time(&mut base, SUNDAY_OPEN + 299).await;
    assert_eq!(custom_error(adl(&mut base, Some(&bob.kp), &bob.key(), pb, 0, 0).await), perr(PerpsError::ProfitCloseQuiet));
    // The keeper books alice's gap deficit meanwhile
    liquidate(&mut base, &keeper, &alice.key(), pa).await.unwrap();
    let pending = pool(&mut base).await.pending_bad_debt_long;
    assert!(pending > 600 * USDC);
    // The fresh deficit restarts the long side's clock, so the mode-0 floor
    // is back at the top; bob's close after the quiet time repays it all
    set_time(&mut base, SUNDAY_OPEN + 300).await;
    assert_eq!(custom_error(adl(&mut base, Some(&bob.kp), &bob.key(), pb, 0, 0).await), perr(PerpsError::AdlScoreBelowFloor));
    let ix = close_ix(&base, &bob, pb);
    let ev: Vec<perps_core::events::PositionClosed> = send_events(&mut base.ctx, &[ix], &[&bob.kp]).await.unwrap();
    assert_eq!(ev[0].bad_debt_hc, pending);
    assert!(!pool(&mut base).await.adl_required());
    assert_buckets(&mut base).await;
}

/// Mutation findings (§7.2 steps 8 and 10): with traders net long under an
/// active net trigger, exposure ADL may not close a profitable short (which
/// raises net), and may not close a long so large that net flips past zero
/// even though `|net|` stays above the target.
#[tokio::test]
async fn exposure_adl_cannot_flip_net_or_close_the_wrong_side() {
    let mut base = base_with(neutral()).await;
    let admin = base.admin.insecure_clone();
    let alice = trader(&mut base, 200_000 * USDC).await;
    let carol = trader(&mut base, 100_000 * USDC).await;
    let pa = open(&mut base, &alice, 0, 13_000, 10).await.unwrap();
    let pc = open(&mut base, &carol, 1, 6_000, 5).await.unwrap();
    // Net +7,000 ($525k) against a 0.1 × B line and target (≈ $100k)
    patch_pool(&mut base, |p| { p.params.k_net_bps = 1_000; p.params.hard_net_bps = 1_000; p.params.adl_net_target_bps = 1_000; }).await;
    // Carol's short is the winner at $74.90, but it is the wrong side
    set_price(&mut base, 74.9).await;
    assert_eq!(custom_error(adl(&mut base, Some(&admin), &carol.key(), pc, 1, 0).await), perr(PerpsError::AdlWrongSide));
    // Alice's long wins at $75.10. 9,000 contracts take net to −2,000
    // ($150k, above the target) — a flip, so refused
    set_price(&mut base, 75.1).await;
    let v = pool_view_at(&mut base, usd(75.1)).await;
    assert!(v.net_notional > v.b / 10 && v.b / 10 < 150_000 * USDC as u128);
    assert_eq!(custom_error(adl(&mut base, Some(&admin), &alice.key(), pa, 1, 9_000 * CONTRACT).await), perr(PerpsError::AdlOvershoot));
    // 6,000 leave $75k, under the target; 5,000 leave $150k on the same side
    assert_eq!(custom_error(adl(&mut base, Some(&admin), &alice.key(), pa, 1, 6_000 * CONTRACT).await), perr(PerpsError::AdlOvershoot));
    adl(&mut base, Some(&admin), &alice.key(), pa, 1, 5_000 * CONTRACT).await.unwrap();
    assert_eq!(pool_view_at(&mut base, usd(75.1)).await.net, 2_000 * CONTRACT as i128);
}

/// Mutation finding (D28 through `adl`): the instruction touches the peak
/// before its triggers, so a 20% drawdown against a peak exactly
/// `DD_WINDOW_SECS` old does not make exposure ADL eligible; one second
/// younger, it does.
#[tokio::test]
async fn exposure_adl_resets_an_expired_peak_first() {
    let params = PoolParams { dd_adl_bps: 1_500, adl_net_target_bps: 0, ..neutral() };
    let mut base = base_with(params).await;
    let admin = base.admin.insecure_clone();
    let alice = trader(&mut base, 80_000 * USDC).await;
    let pa = open(&mut base, &alice, 0, 1_000, 10).await.unwrap();
    set_price(&mut base, 76.0).await;
    let t = now(&mut base).await;
    let nps = pool_view_at(&mut base, 76 * USDC).await.nps;
    let window = perps_core::pool_math::DD_WINDOW_SECS;
    patch_pool(&mut base, |p| { p.nps_peak = nps / 4 * 5; p.nps_peak_at = t - window; }).await;
    assert_eq!(custom_error(adl(&mut base, Some(&admin), &alice.key(), pa, 1, 10 * CONTRACT).await), perr(PerpsError::AdlNotRequired));
    patch_pool(&mut base, |p| { p.nps_peak = nps / 4 * 5; p.nps_peak_at = t - window + 1; }).await;
    let ix = adl_ix(&base, &admin.pubkey(), &alice.key(), pa, 1, 10 * CONTRACT);
    let ev: Vec<perps_core::events::Adl> = send_events(&mut base.ctx, &[ix], &[&admin]).await.unwrap();
    assert_eq!(ev[0].triggers, perps_core::pool_math::TRIGGER_DRAWDOWN);
}

/// Review finding (retroactive wall clock, D35): pending booked Fri 15:00 ET
/// has 2 open hours of age at the weekly close. A Sunday-noon settlement
/// does not make it writable at once (wall time since booking is 45 h); it
/// needs 22 more hours after settlement.
#[tokio::test]
async fn settlement_does_not_age_pending_debt_retroactively() {
    let mut base = base_with(PoolParams { lp_loss_cap_window_bps: 1, ..neutral() }).await;
    configure(&mut base, cme_config()).await.unwrap();
    freeze_funding(&mut base).await;
    let alice = trader(&mut base, 1_000 * USDC).await;
    let bob = trader(&mut base, 2_000 * USDC).await;
    let keeper = trader(&mut base, 0).await;
    let pa = open(&mut base, &alice, 0, 100, 10).await.unwrap();
    let _pb = open(&mut base, &bob, 1, 100, 5).await.unwrap();
    let booked = FRIDAY_CLOSE - 2 * HOUR;
    set_time(&mut base, booked).await;
    set_price(&mut base, 60.0).await;
    liquidate(&mut base, &keeper, &alice.key(), pa).await.unwrap();
    assert_eq!(pool(&mut base).await.pending_since_long, booked);
    let at = SUNDAY_OPEN - 6 * HOUR;
    set_time(&mut base, at).await;
    settle(&mut base, 60 * USDC).await.unwrap();
    assert_eq!(market(&mut base).await.settled_at, at);
    assert_eq!(custom_error(write_off(&mut base).await), perr(PerpsError::BadDebtNotStale));
    set_time(&mut base, at + 22 * HOUR - 1).await;
    assert_eq!(custom_error(write_off(&mut base).await), perr(PerpsError::BadDebtNotStale));
    set_time(&mut base, at + 22 * HOUR).await;
    write_off(&mut base).await.unwrap();
    assert!(!pool(&mut base).await.adl_required());
}

/// Review finding (funding catch-up captured or escaped, D40): opens and
/// closes charge the funding that is due before they set or settle a
/// position's basis. After 5 uncranked hours with longs dominant, a new
/// short is not credited for them and the long that held through them pays
/// them on its close.
#[tokio::test]
async fn opens_and_closes_crank_due_funding_first() {
    let mut base = base_with(neutral()).await;
    let alice = trader(&mut base, 10_000 * USDC).await;
    let bob = trader(&mut base, 10_000 * USDC).await;
    let pa = open(&mut base, &alice, 0, 100, 5).await.unwrap();
    let m0 = market(&mut base).await;
    let t0 = now(&mut base).await;
    assert_eq!(m0.last_funding_time, t0);
    set_time(&mut base, t0 + 5 * HOUR).await;
    set_price(&mut base, 75.0).await;
    // Bob's open charges the 5 periods first, at the long-only rate
    let pb = open(&mut base, &bob, 1, 50, 5).await.unwrap();
    let m1 = market(&mut base).await;
    let step = m1.cumulative_funding - m0.cumulative_funding;
    assert!(step > 0 && step % 5 == 0);
    assert_eq!(m1.last_funding_time, t0 + 5 * HOUR);
    let b = position(&mut base, pb).await;
    assert_eq!(b.last_funding_payment, m1.cumulative_funding);
    assert_eq!(b.accrued_funding(m1.cumulative_funding), 0);
    // Nothing is left for a crank to charge
    assert_eq!(custom_error(update_funding(&mut base).await), perr(PerpsError::FundingNotDue));
    // 3 more uncranked hours (longs still dominant): alice's close charges
    // them first, so it settles all 8 periods she held through
    set_time(&mut base, t0 + 8 * HOUR).await;
    set_price(&mut base, 75.0).await;
    let tokens = token_balance(&mut base, alice.tokens).await;
    let ix = close_ix(&base, &alice, pa);
    let ev: Vec<perps_core::events::PositionClosed> = send_events(&mut base.ctx, &[ix], &[&alice.kp]).await.unwrap();
    let m2 = market(&mut base).await;
    assert!(m2.cumulative_funding > m1.cumulative_funding);
    assert_eq!(m2.last_funding_time, t0 + 8 * HOUR);
    assert_eq!(ev[0].funding, -(100 * (m2.cumulative_funding - m0.cumulative_funding)));
    assert!(token_balance(&mut base, alice.tokens).await > tokens);
    assert_buckets(&mut base).await;
}
