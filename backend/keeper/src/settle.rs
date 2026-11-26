//! Pool cash and settled-market upkeep (docs/lp-pool.md §6.6, §10, §16.4).
//!
//! - `settle_position_pnl` of the largest losers when pool cash is short of
//!   what the winners would be paid (Σ positive P), and every 15 minutes, so
//!   matched winners do not fail `PoolCashInsufficient` (D3).
//! - `close_settled_position` on every open position of a settled market:
//!   losers first, winners only once no loser is left and the profit-close
//!   quiet time after settlement has passed (D19, D35).
//! - `sweep_insurance_excess` when insurance is above its target, or the
//!   market is settled with no open positions (§10.1).

use crate::ix::{self, KeeperResult, ERR_POOL_CASH_INSUFFICIENT};
use crate::liquidator::{chain_now, fetch_lp_pool, fetch_open_positions, for_each_market};
use crate::mirror::{in_profit_close_quiet, sweep_amount};
use crate::state::{LpPoolData, MarketData, PositionData};
use crate::KeeperConfig;
use solana_client::rpc_client::RpcClient;
use solana_sdk::{pubkey::Pubkey, signature::Signer};
use std::collections::HashMap;
use std::sync::Mutex;
use std::{sync::Arc, time::Duration};
use tokio::time::interval;
use tracing::{debug, error, info, warn};

const POOL_CHECK_INTERVAL_SECS: u64 = 30;
/// Periodic settle (§16.4: "or every 15 min").
pub const SETTLE_PERIOD_SECS: i64 = 900;
/// Periodic settles skip losses below $1 (not worth a transaction).
pub const MIN_SETTLE_LOSS: u64 = 1_000_000;
pub const MAX_PERIODIC_SETTLES: usize = 10;

/// The loss `settle_position_pnl` would move into cash, if it accepts the
/// position: `P = pnl(mark) + accrued_funding(F) < 0` (`NothingToSettle`)
/// and `−P < C` (`SettleWouldBankrupt`). It does not crank funding, so `m`
/// is the market as stored.
pub fn settleable_loss(m: &MarketData, p: &PositionData, mark: u64) -> Option<u64> {
    let pnl = p.pnl_with_funding(mark, m.cumulative_funding);
    if pnl >= 0 {
        return None;
    }
    let loss = u64::try_from(-pnl).ok()?;
    (loss < p.collateral).then_some(loss)
}

/// Σ positive realized P (incl. funding) at `mark`: what the pool would pay
/// out if every winner closed now, before haircuts.
pub fn winners_profit(m: &MarketData, positions: &[(Pubkey, PositionData)], mark: u64) -> u128 {
    positions.iter().map(|(_, p)| p.pnl_with_funding(mark, m.cumulative_funding).max(0) as u128).sum()
}

/// Losers to settle, largest loss first. When cash is short of the winners'
/// profit, as many as cover the gap (any size); otherwise, when `periodic`,
/// up to `MAX_PERIODIC_SETTLES` of at least `MIN_SETTLE_LOSS`.
pub fn pick_losers_to_settle(m: &MarketData, pool: &LpPoolData, positions: &[(Pubkey, PositionData)], mark: u64, periodic: bool) -> Vec<(Pubkey, u64)> {
    let mut losers: Vec<(Pubkey, u64)> = positions.iter().filter_map(|(k, p)| settleable_loss(m, p, mark).map(|l| (*k, l))).collect();
    losers.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let need = winners_profit(m, positions, mark).saturating_sub(pool.cash as u128);
    if need > 0 {
        let mut got = 0u128;
        let mut out = Vec::new();
        for l in losers {
            if got >= need {
                break;
            }
            got += l.1 as u128;
            out.push(l);
        }
        return out;
    }
    if periodic {
        losers.retain(|l| l.1 >= MIN_SETTLE_LOSS);
        losers.truncate(MAX_PERIODIC_SETTLES);
        return losers;
    }
    Vec::new()
}

