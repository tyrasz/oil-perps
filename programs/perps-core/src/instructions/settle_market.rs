use anchor_lang::prelude::*;
use crate::errors::PerpsError;
use crate::state::Market;

#[derive(Accounts)]
pub struct SettleMarket<'info> {
    pub authority: Signer<'info>,

    #[account(
        mut,
        seeds = [b"market", market.collateral_mint.as_ref()],
        bump = market.bump,
        constraint = market.authority == authority.key() @ PerpsError::Unauthorized,
    )]
    pub market: Account<'info, Market>,
}

/// Emergency settlement for when the oracle can no longer price the market,
/// e.g. the underlying goes negative as WTI did on 20 April 2020.
///
/// This is final: the market is paused for good, funding and liquidations
/// stop, and every position can only be closed, at `settlement_price`
/// (6 decimals). Losses beyond collateral are absorbed by the insurance fund.
pub fn handler(ctx: Context<SettleMarket>, settlement_price: u64) -> Result<()> {
    let market = &mut ctx.accounts.market;
    require!(!market.is_settled(), PerpsError::MarketSettled);
    require!(settlement_price > 0, PerpsError::InvalidOraclePrice);

    market.settlement_price = settlement_price;
    market.is_paused = true;

    msg!("Market settled at {}", settlement_price);
    Ok(())
}
