//! LP-pool events (docs/lp-pool.md §13). Emitted alongside the four `msg!`
//! lines the indexer parses, which stay byte-identical (D9). `VenueFilled`
//! lives with venue_fill and is unchanged.
//!
//! Sides are `0` long / `1` short, as in `OpenPositionParams`. NAV and net
//! values are i128 / u128, the types `pool_view` computes them in.

use anchor_lang::prelude::*;
use crate::state::PoolParams;

#[event]
pub struct PositionOpened {
    pub position: Pubkey,
    pub owner: Pubkey,
    pub side: u8,
    pub size: u64,
    pub oracle: u64,
    pub exec_price: u64,
    pub collateral: u64,
    pub open_fee: u64,
    pub net_after: i128,
    pub nav_redeem: i128,
}

#[event]
pub struct PositionClosed {
    pub position: Pubkey,
    pub owner: Pubkey,
    pub mark: u64,
    pub exec_price: u64,
    pub pnl: i64,
    pub funding: i64,
    pub solvency_hc: u64,
    pub bad_debt_hc: u64,
    pub fee: u64,
    pub referral_paid: u64,
    pub payout: u64,
    pub deficit: u64,
    pub settled_close: bool,
}

/// §6.5. `stage` is 1 (partial / full with leftover to the owner) or 2
/// (backstop). `insurance_delta` is the signed change of the insurance fund.
#[event]
pub struct PositionLiquidated {
    pub position: Pubkey,
    pub stage: u8,
    pub size_closed: u64,
    pub size_remaining: u64,
    pub mark: u64,
    pub penalty: u64,
    pub keeper_reward: u64,
    pub owner_leftover: u64,
    pub insurance_delta: i64,
    pub deficit: u64,
    pub solvency_hc: u64,
    pub bad_debt_hc: u64,
    pub cash_hc: u64,
}

#[event]
pub struct BadDebt {
    pub position: Pubkey,
    pub side: u8,
    pub deficit: u64,
    pub insurance_paid: u64,
    pub lp_absorbed: u64,
    pub pending_added: u64,
    pub pending_long: u64,
    pub pending_short: u64,
    pub pending_since_long: i64,
    pub pending_since_short: i64,
    pub window_lp_absorbed: u64,
    pub window_ins_paid: u64,
}

#[event]
pub struct PositionPnlSettled {
    pub position: Pubkey,
    pub mark: u64,
    pub loss: u64,
    pub new_collateral: u64,
}

/// §7.2 step 11. `triggers` is a bit set of `pool_math::TRIGGER_*` (mode 1).
#[event]
pub struct Adl {
    pub mode: u8,
    pub position: Pubkey,
    pub owner: Pubkey,
    pub size_closed: u64,
    pub mark: u64,
    pub profit: u64,
    pub solvency_hc: u64,
    pub bad_debt_hc: u64,
    pub score: u128,
    pub floor: u64,
    pub triggers: u8,
    pub net_before: i128,
    pub net_after: i128,
    pub cash_after: u64,
    pub b: u128,
    /// Mode 0: the close fee collected from the slice (D22); mode 1: 0.
    pub fee: u64,
}

#[event]
pub struct BadDebtWrittenOff {
    pub amount: u64,
}

#[event]
pub struct FundingUpdated {
    pub periods: u64,
    pub periods_dropped: u64,
    pub rate: i64,
    pub step: i64,
    pub cumulative_funding: i64,
    pub carry_secs: u32,
}

#[event]
pub struct LpDeposited {
    pub owner: Pubkey,
    pub amount: u64,
    pub shares: u64,
    pub nav_mint: i128,
    pub lp_shares: u64,
}

#[event]
pub struct LpWithdrawalRequested {
    pub owner: Pubkey,
    pub shares: u64,
    /// `requested_value`
    pub value: u64,
    pub nav_redeem: i128,
    pub lp_shares: u64,
    pub unlock_at: i64,
}

#[event]
pub struct LpWithdrawalCancelled {
    pub owner: Pubkey,
    pub shares: u64,
    pub lp_shares: u64,
}

#[event]
pub struct LpWithdrawn {
    pub owner: Pubkey,
    pub shares: u64,
    pub value: u64,
    pub nav_redeem: i128,
    pub lp_shares: u64,
}

#[event]
pub struct InsuranceSwept {
    pub amount: u64,
    pub recovered: u64,
    pub vested: u64,
}

#[event]
pub struct PoolDonated {
    pub donor: Pubkey,
    pub amount: u64,
    pub recovered: u64,
    pub vested: u64,
}

#[event]
pub struct TreasuryWithdrawn {
    pub amount: u64,
    pub destination: Pubkey,
}

#[event]
pub struct PoolParamsProposed {
    pub params: PoolParams,
    pub effective_at: i64,
}

#[event]
pub struct PoolParamsApplied {
    pub params: PoolParams,
    pub effective_at: i64,
}

#[event]
pub struct PoolParamsCancelled {
    pub params: PoolParams,
    pub effective_at: i64,
}

/// Oracle feed rotation (docs/lp-pool.md D47). `price_feed` / `feed_id` are
/// the proposed (Proposed, Cancelled) or new (Applied) feed; `effective_at`
/// is when it may be or was applied.
#[event]
pub struct OracleFeedProposed {
    pub market: Pubkey,
    pub price_feed: Pubkey,
    pub feed_id: [u8; 32],
    pub effective_at: i64,
}

#[event]
pub struct OracleFeedApplied {
    pub market: Pubkey,
    pub price_feed: Pubkey,
    pub feed_id: [u8; 32],
    pub effective_at: i64,
}

#[event]
pub struct OracleFeedCancelled {
    pub market: Pubkey,
    pub price_feed: Pubkey,
    pub feed_id: [u8; 32],
    pub effective_at: i64,
}
