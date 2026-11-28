//! Real Solana mainnet `PriceUpdateV2` accounts (docs/lp-pool.md D53): the
//! push oracle's shard-0 feed accounts as `getMultipleAccounts` returned them
//! at slot 474_383_906 (2026-11-27 02:33:02 UTC), saved byte for byte in
//! `tests/fixtures/mainnet_*.bin` with their address and owner in
//! `mainnet_price_updates.json`. The parser reads them, `oracle::read_price`
//! accepts them under the real Receiver, pinned address and feed id, and a
//! market whose oracle is one of them opens and closes positions at its price.
//!
//! XAU/USD is the one market feed Pyth sponsors on Solana (fresh on both
//! program pairs). The XAG/USD and USOILSPOT accounts exist only under the
//! legacy pair and were last written on 2026-07-03 and 2026-08-26 (the Core
//! upgrade), so they test the stale path with real bytes.

use crate::support::*;
use anchor_lang::prelude::{AccountInfo, Pubkey as AnchorPubkey};
use perps_core::errors::PerpsError;
use perps_core::oracle::{read_price, MAX_PRICE_AGE_SECS};
use perps_core::pyth_push::{
    push_feed_address, PYTH_PUSH_ORACLE_LEGACY, PYTH_PUSH_ORACLE_PRO, PYTH_RECEIVER_LEGACY, PYTH_RECEIVER_PRO,
};
use perps_core::pyth_receiver::{parse_price_update_v2, PRICE_UPDATE_V2_LEN};
use perps_core::state::{Market, PoolParams, Position};
use anchor_lang::prelude::pubkey;

const XAU: [u8; 32] = hex32("765d2ba906dbc32ca17cc11f5310a89e9ee1f6420508c63861f2f8ba4ee34bb2");
const XAG: [u8; 32] = hex32("f2fb02c32b055c805e7238d628e5e9dadef274376114eb1f012337cabe93871e");
const USOILSPOT: [u8; 32] = hex32("925ca92ff005ae943c158e3563f59698ce7e75c5a8c8dd43303a0a154887b3e6");
const SOL: [u8; 32] = hex32("ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d");

/// open_position sides (0 = long, 1 = short; not the venue's SIDE_* masks).
const LONG: u8 = 0;
const SHORT: u8 = 1;

/// The slot's block time: when the accounts were read.
const FETCHED_AT: i64 = 1_795_746_782;

const fn hex32(s: &str) -> [u8; 32] {
    const fn nib(c: u8) -> u8 {
        match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            _ => panic!("hex"),
        }
    }
    let b = s.as_bytes();
    let mut out = [0u8; 32];
    let mut i = 0;
    while i < 32 {
        out[i] = nib(b[2 * i]) << 4 | nib(b[2 * i + 1]);
        i += 1;
    }
    out
}

struct Fixture {
    file: &'static str,
    bytes: &'static [u8],
    address: Pubkey,
    receiver: Pubkey,
    push_oracle: Pubkey,
    feed_id: [u8; 32],
    // What the bytes say, read off independently of the parser (hexdump)
    price: i64,
    conf: u64,
    exponent: i32,
    publish_time: i64,
    ema_price: i64,
}

