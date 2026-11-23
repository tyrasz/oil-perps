use crate::ix;
use crate::liquidator::{for_each_market, MarketData};
use crate::KeeperConfig;
use solana_client::rpc_client::RpcClient;
use solana_sdk::{pubkey::Pubkey, signature::Signer};
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
    let mut due = Vec::new();
    for_each_market(client, config, |commodity, pda, market| {
        due.push((commodity.to_string(), pda, market));
        Ok(0u32)
    })?;

    let mut updated = false;
    for (commodity, market_pda, market_data) in due {
        match update_market_funding(client, config, &commodity, market_pda, market_data).await {
            Ok(u) => updated |= u,
            Err(e) => error!("{} funding update failed: {}", commodity, e),
        }
    }
    Ok(updated)
}

async fn update_market_funding(
    client: &RpcClient,
    config: &KeeperConfig,
    commodity: &str,
    market_pda: Pubkey,
    market_data: MarketData,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    debug!("Checking funding for {} market {}", commodity, market_pda);

    // Pausing only stops new positions; the program still accepts liquidations
    // and funding updates, and skipping them would let bad debt build up.

    let now = crate::liquidator::unix_now();
    if market_data.is_settled() || !market_data.is_in_session(now) {
        debug!("Market is settled or outside trading hours, skipping funding update");
        return Ok(false);
    }

    // Due on open seconds since the last crank plus the carried remainder
    // (§8), not wall clock: the weekend and the daily break do not count
    let open_secs = market_data.funding_open_secs(now);
    if !market_data.funding_due(now) {
        let left = market_data.funding_interval.saturating_sub(open_secs);
        debug!("Next funding update in {} open seconds ({} minutes)", left, left / 60);
        return Ok(false);
    }

    info!(
        "Funding interval elapsed ({} open seconds incl. carry since last update), triggering update",
        open_secs
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
    let instruction = ix::update_funding(&config.perps_program_id, &config.keypair.pubkey(), market_pda, &market_data.pyth_price_feed);
    let signature = ix::send(client, &config.keypair, instruction)?;
    info!("Funding update tx: {}", signature);
    Ok(())
}