/// `close_settled_position` order on a settled market: every loser
/// (`P ≤ 0` at the settlement price), most negative first, so bankrupt
/// positions book their deficits (and the D13 haircut) before any winner is
/// paid; winners, smallest first, only once no loser is left and the quiet
/// time after settlement is over (D35). Winners never fail on cash then:
/// with every loser closed, `U ≥ P` and the solvency haircut caps each
/// payout at cash.
pub fn settled_close_order(m: &MarketData, pool: &LpPoolData, positions: &[(Pubkey, PositionData)], now: i64) -> Vec<(Pubkey, Pubkey)> {
    if !m.is_settled() {
        return Vec::new();
    }
    let mark = m.settlement_price;
    let mut ps: Vec<(i128, Pubkey, Pubkey)> =
        positions.iter().map(|(k, p)| (p.pnl_with_funding(mark, m.cumulative_funding), *k, p.owner)).collect();
    ps.sort();
    let losers: Vec<(Pubkey, Pubkey)> = ps.iter().filter(|x| x.0 <= 0).map(|x| (x.1, x.2)).collect();
    if !losers.is_empty() {
        return losers;
    }
    if in_profit_close_quiet(m, pool, now) {
        return Vec::new();
    }
    ps.iter().map(|x| (x.1, x.2)).collect()
}

// ---------------------------------------------------------------------------
// Loop
// ---------------------------------------------------------------------------

pub async fn run_pool_keeper(client: Arc<RpcClient>, config: Arc<KeeperConfig>) {
    let every = crate::env_secs("KEEPER_POOL_CHECK_SECS", POOL_CHECK_INTERVAL_SECS);
    let backoff = crate::env_secs("KEEPER_ERROR_BACKOFF_SECS", 60);
    let settle_period = crate::env_secs("KEEPER_SETTLE_PERIOD_SECS", SETTLE_PERIOD_SECS as u64) as i64;
    info!("Pool keeper started (every {} s, periodic settle every {} s)", every, settle_period);
    let mut tick = interval(Duration::from_secs(every));
    let last_settle: Mutex<HashMap<Pubkey, i64>> = Mutex::new(HashMap::new());
    let mut consecutive_errors = 0;
    loop {
        tick.tick().await;
        let result = for_each_market(&client, &config, |commodity, pda, market| pool_market(&client, &config, &last_settle, settle_period, commodity, pda, market));
        match result {
            Ok(n) => {
                consecutive_errors = 0;
                if n > 0 {
                    info!("Pool keeper sent {} transactions", n);
                }
            }
            Err(e) => {
                consecutive_errors += 1;
                error!("Pool check error (attempt {}): {}", consecutive_errors, e);
                if consecutive_errors >= 5 {
                    warn!("Too many consecutive errors, backing off...");
                    tokio::time::sleep(Duration::from_secs(backoff)).await;
                    consecutive_errors = 0;
                }
            }
        }
    }
}

fn pool_market(
    client: &RpcClient,
    config: &KeeperConfig,
    last_settle: &Mutex<HashMap<Pubkey, i64>>,
    settle_period: i64,
    commodity: &str,
    market_pda: Pubkey,
    market: MarketData,
) -> KeeperResult<u32> {
    let program = config.perps_program_id;
    let signer = config.keypair.pubkey();
    let Some(pool) = fetch_lp_pool(client, &program, &market_pda)? else {
        debug!("{} has no LP pool", commodity);
        return Ok(0);
    };
    let now = chain_now(client)?;
    let mut sent = 0u32;

    if market.is_settled() {
        let positions = fetch_open_positions(client, &program, &market_pda)?;
        for (position, owner) in settled_close_order(&market, &pool, &positions, now) {
            let ix = ix::close_settled_position(&program, &signer, &market_pda, &position, &owner);
            match ix::send(client, &config.keypair, ix) {
                Ok(sig) => {
                    info!("{} close_settled_position {}: {}", commodity, position, sig);
                    sent += 1;
                }
                Err(e) => {
                    let e = e.to_string();
                    if ix::has_program_error(&e, ERR_POOL_CASH_INSUFFICIENT) {
                        // Should not happen once losers are closed; settle
                        // any that are left and retry next pass
                        warn!("{} close_settled_position {} short of cash; settling losers", commodity, position);
                        sent += settle_losers(client, config, commodity, market_pda, &market, &pool, &positions, market.settlement_price, false);
                    } else {
                        error!("{} close_settled_position {} failed: {}", commodity, position, e);
                    }
                    break;
                }
            }
        }
    } else if market.is_in_session_for(now, crate::SESSION_EDGE_GUARD_SECS) {
        let mark = crate::oracle::fetch_oracle_price(client, &market, now)?;
        let positions = fetch_open_positions(client, &program, &market_pda)?;
        let periodic = {
            let map = last_settle.lock().unwrap();
            map.get(&market_pda).is_none_or(|t| now - t >= settle_period)
        };
        let n = settle_losers(client, config, commodity, market_pda, &market, &pool, &positions, mark, periodic);
        if periodic {
            last_settle.lock().unwrap().insert(market_pda, now);
        }
        sent += n;
    }

    // Sweep last: closes above may have emptied a settled market. Re-read.
    let (market, pool) = match (crate::liquidator::fetch_market(client, config, commodity)?, fetch_lp_pool(client, &program, &market_pda)?) {
        (Some((_, m)), Some(p)) => (m, p),
        _ => return Ok(sent),
    };
    let x = sweep_amount(&market, &pool);
    if x > 0 {
        match ix::send(client, &config.keypair, ix::sweep_insurance_excess(&program, &signer, &market_pda)) {
            Ok(sig) => {
                info!("{} sweep_insurance_excess {}: {}", commodity, x, sig);
                sent += 1;
            }
            Err(e) => error!("{} sweep_insurance_excess failed: {}", commodity, e),
        }
    }
    Ok(sent)
}

