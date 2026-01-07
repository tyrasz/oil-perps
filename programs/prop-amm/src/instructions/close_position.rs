use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Transfer};
use pyth_sdk_solana::load_price_feed_from_account_info;
use crate::state::LpVault;
use crate::errors::PropAmmError;

#[derive(AnchorSerialize, AnchorDeserialize)]
pub struct ClosePositionParams {
    pub size: u64,
    pub is_long: bool,
    pub entry_price: u64,      // Original entry price
    pub min_price: u64,        // Slippage protection for longs
    pub max_price: u64,        // Slippage protection for shorts
}

#[derive(Accounts)]
pub struct ClosePositionAmm<'info> {
    #[account(mut)]
    pub trader: Signer<'info>,

    #[account(
        mut,
        seeds = [b"lp_vault", vault.collateral_mint.as_ref()],
        bump = vault.bump
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
pub struct ClosePositionResult {
    pub exit_price: u64,
    pub pnl: i64,
    pub fee: u64,
    pub settlement: u64,
}

pub fn handler(ctx: Context<ClosePositionAmm>, params: ClosePositionParams) -> Result<ClosePositionResult> {
    let vault = &ctx.accounts.vault;

    require!(vault.is_active, PropAmmError::VaultNotActive);

    // Get oracle price
    let price_feed = load_price_feed_from_account_info(&ctx.accounts.pyth_price_feed)
        .map_err(|_| PropAmmError::InvalidOraclePrice)?;

    let current_time = Clock::get()?.unix_timestamp;
    let price_data = price_feed
        .get_price_no_older_than(current_time, 60)
        .ok_or(PropAmmError::StaleOraclePrice)?;

    require!(price_data.price > 0, PropAmmError::InvalidOraclePrice);

    let oracle_price = (price_data.price as u64)
        .checked_mul(10u64.pow((6 + price_data.expo.abs() as u32) as u32))
        .and_then(|p| p.checked_div(10u64.pow(price_data.expo.abs() as u32)))
        .ok_or(PropAmmError::MathOverflow)? as u64;

    // Calculate exit price with spread
    let exit_price = vault.calculate_exit_price(oracle_price, params.is_long);

    // Slippage protection
    if params.is_long {
        require!(exit_price >= params.min_price, PropAmmError::InvalidParameters);
    } else {
        require!(exit_price <= params.max_price, PropAmmError::InvalidParameters);
    }

    // Calculate PnL
    let entry = params.entry_price as i128;
    let exit = exit_price as i128;
    let size = params.size as i128;

    let price_diff = if params.is_long {
        exit - entry
    } else {
        entry - exit
    };

    let pnl = ((size * price_diff) / 1_000_000) as i64;

    // Calculate fee
    let notional = (params.size as u128 * exit_price as u128 / 1_000_000) as u64;
    let fee = (notional * vault.trading_fee as u64) / 10000;

    // Calculate settlement amount (PnL - fee)
    let settlement = if pnl >= 0 {
        (pnl as u64).saturating_sub(fee)
    } else {
        0
    };

    // Update vault
    let vault = &mut ctx.accounts.vault;
    vault.remove_position(params.size, params.is_long);
    vault.settle_pnl(pnl, fee);

    // If trader has profit, pay from vault
    if settlement > 0 {
        let collateral_mint = vault.collateral_mint;
        let vault_bump = vault.bump;
        let vault_seeds = &[
            b"lp_vault".as_ref(),
            collateral_mint.as_ref(),
            &[vault_bump],
        ];
        let signer = &[&vault_seeds[..]];

        let cpi_accounts = Transfer {
            from: ctx.accounts.vault_token_account.to_account_info(),
            to: ctx.accounts.trader_token_account.to_account_info(),
            authority: ctx.accounts.vault.to_account_info(),
        };
        let cpi_ctx = CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            cpi_accounts,
            signer,
        );
        token::transfer(cpi_ctx, settlement)?;
    }

    msg!(
        "AMM position closed: {} {} @ {} (entry: {}), PnL: {}, fee: {}, settlement: {}",
        if params.is_long { "LONG" } else { "SHORT" },
        params.size,
        exit_price,
        params.entry_price,
        pnl,
        fee,
        settlement
    );

    Ok(ClosePositionResult {
        exit_price,
        pnl,
        fee,
        settlement,
    })
}
