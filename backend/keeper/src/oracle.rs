//! The market price as perps-core reads it (`oracle::read_price`,
//! docs/lp-pool.md D47), so the keeper sends no liquidation, ADL, settle or
//! funding crank the program would refuse on its price.
//!
//! The feed is a Pyth Solana Receiver `PriceUpdateV2`, parsed by the
//! program's own `pyth_receiver.rs` (`#[path]` in main.rs). The checks run in
//! the program's order: the pinned key (`Market.pyth_price_feed`), owner ==
//! `Market.oracle_program` (here the RPC account's owner), at least 133
//! bytes, the discriminator, a `Full` verification level, the feed id
//! (`Market.pyth_feed_id`), `pyth_receiver::is_fresh` (60 s old or 60 s ahead at most, D52), then
//! `validate_price` (positive and representable in 6 dp, confidence, floor).

use crate::ix::KeeperResult;
use crate::price::{self, PriceRejection};
use crate::pyth_receiver::{is_fresh, parse_price_update_v2, ParseRejection};
use crate::state::MarketData;
use solana_client::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use std::fmt;

/// Anchor `ErrorCode::ConstraintRaw`: the Accounts constraint
/// `pyth_price_feed.key() == market.pyth_price_feed`.
pub const ERR_CONSTRAINT_RAW: u32 = 2003;
/// `PerpsError` codes of the price checks (6000 + the variant's index).
pub const ERR_INVALID_ORACLE_PRICE: u32 = 6008;
pub const ERR_STALE_ORACLE_PRICE: u32 = 6009;
pub const ERR_ORACLE_CONFIDENCE_TOO_WIDE: u32 = 6029;
pub const ERR_ORACLE_PRICE_BELOW_FLOOR: u32 = 6030;
/// Wrong owner, size, discriminator or verification tag (D47).
pub const ERR_INVALID_ORACLE_ACCOUNT: u32 = 6031;
pub const ERR_ORACLE_NOT_FULLY_VERIFIED: u32 = 6093;
pub const ERR_ORACLE_FEED_ID_MISMATCH: u32 = 6094;

/// Why the program would refuse the market's price, in the order it checks.
#[derive(Debug, PartialEq, Eq)]
pub enum OracleRejection {
    /// Not the market's pinned `pyth_price_feed`.
    WrongAccount { key: Pubkey },
    /// Not owned by `Market.oracle_program`.
    WrongOwner { owner: Pubkey },
    /// Too short, another account type, a Partial update or an unknown
    /// verification tag.
    Unparsable(ParseRejection),
    /// A Full update for another feed.
    FeedIdMismatch { feed_id: [u8; 32] },
    /// More than `MAX_PRICE_AGE_SECS` old, or more than `MAX_FUTURE_PUBLISH_SECS` ahead of `now`.
    Stale { publish_time: i64, now: i64 },
    /// `validate_price` refused it.
    Price { rejection: PriceRejection, price: i64, conf: u64, expo: i32 },
}

impl OracleRejection {
    /// The error perps-core returns for this refusal.
    pub fn program_error(&self) -> u32 {
        match self {
            OracleRejection::WrongAccount { .. } => ERR_CONSTRAINT_RAW,
            OracleRejection::WrongOwner { .. } => ERR_INVALID_ORACLE_ACCOUNT,
            OracleRejection::Unparsable(ParseRejection::NotFullyVerified) => ERR_ORACLE_NOT_FULLY_VERIFIED,
            OracleRejection::Unparsable(
                ParseRejection::TooShort | ParseRejection::WrongDiscriminator | ParseRejection::BadVerificationLevel,
            ) => ERR_INVALID_ORACLE_ACCOUNT,
            OracleRejection::FeedIdMismatch { .. } => ERR_ORACLE_FEED_ID_MISMATCH,
            OracleRejection::Stale { .. } => ERR_STALE_ORACLE_PRICE,
            OracleRejection::Price { rejection: PriceRejection::Invalid, .. } => ERR_INVALID_ORACLE_PRICE,
            OracleRejection::Price { rejection: PriceRejection::ConfidenceTooWide, .. } => ERR_ORACLE_CONFIDENCE_TOO_WIDE,
            OracleRejection::Price { rejection: PriceRejection::BelowFloor, .. } => ERR_ORACLE_PRICE_BELOW_FLOOR,
        }
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{:02x}", x)).collect()
}

/// The reason, then the error the program would return, e.g. "Oracle price
/// is stale (published 1, now 100, max age 60 s); program error 6009".
impl fmt::Display for OracleRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OracleRejection::WrongAccount { key } => write!(f, "Oracle account {} is not the market's price feed", key),
            OracleRejection::WrongOwner { owner } => {
                write!(f, "Oracle account rejected: owned by {}, not the market's oracle program", owner)
            }
            OracleRejection::Unparsable(r) => write!(f, "Oracle account rejected: {:?}", r),
            OracleRejection::FeedIdMismatch { feed_id } => {
                write!(f, "Oracle account rejected: feed id {} is not the market's", hex(feed_id))
            }
            OracleRejection::Stale { publish_time, now } => {
                write!(f, "Oracle price is stale (published {}, now {}, max age {} s)", publish_time, now, price::MAX_PRICE_AGE_SECS)
            }
            OracleRejection::Price { rejection, price, conf, expo } => {
                write!(f, "Oracle price rejected: {:?} (price={}, conf={}, expo={})", rejection, price, conf, expo)
            }
        }?;
        write!(f, "; program error {}", self.program_error())
    }
}

