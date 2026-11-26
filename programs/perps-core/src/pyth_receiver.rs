// Pyth Solana Receiver `PriceUpdateV2` reader and writer, free of Anchor,
// Solana and program types so prop-amm (`include!`), mock-pyth (`#[path]`),
// the keeper (`#[path]`) and the tests share it. Keep it dependency-free, and
// keep `//` comments: `include!` rejects `//!`.
//
// The layout is pyth-solana-receiver-sdk 2.0.0's `PriceUpdateV2` (an Anchor
// `#[account]`, Borsh), pinned by a parity test against the SDK itself
// (`tests/pyth_receiver_parity.rs`):
//
//   [0..8)    discriminator sha256("account:PriceUpdateV2")[..8]
//   [8..40)   write_authority: Pubkey
//   [40]      verification_level tag: 0 = Partial { num_signatures: u8 }, 1 = Full
//   Full (the only level accepted; 133 bytes serialized, accounts are 134):
//   [41..73)  feed_id [u8; 32]
//   [73..81)  price i64
//   [81..89)  conf u64
//   [89..93)  exponent i32
//   [93..101) publish_time i64
//   [101..109) prev_publish_time i64
//   [109..117) ema_price i64
//   [117..125) ema_conf u64
//   [125..133) posted_slot u64
//   Partial: num_signatures at 41 and every later field one byte further.
//
// Why by hand and not the SDK (docs/lp-pool.md D47): the SDK's
// `Account<PriceUpdateV2>` fixes the owner to the Receiver's program id (no
// localnet mock), its `get_price_no_older_than` checks age one way only
// (any future publish time passes; `is_fresh` bounds it, D52), and it brings pythnet-sdk and its
// dependencies on chain; this file is also shared with code that has no
// Anchor at all.

/// `PriceUpdateV2::LEN`: the size the Receiver allocates.
pub const PRICE_UPDATE_V2_LEN: usize = 134;
/// Bytes a `Full` update serializes to; the shortest account accepted.
pub const PRICE_UPDATE_V2_FULL_LEN: usize = 133;
/// sha256("account:PriceUpdateV2")[..8]
pub const PRICE_UPDATE_V2_DISCRIMINATOR: [u8; 8] = [34, 241, 35, 99, 157, 126, 244, 205];
/// `VerificationLevel` Borsh tags.
pub const VERIFICATION_PARTIAL: u8 = 0;
pub const VERIFICATION_FULL: u8 = 1;

/// Byte offsets of a `Full` update (with the discriminator).
pub mod offsets {
    pub const DISCRIMINATOR: usize = 0;
    pub const WRITE_AUTHORITY: usize = 8;
    pub const VERIFICATION_LEVEL: usize = 40;
    pub const FEED_ID: usize = 41;
    pub const PRICE: usize = 73;
    pub const CONF: usize = 81;
    pub const EXPONENT: usize = 89;
    pub const PUBLISH_TIME: usize = 93;
    pub const PREV_PUBLISH_TIME: usize = 101;
    pub const EMA_PRICE: usize = 109;
    pub const EMA_CONF: usize = 117;
    pub const POSTED_SLOT: usize = 125;
    /// A `Partial` update stores `num_signatures` here and shifts every
    /// later field by one byte.
    pub const PARTIAL_NUM_SIGNATURES: usize = 41;
    pub const PARTIAL_SHIFT: usize = 1;
}

/// A `Full` update's fields.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PriceUpdateV2View {
    pub write_authority: [u8; 32],
    pub feed_id: [u8; 32],
    pub price: i64,
    pub conf: u64,
    pub exponent: i32,
    pub publish_time: i64,
    pub prev_publish_time: i64,
    pub ema_price: i64,
    pub ema_conf: u64,
    pub posted_slot: u64,
}

/// Why an account is not a usable `PriceUpdateV2`, in the order checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseRejection {
    /// Fewer than `PRICE_UPDATE_V2_FULL_LEN` bytes.
    TooShort,
    WrongDiscriminator,
    /// `VerificationLevel::Partial`: fewer guardian signatures than a full quorum.
    NotFullyVerified,
    /// A verification tag that is neither Partial nor Full.
    BadVerificationLevel,
}

