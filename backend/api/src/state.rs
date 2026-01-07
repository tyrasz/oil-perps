use solana_client::rpc_client::RpcClient;
use std::sync::RwLock;

pub struct AppState {
    pub rpc_client: RpcClient,
    pub cached_price: RwLock<Option<u64>>,
    pub cached_orderbook: RwLock<Option<OrderBookSnapshot>>,
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
            rpc_client: RpcClient::new(rpc_url.to_string()),
            cached_price: RwLock::new(None),
            cached_orderbook: RwLock::new(None),
        }
    }
}
