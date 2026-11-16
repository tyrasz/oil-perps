use crate::{KeeperConfig, get_market_pda};
use crate::liquidator::MarketData;
use borsh::BorshDeserialize;
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::Signer,
    transaction::Transaction,
};
use std::{sync::Arc, time::Duration};
use tokio::time::interval;
use tracing::{info, error, warn, debug};

const FUNDING_CHECK_INTERVAL_SECS: u64 = 60; // Check every minute

pub async fn run_funding_keeper(client: Arc<RpcClient>, config: Arc<KeeperConfig>) {
    info!("Funding keeper started");
    info!("Checking for funding updates every {} seconds", FUNDING_CHECK_INTERVAL_SECS);

    let mut check_interval = interval(Duration::from_secs(FUNDING_CHECK_INTERVAL_SECS));
    let mut consecutive_errors = 0;

    loop {
        check_interval.tick().await;

        match check_and_update_funding(&client, &config).await {
            Ok(updated) => {
                consecutive_errors = 0;
                if updated {
                    info!("Funding rate updated successfully");
                }
            }
            Err(e) => {
                consecutive_errors += 1;
                error!("Funding update error (attempt {}): {}", consecutive_errors, e);

                if consecutive_errors >= 5 {
                    warn!("Too many consecutive errors, backing off...");
                    tokio::time::sleep(Duration::from_secs(300)).await;
                    consecutive_errors = 0;
                }
            }
        }
    }
}

async fn check_and_update_funding(
    client: &RpcClient,
    config: &KeeperConfig,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    // Get market PDA
    let (market_pda, _) = get_market_pda(&config.perps_program_id, &config.usdc_mint);

    // Fetch market data
    let market_account = client.get_account(&market_pda)?;
    let market_data: MarketData = BorshDeserialize::deserialize(&mut &market_account.data[8..])?;

    if market_data.is_paused {
        debug!("Market is paused, skipping funding update");
        return Ok(false);
    }

    // Check if enough time has passed since last funding update
    let current_time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs() as i64;

    let time_since_last_funding = current_time - market_data.last_funding_time;
    let time_until_next = market_data.funding_interval - time_since_last_funding;

    if time_until_next > 0 {
        debug!(
            "Next funding update in {} seconds ({} minutes)",
            time_until_next,
            time_until_next / 60
        );
        return Ok(false);
    }

    info!(
        "Funding interval elapsed ({}s since last update), triggering update",
        time_since_last_funding
    );

    // Log current OI for transparency
    info!(
        "Current OI - Long: ${:.2}, Short: ${:.2}",
        market_data.long_open_interest as f64 / 1_000_000.0,
        market_data.short_open_interest as f64 / 1_000_000.0
    );

    // Execute funding update
    execute_funding_update(client, config, &market_pda, &market_data).await?;

    Ok(true)
}

async fn execute_funding_update(
    client: &RpcClient,
    config: &KeeperConfig,
    market_pda: &Pubkey,
    market_data: &MarketData,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Instruction discriminator for "update_funding" in Anchor
    let discriminator: [u8; 8] = [156, 148, 159, 169, 219, 157, 46, 227];

    let instruction = Instruction {
        program_id: config.perps_program_id,
        accounts: vec![
            AccountMeta::new(config.keypair.pubkey(), true),           // keeper (signer)
            AccountMeta::new(*market_pda, false),                      // market
            AccountMeta::new_readonly(market_data.pyth_price_feed, false), // pyth_price_feed
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
    info!("Funding update tx: {}", signature);

    Ok(())
}

// Calculate what the next funding rate will be (for logging)
pub fn estimate_funding_rate(long_oi: u64, short_oi: u64) -> i64 {
    let long = long_oi as i128;
    let short = short_oi as i128;
    let total = long + short;

    if total == 0 {
        return 0;
    }

    // Funding rate = (long_oi - short_oi) / total_oi * base_rate
    // Using 0.01% per hour as base rate = 100 (6 decimals)
    let imbalance = ((long - short) * 1_000_000) / total;
    let base_rate: i128 = 100;
    ((imbalance * base_rate) / 1_000_000) as i64
}
