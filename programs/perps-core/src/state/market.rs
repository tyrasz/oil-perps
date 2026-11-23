use anchor_lang::prelude::*;
use crate::session;

#[account]
#[derive(Default)]
pub struct Market {
    pub authority: Pubkey,
    pub collateral_mint: Pubkey,
    pub vault: Pubkey,
    pub pyth_price_feed: Pubkey,

    // Commodity identifier (e.g., "OIL", "GOLD", "SILVER")
    pub commodity: [u8; 8],             // Fixed-size commodity identifier

    // Market parameters
    pub max_leverage: u32,              // 20x = 20_000 (3 decimals)
    pub maintenance_margin_ratio: u32,  // 5% = 500 (2 decimals, basis points)
    pub initial_margin_ratio: u32,      // 10% = 1000 (2 decimals, basis points)
    pub taker_fee: u32,                 // 0.05% = 5 (2 decimals, basis points)
    pub maker_fee: u32,                 // 0.02% = 2 (2 decimals, basis points)
    pub liquidation_fee: u32,           // 2.5% = 250 (2 decimals, basis points)

    // Open interest tracking
    pub long_open_interest: u64,        // In base units (oil contracts)
    pub short_open_interest: u64,       // In base units (oil contracts)
    pub max_open_interest: u64,         // Cap per side

    // Funding rate
    pub cumulative_funding: i64,        // Running funding per whole contract, USDC (6 decimals); + = longs paid
    pub last_funding_time: i64,         // Unix timestamp
    pub funding_interval: i64,          // Seconds between funding (3600 = 1 hour)

    // Insurance fund
    pub insurance_fund: u64,            // Accumulated from liquidations

    // Counters
    pub total_positions: u64,
    pub total_trades: u64,

    pub bump: u8,
    pub is_paused: bool,

    // Risk controls (set with update_market_config). These occupy what used to
    // be padding, so markets created before they existed read them as zero.
    pub session_schedule: u8,                 // crate::session::SCHEDULE_*; 0 = always open
    pub pre_close_window_secs: u32,           // Weekend margins apply this long before the weekly close; 0 = off
    pub weekend_initial_margin_ratio: u32,    // Basis points; 0 = use initial_margin_ratio
    pub weekend_maintenance_margin_ratio: u32, // Basis points; 0 = use maintenance_margin_ratio
    pub max_confidence_bps: u32,              // Max Pyth confidence / price; 0 = oracle default (1%)
    pub min_oracle_price: u64,                // 6 decimals; prices below halt the market; 0 = off
    pub settlement_price: u64,                // 6 decimals; non-zero once settle_market has run
    pub funding_rate: i64,                    // Latest per-period funding rate (6 decimals, 100 = 0.01%)
    pub funding_carry_secs: u32,              // Open seconds accrued but not yet charged (docs/lp-pool.md §8)
    pub settled_at: i64,                      // Unix time settle_market ran; 0 = not settled (or settled before it existed)
}

impl Market {
    pub const LEN: usize = 8 +  // discriminator
        32 +  // authority
        32 +  // collateral_mint
        32 +  // vault
        32 +  // pyth_price_feed
        8 +   // commodity identifier
        4 +   // max_leverage
        4 +   // maintenance_margin_ratio
        4 +   // initial_margin_ratio
        4 +   // taker_fee
        4 +   // maker_fee
        4 +   // liquidation_fee
        8 +   // long_open_interest
        8 +   // short_open_interest
        8 +   // max_open_interest
        8 +   // cumulative_funding
        8 +   // last_funding_time
        8 +   // funding_interval
        8 +   // insurance_fund
        8 +   // total_positions
        8 +   // total_trades
        1 +   // bump
        1 +   // is_paused
        1 +   // session_schedule
        4 +   // pre_close_window_secs
        4 +   // weekend_initial_margin_ratio
        4 +   // weekend_maintenance_margin_ratio
        4 +   // max_confidence_bps
        8 +   // min_oracle_price
        8 +   // settlement_price
        8 +   // funding_rate
        4 +   // funding_carry_secs
        8 +   // settled_at
        3;    // padding for future use

