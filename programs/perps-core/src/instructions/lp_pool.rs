//! LP flows, donations, the insurance sweep and settled-market closes
//! (docs/lp-pool.md §9, §10.1, §10.4, §10.5).

use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Transfer};
use crate::errors::PerpsError;
use crate::events::{
    InsuranceSwept, LpDeposited, LpWithdrawalCancelled, LpWithdrawalRequested, LpWithdrawn, PoolDonated, PositionClosed,
};
use crate::instructions::settle_close::settle_close;
use crate::oracle::read_price;
use crate::pool_math;
use crate::session;
use crate::state::{
    add_vesting, assert_backed, drawdown_now, pool_view, recover_pending, touch_peak, LpPool, LpPosition, Market, PoolParams,
    Position, PositionStatus, UserAccount, Vault, LP_CLASS_COMMON, LP_DEAD_SHARES,
};

/// Reopen quiet time `Q` (D17): within `lp_reopen_quiet_secs` of a session open.
fn in_quiet(m: &Market, p: &PoolParams, now: i64) -> bool {
    session::seconds_since_open(m.session_schedule, now).map_or(false, |t| t < p.lp_reopen_quiet_secs as i64)
}

/// The pool's pre-close window `W` (`weekend_cap_window_secs`).
fn in_window(m: &Market, p: &PoolParams, now: i64) -> bool {
    session::in_pre_close_window(m.session_schedule, now, p.weekend_cap_window_secs)
}

/// Mark for a withdrawal request: the settlement price once settled,
/// otherwise in session, a fresh oracle and funding current (§9.2 step 2).
#[inline(never)]
fn request_mark(feed: &AccountInfo, m: &Market, now: i64) -> Result<u64> {
    if m.is_settled() {
        return Ok(m.settlement_price);
    }
    require!(m.is_in_session(now), PerpsError::MarketClosed);
    let mark = read_price(feed, m, now)?;
    require!(m.funding_current(now), PerpsError::FundingStale);
    Ok(mark)
}

/// Mark for `lp_withdraw` (§9.4 step 3): settled → settlement price, once
/// the settled quiet time has passed (D41); otherwise in session, a fresh
/// oracle, outside `W` and `Q`, funding current.
#[inline(never)]
fn withdraw_mark(feed: &AccountInfo, m: &Market, p: &PoolParams, now: i64) -> Result<u64> {
    if m.is_settled() {
        require!(
            !pool_math::settled_withdraw_quiet(m.settled_at, now, p.profit_close_quiet_secs, p.lp_reopen_quiet_secs),
            PerpsError::LpFlowWindowClosed
        );
        return Ok(m.settlement_price);
    }
    require!(m.is_in_session(now), PerpsError::MarketClosed);
    let mark = read_price(feed, m, now)?;
    require!(!in_window(m, p, now) && !in_quiet(m, p, now), PerpsError::LpFlowWindowClosed);
    require!(m.funding_current(now), PerpsError::FundingStale);
    Ok(mark)
}

/// Transfer `amount` from `from` (signed by `authority`) into the vault.
fn transfer_in<'info>(
    token_program: AccountInfo<'info>,
    from: AccountInfo<'info>,
    to: AccountInfo<'info>,
    authority: AccountInfo<'info>,
    amount: u64,
) -> Result<()> {
    let cpi = CpiContext::new(token_program, Transfer { from, to, authority });
    token::transfer(cpi, amount)
}

// ---------------------------------------------------------------------------
// init_lp_position(class)
// ---------------------------------------------------------------------------

#[derive(Accounts)]
#[instruction(class: u8)]
pub struct InitLpPosition<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,

    #[account(seeds = [LpPool::SEED, lp_pool.market.as_ref()], bump = lp_pool.bump)]
    pub lp_pool: Box<Account<'info, LpPool>>,

    #[account(
        init,
        payer = owner,
        space = LpPosition::LEN,
        seeds = [LpPosition::SEED, lp_pool.key().as_ref(), owner.key().as_ref(), &[class]],
        bump
    )]
    pub lp_position: Box<Account<'info, LpPosition>>,

    pub system_program: Program<'info, System>,
}

