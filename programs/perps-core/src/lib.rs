use anchor_lang::prelude::*;

/// Anchor's `emit!`, which also hands the event to the test harness when the
/// program runs natively (in-process solana-program-test; never on chain).
///
/// Anchor 1.2's native `sol_log_data` drops the data, so a native run would
/// see no events. Natively this also logs "Program data: <base64>" through
/// the solana-sysvar 3 syscall stubs, which the tests forward to
/// program-test's log (test-sdk's native_stubs.rs), as native runs did before
/// Anchor 1.2. On SBF it is exactly `anchor_lang::emit!` (docs/lp-pool.md
/// D48). Defined before the modules so they all see it; every event in this
/// crate goes through it.
macro_rules! emit_event {
    ($event:expr) => {{
        let event = $event;
        #[cfg(not(target_os = "solana"))]
        {
            #[allow(deprecated)]
            let data = anchor_lang::__private::base64::encode(anchor_lang::Event::data(&event));
            anchor_lang::compat::solana_sysvar::program_stubs::sol_log(&format!("Program data: {}", data));
        }
        anchor_lang::emit!(event);
    }};
}

pub mod errors;
pub mod events;
pub mod oracle;
pub mod pool_math;
pub mod price;
pub mod pyth_push;
pub mod pyth_receiver;
pub mod session;
pub mod instructions;
pub mod state;

