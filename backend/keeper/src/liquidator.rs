use crate::ix::{self, KeeperResult};
use crate::mirror;
use crate::oracle::fetch_oracle_price;
use crate::state::decode_account;
pub use crate::state::{anchor_discriminator, LpPoolData, MarketData, PositionData, STATUS_OPEN};
use crate::{get_lp_pool_pda, get_market_pda, KeeperConfig};
use borsh::BorshDeserialize;
use solana_client::rpc_client::RpcClient;
use solana_client::rpc_config::{RpcAccountInfoConfig, RpcProgramAccountsConfig};
use solana_client::rpc_filter::{Memcmp, RpcFilterType};
use solana_sdk::{pubkey::Pubkey, signature::Signer};
use spl_associated_token_account::address::get_associated_token_address;
use std::{sync::Arc, time::Duration};
use tokio::time::interval;
use tracing::{info, error, warn, debug};

const LIQUIDATION_CHECK_INTERVAL_SECS: u64 = 10;

/// Liquidations sent in one pass at most (the rest wait for the next pass).
pub const MAX_LIQUIDATIONS_PER_PASS: usize = 64;
/// How long a pass waits for its batch of liquidations to confirm.
const LIQUIDATION_CONFIRM_TIMEOUT: Duration = Duration::from_secs(30);

/// `Clock::unix_timestamp` at the client's commitment: the time every session,
/// margin and funding decision on-chain uses. It trails the wall clock (by
/// ~1–1.3 s on a local validator), so deciding by wall time sends
/// `settle_position_pnl` into `MarketClosed` and `update_funding` into
/// `FundingNotDue` around session edges.
pub fn chain_now(client: &RpcClient) -> KeeperResult<i64> {
    let account = client
        .get_account_with_commitment(&solana_sdk::sysvar::clock::id(), client.commitment())?
        .value
        .ok_or("clock sysvar missing")?;
    clock_unix_timestamp(&account.data).ok_or_else(|| "clock sysvar too short".into())
}

/// `unix_timestamp` from the Clock sysvar's bincode layout (slot,
/// epoch_start_timestamp, epoch, leader_schedule_epoch, unix_timestamp).
pub fn clock_unix_timestamp(data: &[u8]) -> Option<i64> {
    Some(i64::from_le_bytes(data.get(32..40)?.try_into().ok()?))
}

pub async fn run_liquidation_keeper(client: Arc<RpcClient>, config: Arc<KeeperConfig>) {
    let every = crate::env_secs("KEEPER_LIQUIDATION_CHECK_SECS", LIQUIDATION_CHECK_INTERVAL_SECS);
    let backoff = crate::env_secs("KEEPER_ERROR_BACKOFF_SECS", 60);
    info!("Liquidation keeper started");
    info!("Checking for liquidatable positions every {} seconds", every);

    let mut check_interval = interval(Duration::from_secs(every));
    let mut consecutive_errors = 0;

    loop {
        check_interval.tick().await;

        match check_and_liquidate(&client, &config).await {
            Ok(liquidated) => {
                consecutive_errors = 0;
                if liquidated > 0 {
                    info!("Successfully liquidated {} positions", liquidated);
                }
            }
            Err(e) => {
                consecutive_errors += 1;
                error!("Liquidation check error (attempt {}): {}", consecutive_errors, e);

                if consecutive_errors >= 5 {
                    warn!("Too many consecutive errors, backing off...");
                    tokio::time::sleep(Duration::from_secs(backoff)).await;
                    consecutive_errors = 0;
                }
            }
        }
    }
}

/// Fetch and decode a commodity's market, or `None` if it isn't initialized.
pub fn fetch_market(client: &RpcClient, config: &KeeperConfig, commodity: &str) -> KeeperResult<Option<(Pubkey, MarketData)>> {
    let (market_pda, _) = get_market_pda(&config.perps_program_id, &config.usdc_mint, commodity);
    let Some(account) = client.get_account_with_commitment(&market_pda, client.commitment())?.value else {
        return Ok(None);
    };
    let market = decode_market(&account.owner, &account.data, &config.perps_program_id);
    if market.is_none() && account.owner == config.perps_program_id {
        // A program-owned account at the market address that does not decode:
        // most likely a program with another Market layout (LEN 434 since D47)
        warn!("{} market {} did not decode ({} bytes); skipping", commodity, market_pda, account.data.len());
    }
    Ok(market.map(|m| (market_pda, m)))
}