pub fn init_lp_position(ctx: Context<InitLpPosition>, class: u8) -> Result<()> {
    require!(class == LP_CLASS_COMMON, PerpsError::InvalidLpClass);
    let pos = &mut ctx.accounts.lp_position;
    pos.owner = ctx.accounts.owner.key();
    pos.pool = ctx.accounts.lp_pool.key();
    pos.class = class;
    pos.bump = *ctx.bumps.get("lp_position").unwrap();
    Ok(())
}

// ---------------------------------------------------------------------------
// lp_deposit(amount, min_shares_out) (§9.1)
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct LpDeposit<'info> {
    pub owner: Signer<'info>,

    #[account(
        seeds = [b"market", market.collateral_mint.as_ref(), market.commodity_seed()],
        bump = market.bump
    )]
    pub market: Box<Account<'info, Market>>,

    #[account(mut, seeds = [LpPool::SEED, market.key().as_ref()], bump = lp_pool.bump)]
    pub lp_pool: Box<Account<'info, LpPool>>,

    #[account(
        mut,
        seeds = [LpPosition::SEED, lp_pool.key().as_ref(), owner.key().as_ref(), &[lp_position.class]],
        bump = lp_position.bump,
        has_one = owner
    )]
    pub lp_position: Box<Account<'info, LpPosition>>,

    #[account(mut, seeds = [b"vault_token", market.key().as_ref()], bump)]
    pub vault_token_account: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        constraint = owner_token_account.owner == owner.key(),
        constraint = owner_token_account.mint == market.collateral_mint
    )]
    pub owner_token_account: Box<Account<'info, TokenAccount>>,

    /// CHECK: Pyth price feed, validated by read_price
    #[account(constraint = pyth_price_feed.key() == market.pyth_price_feed)]
    pub pyth_price_feed: AccountInfo<'info>,

    pub token_program: Program<'info, Token>,
}

pub fn lp_deposit(ctx: Context<LpDeposit>, amount: u64, min_shares_out: u64) -> Result<()> {
    require!(amount > 0, PerpsError::InvalidLpAmount);
    let now = Clock::get()?.unix_timestamp;
    let m = &ctx.accounts.market;
    require!(!m.is_paused, PerpsError::MarketPaused);
    require!(m.is_in_session(now), PerpsError::MarketClosed);
    let mark = read_price(&ctx.accounts.pyth_price_feed, m, now)?;

    let pool = &mut ctx.accounts.lp_pool;
    require!(!in_quiet(m, &pool.params, now), PerpsError::LpFlowWindowClosed);
    require!(m.funding_current(now), PerpsError::FundingStale);
    require!(!pool.adl_required(), PerpsError::PoolAdlPending);

    let v = pool_view(pool, m, mark, now, pool.params.redeem_loss_haircut_bps)?;
    touch_peak(pool, &v, now);

    let (minted, owner_shares) = if pool.lp_shares == 0 {
        // First deposit (D11): LP_DEAD_SHARES are locked for ever
        require!(pool.cash == 0 && pool.open_positions == 0, PerpsError::MarketNotFresh);
        require!(amount > LP_DEAD_SHARES, PerpsError::InvalidLpAmount);
        (amount, amount - LP_DEAD_SHARES)
    } else {
        require!(v.nav_mint > 0, PerpsError::PoolInsolvent);
        let s = pool_math::shares_for_deposit(amount, pool.lp_shares, v.nav_mint).ok_or(PerpsError::MathOverflow)?;
        (s, s)
    };
    if pool.max_pool_cash != 0 {
        require!(
            v.nav_mint.max(0) as u128 + amount as u128 <= pool.max_pool_cash as u128,
            PerpsError::PoolCashCapExceeded
        );
    }
    require!(owner_shares > 0 && owner_shares >= min_shares_out, PerpsError::SlippageExceeded);

    let vt_before = ctx.accounts.vault_token_account.amount;
    transfer_in(
        ctx.accounts.token_program.to_account_info(),
        ctx.accounts.owner_token_account.to_account_info(),
        ctx.accounts.vault_token_account.to_account_info(),
        ctx.accounts.owner.to_account_info(),
        amount,
    )?;

    pool.cash = pool.cash.checked_add(amount).ok_or(PerpsError::MathOverflow)?;
    pool.lp_shares = pool.lp_shares.checked_add(minted).ok_or(PerpsError::MathOverflow)?;
    let pos = &mut ctx.accounts.lp_position;
    pos.shares = pos.shares.checked_add(owner_shares).ok_or(PerpsError::MathOverflow)?;
    pos.deposited_cum = pos.deposited_cum.saturating_add(amount);

    let after = vt_before.checked_add(amount).ok_or(PerpsError::MathOverflow)?;
    assert_backed(pool, m, after)?;
    emit!(LpDeposited { owner: pos.owner, amount, shares: owner_shares, nav_mint: v.nav_mint, lp_shares: pool.lp_shares });
    Ok(())
}

