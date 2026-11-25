//! Borsh mirrors of the perps-core accounts the keeper reads.
//!
//! Borsh decodes fields in declaration order, so every struct here must match
//! the on-chain struct field for field. Each decodes a prefix of the account
//! (after the 8-byte discriminator); the trailing padding is ignored.
//!
//! - `MarketData`: programs/perps-core/src/state/market.rs
//! - `PositionData`: programs/perps-core/src/state/position.rs
//! - `LpPoolData`, `PoolParamsData`, `LpPositionData`:
//!   programs/perps-core/src/state/pool.rs (docs/lp-pool.md §1.2, §1.3),
//!   pinned against the checked-in fixtures `tests/fixtures/lp_pool.bin` and
//!   `lp_position.bin` that perps-core regenerates (§16.4).

use crate::{pool_math, session};
use borsh::{BorshDeserialize, BorshSerialize};
use solana_sdk::pubkey::Pubkey;

pub const SIDE_LONG: u8 = 0;
pub const SIDE_SHORT: u8 = 1;
pub const STATUS_OPEN: u8 = 0;

/// Mirrors `Position` (Position::LEN = 168; this decodes through `opened_price`).
#[derive(BorshDeserialize, BorshSerialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct PositionData {
    pub owner: Pubkey,
    pub market: Pubkey,
    pub side: u8, // 0 = Long, 1 = Short
    pub size: u64,
    pub collateral: u64,
    pub entry_price: u64,
    pub leverage: u32,
    pub realized_pnl: i64,
    pub last_funding_payment: i64,
    pub opened_at: i64,
    pub last_updated_at: i64,
    pub status: u8, // 0 = Open, 1 = Closed, 2 = Liquidated
    pub execution_source: u8,
    pub bump: u8,
    /// Set at open and never changed; the fee and house-notional basis (D21).
    pub opened_price: u64,
}

/// Mirrors `Market` (Market::LEN = 298). `funding_carry_secs` sits at byte
/// 283 and `settled_at` at 287 (with the discriminator).
#[derive(BorshDeserialize, BorshSerialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct MarketData {
    pub authority: Pubkey,
    pub collateral_mint: Pubkey,
    pub vault: Pubkey,
    pub pyth_price_feed: Pubkey,
    pub commodity: [u8; 8],
    pub max_leverage: u32,
    pub maintenance_margin_ratio: u32,
    pub initial_margin_ratio: u32,
    pub taker_fee: u32,
    pub maker_fee: u32,
    pub liquidation_fee: u32,
    pub long_open_interest: u64,
    pub short_open_interest: u64,
    pub max_open_interest: u64,
    pub cumulative_funding: i64,
    pub last_funding_time: i64,
    pub funding_interval: i64,
    pub insurance_fund: u64,
    pub total_positions: u64,
    pub total_trades: u64,
    pub bump: u8,
    pub is_paused: bool,
    pub session_schedule: u8,
    pub pre_close_window_secs: u32,
    pub weekend_initial_margin_ratio: u32,
    pub weekend_maintenance_margin_ratio: u32,
    pub max_confidence_bps: u32,
    pub min_oracle_price: u64,
    pub settlement_price: u64,
    pub funding_rate: i64,
    /// Open seconds accrued but not yet charged (docs/lp-pool.md §8).
    pub funding_carry_secs: u32,
    /// Unix time `settle_market` ran; 0 = not settled, or settled before the
    /// field existed (D35).
    pub settled_at: i64,
}

impl MarketData {
    pub fn is_settled(&self) -> bool {
        self.settlement_price > 0
    }

    pub fn is_in_session(&self, now: i64) -> bool {
        session::is_open(self.session_schedule, now)
    }