fn fixtures() -> Vec<Fixture> {
    vec![
        Fixture {
            file: "mainnet_xau_usd_pro.bin",
            bytes: include_bytes!("../fixtures/mainnet_xau_usd_pro.bin"),
            address: pubkey!("csXV1aFF7uAfTHYWx6dDj718hnf7dUDU8v54dubib17"),
            receiver: PYTH_RECEIVER_PRO,
            push_oracle: PYTH_PUSH_ORACLE_PRO,
            feed_id: XAU,
            price: 3_623_512,
            conf: 263,
            exponent: -3,
            publish_time: 1_795_746_734,
            ema_price: 3_588_779,
        },
        Fixture {
            file: "mainnet_xau_usd_legacy.bin",
            bytes: include_bytes!("../fixtures/mainnet_xau_usd_legacy.bin"),
            address: pubkey!("2uPQGpm8X4ZkxMHxrAW1QuhXcse1AHEgPih6Xp9NuEWW"),
            receiver: PYTH_RECEIVER_LEGACY,
            push_oracle: PYTH_PUSH_ORACLE_LEGACY,
            feed_id: XAU,
            price: 3_629_366,
            conf: 366,
            exponent: -3,
            publish_time: 1_795_746_773,
            ema_price: 3_589_063,
        },
        Fixture {
            file: "mainnet_sol_usd_legacy.bin",
            bytes: include_bytes!("../fixtures/mainnet_sol_usd_legacy.bin"),
            // Pyth's documented SOL/USD sponsored account
            address: pubkey!("7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE"),
            receiver: PYTH_RECEIVER_LEGACY,
            push_oracle: PYTH_PUSH_ORACLE_LEGACY,
            feed_id: SOL,
            price: 7_264_388_666,
            conf: 1_427_456,
            exponent: -8,
            publish_time: 1_795_746_733,
            ema_price: 7_162_347_260,
        },
        Fixture {
            file: "mainnet_sol_usd_pro.bin",
            bytes: include_bytes!("../fixtures/mainnet_sol_usd_pro.bin"),
            address: pubkey!("7AviUf9nL62mcxNbQGKm4nKDQnPjswo6c5MX4D57HmyE"),
            receiver: PYTH_RECEIVER_PRO,
            push_oracle: PYTH_PUSH_ORACLE_PRO,
            feed_id: SOL,
            price: 7_261_603_506,
            conf: 2_401_286,
            exponent: -8,
            publish_time: 1_795_746_776,
            ema_price: 7_164_045_690,
        },
        Fixture {
            file: "mainnet_xag_usd_legacy.bin",
            bytes: include_bytes!("../fixtures/mainnet_xag_usd_legacy.bin"),
            address: pubkey!("H9JxsWwtDZxjSL6m7cdCVsWibj3JBMD9sxqLjadoZnot"),
            receiver: PYTH_RECEIVER_LEGACY,
            push_oracle: PYTH_PUSH_ORACLE_LEGACY,
            feed_id: XAG,
            price: 6_220_878,
            conf: 2_864,
            exponent: -5,
            publish_time: 1_783_050_208,
            ema_price: 6_223_575,
        },
        Fixture {
            file: "mainnet_usoilspot_legacy.bin",
            bytes: include_bytes!("../fixtures/mainnet_usoilspot_legacy.bin"),
            address: pubkey!("4LPPjSGx5s3fvANM78zVhVFhEUQSJGHc1PUFKRjR76sX"),
            receiver: PYTH_RECEIVER_LEGACY,
            push_oracle: PYTH_PUSH_ORACLE_LEGACY,
            feed_id: USOILSPOT,
            price: 8_272_215,
            conf: 672_107,
            exponent: -5,
            publish_time: 1_787_760_039,
            ema_price: 8_201_013,
        },
        // Written on a localnet by the real pro-compatible push oracle,
        // Receiver and Wormhole (cloned from mainnet) when
        // scripts/price-pusher pushed a mainnet XAU/USD update (D54): what
        // the protocol's pusher will leave in the GOLD market's account
        Fixture {
            file: "localnet_pusher_xau_usd_pro.bin",
            bytes: include_bytes!("../fixtures/localnet_pusher_xau_usd_pro.bin"),
            address: pubkey!("csXV1aFF7uAfTHYWx6dDj718hnf7dUDU8v54dubib17"),
            receiver: PYTH_RECEIVER_PRO,
            push_oracle: PYTH_PUSH_ORACLE_PRO,
            feed_id: XAU,
            price: 3_652_722,
            conf: 212,
            exponent: -3,
            publish_time: 1_795_808_657,
            ema_price: 3_642_310,
        },
    ]
}