/// Decode a Market account, or `None` if it isn't one: anyone can send
/// lamports to an unused market address, so check owner, size and type first.
fn decode_market(owner: &Pubkey, data: &[u8], program_id: &Pubkey) -> Option<MarketData> {
    decode_account(owner, data, program_id, "Market")
}

/// Fetch and decode a market's LP pool (`[b"lp_pool", market]`), or `None`
/// if it has none yet.
pub fn fetch_lp_pool(client: &RpcClient, program_id: &Pubkey, market: &Pubkey) -> KeeperResult<Option<LpPoolData>> {
    let pda = get_lp_pool_pda(program_id, market).0;
    let Some(account) = client.get_account_with_commitment(&pda, client.commitment())?.value else {
        return Ok(None);
    };
    Ok(decode_account(&account.owner, &account.data, program_id, "LpPool"))
}

/// Run `f` for every configured market. Errors in one market are logged and
/// don't stop the others; the call fails only if every market failed.
pub fn for_each_market<T: Default + std::ops::AddAssign>(
    client: &RpcClient,
    config: &KeeperConfig,
    mut f: impl FnMut(&str, Pubkey, MarketData) -> KeeperResult<T>,
) -> KeeperResult<T> {
    let mut total = T::default();
    let mut failures = 0;
    let mut last_error = None;
    for commodity in &config.commodities {
        let result = fetch_market(client, config, commodity).and_then(|found| match found {
            Some((pda, market)) => f(commodity, pda, market),
            None => {
                debug!("{} market not initialized, skipping", commodity);
                Ok(T::default())
            }
        });
        match result {
            Ok(v) => total += v,
            Err(e) => {
                error!("{} market: {}", commodity, e);
                failures += 1;
                last_error = Some(e);
            }
        }
    }
    match last_error {
        Some(e) if failures == config.commodities.len() => Err(e),
        _ => Ok(total),
    }
}

async fn check_and_liquidate(client: &RpcClient, config: &KeeperConfig) -> KeeperResult<u32> {
    let mut work = Vec::new();
    let found = for_each_market(client, config, |commodity, pda, market| {
        work.push((commodity.to_string(), pda, market));
        Ok(0u32)
    });
    found?;

    let mut liquidated = 0;
    for (commodity, market_pda, market_data) in work {
        match check_market(client, config, &commodity, market_pda, market_data).await {
            Ok(n) => liquidated += n,
            Err(e) => error!("{} liquidation check failed: {}", commodity, e),
        }
    }
    Ok(liquidated)
}

