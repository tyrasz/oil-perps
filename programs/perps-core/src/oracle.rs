use anchor_lang::prelude::*;
use crate::errors::PerpsError;
use crate::pyth_receiver::{is_fresh, parse_price_update_v2, ParseRejection, PriceUpdateV2View};
use crate::state::Market;

pub use crate::price::*;

/// Read the market's Pyth feed and return a validated 6-decimal price.
pub fn read_price(feed: &AccountInfo, market: &Market, now: i64) -> Result<u64> {
    read_price_max_age(feed, market, now, MAX_PRICE_AGE_SECS)
}

/// `read_price` with a tighter staleness limit, for venues whose quotes must
/// not be filled against an old price.
///
/// Checks, in order (docs/lp-pool.md D47): the pinned account
/// (`market.pyth_price_feed`; every caller's Accounts constraint already
/// refuses another key with ConstraintRaw), its owner
/// (`market.oracle_program`), a `Full` Pyth Receiver `PriceUpdateV2` for
/// `market.pyth_feed_id`, `pyth_receiver::is_fresh` (at most `max_age_secs`
/// old, at most `MAX_FUTURE_PUBLISH_SECS` ahead; D52), then
/// `validate_price` (confidence, floor, 6-dp normalization).
#[inline(never)]
pub fn read_price_max_age(feed: &AccountInfo, market: &Market, now: i64, max_age_secs: u64) -> Result<u64> {
    require_keys_eq!(*feed.key, market.pyth_price_feed, PerpsError::InvalidOracleAccount);
    let update = load_update(feed, &market.oracle_program, &market.pyth_feed_id)?;
    require!(is_fresh(update.publish_time, now, max_age_secs), PerpsError::StaleOraclePrice);

    validate_price(update.price, update.conf, update.exponent, market.max_confidence_bps, market.min_oracle_price)
        .map_err(|rejection| {
            msg!("Oracle price rejected: {:?} (price={}, conf={}, expo={})", rejection, update.price, update.conf, update.exponent);
            match rejection {
                PriceRejection::Invalid => PerpsError::InvalidOraclePrice,
                PriceRejection::ConfidenceTooWide => PerpsError::OracleConfidenceTooWide,
                PriceRejection::BelowFloor => PerpsError::OraclePriceBelowFloor,
            }
            .into()
        })
}

/// A fully verified `PriceUpdateV2` owned by `oracle_program` for `feed_id`,
/// whatever its age or price. `initialize_market` and the feed rotation
/// also check the address (`load_pinnable_update`).
#[inline(never)]
pub fn load_update(feed: &AccountInfo, oracle_program: &Pubkey, feed_id: &[u8; 32]) -> Result<PriceUpdateV2View> {
    require_keys_eq!(*feed.owner, *oracle_program, PerpsError::InvalidOracleAccount);
    let update = {
        let data = feed.try_borrow_data().map_err(|_| PerpsError::InvalidOracleAccount)?;
        parse_price_update_v2(&data).map_err(|rejection| -> PerpsError {
            msg!("Oracle account rejected: {:?}", rejection);
            match rejection {
                ParseRejection::NotFullyVerified => PerpsError::OracleNotFullyVerified,
                ParseRejection::TooShort | ParseRejection::WrongDiscriminator | ParseRejection::BadVerificationLevel => {
                    PerpsError::InvalidOracleAccount
                }
            }
        })?
    };
    require!(update.feed_id == *feed_id, PerpsError::OracleFeedIdMismatch);
    Ok(update)
}

/// `load_update`, then, where `oracle_program` is a Pyth Receiver, that
/// `feed` is the push oracle's shard-0 account for `feed_id`
/// (`pyth_push::is_pinnable_feed`, D52); `InvalidOracleAccount` otherwise.
/// What `initialize_market` and the feed rotation accept as a market's feed.
#[inline(never)]
pub fn load_pinnable_update(feed: &AccountInfo, oracle_program: &Pubkey, feed_id: &[u8; 32]) -> Result<PriceUpdateV2View> {
    let update = load_update(feed, oracle_program, feed_id)?;
    if !crate::pyth_push::is_pinnable_feed(feed.key, oracle_program, feed_id) {
        msg!("Oracle account rejected: not the push oracle's shard-0 feed account for this feed id");
        return err!(PerpsError::InvalidOracleAccount);
    }
    Ok(update)
}
