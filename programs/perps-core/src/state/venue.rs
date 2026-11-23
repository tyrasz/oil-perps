use anchor_lang::prelude::*;
use crate::state::{ceil_div, Market, Position, Side};

/// A program allowed to open matched positions in one market with venue_fill.
///
/// Only that program can sign for `authority` (its PDA `[b"perps_venue",
/// market]`), so the record binds the program, the market and the signer
/// together. `upgrade_authority` pins who can change the venue's code: if the
/// venue's ProgramData names any other upgrade authority, fills halt until
/// the admin re-attests with update_venue. (Pinning the deploy slot instead
/// is griefable: loader ExtendProgram needs no authority and rewrites
/// ProgramData.slot, so anyone could halt every venue for a few lamports.)
#[account]
#[derive(Default)]
pub struct Venue {
    pub market: Pubkey,
    pub program: Pubkey,              // venue program id
    pub authority: Pubkey,            // find_program_address([b"perps_venue", market], program).0
    pub program_data: Pubkey,         // find_program_address([program], bpf_loader_upgradeable).0
    pub max_fill_notional: u64,       // Per fill, at fill price (6 dp USDC)
    pub total_fills: u64,
    pub total_notional: u64,          // Saturating
    pub max_price_deviation_bps: u16, // |fill - oracle| band
    pub max_oracle_age_secs: u16,
    pub min_signers: u8,              // 0..=2 of {taker, maker} must be signers of the CPI
    pub enabled: bool,
    pub bump: u8,
    pub upgrade_authority: Pubkey,    // ProgramData upgrade authority pinned at register/update; default = immutable
}

impl Venue {
    pub const SEED: &'static [u8] = b"venue";
    pub const AUTHORITY_SEED: &'static [u8] = b"perps_venue";
    // No venue-wide notional window: colluding wallets could fill it with wash
    // trades and shut the venue for everyone. Its bytes went to padding.
    pub const LEN: usize = 8 + 32 * 5 + 8 * 3 + 2 + 2 + 1 + 1 + 1 + 68; // 267
}

/// One owner's consent to fills through one venue in one role.
///
/// Perps-core signs and enforces these bounds itself, so a buggy or hostile
/// venue can never commit more of the owner's collateral, at worse prices or
/// higher leverage, than the owner agreed to here. Taker and maker consent are
/// separate accounts: an MM's standing maker approval must not let a venue
/// make it the taker of someone else's order.
#[account]
#[derive(Default)]
pub struct VenueApproval {
    pub owner: Pubkey,
    pub venue: Pubkey,
    pub market: Pubkey,               // = venue.market (for indexers)
    pub allowance: u64,               // Collateral the venue may still commit; spent down, never refilled
    pub max_buy_price: u64,           // 0 = none; applies when owner goes long
    pub min_sell_price: u64,          // 0 = none; applies when owner goes short
    pub expires_at: i64,
    pub fills: u64,
    pub collateral_committed: u64,    // Lifetime stat, saturating
    pub max_leverage: u32,            // 3 dp
    pub max_price_deviation_bps: u16, // Adverse-only bound vs oracle; 0 = off
    pub bump: u8,
    pub role: u8,                     // ROLE_TAKER or ROLE_MAKER; part of the seeds
    pub allowed_sides: u8,            // SIDE_LONG | SIDE_SHORT: which sides the owner may be put on
}

impl VenueApproval {
    pub const SEED: &'static [u8] = b"venue_approval";
    pub const LEN: usize = 8 + 32 * 3 + 8 * 6 + 4 + 2 + 1 + 1 + 1 + 30; // 191
}

pub const ROLE_TAKER: u8 = 0;
pub const ROLE_MAKER: u8 = 1;

/// VenueApproval.allowed_sides bits. A one-shot "buy" approval must not let a
/// venue open a short for the owner instead.
pub const SIDE_LONG: u8 = 1;
pub const SIDE_SHORT: u8 = 2;

