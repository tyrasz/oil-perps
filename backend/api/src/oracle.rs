use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

// =============================================================================
// Configuration
// =============================================================================

/// Maximum age in seconds before a price is considered stale
/// Note: Set to 24 hours for development (Pyth test feeds may be delayed)
/// For production, use 60 seconds
const MAX_STALENESS_SECS: i64 = 86400;  // 24 hours for dev, 60 for prod

/// Maximum allowed deviation from cached price (5% = 500 basis points)
const MAX_DEVIATION_BPS: u64 = 500;

/// Cache validity period (2x staleness for emergency fallback)
const CACHE_VALIDITY_SECS: i64 = 172800;  // 48 hours for dev

// Pyth Hermes API endpoint
const PYTH_HERMES_URL: &str = "https://hermes.pyth.network";

// Twelve Data API endpoint (backup)
const TWELVE_DATA_URL: &str = "https://api.twelvedata.com";

// =============================================================================
// Price Feed Configuration
// =============================================================================

/// Pyth Price Feed IDs (hex format for Hermes API)
pub const PYTH_FEEDS: &[(&str, &str)] = &[
    ("OIL", "925ca92ff005ae943c158e3563f59698ce7e75c5a8c8dd43303a0a154887b3e6"),     // WTI USOILSPOT
    ("GOLD", "765d2ba906dbc32ca17cc11f5310a89e9ee1f6420508c63861f2f8ba4ee34bb2"),    // XAU/USD
    ("SILVER", "f2fb02c32b055c805e7238d628e5e9dadef274376114eb1f012337cabe93871e"),  // XAG/USD
];

/// Twelve Data symbols for backup
pub const TWELVE_DATA_SYMBOLS: &[(&str, &str)] = &[
    ("OIL", "CL"),      // Crude Oil WTI
    ("GOLD", "XAU/USD"),
    ("SILVER", "XAG/USD"),
    ("NATGAS", "NG"),   // Natural Gas
    ("COPPER", "HG"),   // Copper
];

// =============================================================================
// Data Types
// =============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PriceSource {
    Pyth,
    TwelveData,
    Cached,
    Simulated,
}

