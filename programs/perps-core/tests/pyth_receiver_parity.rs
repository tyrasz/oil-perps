//! Pins `src/pyth_receiver.rs` (the hand-written `PriceUpdateV2` reader the
//! program uses) to pyth-solana-receiver-sdk 2.0.0's own types: accounts
//! the SDK serializes parse to the same fields, and what our writer produces
//! the SDK deserializes to the same struct (docs/lp-pool.md D47).

use anchor_lang::prelude::{Clock, Pubkey};
use anchor_lang::{AccountDeserialize, AccountSerialize, Discriminator};
use perps_core::pyth_receiver::{
    is_fresh, parse_price_update_v2, write_price_update_v2, ParseRejection, PriceUpdateV2View, PRICE_UPDATE_V2_DISCRIMINATOR,
    PRICE_UPDATE_V2_FULL_LEN, PRICE_UPDATE_V2_LEN,
};
use pyth_solana_receiver_sdk::price_update::{PriceFeedMessage, PriceUpdateV2, VerificationLevel};

/// Deterministic pseudo-random values (splitmix64), so failures reproduce.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn bytes32(&mut self) -> [u8; 32] {
        let mut b = [0u8; 32];
        for c in b.chunks_mut(8) {
            c.copy_from_slice(&self.next().to_le_bytes());
        }
        b
    }

    fn view(&mut self) -> PriceUpdateV2View {
        PriceUpdateV2View {
            write_authority: self.bytes32(),
            feed_id: self.bytes32(),
            price: self.next() as i64,
            conf: self.next(),
            exponent: self.next() as i32,
            publish_time: self.next() as i64,
            prev_publish_time: self.next() as i64,
            ema_price: self.next() as i64,
            ema_conf: self.next(),
            posted_slot: self.next(),
        }
    }
}

fn sdk(v: &PriceUpdateV2View, level: VerificationLevel) -> PriceUpdateV2 {
    PriceUpdateV2 {
        write_authority: Pubkey::new_from_array(v.write_authority),
        verification_level: level,
        price_message: PriceFeedMessage {
            feed_id: v.feed_id,
            price: v.price,
            conf: v.conf,
            exponent: v.exponent,
            publish_time: v.publish_time,
            prev_publish_time: v.prev_publish_time,
            ema_price: v.ema_price,
            ema_conf: v.ema_conf,
        },
        posted_slot: v.posted_slot,
    }
}

/// The SDK's serialization in a `PriceUpdateV2::LEN` account, as the
/// Receiver allocates it.
fn sdk_account(u: &PriceUpdateV2) -> Vec<u8> {
    let mut d = Vec::new();
    u.try_serialize(&mut d).unwrap();
    d.resize(PriceUpdateV2::LEN, 0);
    d
}

fn view_of(u: &PriceUpdateV2) -> PriceUpdateV2View {
    let m = &u.price_message;
    PriceUpdateV2View {
        write_authority: u.write_authority.to_bytes(),
        feed_id: m.feed_id,
        price: m.price,
        conf: m.conf,
        exponent: m.exponent,
        publish_time: m.publish_time,
        prev_publish_time: m.prev_publish_time,
        ema_price: m.ema_price,
        ema_conf: m.ema_conf,
        posted_slot: u.posted_slot,
    }
}

#[test]
fn sizes_and_discriminator_match_the_sdk() {
    assert_eq!(PriceUpdateV2::LEN, PRICE_UPDATE_V2_LEN);
    assert_eq!(PriceUpdateV2::DISCRIMINATOR, &PRICE_UPDATE_V2_DISCRIMINATOR[..]);
    let full = sdk(&PriceUpdateV2View::default(), VerificationLevel::Full);
    let mut d = Vec::new();
    full.try_serialize(&mut d).unwrap();
    assert_eq!(d.len(), PRICE_UPDATE_V2_FULL_LEN);
    let partial = sdk(&PriceUpdateV2View::default(), VerificationLevel::Partial { num_signatures: 3 });
    let mut d = Vec::new();
    partial.try_serialize(&mut d).unwrap();
    assert_eq!(d.len(), PRICE_UPDATE_V2_LEN);
}

#[test]
fn sdk_full_updates_parse_to_the_same_fields() {
    let mut rng = Rng(1);
    for _ in 0..5_000 {
        let v = rng.view();
        let d = sdk_account(&sdk(&v, VerificationLevel::Full));
        assert_eq!(parse_price_update_v2(&d), Ok(v));
        // The Borsh length alone parses too
        assert_eq!(parse_price_update_v2(&d[..PRICE_UPDATE_V2_FULL_LEN]), Ok(v));
    }
}

#[test]
fn sdk_partial_updates_are_refused() {
    let mut rng = Rng(2);
    for _ in 0..1_000 {
        let v = rng.view();
        let n = rng.next() as u8;
        let d = sdk_account(&sdk(&v, VerificationLevel::Partial { num_signatures: n }));
        assert_eq!(parse_price_update_v2(&d), Err(ParseRejection::NotFullyVerified));
        // Our Partial writer produces the SDK's bytes
        assert_eq!(v.to_partial_bytes(n), d);
    }
}

