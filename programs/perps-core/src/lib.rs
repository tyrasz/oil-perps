use anchor_lang::prelude::*;

pub mod errors;
pub mod oracle;
pub mod price;
pub mod session;
pub mod instructions;
pub mod state;

use instructions::*;

declare_id!("Fg6PaFpoGXkYsidMpWTK6W2BeZ7FEfcYkg476zPFsLnS");

#[program]
pub mod perps_core {
    use super::*;

    pub fn initialize_market(
        ctx: Context<InitializeMarket>,
        params: InitializeMarketParams,
    ) -> Result<()> {
        instructions::initialize_market::handler(ctx, params)
    }

    pub fn initialize_user(ctx: Context<InitializeUser>) -> Result<()> {
        instructions::initialize_user::handler(ctx)
    }

    pub fn deposit_collateral(ctx: Context<DepositCollateral>, amount: u64) -> Result<()> {
        instructions::deposit_collateral::handler(ctx, amount)
    }

    pub fn withdraw_collateral(ctx: Context<WithdrawCollateral>, amount: u64) -> Result<()> {
        instructions::withdraw_collateral::handler(ctx, amount)
    }

    pub fn open_position(ctx: Context<OpenPosition>, params: OpenPositionParams) -> Result<()> {
        instructions::open_position::handler(ctx, params)
    }

    pub fn close_position(ctx: Context<ClosePosition>) -> Result<()> {
        instructions::close_position::handler(ctx)
    }

    pub fn add_margin(ctx: Context<AddMargin>, amount: u64) -> Result<()> {
        instructions::add_margin::handler(ctx, amount)
    }

    pub fn liquidate(ctx: Context<Liquidate>) -> Result<()> {
        instructions::liquidate::handler(ctx)
    }

    pub fn update_funding(ctx: Context<UpdateFunding>) -> Result<()> {
        instructions::update_funding::handler(ctx)
    }

    // Admin: risk controls (trading hours, weekend margins, oracle limits)
    pub fn update_market_config(
        ctx: Context<UpdateMarketConfig>,
        params: MarketConfigParams,
    ) -> Result<()> {
        instructions::update_market_config::handler(ctx, params)
    }

    // Protocol roles: set up once by the upgrade authority (who becomes admin)
    pub fn initialize_config(ctx: Context<InitializeConfig>, params: ProtocolRolesParams) -> Result<()> {
        instructions::protocol_config::initialize_config(ctx, params)
    }

    pub fn propose_admin(ctx: Context<AdminOnly>, new_admin: Pubkey) -> Result<()> {
        instructions::protocol_config::propose_admin(ctx, new_admin)
    }

    pub fn accept_admin(ctx: Context<AcceptAdmin>) -> Result<()> {
        instructions::protocol_config::accept_admin(ctx)
    }

    pub fn update_roles(ctx: Context<AdminOnly>, params: ProtocolRolesParams) -> Result<()> {
        instructions::protocol_config::update_roles(ctx, params)
    }

    // Admin or guardian: stop new positions in a market
    pub fn pause_market(ctx: Context<PauseMarket>) -> Result<()> {
        instructions::pause_market::handler(ctx)
    }

    // Admin: final emergency settlement at a fixed price
    pub fn settle_market(ctx: Context<SettleMarket>, settlement_price: u64) -> Result<()> {
        instructions::settle_market::handler(ctx, settlement_price)
    }

    // Referral system instructions
    pub fn create_referral_code(
        ctx: Context<CreateReferralCode>,
        params: CreateReferralCodeParams,
    ) -> Result<()> {
        instructions::create_referral_code::handler(ctx, params)
    }

    pub fn apply_referral_code(ctx: Context<ApplyReferralCode>) -> Result<()> {
        instructions::apply_referral_code::handler(ctx)
    }
}
