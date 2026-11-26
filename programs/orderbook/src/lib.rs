use anchor_lang::prelude::*;
use anchor_lang::system_program;
use perps_core::instructions::VenueFillParams;
use perps_core::state::{Market, Position, ProtocolConfig, Venue};

declare_id!("2XhUqeGhdkhKdZXkjHmMmFMXBJfYVVCbFxKjKqkCdMzt");

pub const MAX_ORDER_TTL_SECS: i64 = 30 * 86_400;
/// Caps the fills (so Position rent deposit and dust positions) one order can take.
pub const MAX_FILLS: u8 = 16;

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum OrderSide {
    Bid,
    Ask,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum OrderType {
    Limit,
    Market,
    StopLoss,
    TakeProfit,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum OrderStatus {
    Open,
    PartiallyFilled,
    Filled,
    Cancelled,
}

#[account]
pub struct OrderBook {
    pub market: Pubkey,
    pub authority: Pubkey,
    // Informational only: raised by place_order, never lowered when orders
    // fill, cancel or expire. Matching does not read them.
    pub best_bid: u64,
    pub best_ask: u64,
    pub total_bids: u64,
    pub total_asks: u64,
    pub sequence_number: u64,
    pub bump: u8,
    /// Hot key the admin lets crank match_orders, so the upgrade-authority key
    /// stays out of the crank. Default (as on books created before this field)
    /// grants nothing.
    pub matcher: Pubkey,
}

impl OrderBook {
    pub const LEN: usize = 8 + 32 + 32 + 8 + 8 + 8 + 8 + 8 + 1 + 32; // 145, no padding left
}

#[account]
pub struct Order {
    pub owner: Pubkey,
    pub market: Pubkey,
    pub side: OrderSide,
    pub order_type: OrderType,
    pub price: u64,
    pub size: u64,
    pub filled_size: u64,
    pub status: OrderStatus,
    pub created_at: i64,
    pub expires_at: i64,
    pub sequence_number: u64,
    pub bump: u8,
    pub leverage: u32,      // 3 dp; 0 = legacy order from before fills opened positions
    pub max_fills: u8,      // Position rents prepaid at placement
    pub fills: u8,
    pub min_fill_size: u64, // Each fill is at least this, or the whole remainder
}

impl Order {
    pub const LEN: usize = 8 + 32 + 32 + 1 + 1 + 8 + 8 + 8 + 1 + 8 + 8 + 8 + 1 + 4 + 1 + 1 + 8 + 18; // 156
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug)]
pub struct PlaceOrderParams {
    pub side: OrderSide,
    pub order_type: OrderType,
    pub price: u64,
    pub size: u64,
    pub leverage: u32, // 3 dp, 1..=market.max_leverage
    pub expires_at: i64,
    pub max_fills: u8,      // 1..=MAX_FILLS
    pub min_fill_size: u64, // <= size; 0 = size / max_fills
}

#[program]
pub mod orderbook {
    use super::*;

    pub fn initialize_orderbook(ctx: Context<InitializeOrderBook>) -> Result<()> {
        let orderbook = &mut ctx.accounts.orderbook;
        orderbook.market = ctx.accounts.market.key();
        orderbook.authority = ctx.accounts.authority.key();
        orderbook.best_bid = 0;
        orderbook.best_ask = u64::MAX;
        orderbook.total_bids = 0;
        orderbook.total_asks = 0;
        orderbook.sequence_number = 0;
        orderbook.bump = ctx.bumps.orderbook;
        orderbook.matcher = Pubkey::default();
        Ok(())
    }

    pub fn place_order(ctx: Context<PlaceOrder>, params: PlaceOrderParams) -> Result<()> {
        let current_time = Clock::get()?.unix_timestamp;
        let p = params;

        // Only resting limits ever match; the other types never had a trigger
        require!(p.order_type == OrderType::Limit, ErrorCode::UnsupportedOrderType);
        require!(p.price > 0 && p.size > 0, ErrorCode::InvalidOrderParams);
        require!(p.leverage > 0 && p.leverage <= ctx.accounts.market.max_leverage, ErrorCode::InvalidOrderParams);
        require!(
            current_time < p.expires_at && p.expires_at <= current_time.saturating_add(MAX_ORDER_TTL_SECS),
            ErrorCode::InvalidOrderParams
        );
        require!(p.max_fills >= 1 && p.max_fills <= MAX_FILLS, ErrorCode::InvalidOrderParams);
        require!(p.min_fill_size <= p.size, ErrorCode::InvalidOrderParams);

        // The matcher pays perps-core's Position rent on every fill; it is paid
        // back out of this deposit, and what is unused returns on close.
        let deposit = (p.max_fills as u64)
            .checked_mul(Rent::get()?.minimum_balance(Position::LEN))
            .ok_or(ErrorCode::InvalidOrderParams)?;
        system_program::transfer(
            CpiContext::new(
                ctx.accounts.system_program.key(),
                system_program::Transfer {
                    from: ctx.accounts.owner.to_account_info(),
                    to: ctx.accounts.order.to_account_info(),
                },
            ),
            deposit,
        )?;

        let orderbook = &mut ctx.accounts.orderbook;
        let order = &mut ctx.accounts.order;
        order.owner = ctx.accounts.owner.key();
        order.market = orderbook.market;
        order.side = p.side;
        order.order_type = p.order_type;
        order.price = p.price;
        order.size = p.size;
        order.filled_size = 0;
        order.status = OrderStatus::Open;
        order.created_at = current_time;
        order.expires_at = p.expires_at;
        order.sequence_number = orderbook.sequence_number;
        order.bump = ctx.bumps.order;
        order.leverage = p.leverage;
        order.max_fills = p.max_fills;
        order.fills = 0;
        // Resolved here so match_orders reads one field
        order.min_fill_size = if p.min_fill_size == 0 { p.size / p.max_fills as u64 } else { p.min_fill_size };

        orderbook.sequence_number += 1;

        match p.side {
            OrderSide::Bid => {
                orderbook.total_bids += 1;
                if p.price > orderbook.best_bid {
                    orderbook.best_bid = p.price;
                }
            }
            OrderSide::Ask => {
                orderbook.total_asks += 1;
                if p.price < orderbook.best_ask {
                    orderbook.best_ask = p.price;
                }
            }
        }

        msg!("Order placed: {:?} {} @ {}", p.side, p.size, p.price);
        Ok(())
    }

    pub fn cancel_order(ctx: Context<CancelOrder>) -> Result<()> {
        let orderbook = &mut ctx.accounts.orderbook;
        let order = &mut ctx.accounts.order;

        require!(order.status == OrderStatus::Open || order.status == OrderStatus::PartiallyFilled, ErrorCode::InvalidOrder);

        order.status = OrderStatus::Cancelled;
        decrement_side(orderbook, order.side);

        msg!("Order cancelled: {}", order.key());
        Ok(())
    }

    /// Permissionless: an expired order can never match, so anyone may return
    /// its rent and deposit to the owner. Orders the old program left Filled
    /// or Cancelled without closing them are closed the same way, at any time.
    /// (The old cancel_order took its order off the counters; the old
    /// match_orders never did, so a legacy Filled order still counts.)
    pub fn close_expired_order(ctx: Context<CloseExpiredOrder>) -> Result<()> {
        let now = Clock::get()?.unix_timestamp;
        let order = &ctx.accounts.order;
        match order.status {
            OrderStatus::Cancelled => {}
            OrderStatus::Filled => decrement_side(&mut ctx.accounts.orderbook, order.side),
            _ => {
                require!(now > order.expires_at, ErrorCode::OrderNotExpired);
                decrement_side(&mut ctx.accounts.orderbook, order.side);
            }
        }
        msg!("Order closed: {}", order.key());
        Ok(())
    }

    pub fn set_matcher(ctx: Context<SetMatcher>, matcher: Pubkey) -> Result<()> {
        ctx.accounts.orderbook.matcher = matcher;
        msg!("Matcher set: {}", matcher);
        Ok(())
    }

    /// Settle a crossing bid and ask as a perps-core venue fill: the later
    /// order is the taker, and both trade at the resting (maker) price, which
    /// satisfies both limits. perps-core enforces margin, band and each
    /// owner's approval; this program only decides which orders trade.
    /// `position_nonce` seeds the taker's position (the matcher picks it).
    /// `max_fill_size` (0 = none) lets the matcher split a cross into fills
    /// that fit perps-core's per-fill notional cap and OI headroom.
    pub fn match_orders(ctx: Context<MatchOrders>, position_nonce: u64, max_fill_size: u64) -> Result<()> {
        let now = Clock::get()?.unix_timestamp;
        let a = &ctx.accounts;

        let matcher = a.matcher.key();
        require!(
            matcher == a.config.admin || (a.orderbook.matcher != Pubkey::default() && matcher == a.orderbook.matcher),
            ErrorCode::Unauthorized
        );
        check_fillable(&a.bid_order, now)?;
        check_fillable(&a.ask_order, now)?;
        require!(a.bid_order.owner != a.ask_order.owner, ErrorCode::SelfTrade);
        require!(a.bid_order.price >= a.ask_order.price, ErrorCode::PriceMismatch);

        let taker_is_bid = a.bid_order.sequence_number > a.ask_order.sequence_number;
        let (taker, maker) = if taker_is_bid { (&a.bid_order, &a.ask_order) } else { (&a.ask_order, &a.bid_order) };
        let bid_remaining = a.bid_order.size - a.bid_order.filled_size;
        let ask_remaining = a.ask_order.size - a.ask_order.filled_size;
        let crossing = bid_remaining.min(ask_remaining);
        let fill_size = if max_fill_size > 0 { crossing.min(max_fill_size) } else { crossing };
        // Both sides, so neither owner is split into positions below their minimum
        require!(
            (fill_size >= a.bid_order.min_fill_size || fill_size == bid_remaining)
                && (fill_size >= a.ask_order.min_fill_size || fill_size == ask_remaining),
            ErrorCode::FillTooSmall
        );

        let params = VenueFillParams {
            size: fill_size,
            price: maker.price,
            taker_side: if taker_is_bid { 0 } else { 1 },
            taker_leverage: taker.leverage,
            maker_leverage: maker.leverage,
            position_nonce,
        };
        let price = params.price;
        cpi_venue_fill(a, taker_is_bid, ctx.bumps.venue_authority, params)?;

        let rent = Rent::get()?;
        let position_rent = rent.minimum_balance(Position::LEN);
        let order_rent = rent.minimum_balance(Order::LEN);
        let a = ctx.accounts;
        let matcher_info = a.matcher.to_account_info();
        settle_order(&mut a.orderbook, &mut a.bid_order, &a.bid_owner, &matcher_info, fill_size, position_rent, order_rent)?;
        settle_order(&mut a.orderbook, &mut a.ask_order, &a.ask_owner, &matcher_info, fill_size, position_rent, order_rent)?;

        msg!("Orders matched: {} filled at price {}", fill_size, price);
        Ok(())
    }
}

fn decrement_side(orderbook: &mut OrderBook, side: OrderSide) {
    match side {
        OrderSide::Bid => orderbook.total_bids = orderbook.total_bids.saturating_sub(1),
        OrderSide::Ask => orderbook.total_asks = orderbook.total_asks.saturating_sub(1),
    }
}

fn check_fillable(order: &Order, now: i64) -> Result<()> {
    require!(order.status == OrderStatus::Open || order.status == OrderStatus::PartiallyFilled, ErrorCode::InvalidOrder);
    require!(order.order_type == OrderType::Limit, ErrorCode::UnsupportedOrderType);
    // Placed before orders carried leverage or a rent deposit
    require!(order.leverage > 0, ErrorCode::LegacyOrder);
    require!(now <= order.expires_at, ErrorCode::OrderExpired);
    require!(order.fills < order.max_fills, ErrorCode::RentDepositExhausted);
    Ok(())
}

#[inline(never)]
fn cpi_venue_fill<'info>(a: &MatchOrders<'info>, taker_is_bid: bool, bump: u8, params: VenueFillParams) -> Result<()> {
    let (taker, maker) = if taker_is_bid { (&a.bid_owner, &a.ask_owner) } else { (&a.ask_owner, &a.bid_owner) };
    let market = a.orderbook.market;
    let bump = [bump];
    let seeds: &[&[u8]] = &[Venue::AUTHORITY_SEED, market.as_ref(), &bump];
    // taker and maker go in as non-signers: the venue registers with
    // min_signers = 0, and each owner's approval is the bound on what a fill
    // can commit.
    let accounts = perps_core::cpi::accounts::VenueFill {
        venue_authority: a.venue_authority.to_account_info(),
        payer: a.matcher.to_account_info(),
        config: a.config.to_account_info(),
        market: a.market.to_account_info(),
        venue: a.perps_venue.to_account_info(),
        venue_program_data: a.venue_program_data.to_account_info(),
        pyth_price_feed: a.pyth_price_feed.to_account_info(),
        taker: taker.to_account_info(),
        taker_user: a.taker_user.to_account_info(),
        taker_approval: a.taker_approval.to_account_info(),
        taker_position: a.taker_position.to_account_info(),
        maker: maker.to_account_info(),
        maker_user: a.maker_user.to_account_info(),
        maker_approval: a.maker_approval.to_account_info(),
        maker_position: a.maker_position.to_account_info(),
        system_program: a.system_program.to_account_info(),
        lp_pool: a.perps_lp_pool.to_account_info(),
    };
    perps_core::cpi::venue_fill(
        CpiContext::new_with_signer(a.perps_core_program.key(), accounts, &[seeds]),
        params,
    )
}

