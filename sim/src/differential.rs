//! Differential tests: the model against perps-core's own instruction bodies
//! (`settle_close`, `liquidate_position`, `adl_plan` + `adl_apply`,
//! `settle_pnl`, `write_off_stale`, `house_open_pool`), on thousands of
//! random pool states, natively. The scenario runs check the model against
//! the SBF binaries, but a two-day run rarely reaches the rarer paths (pending
//! bad debt, both haircuts, mode-0 ADL slices, stage-1 fallbacks, the LP
//! window cap, the write-off); here every one of them is hit, and the result
//! (error, or every account field) must be identical.

use crate::model::{self, St};
use crate::predict::Rej;
use perps_core::state::{ExecutionSource, LpPool, Market, PoolParams, Position, PositionStatus, Side, UserAccount};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use solana_sdk::pubkey::Pubkey;

const C6: u64 = 1_000_000;

fn market() -> Market {
    Market {
        max_leverage: 20_000, maintenance_margin_ratio: 500, initial_margin_ratio: 1_000, taker_fee: 5, liquidation_fee: 100,
        max_open_interest: 1_000_000 * C6, funding_interval: 3_600, ..Default::default()
    }
}

/// A random but consistent book: positions in the aggregates and OI, cash,
/// insurance, pending debt on either side, a loss window part used.
fn book(rng: &mut StdRng) -> (St, Vec<Position>) {
    let mut m = market();
    m.cumulative_funding = rng.gen_range(-50_000..50_000);
    m.insurance_fund = if rng.gen_bool(0.3) { 0 } else { rng.gen_range(0..300_000) * C6 };
    let mut params = PoolParams::production();
    if rng.gen_bool(0.3) {
        params = PoolParams::neutral();
    }
    if rng.gen_bool(0.3) {
        params.skew_scale = 0;
    }
    params.lp_loss_cap_window_bps = [0, 1, 500, 10_000][rng.gen_range(0..4)];
    params.insurance_claim_cap_per_window = [0, 100 * C6][rng.gen_range(0..2)];
    params.liq_min_remaining_notional = [0, 100 * C6][rng.gen_range(0..2)];
    params.k_net_bps = [10_000, 100_000, 1_000][rng.gen_range(0..3)];
    let cash = if rng.gen_bool(0.2) { rng.gen_range(0..5_000) * C6 } else { rng.gen_range(0..2_000_000) * C6 };
    let mut pool = LpPool { cash, lp_shares: 1_000_000 * C6, params, ..Default::default() };
    pool.pending_withdraw_shares = if rng.gen_bool(0.2) { rng.gen_range(0..500_000) * C6 } else { 0 };
    pool.nps_peak = if rng.gen_bool(0.5) { rng.gen_range(900_000..1_100_000) } else { 0 };
    pool.nps_peak_at = 1;
    if rng.gen_bool(0.5) {
        pool.pending_bad_debt_long = rng.gen_range(1..2_000) * C6 / [1, 1000][rng.gen_range(0..2)];
        pool.pending_since_long = rng.gen_range(0..90_000);
    }
    if rng.gen_bool(0.5) {
        pool.pending_bad_debt_short = rng.gen_range(1..2_000) * C6 / [1, 1000][rng.gen_range(0..2)];
        pool.pending_since_short = rng.gen_range(0..90_000);
    }
    if rng.gen_bool(0.5) {
        pool.loss_window_start = 50_000;
        pool.loss_window_nav = rng.gen_range(0..1_000_000) * C6;
        pool.lp_absorbed_window = rng.gen_range(0..10_000) * C6;
        pool.ins_paid_window = rng.gen_range(0..200) * C6;
    }
    let mut positions = Vec::new();
    for _ in 0..rng.gen_range(1..8) {
        let side = if rng.gen_bool(0.5) { Side::Long } else { Side::Short };
        let size = rng.gen_range(1..5_000) * C6 / [1, 7, 1000][rng.gen_range(0..3)] + rng.gen_range(0..1_000);
        let entry = rng.gen_range(60_000_000..90_000_000);
        let collateral = (size as u128 * entry as u128 / C6 as u128) as u64 / rng.gen_range(2..25) + 1;
        let last = m.cumulative_funding - rng.gen_range(-20_000..20_000);
        let src = if rng.gen_bool(0.5) { ExecutionSource::OrderBook } else { ExecutionSource::Venue };
        let opened = if rng.gen_bool(0.8) { entry } else { rng.gen_range(60_000_000..90_000_000) };
        let p = Position {
            side, size, entry_price: entry, collateral, leverage: 5_000, last_funding_payment: last, status: PositionStatus::Open,
            execution_source: src, opened_price: opened, owner: Pubkey::new_unique(), realized_pnl: rng.gen_range(-1_000..1_000), ..Default::default()
        };
        model::pool_add(&mut pool, side, size, entry, last).unwrap();
        pool.position_collateral += collateral;
        match side {
            Side::Long => m.long_open_interest += size,
            Side::Short => m.short_open_interest += size,
        }
        positions.push(p);
    }
    pool.free_collateral = 10_000_000 * C6;
    (St { m, pool }, positions)
}

