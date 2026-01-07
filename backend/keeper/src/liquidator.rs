use solana_client::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use std::{sync::Arc, time::Duration};
use tokio::time::interval;
use tracing::{info, error, warn};

const LIQUIDATION_CHECK_INTERVAL_SECS: u64 = 5; // Check every 5 seconds

pub async fn run_liquidation_keeper(client: Arc<RpcClient>, program_id: Pubkey) {
    info!("Starting liquidation keeper...");

    let mut check_interval = interval(Duration::from_secs(LIQUIDATION_CHECK_INTERVAL_SECS));

    loop {
        check_interval.tick().await;

        match check_and_liquidate(&client, program_id).await {
            Ok(liquidated) => {
                if liquidated > 0 {
                    info!("Liquidated {} positions", liquidated);
                }
            }
            Err(e) => {
                error!("Error during liquidation check: {}", e);
            }
        }
    }
}

async fn check_and_liquidate(
    client: &RpcClient,
    program_id: Pubkey,
) -> Result<u32, Box<dyn std::error::Error + Send + Sync>> {
    // TODO: Fetch all open positions and check if any are liquidatable
    // For each liquidatable position, submit liquidation transaction

    // For now, just log
    // info!("Scanning for liquidatable positions...");

    // Placeholder implementation
    // In production, you would:
    // 1. Fetch the current oracle price
    // 2. Get all open positions (via getProgramAccounts or from indexer)
    // 3. For each position, check if margin_ratio < maintenance_margin_ratio
    // 4. If liquidatable, build and submit the liquidate transaction

    Ok(0)
}

pub struct LiquidatablePosition {
    pub address: Pubkey,
    pub owner: Pubkey,
    pub size: u64,
    pub margin_ratio: u32,
    pub expected_reward: u64,
}

pub fn find_liquidatable_positions(
    positions: Vec<(Pubkey, PositionData)>,
    current_price: u64,
    maintenance_margin_ratio: u32,
) -> Vec<LiquidatablePosition> {
    positions
        .into_iter()
        .filter_map(|(address, position)| {
            let margin_ratio = calculate_margin_ratio(&position, current_price);
            if margin_ratio < maintenance_margin_ratio {
                Some(LiquidatablePosition {
                    address,
                    owner: position.owner,
                    size: position.size,
                    margin_ratio,
                    expected_reward: estimate_liquidation_reward(&position, current_price),
                })
            } else {
                None
            }
        })
        .collect()
}

fn calculate_margin_ratio(position: &PositionData, current_price: u64) -> u32 {
    let entry = position.entry_price as i128;
    let current = current_price as i128;
    let size = position.size as i128;

    let price_diff = if position.is_long {
        current - entry
    } else {
        entry - current
    };

    let pnl = (size * price_diff) / 1_000_000;
    let equity = (position.collateral as i128) + pnl;

    if equity <= 0 {
        return 0;
    }

    let notional = (size * current) / 1_000_000;
    if notional == 0 {
        return 0;
    }

    ((equity * 10000) / notional) as u32
}

fn estimate_liquidation_reward(position: &PositionData, current_price: u64) -> u64 {
    // Estimate based on remaining collateral and liquidation fee
    let pnl = calculate_pnl(position, current_price);
    let remaining = (position.collateral as i64 + pnl).max(0) as u64;
    // Assuming 2.5% liquidation fee
    remaining * 250 / 10000
}

fn calculate_pnl(position: &PositionData, current_price: u64) -> i64 {
    let entry = position.entry_price as i128;
    let current = current_price as i128;
    let size = position.size as i128;

    let price_diff = if position.is_long {
        current - entry
    } else {
        entry - current
    };

    ((size * price_diff) / 1_000_000) as i64
}

pub struct PositionData {
    pub owner: Pubkey,
    pub size: u64,
    pub collateral: u64,
    pub entry_price: u64,
    pub is_long: bool,
}