// ---------------------------------------------------------------------------
// lp_request_withdrawal(shares) (§9.2)
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct LpRequestWithdrawal<'info> {
    pub owner: Signer<'info>,

    #[account(
        seeds = [b"market", market.collateral_mint.as_ref(), market.commodity_seed()],
        bump = market.bump
    )]
    pub market: Box<Account<'info, Market>>,

    #[account(mut, seeds = [LpPool::SEED, market.key().as_ref()], bump = lp_pool.bump)]
    pub lp_pool: Box<Account<'info, LpPool>>,

    #[account(
        mut,
        seeds = [LpPosition::SEED, lp_pool.key().as_ref(), owner.key().as_ref(), &[lp_position.class]],
        bump = lp_position.bump,
        has_one = owner
    )]
    pub lp_position: Box<Account<'info, LpPosition>>,

    /// CHECK: Pyth price feed, validated by read_price (unused once settled)
    #[account(constraint = pyth_price_feed.key() == market.pyth_price_feed)]
    pub pyth_price_feed: AccountInfo<'info>,
}

/// Starts the delay; never gated by ADL, drawdown or the windows. Pending
/// shares stop counting toward `capNAV` but still count toward `B`.
pub fn lp_request_withdrawal(ctx: Context<LpRequestWithdrawal>, shares: u64) -> Result<()> {
    let pos = &mut ctx.accounts.lp_position;
    let free = pos.shares.saturating_sub(pos.pending_shares);
    require!(shares > 0 && shares <= free, PerpsError::InsufficientShares);
    require!(pos.pending_shares == 0, PerpsError::WithdrawalPending);

    let now = Clock::get()?.unix_timestamp;
    let m = &ctx.accounts.market;
    let mark = request_mark(&ctx.accounts.pyth_price_feed, m, now)?;

    let pool = &mut ctx.accounts.lp_pool;
    let v = pool_view(pool, m, mark, now, pool.params.redeem_loss_haircut_bps)?;
    touch_peak(pool, &v, now);
    let requested_value = pool_math::redeem_value(shares, pool.lp_shares, v.nav_redeem);

    let unlock_at = now
        .checked_add(pool.params.withdrawal_delay_secs as i64)
        .ok_or(PerpsError::MathOverflow)?;
    pos.pending_shares = shares;
    pos.requested_value = requested_value;
    pos.requested_at = now;
    pos.unlock_at = unlock_at;
    pos.claim_window_secs = pool.params.withdrawal_claim_window_secs;
    pool.pending_withdraw_shares = pool.pending_withdraw_shares.checked_add(shares).ok_or(PerpsError::MathOverflow)?;

    emit!(LpWithdrawalRequested {
        owner: pos.owner,
        shares,
        value: requested_value,
        nav_redeem: v.nav_redeem,
        lp_shares: pool.lp_shares,
        unlock_at,
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// lp_cancel_withdrawal() (§9.3)
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct LpCancelWithdrawal<'info> {
    pub owner: Signer<'info>,

    #[account(mut, seeds = [LpPool::SEED, lp_pool.market.as_ref()], bump = lp_pool.bump)]
    pub lp_pool: Box<Account<'info, LpPool>>,

    #[account(
        mut,
        seeds = [LpPosition::SEED, lp_pool.key().as_ref(), owner.key().as_ref(), &[lp_position.class]],
        bump = lp_position.bump,
        has_one = owner
    )]
    pub lp_position: Box<Account<'info, LpPosition>>,
}

fn clear_request(pos: &mut LpPosition) {
    pos.pending_shares = 0;
    pos.requested_value = 0;
    pos.requested_at = 0;
    pos.unlock_at = 0;
    pos.claim_window_secs = 0;
}