async fn check_market(
    client: &RpcClient,
    config: &KeeperConfig,
    commodity: &str,
    market_pda: Pubkey,
    market_data: MarketData,
) -> KeeperResult<u32> {
    debug!("Checking {} market {}", commodity, market_pda);

    // Pausing only stops new positions; the program still accepts liquidations
    // and funding updates, and skipping them would let bad debt build up.

    let now = chain_now(client)?;
    if market_data.is_settled() {
        debug!("Market is settled, no liquidations");
        return Ok(0);
    }
    if !market_data.is_in_session(now) {
        debug!("Market is outside trading hours, skipping liquidation check");
        return Ok(0);
    }
    // Fetch oracle price
    let oracle_price = fetch_oracle_price(client, &market_data, now)?;
    debug!("Current oracle price: ${:.2}", oracle_price as f64 / 1_000_000.0);

    // Find all open positions
    let open_positions = fetch_open_positions(client, &config.perps_program_id, &market_pda)?;
    debug!("Found {} open positions", open_positions.len());

    // Worst first, all sent at once and confirmed together: after a reopen gap
    // owners race the keeper to close bankrupt positions
    let mut candidates = liquidatable(&market_data, &open_positions, oracle_price, now);
    if candidates.len() > MAX_LIQUIDATIONS_PER_PASS {
        warn!("{} liquidatable positions; sending the worst {} this pass", candidates.len(), MAX_LIQUIDATIONS_PER_PASS);
        candidates.truncate(MAX_LIQUIDATIONS_PER_PASS);
    }
    if candidates.is_empty() {
        return Ok(0);
    }
    let mmr = market_data.maintenance_margin_ratio_at(now);
    let liquidator_token_account = get_associated_token_address(&config.keypair.pubkey(), &market_data.collateral_mint);
    let mut ixs = Vec::with_capacity(candidates.len());
    for (position_address, position) in &candidates {
        info!(
            "Found liquidatable position: {} (mark margin: {}%, entry margin: {}%, required: {}%)",
            position_address,
            mark_margin_ratio(position, oracle_price, market_data.cumulative_funding) as f64 / 100.0,
            calculate_margin_ratio(position, oracle_price, market_data.cumulative_funding) as f64 / 100.0,
            mmr as f64 / 100.0
        );
        ixs.push(ix::liquidate(
            &config.perps_program_id,
            &config.keypair.pubkey(),
            &position.owner,
            &market_pda,
            position_address,
            &liquidator_token_account,
            &market_data.pyth_price_feed,
        ));
    }

    let mut liquidated_count = 0;
    for ((position_address, _), result) in candidates.iter().zip(ix::send_batch(client, &config.keypair, ixs, LIQUIDATION_CONFIRM_TIMEOUT)) {
        match result {
            // After a stage-1 slice the position stays Open with a smaller
            // size; the next pass re-checks it (§16.4)
            Ok(sig) => {
                liquidated_count += 1;
                info!("Liquidation successful for position {}: {}", position_address, sig);
            }
            Err(e) if is_liquidation_race(&e.to_string()) => {
                info!("Position {} was closed or recovered before its liquidation landed; skipped", position_address);
            }
            Err(e) => error!("Failed to liquidate position {}: {}", position_address, e),
        }
    }

    Ok(liquidated_count)
}

/// A refusal that only means someone got there first: the owner (or another
/// liquidator) closed the position, or a price move or margin top-up since
/// the scan lifted it back over maintenance.
pub fn is_liquidation_race(err: &str) -> bool {
    ix::has_program_error(err, ix::ERR_POSITION_ALREADY_CLOSED) || ix::has_program_error(err, ix::ERR_NOT_LIQUIDATABLE)
}

/// The positions `liquidate` accepts at `mark` and `now` (§6.1): equity on
/// **mark** notional below the maintenance margin in force, with funding
/// cranked first as the instruction does (D40).
pub fn liquidatable(market: &MarketData, positions: &[(Pubkey, PositionData)], mark: u64, now: i64) -> Vec<(Pubkey, PositionData)> {
    if market.is_settled() || !market.is_in_session(now) {
        return Vec::new();
    }
    let m = mirror::project_funding(market, mark, now);
    let mmr = m.maintenance_margin_ratio_at(now);
    let mut out: Vec<(Pubkey, PositionData)> = positions
        .iter()
        .filter(|(_, p)| p.is_liquidatable(mark, p.accrued_funding(m.cumulative_funding), mmr))
        .cloned()
        .collect();
    // Worst first: most negative equity (the largest deficit if an owner's
    // close or a further move gets there first), then the larger notional
    out.sort_by(|(ka, a), (kb, b)| {
        a.equity(mark, m.cumulative_funding)
            .cmp(&b.equity(mark, m.cumulative_funding))
            .then(b.mark_notional(mark).cmp(&a.mark_notional(mark)))
            .then(ka.cmp(kb))
    });
    out
}

/// Equity over mark notional, bps (`Position::margin_ratio_mark`, display).
fn mark_margin_ratio(position: &PositionData, mark: u64, cumulative_funding: i64) -> u32 {
    let equity = position.equity(mark, cumulative_funding);
    let n = position.mark_notional(mark);
    if equity <= 0 || n == 0 {
        return 0;
    }
    (equity as u128 * 10_000 / n).min(u32::MAX as u128) as u32
}

