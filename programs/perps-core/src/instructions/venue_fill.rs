use anchor_lang::prelude::*;
use crate::errors::PerpsError;
use crate::oracle::read_price_max_age;
use crate::state::{
    venue_margin, within_adverse_bound, within_band, ExecutionSource, Market, Position, ProtocolConfig, Side,
    UserAccount, Venue, VenueApproval, ROLE_MAKER, ROLE_TAKER, SIDE_LONG, SIDE_SHORT,
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug)]
pub struct VenueFillParams {
    pub size: u64,           // Base units, > 0
    pub price: u64,          // 6 dp, > 0
    pub taker_side: u8,      // 0 Long, 1 Short; maker gets the opposite
    pub taker_leverage: u32, // 3 dp
    pub maker_leverage: u32,
    // Seeds the taker position. Chosen by the caller (clients pick a random
    // u64) rather than read from a counter, so no other open, fill or close
    // landing first can invalidate a transaction already built. init rejects
    // a reused nonce.
    pub position_nonce: u64,
}

/// Open a taker position and the opposite maker position, same size and entry
/// price, each margined from its owner's own UserAccount. Only reachable by
/// CPI from a registered venue program signing with `[b"perps_venue", market]`.
///
/// `taker` and `maker` count towards `venue.min_signers` only if the venue's
/// CPI marks them as signers. Anchor's generated CPI marks UncheckedAccount
/// fields as non-signers, so a venue built that way passes a party as a
/// signer only by also putting the same key in a Signer slot (e.g. `payer`).
#[derive(Accounts)]
#[instruction(params: VenueFillParams)]
pub struct VenueFill<'info> {
    pub venue_authority: Signer<'info>,

    #[account(mut)]
    pub payer: Signer<'info>,

    #[account(
        seeds = [ProtocolConfig::SEED],
        bump = config.bump,
        constraint = !config.venue_fills_paused @ PerpsError::VenueFillsPaused
    )]
    pub config: Box<Account<'info, ProtocolConfig>>,

    #[account(
        mut,
        seeds = [b"market", market.collateral_mint.as_ref(), market.commodity_seed()],
        bump = market.bump,
        constraint = !market.is_paused @ PerpsError::MarketPaused
    )]
    pub market: Box<Account<'info, Market>>,

    #[account(
        mut,
        seeds = [Venue::SEED, market.key().as_ref(), venue.program.as_ref()],
        bump = venue.bump,
        constraint = venue.authority == venue_authority.key() @ PerpsError::VenueUnauthorized,
        constraint = venue.enabled @ PerpsError::VenueDisabled
    )]
    pub venue: Box<Account<'info, Venue>>,

    #[account(
        address = venue.program_data @ PerpsError::VenueProgramChanged,
        constraint = venue_program_data.upgrade_authority_address.unwrap_or_default() == venue.upgrade_authority @ PerpsError::VenueProgramChanged
    )]
    pub venue_program_data: Box<Account<'info, ProgramData>>,

    /// CHECK: key pinned by the market
    #[account(constraint = pyth_price_feed.key() == market.pyth_price_feed)]
    pub pyth_price_feed: AccountInfo<'info>,

    /// CHECK: wallet; bound to its UserAccount and approval by seeds; is_signer read for min_signers
    pub taker: UncheckedAccount<'info>,

    #[account(
        mut,
        seeds = [b"user", taker.key().as_ref(), market.key().as_ref()],
        bump = taker_user.bump,
        constraint = taker_user.owner == taker.key()
    )]
    pub taker_user: Box<Account<'info, UserAccount>>,

    #[account(
        mut,
        seeds = [VenueApproval::SEED, taker.key().as_ref(), venue.key().as_ref(), &[ROLE_TAKER]],
        bump = taker_approval.bump
    )]
    pub taker_approval: Box<Account<'info, VenueApproval>>,

    #[account(
        init,
        payer = payer,
        space = Position::LEN,
        seeds = [b"vposition", taker.key().as_ref(), market.key().as_ref(), &params.position_nonce.to_le_bytes()],
        bump
    )]
    pub taker_position: Box<Account<'info, Position>>,

    /// CHECK: as taker. Distinct keys also stop the two mutable UserAccounts
    /// and approvals aliasing.
    #[account(constraint = maker.key() != taker.key() @ PerpsError::SelfTrade)]
    pub maker: UncheckedAccount<'info>,

    #[account(
        mut,
        seeds = [b"user", maker.key().as_ref(), market.key().as_ref()],
        bump = maker_user.bump,
        constraint = maker_user.owner == maker.key()
    )]
    pub maker_user: Box<Account<'info, UserAccount>>,

    #[account(
        mut,
        seeds = [VenueApproval::SEED, maker.key().as_ref(), venue.key().as_ref(), &[ROLE_MAKER]],
        bump = maker_approval.bump
    )]
    pub maker_approval: Box<Account<'info, VenueApproval>>,

    #[account(
        init,
        payer = payer,
        space = Position::LEN,
        seeds = [b"vmaker", taker_position.key().as_ref()],
        bump
    )]
    pub maker_position: Box<Account<'info, Position>>,

    pub system_program: Program<'info, System>,
}

