use anchor_lang::prelude::*;
use anchor_lang::solana_program::bpf_loader_upgradeable;
use crate::errors::PropAmmError;
use crate::state::ProtocolConfig;

/// One-time setup by the program's upgrade authority, who becomes the admin.
#[derive(Accounts)]
pub struct InitializeConfig<'info> {
    #[account(mut)]
    pub upgrade_authority: Signer<'info>,

    #[account(
        init,
        payer = upgrade_authority,
        space = ProtocolConfig::LEN,
        seeds = [ProtocolConfig::SEED],
        bump
    )]
    pub config: Account<'info, ProtocolConfig>,

    /// This program's ProgramData account (owned by the upgradeable loader).
    #[account(
        seeds = [crate::ID.as_ref()],
        bump,
        seeds::program = bpf_loader_upgradeable::ID,
        constraint = program_data.upgrade_authority_address == Some(upgrade_authority.key()) @ PropAmmError::Unauthorized
    )]
    pub program_data: Account<'info, ProgramData>,

    pub system_program: Program<'info, System>,
}

pub fn initialize_config(ctx: Context<InitializeConfig>, oracle_program: Pubkey) -> Result<()> {
    let config = &mut ctx.accounts.config;
    config.admin = ctx.accounts.upgrade_authority.key();
    config.pending_admin = Pubkey::default();
    config.oracle_program = oracle_program;
    config.bump = ctx.bumps.config;
    msg!("Protocol config: admin={}, oracle_program={}", config.admin, config.oracle_program);
    Ok(())
}

#[derive(Accounts)]
pub struct AdminOnly<'info> {
    pub admin: Signer<'info>,

    #[account(
        mut,
        seeds = [ProtocolConfig::SEED],
        bump = config.bump,
        constraint = config.admin == admin.key() @ PropAmmError::Unauthorized
    )]
    pub config: Account<'info, ProtocolConfig>,
}

/// Step 1 of an admin handover: the current admin names the next one.
pub fn propose_admin(ctx: Context<AdminOnly>, new_admin: Pubkey) -> Result<()> {
    ctx.accounts.config.pending_admin = new_admin;
    msg!("Admin handover proposed to {}", new_admin);
    Ok(())
}

/// Change the oracle program that the price feeds of vaults created from
/// now on must belong to. Existing vaults keep the owner they snapshotted
/// (`LpVault.oracle_program`, docs/lp-pool.md D52).
pub fn set_oracle_program(ctx: Context<AdminOnly>, oracle_program: Pubkey) -> Result<()> {
    ctx.accounts.config.oracle_program = oracle_program;
    msg!("Oracle program set to {}", oracle_program);
    Ok(())
}

#[derive(Accounts)]
pub struct AcceptAdmin<'info> {
    pub new_admin: Signer<'info>,

    #[account(
        mut,
        seeds = [ProtocolConfig::SEED],
        bump = config.bump,
        constraint = config.pending_admin == new_admin.key() @ PropAmmError::Unauthorized
    )]
    pub config: Account<'info, ProtocolConfig>,
}

/// Step 2: the proposed admin accepts, proving it controls the key.
pub fn accept_admin(ctx: Context<AcceptAdmin>) -> Result<()> {
    let config = &mut ctx.accounts.config;
    config.admin = ctx.accounts.new_admin.key();
    config.pending_admin = Pubkey::default();
    msg!("Admin is now {}", config.admin);
    Ok(())
}
