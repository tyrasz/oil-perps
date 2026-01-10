use axum::{
    routing::get,
    Router,
};
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};

mod routes;
mod ws;
mod state;
mod oracle;

use state::AppState;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    dotenvy::dotenv().ok();

    let rpc_url = std::env::var("SOLANA_RPC_URL")
        .unwrap_or_else(|_| "http://localhost:8899".to_string());

    let state = Arc::new(AppState::new(&rpc_url));

    // Start background price updates with multi-oracle fallback
    let oracle_service = state.oracle_service.clone();
    oracle_service.start_background_updates();
    tracing::info!("Started multi-oracle price feed (Pyth → Backup → Cache)");

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        // Commodity endpoints
        .route("/api/commodities", get(routes::get_commodities))

        // Market endpoints (commodity-specific)
        .route("/api/market/:commodity", get(routes::get_market_by_commodity))
        .route("/api/market/:commodity/stats", get(routes::get_market_stats_by_commodity))

        // Order book endpoints (commodity-specific)
        .route("/api/orderbook/:commodity", get(routes::get_orderbook_by_commodity))

        // Legacy market endpoints (backward compatibility, defaults to OIL)
        .route("/api/market", get(routes::get_market))
        .route("/api/market/stats", get(routes::get_market_stats))
        .route("/api/market/price", get(routes::get_price))

        // Position endpoints
        .route("/api/positions", get(routes::get_positions))
        .route("/api/positions/:address", get(routes::get_user_positions))

        // Legacy order book endpoints
        .route("/api/orderbook", get(routes::get_orderbook))
        .route("/api/orders/:address", get(routes::get_user_orders))

        // Trade history
        .route("/api/trades", get(routes::get_trades))
        .route("/api/trades/:address", get(routes::get_user_trades))

        // User account
        .route("/api/account/:address", get(routes::get_account))

        // Oracle status
        .route("/api/oracle/status", get(routes::get_oracle_status))

        // WebSocket
        .route("/ws", get(ws::websocket_handler))

        .layer(cors)
        .with_state(state);

    let addr = "0.0.0.0:3003";
    tracing::info!("Starting API server on {}", addr);

    axum::Server::bind(&addr.parse().unwrap())
        .serve(app.into_make_service())
        .await
        .unwrap();
}