use instructions::*;
use state::PoolParams;

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

    pub fn close_position(ctx: Context<ClosePosition>, acceptable_price: u64) -> Result<()> {
        instructions::close_position::handler(ctx, acceptable_price)
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

    // Venues: programs the admin allows to open matched positions in a market
    pub fn register_venue(ctx: Context<RegisterVenue>, params: VenueParams, expected_upgrade_authority: Pubkey) -> Result<()> {
        instructions::venue_admin::register_venue(ctx, params, expected_upgrade_authority)
    }

    pub fn update_venue(ctx: Context<UpdateVenue>, params: VenueParams, enabled: bool, expected_upgrade_authority: Pubkey) -> Result<()> {
        instructions::venue_admin::update_venue(ctx, params, enabled, expected_upgrade_authority)
    }

    // Admin or guardian
    pub fn disable_venue(ctx: Context<DisableVenue>) -> Result<()> {
        instructions::venue_admin::disable_venue(ctx)
    }

    // Pausing: admin or guardian. Resuming: admin.
    pub fn set_venue_fills_paused(ctx: Context<SetVenueFillsPaused>, paused: bool) -> Result<()> {
        instructions::venue_admin::set_venue_fills_paused(ctx, paused)
    }

    // Users: bound what a venue may do with their collateral, per role
    pub fn approve_venue(ctx: Context<ApproveVenue>, role: u8, params: VenueApprovalParams) -> Result<()> {
        instructions::venue_approval::approve_venue(ctx, role, params)
    }

    pub fn update_venue_approval(ctx: Context<UpdateVenueApproval>, params: VenueApprovalParams) -> Result<()> {
        instructions::venue_approval::update_venue_approval(ctx, params)
    }

    pub fn revoke_venue_approval(ctx: Context<RevokeVenueApproval>) -> Result<()> {
        instructions::venue_approval::revoke_venue_approval(ctx)
    }

    // CPI from a registered venue only
    pub fn venue_fill(ctx: Context<VenueFill>, params: VenueFillParams) -> Result<()> {
        instructions::venue_fill::handler(ctx, params)
    }

    // LP pool (docs/lp-pool.md §5, §9, §10). Admin: create the pool and set
    // its parameters (timelocked once LPs exist), the cap and the ADL keeper.
    pub fn initialize_pool(ctx: Context<InitializePool>, params: PoolParams, adl_keeper: Pubkey) -> Result<()> {
        instructions::pool_admin::initialize_pool(ctx, params, adl_keeper)
    }

    pub fn propose_pool_params(ctx: Context<ProposePoolParams>, params: PoolParams) -> Result<()> {
        instructions::pool_admin::propose_pool_params(ctx, params)
    }

    // Anyone, once the timelock has passed
    pub fn apply_pool_params(ctx: Context<ApplyPoolParams>) -> Result<()> {
        instructions::pool_admin::apply_pool_params(ctx)
    }

    pub fn cancel_pool_params(ctx: Context<PoolAdminOnly>) -> Result<()> {
        instructions::pool_admin::cancel_pool_params(ctx)
    }

    pub fn set_max_pool_cash(ctx: Context<PoolAdminOnly>, cap: u64) -> Result<()> {
        instructions::pool_admin::set_max_pool_cash(ctx, cap)
    }

    pub fn set_adl_keeper(ctx: Context<PoolAdminOnly>, key: Pubkey) -> Result<()> {
        instructions::pool_admin::set_adl_keeper(ctx, key)
    }

    pub fn withdraw_treasury(ctx: Context<WithdrawTreasury>, amount: u64) -> Result<()> {
        instructions::pool_admin::withdraw_treasury(ctx, amount)
    }

    // LPs
    pub fn init_lp_position(ctx: Context<InitLpPosition>, class: u8) -> Result<()> {
        instructions::lp_pool::init_lp_position(ctx, class)
    }

    pub fn lp_deposit(ctx: Context<LpDeposit>, amount: u64, min_shares_out: u64) -> Result<()> {
        instructions::lp_pool::lp_deposit(ctx, amount, min_shares_out)
    }

    pub fn lp_request_withdrawal(ctx: Context<LpRequestWithdrawal>, shares: u64) -> Result<()> {
        instructions::lp_pool::lp_request_withdrawal(ctx, shares)
    }

    pub fn lp_cancel_withdrawal(ctx: Context<LpCancelWithdrawal>) -> Result<()> {
        instructions::lp_pool::lp_cancel_withdrawal(ctx)
    }

    pub fn lp_withdraw(ctx: Context<LpWithdraw>) -> Result<()> {
        instructions::lp_pool::lp_withdraw(ctx)
    }

    // Permissionless
    pub fn donate_to_pool(ctx: Context<DonateToPool>, amount: u64) -> Result<()> {
        instructions::lp_pool::donate_to_pool(ctx, amount)
    }

    pub fn sweep_insurance_excess(ctx: Context<SweepInsuranceExcess>) -> Result<()> {
        instructions::lp_pool::sweep_insurance_excess(ctx)
    }

    pub fn close_settled_position(ctx: Context<CloseSettledPosition>) -> Result<()> {
        instructions::lp_pool::close_settled_position(ctx)
    }

    // Moves a losing position's unrealized loss into pool cash (D3)
    pub fn settle_position_pnl(ctx: Context<SettlePositionPnl>) -> Result<()> {
        instructions::adl::settle_position_pnl(ctx)
    }

    // Mode 0 (deficit): anyone. Mode 1 (exposure): the ADL keeper or admin.
    pub fn adl(ctx: Context<AdlPosition>, mode: u8, size: u64) -> Result<()> {
        instructions::adl::adl(ctx, mode, size)
    }

    pub fn write_off_bad_debt(ctx: Context<WriteOffBadDebt>) -> Result<()> {
        instructions::adl::write_off_bad_debt(ctx)
    }

    // Oracle feed rotation (docs/lp-pool.md D47). Admin proposes and
    // cancels; anyone applies once the timelock has passed.
    pub fn propose_oracle_feed(ctx: Context<ProposeOracleFeed>, feed_id: [u8; 32]) -> Result<()> {
        instructions::oracle_admin::propose_oracle_feed(ctx, feed_id)
    }

    pub fn apply_oracle_feed(ctx: Context<ApplyOracleFeed>) -> Result<()> {
        instructions::oracle_admin::apply_oracle_feed(ctx)
    }

    pub fn cancel_oracle_feed(ctx: Context<CancelOracleFeed>) -> Result<()> {
        instructions::oracle_admin::cancel_oracle_feed(ctx)
    }
}
