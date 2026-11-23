use anchor_lang::prelude::*;

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Default)]
pub enum Side {
    #[default]
    Long,
    Short,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Default)]
pub enum PositionStatus {
    #[default]
    Open,
    Closed,
    Liquidated,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExecutionSource {
    #[default]
    OrderBook,
    Amm,
    // Opened by venue_fill. Must stay last so existing tags keep their meaning.
    Venue,
}

#[account]
#[derive(Default)]
pub struct Position {
    pub owner: Pubkey,
    pub market: Pubkey,

    // Position details
    pub side: Side,
    pub size: u64,                    // Position size in base units (oil contracts)
    pub collateral: u64,              // Deposited collateral in quote units (USDC)
    pub entry_price: u64,             // Average entry price (6 decimals)
    pub leverage: u32,                // Leverage used (3 decimals, 10x = 10_000)

    // PnL tracking
    pub realized_pnl: i64,            // Cumulative realized PnL
    pub last_funding_payment: i64,    // Market::cumulative_funding when funding was last settled

    // Timestamps
    pub opened_at: i64,
    pub last_updated_at: i64,

    pub status: PositionStatus,
    pub execution_source: ExecutionSource,
    pub bump: u8,

    // docs/lp-pool.md §1.1 (appended). Set by `init_open`, never changed:
    // `entry_price` is the PnL basis and settle_position_pnl resets it.
    pub opened_price: u64,
}

impl Position {
    pub const LEN: usize = 8 +  // discriminator
        32 +  // owner
        32 +  // market
        1 +   // side
        8 +   // size
        8 +   // collateral
        8 +   // entry_price
        4 +   // leverage
        8 +   // realized_pnl
        8 +   // last_funding_payment
        8 +   // opened_at
        8 +   // last_updated_at
        1 +   // status
        1 +   // execution_source
        1 +   // bump
        8 +   // opened_price
        24;   // padding

    /// Set every field of a freshly created open position.
    #[allow(clippy::too_many_arguments)]
    pub fn init_open(
        &mut self,
        owner: Pubkey,
        market: Pubkey,
        side: Side,
        size: u64,
        collateral: u64,
        entry_price: u64,
        leverage: u32,
        cumulative_funding: i64,
        now: i64,
        execution_source: ExecutionSource,
        bump: u8,
    ) {
        self.owner = owner;
        self.market = market;
        self.side = side;
        self.size = size;
        self.collateral = collateral;
        self.entry_price = entry_price;
        self.leverage = leverage;
        self.realized_pnl = 0;
        self.last_funding_payment = cumulative_funding;
        self.opened_at = now;
        self.last_updated_at = now;
        self.status = PositionStatus::Open;
        self.execution_source = execution_source;
        self.bump = bump;
        self.opened_price = entry_price;
    }

    /// `floor(size·opened_price, 1e6)`: the notional this position was opened
    /// at, for `size` of it (D21). Per-account house notional, the close fee
    /// and the stage-2 reward use it; removing a slice `x` of `q` removes
    /// `opened_notional(q) − opened_notional(q − x)` so the sum telescopes.
    pub fn opened_notional(&self, size: u64) -> u64 {
        crate::pool_math::notional_floor(size, self.opened_price)
    }

    /// `C + pnl(mark) + accrued_funding(F)`, exact in i128 (§6.1).
    pub fn equity(&self, mark: u64, cumulative_funding: i64) -> i128 {
        self.collateral as i128 + self.unrealized_pnl(mark) as i128 + self.accrued_funding(cumulative_funding) as i128
    }

    pub fn notional_value(&self) -> u64 {
        // size * entry_price / 1_000_000 (adjust for decimals)
        (self.size as u128)
            .checked_mul(self.entry_price as u128)
            .and_then(|v| v.checked_div(1_000_000))
            .unwrap_or(0) as u64
    }

    pub fn unrealized_pnl(&self, current_price: u64) -> i64 {
        let entry = self.entry_price as i128;
        let current = current_price as i128;
        let size = self.size as i128;

        let price_diff = match self.side {
            Side::Long => current - entry,
            Side::Short => entry - current,
        };

        // PnL = size * price_diff / 1_000_000
        ((size * price_diff) / 1_000_000) as i64
    }

