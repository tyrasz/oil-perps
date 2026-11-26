//! Oracle feed rotation (docs/lp-pool.md D47): propose / apply / cancel a new
//! pinned `PriceUpdateV2` account and feed id for a market, in the style of
//! the pool-params timelock (pool_admin.rs).
//!
//! The delay is the pool-params timelock for the pool's current params,
//! `params_timelock_secs(d, d, W, W)` with `d = withdrawal_delay_secs` and
//! `W = weekend_cap_window_secs` (production: 4 d + 1 d + 5 d = 10 d; never
//! under 1 d), so an LP who distrusts the new feed has the time to request a
//! withdrawal and try to claim it before the feed takes effect. It is not a
//! guaranteed exit (a claim can still be refused by the pool's caps and
//! gates), and it does not bound the admin: `settle_market` sets a final
//! price at once (docs/lp-pool.md D52). Before the seed (`lp_shares == 0`)
//! nobody is exposed and the change applies at once. A settled market's
//! feed is final: propose and apply are both `MarketSettled`.
//!
//! Where the oracle program is one of Pyth's Receivers, the account must be
//! the push oracle's shard-0 PDA for the feed id (`pyth_push`, D52).

use anchor_lang::prelude::*;
use crate::errors::PerpsError;
use crate::events::{OracleFeedApplied, OracleFeedCancelled, OracleFeedProposed};
use crate::oracle::load_pinnable_update;
use crate::pool_math;
use crate::state::{LpPool, Market, ProtocolConfig};

#[derive(Accounts)]
pub struct ProposeOracleFeed<'info> {
    pub admin: Signer<'info>,

    #[account(
        seeds = [ProtocolConfig::SEED],
        bump = config.bump,
        constraint = config.admin == admin.key() @ PerpsError::Unauthorized
    )]
    pub config: Box<Account<'info, ProtocolConfig>>,

    #[account(
        mut,
        seeds = [b"market", market.collateral_mint.as_ref(), market.commodity_seed()],
        bump = market.bump
    )]
    pub market: Box<Account<'info, Market>>,

    #[account(seeds = [LpPool::SEED, market.key().as_ref()], bump = lp_pool.bump)]
    pub lp_pool: Box<Account<'info, LpPool>>,

    /// CHECK: the proposed Pyth Receiver `PriceUpdateV2` account, owned by
    /// the configured oracle program; parsed in the handler
    #[account(constraint = *new_price_feed.owner == config.oracle_program @ PerpsError::InvalidOracleAccount)]
    pub new_price_feed: UncheckedAccount<'info>,
}

/// Admin. Validates the new feed as `initialize_market` does (Full update
/// for `feed_id`, no age check). A new proposal replaces a pending one and
/// restarts the timelock.
pub fn propose_oracle_feed(ctx: Context<ProposeOracleFeed>, feed_id: [u8; 32]) -> Result<()> {
    let market_key = ctx.accounts.market.key();
    let market = &mut ctx.accounts.market;
    require!(!market.is_settled(), PerpsError::MarketSettled);
    require!(feed_id != [0u8; 32], PerpsError::InvalidMarketConfig);
    let oracle_program = ctx.accounts.config.oracle_program;
    load_pinnable_update(&ctx.accounts.new_price_feed, &oracle_program, &feed_id)?;
    let price_feed = ctx.accounts.new_price_feed.key();
    let now = Clock::get()?.unix_timestamp;
    let pool = &ctx.accounts.lp_pool;

    if pool.lp_shares == 0 {
        market.pyth_price_feed = price_feed;
        market.pyth_feed_id = feed_id;
        market.oracle_program = oracle_program;
        clear_pending(market);
        emit_event!(OracleFeedProposed { market: market_key, price_feed, feed_id, effective_at: now });
        emit_event!(OracleFeedApplied { market: market_key, price_feed, feed_id, effective_at: now });
        msg!("Oracle feed applied (no LPs yet): {}", price_feed);
        return Ok(());
    }

    let p = &pool.params;
    let t = pool_math::params_timelock_secs(
        p.withdrawal_delay_secs,
        p.withdrawal_delay_secs,
        p.weekend_cap_window_secs,
        p.weekend_cap_window_secs,
    );
    let effective_at = now.checked_add(t).ok_or(PerpsError::MathOverflow)?;
    market.pending_price_feed = price_feed;
    market.pending_feed_id = feed_id;
    market.pending_oracle_at = effective_at;
    emit_event!(OracleFeedProposed { market: market_key, price_feed, feed_id, effective_at });
    msg!("Oracle feed proposed: {}, effective at {}", price_feed, effective_at);
    Ok(())
}