fn user_for(p: &Position, rng: &mut StdRng) -> UserAccount {
    let house = if p.execution_source == ExecutionSource::OrderBook { model::opened_notional(p, p.size) } else { 0 };
    UserAccount { owner: p.owner, collateral_balance: rng.gen_range(0..10_000) * C6, house_entry_notional: house + rng.gen_range(0..1_000) * C6, ..Default::default() }
}

/// The engine's error name, as the chain decoder would report it.
fn name(e: &anchor_lang::error::Error) -> String {
    match e {
        anchor_lang::error::Error::AnchorError(a) => a.error_name.clone(),
        other => format!("{:?}", other),
    }
}

/// Compare an engine result with the model's, and the accounts they left.
fn same<T, U>(what: &str, i: usize, engine: &Result<T, anchor_lang::error::Error>, model: &Result<U, Rej>, e: (&St, &Position, &UserAccount), m: (&St, &Position, &UserAccount)) {
    match (engine, model) {
        (Err(a), Err(b)) => assert_eq!(name(a), b.1, "{} #{}: errors differ", what, i),
        (Ok(_), Ok(_)) => {
            let mut d = model::diff_market(&e.0.m, &m.0.m);
            d.extend(model::diff_pool(&e.0.pool, &m.0.pool));
            d.extend(model::diff_position(e.1, m.1));
            d.extend(model::diff_user(e.2, m.2));
            assert!(d.is_empty(), "{} #{}: accounts differ: {}", what, i, d.join("; "));
        }
        (Err(a), Ok(_)) => panic!("{} #{}: engine fails {}, model lands", what, i, name(a)),
        (Ok(_), Err(b)) => panic!("{} #{}: model predicts {}, engine lands", what, i, b.1),
    }
}

const N: usize = 3_000;

#[test]
fn closes_match_the_engine() {
    let mut rng = StdRng::seed_from_u64(1);
    let mut outcomes = std::collections::BTreeMap::<String, u32>::new();
    for i in 0..N {
        let (st, ps) = book(&mut rng);
        let p = ps[rng.gen_range(0..ps.len())].clone();
        let u = user_for(&p, &mut rng);
        let mark = (p.entry_price as i64 + rng.gen_range(-15_000_000..15_000_000)) as u64;
        let exec = mark + rng.gen_range(0..100_000);
        let now = 100_000;
        let (mut es, mut ep, mut eu) = (st.clone(), p.clone(), u.clone());
        let er = perps_core::instructions::settle_close(&mut es.m, &mut es.pool, &mut ep, &mut eu, Pubkey::default(), mark, exec, now, None);
        let (mut ms, mut mp, mut mu) = (st.clone(), p.clone(), u.clone());
        let mr = model::settle_close(&mut ms, &mut mp, &mut mu, mark, exec, now);
        same("settle_close", i, &er, &mr, (&es, &ep, &eu), (&ms, &mp, &mu));
        let k = match &mr {
            Err(r) => r.1.to_string(),
            Ok(o) => format!("{}{}{}", if o.deficit.is_some() { "deficit" } else { "ok" }, if o.bad_debt_hc > 0 { "+bdhc" } else { "" }, if o.solvency_hc > 0 { "+shc" } else { "" }),
        };
        *outcomes.entry(k).or_default() += 1;
    }
    for k in ["ok", "deficit", "PoolCashInsufficient", "PoolExposureLimit"] {
        assert!(outcomes.contains_key(k), "closes never reached {}: {:?}", k, outcomes);
    }
    assert!(outcomes.keys().any(|k| k.contains("bdhc")) && outcomes.keys().any(|k| k.contains("shc")), "{:?}", outcomes);
}