    /// `(initial, maintenance)` ratios in force at `now` (Market::margin_ratios).
    pub fn margin_ratios(&self, now: i64) -> (u32, u32) {
        if session::in_pre_close_window(self.session_schedule, now, self.pre_close_window_secs) {
            (
                self.initial_margin_ratio.max(self.weekend_initial_margin_ratio),
                self.maintenance_margin_ratio.max(self.weekend_maintenance_margin_ratio),
            )
        } else {
            (self.initial_margin_ratio, self.maintenance_margin_ratio)
        }
    }

    /// Maintenance margin the program applies at `now`.
    pub fn maintenance_margin_ratio_at(&self, now: i64) -> u32 {
        self.margin_ratios(now).1
    }

    /// Open seconds since the last crank plus the carry (§8 steps 3–5).
    pub fn funding_open_secs(&self, now: i64) -> i64 {
        session::open_seconds_between(self.session_schedule, self.last_funding_time, now)
            .saturating_add(self.funding_carry_secs as i64)
    }

    /// Whether `update_funding` would charge at least one period at `now`
    /// (it fails `FundingNotDue` otherwise). Settled and closed markets are
    /// rejected by the program before this matters.
    pub fn funding_due(&self, now: i64) -> bool {
        pool_math::funding_catchup(self.funding_open_secs(now), self.funding_interval).is_some()
    }
}

/// Mirrors `PoolParams` (Borsh, 104 bytes; offsets relative to the struct).
#[derive(BorshDeserialize, BorshSerialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PoolParamsData {
    pub skew_scale: u64,                     //  0
    pub insurance_target: u64,               //  8
    pub insurance_claim_cap_per_window: u64, // 16
    pub liq_min_remaining_notional: u64,     // 24
    pub k_net_bps: u32,                      // 32
    pub hard_net_bps: u32,                   // 36
    pub per_account_cap_bps: u32,            // 40
    pub weekend_lp_loss_bps: u32,            // 44
    pub weekend_gap_bps: u32,                // 48
    pub weekend_cap_window_secs: u32,        // 52
    pub withdrawal_delay_secs: u32,          // 56
    pub withdrawal_claim_window_secs: u32,   // 60
    pub lp_reopen_quiet_secs: u32,           // 64
    pub adl_net_target_bps: u32,             // 68
    pub adl_exposure_score_floor_bps: u32,   // 72
    pub dd_freeze_bps: u16,                  // 76
    pub dd_adl_bps: u16,                     // 78
    pub dd_withdraw_block_bps: u16,          // 80
    pub redeem_loss_haircut_bps: u16,        // 82
    pub open_fee_bps: u16,                   // 84
    pub max_impact_bps: u16,                 // 86
    pub liq_penalty_bps: u16,                // 88
    pub liq_keeper_share_bps: u16,           // 90
    pub backstop_threshold_bps: u16,         // 92
    pub min_liq_slice_bps: u16,              // 94
    pub treasury_fee_bps: u16,               // 96
    pub lp_loss_cap_window_bps: u16,         // 98
    pub profit_close_quiet_secs: u32,        // 100
}

impl PoolParamsData {
    #[cfg(test)]
    pub const LEN: usize = 104;

    /// `G_bps` (§3.1): the override, else `max(IM, weekend IM)`.
    pub fn g_bps(&self, m: &MarketData) -> u64 {
        pool_math::g_bps(self.weekend_gap_bps, m.initial_margin_ratio, m.weekend_initial_margin_ratio)
    }

    /// `PoolParams::production()` (§15.1), for tests.
    #[cfg(test)]
    pub fn production() -> Self {
        PoolParamsData {
            skew_scale: 2_500_000_000_000,
            insurance_target: 250_000_000_000,
            insurance_claim_cap_per_window: 0,
            liq_min_remaining_notional: 100_000_000,
            k_net_bps: 4_000,
            hard_net_bps: 6_000,
            per_account_cap_bps: 2_500,
            weekend_lp_loss_bps: 1_000,
            weekend_gap_bps: 0,
            weekend_cap_window_secs: 86_400,
            withdrawal_delay_secs: 345_600,
            withdrawal_claim_window_secs: 259_200,
            lp_reopen_quiet_secs: 1_800,
            adl_net_target_bps: 2_000,
            adl_exposure_score_floor_bps: 0,
            dd_freeze_bps: 1_000,
            dd_adl_bps: 1_500,
            dd_withdraw_block_bps: 0,
            redeem_loss_haircut_bps: 5_000,
            open_fee_bps: 5,
            max_impact_bps: 100,
            liq_penalty_bps: 200,
            liq_keeper_share_bps: 2_000,
            backstop_threshold_bps: 6_667,
            min_liq_slice_bps: 2_000,
            treasury_fee_bps: 1_000,
            lp_loss_cap_window_bps: 500,
            profit_close_quiet_secs: 300,
        }
    }

