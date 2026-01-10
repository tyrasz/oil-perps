use anchor_lang::prelude::*;

declare_id!("2XhUqeGhdkhKdZXkjHmMmFMXBJfYVVCbFxKjKqkCdMzt");

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Default)]
pub enum OrderSide {
    #[default]
    Bid,
    Ask,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Default)]
pub enum OrderType {
    #[default]
    Limit,
    Market,
    StopLoss,
    TakeProfit,
    StopLimit,
    TrailingStop,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Default)]
pub enum OrderStatus {
    #[default]
    Open,
    PartiallyFilled,
    Filled,
    Cancelled,
    Triggered,      // For conditional orders that have been triggered
    Expired,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Default)]
pub enum TriggerCondition {
    #[default]
    None,
    PriceAbove,     // Trigger when price >= trigger_price
    PriceBelow,     // Trigger when price <= trigger_price
}

#[account]
pub struct OrderBook {
    pub market: Pubkey,
    pub authority: Pubkey,
    pub best_bid: u64,
    pub best_ask: u64,
    pub total_bids: u64,
    pub total_asks: u64,
    pub total_trigger_orders: u64,
    pub sequence_number: u64,
    pub bump: u8,
}

impl OrderBook {
    pub const LEN: usize = 8 + 32 + 32 + 8 + 8 + 8 + 8 + 8 + 8 + 1 + 32;
}

#[account]
#[derive(Default)]
pub struct Order {
    pub owner: Pubkey,
    pub market: Pubkey,
    pub side: OrderSide,
    pub order_type: OrderType,
    pub price: u64,                     // Limit price (0 for market orders)
    pub size: u64,
    pub filled_size: u64,
    pub status: OrderStatus,
    pub created_at: i64,
    pub expires_at: i64,

    // Trigger order fields
    pub trigger_price: u64,             // Price at which to trigger (for stop/take-profit)
    pub trigger_condition: TriggerCondition,

    // Trailing stop fields
    pub trailing_amount: u64,           // Trail distance (in price units or basis points)
    pub trailing_percent: bool,         // If true, trailing_amount is in basis points
    pub highest_price: u64,             // Highest price seen (for trailing stop on long)
    pub lowest_price: u64,              // Lowest price seen (for trailing stop on short)

    // OCO (One-Cancels-Other) fields
    pub linked_order: Pubkey,           // Linked OCO order (Pubkey::default() if none)
    pub is_oco: bool,

    // Position reference (for position-linked orders)
    pub position: Pubkey,               // Associated position (for SL/TP on positions)
    pub reduce_only: bool,              // Only reduce position, don't open new

    pub sequence_number: u64,
    pub bump: u8,
}

impl Order {
    pub const LEN: usize = 8 +  // discriminator
        32 +  // owner
        32 +  // market
        1 +   // side
        1 +   // order_type
        8 +   // price
        8 +   // size
        8 +   // filled_size
        1 +   // status
        8 +   // created_at
        8 +   // expires_at
        8 +   // trigger_price
        1 +   // trigger_condition
        8 +   // trailing_amount
        1 +   // trailing_percent
        8 +   // highest_price
        8 +   // lowest_price
        32 +  // linked_order
        1 +   // is_oco
        32 +  // position
        1 +   // reduce_only
        8 +   // sequence_number
        1 +   // bump
        64;   // padding

    /// Check if trigger condition is met
    pub fn should_trigger(&self, current_price: u64) -> bool {
        match self.trigger_condition {
            TriggerCondition::None => true, // Regular limit/market orders
            TriggerCondition::PriceAbove => current_price >= self.trigger_price,
            TriggerCondition::PriceBelow => current_price <= self.trigger_price,
        }
    }

