use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Transfer};
use crate::state::{MmRegistry, MarketMaker, TwoSidedQuote};
use crate::errors::MmRegistryError;

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy)]
pub struct FillQuoteParams {
    pub size: u64,
    pub is_buy: bool,  // true = taker is buying (fills MM's ask), false = taker is selling (fills MM's bid)
}

#[derive(AnchorSerialize, AnchorDeserialize)]
pub struct FillQuoteResult {
    pub fill_price: u64,
    pub fill_size: u64,
    pub fee: u64,
}

#[derive(Accounts)]
pub struct FillQuote<'info> {
    /// The taker (trader) filling the quote
    #[account(mut)]
    pub taker: Signer<'info>,

    #[account(
        mut,
        seeds = [b"mm_registry", registry.market.as_ref()],
        bump = registry.bump,
        constraint = registry.is_trading_enabled @ MmRegistryError::TradingDisabled
    )]
    pub registry: Account<'info, MmRegistry>,

    #[account(
        mut,
        seeds = [b"market_maker", registry.key().as_ref(), market_maker.owner.as_ref()],
        bump = market_maker.bump
    )]
    pub market_maker: Account<'info, MarketMaker>,

    #[account(
        mut,
        constraint = quote.market_maker == market_maker.key() @ MmRegistryError::Unauthorized,
        constraint = quote.is_active @ MmRegistryError::QuoteNotActive
    )]
    pub quote: Account<'info, TwoSidedQuote>,

    #[account(
        mut,
        seeds = [b"mm_collateral", market_maker.key().as_ref()],
        bump
    )]
    pub mm_collateral_account: Account<'info, TokenAccount>,

    #[account(
        mut,
        constraint = taker_token_account.owner == taker.key(),
        constraint = taker_token_account.mint == registry.collateral_mint
    )]
    pub taker_token_account: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
}

pub fn handler(ctx: Context<FillQuote>, params: FillQuoteParams) -> Result<FillQuoteResult> {
    let quote = &ctx.accounts.quote;
    let registry = &ctx.accounts.registry;
    let current_time = Clock::get()?.unix_timestamp;

    // Validate quote is still valid
    require!(quote.is_valid(current_time), MmRegistryError::QuoteExpired);

    // Determine fill price and validate size
    let (fill_price, max_size) = if params.is_buy {
        // Taker buying = filling MM's ask
        (quote.ask_price, quote.ask_remaining)
    } else {
        // Taker selling = filling MM's bid
        (quote.bid_price, quote.bid_remaining)
    };

    require!(params.size >= quote.min_fill_size, MmRegistryError::FillSizeTooSmall);
    require!(params.size <= max_size, MmRegistryError::FillSizeExceedsRemaining);

    // Calculate notional and fee
    let notional = (params.size as u128 * fill_price as u128 / 1_000_000) as u64;
    let fee = (notional * registry.mm_fee as u64) / 10000;

    // Process the fill based on direction
    if params.is_buy {
        // Taker buys from MM (MM sells/goes short)
        // Taker pays notional + fee, MM receives notional
        let taker_pays = notional + fee;

        // Transfer from taker to MM collateral
        let cpi_accounts = Transfer {
            from: ctx.accounts.taker_token_account.to_account_info(),
            to: ctx.accounts.mm_collateral_account.to_account_info(),
            authority: ctx.accounts.taker.to_account_info(),
        };
        let cpi_ctx = CpiContext::new(
            ctx.accounts.token_program.to_account_info(),
            cpi_accounts
        );
        token::transfer(cpi_ctx, taker_pays)?;

        // Update quote
        let quote = &mut ctx.accounts.quote;
        quote.ask_remaining = quote.ask_remaining.saturating_sub(params.size);
        quote.updated_at = current_time;

        if quote.bid_remaining == 0 && quote.ask_remaining == 0 {
            quote.is_active = false;
        }

        // Update MM inventory (MM is selling/shorting)
        let market_maker = &mut ctx.accounts.market_maker;
        market_maker.update_inventory(params.size as i64, fill_price, false);
        market_maker.collateral_deposited += notional; // MM receives payment
    } else {
        // Taker sells to MM (MM buys/goes long)
        // MM pays notional, taker receives notional - fee
        let taker_receives = notional.saturating_sub(fee);

        // Transfer from MM collateral to taker
        let market_maker_key = ctx.accounts.market_maker.key();
        let seeds = &[
            b"mm_collateral".as_ref(),
            market_maker_key.as_ref(),
            &[*ctx.bumps.get("mm_collateral_account").unwrap()],
        ];
        let signer = &[&seeds[..]];

        let cpi_accounts = Transfer {
            from: ctx.accounts.mm_collateral_account.to_account_info(),
            to: ctx.accounts.taker_token_account.to_account_info(),
            authority: ctx.accounts.mm_collateral_account.to_account_info(),
        };
        let cpi_ctx = CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            cpi_accounts,
            signer
        );
        token::transfer(cpi_ctx, taker_receives)?;

        // Update quote
        let quote = &mut ctx.accounts.quote;
        quote.bid_remaining = quote.bid_remaining.saturating_sub(params.size);
        quote.updated_at = current_time;

        if quote.bid_remaining == 0 && quote.ask_remaining == 0 {
            quote.is_active = false;
        }

        // Update MM inventory (MM is buying/going long)
        let market_maker = &mut ctx.accounts.market_maker;
        market_maker.update_inventory(params.size as i64, fill_price, true);
        market_maker.collateral_deposited = market_maker.collateral_deposited.saturating_sub(notional);
    }

    // Update MM stats
    let market_maker = &mut ctx.accounts.market_maker;
    market_maker.total_volume += notional;
    market_maker.total_fills += 1;
    market_maker.total_fees_paid += fee;
    market_maker.last_active_at = current_time;

    // Recalculate locked collateral based on remaining quote sizes
    let quote = &ctx.accounts.quote;
    let new_max_notional = std::cmp::max(
        (quote.bid_remaining as u128 * quote.bid_price as u128 / 1_000_000) as u64,
        (quote.ask_remaining as u128 * quote.ask_price as u128 / 1_000_000) as u64
    );
    let new_collateral_locked = new_max_notional / 10;
    let collateral_freed = quote.collateral_locked.saturating_sub(new_collateral_locked);

    let market_maker = &mut ctx.accounts.market_maker;
    market_maker.unlock_collateral(collateral_freed);

    // Check if quote is fully filled
    let quote = &mut ctx.accounts.quote;
    if !quote.is_active {
        let market_maker = &mut ctx.accounts.market_maker;
        market_maker.active_quotes = market_maker.active_quotes.saturating_sub(1);

        let registry = &mut ctx.accounts.registry;
        registry.active_quotes = registry.active_quotes.saturating_sub(1);
    }

    // Update registry stats
    let registry = &mut ctx.accounts.registry;
    registry.total_volume += notional;
    registry.total_fees += fee;

    msg!(
        "Quote filled: {} {} @ {}, fee={}",
        if params.is_buy { "BUY" } else { "SELL" },
        params.size,
        fill_price,
        fee
    );

    Ok(FillQuoteResult {
        fill_price,
        fill_size: params.size,
        fee,
    })
}