    /// `PoolParams::neutral()` (§15.2), for tests.
    #[cfg(test)]
    pub fn neutral() -> Self {
        PoolParamsData {
            skew_scale: 0,
            open_fee_bps: 0,
            treasury_fee_bps: 0,
            insurance_target: u64::MAX,
            k_net_bps: 100_000,
            hard_net_bps: 0,
            per_account_cap_bps: 0,
            weekend_cap_window_secs: 0,
            dd_freeze_bps: 0,
            dd_adl_bps: 0,
            dd_withdraw_block_bps: 0,
            backstop_threshold_bps: 10_000,
            liq_penalty_bps: 0,
            lp_loss_cap_window_bps: 10_000,
            withdrawal_delay_secs: 0,
            withdrawal_claim_window_secs: 0,
            lp_reopen_quiet_secs: 0,
            profit_close_quiet_secs: 0,
            // Kept at its D43 value: D45 left neutral unchanged
            adl_net_target_bps: 2_500,
            ..Self::production()
        }
    }
}

/// Mirrors `LpPool` (LpPool::LEN = 601; 553 bytes with the discriminator, then
/// 48 bytes of padding). PDA `[b"lp_pool", market]`.
#[derive(BorshDeserialize, BorshSerialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LpPoolData {
    pub market: Pubkey,                // 8
    pub adl_keeper: Pubkey,            // 40
    pub free_collateral: u64,          // 72
    pub position_collateral: u64,      // 80
    pub cash: u64,                     // 88
    pub treasury: u64,                 // 96
    pub long_entry_sum: u128,          // 104
    pub short_entry_sum: u128,         // 120
    pub long_funding_sum: i128,        // 136
    pub short_funding_sum: i128,       // 152
    pub open_positions: u64,           // 168
    pub lp_shares: u64,                // 176
    pub pending_withdraw_shares: u64,  // 184
    pub max_pool_cash: u64,            // 192
    pub pending_bad_debt_long: u64,    // 200
    pub pending_bad_debt_short: u64,   // 208
    pub pending_since_long: i64,       // 216
    pub bad_debt_cum: u64,             // 224
    pub bad_debt_recovered_cum: u64,   // 232
    pub bad_debt_written_off_cum: u64, // 240
    pub loss_window_start: i64,        // 248
    pub loss_window_nav: u64,          // 256
    pub lp_absorbed_window: u64,       // 264
    pub ins_paid_window: u64,          // 272
    pub nps_peak: u64,                 // 280
    pub nps_peak_at: i64,              // 288
    pub vest_amount: u64,              // 296
    pub vest_start: i64,               // 304
    pub vest_end: i64,                 // 312
    pub donated_cum: u64,              // 320
    pub pending_params_at: i64,        // 328
    pub params: PoolParamsData,        // 336
    pub pending_params: PoolParamsData, // 440
    pub bump: u8,                      // 544
    pub pending_since_short: i64,      // 545 (D36)
}

impl LpPoolData {
    pub fn adl_required(&self) -> bool {
        self.pending_bad_debt_long > 0 || self.pending_bad_debt_short > 0
    }

