use crate::{price, session};
use crate::{KeeperConfig, get_market_pda, get_vault_pda, get_vault_token_pda, get_user_account_pda};
use borsh::{BorshDeserialize, BorshSerialize};
use solana_client::rpc_client::RpcClient;
use solana_client::rpc_config::{RpcAccountInfoConfig, RpcProgramAccountsConfig};
use solana_client::rpc_filter::{Memcmp, RpcFilterType};
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::Signer,
    transaction::Transaction,
};
use spl_associated_token_account::get_associated_token_address;
use std::{sync::Arc, time::Duration};
use tokio::time::interval;
use tracing::{info, error, warn, debug};

const LIQUIDATION_CHECK_INTERVAL_SECS: u64 = 10;

// Simplified position struct for deserialization
#[derive(BorshDeserialize, BorshSerialize, Debug, Clone, Default)]
pub struct PositionData {
    pub owner: Pubkey,
    pub market: Pubkey,
    pub side: u8, // 0 = Long, 1 = Short
    pub size: u64,
    pub collateral: u64,
    pub entry_price: u64,
    pub leverage: u32,
    pub realized_pnl: i64,
    pub last_funding_payment: i64,
    pub opened_at: i64,
    pub last_updated_at: i64,
    pub status: u8, // 0 = Open, 1 = Closed, 2 = Liquidated
}

// Mirrors programs/perps-core/src/state/market.rs. Borsh decodes fields in
// declaration order, so this must match the on-chain struct exactly.
#[derive(BorshDeserialize, BorshSerialize, Debug, Clone, Default)]
pub struct MarketData {
    pub authority: Pubkey,
    pub collateral_mint: Pubkey,
    pub vault: Pubkey,
    pub pyth_price_feed: Pubkey,
    pub commodity: [u8; 8],
    pub max_leverage: u32,
    pub maintenance_margin_ratio: u32,
    pub initial_margin_ratio: u32,
    pub taker_fee: u32,
    pub maker_fee: u32,
    pub liquidation_fee: u32,
    pub long_open_interest: u64,
    pub short_open_interest: u64,
    pub max_open_interest: u64,
    pub cumulative_funding: i64,
    pub last_funding_time: i64,
    pub funding_interval: i64,
    pub insurance_fund: u64,
    pub total_positions: u64,
    pub total_trades: u64,
    pub bump: u8,
    pub is_paused: bool,
    pub session_schedule: u8,
    pub pre_close_window_secs: u32,
    pub weekend_initial_margin_ratio: u32,
    pub weekend_maintenance_margin_ratio: u32,
    pub max_confidence_bps: u32,
    pub min_oracle_price: u64,
    pub settlement_price: u64,
    pub funding_rate: i64,
}

impl MarketData {
    pub fn is_settled(&self) -> bool {
        self.settlement_price > 0
    }

    pub fn is_in_session(&self, now: i64) -> bool {
        session::is_open(self.session_schedule, now)
    }

    /// Maintenance margin the program applies at `now` (mirrors Market::margin_ratios).
    pub fn maintenance_margin_ratio_at(&self, now: i64) -> u32 {
        if session::in_pre_close_window(self.session_schedule, now, self.pre_close_window_secs) {
            self.maintenance_margin_ratio.max(self.weekend_maintenance_margin_ratio)
        } else {
            self.maintenance_margin_ratio
        }
    }
}

pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Anchor's 8-byte discriminator: the first 8 bytes of sha256("<namespace>:<name>").
pub fn anchor_discriminator(namespace: &str, name: &str) -> [u8; 8] {
    let hash = solana_sdk::hash::hash(format!("{}:{}", namespace, name).as_bytes());
    let mut out = [0u8; 8];
    out.copy_from_slice(&hash.to_bytes()[..8]);
    out
}

pub async fn run_liquidation_keeper(client: Arc<RpcClient>, config: Arc<KeeperConfig>) {
    info!("Liquidation keeper started");
    info!("Checking for liquidatable positions every {} seconds", LIQUIDATION_CHECK_INTERVAL_SECS);

    let mut check_interval = interval(Duration::from_secs(LIQUIDATION_CHECK_INTERVAL_SECS));
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
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    consecutive_errors = 0;
                }
            }
        }
    }
}

type KeeperResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Fetch and decode a commodity's market, or `None` if it isn't initialized.
pub fn fetch_market(client: &RpcClient, config: &KeeperConfig, commodity: &str) -> KeeperResult<Option<(Pubkey, MarketData)>> {
    let (market_pda, _) = get_market_pda(&config.perps_program_id, &config.usdc_mint, commodity);
    let Some(account) = client.get_account_with_commitment(&market_pda, client.commitment())?.value else {
        return Ok(None);
    };
    Ok(decode_market(&account.owner, &account.data, &config.perps_program_id).map(|m| (market_pda, m)))
}

