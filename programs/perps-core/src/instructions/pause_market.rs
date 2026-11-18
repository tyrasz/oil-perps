use anchor_lang::prelude::*;
use crate::errors::PerpsError;
use crate::state::{Market, ProtocolConfig};

/// Pause a market (admin or guardian). Only stops new positions; closes,
/// liquidations and funding continue. Unpausing is admin-only, via
/// update_market_config.
#[derive(Accounts)]
pub struct PauseMarket<'info> {
    pub signer: Signer<'info>,

    #[account(
        seeds = [ProtocolConfig::SEED],
        bump = config.bump,
        constraint = config.can_pause(&signer.key()) @ PerpsError::Unauthorized
    )]
    pub config: Account<'info, ProtocolConfig>,

    #[account(
        mut,
        seeds = [b"market", market.collateral_mint.as_ref(), market.commodity_seed()],
        bump = market.bump,
    )]
    pub market: Account<'info, Market>,
}

pub fn handler(ctx: Context<PauseMarket>) -> Result<()> {
    ctx.accounts.market.is_paused = true;
    msg!("Market paused by {}", ctx.accounts.signer.key());
    Ok(())
}
