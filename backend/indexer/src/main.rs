use solana_client::{
    pubsub_client::PubsubClient,
    rpc_client::RpcClient,
    rpc_config::{RpcTransactionLogsConfig, RpcTransactionLogsFilter},
};
use solana_sdk::{commitment_config::CommitmentConfig, pubkey::Pubkey};
use std::str::FromStr;
use tracing::{info, error};

mod db;
mod parser;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::init();
    dotenvy::dotenv().ok();

    let rpc_url = std::env::var("SOLANA_RPC_URL")
        .unwrap_or_else(|_| "http://localhost:8899".to_string());
    let ws_url = std::env::var("SOLANA_WS_URL")
        .unwrap_or_else(|_| "ws://localhost:8900".to_string());
    let database_url = std::env::var("DATABASE_URL")
        .expect("DATABASE_URL must be set");

    let perps_program_id = std::env::var("PERPS_PROGRAM_ID")
        .unwrap_or_else(|_| "Fg6PaFpoGXkYsidMpWTK6W2BeZ7FEfcYkg476zPFsLnS".to_string());

    info!("Starting indexer...");
    info!("RPC: {}", rpc_url);
    info!("Program ID: {}", perps_program_id);

    // Initialize database pool
    let pool = db::create_pool(&database_url).await?;
    db::run_migrations(&pool).await?;

    let program_pubkey = Pubkey::from_str(&perps_program_id)?;

    // Subscribe to program logs
    info!("Subscribing to program logs...");

    let (mut logs_subscription, logs_receiver) = PubsubClient::logs_subscribe(
        &ws_url,
        RpcTransactionLogsFilter::Mentions(vec![perps_program_id.clone()]),
        RpcTransactionLogsConfig {
            commitment: Some(CommitmentConfig::confirmed()),
        },
    )?;

    info!("Subscribed! Listening for events...");

    // Process incoming logs
    loop {
        match logs_receiver.recv() {
            Ok(logs) => {
                let signature = logs.value.signature;
                info!("Processing transaction: {}", signature);

                for log in logs.value.logs {
                    if let Some(event) = parser::parse_log(&log) {
                        match event {
                            parser::PerpsEvent::PositionOpened { owner, size, side, entry_price } => {
                                info!("Position opened: {} {} @ {}",
                                    if side { "LONG" } else { "SHORT" }, size, entry_price);
                                if let Err(e) = db::insert_trade(&pool, &signature, &owner, side, size, entry_price).await {
                                    error!("Failed to insert trade: {}", e);
                                }
                            }
                            parser::PerpsEvent::PositionClosed { owner, pnl, settlement } => {
                                info!("Position closed: owner={}, pnl={}, settlement={}", owner, pnl, settlement);
                            }
                            parser::PerpsEvent::Liquidation { owner, size, reward } => {
                                info!("Liquidation: owner={}, size={}, reward={}", owner, size, reward);
                            }
                            parser::PerpsEvent::FundingUpdated { rate, long_oi, short_oi } => {
                                info!("Funding updated: rate={}, long_oi={}, short_oi={}", rate, long_oi, short_oi);
                            }
                        }
                    }
                }
            }
            Err(e) => {
                error!("Error receiving logs: {}", e);
                break;
            }
        }
    }

    Ok(())
}
