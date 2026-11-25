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
//! It owns accounts in the exact pyth-sdk-solana 0.8 `PriceAccount` layout
//! (version 2, 3_312 bytes, the one `scripts/test-ts.sh` writes into genesis),
//! so `pyth_sdk_solana::load_price_feed_from_account_info` and perps-core's
//! `oracle::read_price` read them unchanged. Every write stamps `timestamp`
//! (the publish time) and the slots from the on-chain `Clock`, so
//! `|publish_time − now| ≤ 60 s` in `read_price` is real: a feed nobody
//! updates goes stale.
//!
//! ## Instructions (first byte = tag, then little-endian fields)
//!
//! | tag | name | data | accounts |
//! |---|---|---|---|
//! | 0 | `Initialize` | `price: i64, conf: u64, expo: i32` (21 bytes) | `[w, s]` feed, `[s]` authority |
//! | 1 | `SetPrice` | `price: i64, conf: u64, expo: i32, status: u32` (25 bytes) | `[w]` feed, `[s]` authority |
//! | 2 | `SetAuthority` | `new_authority: [u8; 32]` (33 bytes) | `[w]` feed, `[s]` authority |
//!
//! * The feed account is created beforehand by the client
//!   (`SystemProgram.createAccount`, `space = 3_312`, owner = this program,
//!   rent-exempt), in the same transaction as `Initialize` or earlier;
//!   `Initialize` also needs the feed's signature, so nobody else can claim a
//!   created account, and fails on one already initialized.
//! * `status`: 0 Unknown, 1 Trading, 2 Halted, 3 Auction. While not Trading,
//!   readers get the last Trading price (`prev_*`) with its publish time, as
//!   with real Pyth, so it goes stale after 60 s.
//! * The authority is stored as the feed's only publisher (`comp[0].publisher`,
//!   offset 240); `num = 1`.
//! * `expo` applies to the stored previous price too (keep it fixed, -8).

use solana_program::{
    account_info::{next_account_info, AccountInfo},
    clock::Clock,
    entrypoint::ProgramResult,
    msg,
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvar::Sysvar,
};

solana_program::declare_id!("MockPyth11111111111111111111111111111111111");

#[cfg(not(feature = "no-entrypoint"))]
solana_program::entrypoint!(process_instruction);

/// pyth-sdk-solana 0.8 `PriceAccount` size and header values.
pub const PRICE_ACCOUNT_SIZE: usize = 3_312;
pub const MAGIC: u32 = 0xa1b2_c3d4;
pub const VERSION_2: u32 = 2;
pub const ACCOUNT_TYPE_PRICE: u32 = 3;
pub const PRICE_TYPE_PRICE: u32 = 1;
pub const STATUS_TRADING: u32 = 1;
/// Highest accepted `status` (Auction).
pub const MAX_STATUS: u32 = 3;

pub const IX_INITIALIZE: u8 = 0;
pub const IX_SET_PRICE: u8 = 1;
pub const IX_SET_AUTHORITY: u8 = 2;

/// Byte offsets in the `PriceAccount` (repr C).
pub mod offsets {
    pub const MAGIC: usize = 0;
    pub const VER: usize = 4;
    pub const ATYPE: usize = 8;
    pub const SIZE: usize = 12;
    pub const PTYPE: usize = 16;
    pub const EXPO: usize = 20;
    pub const NUM: usize = 24;
    pub const NUM_QT: usize = 28;
    pub const LAST_SLOT: usize = 32;
    pub const VALID_SLOT: usize = 40;
    pub const EMA_PRICE: usize = 48; // Rational { val, numer, denom }
    pub const EMA_CONF: usize = 72;
    pub const TIMESTAMP: usize = 96;
    pub const MIN_PUB: usize = 104;
    pub const PREV_SLOT: usize = 176;
    pub const PREV_PRICE: usize = 184;
    pub const PREV_CONF: usize = 192;
    pub const PREV_TIMESTAMP: usize = 200;
    pub const AGG: usize = 208; // PriceInfo { price, conf, status, corp_act, pub_slot }
    pub const COMP0_PUBLISHER: usize = 240;
    pub const COMP0_AGG: usize = 272;
    pub const COMP0_LATEST: usize = 304;
}

