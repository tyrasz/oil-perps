use anchor_lang::prelude::*;
use crate::oracle::read_price;
use crate::state::{Market, Position, UserAccount, Side, PositionStatus, ExecutionSource};
use crate::errors::PerpsError;

#[derive(AnchorSerialize, AnchorDeserialize)]
pub struct OpenPositionParams {
    pub side: u8,           // 0 = Long, 1 = Short
    pub size: u64,          // Position size in base units
    pub leverage: u32,      // Leverage (3 decimals: 10x = 10_000)
}

#[derive(Accounts)]
#[instruction(params: OpenPositionParams)]
pub struct OpenPosition<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,

    #[account(
        mut,
        seeds = [b"user", owner.key().as_ref(), market.key().as_ref()],
        bump = user_account.bump,
        constraint = user_account.owner == owner.key()
    )]
    pub user_account: Account<'info, UserAccount>,

    #[account(
        mut,
        seeds = [b"market", market.collateral_mint.as_ref(), market.commodity_seed()],
        bump = market.bump,
        constraint = !market.is_paused @ PerpsError::MarketPaused
    )]
    pub market: Account<'info, Market>,

    #[account(
        init,
        payer = owner,
        space = Position::LEN,
        seeds = [b"position", owner.key().as_ref(), market.key().as_ref(), &market.total_positions.to_le_bytes()],
        bump
    )]
    pub position: Account<'info, Position>,

    /// CHECK: Pyth price feed, validated below
    #[account(constraint = pyth_price_feed.key() == market.pyth_price_feed)]
    pub pyth_price_feed: AccountInfo<'info>,

    pub system_program: Program<'info, System>,
}

pub fn handler(ctx: Context<OpenPosition>, params: OpenPositionParams) -> Result<()> {
    let market = &mut ctx.accounts.market;
    let user_account = &mut ctx.accounts.user_account;
    let position = &mut ctx.accounts.position;

    // Validate leverage
    require!(
        params.leverage > 0 && params.leverage <= market.max_leverage,
        PerpsError::ExcessiveLeverage
    );

    let current_time = Clock::get()?.unix_timestamp;
    require!(!market.is_settled(), PerpsError::MarketSettled);
    require!(market.is_in_session(current_time), PerpsError::MarketClosed);

    // Validated oracle price (staleness, confidence, floor), 6 decimals
    let oracle_price = read_price(&ctx.accounts.pyth_price_feed, market, current_time)?;

    // Calculate required collateral
    // notional = size * price / 1_000_000
    // required_collateral = notional / leverage (must meet initial margin)
    let notional = (params.size as u128)
        .checked_mul(oracle_price as u128)
        .and_then(|v| v.checked_div(1_000_000))
        .ok_or(PerpsError::MathOverflow)? as u64;

    let required_collateral = market
        .required_collateral(notional, params.leverage, current_time)
        .ok_or(PerpsError::ExcessiveLeverage)?;

    require!(
        user_account.collateral_balance >= required_collateral,
        PerpsError::InsufficientCollateral
    );

    // Parse side
    let side = match params.side {
        0 => Side::Long,
        1 => Side::Short,
        _ => return Err(PerpsError::InvalidPositionSide.into()),
    };

    // Check OI caps
    match side {
        Side::Long => require!(
            market.can_increase_long_oi(params.size),
            PerpsError::OpenInterestCapExceeded
        ),
        Side::Short => require!(
            market.can_increase_short_oi(params.size),
            PerpsError::OpenInterestCapExceeded
        ),
    }

    // Deduct collateral from user
    user_account.collateral_balance = user_account.collateral_balance
        .checked_sub(required_collateral)
        .ok_or(PerpsError::MathOverflow)?;

    // Initialize position
    position.owner = ctx.accounts.owner.key();
    position.market = market.key();
    position.side = side;
    position.size = params.size;
    position.collateral = required_collateral;
    position.entry_price = oracle_price;
    position.leverage = params.leverage;
    position.realized_pnl = 0;
    position.last_funding_payment = market.funding_rate;
    position.opened_at = current_time;
    position.last_updated_at = current_time;
    position.status = PositionStatus::Open;
    position.execution_source = ExecutionSource::OrderBook;
    position.bump = *ctx.bumps.get("position").unwrap();

    // Update market OI
    match side {
        Side::Long => {
            market.long_open_interest = market.long_open_interest
                .checked_add(params.size)
                .ok_or(PerpsError::MathOverflow)?;
        }
        Side::Short => {
            market.short_open_interest = market.short_open_interest
                .checked_add(params.size)
                .ok_or(PerpsError::MathOverflow)?;
        }
    }

    market.total_positions = market.total_positions
        .checked_add(1)
        .ok_or(PerpsError::MathOverflow)?;

    market.total_trades = market.total_trades
        .checked_add(1)
        .ok_or(PerpsError::MathOverflow)?;

    user_account.total_positions = user_account.total_positions
        .checked_add(1)
        .ok_or(PerpsError::MathOverflow)?;

    user_account.total_trades = user_account.total_trades
        .checked_add(1)
        .ok_or(PerpsError::MathOverflow)?;

    msg!(
        "Position opened: {} {} @ {} with {}x leverage",
        if side == Side::Long { "LONG" } else { "SHORT" },
        params.size,
        oracle_price,
        params.leverage / 1000
    );

    Ok(())
}