/// Offset of `Position::market` (after the discriminator and owner).
const POSITION_MARKET_OFFSET: usize = 8 + 32;

pub fn fetch_open_positions(client: &RpcClient, program_id: &Pubkey, market: &Pubkey) -> KeeperResult<Vec<(Pubkey, PositionData)>> {
    // Open positions in this market only
    let filters = vec![
        RpcFilterType::Memcmp(Memcmp::new_raw_bytes(0, anchor_discriminator("account", "Position").to_vec())),
        RpcFilterType::Memcmp(Memcmp::new_raw_bytes(POSITION_MARKET_OFFSET, market.to_bytes().to_vec())),
        // Status at offset 8 + 32 + 32 + 1 + 8 + 8 + 8 + 4 + 8 + 8 + 8 + 8 = 133
        RpcFilterType::Memcmp(Memcmp::new_raw_bytes(133, vec![0])), // Open status
    ];

    let config = RpcProgramAccountsConfig {
        filters: Some(filters),
        account_config: RpcAccountInfoConfig {
            encoding: Some(solana_account_decoder::UiAccountEncoding::Base64),
            ..Default::default()
        },
        ..Default::default()
    };

    let accounts = client.get_program_ui_accounts_with_config(program_id, config)?;

    let mut positions = Vec::new();
    for (pubkey, account) in accounts {
        // Base64 always decodes; failing the scan otherwise is what the
        // deprecated `get_program_accounts_with_config` did.
        let data = account.data.decode().ok_or("getProgramAccounts: undecodable account data")?;
        let Some(data) = data.get(8..) else { continue };
        if let Ok(position) = PositionData::deserialize(&mut &data[..]) {
            if position.status == STATUS_OPEN {
                positions.push((pubkey, position));
            }
        }
    }

    Ok(positions)
}