/// Positions are one per fill, so this is the trade tape.
#[event]
pub struct VenueFilled {
    pub venue: Pubkey,
    pub market: Pubkey,
    pub taker: Pubkey,
    pub maker: Pubkey,
    pub taker_position: Pubkey,
    pub maker_position: Pubkey,
    pub taker_side: u8,
    pub size: u64,
    pub price: u64,
    pub oracle_price: u64,
    pub taker_collateral: u64,
    pub maker_collateral: u64,
    pub timestamp: i64,
}

pub fn handler(ctx: Context<VenueFill>, params: VenueFillParams) -> Result<()> {
    let taker_bump = *ctx.bumps.get("taker_position").unwrap();
    let maker_bump = *ctx.bumps.get("maker_position").unwrap();
    let a = ctx.accounts;
    let now = Clock::get()?.unix_timestamp;

    require!(!a.market.is_settled(), PerpsError::MarketSettled);
    require!(a.market.is_in_session(now), PerpsError::MarketClosed);
    require!(params.size > 0 && params.price > 0, PerpsError::InvalidFillParams);
    let (taker_side, maker_side) = match params.taker_side {
        0 => (Side::Long, Side::Short),
        1 => (Side::Short, Side::Long),
        _ => return Err(PerpsError::InvalidPositionSide.into()),
    };
    let signers = a.taker.is_signer as u8 + a.maker.is_signer as u8;
    require!(signers >= a.venue.min_signers, PerpsError::MissingSigner);

    let oracle = read_price_max_age(&a.pyth_price_feed, &a.market, now, a.venue.max_oracle_age_secs as u64)?;
    require!(within_band(params.price, oracle, a.venue.max_price_deviation_bps), PerpsError::FillPriceOutOfBand);

    let notional = u64::try_from(params.size as u128 * params.price as u128 / 1_000_000)
        .map_err(|_| PerpsError::MathOverflow)?;
    // A zero-notional dust fill would open positions with no margin ratio
    require!(notional > 0, PerpsError::InvalidFillParams);
    require!(notional <= a.venue.max_fill_notional, PerpsError::FillTooLarge);
    // A matched fill still adds gross exposure on both sides
    require!(
        a.market.can_increase_long_oi(params.size) && a.market.can_increase_short_oi(params.size),
        PerpsError::OpenInterestCapExceeded
    );

    let taker_collateral = take_margin(
        &a.market, &mut a.taker_user, &mut a.taker_approval, taker_side,
        params.taker_leverage, params.size, params.price, oracle, now,
    )?;
    let maker_collateral = take_margin(
        &a.market, &mut a.maker_user, &mut a.maker_approval, maker_side,
        params.maker_leverage, params.size, params.price, oracle, now,
    )?;

    let market_key = a.market.key();
    let funding = a.market.cumulative_funding;
    a.taker_position.init_open(
        a.taker.key(), market_key, taker_side, params.size, taker_collateral, params.price,
        params.taker_leverage, funding, now, ExecutionSource::Venue, taker_bump,
    );
    a.maker_position.init_open(
        a.maker.key(), market_key, maker_side, params.size, maker_collateral, params.price,
        params.maker_leverage, funding, now, ExecutionSource::Venue, maker_bump,
    );

    // Holds by construction of venue_margin; guards against future edits
    let (im, _) = a.market.margin_ratios(now);
    require!(
        a.taker_position.margin_ratio(oracle, 0) >= im && a.maker_position.margin_ratio(oracle, 0) >= im,
        PerpsError::VenueMarginInvariant
    );

    // market.total_positions is open_position's PDA nonce: leave it alone, or
    // every open_position built before this fill landed would fail.
    let market = &mut a.market;
    market.long_open_interest = market.long_open_interest.checked_add(params.size).ok_or(PerpsError::MathOverflow)?;
    market.short_open_interest = market.short_open_interest.checked_add(params.size).ok_or(PerpsError::MathOverflow)?;
    market.total_trades = market.total_trades.checked_add(1).ok_or(PerpsError::MathOverflow)?;
    for user in [&mut a.taker_user, &mut a.maker_user] {
        user.total_positions = user.total_positions.checked_add(1).ok_or(PerpsError::MathOverflow)?;
        user.total_trades = user.total_trades.checked_add(1).ok_or(PerpsError::MathOverflow)?;
    }
    let venue = &mut a.venue;
    venue.total_fills = venue.total_fills.checked_add(1).ok_or(PerpsError::MathOverflow)?;
    venue.total_notional = venue.total_notional.saturating_add(notional);

    emit!(VenueFilled {
        venue: venue.key(),
        market: market_key,
        taker: a.taker.key(),
        maker: a.maker.key(),
        taker_position: a.taker_position.key(),
        maker_position: a.maker_position.key(),
        taker_side: params.taker_side,
        size: params.size,
        price: params.price,
        oracle_price: oracle,
        taker_collateral,
        maker_collateral,
        timestamp: now,
    });
    Ok(())
}