fn fixture(file: &str) -> Fixture {
    fixtures().into_iter().find(|f| f.file == file).unwrap()
}

/// `raw · 10^expo` in 6-dp units, truncated (as read_price normalizes).
fn six_dp(raw: i64, expo: i32) -> u64 {
    let shift = expo + 6;
    if shift >= 0 {
        raw as u64 * 10u64.pow(shift as u32)
    } else {
        raw as u64 / 10u64.pow((-shift) as u32)
    }
}

/// `read_price` straight on an AccountInfo: the account `f` as mainnet holds it.
fn read(f: &Fixture, market: &Market, now: i64) -> anchor_lang::Result<u64> {
    let key: AnchorPubkey = f.address;
    let owner: AnchorPubkey = f.receiver;
    let mut lamports = 1_823_520u64;
    let mut data = f.bytes.to_vec();
    let info = AccountInfo::new(&key, false, false, &mut lamports, &mut data, &owner, false);
    read_price(&info, market, now)
}

fn market_on(f: &Fixture) -> Market {
    Market { pyth_price_feed: f.address, oracle_program: f.receiver, pyth_feed_id: f.feed_id, ..Default::default() }
}

fn code(e: anchor_lang::error::Error) -> u32 {
    match e {
        anchor_lang::error::Error::AnchorError(a) => a.error_code_number,
        other => panic!("not an AnchorError: {:?}", other),
    }
}

/// Every account: the size the Receiver allocates, the push oracle's
/// shard-0 PDA for its feed id, written by that PDA, Full, with the values
/// the hexdump shows; and the EMA is in the same units.
#[test]
fn parser_reads_the_mainnet_accounts() {
    for f in fixtures() {
        assert_eq!(f.bytes.len(), PRICE_UPDATE_V2_LEN, "{}", f.file);
        assert_eq!(push_feed_address(&f.push_oracle, &f.feed_id), f.address, "{}", f.file);
        let v = parse_price_update_v2(f.bytes).unwrap_or_else(|r| panic!("{}: {:?}", f.file, r));
        assert_eq!(v.feed_id, f.feed_id, "{}", f.file);
        // The push oracle posts with the feed PDA itself as write authority
        assert_eq!(Pubkey::new_from_array(v.write_authority), f.address, "{}", f.file);
        assert_eq!((v.price, v.conf, v.exponent, v.publish_time, v.ema_price), (f.price, f.conf, f.exponent, f.publish_time, f.ema_price), "{}", f.file);
        assert!(v.prev_publish_time <= v.publish_time && v.posted_slot > 0, "{}", f.file);
        // Re-encoding the view gives the account's first 133 bytes back
        let mut again = vec![0u8; PRICE_UPDATE_V2_LEN];
        perps_core::pyth_receiver::write_price_update_v2(&mut again, &v);
        assert_eq!(&again[..133], &f.bytes[..133], "{}", f.file);
    }
}