    /// The fields the valuation reads.
    pub fn state(&self) -> pool_math::PoolState {
        pool_math::PoolState {
            cash: self.cash,
            long_entry_sum: self.long_entry_sum,
            short_entry_sum: self.short_entry_sum,
            long_funding_sum: self.long_funding_sum,
            short_funding_sum: self.short_funding_sum,
            open_positions: self.open_positions,
            lp_shares: self.lp_shares,
            pending_withdraw_shares: self.pending_withdraw_shares,
            nps_peak: self.nps_peak,
            vest_amount: self.vest_amount,
            vest_start: self.vest_start,
            vest_end: self.vest_end,
        }
    }

    /// Pending bad debt a winner on `side` repays (D13): a short winner
    /// repays bankrupt longs.
    pub fn opposite_pending(&self, side: u8) -> u64 {
        if side == SIDE_SHORT {
            self.pending_bad_debt_long
        } else {
            self.pending_bad_debt_short
        }
    }

    /// `(pending, pending_since)` of the bankrupt `side` (D36).
    pub fn pending_of(&self, bankrupt: u8) -> (u64, i64) {
        if bankrupt == SIDE_LONG {
            (self.pending_bad_debt_long, self.pending_since_long)
        } else {
            (self.pending_bad_debt_short, self.pending_since_short)
        }
    }
}

/// Mirrors `LpPosition` (LpPosition::LEN = 166; 134 bytes with the
/// discriminator). PDA `[b"lp_position", lp_pool, owner, &[class]]`. The
/// keeper does not act on LP positions; the mirror pins the layout (§16.4).
#[allow(dead_code)]
#[derive(BorshDeserialize, BorshSerialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LpPositionData {
    pub owner: Pubkey,
    pub pool: Pubkey,
    pub shares: u64,
    pub pending_shares: u64,
    pub requested_value: u64,
    pub requested_at: i64,
    pub unlock_at: i64,
    pub deposited_cum: u64,
    pub withdrawn_cum: u64,
    pub claim_window_secs: u32,
    pub class: u8,
    pub bump: u8,
}

/// Anchor's 8-byte discriminator: the first 8 bytes of sha256("<namespace>:<name>").
pub fn anchor_discriminator(namespace: &str, name: &str) -> [u8; 8] {
    let hash = solana_sdk::hash::hash(format!("{}:{}", namespace, name).as_bytes());
    let mut out = [0u8; 8];
    out.copy_from_slice(&hash.to_bytes()[..8]);
    out
}

