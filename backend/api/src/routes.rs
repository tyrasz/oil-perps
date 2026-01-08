use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::collections::HashMap;

use crate::state::AppState;

// Commodity configuration
#[derive(Clone, Serialize)]
pub struct CommodityConfig {
    pub id: String,
    pub symbol: String,
    pub name: String,
    pub base_price: u64,  // in 6 decimals
    pub max_leverage: u32,
    pub pyth_feed: String,
}

lazy_static::lazy_static! {
    pub static ref COMMODITIES: HashMap<String, CommodityConfig> = {
        let mut m = HashMap::new();
        m.insert("OIL".to_string(), CommodityConfig {
            id: "OIL".to_string(),
            symbol: "OIL-PERP".to_string(),
            name: "Crude Oil".to_string(),
            base_price: 75_000_000,  // $75.00
            max_leverage: 20_000,
            pyth_feed: "GVXRSBjFk6e6J3NbVPXohDJetcTjaeeuykUpbQF8UoMU".to_string(),
        });
        m.insert("GOLD".to_string(), CommodityConfig {
            id: "GOLD".to_string(),
            symbol: "GOLD-PERP".to_string(),
            name: "Gold".to_string(),
            base_price: 2_000_000_000,  // $2000.00
            max_leverage: 20_000,
            pyth_feed: "sXgHcPCNsXM8KaC3CXNXQS8qprLR4dVxQyJyxNmBsLR".to_string(),
        });
        m.insert("SILVER".to_string(), CommodityConfig {
            id: "SILVER".to_string(),
            symbol: "SILVER-PERP".to_string(),
            name: "Silver".to_string(),
            base_price: 24_000_000,  // $24.00
            max_leverage: 20_000,
            pyth_feed: "77JipqJaP9LPFyEGjT2zqz5qxL6KBx3nVtbMd1PPdPr9".to_string(),
        });
        m.insert("NATGAS".to_string(), CommodityConfig {
            id: "NATGAS".to_string(),
            symbol: "NATGAS-PERP".to_string(),
            name: "Natural Gas".to_string(),
            base_price: 2_500_000,  // $2.50
            max_leverage: 15_000,
            pyth_feed: "DBE3N8uNjhKPNAR4oJT8vKwZQN5yDsRXGBCQu4k3Gfgr".to_string(),
        });
        m.insert("COPPER".to_string(), CommodityConfig {
            id: "COPPER".to_string(),
            symbol: "COPPER-PERP".to_string(),
            name: "Copper".to_string(),
            base_price: 4_200_000,  // $4.20
            max_leverage: 15_000,
            pyth_feed: "4wxQsP2B7HNyH4sH3n2J1oKeW6vPExU6mBLgPswN8pqZ".to_string(),
        });
        m
    };
}

#[derive(Serialize)]
pub struct MarketInfo {
    pub address: String,
    pub commodity: String,
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
    pub commodity: String,
    pub price: u64,
    pub volume_24h: u64,
    pub open_interest: u64,
    pub funding_rate: i64,
    pub price_change_24h: f64,
}

#[derive(Serialize)]
pub struct PriceInfo {
    pub commodity: String,
    pub price: u64,
    pub timestamp: i64,
    pub confidence: u64,
}

#[derive(Serialize)]
pub struct PositionInfo {
    pub address: String,
    pub owner: String,
    pub commodity: String,
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
    pub commodity: String,
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
    pub commodity: String,
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
    pub commodity: String,
    pub bids: Vec<(u64, u64)>,
    pub asks: Vec<(u64, u64)>,
    pub timestamp: i64,
}

