use anchor_lang::prelude::*;
use crate::state::{MmRegistry, MarketMaker, TwoSidedQuote, MmStatus, MAX_QUOTE_TTL_SECS};
use crate::errors::MmRegistryError;

#[derive(AnchorSerialize, AnchorDeserialize)]
pub struct UpdateQuoteParams {
    pub bid_price: Option<u64>,
    pub bid_size: Option<u64>,
    pub ask_price: Option<u64>,
    pub ask_size: Option<u64>,
    pub leverage: Option<u32>,   // > 0
    pub expires_in: Option<i64>, // 1..=MAX_QUOTE_TTL_SECS from now
}

#[derive(Accounts)]
pub struct UpdateQuote<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,

    #[account(
        seeds = [b"mm_registry", registry.market.as_ref()],
        bump = registry.bump,
        constraint = registry.is_trading_enabled @ MmRegistryError::TradingDisabled
    )]
    pub registry: Account<'info, MmRegistry>,

    #[account(
        mut,
        seeds = [b"market_maker", registry.key().as_ref(), owner.key().as_ref()],
        bump = market_maker.bump,
        constraint = market_maker.owner == owner.key() @ MmRegistryError::Unauthorized,
        constraint = market_maker.status == MmStatus::Active @ MmRegistryError::MarketMakerNotActive
    )]
    pub market_maker: Account<'info, MarketMaker>,

    #[account(
        mut,
        constraint = quote.market_maker == market_maker.key() @ MmRegistryError::Unauthorized,
        constraint = quote.is_active @ MmRegistryError::QuoteNotActive
    )]
    pub quote: Account<'info, TwoSidedQuote>,
}

pub fn handler(ctx: Context<UpdateQuote>, params: UpdateQuoteParams) -> Result<()> {
    let registry = &ctx.accounts.registry;
    let quote = &mut ctx.accounts.quote;
    let current_time = Clock::get()?.unix_timestamp;

    // Update prices if provided
    let new_bid_price = params.bid_price.unwrap_or(quote.bid_price);
    let new_ask_price = params.ask_price.unwrap_or(quote.ask_price);

    require!(new_bid_price > 0, MmRegistryError::InvalidPrice);
    require!(new_ask_price > new_bid_price, MmRegistryError::InvalidPrice);

    // Check spread
    // u128 compare: casting to u32 let a 2^32 bps spread wrap to 0 and pass
    let spread = (new_ask_price - new_bid_price) as u128 * 10000 / new_bid_price as u128;
    require!(spread <= registry.max_spread as u128, MmRegistryError::SpreadTooWide);

    // Update sizes if provided (can only increase, not decrease below remaining)
    let new_bid_size = params.bid_size.unwrap_or(quote.bid_size);
    let new_ask_size = params.ask_size.unwrap_or(quote.ask_size);

    require!(
        new_bid_size >= registry.min_quote_size && new_bid_size <= registry.max_quote_size,
        MmRegistryError::QuoteSizeTooSmall
    );
    require!(
        new_ask_size >= registry.min_quote_size && new_ask_size <= registry.max_quote_size,
        MmRegistryError::QuoteSizeTooLarge
    );

    // Calculate new remaining based on size changes
    let bid_filled = quote.bid_size - quote.bid_remaining;
    let ask_filled = quote.ask_size - quote.ask_remaining;

    let new_bid_remaining = new_bid_size.saturating_sub(bid_filled);
    let new_ask_remaining = new_ask_size.saturating_sub(ask_filled);

    if let Some(leverage) = params.leverage {
        require!(leverage > 0, MmRegistryError::InvalidQuoteParams);
        quote.leverage = leverage;
    }
    if let Some(expires_in) = params.expires_in {
        require!(expires_in > 0 && expires_in <= MAX_QUOTE_TTL_SECS, MmRegistryError::InvalidQuoteParams);
        quote.expires_at = current_time + expires_in;
    }

    // Quotes lock nothing any more; this releases a legacy quote's lock, and
    // with a leverage and expiry set above makes it fillable
    let market_maker = &mut ctx.accounts.market_maker;
    market_maker.unlock_collateral(quote.collateral_locked);

    // Update quote
    quote.bid_price = new_bid_price;
    quote.bid_size = new_bid_size;
    quote.bid_remaining = new_bid_remaining;

    quote.ask_price = new_ask_price;
    quote.ask_size = new_ask_size;
    quote.ask_remaining = new_ask_remaining;

    quote.collateral_locked = 0;
    quote.updated_at = current_time;

    // Update MM timestamp
    market_maker.last_active_at = current_time;

    msg!(
        "Quote updated: bid={}@{}, ask={}@{}, spread={}bps",
        new_bid_remaining,
        new_bid_price,
        new_ask_remaining,
        new_ask_price,
        spread
    );

    Ok(())
}
