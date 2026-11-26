//! # mock-pyth — LOCALNET / TEST ONLY
//!
//! **Never deploy this program to devnet, mainnet or any shared cluster, and
//! never set a production `ProtocolConfig.oracle_program` to it.** Whoever
//! holds a feed's authority key sets its price at will. It exists so a live
//! local run (docs/localnet.md) can drive perps-core's oracle with real
//! freshness on `solana-test-validator`; it is built only by
//! `scripts/build-sbf.sh --localnet` (into `target/deploy-localnet`), never by
//! the default build.
//!
//! It owns accounts in the exact Pyth Solana Receiver `PriceUpdateV2` layout
//! (pyth-solana-receiver-sdk 2.0.0; 134 bytes, discriminator, write
//! authority, verification level, price message, posted slot), written with
//! perps-core's dependency-free `pyth_receiver.rs`, so perps-core's
//! `oracle::read_price` reads them as it reads the Receiver's accounts once
//! `ProtocolConfig.oracle_program` is this program (docs/lp-pool.md D47).
//! Every price write stamps `publish_time` and `posted_slot` from the
//! on-chain `Clock`, so `|publish_time − now| ≤ 60 s` in `read_price` is
//! real: a feed nobody updates goes stale.
//!
//! ## Instructions (first byte = tag, then little-endian fields)
//!
//! | tag | name | data | accounts |
//! |---|---|---|---|
//! | 0 | `Initialize` | `feed_id: [u8; 32], price: i64, conf: u64, expo: i32` (53 bytes) | `[w, s]` feed, `[s]` authority |
//! | 1 | `SetPrice` | `price: i64, conf: u64, expo: i32, status: u32` (25 bytes) | `[w]` feed, `[s]` authority |
//! | 2 | `SetAuthority` | `new_authority: [u8; 32]` (33 bytes) | `[w]` feed, `[s]` authority |
//! | 3 | `SetVerification` | `level: u8` (0 Partial, 1 Full), `num_signatures: u8` (3 bytes) | `[w]` feed, `[s]` authority |
//! | 4 | `SetFeedId` | `feed_id: [u8; 32]` (33 bytes) | `[w]` feed, `[s]` authority |
//!
//! * The feed account is created beforehand by the client
//!   (`SystemProgram.createAccount`, `space = 134`, owner = this program,
//!   rent-exempt), in the same transaction as `Initialize` or earlier;
//!   `Initialize` also needs the feed's signature, so nobody else can claim a
//!   created account, and fails on one already initialized. It writes a
//!   `Full` update: `publish_time = prev_publish_time = Clock.unix_timestamp`,
//!   EMA = price / conf, `posted_slot = Clock.slot`.
//! * The authority is the update's `write_authority` (bytes 8..40).
//! * `SetPrice` `status`: 1 (Trading) publishes: `prev_publish_time` takes
//!   the old `publish_time`, then price, conf, expo, EMA, `publish_time` and
//!   `posted_slot` are written. 0 (Unknown), 2 (Halted) and 3 (Auction)
//!   write nothing: the update freezes and goes stale after 60 s, as a Pyth
//!   feed does while its market is closed (no newer update exists). Other
//!   values are refused.
//! * `SetVerification` and `SetFeedId` rewrite the update in place for
//!   negative tests (a `Partial` update is refused by perps-core with
//!   `OracleNotFullyVerified`, another feed id with `OracleFeedIdMismatch`).
//!   A `Partial` update has the Receiver's Partial layout (every field after
//!   the level one byte further).

use solana_program::{
    account_info::{next_account_info, AccountInfo},
    clock::Clock,
    entrypoint::ProgramResult,
    msg,
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvar::Sysvar,
};

/// perps-core's dependency-free `PriceUpdateV2` reader and writer.
#[path = "../../perps-core/src/pyth_receiver.rs"]
pub mod pyth_receiver;

use pyth_receiver::{
    offsets, write_price_update_v2, write_price_update_v2_partial, PriceUpdateV2View, PRICE_UPDATE_V2_DISCRIMINATOR,
    VERIFICATION_FULL, VERIFICATION_PARTIAL,
};

solana_program::declare_id!("MockPyth11111111111111111111111111111111111");

#[cfg(not(feature = "no-entrypoint"))]
solana_program::entrypoint!(process_instruction);