/// Decode an Anchor account of type `name` owned by `program_id`, or `None`
/// if it is not one (wrong owner, discriminator or too short). Anyone can
/// send lamports to an unused PDA, so the owner and type are checked first.
pub fn decode_account<T: BorshDeserialize>(owner: &Pubkey, data: &[u8], program_id: &Pubkey, name: &str) -> Option<T> {
    if owner != program_id || data.get(..8)? != anchor_discriminator("account", name) {
        return None;
    }
    T::deserialize(&mut data.get(8..)?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LP_POOL_FIXTURE: &[u8] = include_bytes!("../../../programs/perps-core/tests/fixtures/lp_pool.bin");
    const LP_POSITION_FIXTURE: &[u8] = include_bytes!("../../../programs/perps-core/tests/fixtures/lp_position.bin");

    /// The values perps-core's `sample_params(base)` writes (state/pool.rs).
    fn sample_params(base: u64) -> PoolParamsData {
        PoolParamsData {
            skew_scale: base + 1,
            insurance_target: base + 2,
            insurance_claim_cap_per_window: base + 3,
            liq_min_remaining_notional: base + 4,
            k_net_bps: 0x0100_0005,
            hard_net_bps: 0x0100_0006,
            per_account_cap_bps: 0x0100_0007,
            weekend_lp_loss_bps: 0x0100_0008,
            weekend_gap_bps: 0x0100_0009,
            weekend_cap_window_secs: 0x0100_000a,
            withdrawal_delay_secs: 0x0100_000b,
            withdrawal_claim_window_secs: 0x0100_000c,
            lp_reopen_quiet_secs: 0x0100_000d,
            adl_net_target_bps: 0x0100_000e,
            adl_exposure_score_floor_bps: 0x0100_000f,
            dd_freeze_bps: 0x0210,
            dd_adl_bps: 0x0211,
            dd_withdraw_block_bps: 0x0212,
            redeem_loss_haircut_bps: 0x0213,
            open_fee_bps: 0x0214,
            max_impact_bps: 0x0215,
            liq_penalty_bps: 0x0216,
            liq_keeper_share_bps: 0x0217,
            backstop_threshold_bps: 0x0218,
            min_liq_slice_bps: 0x0219,
            treasury_fee_bps: 0x021a,
            lp_loss_cap_window_bps: 0x021b,
            profit_close_quiet_secs: 0x0100_001c,
        }
    }

    /// §16.4: decode the fixture perps-core serialises from a fully populated
    /// `LpPool` with distinct field values, and check every field.
    #[test]
    fn keeper_pool_layout_is_pinned() {
        assert_eq!(LP_POOL_FIXTURE.len(), 601 - 48);
        assert_eq!(&LP_POOL_FIXTURE[..8], anchor_discriminator("account", "LpPool"));
        let program = Pubkey::new_unique();
        let p: LpPoolData = decode_account(&program, LP_POOL_FIXTURE, &program, "LpPool").unwrap();
        assert_eq!(p.market, Pubkey::new_from_array([0x11; 32]));
        assert_eq!(p.adl_keeper, Pubkey::new_from_array([0x22; 32]));
        assert_eq!(
            [p.free_collateral, p.position_collateral, p.cash, p.treasury],
            [0x0300_0000_0000_0001, 0x0300_0000_0000_0002, 0x0300_0000_0000_0003, 0x0300_0000_0000_0004]
        );
        assert_eq!(p.long_entry_sum, 0x0400_0000_0000_0000_0000_0000_0000_0005);
        assert_eq!(p.short_entry_sum, 0x0400_0000_0000_0000_0000_0000_0000_0006);
        assert_eq!(p.long_funding_sum, -0x0400_0000_0000_0000_0000_0000_0000_0007);
        assert_eq!(p.short_funding_sum, 0x0400_0000_0000_0000_0000_0000_0000_0008);
        assert_eq!(
            [p.open_positions, p.lp_shares, p.pending_withdraw_shares, p.max_pool_cash],
            [0x0500_0000_0000_0009, 0x0500_0000_0000_000a, 0x0500_0000_0000_000b, 0x0500_0000_0000_000c]
        );
        assert_eq!((p.pending_bad_debt_long, p.pending_bad_debt_short), (0x0500_0000_0000_000d, 0x0500_0000_0000_000e));
        assert_eq!((p.pending_since_long, p.pending_since_short), (-0x0500_0000_0000_000f, -0x0500_0000_0000_001f));
        assert_eq!(
            [p.bad_debt_cum, p.bad_debt_recovered_cum, p.bad_debt_written_off_cum],
            [0x0500_0000_0000_0010, 0x0500_0000_0000_0011, 0x0500_0000_0000_0012]
        );
        assert_eq!(p.loss_window_start, 0x0500_0000_0000_0013);
        assert_eq!(
            [p.loss_window_nav, p.lp_absorbed_window, p.ins_paid_window, p.nps_peak],
            [0x0500_0000_0000_0014, 0x0500_0000_0000_0015, 0x0500_0000_0000_0016, 0x0500_0000_0000_0017]
        );
        assert_eq!(p.nps_peak_at, 0x0500_0000_0000_0018);
        assert_eq!(p.vest_amount, 0x0500_0000_0000_0019);
        assert_eq!((p.vest_start, p.vest_end), (0x0500_0000_0000_001a, 0x0500_0000_0000_001b));
        assert_eq!(p.donated_cum, 0x0500_0000_0000_001c);
        assert_eq!(p.pending_params_at, 0x0500_0000_0000_001d);
        assert_eq!(p.params, sample_params(0x0600_0000_0000_0000));
        assert_eq!(p.pending_params, sample_params(0x0700_0000_0000_0000));
        assert_eq!(p.bump, 0xfe);
        // Byte offsets the SDK reads (§16.3): pending_params 440, bump 544
        assert_eq!(&LP_POOL_FIXTURE[440..544], borsh::to_vec(&p.pending_params).unwrap().as_slice());
        assert_eq!(LP_POOL_FIXTURE[544], 0xfe);
        // And it re-serialises to the same bytes
        let mut out = LP_POOL_FIXTURE[..8].to_vec();
        out.extend(borsh::to_vec(&p).unwrap());
        assert_eq!(out, LP_POOL_FIXTURE);
        assert_eq!(borsh::to_vec(&PoolParamsData::default()).unwrap().len(), PoolParamsData::LEN);
    }

    #[test]
    fn keeper_lp_position_layout_is_pinned() {
        assert_eq!(LP_POSITION_FIXTURE.len(), 166 - 32);
        let program = Pubkey::new_unique();
        let lp: LpPositionData = decode_account(&program, LP_POSITION_FIXTURE, &program, "LpPosition").unwrap();
        assert_eq!(lp.owner, Pubkey::new_from_array([0x33; 32]));
        assert_eq!(lp.pool, Pubkey::new_from_array([0x44; 32]));
        assert_eq!(
            [lp.shares, lp.pending_shares, lp.requested_value, lp.deposited_cum, lp.withdrawn_cum],
            [0x0800_0000_0000_0001, 0x0800_0000_0000_0002, 0x0800_0000_0000_0003, 0x0800_0000_0000_0006, 0x0800_0000_0000_0007]
        );
        assert_eq!((lp.requested_at, lp.unlock_at), (0x0800_0000_0000_0004, 0x0800_0000_0000_0005));
        assert_eq!((lp.claim_window_secs, lp.class, lp.bump), (0x0900_0008, 0x0a, 0xfd));
        let mut out = LP_POSITION_FIXTURE[..8].to_vec();
        out.extend(borsh::to_vec(&lp).unwrap());
        assert_eq!(out, LP_POSITION_FIXTURE);
    }

    #[test]
    fn decode_rejects_other_accounts() {
        let program = Pubkey::new_unique();
        assert!(decode_account::<LpPoolData>(&Pubkey::default(), LP_POOL_FIXTURE, &program, "LpPool").is_none());
        assert!(decode_account::<LpPoolData>(&program, LP_POOL_FIXTURE, &program, "Market").is_none());
        assert!(decode_account::<LpPoolData>(&program, &LP_POOL_FIXTURE[..100], &program, "LpPool").is_none());
        assert!(decode_account::<LpPoolData>(&program, &[], &program, "LpPool").is_none());
    }

    #[test]
    fn market_offsets_of_the_appended_fields() {
        // §16.3: funding_carry_secs at 283 and settled_at at 287, LEN 298
        let m = MarketData { funding_carry_secs: 0xa1b2_c3d4, settled_at: -0x0102_0304_0506_0708, ..Default::default() };
        let mut b = vec![0u8; 8];
        b.extend(borsh::to_vec(&m).unwrap());
        assert_eq!(u32::from_le_bytes(b[283..287].try_into().unwrap()), 0xa1b2_c3d4);
        assert_eq!(i64::from_le_bytes(b[287..295].try_into().unwrap()), -0x0102_0304_0506_0708);
        assert_eq!(b.len() + 3, 298);
    }

    #[test]
    fn position_status_offset_matches_layout() {
        let p = PositionData { status: 0xab, opened_price: 7, ..Default::default() };
        let mut b = vec![0u8; 8];
        b.extend(borsh::to_vec(&p).unwrap());
        assert_eq!(b[133], 0xab);
        // Position::LEN = 168 with 24 bytes of padding
        assert_eq!(b.len() + 24, 168);
    }
}