/// `Initialize` data.
pub fn initialize_data(price: i64, conf: u64, expo: i32) -> Vec<u8> {
    let mut d = vec![IX_INITIALIZE];
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

fn put_u32(d: &mut [u8], at: usize, v: u32) {
    d[at..at + 4].copy_from_slice(&v.to_le_bytes());
}
fn put_u64(d: &mut [u8], at: usize, v: u64) {
    d[at..at + 8].copy_from_slice(&v.to_le_bytes());
}
fn put_i64(d: &mut [u8], at: usize, v: i64) {
    d[at..at + 8].copy_from_slice(&v.to_le_bytes());
}
fn get_u32(d: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(d[at..at + 4].try_into().unwrap())
}
fn get_u64(d: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(d[at..at + 8].try_into().unwrap())
}
fn get_i64(d: &[u8], at: usize) -> i64 {
    i64::from_le_bytes(d[at..at + 8].try_into().unwrap())
}

fn put_price_info(d: &mut [u8], at: usize, price: i64, conf: u64, status: u32, slot: u64) {
    put_i64(d, at, price);
    put_u64(d, at + 8, conf);
    put_u32(d, at + 16, status);
    put_u32(d, at + 20, 0); // corp_act
    put_u64(d, at + 24, slot);
}

/// Write a new aggregate stamped with `clock`; the last Trading aggregate
/// moves to `prev_*` first.
fn publish(d: &mut [u8], price: i64, conf: u64, expo: i32, status: u32, clock: &Clock) {
    use offsets as o;
    if get_u32(d, o::AGG + 16) == STATUS_TRADING {
        let (p, c, slot, ts) = (get_i64(d, o::AGG), get_u64(d, o::AGG + 8), get_u64(d, o::AGG + 24), get_i64(d, o::TIMESTAMP));
        put_u64(d, o::PREV_SLOT, slot);
        put_i64(d, o::PREV_PRICE, p);
        put_u64(d, o::PREV_CONF, c);
        put_i64(d, o::PREV_TIMESTAMP, ts);
    }
    let slot = clock.slot;
    d[o::EXPO..o::EXPO + 4].copy_from_slice(&expo.to_le_bytes());
    put_u64(d, o::LAST_SLOT, slot);
    put_u64(d, o::VALID_SLOT, slot);
    for (at, v) in [(o::EMA_PRICE, price), (o::EMA_CONF, conf as i64)] {
        put_i64(d, at, v);
        put_i64(d, at + 8, v);
        put_i64(d, at + 16, 1);
    }
    put_i64(d, o::TIMESTAMP, clock.unix_timestamp);
    put_price_info(d, o::AGG, price, conf, status, slot);
    put_price_info(d, o::COMP0_AGG, price, conf, status, slot);
    put_price_info(d, o::COMP0_LATEST, price, conf, status, slot);
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
    let initialized = get_u32(&d, offsets::MAGIC) == MAGIC;
    if tag != IX_INITIALIZE {
        if !initialized {
            return Err(ProgramError::UninitializedAccount);
        }
        if d[offsets::COMP0_PUBLISHER..offsets::COMP0_PUBLISHER + 32] != authority.key.to_bytes() {
            msg!("mock-pyth: signer is not the feed authority");
            return Err(ProgramError::IllegalOwner);
        }
    }
    match tag {
        IX_INITIALIZE if rest.len() == 20 => {
            if initialized {
                return Err(ProgramError::AccountAlreadyInitialized);
            }
            if !feed.is_signer {
                return Err(ProgramError::MissingRequiredSignature);
            }
            let price = i64::from_le_bytes(rest[0..8].try_into().unwrap());
            let conf = u64::from_le_bytes(rest[8..16].try_into().unwrap());
            let expo = i32::from_le_bytes(rest[16..20].try_into().unwrap());
            use offsets as o;
            put_u32(&mut d, o::MAGIC, MAGIC);
            put_u32(&mut d, o::VER, VERSION_2);
            put_u32(&mut d, o::ATYPE, ACCOUNT_TYPE_PRICE);
            put_u32(&mut d, o::SIZE, PRICE_ACCOUNT_SIZE as u32);
            put_u32(&mut d, o::PTYPE, PRICE_TYPE_PRICE);
            put_u32(&mut d, o::NUM, 1);
            put_u32(&mut d, o::NUM_QT, 1);
            d[o::MIN_PUB] = 1;
            d[o::COMP0_PUBLISHER..o::COMP0_PUBLISHER + 32].copy_from_slice(authority.key.as_ref());
            let clock = Clock::get()?;
            publish(&mut d, price, conf, expo, STATUS_TRADING, &clock);
            // The opening price is also the previous one
            let (slot, ts) = (clock.slot, clock.unix_timestamp);
            put_u64(&mut d, o::PREV_SLOT, slot);
            put_i64(&mut d, o::PREV_PRICE, price);
            put_u64(&mut d, o::PREV_CONF, conf);
            put_i64(&mut d, o::PREV_TIMESTAMP, ts);
            msg!("mock-pyth: initialized price={} conf={} expo={}", price, conf, expo);
        }
        IX_SET_PRICE if rest.len() == 24 => {
            let price = i64::from_le_bytes(rest[0..8].try_into().unwrap());
            let conf = u64::from_le_bytes(rest[8..16].try_into().unwrap());
            let expo = i32::from_le_bytes(rest[16..20].try_into().unwrap());
            let status = u32::from_le_bytes(rest[20..24].try_into().unwrap());
            if status > MAX_STATUS {
                return Err(ProgramError::InvalidInstructionData);
            }
            publish(&mut d, price, conf, expo, status, &Clock::get()?);
        }
        IX_SET_AUTHORITY if rest.len() == 32 => {
            d[offsets::COMP0_PUBLISHER..offsets::COMP0_PUBLISHER + 32].copy_from_slice(rest);
        }
        _ => return Err(ProgramError::InvalidInstructionData),
    }
    Ok(())
}
