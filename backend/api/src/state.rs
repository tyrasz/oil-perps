use std::sync::{Arc, RwLock};
use crate::oracle::OracleService;

pub struct AppState {
    pub rpc_url: String,  // Store URL for future RPC client usage
    pub cached_orderbook: RwLock<Option<OrderBookSnapshot>>,
    pub oracle_service: Arc<OracleService>,
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
        }
    }
}
