//! Test fixtures shared by the decision tests. They build the same markets,
//! pools and positions as the perps-core unit tests (instructions/adl.rs,
//! settle_close.rs) so the keeper's mirrors are checked against the same
//! vectors.

use crate::state::{LpPoolData, MarketData, PoolParamsData, PositionData, SIDE_LONG, STATUS_OPEN};
use solana_sdk::pubkey::Pubkey;

pub const C6: u64 = 1_000_000;
pub const HOUR: i64 = 3_600;
/// Fri 2026-11-13 17:00 ET.
pub const FRIDAY_CLOSE: i64 = 1_794_607_200;
/// Sun 2026-11-15 18:00 ET.
pub const SUNDAY_OPEN: i64 = 1_794_783_600;
/// Wed 2026-11-11 12:00 ET, in session.
pub const WEDNESDAY: i64 = 1_794_416_400;

/// perps-core adl.rs `market()`.
pub fn market() -> MarketData {
    MarketData {
        maintenance_margin_ratio: 500,
        initial_margin_ratio: 1_000,
        max_leverage: 20_000,
        taker_fee: 5,
        liquidation_fee: 100,
        max_open_interest: u64::MAX,
        funding_interval: 3_600,
        ..Default::default()
    }
}

/// perps-core adl.rs `pool()`: $1M of cash and shares, neutral params with
/// fees, a $10 insurance target and a skew scale.
pub fn pool() -> LpPoolData {
    let params = PoolParamsData {
        open_fee_bps: 5,
        treasury_fee_bps: 1_000,
        insurance_target: 10 * C6,
        skew_scale: 2_500_000_000_000,
        max_impact_bps: 100,
        ..PoolParamsData::neutral()
    };
    LpPoolData { cash: 1_000_000 * C6, lp_shares: 1_000_000 * C6, params, ..Default::default() }
}

/// `pool_add` + OI + position collateral, as perps-core's test `open` does.
pub fn open(m: &mut MarketData, pool: &mut LpPoolData, side: u8, size: u64, entry: u64, c: u64) -> PositionData {
    let p = PositionData {
        owner: Pubkey::new_unique(),
        side,
        size,
        collateral: c,
        entry_price: entry,
        opened_price: entry,
        leverage: 5_000,
        last_funding_payment: m.cumulative_funding,
        status: STATUS_OPEN,
        execution_source: 2,
        ..Default::default()
    };
    let e = size as u128 * entry as u128;
    let f = size as i128 * p.last_funding_payment as i128;
    if side == SIDE_LONG {
        pool.long_entry_sum += e;
        pool.long_funding_sum += f;
        m.long_open_interest += size;
    } else {
        pool.short_entry_sum += e;
        pool.short_funding_sum += f;
        m.short_open_interest += size;
    }
    pool.open_positions += 1;
    pool.position_collateral += c;
    p
}

pub fn keyed(positions: &[PositionData]) -> Vec<(Pubkey, PositionData)> {
    positions.iter().map(|p| (Pubkey::new_unique(), p.clone())).collect()
}