/// Decode a Market account, or `None` if it isn't one: anyone can send
/// lamports to an unused market address, so check owner, size and type first.
fn decode_market(owner: &Pubkey, data: &[u8], program_id: &Pubkey) -> Option<MarketData> {
    if owner != program_id || data.get(..8)? != anchor_discriminator("account", "Market") {
        return None;
    }
    MarketData::deserialize(&mut data.get(8..)?).ok()
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

    let now = unix_now();
    if market_data.is_settled() {
        debug!("Market is settled, no liquidations");
        return Ok(0);
    }
    if !market_data.is_in_session(now) {
        debug!("Market is outside trading hours, skipping liquidation check");
        return Ok(0);
    }
    let maintenance_margin_ratio = market_data.maintenance_margin_ratio_at(now);

    // Fetch oracle price
    let oracle_price = fetch_oracle_price(client, &market_data, now)?;
    debug!("Current oracle price: ${:.2}", oracle_price as f64 / 1_000_000.0);

    // Find all open positions
    let open_positions = fetch_open_positions(client, &config.perps_program_id, &market_pda)?;
    debug!("Found {} open positions", open_positions.len());

    let mut liquidated_count = 0;

    for (position_address, position) in open_positions {
        // Check if position is liquidatable
        let margin_ratio = calculate_margin_ratio(&position, oracle_price, market_data.cumulative_funding);

        if margin_ratio < maintenance_margin_ratio {
            info!(
                "Found liquidatable position: {} (margin: {}%, required: {}%)",
                position_address,
                margin_ratio as f64 / 100.0,
                maintenance_margin_ratio as f64 / 100.0
            );

            match execute_liquidation(client, config, &market_pda, &market_data, &position_address, &position).await {
                Ok(_) => {
                    liquidated_count += 1;
                    info!("Liquidation successful for position {}", position_address);
                }
                Err(e) => {
                    error!("Failed to liquidate position {}: {}", position_address, e);
                }
            }
        }
    }

    Ok(liquidated_count)
}

/// Read the market's Pyth price with the same rules as `oracle::read_price` in
/// perps-core (staleness, confidence, floor), so the keeper doesn't submit
/// liquidations the program would reject.
fn fetch_oracle_price(
    client: &RpcClient,
    market: &MarketData,
    now: i64,
) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
    let account = client.get_account(&market.pyth_price_feed)?;
    let price_account = pyth_sdk_solana::state::load_price_account::<32, ()>(&account.data)
        .map_err(|e| format!("Failed to parse Pyth feed: {:?}", e))?;
    // Same selection as pyth-sdk's PriceFeed: the aggregate while trading,
    // otherwise the previous price.
    let (px, conf, publish_time) = if price_account.agg.status == pyth_sdk_solana::state::PriceStatus::Trading {
        (price_account.agg.price, price_account.agg.conf, price_account.timestamp)
    } else {
        (price_account.prev_price, price_account.prev_conf, price_account.prev_timestamp)
    };
    if !is_fresh(publish_time, now) {
        return Err("Oracle price is stale".into());
    }
    validated_price(px, conf, price_account.expo, market)
}

/// pyth-sdk's get_price_no_older_than: |publish_time - now| <= max age.
fn is_fresh(publish_time: i64, now: i64) -> bool {
    publish_time.abs_diff(now) <= price::MAX_PRICE_AGE_SECS
}

fn validated_price(
    price: i64,
    conf: u64,
    expo: i32,
    market: &MarketData,
) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
    price::validate_price(price, conf, expo, market.max_confidence_bps, market.min_oracle_price)
        .map_err(|r| format!("Oracle price rejected: {:?} (price={}, conf={}, expo={})", r, price, conf, expo).into())
}

/// Offset of `Position::market` (after the discriminator and owner).
const POSITION_MARKET_OFFSET: usize = 8 + 32;

fn fetch_open_positions(
    client: &RpcClient,
    program_id: &Pubkey,
    market: &Pubkey,
) -> Result<Vec<(Pubkey, PositionData)>, Box<dyn std::error::Error + Send + Sync>> {
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

    let accounts = client.get_program_accounts_with_config(program_id, config)?;

    let mut positions = Vec::new();
    for (pubkey, account) in accounts {
        if let Ok(position) = PositionData::deserialize(&mut &account.data[8..]) {
            if position.status == 0 {
                // Open status
                positions.push((pubkey, position));
            }
        }
    }

    Ok(positions)
}

/// Funding owed to (+) or by (-) the position since it last settled, in USDC.
/// Mirrors `Position::accrued_funding` in perps-core: `cumulative_funding` is
/// USDC per whole contract, accumulated at each funding period's price.
fn accrued_funding(position: &PositionData, cumulative_funding: i64) -> i64 {
    let per_contract = cumulative_funding as i128 - position.last_funding_payment as i128;
    let payment = (position.size as i128) * per_contract / 1_000_000;
    let signed = if position.side == 0 { -payment } else { payment };
    signed.clamp(i64::MIN as i128, i64::MAX as i128) as i64
}