/// Needs no price; works on a lapsed request.
pub fn lp_cancel_withdrawal(ctx: Context<LpCancelWithdrawal>) -> Result<()> {
    let pos = &mut ctx.accounts.lp_position;
    require!(pos.pending_shares > 0, PerpsError::NoWithdrawalPending);
    let pool = &mut ctx.accounts.lp_pool;
    let shares = pos.pending_shares;
    pool.pending_withdraw_shares = pool.pending_withdraw_shares.checked_sub(shares).ok_or(PerpsError::BucketUnderflow)?;
    clear_request(pos);
    emit!(LpWithdrawalCancelled { owner: pos.owner, shares, lp_shares: pool.lp_shares });
    Ok(())
}

// ---------------------------------------------------------------------------
// lp_withdraw() (§9.4)
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct LpWithdraw<'info> {
    pub owner: Signer<'info>,

    #[account(
        seeds = [b"market", market.collateral_mint.as_ref(), market.commodity_seed()],
        bump = market.bump
    )]
    pub market: Box<Account<'info, Market>>,

    #[account(mut, seeds = [LpPool::SEED, market.key().as_ref()], bump = lp_pool.bump)]
    pub lp_pool: Box<Account<'info, LpPool>>,

    #[account(
        mut,
        seeds = [LpPosition::SEED, lp_pool.key().as_ref(), owner.key().as_ref(), &[lp_position.class]],
        bump = lp_position.bump,
        has_one = owner
    )]
    pub lp_position: Box<Account<'info, LpPosition>>,

    #[account(seeds = [b"vault", market.key().as_ref()], bump = vault.bump)]
    pub vault: Box<Account<'info, Vault>>,

    #[account(mut, seeds = [b"vault_token", market.key().as_ref()], bump)]
    pub vault_token_account: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        constraint = owner_token_account.owner == owner.key(),
        constraint = owner_token_account.mint == market.collateral_mint
    )]
    pub owner_token_account: Box<Account<'info, TokenAccount>>,

    /// CHECK: Pyth price feed, validated by read_price (unused once settled)
    #[account(constraint = pyth_price_feed.key() == market.pyth_price_feed)]
    pub pyth_price_feed: AccountInfo<'info>,

    pub token_program: Program<'info, Token>,
}

pub fn lp_withdraw(ctx: Context<LpWithdraw>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let m = &ctx.accounts.market;
    let settled = m.is_settled();
    let pos = &mut ctx.accounts.lp_position;

    // 1–2. Request, delay and claim window
    require!(pos.pending_shares > 0, PerpsError::NoWithdrawalPending);
    require!(now >= pos.unlock_at, PerpsError::WithdrawalDelayNotElapsed);
    if !settled && pos.claim_window_secs > 0 {
        let p = &ctx.accounts.lp_pool.params;
        let window = pool_math::effective_claim_window_secs(m.session_schedule, pos.claim_window_secs, p.weekend_cap_window_secs, p.lp_reopen_quiet_secs);
        let open = session::open_seconds_between(m.session_schedule, pos.unlock_at, now);
        require!(open as u64 <= window, PerpsError::WithdrawalExpired);
    }

    // 3–5. Mark, gates, valuation
    let pool = &mut ctx.accounts.lp_pool;
    let mark = withdraw_mark(&ctx.accounts.pyth_price_feed, m, &pool.params, now)?;
    require!(!pool.adl_required(), PerpsError::PoolAdlPending);
    let v = pool_view(pool, m, mark, now, pool.params.redeem_loss_haircut_bps)?;
    touch_peak(pool, &v, now);
    if !settled {
        require!(
            !pool_math::dd_withdraw_blocked(drawdown_now(pool, &v), pool.params.dd_withdraw_block_bps),
            PerpsError::PoolWithdrawalsBlocked
        );
    }

    // 6–8. Value: losses during the delay, no gains
    let pending = pos.pending_shares;
    let value = pool_math::redeem_value(pending, pool.lp_shares, v.nav_redeem).min(pos.requested_value);
    require!(value <= pool.cash, PerpsError::PoolCashInsufficient);
    if !settled {
        let b_after = (v.nav_redeem - value as i128).max(0) as u128;
        require!(
            pool_math::hard_net_ok(v.net_notional, pool.params.hard_net_bps, b_after),
            PerpsError::PoolCapWouldBreach
        );
        // The capital behind the net cannot leave below the weekend bound
        // (D43): G·net_notional ≤ α·B' with the bound on
        require!(
            !pool.params.weekend_bound_on()
                || pool_math::weekend_cap_ok(v.net_notional, pool.params.g_bps(m), pool.params.weekend_lp_loss_bps, b_after),
            PerpsError::PoolCapWouldBreach
        );
    }

    // 9. Transfer, signed by the vault PDA
    let vt_before = ctx.accounts.vault_token_account.amount;
    let market_key = m.key();
    let vault_seeds = &[b"vault".as_ref(), market_key.as_ref(), &[ctx.accounts.vault.bump]];
    let signer = &[&vault_seeds[..]];
    let cpi = CpiContext::new_with_signer(
        ctx.accounts.token_program.to_account_info(),
        Transfer {
            from: ctx.accounts.vault_token_account.to_account_info(),
            to: ctx.accounts.owner_token_account.to_account_info(),
            authority: ctx.accounts.vault.to_account_info(),
        },
        signer,
    );
    token::transfer(cpi, value)?;

    // 10. Effects
    pool.cash = pool.cash.checked_sub(value).ok_or(PerpsError::BucketUnderflow)?;
    pool.lp_shares = pool.lp_shares.checked_sub(pending).ok_or(PerpsError::BucketUnderflow)?;
    pool.pending_withdraw_shares = pool.pending_withdraw_shares.checked_sub(pending).ok_or(PerpsError::BucketUnderflow)?;
    pos.shares = pos.shares.checked_sub(pending).ok_or(PerpsError::BucketUnderflow)?;
    pos.withdrawn_cum = pos.withdrawn_cum.saturating_add(value);
    clear_request(pos);

    // 11.
    let after = vt_before.checked_sub(value).ok_or(PerpsError::VaultInvariantViolated)?;
    assert_backed(pool, m, after)?;
    emit!(LpWithdrawn { owner: pos.owner, shares: pending, value, nav_redeem: v.nav_redeem, lp_shares: pool.lp_shares });
    Ok(())
}

