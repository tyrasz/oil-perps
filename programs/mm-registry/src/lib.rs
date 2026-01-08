use anchor_lang::prelude::*;

pub mod state;
pub mod instructions;
pub mod errors;

pub use state::*;
pub use instructions::*;
pub use errors::*;

declare_id!("MMReg11111111111111111111111111111111111111");

#[program]
pub mod mm_registry {
    use super::*;

    /// Initialize the Market Maker Registry
    pub fn initialize_registry(
        ctx: Context<InitializeRegistry>,
        params: InitializeRegistryParams,
    ) -> Result<()> {
        instructions::initialize_registry::handler(ctx, params)
    }

    /// Register as a Market Maker
    pub fn register_mm(ctx: Context<RegisterMm>, initial_collateral: u64) -> Result<()> {
        instructions::register_mm::handler(ctx, initial_collateral)
    }

    /// Deposit additional collateral
    pub fn deposit_collateral(ctx: Context<DepositCollateral>, amount: u64) -> Result<()> {
        instructions::deposit_collateral::handler(ctx, amount)
    }

    /// Withdraw available collateral
    pub fn withdraw_collateral(ctx: Context<WithdrawCollateral>, amount: u64) -> Result<()> {
        instructions::withdraw_collateral::handler(ctx, amount)
    }

    /// Post a two-sided quote (bid and ask)
    pub fn post_quote(ctx: Context<PostQuote>, params: PostQuoteParams) -> Result<()> {
        instructions::post_quote::handler(ctx, params)
    }

    /// Update an existing quote
    pub fn update_quote(ctx: Context<UpdateQuote>, params: UpdateQuoteParams) -> Result<()> {
        instructions::update_quote::handler(ctx, params)
    }

    /// Cancel a quote
    pub fn cancel_quote(ctx: Context<CancelQuote>) -> Result<()> {
        instructions::cancel_quote::handler(ctx)
    }

    /// Fill a quote (called by order router or traders)
    pub fn fill_quote(
        ctx: Context<FillQuote>,
        params: FillQuoteParams,
    ) -> Result<FillQuoteResult> {
        instructions::fill_quote::handler(ctx, params)
    }

    /// Deregister as a Market Maker
    pub fn deregister_mm(ctx: Context<DeregisterMm>) -> Result<()> {
        instructions::deregister_mm::handler(ctx)
    }
}