/// Margin ratio in basis points. Mirrors `Position::margin_ratio` in perps-core
/// (entry-price notional, funding included) so the keeper only submits
/// liquidations the program will accept.
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
    let funding = accrued_funding(position, cumulative_funding) as i128;
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

async fn execute_liquidation(
    client: &RpcClient,
    config: &KeeperConfig,
    market_pda: &Pubkey,
    market_data: &MarketData,
    position_address: &Pubkey,
    position: &PositionData,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (vault_pda, _) = get_vault_pda(&config.perps_program_id, market_pda);
    let (vault_token_pda, _) = get_vault_token_pda(&config.perps_program_id, market_pda);
    let (user_account_pda, _) = get_user_account_pda(&config.perps_program_id, &position.owner, market_pda);

    // Get liquidator's token account
    let liquidator_token_account = get_associated_token_address(
        &config.keypair.pubkey(),
        &market_data.collateral_mint,
    );

    // Build liquidate instruction
    let discriminator = anchor_discriminator("global", "liquidate");

    let instruction = Instruction {
        program_id: config.perps_program_id,
        accounts: vec![
            AccountMeta::new(config.keypair.pubkey(), true),     // liquidator (signer)
            AccountMeta::new_readonly(position.owner, false),    // position_owner
            AccountMeta::new(user_account_pda, false),           // user_account
            AccountMeta::new(*market_pda, false),                // market
            AccountMeta::new(*position_address, false),          // position
            AccountMeta::new(vault_pda, false),                  // vault
            AccountMeta::new(vault_token_pda, false),            // vault_token_account
            AccountMeta::new(liquidator_token_account, false),   // liquidator_token_account
            AccountMeta::new_readonly(market_data.pyth_price_feed, false), // pyth_price_feed
            AccountMeta::new_readonly(spl_token::id(), false),   // token_program
        ],
        data: discriminator.to_vec(),
    };

    // Get recent blockhash
    let recent_blockhash = client.get_latest_blockhash()?;

    // Build and sign transaction
    let transaction = Transaction::new_signed_with_payer(
        &[instruction],
        Some(&config.keypair.pubkey()),
        &[&config.keypair],
        recent_blockhash,
    );

    // Send transaction
    let signature = client.send_and_confirm_transaction(&transaction)?;
    info!("Liquidation tx: {}", signature);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        bytes.extend_from_slice(&[0u8; 15]); // padding
        assert_eq!(8 + bytes.len(), 298); // Market::LEN

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
    }

    #[test]
    fn ignores_accounts_that_are_not_markets() {
        let program = Pubkey::new_unique();
        let mut data = anchor_discriminator("account", "Market").to_vec();
        data.extend(vec![0u8; 290]);
        assert!(decode_market(&program, &data, &program).is_some());
        // Lamports sent to an unused market address: system-owned, empty
        assert!(decode_market(&Pubkey::default(), &[], &program).is_none());
        // Program-owned but too short, or another account type
        assert!(decode_market(&program, &data[..5], &program).is_none());
        let mut other = anchor_discriminator("account", "Position").to_vec();
        other.extend(vec![0u8; 290]);
        assert!(decode_market(&program, &other, &program).is_none());
    }

    #[test]
    fn markets_created_before_risk_fields_decode_as_defaults() {
        // A market created before the risk fields existed has zeros there.
        let mut bytes = vec![0u8; 298 - 8];
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

    #[test]
    fn oracle_checks_match_program() {
        let market = MarketData { min_oracle_price: 5_000_000, ..Default::default() };
        // $75.00 +/- $0.05 passes; a 2% band fails the default 1% limit
        assert_eq!(validated_price(7_500_000_000, 5_000_000, -8, &market).unwrap(), 75_000_000);
        assert!(validated_price(7_500_000_000, 150_000_000, -8, &market).is_err());
        // Below the $5 floor, and negative prices, are rejected
        assert!(validated_price(499_000_000, 0, -8, &market).is_err());
        assert!(validated_price(-3_763_000_000, 0, -8, &market).is_err());
    }

    #[test]
    fn staleness_matches_pyth_sdk() {
        assert!(is_fresh(1_000, 1_060));
        assert!(!is_fresh(1_000, 1_061));
        assert!(is_fresh(1_060, 1_000)); // pyth-sdk compares the absolute difference
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
        assert_eq!(accrued_funding(&oil_long(), 397_500), -39_750_000);
        assert_eq!(calculate_margin_ratio(&oil_long(), 75_000_000, 397_500), 947);
    }
}