#[test]
fn liquidations_match_the_engine() {
    let mut rng = StdRng::seed_from_u64(2);
    let mut outcomes = std::collections::BTreeMap::<String, u32>::new();
    for i in 0..N {
        let (st, ps) = book(&mut rng);
        let p = ps[rng.gen_range(0..ps.len())].clone();
        let u = user_for(&p, &mut rng);
        // Around the maintenance line: mostly liquidatable, some bankrupt
        let move_bps = rng.gen_range(500..2_500) as i64;
        let dir = if p.side == Side::Long { -1 } else { 1 };
        let mark = (p.entry_price as i64 + dir * p.entry_price as i64 * move_bps / 10_000) as u64;
        let now = 100_000;
        let (mut es, mut ep, mut eu) = (st.clone(), p.clone(), u.clone());
        let er = perps_core::instructions::liquidate_position(&mut es.m, &mut es.pool, &mut ep, &mut eu, Pubkey::default(), mark, now);
        let (mut ms, mut mp, mut mu) = (st.clone(), p.clone(), u.clone());
        let mr = model::liquidate_core(&mut ms, &mut mp, &mut mu, mark, now);
        same("liquidate", i, &er, &mr, (&es, &ep, &eu), (&ms, &mp, &mu));
        if let (Ok(e), Ok(o)) = (&er, &mr) {
            assert_eq!((e.keeper_reward, e.owner_leftover, e.penalty, e.size_closed), (o.keeper_reward, o.owner_leftover, o.penalty, o.size_closed), "liquidate #{}", i);
        }
        let k = match &mr {
            Err(r) => r.1.to_string(),
            Ok(o) => format!("stage{}{}{}", o.stage, if o.size_remaining > 0 { "partial" } else { "" }, if o.deficit.map_or(false, |d| d.pending > 0) { "+pending" } else { "" }),
        };
        *outcomes.entry(k).or_default() += 1;
    }
    for k in ["stage1", "stage1partial", "stage2", "stage2+pending", "NotLiquidatable"] {
        assert!(outcomes.contains_key(k), "liquidations never reached {}: {:?}", k, outcomes);
    }
}

#[test]
fn adl_matches_the_engine() {
    let mut rng = StdRng::seed_from_u64(3);
    let mut outcomes = std::collections::BTreeMap::<String, u32>::new();
    for i in 0..N {
        let (mut st, ps) = book(&mut rng);
        if rng.gen_bool(0.3) {
            st.pool.params.hard_net_bps = 0;
            st.pool.params.weekend_cap_window_secs = 0;
        }
        let p = ps[rng.gen_range(0..ps.len())].clone();
        let u = user_for(&p, &mut rng);
        let dir = if p.side == Side::Long { 1 } else { -1 };
        let mark = (p.entry_price as i64 + dir * rng.gen_range(-200_000..8_000_000)) as u64;
        let now = rng.gen_range(90_000..200_000);
        let mode = if rng.gen_bool(0.7) { 0 } else { 1 };
        let size = if rng.gen_bool(0.5) { 0 } else { rng.gen_range(1..=p.size) };
        let (mut es, mut ep, mut eu) = (st.clone(), p.clone(), u.clone());
        let er = (|| {
            let v = perps_core::state::pool_view(&es.pool, &es.m, mark, now, es.pool.params.redeem_loss_haircut_bps)?;
            perps_core::state::touch_peak(&mut es.pool, &v, now);
            let plan = perps_core::instructions::adl_plan(&es.m, &es.pool, &v, &ep, mode, size, mark, now)?;
            let o = perps_core::instructions::adl_apply(&mut es.m, &mut es.pool, &v, &mut ep, &mut eu, &plan, now)?;
            let after = perps_core::state::pool_view(&es.pool, &es.m, mark, now, es.pool.params.redeem_loss_haircut_bps)?;
            if mode == 1 && !perps_core::instructions::exposure_stop_ok(v.net, &after, plan.target) {
                return Err(perps_core::errors::PerpsError::AdlOvershoot.into());
            }
            Ok((plan, o))
        })();
        let (mut ms, mut mp, mut mu) = (st.clone(), p.clone(), u.clone());
        let mr = model::adl_core(&mut ms, &mut mp, &mut mu, mode, size, mark, now);
        same("adl", i, &er, &mr, (&es, &ep, &eu), (&ms, &mp, &mu));
        if let (Ok((plan, o)), Ok(m)) = (&er, &mr) {
            assert_eq!((plan.x, plan.score, plan.floor, plan.triggers, o.paid, o.fee, o.credit), (m.x, m.score, m.floor, m.triggers, m.paid, m.fee, m.credit), "adl #{}", i);
        }
        let k = match &mr {
            Err(r) => r.1.to_string(),
            Ok(o) => format!("mode{}{}", o.mode, if o.x < p.size { "partial" } else { "" }),
        };
        *outcomes.entry(k).or_default() += 1;
    }
    for k in ["mode0", "mode0partial", "mode1", "AdlScoreBelowFloor", "AdlWrongSide", "AdlNotRequired", "AdlCandidateNotProfitable"] {
        assert!(outcomes.contains_key(k), "ADL never reached {}: {:?}", k, outcomes);
    }
}

