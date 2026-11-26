use solana_client::rpc_client::RpcClient;
use solana_commitment_config::CommitmentConfig;
use solana_sdk::{
    pubkey::Pubkey,
    signature::{Keypair, Signer},
};
use std::{str::FromStr, sync::Arc};
use tracing::{info, error};

mod adl;
mod funding;
mod ix;
mod liquidator;
mod mirror;
mod oracle;
mod settle;
mod state;
#[cfg(test)]
mod testkit;
// Compiled from the program's sources so the keeper makes the same decisions.
// All four are dependency-free (docs/lp-pool.md §2.7, D29, D47). They build
// with the platform-tools' Rust on-chain, so newer clippy lints are allowed here.
#[allow(dead_code, clippy::unnecessary_map_or)]
#[path = "../../../programs/perps-core/src/session.rs"]
mod session;
#[allow(dead_code)]
#[path = "../../../programs/perps-core/src/price.rs"]
mod price;
#[allow(dead_code, clippy::manual_is_multiple_of)]
#[path = "../../../programs/perps-core/src/pool_math.rs"]
mod pool_math;
// perps-core's Pyth Solana Receiver `PriceUpdateV2` reader (D47); `oracle`
// applies the checks of perps-core's `oracle::read_price` with it.
#[allow(dead_code)]
#[path = "../../../programs/perps-core/src/pyth_receiver.rs"]
mod pyth_receiver;

// Program configuration
pub struct KeeperConfig {
    pub rpc_url: String,
    pub perps_program_id: Pubkey,
    pub usdc_mint: Pubkey,
    /// Commodities to keep, one market each (MARKETS env, comma-separated).
    pub commodities: Vec<String>,
    pub keypair: Keypair,
}

pub const DEFAULT_MARKETS: &str = "OIL,GOLD,SILVER,NATGAS,COPPER";

/// Seconds before a session close in which the keeper sends no settle or
/// funding crank: decided at the chain's clock, the transaction lands a slot
/// (or a few) later, after the close (`MarketClosed`).
pub const SESSION_EDGE_GUARD_SECS: i64 = 2;

/// A positive number of seconds from env var `name`, else `default`. The poll
/// intervals and back-offs are tuned for CME hours; a localnet run on the
/// one-hour test week (docs/localnet.md) sets them shorter.
pub fn env_secs(name: &str, default: u64) -> u64 {
    parse_secs(std::env::var(name).ok().as_deref(), default)
}

pub fn parse_secs(value: Option<&str>, default: u64) -> u64 {
    value.and_then(|v| v.trim().parse::<u64>().ok()).filter(|&v| v > 0).unwrap_or(default)
}

pub fn parse_markets(s: &str) -> Vec<String> {
    s.split(',')
        .map(|c| c.trim().to_ascii_uppercase())
        .filter(|c| !c.is_empty())
        .collect()
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
        .unwrap_or_else(|_| "Fg6PaFpoGXkYsidMpWTK6W2BeZ7FEfcYkg476zPFsLnS".to_string()); // perps-core declare_id!

    let usdc_mint = std::env::var("USDC_MINT")
        .unwrap_or_else(|_| "4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU".to_string());

    let commodities = parse_markets(&std::env::var("MARKETS").unwrap_or_else(|_| DEFAULT_MARKETS.to_string()));

    info!("========================================");
    info!("   Oil Perps Keeper Bot Starting");
    info!("========================================");
    info!("RPC URL: {}", rpc_url);
    info!("Program ID: {}", perps_program_id);
    info!("Markets: {}", commodities.join(", "));

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
        commodities,
        keypair,
    };

    let config = Arc::new(config);

    // Run keeper tasks concurrently
    let funding_client = client.clone();
    let funding_config = config.clone();
    let liquidator_client = client.clone();
    let liquidator_config = config.clone();
    let adl_client = client.clone();
    let adl_config = config.clone();
    let pool_client = client.clone();
    let pool_config = config.clone();

    info!("Starting keeper services...");

    let funding_handle = tokio::spawn(async move {
        funding::run_funding_keeper(funding_client, funding_config).await;
    });

    let liquidator_handle = tokio::spawn(async move {
        liquidator::run_liquidation_keeper(liquidator_client, liquidator_config).await;
    });

    // ADL and bad-debt write-off (docs/lp-pool.md §7, §16.4)
    let adl_handle = tokio::spawn(async move {
        adl::run_adl_keeper(adl_client, adl_config).await;
    });

    // settle_position_pnl, settled-market closes and insurance sweeps (§6.6, §10)
    let pool_handle = tokio::spawn(async move {
        settle::run_pool_keeper(pool_client, pool_config).await;
    });

    // Run until any task exits
    tokio::select! {
        result = funding_handle => {
            error!("Funding keeper exited: {:?}", result);
        }
        result = liquidator_handle => {
            error!("Liquidation keeper exited: {:?}", result);
        }
        result = adl_handle => {
            error!("ADL keeper exited: {:?}", result);
        }
        result = pool_handle => {
            error!("Pool keeper exited: {:?}", result);
        }
    }

    Ok(())
}