impl std::error::Error for OracleRejection {}

/// Fetch the market's feed account and return the 6-decimal price the
/// program would read at `now`.
pub fn fetch_oracle_price(client: &RpcClient, market: &MarketData, now: i64) -> KeeperResult<u64> {
    let account = client.get_account(&market.pyth_price_feed)?;
    Ok(read_price(&market.pyth_price_feed, &account.owner, &account.data, market, now)?)
}

/// `oracle::read_price` on an account's key, owner and data.
pub fn read_price(key: &Pubkey, owner: &Pubkey, data: &[u8], market: &MarketData, now: i64) -> Result<u64, OracleRejection> {
    if *key != market.pyth_price_feed {
        return Err(OracleRejection::WrongAccount { key: *key });
    }
    if *owner != market.oracle_program {
        return Err(OracleRejection::WrongOwner { owner: *owner });
    }
    let update = parse_price_update_v2(data).map_err(OracleRejection::Unparsable)?;
    if update.feed_id != market.pyth_feed_id {
        return Err(OracleRejection::FeedIdMismatch { feed_id: update.feed_id });
    }
    if !is_fresh(update.publish_time, now, price::MAX_PRICE_AGE_SECS) {
        return Err(OracleRejection::Stale { publish_time: update.publish_time, now });
    }
    validated_price(update.price, update.conf, update.exponent, market)
}