    /// Update trailing stop trigger price based on current market price
    pub fn update_trailing_stop(&mut self, current_price: u64) {
        if self.order_type != OrderType::TrailingStop {
            return;
        }

        match self.side {
            OrderSide::Ask => {
                // Selling (closing long) - track highest price, trigger on pullback
                if current_price > self.highest_price {
                    self.highest_price = current_price;
                    // Update trigger price
                    if self.trailing_percent {
                        // trailing_amount is in basis points (100 = 1%)
                        self.trigger_price = current_price
                            .saturating_sub(current_price * self.trailing_amount / 10000);
                    } else {
                        self.trigger_price = current_price.saturating_sub(self.trailing_amount);
                    }
                }
            }
            OrderSide::Bid => {
                // Buying (closing short) - track lowest price, trigger on rally
                if current_price < self.lowest_price || self.lowest_price == 0 {
                    self.lowest_price = current_price;
                    // Update trigger price
                    if self.trailing_percent {
                        self.trigger_price = current_price
                            .saturating_add(current_price * self.trailing_amount / 10000);
                    } else {
                        self.trigger_price = current_price.saturating_add(self.trailing_amount);
                    }
                }
            }
        }
    }
}

#[derive(AnchorSerialize, AnchorDeserialize)]
pub struct PlaceOrderParams {
    pub side: OrderSide,
    pub order_type: OrderType,
    pub price: u64,
    pub size: u64,
    pub expires_at: i64,
    pub trigger_price: u64,
    pub trigger_condition: TriggerCondition,
    pub trailing_amount: u64,
    pub trailing_percent: bool,
    pub reduce_only: bool,
}

#[derive(AnchorSerialize, AnchorDeserialize)]
pub struct PlaceOcoParams {
    // Primary order (e.g., take-profit)
    pub primary_side: OrderSide,
    pub primary_type: OrderType,
    pub primary_price: u64,
    pub primary_trigger_price: u64,
    pub primary_trigger_condition: TriggerCondition,

    // Secondary order (e.g., stop-loss)
    pub secondary_side: OrderSide,
    pub secondary_type: OrderType,
    pub secondary_price: u64,
    pub secondary_trigger_price: u64,
    pub secondary_trigger_condition: TriggerCondition,