/// Record one fill on `order`, pay the matcher back the Position rent it
/// fronted for this order's side, and close the order once it is filled or
/// its prepaid fills are used up (the remainder is cancelled).
#[inline(never)]
fn settle_order<'info>(
    orderbook: &mut OrderBook,
    order: &mut Account<'info, Order>,
    owner: &UncheckedAccount<'info>,
    matcher: &AccountInfo<'info>,
    fill_size: u64,
    position_rent: u64,
    order_rent: u64,
) -> Result<()> {
    order.filled_size = order.filled_size.checked_add(fill_size).ok_or(ErrorCode::InvalidOrder)?;
    order.fills = order.fills.checked_add(1).ok_or(ErrorCode::RentDepositExhausted)?;
    let filled = order.filled_size == order.size;
    order.status = if filled { OrderStatus::Filled } else { OrderStatus::PartiallyFilled };

    // The orderbook owns the order, so it can debit it directly
    let info = order.to_account_info();
    let left = info
        .lamports()
        .checked_sub(position_rent)
        .filter(|l| *l >= order_rent)
        .ok_or(ErrorCode::RentDepositExhausted)?;
    let paid = matcher.lamports().checked_add(position_rent).ok_or(ErrorCode::RentDepositExhausted)?;
    **info.try_borrow_mut_lamports()? = left;
    **matcher.try_borrow_mut_lamports()? = paid;

    if filled || order.fills >= order.max_fills {
        if !filled {
            order.status = OrderStatus::Cancelled;
        }
        decrement_side(orderbook, order.side);
        order.close(owner.to_account_info())?;
    }
    Ok(())
}

