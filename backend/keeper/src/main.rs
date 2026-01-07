use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
};
use std::{str::FromStr, sync::Arc, time::Duration};
use tokio::time::interval;
use tracing::{info, error, warn};

mod funding;
mod liquidator;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::init();
    dotenvy::dotenv().ok();

    let rpc_url = std::env::var("SOLANA_RPC_URL")
        .unwrap_or_else(|_| "http://localhost:8899".to_string());

    let keypair_path = std::env::var("KEEPER_KEYPAIR")
        .unwrap_or_else(|_| shellexpand::tilde("~/.config/solana/id.json").to_string());

    let perps_program_id = std::env::var("PERPS_PROGRAM_ID")
        .unwrap_or_else(|_| "Fg6PaFpoGXkYsidMpWTK6W2BeZ7FEfcYkg476zPFsLnS".to_string());

    info!("Starting keeper...");
    info!("RPC: {}", rpc_url);

    let client = Arc::new(RpcClient::new_with_commitment(
        rpc_url,
        CommitmentConfig::confirmed(),
    ));

    let keypair = load_keypair(&keypair_path)?;
    info!("Keeper wallet: {}", keypair.pubkey());

    let program_id = Pubkey::from_str(&perps_program_id)?;

    // Run keeper tasks concurrently
    let funding_client = client.clone();
    let liquidator_client = client.clone();

    let funding_handle = tokio::spawn(async move {
        funding::run_funding_keeper(funding_client, program_id).await;
    });

    let liquidator_handle = tokio::spawn(async move {
        liquidator::run_liquidation_keeper(liquidator_client, program_id).await;
    });

    // Wait for both tasks
    tokio::select! {
        _ = funding_handle => {
            error!("Funding keeper exited");
        }
        _ = liquidator_handle => {
            error!("Liquidation keeper exited");
        }
    }

    Ok(())
}

fn load_keypair(path: &str) -> Result<Keypair, Box<dyn std::error::Error>> {
    let keypair_data = std::fs::read_to_string(path)?;
    let bytes: Vec<u8> = serde_json::from_str(&keypair_data)?;
    Ok(Keypair::from_bytes(&bytes)?)
}