#[allow(clippy::too_many_arguments)]
fn settle_losers(
    client: &RpcClient,
    config: &KeeperConfig,
    commodity: &str,
    market_pda: Pubkey,
    market: &MarketData,
    pool: &LpPoolData,
    positions: &[(Pubkey, PositionData)],
    mark: u64,
    periodic: bool,
) -> u32 {
    let program = config.perps_program_id;
    let signer = config.keypair.pubkey();
    let mut sent = 0;
    for (position, loss) in pick_losers_to_settle(market, pool, positions, mark, periodic) {
        let ix = ix::settle_position_pnl(&program, &signer, &market_pda, &position, &market.pyth_price_feed);
        match ix::send(client, &config.keypair, ix) {
            Ok(sig) => {
                info!("{} settle_position_pnl {} (loss {}): {}", commodity, position, loss, sig);
                sent += 1;
            }
            Err(e) if ix::is_settle_race(&e.to_string()) => {
                info!("{} settle_position_pnl {} skipped: the position changed since the scan", commodity, position)
            }
            Err(e) => error!("{} settle_position_pnl {} failed: {}", commodity, position, e),
        }
    }
    sent
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{SIDE_LONG, SIDE_SHORT};
    use crate::testkit::*;

    #[test]
    fn settleable_loss_mirrors_settle_pnl() {
        // adl.rs `settle_pnl_is_zero_sum_and_keeps_equity`: long 10 @ $75
        // with $150, funding index 1_234, at $70: $50 + $0.01234
        let (mut m, mut pool) = (market(), pool());
        let p = open(&mut m, &mut pool, SIDE_LONG, 10 * C6, 75 * C6, 150 * C6);
        m.cumulative_funding = 1_234;
        assert_eq!(settleable_loss(&m, &p, 70 * C6), Some(50 * C6 + 12_340));
        // Winners and flat positions: NothingToSettle
        assert_eq!(settleable_loss(&m, &p, 80 * C6), None);
        // A loss ≥ collateral: SettleWouldBankrupt (the liquidator's job);
        // `settle_pnl_rejects_a_loss_equal_to_collateral`
        m.cumulative_funding = 0;
        let q = open(&mut m, &mut pool, SIDE_LONG, 10 * C6, 75 * C6, 50 * C6);
        assert_eq!(settleable_loss(&m, &q, 70 * C6), None);
        assert_eq!(settleable_loss(&m, &q, 70 * C6 + 1), Some(49_999_990));
    }

    #[test]
    fn settles_the_largest_losers_when_cash_is_short() {
        let (mut m, mut pool) = (market(), pool());
        pool.cash = 100 * C6;
        // Winners: two shorts up $300 and $200 at $70
        let w1 = open(&mut m, &mut pool, SIDE_SHORT, 60 * C6, 75 * C6, 1_000 * C6);
        let w2 = open(&mut m, &mut pool, SIDE_SHORT, 40 * C6, 75 * C6, 1_000 * C6);
        // Losers: $250, $150 and $100 down; one bankrupt one ($200 on $100)
        let l1 = open(&mut m, &mut pool, SIDE_LONG, 50 * C6, 75 * C6, 1_000 * C6);
        let l2 = open(&mut m, &mut pool, SIDE_LONG, 30 * C6, 75 * C6, 1_000 * C6);
        let l3 = open(&mut m, &mut pool, SIDE_LONG, 20 * C6, 75 * C6, 1_000 * C6);
        let bust = open(&mut m, &mut pool, SIDE_LONG, 40 * C6, 75 * C6, 100 * C6);
        let positions = keyed(&[w1, w2, l1, l2, l3, bust]);
        let mark = 70 * C6;
        assert_eq!(winners_profit(&m, &positions, mark), 500 * C6 as u128);
        // Short by $400: $250 + $150
        let picked = pick_losers_to_settle(&m, &pool, &positions, mark, false);
        assert_eq!(picked, vec![(positions[2].0, 250 * C6), (positions[3].0, 150 * C6)]);
        // Enough cash and not periodic: nothing
        pool.cash = 500 * C6;
        assert!(pick_losers_to_settle(&m, &pool, &positions, mark, false).is_empty());
        // Periodic: every settleable loser, largest first (never the bankrupt one)
        let picked = pick_losers_to_settle(&m, &pool, &positions, mark, true);
        assert_eq!(picked.iter().map(|x| x.1).collect::<Vec<_>>(), vec![250 * C6, 150 * C6, 100 * C6]);
    }

    #[test]
    fn periodic_settles_skip_dust_and_cap_the_count() {
        let (mut m, mut pool) = (market(), pool());
        let mut ps = Vec::new();
        for i in 0..15u64 {
            ps.push(open(&mut m, &mut pool, SIDE_LONG, C6, 75 * C6 + i, 10 * C6));
        }
        let positions = keyed(&ps);
        // $0.50 + i·1e-6 of loss each at $74.50: all below $1
        assert!(pick_losers_to_settle(&m, &pool, &positions, 74_500_000, true).is_empty());
        // $2 each at $73: ten of them
        assert_eq!(pick_losers_to_settle(&m, &pool, &positions, 73 * C6, true).len(), MAX_PERIODIC_SETTLES);
    }

    #[test]
    fn settled_markets_close_losers_first_then_winners_after_the_quiet_time() {
        let (mut m, mut pool) = (market(), pool());
        pool.params.profit_close_quiet_secs = 300;
        let w = open(&mut m, &mut pool, SIDE_SHORT, 10 * C6, 75 * C6, 150 * C6);
        let l = open(&mut m, &mut pool, SIDE_LONG, 10 * C6, 75 * C6, 150 * C6);
        let bust = open(&mut m, &mut pool, SIDE_LONG, 10 * C6, 80 * C6, 50 * C6);
        let flat = open(&mut m, &mut pool, SIDE_SHORT, 10 * C6, 70 * C6, 50 * C6);
        let positions = keyed(&[w, l, bust, flat]);
        // Not settled: nothing
        assert!(settled_close_order(&m, &pool, &positions, 1).is_empty());
        m.settlement_price = 70 * C6;
        m.settled_at = 1_000;
        // Losers: −$100 (bankrupt), −$50, then the flat one (P = 0)
        let order = settled_close_order(&m, &pool, &positions, 1_000);
        assert_eq!(order.iter().map(|x| x.0).collect::<Vec<_>>(), vec![positions[2].0, positions[1].0, positions[3].0]);
        assert_eq!(order[0].1, positions[2].1.owner);
        // With the losers gone, the winner waits out the quiet time
        let winners = keyed(&[positions[0].1.clone()]);
        assert!(settled_close_order(&m, &pool, &winners, 1_299).is_empty());
        assert_eq!(settled_close_order(&m, &pool, &winners, 1_300).len(), 1);
        // Settled before settled_at existed: no quiet time
        m.settled_at = 0;
        assert_eq!(settled_close_order(&m, &pool, &winners, 1_000).len(), 1);
    }

    /// With every loser closed, each winner's payout fits cash (the claim in
    /// `settled_close_order`'s doc): U ≥ P, so the solvency haircut caps it.
    #[test]
    fn winners_alone_never_fail_on_cash() {
        use crate::mirror::{pool_view, quote_profit};
        let (mut m, mut pool) = (market(), pool());
        pool.cash = 7 * C6;
        let a = open(&mut m, &mut pool, SIDE_SHORT, 10 * C6, 75 * C6, 150 * C6);
        let b = open(&mut m, &mut pool, SIDE_SHORT, 30 * C6, 72 * C6, 150 * C6);
        m.settlement_price = 70 * C6;
        let v = pool_view(&pool, &m, 70 * C6, 1).unwrap();
        for p in [a, b] {
            let pnl = p.pnl_with_funding(70 * C6, 0) as u64;
            let q = quote_profit(&pool, &v, p.side, pnl, false).unwrap();
            assert!(q.paid <= pool.cash && q.solvency_hc > 0);
        }
    }
}
