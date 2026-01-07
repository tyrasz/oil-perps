use anchor_lang::prelude::*;

declare_id!("2XhUqeGhdkhKdZXkjHmMmFMXBJfYVVCbFxKjKqkCdMzt");

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq)]
pub enum OrderSide {
    Bid,
    Ask,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq)]
pub enum OrderType {
    Limit,
    Market,
    StopLoss,
    TakeProfit,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq)]
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
    pub best_bid: u64,
    pub best_ask: u64,
    pub total_bids: u64,
    pub total_asks: u64,
    pub sequence_number: u64,
    pub bump: u8,
}

impl OrderBook {
    pub const LEN: usize = 8 + 32 + 32 + 8 + 8 + 8 + 8 + 8 + 1 + 32;
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
}

impl Order {
    pub const LEN: usize = 8 + 32 + 32 + 1 + 1 + 8 + 8 + 8 + 1 + 8 + 8 + 8 + 1 + 32;
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
        orderbook.bump = *ctx.bumps.get("orderbook").unwrap();
        Ok(())
    }

    pub fn place_order(
        ctx: Context<PlaceOrder>,
        side: OrderSide,
        order_type: OrderType,
        price: u64,
        size: u64,
        expires_at: i64,
    ) -> Result<()> {
        let orderbook = &mut ctx.accounts.orderbook;
        let order = &mut ctx.accounts.order;
        let current_time = Clock::get()?.unix_timestamp;

        order.owner = ctx.accounts.owner.key();
        order.market = orderbook.market;
        order.side = side;
        order.order_type = order_type;
        order.price = price;
        order.size = size;
        order.filled_size = 0;
        order.status = OrderStatus::Open;
        order.created_at = current_time;
        order.expires_at = expires_at;
        order.sequence_number = orderbook.sequence_number;
        order.bump = *ctx.bumps.get("order").unwrap();

        orderbook.sequence_number += 1;

        match side {
            OrderSide::Bid => {
                orderbook.total_bids += 1;
                if price > orderbook.best_bid {
                    orderbook.best_bid = price;
                }
            }
            OrderSide::Ask => {
                orderbook.total_asks += 1;
                if price < orderbook.best_ask {
                    orderbook.best_ask = price;
                }
            }
        }

        msg!("Order placed: {:?} {} @ {}", side, size, price);
        Ok(())
    }

    pub fn cancel_order(ctx: Context<CancelOrder>) -> Result<()> {
        let orderbook = &mut ctx.accounts.orderbook;
        let order = &mut ctx.accounts.order;

        require!(order.status == OrderStatus::Open || order.status == OrderStatus::PartiallyFilled, ErrorCode::InvalidOrder);

        order.status = OrderStatus::Cancelled;

        match order.side {
            OrderSide::Bid => orderbook.total_bids = orderbook.total_bids.saturating_sub(1),
            OrderSide::Ask => orderbook.total_asks = orderbook.total_asks.saturating_sub(1),
        }

        msg!("Order cancelled: {}", order.key());
        Ok(())
    }

    pub fn match_orders(ctx: Context<MatchOrders>) -> Result<()> {
        let bid_order = &mut ctx.accounts.bid_order;
        let ask_order = &mut ctx.accounts.ask_order;

        require!(bid_order.price >= ask_order.price, ErrorCode::PriceMismatch);
        require!(bid_order.status == OrderStatus::Open || bid_order.status == OrderStatus::PartiallyFilled, ErrorCode::InvalidOrder);
        require!(ask_order.status == OrderStatus::Open || ask_order.status == OrderStatus::PartiallyFilled, ErrorCode::InvalidOrder);

        let bid_remaining = bid_order.size - bid_order.filled_size;
        let ask_remaining = ask_order.size - ask_order.filled_size;
        let fill_size = bid_remaining.min(ask_remaining);

        bid_order.filled_size += fill_size;
        ask_order.filled_size += fill_size;

        if bid_order.filled_size == bid_order.size {
            bid_order.status = OrderStatus::Filled;
        } else {
            bid_order.status = OrderStatus::PartiallyFilled;
        }

        if ask_order.filled_size == ask_order.size {
            ask_order.status = OrderStatus::Filled;
        } else {
            ask_order.status = OrderStatus::PartiallyFilled;
        }

        msg!("Orders matched: {} filled at price {}", fill_size, ask_order.price);
        Ok(())
    }
}

#[derive(Accounts)]
pub struct InitializeOrderBook<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    /// CHECK: Market account from perps-core
    pub market: AccountInfo<'info>,

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

    #[account(
        init,
        payer = owner,
        space = Order::LEN,
        seeds = [b"order", owner.key().as_ref(), &orderbook.sequence_number.to_le_bytes()],
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
        constraint = order.owner == owner.key()
    )]
    pub order: Account<'info, Order>,
}

#[derive(Accounts)]
pub struct MatchOrders<'info> {
    #[account(mut)]
    pub matcher: Signer<'info>,

    #[account(
        mut,
        seeds = [b"orderbook", orderbook.market.as_ref()],
        bump = orderbook.bump
    )]
    pub orderbook: Account<'info, OrderBook>,

    #[account(mut, constraint = bid_order.side == OrderSide::Bid)]
    pub bid_order: Account<'info, Order>,

    #[account(mut, constraint = ask_order.side == OrderSide::Ask)]
    pub ask_order: Account<'info, Order>,
}

#[error_code]
pub enum ErrorCode {
    #[msg("Invalid order status")]
    InvalidOrder,
    #[msg("Bid price must be >= ask price")]
    PriceMismatch,
}
