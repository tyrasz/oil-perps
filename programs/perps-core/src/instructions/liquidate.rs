use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Transfer};
use pyth_sdk_solana::load_price_feed_from_account_info;
use crate::state::{Market, Position, Vault, UserAccount, Side, PositionStatus};
use crate::errors::PerpsError;

#[derive(Accounts)]
pub struct Liquidate<'info> {
    #[account(mut)]
    pub liquidator: Signer<'info>,

    /// CHECK: Position owner, doesn't need to sign
    pub position_owner: AccountInfo<'info>,

    #[account(
        mut,
        seeds = [b"user", position_owner.key().as_ref()],
        bump = user_account.bump
    )]
    pub user_account: Account<'info, UserAccount>,

    #[account(
        mut,
        seeds = [b"market", market.collateral_mint.as_ref()],
        bump = market.bump
    )]
    pub market: Account<'info, Market>,

    #[account(
        mut,
        constraint = position.owner == position_owner.key(),
        constraint = position.market == market.key(),
        constraint = position.status == PositionStatus::Open @ PerpsError::PositionAlreadyClosed
    )]
    pub position: Account<'info, Position>,

    #[account(
        mut,
        seeds = [b"vault", market.key().as_ref()],
        bump = vault.bump
    )]
    pub vault: Account<'info, Vault>,

    #[account(
        mut,
        seeds = [b"vault_token", market.key().as_ref()],
        bump
    )]
    pub vault_token_account: Account<'info, TokenAccount>,

    #[account(
        mut,
        constraint = liquidator_token_account.owner == liquidator.key(),
        constraint = liquidator_token_account.mint == market.collateral_mint
    )]
    pub liquidator_token_account: Account<'info, TokenAccount>,

    /// CHECK: Pyth price feed
    #[account(constraint = pyth_price_feed.key() == market.pyth_price_feed)]
    pub pyth_price_feed: AccountInfo<'info>,

    pub token_program: Program<'info, Token>,
}

pub fn handler(ctx: Context<Liquidate>) -> Result<()> {
    let market = &mut ctx.accounts.market;
    let position = &mut ctx.accounts.position;
    let user_account = &mut ctx.accounts.user_account;

    // Get oracle price
    let price_feed = load_price_feed_from_account_info(&ctx.accounts.pyth_price_feed)
        .map_err(|_| PerpsError::InvalidOraclePrice)?;

    let current_time = Clock::get()?.unix_timestamp;
    let price_data = price_feed
        .get_price_no_older_than(current_time, 60)
        .ok_or(PerpsError::StaleOraclePrice)?;

    require!(price_data.price > 0, PerpsError::InvalidOraclePrice);

    let oracle_price = (price_data.price as u64)
        .checked_mul(10u64.pow((6 + price_data.expo.abs() as u32) as u32))
        .and_then(|p| p.checked_div(10u64.pow(price_data.expo.abs() as u32)))
        .ok_or(PerpsError::MathOverflow)? as u64;

    // Check if position is liquidatable
    require!(
        position.is_liquidatable(oracle_price, market.maintenance_margin_ratio),
        PerpsError::NotLiquidatable
    );

    // Calculate liquidation amounts
    let pnl = position.unrealized_pnl(oracle_price);
    let remaining_collateral = (position.collateral as i64 + pnl).max(0) as u64;

    // Liquidation fee goes to liquidator
    let liquidation_reward = (remaining_collateral as u128 * market.liquidation_fee as u128 / 10000) as u64;

    // Remainder goes to insurance fund
    let to_insurance = remaining_collateral.saturating_sub(liquidation_reward);

    // Update market OI
    match position.side {
        Side::Long => {
            market.long_open_interest = market.long_open_interest
                .saturating_sub(position.size);
        }
        Side::Short => {
            market.short_open_interest = market.short_open_interest
                .saturating_sub(position.size);
        }
    }

    // Add to insurance fund
    market.insurance_fund = market.insurance_fund.saturating_add(to_insurance);

    // Mark position as liquidated
    position.status = PositionStatus::Liquidated;
    position.realized_pnl = pnl;
    position.last_updated_at = current_time;

    // Update user stats
    user_account.realized_pnl = user_account.realized_pnl.saturating_add(pnl);

    // Pay liquidator
    if liquidation_reward > 0 {
        let market_key = market.key();
        let vault_bump = ctx.accounts.vault.bump;
        let vault_seeds = &[
            b"vault".as_ref(),
            market_key.as_ref(),
            &[vault_bump],
        ];
        let signer = &[&vault_seeds[..]];

        let cpi_accounts = Transfer {
            from: ctx.accounts.vault_token_account.to_account_info(),
            to: ctx.accounts.liquidator_token_account.to_account_info(),
            authority: ctx.accounts.vault.to_account_info(),
        };
        let cpi_ctx = CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            cpi_accounts,
            signer,
        );
        token::transfer(cpi_ctx, liquidation_reward)?;
    }

    msg!(
        "Position liquidated: owner={}, size={}, reward={}, insurance={}",
        position.owner,
        position.size,
        liquidation_reward,
        to_insurance
    );

    Ok(())
}
