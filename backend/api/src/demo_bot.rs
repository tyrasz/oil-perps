use rand::Rng;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

use crate::oracle::OracleService;
use crate::state::{AppState, OrderBookSnapshot};

/// Demo trading bot that simulates market activity
pub struct DemoBot {
    oracle: Arc<OracleService>,
    state: Arc<AppState>,
    recent_trades: Arc<RwLock<Vec<SimulatedTrade>>>,
}

#[derive(Clone, Debug)]
pub struct SimulatedTrade {
    pub commodity: String,
    pub side: String,      // "buy" or "sell"
    pub price: u64,        // 6 decimals
    pub size: u64,         // 6 decimals
    pub timestamp: i64,
    pub maker: String,
    pub taker: String,
}

impl DemoBot {
    pub fn new(oracle: Arc<OracleService>, state: Arc<AppState>) -> Self {
        Self {
            oracle,
            state,
            recent_trades: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Start the demo bot background tasks
    pub fn start(self: Arc<Self>) {
        let bot = self.clone();

        // Task 1: Update orderbook every 2 seconds
        tokio::spawn(async move {
            loop {
                bot.update_orderbooks().await;
                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
            }
        });

        let bot2 = self.clone();

        // Task 2: Execute random trades every 3-8 seconds
        tokio::spawn(async move {
            loop {
                bot2.execute_random_trade().await;
                let delay = {
                    let mut rng = rand::thread_rng();
                    rng.gen_range(3..8)
                };
                tokio::time::sleep(tokio::time::Duration::from_secs(delay)).await;
            }
        });

        info!("Demo trading bot started");
    }

    /// Update orderbooks for all commodities based on current oracle prices
    async fn update_orderbooks(&self) {
        let commodities = ["OIL", "GOLD", "SILVER", "NATGAS", "COPPER"];

        for commodity in commodities {
            if let Some(price_data) = self.oracle.get_price(commodity).await {
                let orderbook = self.generate_orderbook(price_data.price);

                // Update cached orderbook (for now just storing the last one)
                // In a real implementation, we'd have per-commodity orderbooks
                if commodity == "OIL" {
                    let mut cached = self.state.cached_orderbook.write().unwrap();
                    *cached = Some(orderbook);
                }
            }
        }
    }

    /// Generate a realistic orderbook around the current price
    fn generate_orderbook(&self, mid_price: u64) -> OrderBookSnapshot {
        let mut rng = rand::thread_rng();
        let mut bids = Vec::new();
        let mut asks = Vec::new();

        // Generate 10 levels on each side
        for i in 0..10 {
            // Spread increases with distance from mid price
            let spread_bps = 5 + i * 3; // 0.05% to 0.32% spread

            // Bid side (below mid price)
            let bid_price = mid_price - (mid_price * spread_bps / 10000);
            let bid_size = rng.gen_range(1_000_000..50_000_000); // 1 to 50 units
            bids.push((bid_price, bid_size));

            // Ask side (above mid price)
            let ask_price = mid_price + (mid_price * spread_bps / 10000);
            let ask_size = rng.gen_range(1_000_000..50_000_000);
            asks.push((ask_price, ask_size));
        }

        // Sort bids descending (highest first), asks ascending (lowest first)
        bids.sort_by(|a, b| b.0.cmp(&a.0));
        asks.sort_by(|a, b| a.0.cmp(&b.0));

        OrderBookSnapshot {
            bids,
            asks,
            timestamp: chrono::Utc::now().timestamp(),
        }
    }

    /// Execute a random simulated trade
    async fn execute_random_trade(&self) {
        // Pick a random commodity (generate index before await)
        let commodities = ["OIL", "GOLD", "SILVER", "NATGAS", "COPPER"];
        let commodity_idx = {
            let mut rng = rand::thread_rng();
            rng.gen_range(0..commodities.len())
        };
        let commodity = commodities[commodity_idx];

        // Get current price
        let price = match self.oracle.get_price(commodity).await {
            Some(pd) => pd.price,
            None => return,
        };

        // Generate all random values in a block (rng is dropped before await)
        let trade = {
            let mut rng = rand::thread_rng();
            let side = if rng.gen_bool(0.5) { "buy" } else { "sell" };

            // Price slightly off mid (simulating market order slippage)
            let slippage_bps: i64 = rng.gen_range(-10..10);
            let trade_price = if slippage_bps >= 0 {
                price + (price * slippage_bps as u64 / 10000)
            } else {
                price - (price * (-slippage_bps) as u64 / 10000)
            };

            // Random size based on commodity
            let size = match commodity {
                "OIL" => rng.gen_range(100_000..10_000_000),      // 0.1 to 10 barrels
                "GOLD" => rng.gen_range(10_000..1_000_000),       // 0.01 to 1 oz
                "SILVER" => rng.gen_range(100_000..5_000_000),    // 0.1 to 5 oz
                "NATGAS" => rng.gen_range(1_000_000..100_000_000), // 1 to 100 MMBtu
                "COPPER" => rng.gen_range(100_000..10_000_000),   // 0.1 to 10 lbs
                _ => rng.gen_range(1_000_000..10_000_000),
            };

            // Generate fake addresses
            let maker = format!("Demo{}...{}", rng.gen_range(1..100), rng.gen_range(1000..9999));
            let taker = format!("Bot{}...{}", rng.gen_range(1..50), rng.gen_range(1000..9999));

            // Log the trade
            let price_usd = trade_price as f64 / 1_000_000.0;
            let size_display = size as f64 / 1_000_000.0;
            info!(
                "[TRADE] {} {} {:.4} {} @ ${:.2}",
                taker, side.to_uppercase(), size_display, commodity, price_usd
            );

            SimulatedTrade {
                commodity: commodity.to_string(),
                side: side.to_string(),
                price: trade_price,
                size,
                timestamp: chrono::Utc::now().timestamp(),
                maker,
                taker,
            }
        };

        // Store in recent trades (rng is dropped, safe to await)
        let mut trades = self.recent_trades.write().await;
        trades.push(trade);

        // Keep only last 100 trades
        if trades.len() > 100 {
            trades.remove(0);
        }
    }

    /// Get recent trades for a commodity
    pub async fn get_recent_trades(&self, commodity: Option<&str>) -> Vec<SimulatedTrade> {
        let trades = self.recent_trades.read().await;
        match commodity {
            Some(c) => trades.iter()
                .filter(|t| t.commodity == c)
                .cloned()
                .collect(),
            None => trades.clone(),
        }
    }
}
