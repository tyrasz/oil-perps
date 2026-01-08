use anchor_lang::prelude::*;
use anchor_spl::token::{Mint, Token, TokenAccount};
use crate::state::{Market, Vault};
use crate::errors::PerpsError;

#[derive(AnchorSerialize, AnchorDeserialize)]
pub struct InitializeMarketParams {
    pub commodity: String,              // Commodity identifier (e.g., "OIL", "GOLD")
    pub max_leverage: u32,
    pub maintenance_margin_ratio: u32,
    pub initial_margin_ratio: u32,
    pub taker_fee: u32,
    pub maker_fee: u32,
    pub liquidation_fee: u32,
    pub max_open_interest: u64,
    pub funding_interval: i64,
}

#[derive(Accounts)]
pub struct InitializeMarket<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(
        init,
        payer = authority,
        space = Market::LEN,
        seeds = [b"market", collateral_mint.key().as_ref()],
        bump
    )]
    pub market: Account<'info, Market>,

    #[account(
        init,
        payer = authority,
        space = Vault::LEN,
        seeds = [b"vault", market.key().as_ref()],
        bump
    )]
    pub vault: Account<'info, Vault>,

    #[account(
        init,
        payer = authority,
        token::mint = collateral_mint,
        token::authority = vault,
        seeds = [b"vault_token", market.key().as_ref()],
        bump
    )]
    pub vault_token_account: Account<'info, TokenAccount>,

    pub collateral_mint: Account<'info, Mint>,

    /// CHECK: Pyth price feed account, validated by Pyth SDK
    pub pyth_price_feed: AccountInfo<'info>,

    pub system_program: Program<'info, System>,
    pub token_program: Program<'info, Token>,
    pub rent: Sysvar<'info, Rent>,
}

pub fn handler(ctx: Context<InitializeMarket>, params: InitializeMarketParams) -> Result<()> {
    require!(!params.commodity.is_empty() && params.commodity.len() <= 8, PerpsError::InvalidMarketConfig);
    require!(params.max_leverage > 0 && params.max_leverage <= 100_000, PerpsError::InvalidMarketConfig);
    require!(params.maintenance_margin_ratio > 0, PerpsError::InvalidMarketConfig);
    require!(params.initial_margin_ratio > params.maintenance_margin_ratio, PerpsError::InvalidMarketConfig);

    let market = &mut ctx.accounts.market;
    market.authority = ctx.accounts.authority.key();
    market.collateral_mint = ctx.accounts.collateral_mint.key();
    market.vault = ctx.accounts.vault.key();
    market.pyth_price_feed = ctx.accounts.pyth_price_feed.key();
    market.commodity = Market::commodity_from_str(&params.commodity);

    market.max_leverage = params.max_leverage;
    market.maintenance_margin_ratio = params.maintenance_margin_ratio;
    market.initial_margin_ratio = params.initial_margin_ratio;
    market.taker_fee = params.taker_fee;
    market.maker_fee = params.maker_fee;
    market.liquidation_fee = params.liquidation_fee;
    market.max_open_interest = params.max_open_interest;
    market.funding_interval = params.funding_interval;

    market.long_open_interest = 0;
    market.short_open_interest = 0;
    market.funding_rate = 0;
    market.last_funding_time = Clock::get()?.unix_timestamp;
    market.insurance_fund = 0;
    market.total_positions = 0;
    market.total_trades = 0;
    market.is_paused = false;
    market.bump = *ctx.bumps.get("market").unwrap();

    let vault = &mut ctx.accounts.vault;
    vault.market = market.key();
    vault.collateral_mint = ctx.accounts.collateral_mint.key();
    vault.token_account = ctx.accounts.vault_token_account.key();
    vault.total_deposits = 0;
    vault.bump = *ctx.bumps.get("vault").unwrap();

    msg!("Market initialized: {} for commodity: {}", market.key(), params.commodity);
    Ok(())
}