    /// Funding accrued since the position last settled, in collateral units
    /// (USDC, 6 decimals). Positive means the position is owed funding.
    ///
    /// `cumulative_funding` is the market's running funding per whole
    /// contract in USDC, accumulated at each period's price.
    pub fn accrued_funding(&self, cumulative_funding: i64) -> i64 {
        let per_contract = cumulative_funding as i128 - self.last_funding_payment as i128;
        let payment = (self.size as i128) * per_contract / 1_000_000;

        // Positive funding: longs pay shorts.
        let signed = match self.side {
            Side::Long => -payment,
            Side::Short => payment,
        };
        signed.clamp(i64::MIN as i128, i64::MAX as i128) as i64
    }

    /// Equity (collateral + unrealized PnL + accrued funding) as a share of
    /// notional, in basis points.
    pub fn margin_ratio(&self, current_price: u64, accrued_funding: i64) -> u32 {
        let pnl = self.unrealized_pnl(current_price);
        let equity = (self.collateral as i64)
            .saturating_add(pnl)
            .saturating_add(accrued_funding);

        if equity <= 0 {
            return 0;
        }

        let notional = self.notional_value();
        if notional == 0 {
            return 0;
        }

        // margin_ratio = equity / notional * 10000 (basis points)
        let ratio = (equity as u128) * 10_000 / notional as u128;
        ratio.min(u32::MAX as u128) as u32
    }

    /// `floor(size·mark, 1e6)` (u128): the mark notional liquidation uses (§6.1).
    pub fn mark_notional(&self, mark: u64) -> u128 {
        self.size as u128 * mark as u128 / 1_000_000
    }

    /// Equity as a share of **mark** notional, in bps (display, §6.1).
    /// `margin_ratio` keeps entry notional (the venue backstop and the UI).
    pub fn margin_ratio_mark(&self, mark: u64, accrued_funding: i64) -> u32 {
        let equity = self.collateral as i128 + self.unrealized_pnl(mark) as i128 + accrued_funding as i128;
        let n = self.mark_notional(mark);
        if equity <= 0 || n == 0 {
            return 0;
        }
        (equity as u128 * 10_000 / n).min(u32::MAX as u128) as u32
    }

    /// Liquidatable on mark notional (§6.1): `equity·10_000 < mmr·N_mark`,
    /// with `N_mark = floor(size·mark, 1e6)`; with `N_mark == 0` only when
    /// equity is negative.
    pub fn is_liquidatable(
        &self,
        current_price: u64,
        accrued_funding: i64,
        maintenance_margin_ratio: u32,
    ) -> bool {
        let equity = self.collateral as i128 + self.unrealized_pnl(current_price) as i128 + accrued_funding as i128;
        let n = self.mark_notional(current_price);
        if n == 0 {
            return equity < 0;
        }
        // |equity| < 2^65 and n·mmr < 2^64·2^32, so neither side overflows
        equity * 10_000 < (maintenance_margin_ratio as u128 * n) as i128
    }
}

#[account]
#[derive(Default)]
pub struct UserAccount {
    pub owner: Pubkey,
    pub collateral_balance: u64,      // Free collateral not in positions
    pub total_positions: u32,
    pub total_trades: u64,
    pub realized_pnl: i64,
    pub bump: u8,
    // docs/lp-pool.md §1.1 (appended): Σ opened_notional of this user's open
    // ExecutionSource::OrderBook positions (per-account cap, D21).
    pub house_entry_notional: u64,
}

impl UserAccount {
    pub const LEN: usize = 8 + 32 + 8 + 4 + 8 + 8 + 1 + 8 + 8;
}

#[cfg(test)]
mod tests {
    use super::*;

    const USDC: i64 = 1_000_000;

    /// 100 contracts of oil at $75 with $750 collateral (10x).
    fn oil_position(side: Side) -> Position {
        Position {
            side,
            size: 100 * 1_000_000,
            collateral: 750 * USDC as u64,
            entry_price: 75_000_000,
            last_funding_payment: 0,
            ..Default::default()
        }
    }

    use crate::state::funding_per_contract;

    #[test]
    fn funding_scales_with_notional_not_contract_count() {
        // 0.01% on $7,500 notional = $0.75
        let long = oil_position(Side::Long);
        assert_eq!(long.accrued_funding(funding_per_contract(100, 75_000_000)), -750_000);
        // Same rate at $150 oil costs twice as much
        assert_eq!(long.accrued_funding(funding_per_contract(100, 150_000_000)), -1_500_000);
    }

    #[test]
    fn each_period_is_charged_at_its_own_price() {
        // 0.01% at $75, then 0.01% at $150: $0.75 + $1.50
        let index = funding_per_contract(100, 75_000_000) + funding_per_contract(100, 150_000_000);
        assert_eq!(oil_position(Side::Long).accrued_funding(index), -2_250_000);
    }