    // Shared params
    pub size: u64,
    pub expires_at: i64,
    pub reduce_only: bool,
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
        orderbook.total_trigger_orders = 0;
        orderbook.sequence_number = 0;
        orderbook.bump = *ctx.bumps.get("orderbook").unwrap();
        Ok(())
    }

    /// Place a standard order (limit, market, stop-loss, take-profit, trailing stop)
    pub fn place_order(ctx: Context<PlaceOrder>, params: PlaceOrderParams) -> Result<()> {
        let orderbook = &mut ctx.accounts.orderbook;
        let order = &mut ctx.accounts.order;
        let current_time = Clock::get()?.unix_timestamp;

        // Validate order params
        require!(params.size > 0, ErrorCode::InvalidOrderSize);

        // For limit orders, price must be set
        if params.order_type == OrderType::Limit || params.order_type == OrderType::StopLimit {
            require!(params.price > 0, ErrorCode::InvalidPrice);
        }

        // For trigger orders, trigger price must be set
        if params.order_type == OrderType::StopLoss
            || params.order_type == OrderType::TakeProfit
            || params.order_type == OrderType::StopLimit
            || params.order_type == OrderType::TrailingStop
        {
            require!(params.trigger_price > 0 || params.order_type == OrderType::TrailingStop,
                ErrorCode::InvalidTriggerPrice);
        }

        order.owner = ctx.accounts.owner.key();
        order.market = orderbook.market;
        order.side = params.side;
        order.order_type = params.order_type;
        order.price = params.price;
        order.size = params.size;
        order.filled_size = 0;
        order.status = OrderStatus::Open;
        order.created_at = current_time;
        order.expires_at = params.expires_at;
        order.trigger_price = params.trigger_price;
        order.trigger_condition = params.trigger_condition;
        order.trailing_amount = params.trailing_amount;
        order.trailing_percent = params.trailing_percent;
        order.highest_price = 0;
        order.lowest_price = u64::MAX;
        order.linked_order = Pubkey::default();
        order.is_oco = false;
        order.position = Pubkey::default();
        order.reduce_only = params.reduce_only;
        order.sequence_number = orderbook.sequence_number;
        order.bump = *ctx.bumps.get("order").unwrap();

        orderbook.sequence_number += 1;

        // Track order counts
        let is_trigger_order = params.trigger_condition != TriggerCondition::None;
        if is_trigger_order {
            orderbook.total_trigger_orders += 1;
        }

        match params.side {
            OrderSide::Bid => {
                orderbook.total_bids += 1;
                if !is_trigger_order && params.price > orderbook.best_bid {
                    orderbook.best_bid = params.price;
                }
            }
            OrderSide::Ask => {
                orderbook.total_asks += 1;
                if !is_trigger_order && params.price < orderbook.best_ask {
                    orderbook.best_ask = params.price;
                }
            }
        }

        msg!("Order placed: {:?} {:?} {} @ {} (trigger: {})",
            params.order_type, params.side, params.size, params.price, params.trigger_price);
        Ok(())
    }

    /// Place OCO (One-Cancels-Other) order pair
    pub fn place_oco_order(ctx: Context<PlaceOcoOrder>, params: PlaceOcoParams) -> Result<()> {
        let orderbook = &mut ctx.accounts.orderbook;
        let primary_order = &mut ctx.accounts.primary_order;
        let secondary_order = &mut ctx.accounts.secondary_order;
        let current_time = Clock::get()?.unix_timestamp;

        require!(params.size > 0, ErrorCode::InvalidOrderSize);

        // Initialize primary order
        primary_order.owner = ctx.accounts.owner.key();
        primary_order.market = orderbook.market;
        primary_order.side = params.primary_side;
        primary_order.order_type = params.primary_type;
        primary_order.price = params.primary_price;
        primary_order.size = params.size;
        primary_order.filled_size = 0;
        primary_order.status = OrderStatus::Open;
        primary_order.created_at = current_time;
        primary_order.expires_at = params.expires_at;
        primary_order.trigger_price = params.primary_trigger_price;
        primary_order.trigger_condition = params.primary_trigger_condition;
        primary_order.trailing_amount = 0;
        primary_order.trailing_percent = false;
        primary_order.highest_price = 0;
        primary_order.lowest_price = u64::MAX;
        primary_order.linked_order = secondary_order.key();
        primary_order.is_oco = true;
        primary_order.position = Pubkey::default();
        primary_order.reduce_only = params.reduce_only;
        primary_order.sequence_number = orderbook.sequence_number;
        primary_order.bump = *ctx.bumps.get("primary_order").unwrap();

        // Initialize secondary order
        secondary_order.owner = ctx.accounts.owner.key();
        secondary_order.market = orderbook.market;
        secondary_order.side = params.secondary_side;
        secondary_order.order_type = params.secondary_type;
        secondary_order.price = params.secondary_price;
        secondary_order.size = params.size;
        secondary_order.filled_size = 0;
        secondary_order.status = OrderStatus::Open;
        secondary_order.created_at = current_time;
        secondary_order.expires_at = params.expires_at;
        secondary_order.trigger_price = params.secondary_trigger_price;
        secondary_order.trigger_condition = params.secondary_trigger_condition;
        secondary_order.trailing_amount = 0;
        secondary_order.trailing_percent = false;
        secondary_order.highest_price = 0;
        secondary_order.lowest_price = u64::MAX;
        secondary_order.linked_order = primary_order.key();
        secondary_order.is_oco = true;
        secondary_order.position = Pubkey::default();
        secondary_order.reduce_only = params.reduce_only;
        secondary_order.sequence_number = orderbook.sequence_number + 1;
        secondary_order.bump = *ctx.bumps.get("secondary_order").unwrap();

        orderbook.sequence_number += 2;
        orderbook.total_trigger_orders += 2;

        msg!("OCO order pair placed: primary={}, secondary={}",
            primary_order.key(), secondary_order.key());
        Ok(())
    }

    /// Trigger a conditional order (called by keeper when conditions are met)
    pub fn trigger_order(ctx: Context<TriggerOrder>, current_price: u64) -> Result<()> {
        let order = &mut ctx.accounts.order;
        let orderbook = &mut ctx.accounts.orderbook;

        require!(order.status == OrderStatus::Open, ErrorCode::InvalidOrder);
        require!(order.should_trigger(current_price), ErrorCode::TriggerConditionNotMet);

        // Mark as triggered - actual execution happens separately
        order.status = OrderStatus::Triggered;
        orderbook.total_trigger_orders = orderbook.total_trigger_orders.saturating_sub(1);

        msg!("Order triggered: {} at price {}", order.key(), current_price);
        Ok(())
    }

    /// Update trailing stop trigger price (called by keeper on price updates)
    pub fn update_trailing_stop(ctx: Context<UpdateTrailingStop>, current_price: u64) -> Result<()> {
        let order = &mut ctx.accounts.order;

        require!(order.status == OrderStatus::Open, ErrorCode::InvalidOrder);
        require!(order.order_type == OrderType::TrailingStop, ErrorCode::NotTrailingStop);

        order.update_trailing_stop(current_price);

        msg!("Trailing stop updated: trigger_price={}", order.trigger_price);
        Ok(())
    }

    /// Cancel an order
    pub fn cancel_order(ctx: Context<CancelOrder>) -> Result<()> {
        let orderbook = &mut ctx.accounts.orderbook;
        let order = &mut ctx.accounts.order;

        require!(
            order.status == OrderStatus::Open || order.status == OrderStatus::PartiallyFilled,
            ErrorCode::InvalidOrder
        );

        order.status = OrderStatus::Cancelled;

        // Update counts
        let is_trigger_order = order.trigger_condition != TriggerCondition::None;
        if is_trigger_order {
            orderbook.total_trigger_orders = orderbook.total_trigger_orders.saturating_sub(1);
        }

        match order.side {
            OrderSide::Bid => orderbook.total_bids = orderbook.total_bids.saturating_sub(1),
            OrderSide::Ask => orderbook.total_asks = orderbook.total_asks.saturating_sub(1),
        }

        msg!("Order cancelled: {}", order.key());
        Ok(())
    }

    /// Cancel OCO order pair (cancels both linked orders)
    pub fn cancel_oco_order(ctx: Context<CancelOcoOrder>) -> Result<()> {
        let orderbook = &mut ctx.accounts.orderbook;
        let primary_order = &mut ctx.accounts.primary_order;
        let linked_order = &mut ctx.accounts.linked_order;

        require!(primary_order.is_oco, ErrorCode::NotOcoOrder);
        require!(primary_order.linked_order == linked_order.key(), ErrorCode::InvalidLinkedOrder);

        // Cancel both orders
        if primary_order.status == OrderStatus::Open || primary_order.status == OrderStatus::PartiallyFilled {
            primary_order.status = OrderStatus::Cancelled;
            orderbook.total_trigger_orders = orderbook.total_trigger_orders.saturating_sub(1);
        }

        if linked_order.status == OrderStatus::Open || linked_order.status == OrderStatus::PartiallyFilled {
            linked_order.status = OrderStatus::Cancelled;
            orderbook.total_trigger_orders = orderbook.total_trigger_orders.saturating_sub(1);
        }

        msg!("OCO order pair cancelled: {}, {}", primary_order.key(), linked_order.key());
        Ok(())
    }

    /// Execute OCO cancellation when one order fills (called by keeper)
    pub fn execute_oco_cancel(ctx: Context<ExecuteOcoCancel>) -> Result<()> {
        let orderbook = &mut ctx.accounts.orderbook;
        let filled_order = &ctx.accounts.filled_order;
        let linked_order = &mut ctx.accounts.linked_order;

        require!(filled_order.is_oco, ErrorCode::NotOcoOrder);
        require!(filled_order.linked_order == linked_order.key(), ErrorCode::InvalidLinkedOrder);
        require!(
            filled_order.status == OrderStatus::Filled || filled_order.status == OrderStatus::Triggered,
            ErrorCode::OrderNotFilled
        );

        // Cancel the linked order
        if linked_order.status == OrderStatus::Open || linked_order.status == OrderStatus::PartiallyFilled {
            linked_order.status = OrderStatus::Cancelled;
            orderbook.total_trigger_orders = orderbook.total_trigger_orders.saturating_sub(1);
        }

        msg!("OCO linked order cancelled: {}", linked_order.key());
        Ok(())
    }

    /// Match orders
    pub fn match_orders(ctx: Context<MatchOrders>) -> Result<()> {
        let bid_order = &mut ctx.accounts.bid_order;
        let ask_order = &mut ctx.accounts.ask_order;

        require!(bid_order.price >= ask_order.price, ErrorCode::PriceMismatch);
        require!(
            bid_order.status == OrderStatus::Open
            || bid_order.status == OrderStatus::PartiallyFilled
            || bid_order.status == OrderStatus::Triggered,
            ErrorCode::InvalidOrder
        );
        require!(
            ask_order.status == OrderStatus::Open
            || ask_order.status == OrderStatus::PartiallyFilled
            || ask_order.status == OrderStatus::Triggered,
            ErrorCode::InvalidOrder
        );

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
pub struct PlaceOcoOrder<'info> {
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
    pub primary_order: Account<'info, Order>,

    #[account(
        init,
        payer = owner,
        space = Order::LEN,
        seeds = [b"order", owner.key().as_ref(), &(orderbook.sequence_number + 1).to_le_bytes()],
        bump
    )]
    pub secondary_order: Account<'info, Order>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct TriggerOrder<'info> {
    #[account(mut)]
    pub keeper: Signer<'info>,

    #[account(
        mut,
        seeds = [b"orderbook", orderbook.market.as_ref()],
        bump = orderbook.bump
    )]
    pub orderbook: Account<'info, OrderBook>,

    #[account(mut)]
    pub order: Account<'info, Order>,

    /// CHECK: Pyth price feed for price verification
    pub pyth_price_feed: AccountInfo<'info>,
}

