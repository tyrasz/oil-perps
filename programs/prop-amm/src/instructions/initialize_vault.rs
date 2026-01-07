use anchor_lang::prelude::*;
use anchor_spl::token::{Mint, Token, TokenAccount};
use crate::state::LpVault;
use crate::errors::PropAmmError;

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
}

#[derive(Accounts)]
pub struct InitializeVault<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

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
    pub market: AccountInfo<'info>,

    /// CHECK: Pyth price feed
    pub pyth_price_feed: AccountInfo<'info>,

    pub system_program: Program<'info, System>,
    pub token_program: Program<'info, Token>,
    pub rent: Sysvar<'info, Rent>,
}

pub fn handler(ctx: Context<InitializeVault>, params: InitializeVaultParams) -> Result<()> {
    require!(params.max_exposure > 0, PropAmmError::InvalidParameters);
    require!(params.max_utilization > 0 && params.max_utilization <= 10000, PropAmmError::InvalidParameters);
    require!(params.lp_fee_share <= 10000, PropAmmError::InvalidParameters);

    let vault = &mut ctx.accounts.vault;

    vault.authority = ctx.accounts.authority.key();
    vault.market = ctx.accounts.market.key();
    vault.collateral_mint = ctx.accounts.collateral_mint.key();
    vault.token_account = ctx.accounts.vault_token_account.key();
    vault.pyth_price_feed = ctx.accounts.pyth_price_feed.key();

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

    vault.bump = *ctx.bumps.get("vault").unwrap();
    vault.is_active = true;

    msg!("LP Vault initialized: max_exposure={}, base_spread={}bps",
         params.max_exposure, params.base_spread);

    Ok(())
}
