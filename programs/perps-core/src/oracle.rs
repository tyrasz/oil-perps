use anchor_lang::prelude::*;
use pyth_sdk_solana::load_price_feed_from_account_info;
use crate::errors::PerpsError;
use crate::state::Market;

pub use crate::price::*;

/// Read the market's Pyth feed and return a validated 6-decimal price.
pub fn read_price(feed: &AccountInfo, market: &Market, now: i64) -> Result<u64> {
    let price_feed = load_price_feed_from_account_info(feed)
        .map_err(|_| PerpsError::InvalidOraclePrice)?;
    let price = price_feed
        .get_price_no_older_than(now, MAX_PRICE_AGE_SECS)
        .ok_or(PerpsError::StaleOraclePrice)?;

    validate_price(price.price, price.conf, price.expo, market.max_confidence_bps, market.min_oracle_price)
        .map_err(|rejection| {
            msg!("Oracle price rejected: {:?} (price={}, conf={}, expo={})", rejection, price.price, price.conf, price.expo);
            match rejection {
                PriceRejection::Invalid => PerpsError::InvalidOraclePrice,
                PriceRejection::ConfidenceTooWide => PerpsError::OracleConfidenceTooWide,
                PriceRejection::BelowFloor => PerpsError::OraclePriceBelowFloor,
            }
            .into()
        })
}
