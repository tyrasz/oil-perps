use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
};
use std::{str::FromStr, sync::Arc};
use tracing::{info, error};

mod funding;
mod liquidator;

// Program configuration
pub struct KeeperConfig {
    pub rpc_url: String,
    pub perps_program_id: Pubkey,
    pub usdc_mint: Pubkey,
    pub keypair: Keypair,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("oil_perps_keeper=info".parse().unwrap()),
        )
        .init();

    dotenvy::dotenv().ok();

    // Load configuration
    let rpc_url = std::env::var("SOLANA_RPC_URL")
        .unwrap_or_else(|_| "https://api.devnet.solana.com".to_string());

    let keypair_path = std::env::var("KEEPER_KEYPAIR")
        .unwrap_or_else(|_| shellexpand::tilde("~/.config/solana/id.json").to_string());

    let perps_program_id = std::env::var("PERPS_PROGRAM_ID")
        .unwrap_or_else(|_| "Perp11111111111111111111111111111111111111".to_string());

    let usdc_mint = std::env::var("USDC_MINT")
        .unwrap_or_else(|_| "4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU".to_string());

    info!("========================================");
    info!("   Oil Perps Keeper Bot Starting");
    info!("========================================");
    info!("RPC URL: {}", rpc_url);
    info!("Program ID: {}", perps_program_id);

    let client = Arc::new(RpcClient::new_with_commitment(
        rpc_url.clone(),
        CommitmentConfig::confirmed(),
    ));

    let keypair = load_keypair(&keypair_path)?;
    info!("Keeper wallet: {}", keypair.pubkey());

    // Check wallet balance
    let balance = client.get_balance(&keypair.pubkey())?;
    info!("Wallet balance: {} SOL", balance as f64 / 1_000_000_000.0);

    if balance < 10_000_000 {
        error!("Warning: Low SOL balance. Consider funding the keeper wallet.");
    }

    let config = KeeperConfig {
        rpc_url,
        perps_program_id: Pubkey::from_str(&perps_program_id)?,
        usdc_mint: Pubkey::from_str(&usdc_mint)?,
        keypair,
    };

    let config = Arc::new(config);

    // Run keeper tasks concurrently
    let funding_client = client.clone();
    let funding_config = config.clone();
    let liquidator_client = client.clone();
    let liquidator_config = config.clone();

    info!("Starting keeper services...");

    let funding_handle = tokio::spawn(async move {
        funding::run_funding_keeper(funding_client, funding_config).await;
    });

    let liquidator_handle = tokio::spawn(async move {
        liquidator::run_liquidation_keeper(liquidator_client, liquidator_config).await;
    });

    // Wait for both tasks
    tokio::select! {
        result = funding_handle => {
            error!("Funding keeper exited: {:?}", result);
        }
        result = liquidator_handle => {
            error!("Liquidation keeper exited: {:?}", result);
        }
    }

    Ok(())
}

fn load_keypair(path: &str) -> Result<Keypair, Box<dyn std::error::Error>> {
    let keypair_data = std::fs::read_to_string(path)?;
    let bytes: Vec<u8> = serde_json::from_str(&keypair_data)?;
    Ok(Keypair::from_bytes(&bytes)?)
}

// Helper to derive PDAs
pub fn get_market_pda(program_id: &Pubkey, collateral_mint: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[b"market", collateral_mint.as_ref()],
        program_id,
    )
}

pub fn get_vault_pda(program_id: &Pubkey, market: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[b"vault", market.as_ref()],
        program_id,
    )
}

pub fn get_vault_token_pda(program_id: &Pubkey, market: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[b"vault_token", market.as_ref()],
        program_id,
    )
}

pub fn get_user_account_pda(program_id: &Pubkey, owner: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[b"user", owner.as_ref()],
        program_id,
    )
}