pub const MAX_VENUE_BAND_BPS: u16 = 500;
pub const MAX_VENUE_ORACLE_AGE_SECS: u16 = 60;
pub const MAX_APPROVAL_SECS: i64 = 90 * 86_400;

/// Inclusive band check, u128.
pub fn within_band(price: u64, oracle: u64, bps: u16) -> bool {
    let diff = if price > oracle { price - oracle } else { oracle - price } as u128;
    diff * 10_000 <= oracle as u128 * bps as u128
}

/// Whether `price` is no more than `bps` worse than `oracle` for a party
/// going `side` (a long overpays, a short undersells). 0 = no bound.
pub fn within_adverse_bound(side: Side, price: u64, oracle: u64, bps: u16) -> bool {
    if bps == 0 {
        return true;
    }
    let paid = price as u128 * 10_000;
    match side {
        Side::Long => paid <= oracle as u128 * (10_000 + bps as u128),
        Side::Short => paid >= oracle as u128 * 10_000u128.saturating_sub(bps as u128),
    }
}

/// Collateral a venue fill posts for one side. Public so venues and the SDK
/// use the same math. `None` => leverage is above what the in-force initial
/// margin allows (or overflow).
///
/// `max(notional_fill / lev, IM x notional_oracle) + adverse`, where adverse
/// is the side's immediate loss at the oracle. Equity at the oracle is then at
/// least IM of both the fill and the oracle notional, so no position is born
/// liquidatable and a colluding pair cannot push a loss onto the vault.
#[inline(never)]
pub fn venue_margin(m: &Market, side: Side, size: u64, price: u64, oracle: u64, leverage: u32, now: i64) -> Option<u64> {
    let notional_fill = u64::try_from(size as u128 * price as u128 / 1_000_000).ok()?;
    let notional_oracle = size as u128 * oracle as u128 / 1_000_000;
    let base = m.required_collateral(notional_fill, leverage, now)?;
    let (im, _) = m.margin_ratios(now);
    let im_oracle = u64::try_from(ceil_div(notional_oracle * im as u128, 10_000)).ok()?;
    // Same truncation as close/liquidate, so the two legs net exactly
    let probe = Position { side, size, entry_price: price, ..Default::default() };
    let pnl = probe.unrealized_pnl(oracle);
    let adverse = if pnl < 0 { pnl.unsigned_abs() } else { 0 };
    base.max(im_oracle).checked_add(adverse)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session;

    /// sdk/mm-registry decodes these accounts by byte offset and hand-encodes
    /// these instructions; a layout change here must change it too.
    #[test]
    fn sdk_layout_is_pinned() {
        use anchor_lang::{AccountSerialize, Discriminator};
        fn bytes<T: AccountSerialize>(account: &T) -> Vec<u8> {
            let mut out = Vec::new();
            account.try_serialize(&mut out).unwrap();
            out
        }
        let feed = Pubkey::new_from_array([0xab; 32]);
        let m = Market {
            pyth_price_feed: feed, max_leverage: 0x0102_0304, initial_margin_ratio: 0x0506_0708,
            weekend_initial_margin_ratio: 0x090a_0b0c, funding_carry_secs: 0x1112_1314,
            settled_at: 0x1516_1718_191a_1b1c, ..Default::default()
        };
        let b = bytes(&m);
        assert_eq!(&b[104..136], feed.as_ref());
        assert_eq!(&b[144..148], &0x0102_0304u32.to_le_bytes());
        assert_eq!(&b[152..156], &0x0506_0708u32.to_le_bytes());
        assert_eq!(&b[247..251], &0x090a_0b0cu32.to_le_bytes());
        // Appended by the LP pool (docs/lp-pool.md §1.1, D35); the SDK reads
        // both (sdk/mm-registry PerpsMarketInfo fundingCarrySecs / settledAt)
        assert_eq!(&b[283..287], &0x1112_1314u32.to_le_bytes());
        assert_eq!(&b[287..295], &0x1516_1718_191a_1b1ci64.to_le_bytes());

        let v = Venue { max_price_deviation_bps: 0x0d0e, max_oracle_age_secs: 0x0f10, min_signers: 2, enabled: true, ..Default::default() };
        let b = bytes(&v);
        assert_eq!(&b[160..162], &0x0d0eu16.to_le_bytes());
        assert_eq!(&b[162..164], &0x0f10u16.to_le_bytes());
        assert_eq!((b[164], b[165]), (2, 1));

        assert_eq!(crate::instruction::ApproveVenue::DISCRIMINATOR, [254, 60, 62, 30, 179, 64, 176, 184]);
        assert_eq!(crate::instruction::UpdateVenueApproval::DISCRIMINATOR, [243, 224, 3, 21, 150, 246, 103, 7]);
        // VenueApprovalParams as the SDK encodes it: 39 bytes, allowed_sides last
        let params = crate::instructions::VenueApprovalParams {
            allowance: 1, max_leverage: 2, max_buy_price: 3, min_sell_price: 4, max_price_deviation_bps: 5, expires_at: 6, allowed_sides: 7,
        };
        let enc = params.try_to_vec().unwrap();
        assert_eq!(enc.len(), 39);
        assert_eq!(enc[38], 7);
    }

    const CONTRACT: u64 = 1_000_000;
    // Fri 2026-11-13 22:00 UTC is the weekly close
    const FRIDAY_CLOSE: i64 = 1_794_607_200;
    const WEDNESDAY: i64 = 1_794_416_400;

    fn oil() -> Market {
        Market {
            max_leverage: 20_000,
            initial_margin_ratio: 1_000,
            maintenance_margin_ratio: 500,
            session_schedule: session::SCHEDULE_CME_GLOBEX,
            pre_close_window_secs: 7_200,
            weekend_initial_margin_ratio: 2_000,
            weekend_maintenance_margin_ratio: 1_000,
            ..Default::default()
        }
    }

    #[test]
    fn account_sizes_are_pinned() {
        assert_eq!(Venue::LEN, 267);
        assert_eq!(8 + Venue::default().try_to_vec().unwrap().len() + 68, Venue::LEN);
        assert_eq!(VenueApproval::LEN, 191);
        assert_eq!(8 + VenueApproval::default().try_to_vec().unwrap().len() + 30, VenueApproval::LEN);
    }

    #[test]
    fn band_is_inclusive() {
        let oracle = 75_000_000;
        // 50 bps of $75 = $0.375
        assert!(within_band(75_375_000, oracle, 50));
        assert!(within_band(74_625_000, oracle, 50));
        assert!(!within_band(75_375_001, oracle, 50));
        assert!(!within_band(74_624_999, oracle, 50));
        assert!(within_band(oracle, oracle, 0));
        assert!(!within_band(oracle + 1, oracle, 0));
        assert!(!within_band(oracle - 1, oracle, 0));
    }

    #[test]
    fn adverse_bound_only_limits_the_losing_direction() {
        let oracle = 75_000_000;
        // 30 bps of $75 = $0.225
        assert!(within_adverse_bound(Side::Long, 75_225_000, oracle, 30));
        assert!(!within_adverse_bound(Side::Long, 75_225_001, oracle, 30));
        assert!(within_adverse_bound(Side::Long, 1, oracle, 30));
        assert!(within_adverse_bound(Side::Short, 74_775_000, oracle, 30));
        assert!(!within_adverse_bound(Side::Short, 74_774_999, oracle, 30));
        assert!(within_adverse_bound(Side::Short, u64::MAX, oracle, 30));
        assert!(within_adverse_bound(Side::Short, 1, oracle, 0));
        assert!(within_adverse_bound(Side::Short, 1, oracle, 10_000));
    }

    #[test]
    fn worked_example_from_the_spec() {
        // Taker buys 100 at the $75.03 ask, oracle $75, both at 10x
        let m = oil();
        let (size, price, oracle) = (100 * CONTRACT, 75_030_000, 75_000_000);
        assert_eq!(venue_margin(&m, Side::Long, size, price, oracle, 10_000, WEDNESDAY), Some(753_300_000));
        assert_eq!(venue_margin(&m, Side::Short, size, price, oracle, 10_000, WEDNESDAY), Some(750_300_000));
        let pos = |side, collateral| Position { side, size, entry_price: price, collateral, ..Default::default() };
        assert_eq!(pos(Side::Long, 753_300_000).margin_ratio(oracle, 0), 1_000);
        assert_eq!(pos(Side::Short, 750_300_000).margin_ratio(oracle, 0), 1_003);
    }

    #[test]
    fn margin_keeps_both_legs_at_or_above_initial_margin() {
        let m = oil();
        let sizes = [1, 999, 37 * CONTRACT + 1, 100 * CONTRACT];
        let prices = [74_625_000, 74_999_999, 75_000_000, 75_000_001, 75_030_000, 75_375_000];
        let oracles = [74_700_000, 75_000_000, 75_300_000];
        let leverages = [1_000, 2_000, 3_000, 5_000, 7_777, 10_000];
        for now in [WEDNESDAY, FRIDAY_CLOSE - 60] {
            let (im, _) = m.margin_ratios(now);
            for &size in &sizes {
                for &price in &prices {
                    for &oracle in &oracles {
                        for &lev in &leverages {
                            for side in [Side::Long, Side::Short] {
                                let required = m.required_collateral((size as u128 * price as u128 / 1_000_000) as u64, lev, now);
                                let c = venue_margin(&m, side, size, price, oracle, lev, now);
                                assert_eq!(c.is_some(), required.is_some(), "leverage gate matches required_collateral");
                                let c = match c { Some(c) => c, None => continue };
                                let pos = Position { side, size, entry_price: price, collateral: c, ..Default::default() };
                                let equity = c as i128 + pos.unrealized_pnl(oracle) as i128;
                                let n_fill = size as i128 * price as i128 / 1_000_000;
                                let n_oracle = size as i128 * oracle as i128 / 1_000_000;
                                assert!(equity * 10_000 >= n_oracle * im as i128, "{:?}", (size, price, oracle, lev, now));
                                assert!(equity * 10_000 >= n_fill * im as i128, "{:?}", (size, price, oracle, lev, now));
                                if n_fill > 0 {
                                    assert!(pos.margin_ratio(oracle, 0) >= im);
                                }
                                if price == oracle {
                                    assert_eq!(Some(c), required);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn margin_rounds_up() {
        // $1 at 3x = 0.333334, and 10% of a $0.000001 oracle notional rounds to 1
        let m = Market { initial_margin_ratio: 1_000, max_leverage: 20_000, ..Default::default() };
        assert_eq!(venue_margin(&m, Side::Long, CONTRACT, 1_000_000, 1_000_000, 3_000, 0), Some(333_334));
        assert_eq!(venue_margin(&m, Side::Long, 1, 1_000_000, 1_000_000, 1_000, 0), Some(1));
    }

    #[test]
    fn weekend_margin_rejects_ten_x() {
        let m = oil();
        let args = (100 * CONTRACT, 75_000_000, 75_000_000);
        assert_eq!(venue_margin(&m, Side::Long, args.0, args.1, args.2, 10_000, FRIDAY_CLOSE - 60), None);
        assert_eq!(venue_margin(&m, Side::Long, args.0, args.1, args.2, 5_000, FRIDAY_CLOSE - 60), Some(1_500_000_000));
        assert_eq!(venue_margin(&m, Side::Long, args.0, args.1, args.2, 0, WEDNESDAY), None);
    }
}