/// read_price with the real pinned PDA, Receiver and feed id: the fresh
/// feeds give `price · 10^expo` at 6 dp, exactly, at every clock within the
/// age limits; the stale ones are StaleOraclePrice.
#[test]
fn read_price_accepts_the_real_accounts() {
    let stale = PerpsError::StaleOraclePrice as u32 + anchor_lang::error::ERROR_CODE_OFFSET;
    for f in fixtures().into_iter().filter(|f| f.file != "mainnet_usoilspot_legacy.bin") {
        let m = market_on(&f);
        let want = six_dp(f.price, f.exponent);
        if f.publish_time + (MAX_PRICE_AGE_SECS as i64) < FETCHED_AT {
            assert_eq!(code(read(&f, &m, FETCHED_AT).unwrap_err()), stale, "{}", f.file);
        }
        for now in [f.publish_time - 60, f.publish_time, f.publish_time + 60] {
            assert_eq!(read(&f, &m, now).unwrap(), want, "{} at {}", f.file, now);
        }
        assert_eq!(code(read(&f, &m, f.publish_time + 61).unwrap_err()), stale, "{}", f.file);
        assert_eq!(code(read(&f, &m, f.publish_time - 61).unwrap_err()), stale, "{}", f.file);
    }
    // The scaling, spelled out: XAU 3_623_512e-3 = $3,623.512; SOL 7_264_388_666e-8 = $72.64388666, truncated to $72.643886
    assert_eq!(read(&fixture("mainnet_xau_usd_pro.bin"), &market_on(&fixture("mainnet_xau_usd_pro.bin")), FETCHED_AT).unwrap(), 3_623_512_000);
    let sol = fixture("mainnet_sol_usd_legacy.bin");
    assert_eq!(read(&sol, &market_on(&sol), FETCHED_AT).unwrap(), 72_643_886);
    // ($3,623.512 ± $0.263 is 0.7 bps, inside the default 1% limit.)
    // USOILSPOT's last update, $82.72 ± $6.72 (8%), is refused even at its own
    // publish time; by its age at the fetch, StaleOraclePrice first
    let oil = fixture("mainnet_usoilspot_legacy.bin");
    let cw = PerpsError::OracleConfidenceTooWide as u32 + anchor_lang::error::ERROR_CODE_OFFSET;
    assert_eq!(code(read(&oil, &market_on(&oil), oil.publish_time).unwrap_err()), cw);
    // Under a 9% limit it scales like the others: 8_272_215e-5 = $82.72215
    assert_eq!(read(&oil, &Market { max_confidence_bps: 900, ..market_on(&oil) }, oil.publish_time).unwrap(), 82_722_150);
    assert_eq!(code(read(&oil, &market_on(&oil), FETCHED_AT).unwrap_err()), stale);
}

/// The other pair's Receiver, another feed id or another pinned key: refused.
#[test]
fn read_price_refuses_a_mismatched_real_account() {
    let f = fixture("mainnet_xau_usd_pro.bin");
    let err = |e: PerpsError| e as u32 + anchor_lang::error::ERROR_CODE_OFFSET;
    let t = f.publish_time;
    let other_pair = Market { oracle_program: PYTH_RECEIVER_LEGACY, ..market_on(&f) };
    assert_eq!(code(read(&f, &other_pair, t).unwrap_err()), err(PerpsError::InvalidOracleAccount));
    let other_id = Market { pyth_feed_id: XAG, ..market_on(&f) };
    assert_eq!(code(read(&f, &other_id, t).unwrap_err()), err(PerpsError::OracleFeedIdMismatch));
    let other_key = Market { pyth_price_feed: push_feed_address(&PYTH_PUSH_ORACLE_LEGACY, &XAU), ..market_on(&f) };
    assert_eq!(code(read(&f, &other_key, t).unwrap_err()), err(PerpsError::InvalidOracleAccount));
}

/// A GOLD market created under the real Receiver, pinned to the real
/// shard-0 account (initialize_market's address, owner, Full and feed id
/// checks), at a clock 5 s after the account's publish time.
async fn gold_market(file: &str) -> (Base, Fixture) {
    let f = fixture(file);
    let feed = FeedSetup {
        commodity: "GOLD",
        oracle_program: f.receiver,
        oracle: f.address,
        account: feed_account(f.bytes.to_vec(), f.receiver),
        feed_id: f.feed_id,
        clock: f.publish_time + 5,
    };
    (start_on_feed(ProgramTest::default(), PoolParams::neutral(), LP_SEED, feed).await, f)
}