#[derive(Deserialize)]
pub struct PaginationQuery {
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

// List all available commodities
pub async fn get_commodities() -> impl IntoResponse {
    let commodities: Vec<&CommodityConfig> = COMMODITIES.values().collect();
    Json(commodities)
}

// Get market info for a specific commodity
pub async fn get_market_by_commodity(
    State(_state): State<Arc<AppState>>,
    Path(commodity): Path<String>,
) -> impl IntoResponse {
    let commodity_upper = commodity.to_uppercase();
    let config = COMMODITIES.get(&commodity_upper);

    match config {
        Some(cfg) => {
            let market = MarketInfo {
                address: format!("{}_market_address", commodity_upper),
                commodity: commodity_upper,
                collateral_mint: "USDC".to_string(),
                max_leverage: cfg.max_leverage,
                maintenance_margin_ratio: 500,
                initial_margin_ratio: 1000,
                taker_fee: 5,
                maker_fee: 2,
                long_open_interest: 0,
                short_open_interest: 0,
                funding_rate: 0,
                is_paused: false,
            };
            Json(market).into_response()
        }
        None => (StatusCode::NOT_FOUND, "Commodity not found").into_response(),
    }
}

// Get market stats for a specific commodity
pub async fn get_market_stats_by_commodity(
    State(_state): State<Arc<AppState>>,
    Path(commodity): Path<String>,
) -> impl IntoResponse {
    let commodity_upper = commodity.to_uppercase();
    let config = COMMODITIES.get(&commodity_upper);

    match config {
        Some(cfg) => {
            let stats = MarketStats {
                commodity: commodity_upper,
                price: cfg.base_price,
                volume_24h: 1_000_000_000_000,
                open_interest: 500_000_000_000,
                funding_rate: 100,
                price_change_24h: 1.5,
            };
            Json(stats).into_response()
        }
        None => (StatusCode::NOT_FOUND, "Commodity not found").into_response(),
    }
}

// Get orderbook for a specific commodity
pub async fn get_orderbook_by_commodity(
    State(state): State<Arc<AppState>>,
    Path(commodity): Path<String>,
) -> impl IntoResponse {
    let commodity_upper = commodity.to_uppercase();

    if !COMMODITIES.contains_key(&commodity_upper) {
        return (StatusCode::NOT_FOUND, "Commodity not found").into_response();
    }

    let orderbook = state.cached_orderbook.read().unwrap();
    match &*orderbook {
        Some(ob) => Json(OrderBookResponse {
            commodity: commodity_upper,
            bids: ob.bids.clone(),
            asks: ob.asks.clone(),
            timestamp: ob.timestamp,
        }).into_response(),
        None => Json(OrderBookResponse {
            commodity: commodity_upper,
            bids: vec![],
            asks: vec![],
            timestamp: chrono::Utc::now().timestamp(),
        }).into_response(),
    }
}

// Legacy endpoints for backward compatibility
pub async fn get_market(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    get_market_by_commodity(State(state), Path("OIL".to_string())).await
}

pub async fn get_market_stats(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    get_market_stats_by_commodity(State(state), Path("OIL".to_string())).await
}

pub async fn get_price(
    State(_state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let price = PriceInfo {
        commodity: "OIL".to_string(),
        price: 75_000_000,
        timestamp: chrono::Utc::now().timestamp(),
        confidence: 50_000,
    };
    Json(price)
}

pub async fn get_positions(
    State(_state): State<Arc<AppState>>,
    Query(_pagination): Query<PaginationQuery>,
) -> impl IntoResponse {
    // TODO: Fetch from indexer database
    let positions: Vec<PositionInfo> = vec![];
    Json(positions)
}

pub async fn get_user_positions(
    State(_state): State<Arc<AppState>>,
    Path(_address): Path<String>,
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
            commodity: "OIL".to_string(),
            bids: ob.bids.clone(),
            asks: ob.asks.clone(),
            timestamp: ob.timestamp,
        }),
        None => Json(OrderBookResponse {
            commodity: "OIL".to_string(),
            bids: vec![],
            asks: vec![],
            timestamp: chrono::Utc::now().timestamp(),
        }),
    }
}

pub async fn get_user_orders(
    State(_state): State<Arc<AppState>>,
    Path(_address): Path<String>,
) -> impl IntoResponse {
    let orders: Vec<OrderInfo> = vec![];
    Json(orders)
}

pub async fn get_trades(
    State(_state): State<Arc<AppState>>,
    Query(_pagination): Query<PaginationQuery>,
) -> impl IntoResponse {
    let trades: Vec<TradeInfo> = vec![];
    Json(trades)
}

pub async fn get_user_trades(
    State(_state): State<Arc<AppState>>,
    Path(_address): Path<String>,
) -> impl IntoResponse {
    let trades: Vec<TradeInfo> = vec![];
    Json(trades)
}

pub async fn get_account(
    State(_state): State<Arc<AppState>>,
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