#[derive(Accounts)]
pub struct InitializeOrderBook<'info> {
    /// Must be the perps-core admin: one book per market, so a permissionless
    /// init would let anyone claim it (and its matching authority).
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(
        seeds = [ProtocolConfig::SEED],
        bump = config.bump,
        seeds::program = perps_core::ID,
        constraint = config.admin == authority.key() @ ErrorCode::Unauthorized
    )]
    pub config: Box<Account<'info, ProtocolConfig>>,

    /// The perps-core market this book serves (owner checked by Account)
    pub market: Box<Account<'info, Market>>,

    #[account(
        init,
        payer = authority,
        space = OrderBook::LEN,
        seeds = [b"orderbook", market.key().as_ref()],
        bump
    )]
    pub orderbook: Account<'info, OrderBook>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct PlaceOrder<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,

    #[account(
        mut,
        seeds = [b"orderbook", orderbook.market.as_ref()],
        bump = orderbook.bump
    )]
    pub orderbook: Account<'info, OrderBook>,

    /// For max_leverage
    #[account(address = orderbook.market @ ErrorCode::WrongOrderBook)]
    pub market: Box<Account<'info, Market>>,

    #[account(
        init,
        payer = owner,
        space = Order::LEN,
        seeds = [b"order", orderbook.key().as_ref(), owner.key().as_ref(), &orderbook.sequence_number.to_le_bytes()],
        bump
    )]
    pub order: Account<'info, Order>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct CancelOrder<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,

    #[account(
        mut,
        seeds = [b"orderbook", orderbook.market.as_ref()],
        bump = orderbook.bump
    )]
    pub orderbook: Account<'info, OrderBook>,

    #[account(
        mut,
        constraint = order.owner == owner.key(),
        constraint = order.market == orderbook.market @ ErrorCode::WrongOrderBook,
        close = owner
    )]
    pub order: Account<'info, Order>,
}