    /// Convert commodity bytes to string
    pub fn commodity_str(&self) -> String {
        String::from_utf8_lossy(&self.commodity)
            .trim_end_matches('\0')
            .to_string()
    }

    /// PDA seed for this market's commodity: the identifier without padding.
    pub fn commodity_seed(&self) -> &[u8] {
        let len = self.commodity.iter().position(|&b| b == 0).unwrap_or(self.commodity.len());
        &self.commodity[..len]
    }

    /// 1-8 uppercase ASCII letters or digits, e.g. "OIL", "NATGAS".
    pub fn is_valid_commodity(s: &str) -> bool {
        !s.is_empty() && s.len() <= 8 && s.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
    }

    /// Create commodity bytes from string
    pub fn commodity_from_str(s: &str) -> [u8; 8] {
        let mut bytes = [0u8; 8];
        let src = s.as_bytes();
        let len = src.len().min(8);
        bytes[..len].copy_from_slice(&src[..len]);
        bytes
    }

    pub fn can_increase_long_oi(&self, size: u64) -> bool {
        self.long_open_interest.checked_add(size)
            .map(|new_oi| new_oi <= self.max_open_interest)
            .unwrap_or(false)
    }

    pub fn can_increase_short_oi(&self, size: u64) -> bool {
        self.short_open_interest.checked_add(size)
            .map(|new_oi| new_oi <= self.max_open_interest)
            .unwrap_or(false)
    }
}

impl Market {
    pub fn is_settled(&self) -> bool {
        self.settlement_price > 0
    }

    /// Whether the market is within its trading session at `now`.
    pub fn is_in_session(&self, now: i64) -> bool {
        session::is_open(self.session_schedule, now)
    }

    /// Whether funding has been cranked recently enough for LP flows (§8):
    /// settled, or fewer than `funding_interval` open seconds (plus carry)
    /// since the last crank.
    pub fn funding_current(&self, now: i64) -> bool {
        session::funding_current(
            self.session_schedule,
            self.is_settled(),
            self.last_funding_time,
            self.funding_carry_secs,
            self.funding_interval,
            now,
        )
    }

    /// Whether weekend margin requirements apply at `now`.
    pub fn in_pre_close_window(&self, now: i64) -> bool {
        session::in_pre_close_window(self.session_schedule, now, self.pre_close_window_secs)
    }

    /// `(initial, maintenance)` margin ratios in force at `now`, in basis points.
    /// Before the weekly close they rise to the weekend values, so positions
    /// are carried over the closure with enough margin to absorb a gap.
    pub fn margin_ratios(&self, now: i64) -> (u32, u32) {
        if self.in_pre_close_window(now) {
            (
                self.initial_margin_ratio.max(self.weekend_initial_margin_ratio),
                self.maintenance_margin_ratio.max(self.weekend_maintenance_margin_ratio),
            )
        } else {
            (self.initial_margin_ratio, self.maintenance_margin_ratio)
        }
    }

    /// Collateral to post at `now` for a position of `notional` at `leverage`
    /// (3 decimals, 10x = 10_000): `notional / leverage`, rounded up.
    /// Returns `None` if that is below the initial margin in force, i.e. the
    /// leverage is higher than the market currently allows.
    pub fn required_collateral(&self, notional: u64, leverage: u32, now: i64) -> Option<u64> {
        if leverage == 0 {
            return None;
        }
        let (initial_margin_ratio, _) = self.margin_ratios(now);
        let collateral = ceil_div(notional as u128 * 1_000, leverage as u128);
        let initial_margin = ceil_div(notional as u128 * initial_margin_ratio as u128, 10_000);
        if collateral < initial_margin {
            return None;
        }
        u64::try_from(collateral).ok()
    }