#[derive(Accounts)]
pub struct UpdateTrailingStop<'info> {
    #[account(mut)]
    pub keeper: Signer<'info>,

    #[account(mut)]
    pub order: Account<'info, Order>,

    /// CHECK: Pyth price feed for price updates
    pub pyth_price_feed: AccountInfo<'info>,
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
pub struct CancelOcoOrder<'info> {
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
        constraint = primary_order.owner == owner.key()
    )]
    pub primary_order: Account<'info, Order>,

    #[account(mut)]
    pub linked_order: Account<'info, Order>,
}

#[derive(Accounts)]
pub struct ExecuteOcoCancel<'info> {
    #[account(mut)]
    pub keeper: Signer<'info>,

    #[account(
        mut,
        seeds = [b"orderbook", orderbook.market.as_ref()],
        bump = orderbook.bump
    )]
    pub orderbook: Account<'info, OrderBook>,

    pub filled_order: Account<'info, Order>,

    #[account(mut)]
    pub linked_order: Account<'info, Order>,
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
    #[msg("Invalid order size")]
    InvalidOrderSize,
    #[msg("Invalid price")]
    InvalidPrice,
    #[msg("Invalid trigger price")]
    InvalidTriggerPrice,
    #[msg("Trigger condition not met")]
    TriggerConditionNotMet,
    #[msg("Order is not a trailing stop")]
    NotTrailingStop,
    #[msg("Order is not an OCO order")]
    NotOcoOrder,
    #[msg("Invalid linked order")]
    InvalidLinkedOrder,
    #[msg("Order not filled")]
    OrderNotFilled,
}
