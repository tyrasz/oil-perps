use anchor_lang::prelude::*;
use perps_core::instructions::VenueFillParams;
use crate::state::{MmRegistry, MarketMaker, MmStatus, TwoSidedQuote};
use crate::errors::MmRegistryError;

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy)]
pub struct FillQuoteParams {
    pub size: u64,
    pub is_buy: bool,        // true: taker long vs the ask (MM short); false: taker short vs the bid (MM long)
    // Guards the taker against an update_quote landing first: buy needs ask <= limit, sell bid >= limit
    pub limit_price: u64,
    pub taker_leverage: u32, // 3 decimals
    pub allow_partial: bool, // fill what remains instead of failing when size > remaining
    // Seeds the taker's perps-core position; clients pick a random u64
    pub position_nonce: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize)]
pub struct FillQuoteResult {
    pub fill_price: u64,
    pub fill_size: u64,
    pub fee: u64, // Always 0: perps-core charges its taker fee on close
}

/// Fill a quote by opening a taker position and the opposite MM position in
/// perps-core (`venue_fill`), each margined from its owner's own UserAccount
/// and bounded by its owner's VenueApproval. No tokens move here.
#[derive(Accounts)]
pub struct FillQuote<'info> {
    /// Also perps-core's payer: being in a Signer slot of the CPI is what makes
    /// the taker count towards the venue's `min_signers` (Anchor's CPI passes
    /// the UncheckedAccount `taker` slot as a non-signer).
    #[account(mut)]
    pub taker: Signer<'info>,

    #[account(
        mut,
        seeds = [b"mm_registry", registry.market.as_ref()],
        bump = registry.bump,
        constraint = registry.is_trading_enabled @ MmRegistryError::TradingDisabled
    )]
    pub registry: Box<Account<'info, MmRegistry>>,

    #[account(
        mut,
        seeds = [b"market_maker", registry.key().as_ref(), market_maker.owner.as_ref()],
        bump = market_maker.bump,
        constraint = market_maker.status == MmStatus::Active @ MmRegistryError::MarketMakerNotActive
    )]
    pub market_maker: Box<Account<'info, MarketMaker>>,

    #[account(
        mut,
        constraint = quote.market_maker == market_maker.key() @ MmRegistryError::Unauthorized,
        constraint = quote.registry == registry.key() @ MmRegistryError::Unauthorized,
        constraint = quote.is_active @ MmRegistryError::QuoteNotActive
    )]
    pub quote: Box<Account<'info, TwoSidedQuote>>,

    /// CHECK: the MM's wallet, which owns the maker position
    #[account(address = market_maker.owner @ MmRegistryError::Unauthorized)]
    pub mm_owner: UncheckedAccount<'info>,

    /// CHECK: signs the venue_fill CPI; perps-core checks it is this venue's authority
    #[account(seeds = [b"perps_venue", registry.market.as_ref()], bump)]
    pub venue_authority: UncheckedAccount<'info>,

    /// CHECK: perps-core validates
    pub perps_config: UncheckedAccount<'info>,

    /// CHECK: perps-core validates
    #[account(mut)]
    pub perps_venue: UncheckedAccount<'info>,

    /// CHECK: perps-core validates
    pub venue_program_data: UncheckedAccount<'info>,

    /// CHECK: must be this registry's market; perps-core validates the rest.
    /// Never a typed Account here: the CPI mutates it.
    #[account(mut, address = registry.market @ MmRegistryError::WrongMarket)]
    pub market: UncheckedAccount<'info>,

    /// CHECK: perps-core validates
    pub pyth_price_feed: UncheckedAccount<'info>,

    /// CHECK: perps-core validates
    #[account(mut)]
    pub taker_user: UncheckedAccount<'info>,

    /// CHECK: perps-core validates
    #[account(mut)]
    pub taker_approval: UncheckedAccount<'info>,

    /// CHECK: perps-core inits
    #[account(mut)]
    pub taker_position: UncheckedAccount<'info>,

    /// CHECK: perps-core validates
    #[account(mut)]
    pub mm_user: UncheckedAccount<'info>,

    /// CHECK: perps-core validates
    #[account(mut)]
    pub mm_approval: UncheckedAccount<'info>,

    /// CHECK: perps-core inits
    #[account(mut)]
    pub mm_position: UncheckedAccount<'info>,

    pub perps_core_program: Program<'info, perps_core::program::PerpsCore>,
    pub system_program: Program<'info, System>,
}