#[test]
fn our_writer_deserializes_with_the_sdk() {
    let mut rng = Rng(3);
    for _ in 0..5_000 {
        let v = rng.view();
        let mut d = vec![0xa5u8; PRICE_UPDATE_V2_LEN];
        write_price_update_v2(&mut d, &v);
        let u = PriceUpdateV2::try_deserialize(&mut d.as_slice()).unwrap();
        assert_eq!(u.verification_level, VerificationLevel::Full);
        assert_eq!(view_of(&u), v);
        assert_eq!(&d[..PRICE_UPDATE_V2_FULL_LEN], &sdk_account(&u)[..PRICE_UPDATE_V2_FULL_LEN]);

        let n = rng.next() as u8;
        let p = PriceUpdateV2::try_deserialize(&mut v.to_partial_bytes(n).as_slice()).unwrap();
        assert_eq!(p.verification_level, VerificationLevel::Partial { num_signatures: n });
        assert_eq!(view_of(&p), v);
    }
}

#[test]
fn sdk_and_reader_agree_on_rejections() {
    let v = Rng(4).view();
    let good = sdk_account(&sdk(&v, VerificationLevel::Full));
    // Wrong discriminator: both refuse
    let mut d = good.clone();
    d[3] ^= 1;
    assert!(PriceUpdateV2::try_deserialize(&mut d.as_slice()).is_err());
    assert_eq!(parse_price_update_v2(&d), Err(ParseRejection::WrongDiscriminator));
    // Truncated: both refuse
    let d = &good[..PRICE_UPDATE_V2_FULL_LEN - 1];
    assert!(PriceUpdateV2::try_deserialize(&mut &d[..]).is_err());
    assert_eq!(parse_price_update_v2(d), Err(ParseRejection::TooShort));
    // An unknown verification tag: both refuse
    let mut d = good.clone();
    d[40] = 2;
    assert!(PriceUpdateV2::try_deserialize(&mut d.as_slice()).is_err());
    assert_eq!(parse_price_update_v2(&d), Err(ParseRejection::BadVerificationLevel));
}

/// Where the two freshness rules differ, on purpose: the SDK accepts any
/// publish time ahead of the clock, `is_fresh` one up to
/// `MAX_FUTURE_PUBLISH_SECS` (60 s, D52) ahead, whatever the age limit.
#[test]
fn freshness_is_the_sdks_plus_the_future_side() {
    let feed = [9u8; 32];
    let v = PriceUpdateV2View::new(feed, 7_500_000_000, 0, -8, 10_000);
    let u = sdk(&v, VerificationLevel::Full);
    let at = |t: i64| Clock { unix_timestamp: t, ..Clock::default() };
    for now in [9_000i64, 9_939, 9_940, 10_000, 10_060, 10_061, 11_000] {
        let sdk_ok = u.get_price_no_older_than(&at(now), 60, &feed).is_ok();
        let ours = is_fresh(v.publish_time, now, 60);
        if now >= 10_000 {
            assert_eq!(sdk_ok, ours, "now {}", now);
        } else {
            assert!(sdk_ok, "now {}", now);
            assert_eq!(ours, now >= 9_940, "now {}", now);
        }
    }
    // Feed id and verification are checked by the SDK too
    assert!(u.get_price_no_older_than(&at(10_000), 60, &[8u8; 32]).is_err());
    let p = sdk(&v, VerificationLevel::Partial { num_signatures: 13 });
    assert!(p.get_price_no_older_than(&at(10_000), 60, &feed).is_err());
}

/// The fixture view: recognisable values, the OIL (USOILSPOT) feed id.
fn fixture_view() -> PriceUpdateV2View {
    let feed_id = [
        0x92, 0x5c, 0xa9, 0x2f, 0xf0, 0x05, 0xae, 0x94, 0x3c, 0x15, 0x8e, 0x35, 0x63, 0xf5, 0x96, 0x98,
        0xce, 0x7e, 0x75, 0xc5, 0xa8, 0xc8, 0xdd, 0x43, 0x30, 0x3a, 0x0a, 0x15, 0x48, 0x87, 0xb3, 0xe6,
    ];
    PriceUpdateV2View {
        write_authority: [0x11; 32],
        feed_id,
        price: 7_512_345_678,
        conf: 4_200_000,
        exponent: -8,
        publish_time: 1_794_416_400,
        prev_publish_time: 1_794_416_399,
        ema_price: 7_500_000_000,
        ema_conf: 5_000_000,
        posted_slot: 123_456_789,
    }
}

/// `tests/fixtures/price_update_v2_{full,partial}.bin` are the SDK's bytes
/// for `fixture_view()` (Full, and Partial with 13 signatures), for mirrors
/// that cannot link the SDK (the keeper). `WRITE_FIXTURES=1` rewrites them.
#[test]
fn fixtures_are_the_sdks_bytes() {
    let v = fixture_view();
    let full = sdk_account(&sdk(&v, VerificationLevel::Full));
    let partial = sdk_account(&sdk(&v, VerificationLevel::Partial { num_signatures: 13 }));
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    if std::env::var("WRITE_FIXTURES").is_ok() {
        std::fs::write(dir.join("price_update_v2_full.bin"), &full).unwrap();
        std::fs::write(dir.join("price_update_v2_partial.bin"), &partial).unwrap();
    }
    assert_eq!(include_bytes!("fixtures/price_update_v2_full.bin").as_slice(), full.as_slice());
    assert_eq!(include_bytes!("fixtures/price_update_v2_partial.bin").as_slice(), partial.as_slice());
    assert_eq!(parse_price_update_v2(&full), Ok(v));
    assert_eq!(parse_price_update_v2(&partial), Err(ParseRejection::NotFullyVerified));
}
