use anchor_lang::prelude::*;
use crate::state::{MmRegistry, MarketMaker, TwoSidedQuote, MmStatus, MAX_QUOTE_TTL_SECS};
use crate::errors::MmRegistryError;

#[derive(AnchorSerialize, AnchorDeserialize)]
pub struct PostQuoteParams {
    pub bid_price: u64,
    pub bid_size: u64,
    pub ask_price: u64,
    pub ask_size: u64,
    pub min_fill_size: u64,
    pub expires_in: i64,  // Seconds until expiry, 1..=MAX_QUOTE_TTL_SECS
    pub leverage: u32,    // MM's leverage for fills (3 decimals); perps-core caps it
}

#[derive(Accounts)]
pub struct PostQuote<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,

    #[account(
        mut,
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
        init,
        payer = owner,
        space = TwoSidedQuote::LEN,
        seeds = [b"quote", market_maker.key().as_ref(), &market_maker.quotes_posted.to_le_bytes()],
        bump
    )]
    pub quote: Account<'info, TwoSidedQuote>,

    pub system_program: Program<'info, System>,
}

pub fn handler(ctx: Context<PostQuote>, params: PostQuoteParams) -> Result<()> {
    let registry = &ctx.accounts.registry;
    let market_maker = &ctx.accounts.market_maker;

    // Validate quote parameters
    require!(params.bid_price > 0, MmRegistryError::InvalidPrice);
    require!(params.ask_price > params.bid_price, MmRegistryError::InvalidPrice);
    // Each bound reports its own error (they used to be keyed by side)
    for size in [params.bid_size, params.ask_size] {
        require!(size >= registry.min_quote_size, MmRegistryError::QuoteSizeTooSmall);
        require!(size <= registry.max_quote_size, MmRegistryError::QuoteSizeTooLarge);
    }

    // Check spread
    let spread = (params.ask_price - params.bid_price) as u128 * 10000 / params.bid_price as u128;
    require!(spread <= registry.max_spread as u128, MmRegistryError::SpreadTooWide);

    // Check max quotes
    require!(
        market_maker.active_quotes < market_maker.max_quotes,
        MmRegistryError::MaxQuotesReached
    );

    // Nothing is locked: each fill margins from the MM's perps-core account
    // and approval, checked atomically at fill time
    require!(params.leverage > 0, MmRegistryError::InvalidQuoteParams);
    require!(
        params.expires_in > 0 && params.expires_in <= MAX_QUOTE_TTL_SECS,
        MmRegistryError::InvalidQuoteParams
    );

    let current_time = Clock::get()?.unix_timestamp;

    // Initialize quote
    let quote = &mut ctx.accounts.quote;
    quote.market_maker = ctx.accounts.market_maker.key();
    quote.registry = ctx.accounts.registry.key();

    quote.bid_price = params.bid_price;
    quote.bid_size = params.bid_size;
    quote.bid_remaining = params.bid_size;

    quote.ask_price = params.ask_price;
    quote.ask_size = params.ask_size;
    quote.ask_remaining = params.ask_size;

    quote.min_fill_size = params.min_fill_size;
    quote.collateral_locked = 0;

    quote.created_at = current_time;
    quote.expires_at = current_time + params.expires_in;
    quote.updated_at = current_time;

    quote.is_active = true;
    quote.bump = ctx.bumps.quote;
    quote.leverage = params.leverage;

    // Update market maker
    let market_maker = &mut ctx.accounts.market_maker;
    market_maker.active_quotes += 1;
    market_maker.quotes_posted += 1;
    market_maker.last_active_at = current_time;

    // Update registry
    let registry = &mut ctx.accounts.registry;
    registry.active_quotes += 1;

    msg!(
        "Quote posted: bid={}@{}, ask={}@{}, spread={}bps",
        params.bid_size,
        params.bid_price,
        params.ask_size,
        params.ask_price,
        spread
    );

    Ok(())
}