fn rd8(d: &[u8], at: usize) -> [u8; 8] {
    let mut b = [0u8; 8];
    b.copy_from_slice(&d[at..at + 8]);
    b
}

fn rd_i64(d: &[u8], at: usize) -> i64 {
    i64::from_le_bytes(rd8(d, at))
}

fn rd_u64(d: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(rd8(d, at))
}

fn rd_i32(d: &[u8], at: usize) -> i32 {
    let mut b = [0u8; 4];
    b.copy_from_slice(&d[at..at + 4]);
    i32::from_le_bytes(b)
}

fn rd32(d: &[u8], at: usize) -> [u8; 32] {
    let mut b = [0u8; 32];
    b.copy_from_slice(&d[at..at + 32]);
    b
}

/// Parse a `Full` `PriceUpdateV2`. Checks, in order: length, discriminator,
/// verification level. Nothing else (owner, feed id, age) is checked here.
pub fn parse_price_update_v2(data: &[u8]) -> Result<PriceUpdateV2View, ParseRejection> {
    use offsets as o;
    if data.len() < PRICE_UPDATE_V2_FULL_LEN {
        return Err(ParseRejection::TooShort);
    }
    if data[o::DISCRIMINATOR..o::DISCRIMINATOR + 8] != PRICE_UPDATE_V2_DISCRIMINATOR {
        return Err(ParseRejection::WrongDiscriminator);
    }
    match data[o::VERIFICATION_LEVEL] {
        VERIFICATION_FULL => {}
        VERIFICATION_PARTIAL => return Err(ParseRejection::NotFullyVerified),
        _ => return Err(ParseRejection::BadVerificationLevel),
    }
    Ok(PriceUpdateV2View {
        write_authority: rd32(data, o::WRITE_AUTHORITY),
        feed_id: rd32(data, o::FEED_ID),
        price: rd_i64(data, o::PRICE),
        conf: rd_u64(data, o::CONF),
        exponent: rd_i32(data, o::EXPONENT),
        publish_time: rd_i64(data, o::PUBLISH_TIME),
        prev_publish_time: rd_i64(data, o::PREV_PUBLISH_TIME),
        ema_price: rd_i64(data, o::EMA_PRICE),
        ema_conf: rd_u64(data, o::EMA_CONF),
        posted_slot: rd_u64(data, o::POSTED_SLOT),
    })
}

/// How far a publish time may be ahead of the cluster clock, whatever the
/// age limit (docs/lp-pool.md D52). `publish_time` is Pythnet/publisher wall
/// time and `Clock::unix_timestamp` is the cluster's stake-weighted estimate,
/// which can run seconds behind it, so a just-pushed price can be "in the
/// future"; with a venue's 1–15 s age limit a two-sided check would refuse
/// the freshest price there is. Equal to `MAX_PRICE_AGE_SECS`, so reads at
/// that limit are what they were (±60 s).
pub const MAX_FUTURE_PUBLISH_SECS: u64 = 60;

/// Whether a price published at `publish_time` may be used at `now`: at
/// most `max_age_secs` old (`now − publish_time ≤ max_age_secs`) and at most
/// `MAX_FUTURE_PUBLISH_SECS` ahead of the clock (`publish_time − now`). The
/// SDK's `get_price_no_older_than` checks only the age; the future bound is
/// a sanity limit on a clock lag, separate from the age limit.
pub fn is_fresh(publish_time: i64, now: i64, max_age_secs: u64) -> bool {
    let ahead = publish_time as i128 - now as i128;
    if ahead >= 0 {
        ahead as u128 <= MAX_FUTURE_PUBLISH_SECS as u128
    } else {
        (-ahead) as u128 <= max_age_secs as u128
    }
}

/// Write `v` as a `Full` update into `buf[..PRICE_UPDATE_V2_FULL_LEN]`
/// (discriminator included); later bytes are left as they are.
pub fn write_price_update_v2(buf: &mut [u8], v: &PriceUpdateV2View) {
    write_fields(buf, v, VERIFICATION_FULL, 0);
}