/// Open and close on a market whose oracle is the real mainnet account, on
/// both program pairs: entry and exit at the account's price, no PnL, the
/// fees only. The account is never rewritten.
#[tokio::test]
async fn trades_on_the_real_xau_push_accounts() {
    for file in ["mainnet_xau_usd_pro.bin", "mainnet_xau_usd_legacy.bin", "localnet_pusher_xau_usd_pro.bin"] {
        let (mut base, f) = gold_market(file).await;
        let m = market(&mut base).await;
        assert_eq!((m.pyth_price_feed, m.oracle_program, m.pyth_feed_id), (f.address, f.receiver, XAU), "{}", file);
        let price = six_dp(f.price, f.exponent);
        let alice = trader(&mut base, 10_000 * USDC).await;
        let p = open(&mut base, &alice, LONG, 1, 5).await.unwrap_or_else(|e| panic!("{}: {:?}", file, e));
        let pos: Position = load(&mut base, p).await;
        // The house fills at the oracle price (no skew premium at neutral params)
        assert_eq!((pos.entry_price, pos.size), (price, CONTRACT), "{}", file);
        let before = token_balance(&mut base, alice.tokens).await;
        close(&mut base, &alice, p).await.unwrap();
        let back = token_balance(&mut base, alice.tokens).await - before;
        // No PnL: the margin of 1 contract at 5x comes back less the close
        // fee, ceil(5 bps of the notional) (the open fee was taken from the
        // collateral at open, as in trading/)
        let margin = price / 5;
        let fee = (price * 5).div_ceil(10_000);
        assert_eq!(back, margin - fee, "{}: margin {} fee {}", file, margin, fee);
        assert_eq!(raw(&mut base, f.address).await.unwrap().data, f.bytes, "{}", file);
    }
}

/// The same market 61 s after the account's publish time: every price read
/// is StaleOraclePrice until someone pushes a newer update.
#[tokio::test]
async fn a_real_account_goes_stale_after_the_age_limit() {
    let (mut base, f) = gold_market("mainnet_xau_usd_pro.bin").await;
    let alice = trader(&mut base, 10_000 * USDC).await;
    let p = open(&mut base, &alice, SHORT, 1, 5).await.unwrap();
    set_clock(&mut base, f.publish_time + 61).await;
    assert_eq!(custom_error(close(&mut base, &alice, p).await), perps_err(PerpsError::StaleOraclePrice));
    assert_eq!(custom_error(open(&mut base, &alice, LONG, 1, 5).await.map(|_| ())), perps_err(PerpsError::StaleOraclePrice));
    // A newer push into the same account (same layout, a later time) unblocks it
    let mut data = f.bytes.to_vec();
    perps_core::pyth_receiver::set_publish_times(&mut data, f.publish_time + 61);
    set_feed(&mut base, data, f.receiver).await;
    close(&mut base, &alice, p).await.unwrap();
}

/// A market can be created on a stale real account (no age check at
/// creation, so a market may be set up while closed), but nothing trades on
/// it: the frozen USOILSPOT account the repo used to pin, and XAG's.
#[tokio::test]
async fn the_frozen_legacy_accounts_create_but_never_trade() {
    for (file, commodity) in [("mainnet_usoilspot_legacy.bin", "OIL"), ("mainnet_xag_usd_legacy.bin", "SILVER")] {
        let f = fixture(file);
        let feed = FeedSetup {
            commodity,
            oracle_program: f.receiver,
            oracle: f.address,
            account: feed_account(f.bytes.to_vec(), f.receiver),
            feed_id: f.feed_id,
            clock: FETCHED_AT,
        };
        // No seed deposit: it would read the stale price
        let mut base = start_on_feed(ProgramTest::default(), PoolParams::neutral(), 0, feed).await;
        assert_eq!(market(&mut base).await.pyth_price_feed, f.address, "{}", file);
        let alice = trader(&mut base, 10_000 * USDC).await;
        assert_eq!(custom_error(open(&mut base, &alice, LONG, 1, 5).await.map(|_| ())), perps_err(PerpsError::StaleOraclePrice), "{}", file);
    }
}
