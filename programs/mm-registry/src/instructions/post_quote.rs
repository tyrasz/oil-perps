use anchor_lang::prelude::*;
use crate::state::{MmRegistry, MarketMaker, TwoSidedQuote, MmStatus};
use crate::errors::MmRegistryError;

#[derive(AnchorSerialize, AnchorDeserialize)]
pub struct PostQuoteParams {
    pub bid_price: u64,
    pub bid_size: u64,
    pub ask_price: u64,
    pub ask_size: u64,
    pub min_fill_size: u64,
    pub expires_in: i64,  // Seconds until expiry (0 = no expiry)
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
        seeds = [b"quote", market_maker.key().as_ref(), &market_maker.active_quotes.to_le_bytes()],
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
    require!(
        params.bid_size >= registry.min_quote_size && params.bid_size <= registry.max_quote_size,
        MmRegistryError::QuoteSizeTooSmall
    );
    require!(
        params.ask_size >= registry.min_quote_size && params.ask_size <= registry.max_quote_size,
        MmRegistryError::QuoteSizeTooLarge
    );

    // Check spread
    let spread = ((params.ask_price - params.bid_price) as u128 * 10000 / params.bid_price as u128) as u32;
    require!(spread <= registry.max_spread, MmRegistryError::SpreadTooWide);

    // Check max quotes
    require!(
        market_maker.active_quotes < market_maker.max_quotes,
        MmRegistryError::MaxQuotesReached
    );

    // Calculate collateral requirement (10% of max potential exposure)
    let max_notional = std::cmp::max(
        (params.bid_size as u128 * params.bid_price as u128 / 1_000_000) as u64,
        (params.ask_size as u128 * params.ask_price as u128 / 1_000_000) as u64
    );
    let collateral_required = max_notional / 10;

    require!(
        market_maker.has_available_collateral(collateral_required),
        MmRegistryError::InsufficientCollateral
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
    quote.collateral_locked = collateral_required;

    quote.created_at = current_time;
    quote.expires_at = if params.expires_in > 0 {
        current_time + params.expires_in
    } else {
        0
    };
    quote.updated_at = current_time;

    quote.is_active = true;
    quote.bump = *ctx.bumps.get("quote").unwrap();

    // Update market maker
    let market_maker = &mut ctx.accounts.market_maker;
    market_maker.lock_collateral(collateral_required);
    market_maker.active_quotes += 1;
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