// ---------------------------------------------------------------------------
// donate_to_pool(amount) (§10.4)
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct DonateToPool<'info> {
    pub donor: Signer<'info>,

    #[account(
        seeds = [b"market", market.collateral_mint.as_ref(), market.commodity_seed()],
        bump = market.bump
    )]
    pub market: Box<Account<'info, Market>>,

    #[account(mut, seeds = [LpPool::SEED, market.key().as_ref()], bump = lp_pool.bump)]
    pub lp_pool: Box<Account<'info, LpPool>>,

    #[account(mut, seeds = [b"vault_token", market.key().as_ref()], bump)]
    pub vault_token_account: Box<Account<'info, TokenAccount>>,

    #[account(mut, constraint = donor_token_account.mint == market.collateral_mint)]
    pub donor_token_account: Box<Account<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,
}

/// The recapitalisation path: pays pending bad debt first, vests the rest.
/// No session or oracle gate, and allowed while `adl_required()`.
pub fn donate_to_pool(ctx: Context<DonateToPool>, amount: u64) -> Result<()> {
    require!(amount > 0, PerpsError::InvalidLpAmount);
    require!(ctx.accounts.lp_pool.lp_shares > 0, PerpsError::PoolHasNoLps);
    let now = Clock::get()?.unix_timestamp;

    let vt_before = ctx.accounts.vault_token_account.amount;
    transfer_in(
        ctx.accounts.token_program.to_account_info(),
        ctx.accounts.donor_token_account.to_account_info(),
        ctx.accounts.vault_token_account.to_account_info(),
        ctx.accounts.donor.to_account_info(),
        amount,
    )?;

    let pool = &mut ctx.accounts.lp_pool;
    pool.cash = pool.cash.checked_add(amount).ok_or(PerpsError::MathOverflow)?;
    pool.donated_cum = pool.donated_cum.saturating_add(amount);
    let recovered = recover_pending(pool, amount);
    let vested = amount - recovered;
    add_vesting(pool, vested, now)?;

    let after = vt_before.checked_add(amount).ok_or(PerpsError::MathOverflow)?;
    assert_backed(pool, &ctx.accounts.market, after)?;
    emit!(PoolDonated { donor: ctx.accounts.donor.key(), amount, recovered, vested });
    Ok(())
}