#[derive(Accounts)]
pub struct CloseExpiredOrder<'info> {
    #[account(
        mut,
        seeds = [b"orderbook", orderbook.market.as_ref()],
        bump = orderbook.bump
    )]
    pub orderbook: Account<'info, OrderBook>,

    #[account(
        mut,
        constraint = order.market == orderbook.market @ ErrorCode::WrongOrderBook,
        close = owner
    )]
    pub order: Account<'info, Order>,

    /// CHECK: receives the order's rent and deposit
    #[account(mut, address = order.owner @ ErrorCode::Unauthorized)]
    pub owner: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct SetMatcher<'info> {
    pub admin: Signer<'info>,

    #[account(
        seeds = [ProtocolConfig::SEED],
        bump = config.bump,
        seeds::program = perps_core::ID,
        constraint = config.admin == admin.key() @ ErrorCode::Unauthorized
    )]
    pub config: Box<Account<'info, ProtocolConfig>>,

    #[account(
        mut,
        seeds = [b"orderbook", orderbook.market.as_ref()],
        bump = orderbook.bump
    )]
    pub orderbook: Account<'info, OrderBook>,
}

#[derive(Accounts)]
pub struct MatchOrders<'info> {
    /// The live perps-core admin (it follows an admin handover;
    /// orderbook.authority is a snapshot from init) or the book's hot-key
    /// matcher. Orders are loose accounts with no on-chain price-time
    /// priority, so an open matcher could choose which orders trade. Pays
    /// both Positions' rent and is paid back from the orders' deposits.
    #[account(mut)]
    pub matcher: Signer<'info>,

    /// Also forwarded to venue_fill
    #[account(
        seeds = [ProtocolConfig::SEED],
        bump = config.bump,
        seeds::program = perps_core::ID
    )]
    pub config: Box<Account<'info, ProtocolConfig>>,

    #[account(
        mut,
        seeds = [b"orderbook", orderbook.market.as_ref()],
        bump = orderbook.bump
    )]
    pub orderbook: Box<Account<'info, OrderBook>>,

    #[account(
        mut,
        constraint = bid_order.side == OrderSide::Bid,
        constraint = bid_order.market == orderbook.market @ ErrorCode::WrongOrderBook
    )]
    pub bid_order: Box<Account<'info, Order>>,

    #[account(
        mut,
        constraint = ask_order.side == OrderSide::Ask,
        constraint = ask_order.market == orderbook.market @ ErrorCode::WrongOrderBook
    )]
    pub ask_order: Box<Account<'info, Order>>,

    /// CHECK: position owner in venue_fill; receives the order's lamports when it closes
    #[account(mut, address = bid_order.owner)]
    pub bid_owner: UncheckedAccount<'info>,

    /// CHECK: as bid_owner
    #[account(mut, address = ask_order.owner)]
    pub ask_owner: UncheckedAccount<'info>,

    /// CHECK: this program's venue signer for the market
    #[account(seeds = [Venue::AUTHORITY_SEED, orderbook.market.as_ref()], bump)]
    pub venue_authority: UncheckedAccount<'info>,

    /// CHECK: perps-core validates
    #[account(mut)]
    pub perps_venue: UncheckedAccount<'info>,

    /// CHECK: perps-core validates
    pub venue_program_data: UncheckedAccount<'info>,

    /// CHECK: must be this book's market; perps-core validates the rest.
    /// Untyped because the CPI mutates it.
    #[account(mut, address = orderbook.market @ ErrorCode::WrongOrderBook)]
    pub market: UncheckedAccount<'info>,

    /// CHECK: perps-core validates
    pub pyth_price_feed: UncheckedAccount<'info>,

    /// CHECK: perps-core validates
    #[account(mut)]
    pub taker_user: UncheckedAccount<'info>,

    /// CHECK: perps-core validates
    #[account(mut)]
    pub taker_approval: UncheckedAccount<'info>,

    /// CHECK: perps-core validates and inits
    #[account(mut)]
    pub taker_position: UncheckedAccount<'info>,

    /// CHECK: perps-core validates
    #[account(mut)]
    pub maker_user: UncheckedAccount<'info>,

    /// CHECK: perps-core validates
    #[account(mut)]
    pub maker_approval: UncheckedAccount<'info>,

    /// CHECK: perps-core validates and inits
    #[account(mut)]
    pub maker_position: UncheckedAccount<'info>,

    /// CHECK: perps-core validates (the market's LP pool, venue_fill account 17)
    #[account(mut)]
    pub perps_lp_pool: UncheckedAccount<'info>,

    pub perps_core_program: Program<'info, perps_core::program::PerpsCore>,
    pub system_program: Program<'info, System>,
}

