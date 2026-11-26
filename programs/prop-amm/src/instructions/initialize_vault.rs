use anchor_lang::prelude::*;
use anchor_spl::token::{Mint, Token, TokenAccount};
use crate::state::{LpVault, ProtocolConfig};
use crate::errors::PropAmmError;
use crate::oracle::{load_update, pyth_push};

#[derive(AnchorSerialize, AnchorDeserialize)]
pub struct InitializeVaultParams {
    pub max_exposure: u64,
    pub max_utilization: u32,
    pub max_position_size: u64,
    pub base_spread: u32,
    pub max_skew_spread: u32,
    pub trading_fee: u32,
    pub lp_fee_share: u32,
    pub withdrawal_delay: i64,
    /// Pyth feed id the `pyth_price_feed` account must carry (non-zero;
    /// docs/lp-pool.md D47)
    pub pyth_feed_id: [u8; 32],
}

#[derive(Accounts)]
pub struct InitializeVault<'info> {
    /// Must be the protocol admin; nobody else can create vaults.
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(
        seeds = [ProtocolConfig::SEED],
        bump = config.bump,
        constraint = config.admin == authority.key() @ PropAmmError::Unauthorized
    )]
    pub config: Account<'info, ProtocolConfig>,

    #[account(
        init,
        payer = authority,
        space = LpVault::LEN,
        seeds = [b"lp_vault", collateral_mint.key().as_ref()],
        bump
    )]
    pub vault: Account<'info, LpVault>,

    #[account(
        init,
        payer = authority,
        token::mint = collateral_mint,
        token::authority = vault,
        seeds = [b"vault_token", vault.key().as_ref()],
        bump
    )]
    pub vault_token_account: Account<'info, TokenAccount>,

    pub collateral_mint: Account<'info, Mint>,

    /// CHECK: Market account from perps-core
    pub market: UncheckedAccount<'info>,

    /// CHECK: Pyth Receiver `PriceUpdateV2` for `params.pyth_feed_id`, owned by
    /// the configured oracle program; checked again in the handler
    #[account(constraint = *pyth_price_feed.owner == config.oracle_program @ PropAmmError::InvalidOracleAccount)]
    pub pyth_price_feed: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,
    pub token_program: Program<'info, Token>,
    pub rent: Sysvar<'info, Rent>,
}

pub fn handler(ctx: Context<InitializeVault>, params: InitializeVaultParams) -> Result<()> {
    require!(params.max_exposure > 0, PropAmmError::InvalidParameters);
    require!(params.max_utilization > 0 && params.max_utilization <= 10000, PropAmmError::InvalidParameters);
    require!(params.lp_fee_share <= 10000, PropAmmError::InvalidParameters);
    require!(params.pyth_feed_id != [0u8; 32], PropAmmError::InvalidParameters);
    let oracle_program = ctx.accounts.config.oracle_program;
    load_update(&ctx.accounts.pyth_price_feed, &oracle_program, &params.pyth_feed_id)?;
    // Where the oracle program is a Pyth Receiver: the push oracle's shard-0
    // account for the feed id, not an account anyone can post into (D52)
    require!(
        pyth_push::is_pinnable_feed(ctx.accounts.pyth_price_feed.key, &oracle_program, &params.pyth_feed_id),
        PropAmmError::InvalidOracleAccount
    );

    let vault = &mut ctx.accounts.vault;

    vault.authority = ctx.accounts.authority.key();
    vault.market = ctx.accounts.market.key();
    vault.collateral_mint = ctx.accounts.collateral_mint.key();
    vault.token_account = ctx.accounts.vault_token_account.key();
    vault.pyth_price_feed = ctx.accounts.pyth_price_feed.key();
    vault.pyth_feed_id = params.pyth_feed_id;
    // Snapshot: a later set_oracle_program does not move this vault (D52)
    vault.oracle_program = oracle_program;

    vault.total_assets = 0;
    vault.total_shares = 0;
    vault.pending_fees = 0;

    vault.net_exposure = 0;
    vault.total_long_size = 0;
    vault.total_short_size = 0;

    vault.unrealized_pnl = 0;
    vault.cumulative_fees = 0;
    vault.cumulative_pnl = 0;

    vault.max_exposure = params.max_exposure;
    vault.max_utilization = params.max_utilization;
    vault.max_position_size = params.max_position_size;

    vault.base_spread = params.base_spread;
    vault.max_skew_spread = params.max_skew_spread;
    vault.trading_fee = params.trading_fee;
    vault.lp_fee_share = params.lp_fee_share;
    vault.withdrawal_delay = params.withdrawal_delay;

    vault.bump = ctx.bumps.vault;
    vault.is_active = true;

    msg!("LP Vault initialized: max_exposure={}, base_spread={}bps",
         params.max_exposure, params.base_spread);

    Ok(())
}