// ---------------------------------------------------------------------------
// sweep_insurance_excess() (§10.1)
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct SweepInsuranceExcess<'info> {
    pub signer: Signer<'info>,

    #[account(
        mut,
        seeds = [b"market", market.collateral_mint.as_ref(), market.commodity_seed()],
        bump = market.bump
    )]
    pub market: Box<Account<'info, Market>>,

    #[account(mut, seeds = [LpPool::SEED, market.key().as_ref()], bump = lp_pool.bump)]
    pub lp_pool: Box<Account<'info, LpPool>>,
}

/// Insurance above `insurance_target` (or, settled with no open positions,
/// the whole fund) moves to LP cash: pending bad debt first, the rest vests.
pub fn sweep_insurance_excess(ctx: Context<SweepInsuranceExcess>) -> Result<()> {
    let m = &mut ctx.accounts.market;
    let pool = &mut ctx.accounts.lp_pool;
    require!(pool.lp_shares > 0, PerpsError::PoolHasNoLps);
    let x = if m.is_settled() && pool.open_positions == 0 {
        m.insurance_fund
    } else {
        m.insurance_fund.saturating_sub(pool.params.insurance_target)
    };
    require!(x > 0, PerpsError::NothingToSweep);
    let now = Clock::get()?.unix_timestamp;

    m.insurance_fund = m.insurance_fund.checked_sub(x).ok_or(PerpsError::BucketUnderflow)?;
    pool.cash = pool.cash.checked_add(x).ok_or(PerpsError::MathOverflow)?;
    let recovered = recover_pending(pool, x);
    let vested = x - recovered;
    add_vesting(pool, vested, now)?;

    emit!(InsuranceSwept { amount: x, recovered, vested });
    Ok(())
}

// ---------------------------------------------------------------------------
// close_settled_position() (§10.5, D19)
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct CloseSettledPosition<'info> {
    pub signer: Signer<'info>,

    #[account(
        mut,
        seeds = [b"market", market.collateral_mint.as_ref(), market.commodity_seed()],
        bump = market.bump,
        constraint = market.is_settled() @ PerpsError::MarketNotSettled
    )]
    pub market: Box<Account<'info, Market>>,

    #[account(mut, seeds = [LpPool::SEED, market.key().as_ref()], bump = lp_pool.bump)]
    pub lp_pool: Box<Account<'info, LpPool>>,

    #[account(
        mut,
        constraint = position.market == market.key(),
        constraint = position.status == PositionStatus::Open @ PerpsError::PositionAlreadyClosed
    )]
    pub position: Box<Account<'info, Position>>,

    /// CHECK: only its address is used, to derive the owner's UserAccount
    #[account(address = position.owner)]
    pub position_owner: UncheckedAccount<'info>,

    #[account(
        mut,
        seeds = [b"user", position_owner.key().as_ref(), market.key().as_ref()],
        bump = user_account.bump,
        constraint = user_account.owner == position_owner.key()
    )]
    pub user_account: Box<Account<'info, UserAccount>>,
}

/// Anyone may close any position of a settled market at the settlement
/// price. The equity goes to the owner's free collateral, so the bucket step
/// is zero-sum and no token account is needed; a deficit goes through the
/// waterfall; a winner is paid from cash (close losers first).
pub fn close_settled_position(ctx: Context<CloseSettledPosition>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let position_key = ctx.accounts.position.key();
    let a = ctx.accounts;
    let price = a.market.settlement_price;
    let o = settle_close(&mut a.market, &mut a.lp_pool, &mut a.position, &mut a.user_account, position_key, price, price, now, None)?;

    a.user_account.collateral_balance = a.user_account.collateral_balance.checked_add(o.payout).ok_or(PerpsError::MathOverflow)?;
    a.lp_pool.free_collateral = a.lp_pool.free_collateral.checked_add(o.payout).ok_or(PerpsError::MathOverflow)?;

    msg!("Position closed: PnL={}, Funding={}, Fee={}, Settlement={}", o.pnl, o.funding, o.fee, o.payout);
    emit!(PositionClosed {
        position: position_key,
        owner: a.position.owner,
        mark: price,
        exec_price: price,
        pnl: o.pnl,
        funding: o.funding,
        solvency_hc: o.solvency_hc,
        bad_debt_hc: o.bad_debt_hc,
        fee: o.fee,
        referral_paid: o.referral_paid,
        payout: o.payout,
        deficit: o.deficit,
        settled_close: true,
    });
    Ok(())
}