/// `validate_price` with the market's confidence limit and floor.
pub fn validated_price(price: i64, conf: u64, expo: i32, market: &MarketData) -> Result<u64, OracleRejection> {
    price::validate_price(price, conf, expo, market.max_confidence_bps, market.min_oracle_price)
        .map_err(|rejection| OracleRejection::Price { rejection, price, conf, expo })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pyth_receiver::{PriceUpdateV2View, PRICE_UPDATE_V2_FULL_LEN, PRICE_UPDATE_V2_LEN};

    /// The SDK's bytes (pyth-solana-receiver-sdk 2.0.0) for perps-core's
    /// `fixture_view()` in tests/pyth_receiver_parity.rs: a Full update, and
    /// the same as Partial with 13 signatures.
    const FULL: &[u8] = include_bytes!("../../../programs/perps-core/tests/fixtures/price_update_v2_full.bin");
    const PARTIAL: &[u8] = include_bytes!("../../../programs/perps-core/tests/fixtures/price_update_v2_partial.bin");

    /// USOILSPOT, the feed id of the fixtures (and of the OIL test market).
    const OIL_FEED_ID: [u8; 32] = [
        0x92, 0x5c, 0xa9, 0x2f, 0xf0, 0x05, 0xae, 0x94, 0x3c, 0x15, 0x8e, 0x35, 0x63, 0xf5, 0x96, 0x98,
        0xce, 0x7e, 0x75, 0xc5, 0xa8, 0xc8, 0xdd, 0x43, 0x30, 0x3a, 0x0a, 0x15, 0x48, 0x87, 0xb3, 0xe6,
    ];
    /// XAU's feed id, the GOLD market's in perps-core's integration tests.
    const XAU_FEED_ID: [u8; 32] = [
        0x76, 0x5d, 0x2b, 0xa9, 0x06, 0xdb, 0xc3, 0x2c, 0xa1, 0x7c, 0xc1, 0x1f, 0x53, 0x10, 0xa8, 0x9e,
        0x9e, 0xe1, 0xf6, 0x42, 0x05, 0x08, 0xc6, 0x38, 0x61, 0xf2, 0xf8, 0xba, 0x4e, 0xe3, 0x4b, 0xb2,
    ];
    /// The fixtures' publish time (`testkit::WEDNESDAY`).
    const PUBLISHED: i64 = 1_794_416_400;
    /// The test oracle program of perps-core's tests.
    const ORACLE: Pubkey = Pubkey::new_from_array([0x50; 32]);

    fn feed() -> Pubkey {
        Pubkey::new_from_array([0x77; 32])
    }

    fn market() -> MarketData {
        MarketData {
            pyth_price_feed: feed(),
            pyth_feed_id: OIL_FEED_ID,
            oracle_program: ORACLE,
            min_oracle_price: 5_000_000,
            ..Default::default()
        }
    }

    fn read(data: &[u8], m: &MarketData, now: i64) -> Result<u64, OracleRejection> {
        read_price(&feed(), &ORACLE, data, m, now)
    }

    fn code(r: Result<u64, OracleRejection>) -> u32 {
        r.unwrap_err().program_error()
    }

    #[test]
    fn reads_the_sdks_full_fixture() {
        assert_eq!(FULL.len(), PRICE_UPDATE_V2_LEN);
        assert_eq!(crate::testkit::WEDNESDAY, PUBLISHED);
        // $75.12345678 ± $0.042, truncated to 6 dp
        assert_eq!(read(FULL, &market(), PUBLISHED), Ok(75_123_456));
        // The 134th byte is unused: the 133-byte serialization reads the same
        assert_eq!(read(&FULL[..PRICE_UPDATE_V2_FULL_LEN], &market(), PUBLISHED), Ok(75_123_456));
        // The keeper's writer (the program's) gives the SDK's bytes
        let mut v = PriceUpdateV2View::new(OIL_FEED_ID, 7_512_345_678, 4_200_000, -8, PUBLISHED);
        v.write_authority = [0x11; 32];
        v.prev_publish_time = PUBLISHED - 1;
        v.ema_price = 7_500_000_000;
        v.ema_conf = 5_000_000;
        v.posted_slot = 123_456_789;
        assert_eq!(v.to_bytes(), FULL);
        assert_eq!(v.to_partial_bytes(13), PARTIAL);
    }

    #[test]
    fn stale_both_ways_at_the_programs_max_age() {
        let m = market();
        assert_eq!(price::MAX_PRICE_AGE_SECS, 60);
        assert!(read(FULL, &m, PUBLISHED + 60).is_ok());
        assert!(read(FULL, &m, PUBLISHED - 60).is_ok()); // published ahead of the chain clock
        for now in [PUBLISHED + 61, PUBLISHED - 61, PUBLISHED + 3_600, 0] {
            assert_eq!(read(FULL, &m, now), Err(OracleRejection::Stale { publish_time: PUBLISHED, now }), "now {}", now);
        }
        assert_eq!(code(read(FULL, &m, PUBLISHED + 61)), ERR_STALE_ORACLE_PRICE);
        // A frozen mock (SetPrice with status 0/2/3) is just an old update
        let old = PriceUpdateV2View::new(OIL_FEED_ID, 7_500_000_000, 0, -8, PUBLISHED - 61).to_bytes();
        assert_eq!(code(read(&old, &m, PUBLISHED)), ERR_STALE_ORACLE_PRICE);
    }

    #[test]
    fn wrong_feed_id_is_refused() {
        let gold = MarketData { pyth_feed_id: XAU_FEED_ID, ..market() };
        assert_eq!(read(FULL, &gold, PUBLISHED), Err(OracleRejection::FeedIdMismatch { feed_id: OIL_FEED_ID }));
        assert_eq!(code(read(FULL, &gold, PUBLISHED)), ERR_ORACLE_FEED_ID_MISMATCH);
        // One bit off in the account's id, or a market with no id set
        let mut d = FULL.to_vec();
        d[41 + 31] ^= 1;
        assert_eq!(code(read(&d, &market(), PUBLISHED)), ERR_ORACLE_FEED_ID_MISMATCH);
        let unset = MarketData { pyth_feed_id: [0; 32], ..market() };
        assert_eq!(code(read(FULL, &unset, PUBLISHED)), ERR_ORACLE_FEED_ID_MISMATCH);
        // The id is checked before the age, as on chain
        assert_eq!(code(read(FULL, &gold, PUBLISHED + 61)), ERR_ORACLE_FEED_ID_MISMATCH);
    }

    #[test]
    fn partial_verification_is_refused() {
        assert_eq!(PARTIAL.len(), PRICE_UPDATE_V2_LEN);
        assert_eq!((PARTIAL[40], PARTIAL[41]), (0, 13));
        assert_eq!(read(PARTIAL, &market(), PUBLISHED), Err(OracleRejection::Unparsable(ParseRejection::NotFullyVerified)));
        assert_eq!(code(read(PARTIAL, &market(), PUBLISHED)), ERR_ORACLE_NOT_FULLY_VERIFIED);
        // Whatever its feed id or age: the level is checked first
        let gold = MarketData { pyth_feed_id: XAU_FEED_ID, ..market() };
        assert_eq!(code(read(PARTIAL, &gold, PUBLISHED + 3_600)), ERR_ORACLE_NOT_FULLY_VERIFIED);
        // The mock's SetVerification(0, n) writes the same layout
        let v = PriceUpdateV2View::new(OIL_FEED_ID, 7_500_000_000, 0, -8, PUBLISHED);
        assert_eq!(code(read(&v.to_partial_bytes(1), &market(), PUBLISHED)), ERR_ORACLE_NOT_FULLY_VERIFIED);
        // An unknown tag is not an account the program reads at all
        let mut d = FULL.to_vec();
        d[40] = 2;
        assert_eq!(read(&d, &market(), PUBLISHED), Err(OracleRejection::Unparsable(ParseRejection::BadVerificationLevel)));
        assert_eq!(code(read(&d, &market(), PUBLISHED)), ERR_INVALID_ORACLE_ACCOUNT);
    }

    #[test]
    fn owner_key_size_and_type_come_first() {
        let m = market();
        // The RPC account's owner must be the market's oracle program
        let other = Pubkey::new_from_array([0x51; 32]);
        assert_eq!(read_price(&feed(), &other, FULL, &m, PUBLISHED), Err(OracleRejection::WrongOwner { owner: other }));
        assert_eq!(code(read_price(&feed(), &other, FULL, &m, PUBLISHED)), ERR_INVALID_ORACLE_ACCOUNT);
        // Not the pinned key (the Accounts constraint refuses it first)
        let elsewhere = Pubkey::new_from_array([0x78; 32]);
        assert_eq!(code(read_price(&elsewhere, &other, PARTIAL, &m, PUBLISHED)), ERR_CONSTRAINT_RAW);
        // Too short, another account type, a legacy v2 price account
        assert_eq!(read(&FULL[..132], &m, PUBLISHED), Err(OracleRejection::Unparsable(ParseRejection::TooShort)));
        let mut d = FULL.to_vec();
        d[0] ^= 1;
        assert_eq!(read(&d, &m, PUBLISHED), Err(OracleRejection::Unparsable(ParseRejection::WrongDiscriminator)));
        let mut legacy = vec![0u8; 3_312];
        legacy[..4].copy_from_slice(&0xa1b2_c3d4u32.to_le_bytes());
        for data in [&FULL[..132], &d[..], &legacy[..], &[][..]] {
            assert_eq!(code(read(data, &m, PUBLISHED)), ERR_INVALID_ORACLE_ACCOUNT);
        }
    }

    #[test]
    fn price_checks_follow_the_feed_checks() {
        let m = market();
        let at = |price: i64, conf: u64, expo: i32| PriceUpdateV2View::new(OIL_FEED_ID, price, conf, expo, PUBLISHED).to_bytes();
        // Exactly 1% confidence passes, one unit more does not
        assert_eq!(read(&at(7_500_000_000, 75_000_000, -8), &m, PUBLISHED), Ok(75_000_000));
        assert_eq!(code(read(&at(7_500_000_000, 75_000_001, -8), &m, PUBLISHED)), ERR_ORACLE_CONFIDENCE_TOO_WIDE);
        // Below the $5 floor; negative (a negative WTI goes through settle_market)
        assert_eq!(code(read(&at(499_000_000, 0, -8), &m, PUBLISHED)), ERR_ORACLE_PRICE_BELOW_FLOOR);
        assert_eq!(code(read(&at(-3_763_000_000, 0, -8), &m, PUBLISHED)), ERR_INVALID_ORACLE_PRICE);
        // Exponents the program accepts
        assert_eq!(read(&at(75, 0, 0), &m, PUBLISHED), Ok(75_000_000));
        assert_eq!(read(&at(8, 0, 1), &m, PUBLISHED), Ok(80_000_000));
        assert_eq!(read(&at(75_000_000_000_000, 0, -12), &m, PUBLISHED), Ok(75_000_000));
        // A market's own confidence limit
        let tight = MarketData { max_confidence_bps: 5, ..m };
        assert_eq!(code(read(FULL, &tight, PUBLISHED)), ERR_ORACLE_CONFIDENCE_TOO_WIDE); // 0.056%
        // A stale bad price is reported as stale, as on chain
        assert_eq!(code(read(&at(-1, 0, -8), &market(), PUBLISHED + 61)), ERR_STALE_ORACLE_PRICE);
    }

    #[test]
    fn rejections_read_as_the_programs_errors() {
        assert!(OracleRejection::Stale { publish_time: 1, now: 100 }.to_string().contains("stale"));
        let e = OracleRejection::FeedIdMismatch { feed_id: OIL_FEED_ID }.to_string();
        assert!(e.contains("925ca92ff005ae94"), "{}", e);
        let e: Box<dyn std::error::Error + Send + Sync> = Box::new(OracleRejection::Unparsable(ParseRejection::NotFullyVerified));
        assert_eq!(e.to_string(), "Oracle account rejected: NotFullyVerified; program error 6093");
    }

    /// The codes are `PerpsError`'s: 6000 + the variant's index in errors.rs.
    #[test]
    fn error_codes_match_errors_rs() {
        let src = include_str!("../../../programs/perps-core/src/errors.rs");
        let body = &src[src.find("pub enum PerpsError").unwrap()..];
        let variants: Vec<&str> = body
            .lines()
            .map(str::trim)
            .filter(|l| l.ends_with(',') && l.chars().next().is_some_and(|c| c.is_ascii_uppercase()))
            .map(|l| l.trim_end_matches(','))
            .collect();
        let code_of = |name: &str| 6000 + variants.iter().position(|v| *v == name).unwrap_or_else(|| panic!("{}", name)) as u32;
        assert_eq!(code_of("InvalidOraclePrice"), ERR_INVALID_ORACLE_PRICE);
        assert_eq!(code_of("StaleOraclePrice"), ERR_STALE_ORACLE_PRICE);
        assert_eq!(code_of("OracleConfidenceTooWide"), ERR_ORACLE_CONFIDENCE_TOO_WIDE);
        assert_eq!(code_of("OraclePriceBelowFloor"), ERR_ORACLE_PRICE_BELOW_FLOOR);
        assert_eq!(code_of("InvalidOracleAccount"), ERR_INVALID_ORACLE_ACCOUNT);
        assert_eq!(code_of("OracleNotFullyVerified"), ERR_ORACLE_NOT_FULLY_VERIFIED);
        assert_eq!(code_of("OracleFeedIdMismatch"), ERR_ORACLE_FEED_ID_MISMATCH);
        // The ones ix.rs matches on, too
        assert_eq!(code_of("NotLiquidatable"), crate::ix::ERR_NOT_LIQUIDATABLE);
        assert_eq!(code_of("PositionAlreadyClosed"), crate::ix::ERR_POSITION_ALREADY_CLOSED);
        assert_eq!(code_of("PoolCashInsufficient"), crate::ix::ERR_POOL_CASH_INSUFFICIENT);
        assert_eq!(code_of("FundingNotDue"), crate::ix::ERR_FUNDING_NOT_DUE);
        assert_eq!(code_of("NothingToSettle"), crate::ix::ERR_NOTHING_TO_SETTLE);
        assert_eq!(code_of("SettleWouldBankrupt"), crate::ix::ERR_SETTLE_WOULD_BANKRUPT);
    }
}