#[derive(Accounts)]
pub struct ApplyOracleFeed<'info> {
    #[account(
        mut,
        seeds = [b"market", market.collateral_mint.as_ref(), market.commodity_seed()],
        bump = market.bump
    )]
    pub market: Box<Account<'info, Market>>,

    #[account(seeds = [ProtocolConfig::SEED], bump = config.bump)]
    pub config: Box<Account<'info, ProtocolConfig>>,

    /// CHECK: must be the pending feed; re-validated in the handler
    #[account(constraint = new_price_feed.key() == market.pending_price_feed @ PerpsError::InvalidOracleAccount)]
    pub new_price_feed: UncheckedAccount<'info>,
}

/// Permissionless once the timelock has passed. Re-validates the feed
/// against the oracle program configured now.
pub fn apply_oracle_feed(ctx: Context<ApplyOracleFeed>) -> Result<()> {
    let market_key = ctx.accounts.market.key();
    let now = Clock::get()?.unix_timestamp;
    let market = &mut ctx.accounts.market;
    // A settled market's feed is final (propose refuses one too); a rotation
    // left pending by settle_market can still be cancelled (D52)
    require!(!market.is_settled(), PerpsError::MarketSettled);
    require!(market.pending_oracle_at != 0 && now >= market.pending_oracle_at, PerpsError::OracleChangeTimelocked);
    let oracle_program = ctx.accounts.config.oracle_program;
    let feed_id = market.pending_feed_id;
    load_pinnable_update(&ctx.accounts.new_price_feed, &oracle_program, &feed_id)?;

    let (price_feed, effective_at) = (market.pending_price_feed, market.pending_oracle_at);
    market.pyth_price_feed = price_feed;
    market.pyth_feed_id = feed_id;
    market.oracle_program = oracle_program;
    clear_pending(market);
    emit_event!(OracleFeedApplied { market: market_key, price_feed, feed_id, effective_at });
    msg!("Oracle feed applied: {}", price_feed);
    Ok(())
}

#[derive(Accounts)]
pub struct CancelOracleFeed<'info> {
    pub admin: Signer<'info>,

    #[account(
        seeds = [ProtocolConfig::SEED],
        bump = config.bump,
        constraint = config.admin == admin.key() @ PerpsError::Unauthorized
    )]
    pub config: Box<Account<'info, ProtocolConfig>>,

    #[account(
        mut,
        seeds = [b"market", market.collateral_mint.as_ref(), market.commodity_seed()],
        bump = market.bump
    )]
    pub market: Box<Account<'info, Market>>,
}

pub fn cancel_oracle_feed(ctx: Context<CancelOracleFeed>) -> Result<()> {
    let market_key = ctx.accounts.market.key();
    let market = &mut ctx.accounts.market;
    require!(market.pending_oracle_at != 0, PerpsError::NoPendingOracleChange);
    let (price_feed, feed_id, effective_at) = (market.pending_price_feed, market.pending_feed_id, market.pending_oracle_at);
    clear_pending(market);
    emit_event!(OracleFeedCancelled { market: market_key, price_feed, feed_id, effective_at });
    msg!("Oracle feed proposal cancelled");
    Ok(())
}

fn clear_pending(market: &mut Market) {
    market.pending_price_feed = Pubkey::default();
    market.pending_feed_id = [0u8; 32];
    market.pending_oracle_at = 0;
}
