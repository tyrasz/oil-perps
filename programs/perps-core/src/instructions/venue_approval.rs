use anchor_lang::prelude::*;
use crate::errors::PerpsError;
use anchor_lang::solana_program::instruction::{get_stack_height, TRANSACTION_LEVEL_STACK_HEIGHT};
use anchor_lang::solana_program::sysvar::instructions::{self as ix_sysvar, load_current_index_checked, load_instruction_at_checked};
use crate::state::{Market, Venue, VenueApproval, MAX_APPROVAL_SECS, ROLE_MAKER, SIDE_LONG, SIDE_SHORT};

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug)]
pub struct VenueApprovalParams {
    pub allowance: u64,
    pub max_leverage: u32,            // 1..=market.max_leverage
    pub max_buy_price: u64,           // 0 = none
    pub min_sell_price: u64,          // 0 = none
    pub max_price_deviation_bps: u16, // 0..=10_000
    pub expires_at: i64,              // now < expires_at <= now + MAX_APPROVAL_SECS
    pub allowed_sides: u8,            // SIDE_LONG | SIDE_SHORT, non-zero
}

impl VenueApprovalParams {
    pub fn validate(&self, market: &Market, now: i64) -> bool {
        self.max_leverage > 0
            && self.max_leverage <= market.max_leverage
            && self.max_price_deviation_bps <= 10_000
            && self.expires_at > now
            && self.expires_at <= now.saturating_add(MAX_APPROVAL_SECS)
            && self.allowed_sides != 0
            && self.allowed_sides & !(SIDE_LONG | SIDE_SHORT) == 0
    }

    fn apply(&self, approval: &mut VenueApproval) {
        approval.allowance = self.allowance;
        approval.max_leverage = self.max_leverage;
        approval.max_buy_price = self.max_buy_price;
        approval.min_sell_price = self.min_sell_price;
        approval.max_price_deviation_bps = self.max_price_deviation_bps;
        approval.expires_at = self.expires_at;
        approval.allowed_sides = self.allowed_sides;
    }

    /// True if these params grant nothing `approval` doesn't already: always
    /// safe to apply, from anywhere (it is how an owner pauses its quotes).
    fn only_narrows(&self, approval: &VenueApproval) -> bool {
        // 0 means "no bound" for the limits and the deviation
        let tighter_cap = |new: u64, old: u64| old == 0 || (new != 0 && new <= old);
        self.allowance <= approval.allowance
            && self.max_leverage <= approval.max_leverage
            && self.allowed_sides & !approval.allowed_sides == 0
            && self.expires_at <= approval.expires_at
            && tighter_cap(self.max_buy_price, approval.max_buy_price)
            && (approval.min_sell_price == 0 || self.min_sell_price >= approval.min_sell_price)
            && tighter_cap(self.max_price_deviation_bps as u64, approval.max_price_deviation_bps as u64)
    }
}

/// Who may grant (create or widen) an approval for `venue`.
///
/// A taker signs the transaction that calls the venue, and a signer stays a
/// signer in every CPI, so a compromised venue could otherwise widen the
/// taker's approval mid-transaction and then fill against it. A grant must
/// therefore come either from a top-level instruction, or from exactly one
/// CPI deep under a top-level program that is not the venue: the owner's own
/// wallet program (a multisig vault, a PDA owner). The venue itself is then
/// the top-level program, or (behind a router) at least two CPIs deep.
/// Narrowing and revoking are always allowed.
fn require_grant_allowed(venue: &Venue, instructions: &AccountInfo) -> Result<()> {
    let height = get_stack_height();
    if height == TRANSACTION_LEVEL_STACK_HEIGHT {
        return Ok(());
    }
    require!(height == TRANSACTION_LEVEL_STACK_HEIGHT + 1, PerpsError::ApprovalViaCpi);
    let current = load_current_index_checked(instructions)? as usize;
    let top_level = load_instruction_at_checked(current, instructions)?;
    require!(top_level.program_id != venue.program, PerpsError::ApprovalViaCpi);
    Ok(())
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

    /// CHECK: the instructions sysvar (address-checked), for require_grant_allowed
    #[account(address = ix_sysvar::ID)]
    pub instructions: UncheckedAccount<'info>,
}

pub fn approve_venue(ctx: Context<ApproveVenue>, role: u8, params: VenueApprovalParams) -> Result<()> {
    require_grant_allowed(&ctx.accounts.venue, &ctx.accounts.instructions)?;
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

    /// CHECK: the instructions sysvar (address-checked), for require_grant_allowed
    #[account(address = ix_sysvar::ID)]
    pub instructions: UncheckedAccount<'info>,
}

pub fn update_venue_approval(ctx: Context<UpdateVenueApproval>, params: VenueApprovalParams) -> Result<()> {
    if !params.only_narrows(&ctx.accounts.approval) {
        require_grant_allowed(&ctx.accounts.venue, &ctx.accounts.instructions)?;
    }
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