    #[test]
    fn funding_sign_depends_on_side_and_rate() {
        assert_eq!(oil_position(Side::Short).accrued_funding(7_500), 750_000);
        assert_eq!(oil_position(Side::Long).accrued_funding(-7_500), 750_000);
    }

    #[test]
    fn funding_only_counts_since_last_settlement() {
        let mut long = oil_position(Side::Long);
        long.last_funding_payment = 22_500;
        assert_eq!(long.accrued_funding(30_000), -750_000);
        assert_eq!(long.accrued_funding(22_500), 0);
    }

    #[test]
    fn margin_ratio_includes_accrued_funding() {
        let long = oil_position(Side::Long);
        // $750 equity on $7,500 notional = 10%
        assert_eq!(long.margin_ratio(75_000_000, 0), 1_000);
        // $400 of unpaid funding leaves $350 = 4.67%
        assert_eq!(long.margin_ratio(75_000_000, -400 * USDC), 466);
        assert!(!long.is_liquidatable(75_000_000, 0, 500));
        assert!(long.is_liquidatable(75_000_000, -400 * USDC, 500));
    }

    #[test]
    fn account_sizes_are_pinned() {
        assert_eq!(Position::LEN, 168);
        let serialized = Position::default().try_to_vec().unwrap().len();
        assert_eq!(8 + serialized + 24, Position::LEN);
        assert_eq!(ExecutionSource::Venue.try_to_vec().unwrap(), vec![2]);
        assert_eq!(UserAccount::LEN, 85);
        assert_eq!(8 + UserAccount::default().try_to_vec().unwrap().len() + 8, UserAccount::LEN);
    }

    /// docs/lp-pool.md §1.1: appended fields, existing offsets unchanged.
    #[test]
    fn appended_fields_have_pinned_offsets() {
        use anchor_lang::AccountSerialize;
        let p = Position { bump: 0xee, opened_price: 0x0102_0304_0506_0708, ..Default::default() };
        let mut b = Vec::new();
        p.try_serialize(&mut b).unwrap();
        assert_eq!(b[135], 0xee);
        assert_eq!(&b[136..144], &0x0102_0304_0506_0708u64.to_le_bytes());
        let u = UserAccount { bump: 0xdd, house_entry_notional: 0x1112_1314_1516_1718, ..Default::default() };
        let mut b = Vec::new();
        u.try_serialize(&mut b).unwrap();
        assert_eq!(b[68], 0xdd);
        assert_eq!(&b[69..77], &0x1112_1314_1516_1718u64.to_le_bytes());
    }

    #[test]
    fn init_open_sets_opened_price() {
        let mut p = Position::default();
        p.init_open(Pubkey::default(), Pubkey::default(), Side::Long, 5, 6, 75_030_000, 10_000, 7, 8, ExecutionSource::OrderBook, 9);
        assert_eq!((p.entry_price, p.opened_price), (75_030_000, 75_030_000));
        assert_eq!(p.opened_notional(100 * CONTRACT_SIZE), 7_503_000_000);
    }

    /// §17.1 #16: random partial slices remove exactly opened_notional(size).
    #[test]
    fn opened_notional_telescopes() {
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..2_000 {
            let size = 1 + next() % 1_000_000_000_000;
            let p = Position { size, opened_price: 1 + next() % 200_000_000, ..Default::default() };
            let total = p.opened_notional(size);
            let (mut q, mut removed) = (size, 0u64);
            while q > 0 {
                let x = 1 + next() % q;
                removed += p.opened_notional(q) - p.opened_notional(q - x);
                q -= x;
            }
            assert_eq!(removed, total);
            // A settle (entry_price reset) does not change it
            let settled = Position { entry_price: 1, ..p.clone() };
            assert_eq!(settled.opened_notional(size), total);
        }
    }

    #[test]
    fn equity_is_collateral_plus_pnl_plus_funding() {
        let long = oil_position(Side::Long);
        assert_eq!(long.equity(76_000_000, 0), 850 * USDC as i128);
        assert_eq!(long.equity(75_000_000, 7_500), 750 * USDC as i128 - 750_000);
    }