pub fn handler(ctx: Context<FillQuote>, params: FillQuoteParams) -> Result<FillQuoteResult> {
    let bump = *ctx.bumps.get("venue_authority").unwrap();
    let current_time = Clock::get()?.unix_timestamp;
    let quote = &ctx.accounts.quote;

    require!(quote.is_valid(current_time), MmRegistryError::QuoteExpired);
    // Legacy quotes (no leverage, no expiry) must be migrated with update_quote
    require!(quote.leverage > 0 && quote.expires_at > 0, MmRegistryError::QuoteNotFillable);
    require!(ctx.accounts.taker.key() != ctx.accounts.market_maker.owner, MmRegistryError::SelfTrade);
    require!(params.limit_price > 0, MmRegistryError::InvalidPrice);

    let (fill_price, remaining) = if params.is_buy {
        (quote.ask_price, quote.ask_remaining)
    } else {
        (quote.bid_price, quote.bid_remaining)
    };
    require!(remaining > 0, MmRegistryError::FillSizeExceedsRemaining);
    let within_limit = if params.is_buy { fill_price <= params.limit_price } else { fill_price >= params.limit_price };
    require!(within_limit, MmRegistryError::PriceWorseThanLimit);

    let fill_size = if params.size <= remaining {
        params.size
    } else if params.allow_partial {
        remaining
    } else {
        return Err(MmRegistryError::FillSizeExceedsRemaining.into());
    };
    // The last tail may be below min_fill_size, or it could never be filled
    require!(
        fill_size > 0 && (fill_size >= quote.min_fill_size || fill_size == remaining),
        MmRegistryError::FillSizeTooSmall
    );
    let notional = u64::try_from(fill_size as u128 * fill_price as u128 / 1_000_000)
        .map_err(|_| MmRegistryError::MathOverflow)?;

    cpi_venue_fill(&ctx.accounts, bump, VenueFillParams {
        size: fill_size,
        price: fill_price,
        taker_side: if params.is_buy { 0 } else { 1 },
        taker_leverage: params.taker_leverage,
        maker_leverage: quote.leverage,
        position_nonce: params.position_nonce,
    })?;

    let quote = &mut ctx.accounts.quote;
    if params.is_buy {
        quote.ask_remaining -= fill_size;
    } else {
        quote.bid_remaining -= fill_size;
    }
    quote.updated_at = current_time;
    let exhausted = quote.bid_remaining == 0 && quote.ask_remaining == 0;

    // Stats only: saturate rather than fail a fill that perps-core accepted
    let market_maker = &mut ctx.accounts.market_maker;
    market_maker.total_volume = market_maker.total_volume.saturating_add(notional);
    market_maker.total_fills = market_maker.total_fills.saturating_add(1);
    market_maker.last_active_at = current_time;
    let registry = &mut ctx.accounts.registry;
    registry.total_volume = registry.total_volume.saturating_add(notional);

    if exhausted {
        ctx.accounts.quote.is_active = false;
        let market_maker = &mut ctx.accounts.market_maker;
        market_maker.active_quotes = market_maker.active_quotes.saturating_sub(1);
        let registry = &mut ctx.accounts.registry;
        registry.active_quotes = registry.active_quotes.saturating_sub(1);

        // Close it: cancel_quote only takes active quotes, so a filled one
        // would otherwise live forever, and after deregister/re-register
        // (quotes_posted restarts at 0) its PDA would block post_quote.
        // Rent goes to the market_maker PDA and returns to the owner on
        // deregister.
        let market_maker_info = ctx.accounts.market_maker.to_account_info();
        ctx.accounts.quote.close(market_maker_info)?;
    }

    msg!(
        "Quote filled: {} {} @ {}",
        if params.is_buy { "BUY" } else { "SELL" },
        fill_size,
        fill_price
    );

    Ok(FillQuoteResult { fill_price, fill_size, fee: 0 })
}

/// perps-core `venue_fill`, signed by this program's venue authority for the
/// registry's market. The taker fills both the `payer` and `taker` slots; see
/// `FillQuote::taker`.
#[inline(never)]
fn cpi_venue_fill<'info>(a: &FillQuote<'info>, bump: u8, params: VenueFillParams) -> Result<()> {
    let market = a.registry.market;
    let bump = [bump];
    let seeds: &[&[u8]] = &[b"perps_venue", market.as_ref(), &bump];
    let accounts = perps_core::cpi::accounts::VenueFill {
        venue_authority: a.venue_authority.to_account_info(),
        payer: a.taker.to_account_info(),
        config: a.perps_config.to_account_info(),
        market: a.market.to_account_info(),
        venue: a.perps_venue.to_account_info(),
        venue_program_data: a.venue_program_data.to_account_info(),
        pyth_price_feed: a.pyth_price_feed.to_account_info(),
        taker: a.taker.to_account_info(),
        taker_user: a.taker_user.to_account_info(),
        taker_approval: a.taker_approval.to_account_info(),
        taker_position: a.taker_position.to_account_info(),
        maker: a.mm_owner.to_account_info(),
        maker_user: a.mm_user.to_account_info(),
        maker_approval: a.mm_approval.to_account_info(),
        maker_position: a.mm_position.to_account_info(),
        system_program: a.system_program.to_account_info(),
    };
    perps_core::cpi::venue_fill(
        CpiContext::new_with_signer(a.perps_core_program.to_account_info(), accounts, &[seeds]),
        params,
    )
}
