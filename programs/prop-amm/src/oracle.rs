use anchor_lang::prelude::*;
use crate::errors::PropAmmError;

// Shared with perps-core (and the keeper) so price handling cannot drift.
// `include!` rather than `#[path]`: Anchor's IDL parser can't follow `#[path]`.
include!("../../perps-core/src/price.rs");

/// perps-core's Pyth Receiver `PriceUpdateV2` reader (docs/lp-pool.md D47).
pub mod pyth_receiver {
    include!("../../perps-core/src/pyth_receiver.rs");
}

/// Pyth's program pairs and the push oracle's feed address (D52).
pub mod pyth_push {
    include!("../../perps-core/src/pyth_push.rs");
}

use pyth_receiver::{is_fresh, parse_price_update_v2, ParseRejection, PriceUpdateV2View};

/// A fully verified `PriceUpdateV2` owned by `oracle_program` for `feed_id`,
/// whatever its age, with perps-core's checks and order.
pub fn load_update(feed: &AccountInfo, oracle_program: &Pubkey, feed_id: &[u8; 32]) -> Result<PriceUpdateV2View> {
    require_keys_eq!(*feed.owner, *oracle_program, PropAmmError::InvalidOracleAccount);
    let update = {
        let data = feed.try_borrow_data().map_err(|_| PropAmmError::InvalidOracleAccount)?;
        parse_price_update_v2(&data).map_err(|rejection| match rejection {
            ParseRejection::NotFullyVerified => PropAmmError::OracleNotFullyVerified,
            ParseRejection::TooShort | ParseRejection::WrongDiscriminator | ParseRejection::BadVerificationLevel => {
                PropAmmError::InvalidOracleAccount
            }
        })?
    };
    require!(update.feed_id == *feed_id, PropAmmError::OracleFeedIdMismatch);
    Ok(update)
}

/// The vault's oracle price (6 dp): `load_update`, published within
/// `MAX_PRICE_AGE_SECS` of `now` (at most `MAX_FUTURE_PUBLISH_SECS` ahead,
/// D52), default 1% confidence limit.
pub fn read_vault_price(feed: &AccountInfo, oracle_program: &Pubkey, feed_id: &[u8; 32], now: i64) -> Result<u64> {
    let update = load_update(feed, oracle_program, feed_id)?;
    require!(is_fresh(update.publish_time, now, MAX_PRICE_AGE_SECS), PropAmmError::StaleOraclePrice);
    // Same price rules as perps-core: 6 decimals, default 1% confidence limit
    validate_price(update.price, update.conf, update.exponent, 0, 0).map_err(|r| {
        match r {
            PriceRejection::ConfidenceTooWide => PropAmmError::OracleConfidenceTooWide,
            _ => PropAmmError::InvalidOraclePrice,
        }
        .into()
    })
}