    /// Stage-2 liquidator reward target: `liquidation_fee` applied to the
    /// maintenance margin **in force** (`mmr`) on `notional`, which callers
    /// pass as the position's `opened_notional` (docs/lp-pool.md §6.4, D8).
    /// Basing it on the margin requirement rather than on whatever collateral
    /// is left keeps the reward from falling to zero on positions that are
    /// already underwater, which are the ones that most need liquidating.
    pub fn liquidation_reward_target(&self, notional: u64, mmr: u32) -> u64 {
        let reward = (notional as u128) * mmr as u128 * self.liquidation_fee as u128 / 100_000_000;
        reward.min(u64::MAX as u128) as u64
    }
}

pub(crate) fn ceil_div(a: u128, b: u128) -> u128 {
    (a + b - 1) / b
}

/// Funding for one period per whole contract, in USDC (6 decimals): the
/// period's rate (6 decimals) applied to that period's price. Accumulating
/// this, rather than the bare rate, charges each period at its own price.
pub fn funding_per_contract(rate: i64, price: u64) -> i64 {
    let amount = rate as i128 * price as i128 / 1_000_000;
    amount.clamp(i64::MIN as i128, i64::MAX as i128) as i64
}

/// Split a position's final equity into what is paid out and what is owed.
/// Returns `(payout, deficit)`; at most one of them is non-zero.
pub fn split_equity(equity: i64) -> (u64, u64) {
    if equity >= 0 {
        (equity as u64, 0)
    } else {
        (0, equity.unsigned_abs())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// OIL as configured by scripts/initialize-market.ts.
    fn oil_market() -> Market {
        Market {
            max_leverage: 20_000,
            initial_margin_ratio: 1_000,
            maintenance_margin_ratio: 500,
            liquidation_fee: 100,
            ..Default::default()
        }
    }

    const NOTIONAL: u64 = 7_500_000_000; // $7,500

    #[test]
    fn collateral_follows_chosen_leverage() {
        let m = oil_market();
        assert_eq!(m.required_collateral(NOTIONAL, 2_000, 0), Some(3_750_000_000)); // 2x
        assert_eq!(m.required_collateral(NOTIONAL, 5_000, 0), Some(1_500_000_000)); // 5x
        assert_eq!(m.required_collateral(NOTIONAL, 10_000, 0), Some(750_000_000)); // 10x
    }

    #[test]
    fn leverage_above_initial_margin_is_rejected() {
        // 10% initial margin caps leverage at 10x even though max_leverage is 20x
        let m = oil_market();
        assert_eq!(m.required_collateral(NOTIONAL, 10_001, 0), None);
        assert_eq!(m.required_collateral(NOTIONAL, 20_000, 0), None);
        assert_eq!(m.required_collateral(NOTIONAL, 0, 0), None);
    }

    #[test]
    fn collateral_rounds_up() {
        // $1 at 3x = 0.333334
        let m = Market { initial_margin_ratio: 1_000, ..Default::default() };
        assert_eq!(m.required_collateral(1_000_000, 3_000, 0), Some(333_334));
    }

    #[test]
    fn commodity_seed_is_the_unpadded_identifier() {
        for id in ["OIL", "GOLD", "NATGAS", "COPPER12"] {
            let m = Market { commodity: Market::commodity_from_str(id), ..Default::default() };
            assert_eq!(m.commodity_seed(), id.as_bytes());
        }
    }

    #[test]
    fn market_and_user_addresses_match_off_chain_derivation() {
        // Same values are pinned in frontend/src/utils/pda.test.ts and the keeper.
        let mint: Pubkey = "4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU".parse().unwrap();
        let oil = Market { commodity: Market::commodity_from_str("OIL"), collateral_mint: mint, ..Default::default() };
        let (market, _) = Pubkey::find_program_address(
            &[b"market", oil.collateral_mint.as_ref(), oil.commodity_seed()],
            &crate::ID,
        );
        assert_eq!(market.to_string(), "6Chy7k9ArMFQamqogr5jsYVoRTbnP9TaCxvDqRV6ntD9");

        let owner: Pubkey = "11111111111111111111111111111112".parse().unwrap();
        let (user, _) = Pubkey::find_program_address(&[b"user", owner.as_ref(), market.as_ref()], &crate::ID);
        assert_eq!(user.to_string(), "4NBuxBaDE8SpGFiytjqTmkYqkyc628SAsGxrc51jkHso");
    }

    #[test]
    fn validates_commodity_identifiers() {
        assert!(Market::is_valid_commodity("OIL"));
        assert!(Market::is_valid_commodity("NATGAS"));
        for bad in ["", "oil", "OIL GAS", "TOOLONGID", "OIL\0", "ÖL"] {
            assert!(!Market::is_valid_commodity(bad), "{:?}", bad);
        }
    }

    #[test]
    fn each_commodity_gets_its_own_market_address() {
        let program = crate::ID;
        let mint = Pubkey::new_unique();
        let pda = |c: &str| Pubkey::find_program_address(&[b"market", mint.as_ref(), c.as_bytes()], &program).0;
        let ids = ["OIL", "GOLD", "SILVER", "NATGAS", "COPPER"];
        let addresses: std::collections::HashSet<_> = ids.iter().map(|c| pda(c)).collect();
        assert_eq!(addresses.len(), ids.len());
        // The init seed (raw string) matches the seed later instructions use
        let oil = Market { commodity: Market::commodity_from_str("OIL"), collateral_mint: mint, ..Default::default() };
        assert_eq!(
            Pubkey::find_program_address(&[b"market", oil.collateral_mint.as_ref(), oil.commodity_seed()], &program).0,
            pda("OIL")
        );
    }

    #[test]
    fn account_size_is_unchanged_so_existing_markets_still_load() {
        assert_eq!(Market::LEN, 298);
        let serialized = Market::default().try_to_vec().unwrap().len();
        assert_eq!(8 + serialized + 3, Market::LEN);
    }

    /// docs/lp-pool.md §1.1: `funding_carry_secs` is appended after
    /// `funding_rate`, at 283..287.
    #[test]
    fn funding_carry_secs_is_appended() {
        use anchor_lang::AccountSerialize;
        let m = Market { funding_rate: -2, funding_carry_secs: 0x0a0b_0c0d, settled_at: -3, ..Default::default() };
        let mut b = Vec::new();
        m.try_serialize(&mut b).unwrap();
        assert_eq!(&b[275..283], &(-2i64).to_le_bytes());
        assert_eq!(&b[283..287], &0x0a0b_0c0du32.to_le_bytes());
        // settled_at (docs/lp-pool.md §0 D35) follows at 287..295
        assert_eq!(&b[287..295], &(-3i64).to_le_bytes());
        assert_eq!(b.len(), 295);
    }

    #[test]
    fn funding_is_current_until_an_interval_of_open_time_passes() {
        // Wed 2026-11-11 12:00 ET; CME, hourly
        let m = Market {
            session_schedule: session::SCHEDULE_CME_GLOBEX,
            last_funding_time: WEDNESDAY_NOON_ET,
            funding_interval: 3_600,
            ..oil_market()
        };
        assert!(m.funding_current(WEDNESDAY_NOON_ET + 3_599));
        assert!(!m.funding_current(WEDNESDAY_NOON_ET + 3_600));
        // Carry counts
        let c = Market { funding_carry_secs: 600, ..m.clone() };
        assert!(c.funding_current(WEDNESDAY_NOON_ET + 2_999));
        assert!(!c.funding_current(WEDNESDAY_NOON_ET + 3_000));
        // Closed time does not count: last crank 30 min before the weekly
        // close is still current all weekend and until 30 min after the reopen
        let fri = Market { last_funding_time: FRIDAY_CLOSE - 1_800, ..m.clone() };
        let sunday_open = FRIDAY_CLOSE + 49 * 3_600;
        assert!(fri.funding_current(sunday_open + 1_799));
        assert!(!fri.funding_current(sunday_open + 1_800));
        // Settled markets are always current
        let s = Market { settlement_price: 1, ..m };
        assert!(s.funding_current(WEDNESDAY_NOON_ET + 10 * 86_400));
    }

    // Fri 2026-11-13: 22:00 UTC is the 17:00 ET weekly close.
    const FRIDAY_CLOSE: i64 = 1_794_607_200;
    const WEDNESDAY_NOON_ET: i64 = 1_794_416_400; // 2026-11-11 17:00 UTC

    fn oil_market_with_weekend_margins() -> Market {
        Market {
            session_schedule: session::SCHEDULE_CME_GLOBEX,
            pre_close_window_secs: 7_200,
            weekend_initial_margin_ratio: 2_000,
            weekend_maintenance_margin_ratio: 1_000,
            ..oil_market()
        }
    }

    #[test]
    fn weekend_margins_apply_only_before_weekly_close() {
        let m = oil_market_with_weekend_margins();
        assert_eq!(m.margin_ratios(WEDNESDAY_NOON_ET), (1_000, 500));
        assert_eq!(m.margin_ratios(FRIDAY_CLOSE - 7_201), (1_000, 500));
        assert_eq!(m.margin_ratios(FRIDAY_CLOSE - 7_200), (2_000, 1_000));
        assert_eq!(m.margin_ratios(FRIDAY_CLOSE - 1), (2_000, 1_000));
    }

    #[test]
    fn weekend_initial_margin_caps_leverage() {
        let m = oil_market_with_weekend_margins();
        assert_eq!(m.required_collateral(NOTIONAL, 10_000, WEDNESDAY_NOON_ET), Some(750_000_000));
        // 20% weekend initial margin: 10x rejected, 5x allowed
        assert_eq!(m.required_collateral(NOTIONAL, 10_000, FRIDAY_CLOSE - 60), None);
        assert_eq!(m.required_collateral(NOTIONAL, 5_000, FRIDAY_CLOSE - 60), Some(1_500_000_000));
    }

    #[test]
    fn weekend_margins_never_lower_requirements() {
        let m = Market { weekend_initial_margin_ratio: 100, weekend_maintenance_margin_ratio: 0, ..oil_market_with_weekend_margins() };
        assert_eq!(m.margin_ratios(FRIDAY_CLOSE - 60), (1_000, 500));
    }

    #[test]
    fn session_follows_schedule() {
        let m = oil_market_with_weekend_margins();
        assert!(m.is_in_session(FRIDAY_CLOSE - 1));
        assert!(!m.is_in_session(FRIDAY_CLOSE));
        assert!(oil_market().is_in_session(FRIDAY_CLOSE)); // always-open default
    }

    #[test]
    fn liquidation_reward_is_share_of_maintenance_margin_in_force() {
        // 1% of the 5% maintenance margin on $7,500 = $3.75
        assert_eq!(oil_market().liquidation_reward_target(NOTIONAL, 500), 3_750_000);
        // The weekend MMR (10%) doubles it
        assert_eq!(oil_market().liquidation_reward_target(NOTIONAL, 1_000), 7_500_000);
    }

    // The stage-2 split that replaced liquidation_payout / apply_insurance is
    // tested in instructions/liquidate.rs (§6.4).

    #[test]
    fn funding_per_contract_uses_the_period_price() {
        // 0.01% of $75 = $0.0075 per contract
        assert_eq!(funding_per_contract(100, 75_000_000), 7_500);
        assert_eq!(funding_per_contract(100, 150_000_000), 15_000);
        assert_eq!(funding_per_contract(-100, 75_000_000), -7_500);
    }

    #[test]
    fn split_equity_handles_both_signs() {
        assert_eq!(split_equity(250), (250, 0));
        assert_eq!(split_equity(0), (0, 0));
        assert_eq!(split_equity(-75), (0, 75));
        assert_eq!(split_equity(i64::MIN), (0, i64::MIN.unsigned_abs()));
    }
}

#[account]
#[derive(Default)]
pub struct Vault {
    pub market: Pubkey,
    pub collateral_mint: Pubkey,
    pub token_account: Pubkey,
    pub total_deposits: u64,
    pub bump: u8,
}

impl Vault {
    pub const LEN: usize = 8 + 32 + 32 + 32 + 8 + 1 + 32;
}