/// Write `v` as a `Partial { num_signatures }` update (134 bytes), as the
/// Receiver stores a partially verified one. Tests and the mock only.
pub fn write_price_update_v2_partial(buf: &mut [u8], v: &PriceUpdateV2View, num_signatures: u8) {
    write_fields(buf, v, VERIFICATION_PARTIAL, num_signatures);
}

fn write_fields(buf: &mut [u8], v: &PriceUpdateV2View, level: u8, num_signatures: u8) {
    use offsets as o;
    buf[o::DISCRIMINATOR..o::DISCRIMINATOR + 8].copy_from_slice(&PRICE_UPDATE_V2_DISCRIMINATOR);
    buf[o::WRITE_AUTHORITY..o::WRITE_AUTHORITY + 32].copy_from_slice(&v.write_authority);
    buf[o::VERIFICATION_LEVEL] = level;
    let s = if level == VERIFICATION_PARTIAL {
        buf[o::PARTIAL_NUM_SIGNATURES] = num_signatures;
        o::PARTIAL_SHIFT
    } else {
        0
    };
    buf[s + o::FEED_ID..s + o::FEED_ID + 32].copy_from_slice(&v.feed_id);
    buf[s + o::PRICE..s + o::PRICE + 8].copy_from_slice(&v.price.to_le_bytes());
    buf[s + o::CONF..s + o::CONF + 8].copy_from_slice(&v.conf.to_le_bytes());
    buf[s + o::EXPONENT..s + o::EXPONENT + 4].copy_from_slice(&v.exponent.to_le_bytes());
    buf[s + o::PUBLISH_TIME..s + o::PUBLISH_TIME + 8].copy_from_slice(&v.publish_time.to_le_bytes());
    buf[s + o::PREV_PUBLISH_TIME..s + o::PREV_PUBLISH_TIME + 8].copy_from_slice(&v.prev_publish_time.to_le_bytes());
    buf[s + o::EMA_PRICE..s + o::EMA_PRICE + 8].copy_from_slice(&v.ema_price.to_le_bytes());
    buf[s + o::EMA_CONF..s + o::EMA_CONF + 8].copy_from_slice(&v.ema_conf.to_le_bytes());
    buf[s + o::POSTED_SLOT..s + o::POSTED_SLOT + 8].copy_from_slice(&v.posted_slot.to_le_bytes());
}

impl PriceUpdateV2View {
    /// A Full update of `price * 10^exponent` ± `conf`, published (and
    /// previously published) at `publish_time`, EMA equal to the price.
    pub fn new(feed_id: [u8; 32], price: i64, conf: u64, exponent: i32, publish_time: i64) -> Self {
        PriceUpdateV2View {
            write_authority: [0; 32],
            feed_id,
            price,
            conf,
            exponent,
            publish_time,
            prev_publish_time: publish_time,
            ema_price: price,
            ema_conf: conf,
            posted_slot: 0,
        }
    }

    /// The account's `PRICE_UPDATE_V2_LEN` bytes as a `Full` update.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut d = vec![0u8; PRICE_UPDATE_V2_LEN];
        write_price_update_v2(&mut d, self);
        d
    }

    /// The account's `PRICE_UPDATE_V2_LEN` bytes as a `Partial` update.
    pub fn to_partial_bytes(&self, num_signatures: u8) -> Vec<u8> {
        let mut d = vec![0u8; PRICE_UPDATE_V2_LEN];
        write_price_update_v2_partial(&mut d, self, num_signatures);
        d
    }
}

/// Restamp an update's `publish_time` and `prev_publish_time` in place (Full
/// or Partial layout, by its tag), as the test harnesses do when they warp
/// the clock. Other bytes are left as they are.
pub fn set_publish_times(d: &mut [u8], unix_timestamp: i64) {
    use offsets as o;
    let s = if d[o::VERIFICATION_LEVEL] == VERIFICATION_PARTIAL { o::PARTIAL_SHIFT } else { 0 };
    d[s + o::PUBLISH_TIME..s + o::PUBLISH_TIME + 8].copy_from_slice(&unix_timestamp.to_le_bytes());
    d[s + o::PREV_PUBLISH_TIME..s + o::PREV_PUBLISH_TIME + 8].copy_from_slice(&unix_timestamp.to_le_bytes());
}

