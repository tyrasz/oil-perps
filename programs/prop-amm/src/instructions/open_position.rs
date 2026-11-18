use anchor_lang::prelude::*;
use anchor_spl::token::{Token, TokenAccount};
use pyth_sdk_solana::load_price_feed_from_account_info;
use crate::oracle::{validate_price, PriceRejection};
use crate::state::LpVault;
use crate::errors::PropAmmError;

#[derive(AnchorSerialize, AnchorDeserialize)]
pub struct OpenPositionParams {
    pub size: u64,          // Position size in base units
    pub is_long: bool,
    pub max_price: u64,     // Slippage protection for longs
    pub min_price: u64,     // Slippage protection for shorts
}

#[derive(Accounts)]
pub struct OpenPositionAmm<'info> {
    #[account(mut)]
    pub trader: Signer<'info>,

    // Operator-only until positions are program-owned accounts. The trade
    // params (size, side, entry price) are taken on trust and no collateral
    // is posted, so a public caller could open phantom exposure or close a
    // position that never existed and be paid PnL from the LP vault.
    #[account(
        mut,
        seeds = [b"lp_vault", vault.collateral_mint.as_ref()],
        bump = vault.bump,
        constraint = vault.authority == trader.key() @ PropAmmError::Unauthorized
    )]
    pub vault: Account<'info, LpVault>,

    #[account(
        mut,
        seeds = [b"vault_token", vault.key().as_ref()],
        bump
    )]
    pub vault_token_account: Account<'info, TokenAccount>,

    #[account(
        mut,
        constraint = trader_token_account.owner == trader.key(),
        constraint = trader_token_account.mint == vault.collateral_mint
    )]
    pub trader_token_account: Account<'info, TokenAccount>,

    /// CHECK: Pyth price feed
    #[account(constraint = pyth_price_feed.key() == vault.pyth_price_feed)]
    pub pyth_price_feed: AccountInfo<'info>,

    pub token_program: Program<'info, Token>,
}

#[derive(AnchorSerialize, AnchorDeserialize)]
pub struct OpenPositionResult {
    pub entry_price: u64,
    pub fee: u64,
    pub margin_required: u64,
}

pub fn handler(ctx: Context<OpenPositionAmm>, params: OpenPositionParams) -> Result<OpenPositionResult> {
    let vault = &ctx.accounts.vault;

    // Validate vault is active and can accept trade
    require!(vault.is_active, PropAmmError::VaultNotActive);
    require!(
        vault.can_accept_trade(params.size, params.is_long),
        PropAmmError::ExposureLimitExceeded
    );

    // Get oracle price
    let price_feed = load_price_feed_from_account_info(&ctx.accounts.pyth_price_feed)
        .map_err(|_| PropAmmError::InvalidOraclePrice)?;

    let current_time = Clock::get()?.unix_timestamp;
    let price_data = price_feed
        .get_price_no_older_than(current_time, 60)
        .ok_or(PropAmmError::StaleOraclePrice)?;

    require!(price_data.price > 0, PropAmmError::InvalidOraclePrice);

    // Same price rules as perps-core: 6 decimals, default 1% confidence limit
    let oracle_price = validate_price(price_data.price, price_data.conf, price_data.expo, 0, 0)
        .map_err(|r| match r {
            PriceRejection::ConfidenceTooWide => PropAmmError::OracleConfidenceTooWide,
            _ => PropAmmError::InvalidOraclePrice,
        })?;

    // Calculate entry price with spread
    let entry_price = vault.calculate_entry_price(oracle_price, params.is_long);

    // Slippage protection
    if params.is_long {
        require!(entry_price <= params.max_price, PropAmmError::InvalidParameters);
    } else {
        require!(entry_price >= params.min_price, PropAmmError::InvalidParameters);
    }

    // Calculate notional and fee
    let notional = (params.size as u128 * entry_price as u128 / 1_000_000) as u64;
    let fee = (notional * vault.trading_fee as u64) / 10000;

    // For now, we return the calculated values
    // The actual margin transfer will be handled by perps-core integration
    // This instruction is meant to be called via CPI from perps-core

    // Update vault exposure
    let vault = &mut ctx.accounts.vault;
    vault.add_position(params.size, params.is_long);

    msg!(
        "AMM position opened: {} {} @ {} (oracle: {}), fee: {}",
        if params.is_long { "LONG" } else { "SHORT" },
        params.size,
        entry_price,
        oracle_price,
        fee
    );

    Ok(OpenPositionResult {
        entry_price,
        fee,
        margin_required: notional / 10, // 10x leverage default
    })
}
