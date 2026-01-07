use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::state::AppState;

#[derive(Serialize)]
pub struct MarketInfo {
    pub address: String,
    pub collateral_mint: String,
    pub max_leverage: u32,
    pub maintenance_margin_ratio: u32,
    pub initial_margin_ratio: u32,
    pub taker_fee: u32,
    pub maker_fee: u32,
    pub long_open_interest: u64,
    pub short_open_interest: u64,
    pub funding_rate: i64,
    pub is_paused: bool,
}

#[derive(Serialize)]
pub struct MarketStats {
    pub price: u64,
    pub volume_24h: u64,
    pub open_interest: u64,
    pub funding_rate: i64,
    pub price_change_24h: f64,
}

#[derive(Serialize)]
pub struct PriceInfo {
    pub price: u64,
    pub timestamp: i64,
    pub confidence: u64,
}

#[derive(Serialize)]
pub struct PositionInfo {
    pub address: String,
    pub owner: String,
    pub side: String,
    pub size: u64,
    pub collateral: u64,
    pub entry_price: u64,
    pub leverage: u32,
    pub unrealized_pnl: i64,
    pub margin_ratio: u32,
}

#[derive(Serialize)]
pub struct OrderInfo {
    pub address: String,
    pub owner: String,
    pub side: String,
    pub order_type: String,
    pub price: u64,
    pub size: u64,
    pub filled_size: u64,
    pub status: String,
    pub created_at: i64,
}

#[derive(Serialize)]
pub struct TradeInfo {
    pub signature: String,
    pub maker: String,
    pub taker: String,
    pub side: String,
    pub price: u64,
    pub size: u64,
    pub timestamp: i64,
}

#[derive(Serialize)]
pub struct AccountInfo {
    pub address: String,
    pub collateral_balance: u64,
    pub total_positions: u32,
    pub realized_pnl: i64,
}

#[derive(Serialize)]
pub struct OrderBookResponse {
    pub bids: Vec<(u64, u64)>,
    pub asks: Vec<(u64, u64)>,
    pub timestamp: i64,
}

#[derive(Deserialize)]
pub struct PaginationQuery {
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

pub async fn get_market(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    // TODO: Fetch actual market data from chain
    let market = MarketInfo {
        address: "market_address".to_string(),
        collateral_mint: "USDC".to_string(),
        max_leverage: 20000,
        maintenance_margin_ratio: 500,
        initial_margin_ratio: 1000,
        taker_fee: 5,
        maker_fee: 2,
        long_open_interest: 0,
        short_open_interest: 0,
        funding_rate: 0,
        is_paused: false,
    };
    Json(market)
}

pub async fn get_market_stats(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let stats = MarketStats {
        price: 75_000_000, // $75.00 in 6 decimals
        volume_24h: 1_000_000_000_000,
        open_interest: 500_000_000_000,
        funding_rate: 100,
        price_change_24h: 1.5,
    };
    Json(stats)
}

pub async fn get_price(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let price = PriceInfo {
        price: 75_000_000,
        timestamp: chrono::Utc::now().timestamp(),
        confidence: 50_000,
    };
    Json(price)
}

pub async fn get_positions(
    State(state): State<Arc<AppState>>,
    Query(pagination): Query<PaginationQuery>,
) -> impl IntoResponse {
    // TODO: Fetch from indexer database
    let positions: Vec<PositionInfo> = vec![];
    Json(positions)
}

pub async fn get_user_positions(
    State(state): State<Arc<AppState>>,
    Path(address): Path<String>,
) -> impl IntoResponse {
    // TODO: Fetch user positions from chain/indexer
    let positions: Vec<PositionInfo> = vec![];
    Json(positions)
}

pub async fn get_orderbook(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let orderbook = state.cached_orderbook.read().unwrap();
    match &*orderbook {
        Some(ob) => Json(OrderBookResponse {
            bids: ob.bids.clone(),
            asks: ob.asks.clone(),
            timestamp: ob.timestamp,
        }),
        None => Json(OrderBookResponse {
            bids: vec![],
            asks: vec![],
            timestamp: chrono::Utc::now().timestamp(),
        }),
    }
}

pub async fn get_user_orders(
    State(state): State<Arc<AppState>>,
    Path(address): Path<String>,
) -> impl IntoResponse {
    let orders: Vec<OrderInfo> = vec![];
    Json(orders)
}

pub async fn get_trades(
    State(state): State<Arc<AppState>>,
    Query(pagination): Query<PaginationQuery>,
) -> impl IntoResponse {
    let trades: Vec<TradeInfo> = vec![];
    Json(trades)
}

pub async fn get_user_trades(
    State(state): State<Arc<AppState>>,
    Path(address): Path<String>,
) -> impl IntoResponse {
    let trades: Vec<TradeInfo> = vec![];
    Json(trades)
}

pub async fn get_account(
    State(state): State<Arc<AppState>>,
    Path(address): Path<String>,
) -> impl IntoResponse {
    let account = AccountInfo {
        address,
        collateral_balance: 0,
        total_positions: 0,
        realized_pnl: 0,
    };
    Json(account)
}