#[cfg(test)]
mod pyth_receiver_tests {
    use super::*;

    // A Full update built byte by byte from the documented offsets,
    // independent of the writer.
    fn fixture() -> Vec<u8> {
        let mut d = vec![0u8; 134];
        d[0..8].copy_from_slice(&[0x22, 0xf1, 0x23, 0x63, 0x9d, 0x7e, 0xf4, 0xcd]);
        d[8..40].copy_from_slice(&[0xaa; 32]);
        d[40] = 1;
        for (i, b) in d[41..73].iter_mut().enumerate() {
            *b = i as u8 + 1;
        }
        d[73..81].copy_from_slice(&7_512_345_678i64.to_le_bytes());
        d[81..89].copy_from_slice(&1_234u64.to_le_bytes());
        d[89..93].copy_from_slice(&(-8i32).to_le_bytes());
        d[93..101].copy_from_slice(&1_000i64.to_le_bytes());
        d[101..109].copy_from_slice(&999i64.to_le_bytes());
        d[109..117].copy_from_slice(&(-5i64).to_le_bytes());
        d[117..125].copy_from_slice(&77u64.to_le_bytes());
        d[125..133].copy_from_slice(&u64::MAX.to_le_bytes());
        d[133] = 0xee; // unused; the reader must not care
        d
    }

    fn feed_id() -> [u8; 32] {
        let mut f = [0u8; 32];
        for (i, b) in f.iter_mut().enumerate() {
            *b = i as u8 + 1;
        }
        f
    }

    fn expected() -> PriceUpdateV2View {
        PriceUpdateV2View {
            write_authority: [0xaa; 32],
            feed_id: feed_id(),
            price: 7_512_345_678,
            conf: 1_234,
            exponent: -8,
            publish_time: 1_000,
            prev_publish_time: 999,
            ema_price: -5,
            ema_conf: 77,
            posted_slot: u64::MAX,
        }
    }

    #[test]
    fn reads_every_field_at_its_offset() {
        assert_eq!(parse_price_update_v2(&fixture()), Ok(expected()));
        // 133 bytes (the Borsh length) is enough; longer is fine
        assert_eq!(parse_price_update_v2(&fixture()[..133]), Ok(expected()));
        let mut long = fixture();
        long.extend_from_slice(&[9; 50]);
        assert_eq!(parse_price_update_v2(&long), Ok(expected()));
    }

    #[test]
    fn discriminator_is_anchors_for_the_account_name() {
        // sha256("account:PriceUpdateV2") starts 22f123639d7ef4cd
        assert_eq!(PRICE_UPDATE_V2_DISCRIMINATOR, [0x22, 0xf1, 0x23, 0x63, 0x9d, 0x7e, 0xf4, 0xcd]);
    }

    #[test]
    fn rejects_short_buffers_first() {
        let d = fixture();
        assert_eq!(parse_price_update_v2(&d[..132]), Err(ParseRejection::TooShort));
        assert_eq!(parse_price_update_v2(&[]), Err(ParseRejection::TooShort));
        // Too short wins over a bad discriminator
        let mut bad = d[..100].to_vec();
        bad[0] ^= 1;
        assert_eq!(parse_price_update_v2(&bad), Err(ParseRejection::TooShort));
    }

    #[test]
    fn rejects_a_wrong_discriminator() {
        for i in 0..8 {
            let mut d = fixture();
            d[i] ^= 0x80;
            assert_eq!(parse_price_update_v2(&d), Err(ParseRejection::WrongDiscriminator), "byte {}", i);
        }
        // A legacy v2 price account (magic 0xa1b2c3d4) is not an update
        let mut legacy = vec![0u8; 3_312];
        legacy[0..4].copy_from_slice(&0xa1b2_c3d4u32.to_le_bytes());
        assert_eq!(parse_price_update_v2(&legacy), Err(ParseRejection::WrongDiscriminator));
        // Nor is an all-zero (uninitialized) account
        assert_eq!(parse_price_update_v2(&[0u8; 134]), Err(ParseRejection::WrongDiscriminator));
    }