/// `PriceUpdateV2::LEN`: the size a feed account must have.
pub const PRICE_ACCOUNT_SIZE: usize = pyth_receiver::PRICE_UPDATE_V2_LEN;
pub const STATUS_TRADING: u32 = 1;
/// Highest accepted `status` (Auction).
pub const MAX_STATUS: u32 = 3;

pub const IX_INITIALIZE: u8 = 0;
pub const IX_SET_PRICE: u8 = 1;
pub const IX_SET_AUTHORITY: u8 = 2;
pub const IX_SET_VERIFICATION: u8 = 3;
pub const IX_SET_FEED_ID: u8 = 4;

/// `Initialize` data.
pub fn initialize_data(feed_id: &[u8; 32], price: i64, conf: u64, expo: i32) -> Vec<u8> {
    let mut d = vec![IX_INITIALIZE];
    d.extend_from_slice(feed_id);
    d.extend_from_slice(&price.to_le_bytes());
    d.extend_from_slice(&conf.to_le_bytes());
    d.extend_from_slice(&expo.to_le_bytes());
    d
}

/// `SetPrice` data.
pub fn set_price_data(price: i64, conf: u64, expo: i32, status: u32) -> Vec<u8> {
    let mut d = vec![IX_SET_PRICE];
    d.extend_from_slice(&price.to_le_bytes());
    d.extend_from_slice(&conf.to_le_bytes());
    d.extend_from_slice(&expo.to_le_bytes());
    d.extend_from_slice(&status.to_le_bytes());
    d
}

/// `SetAuthority` data.
pub fn set_authority_data(new_authority: &Pubkey) -> Vec<u8> {
    let mut d = vec![IX_SET_AUTHORITY];
    d.extend_from_slice(new_authority.as_ref());
    d
}

/// `SetVerification` data: `level` 0 = Partial { num_signatures }, 1 = Full.
pub fn set_verification_data(level: u8, num_signatures: u8) -> Vec<u8> {
    vec![IX_SET_VERIFICATION, level, num_signatures]
}

/// `SetFeedId` data.
pub fn set_feed_id_data(feed_id: &[u8; 32]) -> Vec<u8> {
    let mut d = vec![IX_SET_FEED_ID];
    d.extend_from_slice(feed_id);
    d
}

/// An initialized update's fields, verification level and Partial signature
/// count, in either layout (pyth_receiver.rs parses Full only).
pub fn read_update(d: &[u8]) -> Option<(PriceUpdateV2View, u8, u8)> {
    use offsets as o;
    if d.len() != PRICE_ACCOUNT_SIZE || d[..8] != PRICE_UPDATE_V2_DISCRIMINATOR {
        return None;
    }
    let level = d[o::VERIFICATION_LEVEL];
    let (s, n) = match level {
        VERIFICATION_FULL => (0, 0),
        VERIFICATION_PARTIAL => (o::PARTIAL_SHIFT, d[o::PARTIAL_NUM_SIGNATURES]),
        _ => return None,
    };
    let b32 = |at: usize| -> [u8; 32] { d[at..at + 32].try_into().unwrap() };
    let b8 = |at: usize| -> [u8; 8] { d[at..at + 8].try_into().unwrap() };
    let v = PriceUpdateV2View {
        write_authority: b32(o::WRITE_AUTHORITY),
        feed_id: b32(s + o::FEED_ID),
        price: i64::from_le_bytes(b8(s + o::PRICE)),
        conf: u64::from_le_bytes(b8(s + o::CONF)),
        exponent: i32::from_le_bytes(d[s + o::EXPONENT..s + o::EXPONENT + 4].try_into().unwrap()),
        publish_time: i64::from_le_bytes(b8(s + o::PUBLISH_TIME)),
        prev_publish_time: i64::from_le_bytes(b8(s + o::PREV_PUBLISH_TIME)),
        ema_price: i64::from_le_bytes(b8(s + o::EMA_PRICE)),
        ema_conf: u64::from_le_bytes(b8(s + o::EMA_CONF)),
        posted_slot: u64::from_le_bytes(b8(s + o::POSTED_SLOT)),
    };
    Some((v, level, n))
}