/// Margin ratio in basis points on **entry** notional (`Position::margin_ratio`),
/// kept for logging; liquidation uses mark notional (`liquidatable`).
fn calculate_margin_ratio(position: &PositionData, current_price: u64, cumulative_funding: i64) -> u32 {
    let entry = position.entry_price as i128;
    let current = current_price as i128;
    let size = position.size as i128;

    let price_diff = if position.side == 0 {
        // Long
        current - entry
    } else {
        // Short
        entry - current
    };

    let pnl = (size * price_diff) / 1_000_000;
    let funding = position.accrued_funding(cumulative_funding) as i128;
    let equity = (position.collateral as i128) + pnl + funding;

    if equity <= 0 {
        return 0;
    }

    let notional = (size * entry) / 1_000_000;
    if notional == 0 {
        return 0;
    }

    ((equity * 10000) / notional).min(u32::MAX as i128) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session;

    fn oil_long() -> PositionData {
        PositionData {
            side: 0,
            size: 100_000_000,        // 100 contracts
            collateral: 750_000_000,  // $750
            entry_price: 75_000_000,  // $75
            ..Default::default()
        }
    }

    #[test]
    fn discriminators_match_anchor() {
        assert_eq!(anchor_discriminator("global", "liquidate"), [223, 179, 226, 125, 48, 46, 39, 74]);
        assert_eq!(anchor_discriminator("global", "update_funding"), [224, 66, 9, 70, 75, 81, 94, 88]);
        assert_eq!(anchor_discriminator("account", "Position"), [170, 188, 143, 228, 122, 64, 247, 208]);
    }

    #[test]
    fn market_data_decodes_on_chain_layout() {
        // Serialize fields in perps-core's Market order and check they land in
        // the right MarketData fields.
        let mut bytes = Vec::new();
        for key in 1u8..=4 {
            bytes.extend_from_slice(&[key; 32]); // authority, collateral_mint, vault, pyth_price_feed
        }
        bytes.extend_from_slice(b"OIL\0\0\0\0\0");
        for v in [20_000u32, 500, 1_000, 5, 2, 100] {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        for v in [11u64, 22, 33] {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        for v in [-7i64, 1_700_000_000, 3_600] {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        for v in [44u64, 55, 66] {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        bytes.push(254); // bump
        bytes.push(1); // is_paused
        bytes.push(1); // session_schedule
        for v in [7_200u32, 2_000, 1_000, 150] {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        bytes.extend_from_slice(&5_000_000u64.to_le_bytes()); // min_oracle_price
        bytes.extend_from_slice(&0u64.to_le_bytes()); // settlement_price
        bytes.extend_from_slice(&(-3i64).to_le_bytes()); // funding_rate
        bytes.extend_from_slice(&5_400u32.to_le_bytes()); // funding_carry_secs
        bytes.extend_from_slice(&1_800_000_000i64.to_le_bytes()); // settled_at
        bytes.extend_from_slice(&[0x92; 32]); // pyth_feed_id
        bytes.extend_from_slice(&[0x50; 32]); // oracle_program
        bytes.extend_from_slice(&[0x61; 32]); // pending_price_feed
        bytes.extend_from_slice(&[0x62; 32]); // pending_feed_id
        bytes.extend_from_slice(&1_800_864_000i64.to_le_bytes()); // pending_oracle_at
        bytes.extend_from_slice(&[0u8; 3]); // padding
        assert_eq!(8 + bytes.len(), 434); // Market::LEN

        let m = MarketData::deserialize(&mut bytes.as_slice()).unwrap();
        assert_eq!(m.vault, Pubkey::new_from_array([3; 32]));
        assert_eq!(m.pyth_price_feed, Pubkey::new_from_array([4; 32]));
        assert_eq!((m.maintenance_margin_ratio, m.initial_margin_ratio), (500, 1_000));
        assert_eq!((m.cumulative_funding, m.funding_rate), (-7, -3));
        assert_eq!((m.last_funding_time, m.funding_interval), (1_700_000_000, 3_600));
        assert_eq!(m.insurance_fund, 44);
        assert_eq!((m.bump, m.is_paused), (254, true));
        assert_eq!(m.session_schedule, 1);
        assert_eq!((m.weekend_initial_margin_ratio, m.weekend_maintenance_margin_ratio), (2_000, 1_000));
        assert_eq!((m.max_confidence_bps, m.min_oracle_price, m.settlement_price), (150, 5_000_000, 0));
        assert_eq!((m.funding_carry_secs, m.settled_at), (5_400, 1_800_000_000));
        assert_eq!((m.pyth_feed_id, m.oracle_program), ([0x92; 32], Pubkey::new_from_array([0x50; 32])));
        assert_eq!((m.pending_price_feed, m.pending_feed_id), (Pubkey::new_from_array([0x61; 32]), [0x62; 32]));
        assert_eq!(m.pending_oracle_at, 1_800_864_000);
    }

    #[test]
    fn ignores_accounts_that_are_not_markets() {
        let program = Pubkey::new_unique();
        let mut data = anchor_discriminator("account", "Market").to_vec();
        data.extend(vec![0u8; 434 - 8]);
        assert!(decode_market(&program, &data, &program).is_some());
        // Lamports sent to an unused market address: system-owned, empty
        assert!(decode_market(&Pubkey::default(), &[], &program).is_none());
        // Program-owned but too short, or another account type
        assert!(decode_market(&program, &data[..5], &program).is_none());
        let mut other = anchor_discriminator("account", "Position").to_vec();
        other.extend(vec![0u8; 434 - 8]);
        assert!(decode_market(&program, &other, &program).is_none());
        // A market in the layout before D47 (LEN 298) is not one any more
        assert!(decode_market(&program, &data[..298], &program).is_none());
    }

    #[test]
    fn markets_created_before_risk_fields_decode_as_defaults() {
        // A market created before the risk fields existed has zeros there.
        let mut bytes = vec![0u8; 434 - 8];
        bytes[4 * 32 + 8..4 * 32 + 12].copy_from_slice(&20_000u32.to_le_bytes());
        let m = MarketData::deserialize(&mut bytes.as_slice()).unwrap();
        assert_eq!(m.max_leverage, 20_000);
        assert_eq!((m.session_schedule, m.settlement_price), (0, 0));
        assert!(m.is_in_session(1_794_607_200)); // always open
    }

    #[test]
    fn keeper_uses_weekend_maintenance_margin_before_close() {
        let m = MarketData {
            maintenance_margin_ratio: 500,
            session_schedule: session::SCHEDULE_CME_GLOBEX,
            pre_close_window_secs: 7_200,
            weekend_maintenance_margin_ratio: 1_000,
            ..Default::default()
        };
        let friday_close = 1_794_607_200; // 2026-11-13 17:00 ET
        assert_eq!(m.maintenance_margin_ratio_at(friday_close - 7_201), 500);
        assert_eq!(m.maintenance_margin_ratio_at(friday_close - 60), 1_000);
        assert!(!m.is_in_session(friday_close));
    }

    #[test]
    fn position_market_offset_matches_layout() {
        let market = Pubkey::new_unique();
        let p = PositionData { owner: Pubkey::new_unique(), market, ..Default::default() };
        let mut bytes = vec![0u8; 8];
        bytes.extend(borsh::to_vec(&p).unwrap());
        assert_eq!(&bytes[POSITION_MARKET_OFFSET..POSITION_MARKET_OFFSET + 32], market.as_ref());
    }

    /// After a reopen gap the keeper sends the worst positions first (it used
    /// to go in RPC/pubkey order and lose the bankrupt ones to their owners).
    #[test]
    fn liquidatable_is_worst_first() {
        let m = MarketData { maintenance_margin_ratio: 500, funding_interval: 3_600, last_funding_time: 1_000, ..Default::default() };
        let long = |size: u64, collateral: u64| PositionData { side: 0, size, collateral, entry_price: 75_000_000, opened_price: 75_000_000, ..Default::default() };
        // At $70 each 100 contracts lose $500: equity +$100, −$300, −$100,
        // and 200 contracts with $900: −$100 on twice the notional
        let a = (Pubkey::new_from_array([1; 32]), long(100_000_000, 600_000_000));
        let b = (Pubkey::new_from_array([2; 32]), long(100_000_000, 200_000_000));
        let c = (Pubkey::new_from_array([3; 32]), long(100_000_000, 400_000_000));
        let d = (Pubkey::new_from_array([4; 32]), long(200_000_000, 900_000_000));
        let healthy = (Pubkey::new_from_array([5; 32]), long(100_000_000, 5_000_000_000));
        let order: Vec<Pubkey> = liquidatable(&m, &[a.clone(), b.clone(), c.clone(), d.clone(), healthy], 70_000_000, 1_000).into_iter().map(|x| x.0).collect();
        // −$300 first; the two −$100 ones larger notional first; +$100 last
        assert_eq!(order, vec![b.0, d.0, c.0, a.0]);
    }

    #[test]
    fn liquidation_races_are_not_errors() {
        assert!(is_liquidation_race("RPC response error -32002: Transaction simulation failed: Error processing Instruction 0: custom program error: 0x177c; 5 log messages"));
        assert!(is_liquidation_race("5Kx..: Error processing Instruction 0: custom program error: 0x1777"));
        // MarketClosed (0x178b), StaleOraclePrice (0x1779) and transport errors are real failures
        assert!(!is_liquidation_race("Error processing Instruction 0: custom program error: 0x178b"));
        assert!(!is_liquidation_race("Error processing Instruction 0: custom program error: 0x1779"));
        assert!(!is_liquidation_race("error sending request for url (http://127.0.0.1:8899/)"));
    }

    #[test]
    fn reads_the_clock_sysvar_timestamp() {
        let clock = solana_sdk::clock::Clock { slot: 9, epoch_start_timestamp: 1, epoch: 2, leader_schedule_epoch: 3, unix_timestamp: 1_795_606_965 };
        let data = solana_sdk::account::create_account_for_test(&clock).data;
        assert_eq!(clock_unix_timestamp(&data), Some(1_795_606_965));
        assert_eq!(clock_unix_timestamp(&data[..39]), None);
    }

    /// Settles and funding cranks skip the seconds before a close (they
    /// land a slot later and would fail MarketClosed there).
    #[test]
    fn session_guard_skips_the_seconds_before_a_close() {
        let m = MarketData { session_schedule: session::SCHEDULE_TEST_HOURLY, ..Default::default() };
        let week = 1_795_604_400; // a multiple of 3_600: c = 0
        assert!(m.is_in_session_for(week + 747, 2));
        assert!(!m.is_in_session_for(week + 748, 2)); // the 750 s daily break
        assert!(!m.is_in_session_for(week + 2_999, 2)); // the weekend at 3_000
        assert!(m.is_in_session_for(week + 810, 2)); // the break is over
        assert!(!m.is_in_session_for(week + 809, 2));
        let cme = MarketData { session_schedule: session::SCHEDULE_CME_GLOBEX, ..Default::default() };
        let friday_close = 1_794_607_200;
        assert!(cme.is_in_session_for(friday_close - 3, 2));
        assert!(!cme.is_in_session_for(friday_close - 1, 2));
    }

    #[test]
    fn margin_ratio_matches_program() {
        // $750 on $7,500 entry notional = 10%
        assert_eq!(calculate_margin_ratio(&oil_long(), 75_000_000, 0), 1_000);
        // $4 drop: $350 equity = 4.66%
        assert_eq!(calculate_margin_ratio(&oil_long(), 71_000_000, 0), 466);
    }

    #[test]
    fn margin_ratio_counts_unpaid_funding() {
        // $0.3975 per contract (0.53% of $75) on 100 contracts = $39.75 owed by the long
        assert_eq!(oil_long().accrued_funding(397_500), -39_750_000);
        assert_eq!(calculate_margin_ratio(&oil_long(), 75_000_000, 397_500), 947);
    }

    /// §6.1: the keeper liquidates on mark notional with the MMR in force,
    /// and counts the funding the instruction cranks first (D40).
    #[test]
    fn liquidatable_uses_mark_notional_and_projected_funding() {
        let m = MarketData { maintenance_margin_ratio: 500, funding_interval: 3_600, last_funding_time: 1_000, ..Default::default() };
        let short = PositionData { side: 1, size: 100_000_000, collateral: 750_000_000, entry_price: 75_000_000, opened_price: 75_000_000, ..Default::default() };
        let positions = vec![(Pubkey::new_unique(), short.clone())];
        // $78.60: E = $390 < 5% of $7,860, though 5.2% of the entry notional
        assert!(calculate_margin_ratio(&short, 78_600_000, 0) > 500);
        assert_eq!(liquidatable(&m, &positions, 78_600_000, 1_000).len(), 1);
        assert!(liquidatable(&m, &positions, 78_500_000, 1_000).is_empty());
        // At $78.50 (E $400 vs $392.50) the periods the instruction cranks
        // first push it over. Shorts pay when net short: OI all short, rate
        // −100 (−0.01%) at $78.50 is $0.785 on 100 contracts per period: nine
        // periods leave $392.935, ten $392.15
        let m2 = MarketData { short_open_interest: 100_000_000, ..m.clone() };
        assert!(liquidatable(&m2, &positions, 78_500_000, 1_000 + 9 * 3_600).is_empty());
        assert_eq!(liquidatable(&m2, &positions, 78_500_000, 1_000 + 10 * 3_600).len(), 1);
        // Settled or closed: never
        let s = MarketData { settlement_price: 1, ..m.clone() };
        assert!(liquidatable(&s, &positions, 78_600_000, 1_000).is_empty());
        let closed = MarketData { session_schedule: session::SCHEDULE_CME_GLOBEX, ..m };
        assert!(liquidatable(&closed, &positions, 78_600_000, 1_794_607_200).is_empty());
        assert_eq!(mark_margin_ratio(&short, 78_600_000, 0), 496);
    }
}