impl std::fmt::Display for PriceSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PriceSource::Pyth => write!(f, "Pyth"),
            PriceSource::TwelveData => write!(f, "TwelveData"),
            PriceSource::Cached => write!(f, "Cached"),
            PriceSource::Simulated => write!(f, "Simulated"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceData {
    pub commodity: String,
    pub price: u64,              // Price in 6 decimals (e.g., 75_000_000 = $75.00)
    pub confidence: u64,         // Confidence interval in 6 decimals
    pub timestamp: i64,          // Unix timestamp
    pub price_change_24h: f64,   // Percentage change
    pub source: PriceSource,     // Where this price came from
    pub is_valid: bool,          // Whether price passed validation
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OracleStatus {
    pub pyth_healthy: bool,
    pub backup_healthy: bool,
    pub last_pyth_update: Option<i64>,
    pub last_backup_update: Option<i64>,
    pub circuit_breaker_active: bool,
    pub commodities_available: Vec<String>,
}

// =============================================================================
// API Response Types
// =============================================================================

#[derive(Debug, Deserialize)]
struct HermesPriceResponse {
    parsed: Vec<ParsedPrice>,
}

#[derive(Debug, Deserialize)]
struct ParsedPrice {
    id: String,
    price: PythPriceInfo,
}

#[derive(Debug, Deserialize)]
struct PythPriceInfo {
    price: String,
    conf: String,
    expo: i32,
    publish_time: i64,
}

#[derive(Debug, Deserialize)]
struct TwelveDataResponse {
    price: Option<String>,
    timestamp: Option<i64>,
}

// =============================================================================
// Oracle Service
// =============================================================================

pub struct OracleService {
    client: reqwest::Client,
    twelve_data_api_key: Option<String>,

    // Current validated prices
    prices: Arc<RwLock<HashMap<String, PriceData>>>,

    // Price cache for fallback
    cache: Arc<RwLock<HashMap<String, PriceData>>>,

    // Previous prices for 24h change calculation
    previous_prices: Arc<RwLock<HashMap<String, u64>>>,

    // Oracle health status
    status: Arc<RwLock<OracleStatus>>,
}

impl OracleService {
    pub fn new() -> Self {
        let twelve_data_api_key = std::env::var("TWELVE_DATA_API_KEY").ok();

        if twelve_data_api_key.is_some() {
            info!("Twelve Data API key configured - backup oracle enabled");
        } else {
            warn!("TWELVE_DATA_API_KEY not set - backup oracle disabled");
        }

        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap(),
            twelve_data_api_key,
            prices: Arc::new(RwLock::new(HashMap::new())),
            cache: Arc::new(RwLock::new(HashMap::new())),
            previous_prices: Arc::new(RwLock::new(HashMap::new())),
            status: Arc::new(RwLock::new(OracleStatus {
                pyth_healthy: false,
                backup_healthy: false,
                last_pyth_update: None,
                last_backup_update: None,
                circuit_breaker_active: false,
                commodities_available: vec![],
            })),
        }
    }

    // =========================================================================
    // Main Price Fetching with Fallback Chain
    // =========================================================================

    /// Fetch prices using fallback chain: Pyth -> Backup API -> Cache
    pub async fn fetch_prices(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let now = chrono::Utc::now().timestamp();
        let mut all_prices: HashMap<String, PriceData> = HashMap::new();

        // Step 1: Try Pyth (primary)
        let pyth_result = self.fetch_pyth_prices().await;
        let pyth_healthy = pyth_result.is_ok();

        if let Ok(pyth_prices) = pyth_result {
            for (commodity, price) in pyth_prices {
                if self.validate_price(&price).await {
                    all_prices.insert(commodity, price);
                }
            }
            info!("Pyth: fetched {} valid prices", all_prices.len());
        } else {
            warn!("Pyth fetch failed: {:?}", pyth_result.err());
        }

        // Step 2: For missing commodities, try backup API
        let missing: Vec<String> = ["OIL", "GOLD", "SILVER", "NATGAS", "COPPER"]
            .iter()
            .filter(|c| !all_prices.contains_key(**c))
            .map(|s| s.to_string())
            .collect();

        let mut backup_healthy = false;
        if !missing.is_empty() && self.twelve_data_api_key.is_some() {
            if let Ok(backup_prices) = self.fetch_backup_prices(&missing).await {
                backup_healthy = true;
                for (commodity, price) in backup_prices {
                    if self.validate_price(&price).await {
                        all_prices.insert(commodity, price);
                    }
                }
                info!("Backup API: fetched {} additional prices", missing.len() - all_prices.len());
            }
        }

        // Step 3: For still-missing commodities, use cache if fresh
        let still_missing: Vec<String> = ["OIL", "GOLD", "SILVER", "NATGAS", "COPPER"]
            .iter()
            .filter(|c| !all_prices.contains_key(**c))
            .map(|s| s.to_string())
            .collect();

        if !still_missing.is_empty() {
            let cache = self.cache.read().await;
            for commodity in &still_missing {
                if let Some(cached) = cache.get(commodity) {
                    if self.is_cache_valid(cached, now) {
                        let mut cached_price = cached.clone();
                        cached_price.source = PriceSource::Cached;
                        all_prices.insert(commodity.clone(), cached_price);
                        warn!("{}: using cached price (age: {}s)", commodity, now - cached.timestamp);
                    }
                }
            }
        }

        // Step 4: For commodities with no data, use simulation (NATGAS, COPPER)
        self.add_simulated_prices(&mut all_prices).await;

        // Update cache with fresh prices
        {
            let mut cache = self.cache.write().await;
            for (commodity, price) in &all_prices {
                if price.source == PriceSource::Pyth || price.source == PriceSource::TwelveData {
                    cache.insert(commodity.clone(), price.clone());
                }
            }
        }

        // Update previous prices for 24h calculation
        self.update_previous_prices(&all_prices).await;

        // Check circuit breaker
        let circuit_breaker_active = all_prices.is_empty() ||
            (all_prices.values().all(|p| p.source == PriceSource::Cached || p.source == PriceSource::Simulated));

        // Update status
        {
            let mut status = self.status.write().await;
            status.pyth_healthy = pyth_healthy;
            status.backup_healthy = backup_healthy;
            status.last_pyth_update = if pyth_healthy { Some(now) } else { status.last_pyth_update };
            status.last_backup_update = if backup_healthy { Some(now) } else { status.last_backup_update };
            status.circuit_breaker_active = circuit_breaker_active;
            status.commodities_available = all_prices.keys().cloned().collect();
        }

        if circuit_breaker_active {
            error!("CIRCUIT BREAKER: No fresh oracle data available!");
        }

        // Store validated prices
        {
            let mut prices = self.prices.write().await;
            *prices = all_prices;
        }

        Ok(())
    }

    // =========================================================================
    // Pyth Oracle
    // =========================================================================

    async fn fetch_pyth_prices(&self) -> Result<HashMap<String, PriceData>, Box<dyn std::error::Error + Send + Sync>> {
        let feed_ids: Vec<&str> = PYTH_FEEDS.iter().map(|(_, id)| *id).collect();
        let ids_param: String = feed_ids.iter()
            .map(|id| format!("ids[]={}", id))
            .collect::<Vec<_>>()
            .join("&");

        let url = format!("{}/v2/updates/price/latest?{}", PYTH_HERMES_URL, ids_param);

        let response = self.client.get(&url).send().await?;

        if !response.status().is_success() {
            return Err(format!("Pyth API error: {}", response.status()).into());
        }

        let data: HermesPriceResponse = response.json().await?;
        let previous = self.previous_prices.read().await;
        let mut prices = HashMap::new();

        for parsed in data.parsed {
            if let Some((commodity, _)) = PYTH_FEEDS.iter().find(|(_, id)| *id == parsed.id) {
                let price_data = self.parse_pyth_price(*commodity, &parsed.price, &previous);

                info!(
                    "[Pyth] {} = ${:.2} (conf: ${:.4})",
                    commodity,
                    price_data.price as f64 / 1_000_000.0,
                    price_data.confidence as f64 / 1_000_000.0
                );

                prices.insert(commodity.to_string(), price_data);
            }
        }

        Ok(prices)
    }

    fn parse_pyth_price(&self, commodity: &str, price_info: &PythPriceInfo, previous: &HashMap<String, u64>) -> PriceData {
        let raw_price: i64 = price_info.price.parse().unwrap_or(0);
        let expo = price_info.expo;

        // Convert to 6 decimal format
        let price_6_decimals = if expo < -6 {
            let divisor = 10_i64.pow((-6 - expo) as u32);
            (raw_price / divisor) as u64
        } else if expo > -6 {
            let multiplier = 10_i64.pow((-6 - expo).unsigned_abs());
            (raw_price * multiplier) as u64
        } else {
            raw_price as u64
        };

        // Calculate confidence
        let raw_conf: i64 = price_info.conf.parse().unwrap_or(0);
        let conf_6_decimals = if expo < -6 {
            let divisor = 10_i64.pow((-6 - expo) as u32);
            (raw_conf / divisor) as u64
        } else if expo > -6 {
            let multiplier = 10_i64.pow((-6 - expo).unsigned_abs());
            (raw_conf * multiplier) as u64
        } else {
            raw_conf as u64
        };

        // Calculate 24h change
        let prev_price = previous.get(commodity).copied().unwrap_or(price_6_decimals);
        let price_change = if prev_price > 0 {
            ((price_6_decimals as f64 - prev_price as f64) / prev_price as f64) * 100.0
        } else {
            0.0
        };

        PriceData {
            commodity: commodity.to_string(),
            price: price_6_decimals,
            confidence: conf_6_decimals,
            timestamp: price_info.publish_time,
            price_change_24h: price_change,
            source: PriceSource::Pyth,
            is_valid: true,
        }
    }

    // =========================================================================
    // Backup Oracle (Twelve Data)
    // =========================================================================

    async fn fetch_backup_prices(&self, commodities: &[String]) -> Result<HashMap<String, PriceData>, Box<dyn std::error::Error + Send + Sync>> {
        let api_key = match &self.twelve_data_api_key {
            Some(key) => key,
            None => return Err("No API key configured".into()),
        };

        let mut prices = HashMap::new();
        let previous = self.previous_prices.read().await;

        for commodity in commodities {
            if let Some((_, symbol)) = TWELVE_DATA_SYMBOLS.iter().find(|(c, _)| *c == commodity.as_str()) {
                let url = format!(
                    "{}/price?symbol={}&apikey={}",
                    TWELVE_DATA_URL, symbol, api_key
                );

                match self.client.get(&url).send().await {
                    Ok(response) if response.status().is_success() => {
                        if let Ok(data) = response.json::<TwelveDataResponse>().await {
                            if let Some(price_str) = data.price {
                                if let Ok(price_f64) = price_str.parse::<f64>() {
                                    let price_6_decimals = (price_f64 * 1_000_000.0) as u64;
                                    let prev_price = previous.get(commodity).copied().unwrap_or(price_6_decimals);
                                    let price_change = if prev_price > 0 {
                                        ((price_6_decimals as f64 - prev_price as f64) / prev_price as f64) * 100.0
                                    } else {
                                        0.0
                                    };

                                    info!(
                                        "[TwelveData] {} = ${:.2}",
                                        commodity,
                                        price_6_decimals as f64 / 1_000_000.0
                                    );

                                    prices.insert(commodity.clone(), PriceData {
                                        commodity: commodity.clone(),
                                        price: price_6_decimals,
                                        confidence: 50_000, // Default confidence for backup
                                        timestamp: chrono::Utc::now().timestamp(),
                                        price_change_24h: price_change,
                                        source: PriceSource::TwelveData,
                                        is_valid: true,
                                    });
                                }
                            }
                        }
                    }
                    Ok(response) => {
                        warn!("TwelveData API error for {}: {}", commodity, response.status());
                    }
                    Err(e) => {
                        warn!("TwelveData fetch error for {}: {}", commodity, e);
                    }
                }
            }
        }

        Ok(prices)
    }

    // =========================================================================
    // Simulated Prices (for commodities without oracle coverage)
    // =========================================================================

    async fn add_simulated_prices(&self, prices: &mut HashMap<String, PriceData>) {
        use rand::Rng;
        let mut rng = rand::thread_rng();
        let now = chrono::Utc::now().timestamp();

        // NATGAS - base around $2.50
        if !prices.contains_key("NATGAS") {
            let base = 2_500_000u64;
            let volatility = (rng.gen::<f64>() - 0.5) * 0.10;
            let price = ((base as f64) * (1.0 + volatility)) as u64;

            prices.insert("NATGAS".to_string(), PriceData {
                commodity: "NATGAS".to_string(),
                price,
                confidence: 10_000,
                timestamp: now,
                price_change_24h: volatility * 100.0,
                source: PriceSource::Simulated,
                is_valid: true,
            });
        }

        // COPPER - base around $4.20
        if !prices.contains_key("COPPER") {
            let base = 4_200_000u64;
            let volatility = (rng.gen::<f64>() - 0.5) * 0.06;
            let price = ((base as f64) * (1.0 + volatility)) as u64;

            prices.insert("COPPER".to_string(), PriceData {
                commodity: "COPPER".to_string(),
                price,
                confidence: 5_000,
                timestamp: now,
                price_change_24h: volatility * 100.0,
                source: PriceSource::Simulated,
                is_valid: true,
            });
        }
    }

    // =========================================================================
    // Validation
    // =========================================================================

    /// Validate a price against staleness and deviation checks
    async fn validate_price(&self, price: &PriceData) -> bool {
        let now = chrono::Utc::now().timestamp();

        // Staleness check
        if now - price.timestamp > MAX_STALENESS_SECS {
            warn!(
                "{}: price is stale (age: {}s, max: {}s)",
                price.commodity,
                now - price.timestamp,
                MAX_STALENESS_SECS
            );
            return false;
        }

        // Deviation check against cache
        let cache = self.cache.read().await;
        if let Some(cached) = cache.get(&price.commodity) {
            let deviation_bps = self.calculate_deviation_bps(price.price, cached.price);
            if deviation_bps > MAX_DEVIATION_BPS {
                warn!(
                    "{}: price deviation too high ({}bps > {}bps) - new: ${:.2}, cached: ${:.2}",
                    price.commodity,
                    deviation_bps,
                    MAX_DEVIATION_BPS,
                    price.price as f64 / 1_000_000.0,
                    cached.price as f64 / 1_000_000.0
                );
                return false;
            }
        }

        true
    }

    fn calculate_deviation_bps(&self, new_price: u64, old_price: u64) -> u64 {
        if old_price == 0 {
            return 0;
        }
        let diff = if new_price > old_price {
            new_price - old_price
        } else {
            old_price - new_price
        };
        (diff * 10_000) / old_price
    }

    fn is_cache_valid(&self, cached: &PriceData, now: i64) -> bool {
        now - cached.timestamp < CACHE_VALIDITY_SECS
    }

    async fn update_previous_prices(&self, prices: &HashMap<String, PriceData>) {
        let mut previous = self.previous_prices.write().await;
        for (commodity, price_data) in prices {
            if !previous.contains_key(commodity) {
                previous.insert(commodity.clone(), price_data.price);
            }
        }
    }

    // =========================================================================
    // Public API
    // =========================================================================

    /// Get current validated price for a commodity
    pub async fn get_price(&self, commodity: &str) -> Option<PriceData> {
        let prices = self.prices.read().await;
        prices.get(commodity).cloned()
    }

    /// Get all current validated prices
    pub async fn get_all_prices(&self) -> HashMap<String, PriceData> {
        let prices = self.prices.read().await;
        prices.clone()
    }

    /// Get oracle health status
    pub async fn get_status(&self) -> OracleStatus {
        let status = self.status.read().await;
        status.clone()
    }

    /// Check if trading should be allowed (circuit breaker check)
    pub async fn is_trading_allowed(&self, commodity: &str) -> bool {
        let status = self.status.read().await;
        if status.circuit_breaker_active {
            return false;
        }

        let prices = self.prices.read().await;
        if let Some(price) = prices.get(commodity) {
            // Only allow trading on real oracle data, not simulated
            price.source != PriceSource::Simulated
        } else {
            false
        }
    }

    /// Start background price update task
    pub fn start_background_updates(self: Arc<Self>) {
        tokio::spawn(async move {
            loop {
                if let Err(e) = self.fetch_prices().await {
                    error!("Oracle update failed: {}", e);
                }

                // Update every 5 seconds
                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
            }
        });
    }
}

impl Default for OracleService {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// Backwards compatibility alias
// =============================================================================

pub type PriceService = OracleService;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_fetch_prices() {
        let service = OracleService::new();
        let result = service.fetch_prices().await;
        assert!(result.is_ok());

        let prices = service.get_all_prices().await;
        assert!(!prices.is_empty());

        // Check OIL price exists and has valid source
        if let Some(oil) = prices.get("OIL") {
            let price_usd = oil.price as f64 / 1_000_000.0;
            assert!(price_usd > 30.0 && price_usd < 200.0, "OIL price ${} out of range", price_usd);
            assert!(oil.source == PriceSource::Pyth || oil.source == PriceSource::TwelveData);
        }
    }

    #[tokio::test]
    async fn test_circuit_breaker() {
        let service = OracleService::new();
        let _ = service.fetch_prices().await;

        let status = service.get_status().await;
        // Circuit breaker should not be active if we got Pyth data
        if status.pyth_healthy {
            assert!(!status.circuit_breaker_active);
        }
    }
}
