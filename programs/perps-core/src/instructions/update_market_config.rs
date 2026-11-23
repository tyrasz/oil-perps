use anchor_lang::prelude::*;
use crate::errors::PerpsError;
use crate::session;
use crate::state::{Market, ProtocolConfig, MAX_OPEN_INTEREST};

/// Authority-controlled risk settings. See the field docs on `Market`.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MarketConfigParams {
    pub is_paused: bool,
    pub session_schedule: u8,
    pub pre_close_window_secs: u32,
    pub weekend_initial_margin_ratio: u32,
    pub weekend_maintenance_margin_ratio: u32,
    pub max_confidence_bps: u32,
    pub min_oracle_price: u64,
    /// Per-side open-interest cap in contracts (D6). `0` keeps the current
    /// value, so `Default`-based callers are unchanged (red-team F19). It may be
    /// set below the current OI, and then only blocks increases.
    pub max_open_interest: u64,
}

const MAX_PRE_CLOSE_WINDOW_SECS: u32 = 2 * 24 * 3_600;

impl MarketConfigParams {
    /// Check the params against the market's base margins.
    pub fn validate(&self, market: &Market) -> bool {
        let weekend_initial = market.initial_margin_ratio.max(self.weekend_initial_margin_ratio);
        let weekend_maintenance = market.maintenance_margin_ratio.max(self.weekend_maintenance_margin_ratio);

        session::is_valid_schedule(self.session_schedule)
            && self.pre_close_window_secs <= MAX_PRE_CLOSE_WINDOW_SECS
            && self.weekend_initial_margin_ratio <= 10_000
            // Opening a position must still leave room above the liquidation line
            && weekend_maintenance < weekend_initial
            && self.max_confidence_bps <= 10_000
            && self.max_open_interest <= MAX_OPEN_INTEREST
    }
}

#[derive(Accounts)]
pub struct UpdateMarketConfig<'info> {
    pub authority: Signer<'info>,

    // Checked against the protocol admin (not the market's creator), so an
    // admin handover covers existing markets.
    #[account(
        seeds = [ProtocolConfig::SEED],
        bump = config.bump,
        constraint = config.admin == authority.key() @ PerpsError::Unauthorized
    )]
    pub config: Account<'info, ProtocolConfig>,

    #[account(
        mut,
        seeds = [b"market", market.collateral_mint.as_ref(), market.commodity_seed()],
        bump = market.bump,
    )]
    pub market: Account<'info, Market>,
}

pub fn handler(ctx: Context<UpdateMarketConfig>, params: MarketConfigParams) -> Result<()> {
    let market = &mut ctx.accounts.market;
    require!(!market.is_settled(), PerpsError::MarketSettled);
    require!(params.validate(market), PerpsError::InvalidMarketConfig);

    market.is_paused = params.is_paused;
    market.session_schedule = params.session_schedule;
    market.pre_close_window_secs = params.pre_close_window_secs;
    market.weekend_initial_margin_ratio = params.weekend_initial_margin_ratio;
    market.weekend_maintenance_margin_ratio = params.weekend_maintenance_margin_ratio;
    market.max_confidence_bps = params.max_confidence_bps;
    market.min_oracle_price = params.min_oracle_price;
    if params.max_open_interest != 0 {
        market.max_open_interest = params.max_open_interest;
    }

    msg!("Market config updated: {:?}", params);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn market() -> Market {
        Market { initial_margin_ratio: 1_000, maintenance_margin_ratio: 500, ..Default::default() }
    }

    fn oil_weekend() -> MarketConfigParams {
        MarketConfigParams {
            session_schedule: session::SCHEDULE_CME_GLOBEX,
            pre_close_window_secs: 7_200,
            weekend_initial_margin_ratio: 2_000,
            weekend_maintenance_margin_ratio: 1_000,
            ..Default::default()
        }
    }

    #[test]
    fn accepts_typical_weekend_config() {
        assert!(oil_weekend().validate(&market()));
        assert!(MarketConfigParams::default().validate(&market()));
    }

    #[test]
    fn max_open_interest_is_bounded() {
        let p = |x| MarketConfigParams { max_open_interest: x, ..oil_weekend() };
        assert!(p(0).validate(&market()));
        assert!(p(MAX_OPEN_INTEREST).validate(&market()));
        assert!(!p(MAX_OPEN_INTEREST + 1).validate(&market()));
    }

    #[test]
    fn rejects_unknown_schedule() {
        assert!(!MarketConfigParams { session_schedule: 7, ..oil_weekend() }.validate(&market()));
    }

    #[test]
    fn rejects_weekend_maintenance_at_or_above_initial() {
        let p = MarketConfigParams { weekend_maintenance_margin_ratio: 2_000, ..oil_weekend() };
        assert!(!p.validate(&market()));
        // Only maintenance raised above the normal 10% initial margin
        let p = MarketConfigParams { weekend_initial_margin_ratio: 0, ..oil_weekend() };
        assert!(!p.validate(&market()));
    }

    #[test]
    fn rejects_out_of_range_values() {
        assert!(!MarketConfigParams { pre_close_window_secs: 3 * 86_400, ..oil_weekend() }.validate(&market()));
        assert!(!MarketConfigParams { max_confidence_bps: 10_001, ..oil_weekend() }.validate(&market()));
        assert!(!MarketConfigParams { weekend_initial_margin_ratio: 10_001, ..oil_weekend() }.validate(&market()));
    }
}