fn load_keypair(path: &str) -> Result<Keypair, Box<dyn std::error::Error>> {
    let keypair_data = std::fs::read_to_string(path)?;
    let bytes: Vec<u8> = serde_json::from_str(&keypair_data)?;
    // solana-keypair 3 also checks the public half against the secret key.
    Ok(Keypair::try_from(bytes.as_slice())?)
}

// Helpers to derive PDAs (must match the seeds in programs/perps-core)

/// One market per (collateral mint, commodity), e.g. "OIL".
pub fn get_market_pda(program_id: &Pubkey, collateral_mint: &Pubkey, commodity: &str) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[b"market", collateral_mint.as_ref(), commodity.as_bytes()],
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

/// The market's LP pool (docs/lp-pool.md §1.2).
pub fn get_lp_pool_pda(program_id: &Pubkey, market: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[b"lp_pool", market.as_ref()], program_id)
}

/// User accounts are per market.
pub fn get_user_account_pda(program_id: &Pubkey, owner: &Pubkey, market: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[b"user", owner.as_ref(), market.as_ref()],
        program_id,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_commodity_has_its_own_market_and_user_accounts() {
        let program = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let owner = Pubkey::new_unique();
        let markets: Vec<_> = parse_markets(DEFAULT_MARKETS)
            .iter()
            .map(|c| get_market_pda(&program, &mint, c).0)
            .collect();
        let unique: std::collections::HashSet<_> = markets.iter().collect();
        assert_eq!(unique.len(), 5);

        let users: std::collections::HashSet<_> =
            markets.iter().map(|m| get_user_account_pda(&program, &owner, m).0).collect();
        assert_eq!(users.len(), 5);
    }

    #[test]
    fn addresses_match_program_derivation() {
        // Same values are pinned in perps-core (state/market.rs) and the frontend.
        let program = Pubkey::from_str("Fg6PaFpoGXkYsidMpWTK6W2BeZ7FEfcYkg476zPFsLnS").unwrap();
        let mint = Pubkey::from_str("4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU").unwrap();
        let (oil, _) = get_market_pda(&program, &mint, "OIL");
        assert_eq!(oil.to_string(), "6Chy7k9ArMFQamqogr5jsYVoRTbnP9TaCxvDqRV6ntD9");
        let owner = Pubkey::from_str("11111111111111111111111111111112").unwrap();
        assert_eq!(
            get_user_account_pda(&program, &owner, &oil).0.to_string(),
            "4NBuxBaDE8SpGFiytjqTmkYqkyc628SAsGxrc51jkHso"
        );
    }

    #[test]
    fn parses_interval_overrides() {
        assert_eq!(parse_secs(None, 60), 60);
        assert_eq!(parse_secs(Some(" 5 "), 60), 5);
        assert_eq!(parse_secs(Some("0"), 60), 60);
        assert_eq!(parse_secs(Some("soon"), 60), 60);
    }

    #[test]
    fn parses_market_list() {
        assert_eq!(parse_markets(" oil, GOLD ,,natgas "), vec!["OIL", "GOLD", "NATGAS"]);
    }
}
