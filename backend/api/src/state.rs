use std::sync::{Arc, RwLock};
use tokio::sync::RwLock as TokioRwLock;
use crate::oracle::OracleService;

/// Simulated trade from demo bot
#[derive(Clone, Debug, serde::Serialize)]
pub struct SimulatedTrade {
    pub commodity: String,
    pub side: String,
    pub price: u64,
    pub size: u64,
    pub timestamp: i64,
    pub maker: String,
    pub taker: String,
}

pub struct AppState {
    pub rpc_url: String,
    pub cached_orderbook: RwLock<Option<OrderBookSnapshot>>,
    pub oracle_service: Arc<OracleService>,
    pub recent_trades: Arc<TokioRwLock<Vec<SimulatedTrade>>>,
}

#[derive(Clone)]
pub struct OrderBookSnapshot {
    pub bids: Vec<(u64, u64)>, // (price, size)
    pub asks: Vec<(u64, u64)>,
    pub timestamp: i64,
}

impl AppState {
    pub fn new(rpc_url: &str) -> Self {
        Self {
            rpc_url: rpc_url.to_string(),
            cached_orderbook: RwLock::new(None),
            oracle_service: Arc::new(OracleService::new()),
            recent_trades: Arc::new(TokioRwLock::new(Vec::new())),
        }
    }
}