/// Check one side against its approval and the market, then move its margin
/// out of free collateral. Returns the collateral posted.
#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn take_margin(
    market: &Market,
    user: &mut UserAccount,
    approval: &mut VenueApproval,
    side: Side,
    leverage: u32,
    size: u64,
    price: u64,
    oracle: u64,
    now: i64,
) -> Result<u64> {
    require!(leverage > 0 && leverage <= market.max_leverage, PerpsError::ExcessiveLeverage);
    require!(leverage <= approval.max_leverage, PerpsError::ApprovalLeverageExceeded);
    require!(now <= approval.expires_at, PerpsError::ApprovalExpired);
    let side_bit = match side {
        Side::Long => SIDE_LONG,
        Side::Short => SIDE_SHORT,
    };
    require!(approval.allowed_sides & side_bit != 0, PerpsError::ApprovalSideNotAllowed);
    let within_limit = match side {
        Side::Long => approval.max_buy_price == 0 || price <= approval.max_buy_price,
        Side::Short => approval.min_sell_price == 0 || price >= approval.min_sell_price,
    };
    require!(within_limit, PerpsError::ApprovalLimitPrice);
    require!(
        within_adverse_bound(side, price, oracle, approval.max_price_deviation_bps),
        PerpsError::ApprovalPriceDeviation
    );

    let collateral = venue_margin(market, side, size, price, oracle, leverage, now).ok_or(PerpsError::ExcessiveLeverage)?;
    require!(user.collateral_balance >= collateral, PerpsError::InsufficientCollateral);
    require!(approval.allowance >= collateral, PerpsError::ApprovalAllowanceExceeded);

    user.collateral_balance -= collateral;
    approval.allowance -= collateral;
    approval.fills = approval.fills.checked_add(1).ok_or(PerpsError::MathOverflow)?;
    approval.collateral_committed = approval.collateral_committed.saturating_add(collateral);
    Ok(collateral)
}
