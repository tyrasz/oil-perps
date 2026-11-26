//! ADL and bad-debt write-off (docs/lp-pool.md §7, §16.4).
//!
//! - Mode 0 (deficit, permissionless) whenever `adl_required()`: the
//!   highest-score opposite-side winner whose score clears the on-chain
//!   decaying floor, sized so a slice that raises `|net|` fits the close caps
//!   (D25). The program bounds the slice to what the pending needs (D37),
//!   settles only the slice's funding (D24) and charges its close fee (D22);
//!   the keeper runs the same plan (`mirror::adl_plan`) before sending.
//! - Mode 1 (exposure, keeper- or admin-signed) when a trigger holds: the
//!   highest-score net-side winner above the param floor, sized to land on
//!   the smallest target `T_net` and checked with `exposure_stop_ok`.
//! - `write_off_bad_debt` once a side's pending is `ADL_WRITE_OFF_SECS` old.

use crate::ix::{self, KeeperResult};
use crate::liquidator::{chain_now, fetch_lp_pool, fetch_open_positions, for_each_market};
use crate::oracle::fetch_oracle_price;
use crate::mirror::{
    self, adl_plan, exposure_triggers, in_profit_close_quiet, net_after_close, pool_view, quote_profit, AdlPlan,
    ADL_MODE_DEFICIT, ADL_MODE_EXPOSURE,
};
use crate::pool_math::{self, PoolView};
use crate::state::{LpPoolData, MarketData, PositionData, SIDE_LONG, SIDE_SHORT};
use crate::KeeperConfig;
use solana_client::rpc_client::RpcClient;
use solana_sdk::{pubkey::Pubkey, signature::Signer};
use std::{sync::Arc, time::Duration};
use tokio::time::interval;
use tracing::{debug, error, info, warn};

const ADL_CHECK_INTERVAL_SECS: u64 = 10;
/// ADLs per market per pass; state is re-read after each one, because each
/// changes `B`, the pending and the targets.
const MAX_ADL_STEPS: usize = 8;

/// One ADL to send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdlAction {
    pub position: Pubkey,
    pub owner: Pubkey,
    pub mode: u8,
    /// The `size` argument (0 = the whole position).
    pub size: u64,
    /// What the program will do with it.
    pub plan: AdlPlan,
}

/// Smallest remainder `r` with `floor(r·mark, 1e6) ≥ min_notional`: a
/// partial that leaves less is closed in full (§7.2 step 3).
pub fn min_remainder_size(min_notional: u64, mark: u64) -> u64 {
    if min_notional == 0 || mark == 0 {
        return 0;
    }
    u64::try_from(pool_math::ceil_u(min_notional as u128 * 1_000_000, mark as u128)).unwrap_or(u64::MAX)
}