    #[test]
    fn rejects_partial_and_unknown_verification() {
        let mut d = fixture();
        d[40] = 0;
        assert_eq!(parse_price_update_v2(&d), Err(ParseRejection::NotFullyVerified));
        for tag in [2u8, 3, 0x80, 0xff] {
            d[40] = tag;
            assert_eq!(parse_price_update_v2(&d), Err(ParseRejection::BadVerificationLevel), "tag {}", tag);
        }
        // A Partial account written by the writer is refused too
        let p = expected().to_partial_bytes(5);
        assert_eq!(p.len(), 134);
        assert_eq!(parse_price_update_v2(&p), Err(ParseRejection::NotFullyVerified));
    }

    #[test]
    fn partial_layout_shifts_fields_by_one() {
        let p = expected().to_partial_bytes(13);
        assert_eq!((p[40], p[41]), (0, 13));
        assert_eq!(&p[42..74], &feed_id());
        assert_eq!(&p[74..82], &7_512_345_678i64.to_le_bytes());
        assert_eq!(&p[94..102], &1_000i64.to_le_bytes());
        assert_eq!(&p[126..134], &u64::MAX.to_le_bytes());
    }

    #[test]
    fn writer_matches_the_fixture() {
        let w = expected().to_bytes();
        let f = fixture();
        assert_eq!(w.len(), PRICE_UPDATE_V2_LEN);
        assert_eq!(&w[..133], &f[..133]);
        assert_eq!(w[133], 0);
        assert_eq!(parse_price_update_v2(&w), Ok(expected()));
    }

    #[test]
    fn restamping_moves_both_publish_times() {
        let mut d = expected().to_bytes();
        set_publish_times(&mut d, 5_000);
        let v = parse_price_update_v2(&d).unwrap();
        assert_eq!((v.publish_time, v.prev_publish_time), (5_000, 5_000));
        assert_eq!(PriceUpdateV2View { publish_time: 1_000, prev_publish_time: 999, ..v }, expected());
        // Partial: the shifted fields
        let mut p = expected().to_partial_bytes(1);
        set_publish_times(&mut p, 6_000);
        assert_eq!(&p[94..102], &6_000i64.to_le_bytes());
        assert_eq!(&p[102..110], &6_000i64.to_le_bytes());
        assert_eq!(&p[74..82], &7_512_345_678i64.to_le_bytes());
    }

    #[test]
    fn freshness_is_two_sided_and_inclusive() {
        for (now, fresh) in [(10_060, true), (10_061, false), (9_940, true), (9_939, false), (10_000, true)] {
            assert_eq!(is_fresh(10_000, now, 60), fresh, "now {}", now);
        }
        assert!(is_fresh(10_000, 10_000, 0));
        assert!(!is_fresh(10_000, 10_001, 0));
        // Ahead of the clock: the fixed future bound, whatever the age limit (D52)
        for max_age in [0u64, 1, 15, 60, 3_600] {
            assert!(is_fresh(10_001, 10_000, max_age), "max_age {}", max_age);
            assert!(is_fresh(10_060, 10_000, max_age), "max_age {}", max_age);
            assert!(!is_fresh(10_061, 10_000, max_age), "max_age {}", max_age);
        }
        assert_eq!(MAX_FUTURE_PUBLISH_SECS, 60);
        // Extreme clocks are stale, not an overflow
        assert!(!is_fresh(i64::MIN, i64::MAX, u64::MAX - 1));
        assert!(is_fresh(i64::MIN, i64::MAX, u64::MAX));
        assert!(!is_fresh(i64::MAX, i64::MIN, 60));
    }

    #[test]
    fn new_sets_prev_and_ema_from_the_price() {
        let v = PriceUpdateV2View::new([7; 32], 5, 6, -3, 77);
        assert_eq!((v.prev_publish_time, v.ema_price, v.ema_conf, v.posted_slot), (77, 5, 6, 0));
        assert_eq!(parse_price_update_v2(&v.to_bytes()), Ok(v));
    }
}