#[error_code]
pub enum ErrorCode {
    #[msg("Invalid order status")]
    InvalidOrder,
    #[msg("Bid price must be >= ask price")]
    PriceMismatch,

    #[msg("Unauthorized")]
    Unauthorized,

    #[msg("Order belongs to a different order book")]
    WrongOrderBook,

    #[msg("Order has expired")]
    OrderExpired,
    #[msg("Order was placed before fills opened positions; cancel and re-place it")]
    LegacyOrder,
    #[msg("Only limit orders are supported")]
    UnsupportedOrderType,
    #[msg("Bid and ask have the same owner")]
    SelfTrade,
    #[msg("Invalid order parameters")]
    InvalidOrderParams,
    #[msg("Order has not expired")]
    OrderNotExpired,
    #[msg("Order's prepaid fills are used up")]
    RentDepositExhausted,
    #[msg("Fill is below an order's minimum and is not its whole remainder")]
    FillTooSmall,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_sizes_are_pinned() {
        let book = OrderBook {
            market: Pubkey::default(), authority: Pubkey::default(), best_bid: 0, best_ask: 0, total_bids: 0,
            total_asks: 0, sequence_number: 0, bump: 0, matcher: Pubkey::default(),
        };
        assert_eq!(OrderBook::LEN, 145);
        assert_eq!(8 + anchor_lang::prelude::borsh::to_vec(&book).unwrap().len(), OrderBook::LEN);

        let order = Order {
            owner: Pubkey::default(), market: Pubkey::default(), side: OrderSide::Bid, order_type: OrderType::Limit,
            price: 0, size: 0, filled_size: 0, status: OrderStatus::Open, created_at: 0, expires_at: 0,
            sequence_number: 0, bump: 0, leverage: 0, max_fills: 0, fills: 0, min_fill_size: 0,
        };
        assert_eq!(Order::LEN, 156);
        assert_eq!(8 + anchor_lang::prelude::borsh::to_vec(&order).unwrap().len() + 18, Order::LEN);
    }
}