/// The largest slice on `side` that `check_close_caps` accepts (D14, D25):
/// any slice that does not raise `|net|`, or one that leaves `|net'|` within
/// the net cap and, while the weekend bound is on (`W > 0`, D43, at any
/// time), the weekend cap, both against `B` (the weekend cap's net of the
/// slice's skew-premium shortfall at the mark, so searched).
pub fn close_cap_room(m: &MarketData, pool: &LpPoolData, v: &PoolView, side: u8, mark: u64) -> u64 {
    if mark == 0 {
        return u64::MAX;
    }
    let p = &pool.params;
    // nn ≤ floor(k·B/1e4) and, with the bound on, nn ≤ floor(α·B/G): both exact
    let mut nn_max = pool_math::bps_of(p.k_net_bps, v.b);
    let g = p.g_bps(m);
    if g > 0 && pool_math::weekend_bound_on(p.weekend_cap_window_secs) {
        nn_max = nn_max.min(pool_math::weekend_target(p.weekend_lp_loss_bps, v.b, g));
    }
    // ceil(n·mark, 1e6) ≤ nn_max ⇔ n ≤ floor(nn_max·1e6 / mark)
    let n_max = pool_math::mul_div_floor(nn_max, 1_000_000, mark as u128).unwrap_or(u128::MAX).min(i128::MAX as u128) as i128;
    // Closing a long moves net down, a short up; work with "toward" = +
    let net = if side == SIDE_LONG { -v.net } else { v.net };
    // Not raising |net|: up to −2·net when net < 0
    let shrink = if net < 0 { 2 * -net } else { 0 };
    let room = shrink.max(n_max.saturating_sub(net)).max(0);
    let room = u64::try_from(room).unwrap_or(u64::MAX);
    // The weekend cap counts the slice's premium shortfall (D43): below the
    // closed form, the largest accepted slice by bisection (monotone in x
    // past `shrink`, where |net| rises)
    if p.skew_scale == 0 || !pool_math::weekend_bound_on(p.weekend_cap_window_secs) || room == u64::MAX {
        return room;
    }
    let ok = |x: u64| mirror::check_close_caps(m, pool, v, side, x, mark, mark).is_ok();
    if ok(room) {
        return room;
    }
    let (mut lo, mut hi) = (u64::try_from(shrink).unwrap_or(u64::MAX).min(room), room);
    while lo < hi {
        let mid = lo + (hi - lo + 1) / 2;
        if ok(mid) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    lo
}

/// Sizes to try for a mode-0 ADL of `p`: the whole position (0) unless the
/// close caps bound it, and a smaller slice that keeps the remainder at the
/// minimum notional, so the program's full-close rule cannot round it up
/// past the cap.
pub fn deficit_sizes(m: &MarketData, pool: &LpPoolData, v: &PoolView, p: &PositionData, mark: u64) -> Vec<u64> {
    let q = p.size;
    let want = if m.is_settled() { q } else { q.min(close_cap_room(m, pool, v, p.side, mark)) };
    if want == 0 {
        return vec![];
    }
    if want >= q {
        return vec![0];
    }
    let mut sizes = vec![want];
    let alt = want.min(q.saturating_sub(min_remainder_size(pool.params.liq_min_remaining_notional, mark)));
    if alt > 0 && alt != want {
        sizes.push(alt);
    }
    sizes
}

fn better(a: &AdlAction, b: &AdlAction) -> bool {
    (a.plan.score, a.plan.profit, b.position) > (b.plan.score, b.plan.profit, a.position)
}

/// Mode 0 (§7.3): the highest-score position the program accepts now.
/// `m` is the market after the funding crank `adl` runs first
/// (`mirror::project_funding`); `mark` the oracle, or the settlement price.
pub fn pick_deficit_adl(m: &MarketData, pool: &LpPoolData, positions: &[(Pubkey, PositionData)], mark: u64, now: i64) -> Option<AdlAction> {
    if !pool.adl_required() || in_profit_close_quiet(m, pool, now) {
        return None;
    }
    let v = pool_view(pool, m, mark, now)?;
    let mut best: Option<AdlAction> = None;
    for (key, p) in positions {
        if pool.opposite_pending(p.side) == 0 {
            continue;
        }
        for size in deficit_sizes(m, pool, &v, p, mark) {
            let Ok(plan) = adl_plan(m, pool, &v, p, ADL_MODE_DEFICIT, size, mark, now) else {
                continue;
            };
            if quote_profit(pool, &v, p.side, plan.profit, false).is_err() {
                continue;
            }
            let a = AdlAction { position: *key, owner: p.owner, mode: ADL_MODE_DEFICIT, size, plan };
            if best.as_ref().is_none_or(|b| better(&a, b)) {
                best = Some(a);
            }
            break;
        }
    }
    best
}

/// Smallest `|net|` whose notional `ceil(n·mark, 1e6)` is at least `target`.
pub fn min_net_for_target(target: u128, mark: u64) -> u128 {
    if target == 0 {
        return 0;
    }
    if mark == 0 {
        return u128::MAX;
    }
    match (target - 1).checked_mul(1_000_000) {
        Some(n) => n / mark as u128 + 1,
        None => u128::MAX,
    }
}

/// Mode 1 (§7.2 step 8, §7.3): when a trigger holds, the highest-score
/// winner on the net side, sized to land `|net|` on the smallest target and
/// no further (`exposure_stop_ok`).
pub fn pick_exposure_adl(m: &MarketData, pool: &LpPoolData, positions: &[(Pubkey, PositionData)], mark: u64, now: i64) -> Option<AdlAction> {
    if m.is_settled() {
        return None;
    }
    let v = pool_view(pool, m, mark, now)?;
    let (t, target) = exposure_triggers(m, pool, &v, now);
    if t == 0 || v.net == 0 {
        return None;
    }
    let n_min = min_net_for_target(target, mark);
    let net_abs = v.net.unsigned_abs();
    if n_min >= net_abs {
        return None;
    }
    let room = u64::try_from(net_abs - n_min).unwrap_or(u64::MAX);
    let side = if v.net > 0 { SIDE_LONG } else { SIDE_SHORT };
    let r_min = min_remainder_size(pool.params.liq_min_remaining_notional, mark);
    let mut best: Option<AdlAction> = None;
    for (key, p) in positions.iter().filter(|(_, p)| p.side == side) {
        let q = p.size;
        let first = q.min(room);
        let mut sizes = vec![first];
        if first < q {
            let alt = first.min(q.saturating_sub(r_min));
            if alt > 0 && alt != first {
                sizes.push(alt);
            }
        }
        for size in sizes {
            let Ok(plan) = adl_plan(m, pool, &v, p, ADL_MODE_EXPOSURE, size, mark, now) else {
                continue;
            };
            if quote_profit(pool, &v, p.side, plan.profit, false).is_err() {
                continue;
            }
            if !mirror::exposure_stop_ok(v.net, net_after_close(v.net, p.side, plan.x), mark, plan.target) {
                continue;
            }
            let a = AdlAction { position: *key, owner: p.owner, mode: ADL_MODE_EXPOSURE, size, plan };
            if best.as_ref().is_none_or(|b| better(&a, b)) {
                best = Some(a);
            }
            break;
        }
    }
    best
}

/// Whether `signer` may send mode 1: the pool's ADL keeper or the admin.
pub fn may_sign_exposure(signer: &Pubkey, pool: &LpPoolData, admin: Option<&Pubkey>) -> bool {
    *signer == pool.adl_keeper || admin == Some(signer)
}

// ---------------------------------------------------------------------------
// Loop
// ---------------------------------------------------------------------------

pub async fn run_adl_keeper(client: Arc<RpcClient>, config: Arc<KeeperConfig>) {
    let every = crate::env_secs("KEEPER_ADL_CHECK_SECS", ADL_CHECK_INTERVAL_SECS);
    let backoff = crate::env_secs("KEEPER_ERROR_BACKOFF_SECS", 60);
    info!("ADL keeper started (every {} s)", every);
    let mut tick = interval(Duration::from_secs(every));
    let mut consecutive_errors = 0;
    loop {
        tick.tick().await;
        let result = for_each_market(&client, &config, |commodity, pda, market| adl_market(&client, &config, commodity, pda, market));
        match result {
            Ok(n) => {
                consecutive_errors = 0;
                if n > 0 {
                    info!("ADL keeper sent {} transactions", n);
                }
            }
            Err(e) => {
                consecutive_errors += 1;
                error!("ADL check error (attempt {}): {}", consecutive_errors, e);
                if consecutive_errors >= 5 {
                    warn!("Too many consecutive errors, backing off...");
                    tokio::time::sleep(Duration::from_secs(backoff)).await;
                    consecutive_errors = 0;
                }
            }
        }
    }
}

/// The price `adl` would use: settlement, or the oracle in session.
pub fn mark_price(client: &RpcClient, m: &MarketData, now: i64) -> KeeperResult<Option<u64>> {
    if m.is_settled() {
        return Ok(Some(m.settlement_price));
    }
    if !m.is_in_session(now) {
        return Ok(None);
    }
    Ok(Some(fetch_oracle_price(client, m, now)?))
}

fn fetch_admin(client: &RpcClient, program_id: &Pubkey) -> Option<Pubkey> {
    let account = client.get_account(&ix::config_pda(program_id)).ok()?;
    if account.owner != *program_id || account.data.get(..8)? != crate::state::anchor_discriminator("account", "ProtocolConfig") {
        return None;
    }
    Some(Pubkey::new_from_array(account.data.get(8..40)?.try_into().ok()?))
}

fn adl_market(client: &RpcClient, config: &KeeperConfig, commodity: &str, market_pda: Pubkey, mut market: MarketData) -> KeeperResult<u32> {
    let program = config.perps_program_id;
    let signer = config.keypair.pubkey();
    let mut sent = 0u32;
    for step in 0..MAX_ADL_STEPS {
        if step > 0 {
            match crate::liquidator::fetch_market(client, config, commodity)? {
                Some((_, m)) => market = m,
                None => break,
            }
        }
        let Some(pool) = fetch_lp_pool(client, &program, &market_pda)? else {
            debug!("{} has no LP pool", commodity);
            break;
        };
        let now = chain_now(client)?;

        // Write-off first: it does not need a price
        if pool.adl_required() && mirror::write_off_amount(&market, &pool, now) > 0 {
            match ix::send(client, &config.keypair, ix::write_off_bad_debt(&program, &signer, &market_pda)) {
                Ok(sig) => {
                    info!("{} write_off_bad_debt: {}", commodity, sig);
                    sent += 1;
                    continue;
                }
                Err(e) => error!("{} write_off_bad_debt failed: {}", commodity, e),
            }
        }
        let Some(mark) = mark_price(client, &market, now)? else {
            break;
        };
        // adl cranks funding before it plans (D40)
        let m = mirror::project_funding(&market, mark, now);
        // Mode 1 only on an unsettled market whose triggers hold (cheap: no
        // position scan), and only if this keeper may sign it
        let exposure = !m.is_settled()
            && pool_view(&pool, &m, mark, now).is_some_and(|v| exposure_triggers(&m, &pool, &v, now).0 != 0);
        let exposure = exposure && {
            let ok = may_sign_exposure(&signer, &pool, fetch_admin(client, &program).as_ref());
            if !ok {
                warn!("{} exposure ADL trigger holds but this keeper is not the pool's ADL keeper or the admin", commodity);
            }
            ok
        };
        if !pool.adl_required() && !exposure {
            break;
        }
        let positions = fetch_open_positions(client, &program, &market_pda)?;
        let mut action = pick_deficit_adl(&m, &pool, &positions, mark, now);
        if action.is_none() && exposure {
            action = pick_exposure_adl(&m, &pool, &positions, mark, now);
        }
        let Some(a) = action else {
            break;
        };
        let ix = ix::adl(&program, &signer, &market_pda, &a.position, &a.owner, &market.pyth_price_feed, a.mode, a.size);
        match ix::send(client, &config.keypair, ix) {
            Ok(sig) => {
                info!(
                    "{} adl mode {} on {}: x={} profit={} score={} floor={} triggers={} tx {}",
                    commodity, a.mode, a.position, a.plan.x, a.plan.profit, a.plan.score, a.plan.floor, a.plan.triggers, sig
                );
                sent += 1;
            }
            Err(e) => {
                error!("{} adl mode {} on {} failed: {}", commodity, a.mode, a.position, e);
                break;
            }
        }
    }
    Ok(sent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mirror::{adl_plan, Reject};
    use crate::pool_math::{ADL_FLOOR_DECAY_SECS, TRIGGER_NET, TRIGGER_WEEKEND};
    use crate::session;
    use crate::state::PoolParamsData;
    use crate::testkit::*;

    #[test]
    fn min_remainder_and_min_net() {
        // $100 at $75: ceil(100e6·1e6/75e6) = 1_333_334 units
        assert_eq!(min_remainder_size(100 * C6, 75 * C6), 1_333_334);
        assert_eq!(pool_math::notional_floor(1_333_334, 75 * C6), 100_000_050);
        assert!(pool_math::notional_floor(1_333_333, 75 * C6) < 100 * C6);
        assert_eq!(min_remainder_size(0, 75 * C6), 0);
        // ceil(n·75e6/1e6) ≥ T
        for t in [0u128, 1, 74, 75, 76, 7_500 * C6 as u128, 7_500 * C6 as u128 + 1] {
            let n = min_net_for_target(t, 75 * C6);
            assert!(pool_math::ceil_u(n * 75, 1) >= t);
            if n > 0 {
                assert!(pool_math::ceil_u((n - 1) * 75, 1) < t, "t={} n={}", t, n);
            }
        }
        assert_eq!(min_net_for_target(u128::MAX, 1), u128::MAX);
    }

    /// `close_cap_room` is exactly the edge of `check_close_caps`.
    #[test]
    fn close_cap_room_is_the_edge_of_the_caps() {
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut m = market();
        m.session_schedule = session::SCHEDULE_CME_GLOBEX;
        m.weekend_initial_margin_ratio = 2_000;
        for i in 0..5_000 {
            let mut pool = LpPoolData { params: PoolParamsData::production(), ..Default::default() };
            pool.params.k_net_bps = 1 + (next() % 30_000) as u32;
            pool.params.weekend_lp_loss_bps = (next() % 3_000) as u32;
            let net = (next() % 40_000_000_000) as i128 - 20_000_000_000;
            let b = (next() % 3_000_000_000_000) as u128;
            let mark = 1 + next() % 200_000_000;
            // The weekend bound is on (W > 0) or off: no time dependence (D43)
            pool.params.weekend_cap_window_secs = if i % 2 == 0 { 0 } else { 86_400 };
            let v = PoolView { net, b, ..Default::default() };
            for side in [SIDE_LONG, SIDE_SHORT] {
                let room = close_cap_room(&m, &pool, &v, side, mark);
                if room < u64::MAX / 2 {
                    assert!(mirror::check_close_caps(&m, &pool, &v, side, room, mark, mark).is_ok(), "{} {} {} {}", net, b, mark, side);
                    assert!(mirror::check_close_caps(&m, &pool, &v, side, room + 1, mark, mark).is_err(), "{} {} {} {} {}", net, b, mark, side, room);
                }
            }
        }
    }

    /// Two winners opposite a pending deficit: the higher score goes first,
    /// nobody before the floor admits them, and nothing during the quiet time.
    #[test]
    fn deficit_adl_picks_the_highest_eligible_score() {
        let (mut m, mut pool) = (market(), pool());
        // Two shorts entered at $80 and $76; the higher-levered one scores higher
        let a = open(&mut m, &mut pool, SIDE_SHORT, 100 * C6, 80 * C6, 1_000 * C6);
        let b = open(&mut m, &mut pool, SIDE_SHORT, 100 * C6, 76 * C6, 3_000 * C6);
        let _l = open(&mut m, &mut pool, SIDE_LONG, 200 * C6, 78 * C6, 3_000 * C6);
        let positions = keyed(&[a.clone(), b.clone()]);
        pool.pending_bad_debt_long = 50 * C6;
        pool.pending_since_long = 1;
        let mark = 70 * C6;
        let v = pool_view(&pool, &m, mark, 1).unwrap();
        let sa = adl_plan(&m, &pool, &v, &a, 0, 0, mark, 1 + ADL_FLOOR_DECAY_SECS).unwrap().score;
        let sb = adl_plan(&m, &pool, &v, &b, 0, 0, mark, 1 + ADL_FLOOR_DECAY_SECS).unwrap().score;
        assert!(sa > sb, "{} {}", sa, sb);
        // Right after booking the floor is 1e6 bps: nobody qualifies
        assert!(pick_deficit_adl(&m, &pool, &positions, mark, 1).is_none());
        // Once the floor reaches a's score, a and only a
        let t = (1..=ADL_FLOOR_DECAY_SECS)
            .find(|dt| pool_math::adl_floor_now(*dt) as u128 <= sa)
            .unwrap();
        let act = pick_deficit_adl(&m, &pool, &positions, mark, 1 + t).unwrap();
        assert_eq!((act.position, act.mode), (positions[0].0, ADL_MODE_DEFICIT));
        assert!(act.plan.floor as u128 <= sa && act.plan.floor as u128 > sb);
        // A size of 0 lets the program cut it to what the pending needs (D37):
        // $50 + 4 units at $10 a contract
        assert_eq!((act.size, act.plan.x), (0, 5_000_001));
        // Later both qualify; still a first
        let act = pick_deficit_adl(&m, &pool, &positions, mark, 1 + ADL_FLOOR_DECAY_SECS).unwrap();
        assert_eq!(act.position, positions[0].0);
        // Losers and same-side winners are never picked
        let only_long = keyed(&[open(&mut m.clone(), &mut pool.clone(), SIDE_LONG, 10 * C6, 60 * C6, 100 * C6)]);
        assert!(pick_deficit_adl(&m, &pool, &only_long, mark, 1 + ADL_FLOOR_DECAY_SECS).is_none());
        // Nothing pending: nothing to do
        let mut p2 = pool;
        p2.pending_bad_debt_long = 0;
        assert!(pick_deficit_adl(&m, &p2, &positions, mark, 1 + ADL_FLOOR_DECAY_SECS).is_none());
    }

    /// D25: a slice that raises |net| is sized within the net cap instead of
    /// failing `PoolExposureLimit` (adl.rs `deficit_adl_that_raises_net_is_capped`).
    #[test]
    fn deficit_adl_is_sized_within_the_close_caps() {
        let (mut m, mut pool) = (market(), pool());
        pool.params.k_net_bps = 10_000;
        let _long = open(&mut m, &mut pool, SIDE_LONG, 20_000 * C6, 75 * C6, 150_000 * C6);
        let short = open(&mut m, &mut pool, SIDE_SHORT, 20_000 * C6, 75 * C6, 150_000 * C6);
        pool.pending_bad_debt_long = 10_000 * C6;
        pool.pending_since_long = 1;
        let (mark, now) = (74_900_000, 1 + ADL_FLOOR_DECAY_SECS);
        let positions = keyed(std::slice::from_ref(&short));
        let v = pool_view(&pool, &m, mark, now).unwrap();
        assert_eq!(adl_plan(&m, &pool, &v, &short, 0, 0, mark, now), Err(Reject::PoolExposureLimit));
        let act = pick_deficit_adl(&m, &pool, &positions, mark, now).unwrap();
        // The largest slice whose net notional fits B
        let room = close_cap_room(&m, &pool, &v, SIDE_SHORT, mark);
        assert_eq!((act.size, act.plan.x), (room, room));
        assert!(room > 10_000 * C6 && room < 20_000 * C6);
        assert_eq!(adl_plan(&m, &pool, &v, &short, 0, room + 1, mark, now), Err(Reject::PoolExposureLimit));
        // Settled: no caps, the whole position
        m.settlement_price = mark;
        assert_eq!(pick_deficit_adl(&m, &pool, &positions, mark, now).unwrap().size, 0);
    }

    /// The minimum-remainder rule would round a capped slice up to the whole
    /// position; the keeper then asks for a slice that leaves the minimum.
    #[test]
    fn deficit_adl_keeps_the_minimum_remainder_under_the_cap() {
        let (mut m, mut pool) = (market(), pool());
        pool.params.k_net_bps = 10_000;
        pool.params.liq_min_remaining_notional = 100 * C6;
        pool.cash = 1_000 * C6;
        pool.lp_shares = 1_000 * C6;
        // B ≈ $1,000: 13.3 contracts of net at $75; the short is 14 contracts
        let short = open(&mut m, &mut pool, SIDE_SHORT, 14 * C6, 76 * C6, 300 * C6);
        let _long = open(&mut m, &mut pool, SIDE_LONG, 14 * C6, 76 * C6, 300 * C6);
        pool.pending_bad_debt_long = 1_000 * C6;
        pool.pending_since_long = 1;
        let (mark, now) = (75 * C6, 1 + ADL_FLOOR_DECAY_SECS);
        let v = pool_view(&pool, &m, mark, now).unwrap();
        let room = close_cap_room(&m, &pool, &v, SIDE_SHORT, mark);
        assert!(room < 14 * C6 && 14 * C6 - room < min_remainder_size(100 * C6, mark), "room {}", room);
        // Asking for the room: the remainder rule closes it all, over the cap
        assert_eq!(adl_plan(&m, &pool, &v, &short, 0, room, mark, now), Err(Reject::PoolExposureLimit));
        let act = pick_deficit_adl(&m, &pool, &keyed(std::slice::from_ref(&short)), mark, now).unwrap();
        assert_eq!(act.size, 14 * C6 - min_remainder_size(100 * C6, mark));
        assert_eq!(act.plan.x, act.size);
    }

    /// Mode 1: the highest score first (in full when it is smaller than the
    /// room), then the next one lands net exactly on the smallest target.
    #[test]
    fn exposure_adl_lands_on_the_target() {
        let setup = |with_small: bool, cash: u64| {
            let (mut m, mut pool) = (market(), pool());
            pool.params.hard_net_bps = 15_000;
            pool.params.k_net_bps = 10_000;
            pool.params.skew_scale = 0;
            pool.cash = cash;
            pool.lp_shares = 100_000 * C6;
            let mut ps = vec![open(&mut m, &mut pool, SIDE_LONG, 2_000 * C6, 75 * C6, 40_000 * C6)];
            if with_small {
                ps.push(open(&mut m, &mut pool, SIDE_LONG, 500 * C6, 70 * C6, 3_000 * C6));
            }
            (m, pool, keyed(&ps))
        };
        let (mark, now) = (76 * C6, 1);
        // 2,500 contracts net long at $76 = $190k > 1.5 × B
        let (m, pool, positions) = setup(true, 100_000 * C6);
        let v = pool_view(&pool, &m, mark, now).unwrap();
        let (t, target) = exposure_triggers(&m, &pool, &v, now);
        assert_eq!((t, target), (TRIGGER_NET, v.b)); // k_net = 1× B
        let act = pick_exposure_adl(&m, &pool, &positions, mark, now).unwrap();
        // The small position (entry $70, more levered) scores higher and is
        // smaller than the room: closed in full
        assert_eq!((act.position, act.size, act.plan.x), (positions[1].0, 500 * C6, 500 * C6));
        assert!(mirror::exposure_stop_ok(v.net, net_after_close(v.net, SIDE_LONG, act.plan.x), mark, target));

        // After it (paid its $3,000): still $152k > 1.5 × $95k, and the big
        // one lands on the new target
        let (m, pool, positions) = setup(false, 97_000 * C6);
        let v = pool_view(&pool, &m, mark, now).unwrap();
        let (_, target) = exposure_triggers(&m, &pool, &v, now);
        let act = pick_exposure_adl(&m, &pool, &positions, mark, now).unwrap();
        assert_eq!(act.size, act.plan.x);
        assert_eq!(act.plan.x as u128, v.net.unsigned_abs() - min_net_for_target(target, mark));
        let after = net_after_close(v.net, SIDE_LONG, act.plan.x);
        assert!(mirror::exposure_stop_ok(v.net, after, mark, target));
        assert!(!mirror::exposure_stop_ok(v.net, after - 1, mark, target));

        // No trigger: nothing; settled: nothing
        let mut calm = pool;
        calm.params.hard_net_bps = 0;
        assert!(pick_exposure_adl(&m, &calm, &positions, mark, now).is_none());
        let mut s = m.clone();
        s.settlement_price = mark;
        assert!(pick_exposure_adl(&s, &pool, &positions, mark, now).is_none());
    }

    /// Mode 1 when one position is smaller than the room: closed in full;
    /// a slice that would leave less than the minimum is trimmed instead of
    /// overshooting.
    #[test]
    fn exposure_adl_full_close_and_remainder_trim() {
        let (mut m, mut pool) = (market(), pool());
        pool.params.hard_net_bps = 1;
        pool.params.k_net_bps = 1;
        pool.params.adl_net_target_bps = 0;
        pool.params.skew_scale = 0;
        let long = open(&mut m, &mut pool, SIDE_LONG, 100 * C6, 75 * C6, 1_500 * C6);
        let positions = keyed(std::slice::from_ref(&long));
        let mark = 76 * C6;
        let v = pool_view(&pool, &m, mark, 1).unwrap();
        let (_, target) = exposure_triggers(&m, &pool, &v, 1);
        // Target ~0.01% of B; room is nearly the whole position, the
        // remainder below the minimum notional, so the slice is trimmed
        let act = pick_exposure_adl(&m, &pool, &positions, mark, 1).unwrap();
        assert_eq!(act.plan.x, 100 * C6 - min_remainder_size(pool.params.liq_min_remaining_notional, mark));
        assert!(mirror::exposure_stop_ok(v.net, net_after_close(v.net, SIDE_LONG, act.plan.x), mark, target));
        // With no minimum, it lands on the target exactly
        pool.params.liq_min_remaining_notional = 0;
        let act = pick_exposure_adl(&m, &pool, &positions, mark, 1).unwrap();
        assert_eq!(act.plan.x as u128, v.net.unsigned_abs() - min_net_for_target(target, mark));
        // A target of 0 closes the whole position
        pool.params.k_net_bps = 0;
        pool.params.hard_net_bps = 1;
        let act = pick_exposure_adl(&m, &pool, &positions, mark, 1).unwrap();
        assert_eq!(act.plan.x, 100 * C6);
    }

    /// The weekend force-reduce (D20) inside W, with the param floor.
    #[test]
    fn exposure_adl_weekend_trigger_and_floor() {
        let (mut m, mut pool) = (market(), pool());
        m.session_schedule = session::SCHEDULE_CME_GLOBEX;
        m.weekend_initial_margin_ratio = 2_000;
        pool.params.weekend_cap_window_secs = 86_400;
        pool.params.weekend_lp_loss_bps = 1_000;
        pool.params.skew_scale = 0;
        pool.cash = 100_000 * C6;
        pool.lp_shares = 100_000 * C6;
        // 1,000 contracts net short at $70 = $70k > α/G·B = $50k
        let short = open(&mut m, &mut pool, SIDE_SHORT, 1_000 * C6, 75 * C6, 15_000 * C6);
        let positions = keyed(std::slice::from_ref(&short));
        let mark = 70 * C6;
        assert!(pick_exposure_adl(&m, &pool, &positions, mark, WEDNESDAY).is_none());
        let now = FRIDAY_CLOSE - 3_600;
        let act = pick_exposure_adl(&m, &pool, &positions, mark, now).unwrap();
        assert_eq!(act.plan.triggers, TRIGGER_WEEKEND);
        let v = pool_view(&pool, &m, mark, now).unwrap();
        let after = net_after_close(v.net, SIDE_SHORT, act.plan.x);
        assert!(after < 0 && mirror::exposure_stop_ok(v.net, after, mark, act.plan.target));
        // A floor above its score excludes it
        pool.params.adl_exposure_score_floor_bps = (act.plan.score + 1) as u32;
        assert!(pick_exposure_adl(&m, &pool, &positions, mark, now).is_none());
    }

    #[test]
    fn exposure_signer() {
        let k = Pubkey::new_unique();
        let pool = LpPoolData { adl_keeper: k, ..Default::default() };
        assert!(may_sign_exposure(&k, &pool, None));
        let admin = Pubkey::new_unique();
        assert!(may_sign_exposure(&admin, &pool, Some(&admin)));
        assert!(!may_sign_exposure(&Pubkey::new_unique(), &pool, Some(&admin)));
    }
}
