use anchor_lang::prelude::*;
use anchor_lang::solana_program::bpf_loader_upgradeable;
use crate::errors::PerpsError;
use crate::state::{Market, ProtocolConfig, Venue, MAX_VENUE_BAND_BPS, MAX_VENUE_ORACLE_AGE_SECS};

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug)]
pub struct VenueParams {
    pub max_price_deviation_bps: u16, // 0..=MAX_VENUE_BAND_BPS
    pub max_oracle_age_secs: u16,     // 1..=60
    pub min_signers: u8,              // 0..=2
    pub max_fill_notional: u64,       // > 0
}

impl VenueParams {
    pub fn validate(&self) -> bool {
        self.max_price_deviation_bps <= MAX_VENUE_BAND_BPS
            && (1..=MAX_VENUE_ORACLE_AGE_SECS).contains(&self.max_oracle_age_secs)
            && self.min_signers <= 2
            && self.max_fill_notional > 0
    }

    fn apply(&self, venue: &mut Venue) {
        venue.max_price_deviation_bps = self.max_price_deviation_bps;
        venue.max_oracle_age_secs = self.max_oracle_age_secs;
        venue.min_signers = self.min_signers;
        venue.max_fill_notional = self.max_fill_notional;
    }
}

/// Admin: allow `venue_program` to open matched positions in `market`.
/// Must run after the venue's deploy, since it pins the deployed slot.
#[derive(Accounts)]
pub struct RegisterVenue<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,

    #[account(
        seeds = [ProtocolConfig::SEED],
        bump = config.bump,
        constraint = config.admin == admin.key() @ PerpsError::Unauthorized
    )]
    pub config: Box<Account<'info, ProtocolConfig>>,

    #[account(
        seeds = [b"market", market.collateral_mint.as_ref(), market.commodity_seed()],
        bump = market.bump
    )]
    pub market: Box<Account<'info, Market>>,

    /// CHECK: any executable program but perps-core itself
    #[account(constraint = venue_program.executable && venue_program.key() != crate::ID @ PerpsError::InvalidVenueParams)]
    pub venue_program: UncheckedAccount<'info>,

    #[account(
        seeds = [venue_program.key().as_ref()],
        bump,
        seeds::program = bpf_loader_upgradeable::ID
    )]
    pub venue_program_data: Box<Account<'info, ProgramData>>,

    #[account(
        init,
        payer = admin,
        space = Venue::LEN,
        seeds = [Venue::SEED, market.key().as_ref(), venue_program.key().as_ref()],
        bump
    )]
    pub venue: Box<Account<'info, Venue>>,

    pub system_program: Program<'info, System>,
}

pub fn register_venue(ctx: Context<RegisterVenue>, params: VenueParams, expected_program_slot: u64) -> Result<()> {
    let market = &ctx.accounts.market;
    require!(!market.is_settled(), PerpsError::MarketSettled);
    require!(params.validate(), PerpsError::InvalidVenueParams);
    // The admin states which deploy it audited; a later upgrade racing this
    // transaction is not silently attested.
    require!(ctx.accounts.venue_program_data.slot == expected_program_slot, PerpsError::InvalidVenueParams);

    let program = ctx.accounts.venue_program.key();
    let market_key = market.key();
    let venue = &mut ctx.accounts.venue;
    venue.market = market_key;
    venue.program = program;
    venue.authority = Pubkey::find_program_address(&[Venue::AUTHORITY_SEED, market_key.as_ref()], &program).0;
    venue.program_data = ctx.accounts.venue_program_data.key();
    venue.program_slot = expected_program_slot;
    params.apply(venue);
    venue.total_fills = 0;
    venue.total_notional = 0;
    venue.enabled = true;
    venue.bump = *ctx.bumps.get("venue").unwrap();
    msg!("Venue registered: program={}, market={}, authority={}, slot={}", program, market_key, venue.authority, venue.program_slot);
    Ok(())
}

/// Admin: change a venue's limits, enable or disable it, and re-pin its
/// program slot. After a venue upgrade this is the only way to resume fills.
#[derive(Accounts)]
pub struct UpdateVenue<'info> {
    pub admin: Signer<'info>,

    #[account(
        seeds = [ProtocolConfig::SEED],
        bump = config.bump,
        constraint = config.admin == admin.key() @ PerpsError::Unauthorized
    )]
    pub config: Box<Account<'info, ProtocolConfig>>,

    #[account(
        mut,
        seeds = [Venue::SEED, venue.market.as_ref(), venue.program.as_ref()],
        bump = venue.bump
    )]
    pub venue: Box<Account<'info, Venue>>,

    #[account(address = venue.program_data @ PerpsError::InvalidVenueParams)]
    pub venue_program_data: Box<Account<'info, ProgramData>>,
}

pub fn update_venue(ctx: Context<UpdateVenue>, params: VenueParams, enabled: bool, expected_program_slot: u64) -> Result<()> {
    require!(params.validate(), PerpsError::InvalidVenueParams);
    require!(ctx.accounts.venue_program_data.slot == expected_program_slot, PerpsError::InvalidVenueParams);
    let venue = &mut ctx.accounts.venue;
    params.apply(venue);
    venue.enabled = enabled;
    venue.program_slot = expected_program_slot;
    msg!("Venue updated: program={}, enabled={}, slot={}", venue.program, enabled, venue.program_slot);
    Ok(())
}

/// Admin or guardian: stop a venue's fills. Re-enabling is admin-only.
#[derive(Accounts)]
pub struct DisableVenue<'info> {
    pub signer: Signer<'info>,

    #[account(
        seeds = [ProtocolConfig::SEED],
        bump = config.bump,
        constraint = config.can_pause(&signer.key()) @ PerpsError::Unauthorized
    )]
    pub config: Box<Account<'info, ProtocolConfig>>,

    #[account(
        mut,
        seeds = [Venue::SEED, venue.market.as_ref(), venue.program.as_ref()],
        bump = venue.bump
    )]
    pub venue: Box<Account<'info, Venue>>,
}

pub fn disable_venue(ctx: Context<DisableVenue>) -> Result<()> {
    ctx.accounts.venue.enabled = false;
    msg!("Venue {} disabled by {}", ctx.accounts.venue.program, ctx.accounts.signer.key());
    Ok(())
}

/// Stop (admin or guardian) or resume (admin only) every venue fill.
#[derive(Accounts)]
pub struct SetVenueFillsPaused<'info> {
    pub signer: Signer<'info>,

    #[account(mut, seeds = [ProtocolConfig::SEED], bump = config.bump)]
    pub config: Box<Account<'info, ProtocolConfig>>,
}

pub fn set_venue_fills_paused(ctx: Context<SetVenueFillsPaused>, paused: bool) -> Result<()> {
    let signer = ctx.accounts.signer.key();
    let config = &mut ctx.accounts.config;
    let allowed = if paused { config.can_pause(&signer) } else { signer == config.admin };
    require!(allowed, PerpsError::Unauthorized);
    config.venue_fills_paused = paused;
    msg!("Venue fills paused={} by {}", paused, signer);
    Ok(())
}
