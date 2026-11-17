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
    pub funding_rate: i64,
    pub last_funding_time: i64,
    pub funding_interval: i64,
    pub insurance_fund: u64,
    pub total_positions: u64,
    pub total_trades: u64,
    pub bump: u8,
    pub is_paused: bool,
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

async fn check_and_liquidate(
    client: &RpcClient,
    config: &KeeperConfig,
) -> Result<u32, Box<dyn std::error::Error + Send + Sync>> {
    // Get market PDA
    let (market_pda, _) = get_market_pda(&config.perps_program_id, &config.usdc_mint);

    // Fetch market data
    let market_account = client.get_account(&market_pda)?;
    let market_data: MarketData = BorshDeserialize::deserialize(&mut &market_account.data[8..])?;

    if market_data.is_paused {
        debug!("Market is paused, skipping liquidation check");
        return Ok(0);
    }

    // Fetch oracle price
    let oracle_price = fetch_oracle_price(client, &market_data.pyth_price_feed)?;
    debug!("Current oracle price: ${:.2}", oracle_price as f64 / 1_000_000.0);

    // Find all open positions
    let open_positions = fetch_open_positions(client, &config.perps_program_id)?;
    debug!("Found {} open positions", open_positions.len());

    let mut liquidated_count = 0;

    for (position_address, position) in open_positions {
        // Check if position is liquidatable
        let margin_ratio = calculate_margin_ratio(&position, oracle_price, market_data.funding_rate);

        if margin_ratio < market_data.maintenance_margin_ratio {
            info!(
                "Found liquidatable position: {} (margin: {}%, required: {}%)",
                position_address,
                margin_ratio as f64 / 100.0,
                market_data.maintenance_margin_ratio as f64 / 100.0
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

fn fetch_oracle_price(
    client: &RpcClient,
    pyth_feed: &Pubkey,
) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
    let account = client.get_account(pyth_feed)?;

    // Parse Pyth price feed
    let price_feed = pyth_sdk_solana::state::load_price_account::<32, ()>(&account.data)
        .map_err(|e| format!("Failed to parse Pyth feed: {:?}", e))?;

    let price = price_feed.agg.price;
    let expo = price_feed.expo;

    // Normalize to 6 decimals
    let normalized = if expo < 0 {
        let divisor = 10i64.pow((-expo) as u32);
        (price * 1_000_000 / divisor) as u64
    } else {
        (price * 1_000_000 * 10i64.pow(expo as u32)) as u64
    };

    Ok(normalized)
}

fn fetch_open_positions(
    client: &RpcClient,
    program_id: &Pubkey,
) -> Result<Vec<(Pubkey, PositionData)>, Box<dyn std::error::Error + Send + Sync>> {
    // Filter for position accounts with status = Open (0)
    let filters = vec![
        RpcFilterType::Memcmp(Memcmp::new_raw_bytes(0, anchor_discriminator("account", "Position").to_vec())),
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
/// Mirrors `Position::accrued_funding` in perps-core.
fn accrued_funding(position: &PositionData, cumulative_funding_rate: i64, price: u64) -> i64 {
    let rate_diff = cumulative_funding_rate as i128 - position.last_funding_payment as i128;
    let notional = (position.size as i128) * (price as i128) / 1_000_000;
    let payment = notional * rate_diff / 1_000_000;
    let signed = if position.side == 0 { -payment } else { payment };
    signed.clamp(i64::MIN as i128, i64::MAX as i128) as i64
}

/// Margin ratio in basis points. Mirrors `Position::margin_ratio` in perps-core
/// (entry-price notional, funding included) so the keeper only submits
/// liquidations the program will accept.
fn calculate_margin_ratio(position: &PositionData, current_price: u64, cumulative_funding_rate: i64) -> u32 {
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
    let funding = accrued_funding(position, cumulative_funding_rate, current_price) as i128;
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
    let (user_account_pda, _) = get_user_account_pda(&config.perps_program_id, &position.owner);

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
        bytes.extend_from_slice(&[0u8; 56]); // padding

        let m = MarketData::deserialize(&mut bytes.as_slice()).unwrap();
        assert_eq!(m.vault, Pubkey::new_from_array([3; 32]));
        assert_eq!(m.pyth_price_feed, Pubkey::new_from_array([4; 32]));
        assert_eq!((m.maintenance_margin_ratio, m.initial_margin_ratio), (500, 1_000));
        assert_eq!(m.funding_rate, -7);
        assert_eq!((m.last_funding_time, m.funding_interval), (1_700_000_000, 3_600));
        assert_eq!(m.insurance_fund, 44);
        assert_eq!((m.bump, m.is_paused), (254, true));
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
        // 0.53% cumulative funding on $7,500 = $39.75 owed by the long
        assert_eq!(accrued_funding(&oil_long(), 5_300, 75_000_000), -39_750_000);
        assert_eq!(calculate_margin_ratio(&oil_long(), 75_000_000, 5_300), 947);
    }
}