    /// Two legs of a venue fill share size and entry price; their PnL and
    /// funding must cancel exactly, including the truncated last unit.
    #[test]
    fn pnl_and_funding_are_odd_in_side() {
        let sizes = [1, 7, 999_999, CONTRACT_SIZE, 123 * CONTRACT_SIZE + 456_789];
        let entries = [1, 999_999, 75_030_000, 74_970_001];
        let marks = [1, 3, 75_000_000, 78_123_457, 100_000_001];
        let indices = [-1_234_567_891, -7_500, -1, 0, 1, 7_499, 22_500, 9_876_543_210];
        for &size in &sizes {
            for &entry_price in &entries {
                let long = Position { side: Side::Long, size, entry_price, last_funding_payment: 11, ..Default::default() };
                let short = Position { side: Side::Short, ..long.clone() };
                for &mark in &marks {
                    assert_eq!(long.unrealized_pnl(mark), -short.unrealized_pnl(mark), "{} {} {}", size, entry_price, mark);
                }
                for &index in &indices {
                    assert_eq!(long.accrued_funding(index), -short.accrued_funding(index), "{} {}", size, index);
                }
            }
        }
    }

    /// Random companion of the grid above, over the full ranges the engine
    /// sees (sizes up to MAX_OPEN_INTEREST, any i64 index delta that does not
    /// clamp): matched legs cancel exactly, so a matched pair realizes 0.
    #[test]
    fn pnl_and_funding_are_odd_in_side_random() {
        let mut x = 0xdead_beef_cafe_f00du64;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for _ in 0..200_000 {
            let size = 1 + next() % crate::pool_math::MAX_OPEN_INTEREST;
            let entry_price = 1 + next() % 1_000_000_000;
            let f0 = (next() % 2_000_000_000) as i64 - 1_000_000_000;
            let long = Position { side: Side::Long, size, entry_price, last_funding_payment: f0, collateral: next() % 1_000_000, ..Default::default() };
            let short = Position { side: Side::Short, ..long.clone() };
            let mark = 1 + next() % 1_000_000_000;
            let index = (next() % 4_000_000_000) as i64 - 2_000_000_000;
            let (pl, ps) = (long.unrealized_pnl(mark), short.unrealized_pnl(mark));
            let (fl, fs) = (long.accrued_funding(index), short.accrued_funding(index));
            assert_eq!(pl, -ps);
            assert_eq!(fl, -fs);
            assert_eq!(long.equity(mark, index) + short.equity(mark, index), 2 * long.collateral as i128);
            // Truncation toward zero: within one unit of the exact value
            let exact = size as i128 * (mark as i128 - entry_price as i128);
            assert!((pl as i128 * 1_000_000 - exact).abs() < 1_000_000);
            assert!((pl as i128 * 1_000_000).abs() <= exact.abs());
        }
    }

    const CONTRACT_SIZE: u64 = 1_000_000;

    /// §17.1 #8: the trigger is on mark notional.
    #[test]
    fn liquidation_uses_mark_notional() {
        // A short at a loss has mark notional above entry notional, so the
        // mark rule is the stricter one. 100 contracts short at $75 with $750:
        // at $78.60 equity is $390, 5.2% of entry notional (safe on the old
        // rule) but 4.96% of the $7,860 mark notional (liquidatable).
        let short = oil_position(Side::Short);
        assert!(short.margin_ratio(78_600_000, 0) >= 500);
        assert!(short.is_liquidatable(78_600_000, 0, 500));
        assert_eq!(short.margin_ratio_mark(78_600_000, 0), 496);
        // A long at a loss is the other way round: at $71.20 equity is $370,
        // 4.93% of entry notional but 5.19% of mark notional (safe)
        let long = oil_position(Side::Long);
        assert!(long.margin_ratio(71_200_000, 0) < 500);
        assert!(!long.is_liquidatable(71_200_000, 0, 500));
        // Exactly at the line is not liquidatable: equity·1e4 == mmr·N
        let p = Position { side: Side::Long, size: 1_000_000, collateral: 5_000_000, entry_price: 100_000_000, ..Default::default() };
        assert!(!p.is_liquidatable(100_000_000, 0, 500));
        assert!(p.is_liquidatable(100_000_000, -1, 500));
        // Zero mark notional: only negative equity
        let dust = Position { side: Side::Long, size: 1, collateral: 0, entry_price: 1, ..Default::default() };
        assert!(!dust.is_liquidatable(1, 0, 500));
        assert!(dust.is_liquidatable(1, -1, 500));
    }

    #[test]
    fn margin_ratio_is_zero_when_underwater() {
        // $10 drop on 100 contracts = -$1,000 > $750 collateral
        assert_eq!(oil_position(Side::Long).margin_ratio(65_000_000, 0), 0);
    }
}
