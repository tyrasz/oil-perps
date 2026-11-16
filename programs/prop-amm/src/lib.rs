use anchor_lang::prelude::*;

pub mod state;
pub mod instructions;
pub mod errors;

pub use state::*;
pub use instructions::*;
pub use errors::*;

declare_id!("PropAMM111111111111111111111111111111111111");

#[program]
pub mod prop_amm {
    use super::*;

    /// Initialize a new LP vault for a market
    pub fn initialize_vault(
        ctx: Context<InitializeVault>,
        params: InitializeVaultParams,
    ) -> Result<()> {
        instructions::initialize_vault::handler(ctx, params)
    }

    /// Deposit USDC into the vault and receive LP shares
    pub fn deposit(ctx: Context<Deposit>, amount: u64) -> Result<()> {
        instructions::deposit::handler(ctx, amount)
    }

    /// Request withdrawal (starts cooldown period)
    pub fn request_withdrawal(ctx: Context<RequestWithdrawal>) -> Result<()> {
        instructions::request_withdrawal::handler(ctx)
    }

    /// Execute withdrawal after cooldown period
    pub fn withdraw(ctx: Context<Withdraw>, shares_to_burn: u64) -> Result<()> {
        instructions::withdraw::handler(ctx, shares_to_burn)
    }

    /// Open a position against the AMM
    pub fn open_position(
        ctx: Context<OpenPositionAmm>,
        params: OpenPositionParams,
    ) -> Result<OpenPositionResult> {
        instructions::open_position::handler(ctx, params)
    }

    /// Close a position against the AMM
    pub fn close_position(
        ctx: Context<ClosePositionAmm>,
        params: ClosePositionParams,
    ) -> Result<ClosePositionResult> {
        instructions::close_position::handler(ctx, params)
    }
}
