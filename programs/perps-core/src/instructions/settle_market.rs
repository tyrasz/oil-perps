use anchor_lang::prelude::*;
use crate::errors::PerpsError;
use crate::instructions::update_funding::{apply_funding, log_funding};
use crate::state::{Market, ProtocolConfig};

#[derive(Accounts)]
pub struct SettleMarket<'info> {
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

/// Emergency settlement for when the oracle can no longer price the market,
/// e.g. the underlying goes negative as WTI did on 20 April 2020.
///
/// This is final: the funding due up to now is charged (D42), then the
/// market is paused for good, funding and liquidations stop, and every position can only be closed, at `settlement_price`
/// (6 decimals). Losses beyond collateral are absorbed by the insurance fund.
pub fn handler(ctx: Context<SettleMarket>, settlement_price: u64) -> Result<()> {
    let market = &mut ctx.accounts.market;
    require!(!market.is_settled(), PerpsError::MarketSettled);
    require!(settlement_price > 0, PerpsError::InvalidOraclePrice);

    // Charge the funding periods due since the last crank first, at the
    // settlement price (D42): nothing can crank a settled market, and every
    // position then closes on this cumulative_funding
    let now = Clock::get()?.unix_timestamp;
    if let Some(ev) = apply_funding(market, settlement_price, now) {
        log_funding(market, settlement_price, ev);
    }

    market.settlement_price = settlement_price;
    market.settled_at = now;
    market.is_paused = true;

    msg!("Market settled at {}", settlement_price);
    Ok(())
}