/// Rewrite the whole account in `level`'s layout (the 134th byte of a Full
/// update is zero, as the Receiver leaves it).
fn store(d: &mut [u8], v: &PriceUpdateV2View, level: u8, num_signatures: u8) {
    d.iter_mut().for_each(|b| *b = 0);
    if level == VERIFICATION_PARTIAL {
        write_price_update_v2_partial(d, v, num_signatures);
    } else {
        write_price_update_v2(d, v);
    }
}

fn le_i64(b: &[u8]) -> i64 {
    i64::from_le_bytes(b.try_into().unwrap())
}

pub fn process_instruction(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let iter = &mut accounts.iter();
    let feed = next_account_info(iter)?;
    let authority = next_account_info(iter)?;
    if feed.owner != program_id {
        return Err(ProgramError::IncorrectProgramId);
    }
    if !feed.is_writable {
        return Err(ProgramError::InvalidArgument);
    }
    if !authority.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    let (&tag, rest) = data.split_first().ok_or(ProgramError::InvalidInstructionData)?;
    let mut d = feed.try_borrow_mut_data()?;
    if d.len() != PRICE_ACCOUNT_SIZE {
        return Err(ProgramError::InvalidAccountData);
    }
    let initialized = d[..8] == PRICE_UPDATE_V2_DISCRIMINATOR;
    if tag == IX_INITIALIZE {
        if rest.len() != 52 {
            return Err(ProgramError::InvalidInstructionData);
        }
        if initialized {
            return Err(ProgramError::AccountAlreadyInitialized);
        }
        if !feed.is_signer {
            return Err(ProgramError::MissingRequiredSignature);
        }
        let clock = Clock::get()?;
        let feed_id: [u8; 32] = rest[0..32].try_into().unwrap();
        let price = le_i64(&rest[32..40]);
        let conf = u64::from_le_bytes(rest[40..48].try_into().unwrap());
        let expo = i32::from_le_bytes(rest[48..52].try_into().unwrap());
        let v = PriceUpdateV2View {
            write_authority: authority.key.to_bytes(),
            posted_slot: clock.slot,
            ..PriceUpdateV2View::new(feed_id, price, conf, expo, clock.unix_timestamp)
        };
        store(&mut d, &v, VERIFICATION_FULL, 0);
        msg!("mock-pyth: initialized price={} conf={} expo={}", price, conf, expo);
        return Ok(());
    }

    if !initialized {
        return Err(ProgramError::UninitializedAccount);
    }
    let (mut v, mut level, mut num_signatures) = read_update(&d).ok_or(ProgramError::InvalidAccountData)?;
    if v.write_authority != authority.key.to_bytes() {
        msg!("mock-pyth: signer is not the feed authority");
        return Err(ProgramError::IllegalOwner);
    }
    match tag {
        IX_SET_PRICE if rest.len() == 24 => {
            let status = u32::from_le_bytes(rest[20..24].try_into().unwrap());
            if status > MAX_STATUS {
                return Err(ProgramError::InvalidInstructionData);
            }
            if status != STATUS_TRADING {
                // Closed or halted: no new update; the stored one goes stale
                return Ok(());
            }
            let clock = Clock::get()?;
            v.price = le_i64(&rest[0..8]);
            v.conf = u64::from_le_bytes(rest[8..16].try_into().unwrap());
            v.exponent = i32::from_le_bytes(rest[16..20].try_into().unwrap());
            v.ema_price = v.price;
            v.ema_conf = v.conf;
            v.prev_publish_time = v.publish_time;
            v.publish_time = clock.unix_timestamp;
            v.posted_slot = clock.slot;
        }
        IX_SET_AUTHORITY if rest.len() == 32 => {
            v.write_authority = rest.try_into().unwrap();
        }
        IX_SET_VERIFICATION if rest.len() == 2 => {
            if rest[0] != VERIFICATION_PARTIAL && rest[0] != VERIFICATION_FULL {
                return Err(ProgramError::InvalidInstructionData);
            }
            level = rest[0];
            num_signatures = if level == VERIFICATION_PARTIAL { rest[1] } else { 0 };
        }
        IX_SET_FEED_ID if rest.len() == 32 => {
            v.feed_id = rest.try_into().unwrap();
        }
        _ => return Err(ProgramError::InvalidInstructionData),
    }
    store(&mut d, &v, level, num_signatures);
    Ok(())
}
