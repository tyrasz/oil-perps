use anchor_lang::prelude::*;
use crate::errors::PerpsError;
use crate::state::{Market, Venue, VenueApproval, MAX_APPROVAL_SECS, ROLE_MAKER};

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug)]
pub struct VenueApprovalParams {
    pub allowance: u64,
    pub max_leverage: u32,            // 1..=market.max_leverage
    pub max_buy_price: u64,           // 0 = none
    pub min_sell_price: u64,          // 0 = none
    pub max_price_deviation_bps: u16, // 0..=10_000
    pub expires_at: i64,              // now < expires_at <= now + MAX_APPROVAL_SECS
}

impl VenueApprovalParams {
    pub fn validate(&self, market: &Market, now: i64) -> bool {
        self.max_leverage > 0
            && self.max_leverage <= market.max_leverage
            && self.max_price_deviation_bps <= 10_000
            && self.expires_at > now
            && self.expires_at <= now.saturating_add(MAX_APPROVAL_SECS)
    }

    fn apply(&self, approval: &mut VenueApproval) {
        approval.allowance = self.allowance;
        approval.max_leverage = self.max_leverage;
        approval.max_buy_price = self.max_buy_price;
        approval.min_sell_price = self.min_sell_price;
        approval.max_price_deviation_bps = self.max_price_deviation_bps;
        approval.expires_at = self.expires_at;
    }
}

/// Consent to fills through `venue` in `role` (ROLE_TAKER or ROLE_MAKER).
#[derive(Accounts)]
#[instruction(role: u8)]
pub struct ApproveVenue<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,

    #[account(
        seeds = [b"market", market.collateral_mint.as_ref(), market.commodity_seed()],
        bump = market.bump
    )]
    pub market: Box<Account<'info, Market>>,

    #[account(
        seeds = [Venue::SEED, market.key().as_ref(), venue.program.as_ref()],
        bump = venue.bump
    )]
    pub venue: Box<Account<'info, Venue>>,

    #[account(
        init,
        payer = owner,
        space = VenueApproval::LEN,
        seeds = [VenueApproval::SEED, owner.key().as_ref(), venue.key().as_ref(), &[role]],
        bump
    )]
    pub approval: Box<Account<'info, VenueApproval>>,

    pub system_program: Program<'info, System>,
}

pub fn approve_venue(ctx: Context<ApproveVenue>, role: u8, params: VenueApprovalParams) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    require!(role <= ROLE_MAKER, PerpsError::InvalidApprovalParams);
    require!(params.validate(&ctx.accounts.market, now), PerpsError::InvalidApprovalParams);
    let approval = &mut ctx.accounts.approval;
    approval.owner = ctx.accounts.owner.key();
    approval.venue = ctx.accounts.venue.key();
    approval.market = ctx.accounts.venue.market;
    params.apply(approval);
    approval.fills = 0;
    approval.collateral_committed = 0;
    approval.role = role;
    approval.bump = *ctx.bumps.get("approval").unwrap();
    msg!("Venue approval: owner={}, venue={}, role={}, allowance={}", approval.owner, approval.venue, role, approval.allowance);
    Ok(())
}

/// Replace an approval's bounds, keeping its stats. Setting `allowance` to 0
/// stops every fill through the venue in one transaction.
#[derive(Accounts)]
pub struct UpdateVenueApproval<'info> {
    pub owner: Signer<'info>,

    #[account(
        seeds = [b"market", market.collateral_mint.as_ref(), market.commodity_seed()],
        bump = market.bump
    )]
    pub market: Box<Account<'info, Market>>,

    #[account(
        seeds = [Venue::SEED, market.key().as_ref(), venue.program.as_ref()],
        bump = venue.bump
    )]
    pub venue: Box<Account<'info, Venue>>,

    #[account(
        mut,
        seeds = [VenueApproval::SEED, owner.key().as_ref(), venue.key().as_ref(), &[approval.role]],
        bump = approval.bump,
        has_one = owner
    )]
    pub approval: Box<Account<'info, VenueApproval>>,
}

pub fn update_venue_approval(ctx: Context<UpdateVenueApproval>, params: VenueApprovalParams) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    require!(params.validate(&ctx.accounts.market, now), PerpsError::InvalidApprovalParams);
    let approval = &mut ctx.accounts.approval;
    params.apply(approval);
    msg!("Venue approval updated: owner={}, venue={}, role={}, allowance={}", approval.owner, approval.venue, approval.role, approval.allowance);
    Ok(())
}

/// Close an approval and refund its rent. Works whatever state the venue
/// program or its Venue record is in.
#[derive(Accounts)]
pub struct RevokeVenueApproval<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,

    #[account(
        mut,
        seeds = [VenueApproval::SEED, owner.key().as_ref(), approval.venue.as_ref(), &[approval.role]],
        bump = approval.bump,
        has_one = owner,
        close = owner
    )]
    pub approval: Box<Account<'info, VenueApproval>>,
}

pub fn revoke_venue_approval(ctx: Context<RevokeVenueApproval>) -> Result<()> {
    msg!("Venue approval revoked: owner={}, venue={}, role={}", ctx.accounts.owner.key(), ctx.accounts.approval.venue, ctx.accounts.approval.role);
    Ok(())
}