#[test]
fn settle_pnl_and_write_off_match_the_engine() {
    let mut rng = StdRng::seed_from_u64(4);
    for i in 0..N {
        let (st, ps) = book(&mut rng);
        let p = ps[rng.gen_range(0..ps.len())].clone();
        let mark = (p.entry_price as i64 + rng.gen_range(-10_000_000..10_000_000)) as u64;
        let now = rng.gen_range(90_000..200_000);
        let (mut es, mut ep) = (st.clone(), p.clone());
        let er = perps_core::instructions::settle_pnl(&es.m.clone(), &mut es.pool, &mut ep, mark, now);
        let (mut ms, mut mp) = (st.clone(), p.clone());
        let mr = match model::settle_pnl(&mut ms, &mut mp, &crate::predict::OracleView { price: mark, conf: 0, published_at: now }, now) {
            Err(r) if r.1 == "MarketClosed" => continue,
            r => r,
        };
        let u = UserAccount::default();
        same("settle_pnl", i, &er, &mr, (&es, &ep, &u), (&ms, &mp, &u));
        let (mut es, mut ms) = (st.clone(), st.clone());
        let w = perps_core::instructions::write_off_stale(&es.m.clone(), &mut es.pool, now);
        let mw = model::write_off(&mut ms, now).unwrap_or(0);
        assert_eq!(w, mw, "write-off #{}", i);
        assert!(model::diff_pool(&es.pool, &ms.pool).is_empty(), "write-off #{}", i);
    }
}

#[test]
fn house_opens_match_the_engine() {
    let mut rng = StdRng::seed_from_u64(5);
    let mut outcomes = std::collections::BTreeMap::<String, u32>::new();
    for i in 0..N {
        let (st, ps) = book(&mut rng);
        let mut u = user_for(&ps[0], &mut rng);
        u.collateral_balance = 100_000_000 * C6;
        let side = if rng.gen_bool(0.5) { Side::Long } else { Side::Short };
        let size = rng.gen_range(1..20_000) * C6;
        let oracle = rng.gen_range(60_000_000..90_000_000);
        let now = 100_000;
        let mut es = st.clone();
        let mut eu = u.clone();
        let er = (|| {
            let q = perps_core::instructions::house_quote(&es.m, &es.pool, side, size, 5_000, oracle, 0, now)?;
            perps_core::instructions::house_open_pool(&mut es.m, &mut es.pool, &mut eu, side, size, oracle, &q, now)?;
            Ok::<_, anchor_lang::error::Error>(q)
        })();
        // The model's open also gates, cranks and counts; run it on an always-open market at a fresh oracle
        let mut ms = st.clone();
        ms.m.last_funding_time = now;
        es.m.last_funding_time = now;
        let mut mu = u.clone();
        let o = crate::predict::OracleView { price: oracle, conf: 0, published_at: now };
        let mr = model::open(&mut ms, &mut mu, &o, now, side, size, 5_000, 0);
        if er.is_ok() {
            es.m.total_positions += 1;
            es.m.total_trades += 1;
            eu.total_positions += 1;
            eu.total_trades += 1;
        }
        let pp = Position::default();
        same("open", i, &er, &mr, (&es, &pp, &eu), (&ms, &pp, &mu));
        if let (Ok(q), Ok(m)) = (&er, &mr) {
            assert_eq!((q.exec, q.fee, q.notional), (m.exec, m.fee, m.notional), "open #{}", i);
        }
        *outcomes.entry(mr.map_or_else(|r| r.1.to_string(), |_| "ok".into())).or_default() += 1;
    }
    for k in ["ok", "PoolExposureLimit", "PoolAdlPending", "AccountExposureLimit"] {
        assert!(outcomes.contains_key(k), "opens never reached {}: {:?}", k, outcomes);
    }
}
