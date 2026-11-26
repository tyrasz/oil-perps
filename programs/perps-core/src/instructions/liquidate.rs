//! Two-stage liquidation (docs/lp-pool.md §6): stage 1 closes a slice (or
//! the whole position) with a penalty and the owner keeps the leftover
//! equity; stage 2 is the backstop, a full close whose remainder goes to
//! insurance. Liquidations execute at the oracle with no impact and never
//! revert on pool cash (§2.8, red-team F21).

use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Transfer};
use crate::errors::PerpsError;
use crate::instructions::update_funding::crank_funding;
use crate::events::PositionLiquidated;
use crate::oracle::read_price;
use crate::pool_math;
use crate::state::{
    absorb_deficit, apply_payout, assert_backed, credit_insurance, pay_profit, pool_remove, pool_replace, pool_view,
    quote_profit, roll_loss_window, touch_peak, ExecutionSource, LpPool, Market, Payout, PoolView, Position,
    PositionStatus, Side, UserAccount, Vault,
};

#[derive(Accounts)]
pub struct Liquidate<'info> {
    #[account(mut)]
    pub liquidator: Signer<'info>,

    /// CHECK: Position owner, doesn't need to sign
    pub position_owner: UncheckedAccount<'info>,

    #[account(
        mut,
        seeds = [b"user", position_owner.key().as_ref(), market.key().as_ref()],
        bump = user_account.bump
    )]
    pub user_account: Box<Account<'info, UserAccount>>,

    #[account(
        mut,
        seeds = [b"market", market.collateral_mint.as_ref(), market.commodity_seed()],
        bump = market.bump
    )]
    pub market: Box<Account<'info, Market>>,

    #[account(
        mut,
        constraint = position.owner == position_owner.key(),
        constraint = position.market == market.key(),
        constraint = position.status == PositionStatus::Open @ PerpsError::PositionAlreadyClosed
    )]
    pub position: Box<Account<'info, Position>>,

    #[account(
        mut,
        seeds = [b"vault", market.key().as_ref()],
        bump = vault.bump
    )]
    pub vault: Box<Account<'info, Vault>>,

    #[account(
        mut,
        seeds = [b"vault_token", market.key().as_ref()],
        bump
    )]
    pub vault_token_account: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        constraint = liquidator_token_account.owner == liquidator.key(),
        constraint = liquidator_token_account.mint == market.collateral_mint
    )]
    pub liquidator_token_account: Box<Account<'info, TokenAccount>>,

    /// CHECK: Pyth price feed
    #[account(constraint = pyth_price_feed.key() == market.pyth_price_feed)]
    pub pyth_price_feed: UncheckedAccount<'info>,

    pub token_program: Program<'info, Token>,

    // docs/lp-pool.md §4.6 (account 11)
    #[account(mut, seeds = [LpPool::SEED, market.key().as_ref()], bump = lp_pool.bump)]
    pub lp_pool: Box<Account<'info, LpPool>>,
}

/// What one `liquidate` did (the `PositionLiquidated` event, §6.5).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LiqOutcome {
    pub stage: u8,
    pub size_closed: u64,
    pub size_remaining: u64,
    pub penalty: u64,
    /// Paid to the liquidator's token account.
    pub keeper_reward: u64,
    /// Credited to the owner's free collateral (stage 1 full close).
    pub owner_leftover: u64,
    pub deficit: u64,
    pub solvency_hc: u64,
    pub bad_debt_hc: u64,
    pub cash_hc: u64,
}

/// §6.3 step 1: the stage-1 slice. `im`, `p` and `min_slice_bps` in bps,
/// `equity` the position's equity at `mark`. The smallest `x` with
/// `E − penalty(x) ≥ IM·notional(size − x)`, at least the minimum slice, and
/// the whole position when the remainder would be below `min_remaining`.
pub fn stage1_slice(size: u64, mark: u64, equity: i128, im: u32, p: u16, min_slice_bps: u16, min_remaining: u64) -> u64 {
    let x_need: i128 = if im as i128 > p as i128 {
        let num = (im as i128)
            .checked_mul(size as i128)
            .and_then(|v| v.checked_mul(mark as i128))
            .and_then(|v| equity.checked_mul(10_000_000_000).and_then(|e| v.checked_sub(e)));
        let den = (mark as i128).checked_mul(im as i128 - p as i128);
        match (num, den) {
            (Some(n), Some(d)) if d > 0 => pool_math::ceil_s(n, d).max(0),
            _ => size as i128,
        }
    } else {
        size as i128
    };
    let min_slice = pool_math::ceil_u(size as u128 * min_slice_bps as u128, 10_000) as i128;
    let x = x_need.max(min_slice).clamp(1, size.max(1) as i128) as u64;
    let x = x.min(size);
    if pool_math::notional_floor(size - x, mark) < min_remaining {
        size
    } else {
        x
    }
}

/// §6.3 step 2: `min(ceil(floor(x·m, 1e6)·p, 10_000), max(E, 0))`.
pub fn stage1_penalty(x: u64, mark: u64, p: u16, equity: i128) -> u64 {
    let n = pool_math::notional_floor(x, mark) as u128;
    let pen = pool_math::ceil_u(n * p as u128, 10_000);
    let cap = equity.max(0) as u128;
    pen.min(cap).min(u64::MAX as u128) as u64
}

/// The accrued funding a slice of `x` of the position settles: the whole
/// position's minus what the remainder keeps, so slice and remainder add up
/// to the position's funding exactly. Departure from §6.3/§7.2, which
/// settle all funding on the slice: that let a partial close pay out the
/// remainder's funding and leave it under water (or bankrupt).
pub fn slice_funding(position: &Position, x: u64, f: i64) -> i128 {
    let all = position.accrued_funding(f) as i128;
    if x >= position.size {
        return all;
    }
    let rest = Position { size: position.size - x, ..*position };
    all - rest.accrued_funding(f) as i128
}

fn checked_u64(x: i128) -> Result<u64> {
    u64::try_from(x).map_err(|_| error!(PerpsError::MathOverflow))
}

/// `OI_side −= x`, checked.
pub fn oi_sub(m: &mut Market, side: Side, x: u64) -> Result<()> {
    match side {
        Side::Long => m.long_open_interest = m.long_open_interest.checked_sub(x).ok_or(PerpsError::MathOverflow)?,
        Side::Short => m.short_open_interest = m.short_open_interest.checked_sub(x).ok_or(PerpsError::MathOverflow)?,
    }
    Ok(())
}

/// For a house position, remove `opened_notional(q) − opened_notional(q − x)`
/// of `q` from the owner's house notional (telescopes; D21).
pub fn house_sub(user: &mut UserAccount, position: &Position, q: u64, x: u64) -> Result<()> {
    if position.execution_source == ExecutionSource::OrderBook {
        let d = position.opened_notional(q) - position.opened_notional(q - x);
        user.house_entry_notional = user.house_entry_notional.checked_sub(d).ok_or(PerpsError::BucketUnderflow)?;
    }
    Ok(())
}

/// Close what is left of the position's bookkeeping after a full exit:
/// aggregates, OI, house notional, status and the user's lifetime PnL.
#[inline(never)]
fn finish_full(m: &mut Market, pool: &mut LpPool, position: &mut Position, user: &mut UserAccount, status: PositionStatus, now: i64) -> Result<()> {
    let size = position.size;
    pool_remove(pool, position.side, size, position.entry_price, position.last_funding_payment)?;
    oi_sub(m, position.side, size)?;
    house_sub(user, position, size, size)?;
    position.status = status;
    position.last_updated_at = now;
    user.realized_pnl = user.realized_pnl.saturating_add(position.realized_pnl);
    Ok(())
}

/// Stage 1 (§6.3). `e` is the equity at `mark` before any haircut.
#[inline(never)]
#[allow(clippy::too_many_arguments)]
pub fn liq_stage1(m: &mut Market, pool: &mut LpPool, v: &PoolView, position: &mut Position, user: &mut UserAccount, mark: u64, im: u32, e: i128, now: i64) -> Result<LiqOutcome> {
    let params = pool.params;
    let size = position.size;
    let side = position.side;
    let c = position.collateral;
    let fnd = position.accrued_funding(m.cumulative_funding) as i128;
    let mut x = stage1_slice(size, mark, e, im, params.liq_penalty_bps, params.min_liq_slice_bps, params.liq_min_remaining_notional);

    if x < size {
        // 3. Partial: quote first, apply nothing until C' > 0 is known
        // The slice settles its own share of funding; the remainder keeps
        // the rest and its funding basis (see `slice_funding`)
        let slice = Position { side, size: x, entry_price: position.entry_price, ..Default::default() };
        let realized = slice.unrealized_pnl(mark) as i128 + slice_funding(position, x, m.cumulative_funding);
        let pen = stage1_penalty(x, mark, params.liq_penalty_bps, e);
        let quote = if realized > 0 {
            quote_profit(pool, v, side, checked_u64(realized)?, true)?
        } else {
            Payout::default()
        };
        let r = if realized > 0 { quote.paid as i128 } else { realized };
        let c_new = c as i128 + r - pen as i128;
        // Departure: a remainder whose equity a haircut would take to 0 or
        // below closes in full instead (§6.3 checks only C' > 0)
        let rest_pos = Position { size: size - x, ..*position };
        let rest_equity = c_new + rest_pos.unrealized_pnl(mark) as i128 + rest_pos.accrued_funding(m.cumulative_funding) as i128;
        if c_new > 0 && rest_equity > 0 {
            if realized > 0 {
                apply_payout(pool, side, &quote)?;
            } else {
                pool.cash = pool.cash.checked_add(checked_u64(-realized)?).ok_or(PerpsError::MathOverflow)?;
            }
            let pc = pool.position_collateral as i128 + (c_new - c as i128);
            pool.position_collateral = u64::try_from(pc).map_err(|_| error!(PerpsError::BucketUnderflow))?;
            let keeper = (pen as u128 * params.liq_keeper_share_bps as u128 / 10_000) as u64;
            credit_insurance(m, pool, pen - keeper)?;
            let rest = size - x;
            let f_old = position.last_funding_payment;
            pool_replace(pool, side, (size, position.entry_price, f_old), (rest, position.entry_price, f_old))?;
            oi_sub(m, side, x)?;
            house_sub(user, position, size, x)?;
            position.size = rest;
            position.collateral = checked_u64(c_new)?;
            position.realized_pnl = position.realized_pnl.saturating_add((r - pen as i128).clamp(i64::MIN as i128, i64::MAX as i128) as i64);
            position.last_updated_at = now;
            return Ok(LiqOutcome {
                stage: 1,
                size_closed: x,
                size_remaining: rest,
                penalty: pen,
                keeper_reward: keeper,
                owner_leftover: 0,
                deficit: 0,
                solvency_hc: quote.solvency_hc,
                bad_debt_hc: quote.bad_debt_hc,
                cash_hc: quote.cash_hc,
            });
        }
        x = size;
    }

    // 4. Full stage-1 close
    debug_assert!(x == size);
    let realized = position.unrealized_pnl(mark) as i128 + fnd;
    let (r, po) = if realized > 0 {
        let po = pay_profit(pool, v, side, checked_u64(realized)?, true)?;
        (po.paid as i128, po)
    } else {
        pool.cash = pool.cash.checked_add(checked_u64(-realized)?).ok_or(PerpsError::MathOverflow)?;
        (realized, Payout::default())
    };
    let e_after = (c as i128 + r).max(0);
    let pen = stage1_penalty(size, mark, params.liq_penalty_bps, e).min(checked_u64(e_after)?);
    let keeper = (pen as u128 * params.liq_keeper_share_bps as u128 / 10_000) as u64;
    // c + r ≥ 0 here (§6.3 step 4), so this is exact
    let leftover = checked_u64(c as i128 + r - pen as i128)?;
    pool.position_collateral = pool.position_collateral.checked_sub(c).ok_or(PerpsError::BucketUnderflow)?;
    credit_insurance(m, pool, pen - keeper)?;
    user.collateral_balance = user.collateral_balance.checked_add(leftover).ok_or(PerpsError::MathOverflow)?;
    pool.free_collateral = pool.free_collateral.checked_add(leftover).ok_or(PerpsError::MathOverflow)?;
    position.realized_pnl = position.realized_pnl.saturating_add((r - pen as i128).clamp(i64::MIN as i128, i64::MAX as i128) as i64);
    finish_full(m, pool, position, user, PositionStatus::Liquidated, now)?;
    Ok(LiqOutcome {
        stage: 1,
        size_closed: size,
        size_remaining: 0,
        penalty: pen,
        keeper_reward: keeper,
        owner_leftover: leftover,
        deficit: 0,
        solvency_hc: po.solvency_hc,
        bad_debt_hc: po.bad_debt_hc,
        cash_hc: po.cash_hc,
    })
}

/// Stage 2, the backstop (§6.4). `nav0` is `NAV_redeem` at the start.
#[inline(never)]
#[allow(clippy::too_many_arguments)]
pub fn liq_stage2(m: &mut Market, pool: &mut LpPool, v: &PoolView, position: &mut Position, user: &mut UserAccount, key: Pubkey, mark: u64, mmr: u32, nav0: i128, now: i64) -> Result<LiqOutcome> {
    let size = position.size;
    let side = position.side;
    let c = position.collateral;
    let target = m.liquidation_reward_target(position.opened_notional(size), mmr);
    let realized = position.unrealized_pnl(mark) as i128 + position.accrued_funding(m.cumulative_funding) as i128;
    let (r, po) = if realized > 0 {
        let po = pay_profit(pool, v, side, checked_u64(realized)?, true)?;
        (po.paid as i128, po)
    } else {
        (realized, Payout::default())
    };
    let e_after = c as i128 + r;

    roll_loss_window(pool, nav0, now);
    let cap_ins = pool_math::insurance_window_cap(pool.params.insurance_claim_cap_per_window, pool.ins_paid_window);
    let (from_col, top, deficit) = if e_after >= 0 {
        if realized <= 0 {
            pool.cash = pool.cash.checked_add(checked_u64(-realized)?).ok_or(PerpsError::MathOverflow)?;
        }
        let e = checked_u64(e_after)?;
        let from_col = target.min(e);
        credit_insurance(m, pool, e - from_col)?;
        let top = (target - from_col).min(m.insurance_fund).min(cap_ins);
        m.insurance_fund -= top;
        pool.ins_paid_window = pool.ins_paid_window.saturating_add(top);
        (from_col, top, 0u64)
    } else {
        let top = target.min(m.insurance_fund).min(cap_ins);
        m.insurance_fund -= top;
        pool.ins_paid_window = pool.ins_paid_window.saturating_add(top);
        pool.cash = pool.cash.checked_add(c).ok_or(PerpsError::MathOverflow)?;
        let d = checked_u64(-e_after)?;
        absorb_deficit(m, pool, key, d, side, nav0, now)?;
        (0u64, top, d)
    };
    pool.position_collateral = pool.position_collateral.checked_sub(c).ok_or(PerpsError::BucketUnderflow)?;
    position.realized_pnl = position.realized_pnl.saturating_add(r.clamp(i64::MIN as i128, i64::MAX as i128) as i64);
    finish_full(m, pool, position, user, PositionStatus::Liquidated, now)?;
    Ok(LiqOutcome {
        stage: 2,
        size_closed: size,
        size_remaining: 0,
        penalty: 0,
        keeper_reward: from_col + top,
        owner_leftover: 0,
        deficit,
        solvency_hc: po.solvency_hc,
        bad_debt_hc: po.bad_debt_hc,
        cash_hc: po.cash_hc,
    })
}

/// §6.2 steps 2–4 and the stage: everything but the price, the transfer and
/// the log.
#[inline(never)]
#[allow(clippy::too_many_arguments)]
pub fn liquidate_position(m: &mut Market, pool: &mut LpPool, position: &mut Position, user: &mut UserAccount, key: Pubkey, mark: u64, now: i64) -> Result<LiqOutcome> {
    let (im, mmr) = m.margin_ratios(now);
    let fnd = position.accrued_funding(m.cumulative_funding);
    require!(position.is_liquidatable(mark, fnd, mmr), PerpsError::NotLiquidatable);
    let e = position.equity(mark, m.cumulative_funding);
    let n = position.mark_notional(mark);

    let v = pool_view(pool, m, mark, now, pool.params.redeem_loss_haircut_bps)?;
    touch_peak(pool, &v, now);
    let nav0 = v.nav_redeem;

    // Stage 1 iff E·1e4 ≥ backstop·N, decided before any haircut
    let backstop = mmr as u128 * pool.params.backstop_threshold_bps as u128 / 10_000;
    let stage1 = e >= 0 && (e as u128) * 10_000 >= backstop * n;
    if stage1 {
        liq_stage1(m, pool, &v, position, user, mark, im, e, now)
    } else {
        liq_stage2(m, pool, &v, position, user, key, mark, mmr, nav0, now)
    }
}

pub fn handler(ctx: Context<Liquidate>) -> Result<()> {
    let current_time = Clock::get()?.unix_timestamp;
    let position_key = ctx.accounts.position.key();
    let a = &mut *ctx.accounts;
    require!(!a.market.is_settled(), PerpsError::MarketSettled);
    require!(a.market.is_in_session(current_time), PerpsError::MarketClosed);
    let oracle_price = read_price(&a.pyth_price_feed, &a.market, current_time)?;
    // Funding due is charged first (D40)
    crank_funding(&mut a.market, oracle_price, current_time);

    let insurance_before = a.market.insurance_fund;
    let o = liquidate_position(&mut a.market, &mut a.lp_pool, &mut a.position, &mut a.user_account, position_key, oracle_price, current_time)?;

    // Pay liquidator
    let vt_before = a.vault_token_account.amount;
    if o.keeper_reward > 0 {
        let market_key = a.market.key();
        let vault_bump = a.vault.bump;
        let vault_seeds = &[b"vault".as_ref(), market_key.as_ref(), &[vault_bump]];
        let signer = &[&vault_seeds[..]];
        let cpi_accounts = Transfer {
            from: a.vault_token_account.to_account_info(),
            to: a.liquidator_token_account.to_account_info(),
            authority: a.vault.to_account_info(),
        };
        let cpi_ctx = CpiContext::new_with_signer(a.token_program.key(), cpi_accounts, signer);
        token::transfer(cpi_ctx, o.keeper_reward)?;
    }
    let after = vt_before.checked_sub(o.keeper_reward).ok_or(PerpsError::VaultInvariantViolated)?;
    assert_backed(&a.lp_pool, &a.market, after)?;

    // The size field is the closed size (§6.5)
    msg!(
        "Position liquidated: owner={}, size={}, reward={}, insurance_fund={} -> {}",
        a.position.owner,
        o.size_closed,
        o.keeper_reward,
        insurance_before,
        a.market.insurance_fund
    );
    emit_event!(PositionLiquidated {
        position: position_key,
        stage: o.stage,
        size_closed: o.size_closed,
        size_remaining: o.size_remaining,
        mark: oracle_price,
        penalty: o.penalty,
        keeper_reward: o.keeper_reward,
        owner_leftover: o.owner_leftover,
        insurance_delta: (a.market.insurance_fund as i128 - insurance_before as i128) as i64,
        deficit: o.deficit,
        solvency_hc: o.solvency_hc,
        bad_debt_hc: o.bad_debt_hc,
        cash_hc: o.cash_hc,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{bucket_total, pool_add, PoolParams};

    const C6: u64 = 1_000_000;

    fn market() -> Market {
        Market {
            maintenance_margin_ratio: 500,
            initial_margin_ratio: 1_000,
            liquidation_fee: 100,
            taker_fee: 5,
            max_open_interest: u64::MAX,
            ..Default::default()
        }
    }

    fn pool_with(cash: u64, params: PoolParams) -> LpPool {
        LpPool { cash, lp_shares: cash.max(1), params, ..Default::default() }
    }

    /// `size` (6 dp) at `entry` (6 dp) with `collateral` (6 dp), in the pool and OI.
    fn open(m: &mut Market, pool: &mut LpPool, user: &mut UserAccount, side: Side, size: u64, entry: u64, collateral: u64) -> Position {
        let mut p = Position::default();
        p.init_open(Pubkey::default(), Pubkey::default(), side, size, collateral, entry, 10_000, m.cumulative_funding, 0, ExecutionSource::OrderBook, 0);
        pool_add(pool, side, size, entry, p.last_funding_payment).unwrap();
        pool.position_collateral += collateral;
        match side {
            Side::Long => m.long_open_interest += size,
            Side::Short => m.short_open_interest += size,
        }
        user.house_entry_notional += p.opened_notional(size);
        p
    }

    /// Liquidate at `mark` and check the bucket step: Σ buckets falls by
    /// exactly the keeper reward, the only tokens that leave (§11).
    fn liq(m: &mut Market, pool: &mut LpPool, user: &mut UserAccount, p: &mut Position, mark: u64) -> Result<LiqOutcome> {
        let before = bucket_total(pool, m);
        let free_before = pool.free_collateral;
        let o = liquidate_position(m, pool, p, user, Pubkey::new_unique(), mark, 1)?;
        assert_eq!(bucket_total(pool, m), before - o.keeper_reward as u128);
        assert_eq!(pool.free_collateral - free_before, o.owner_leftover);
        Ok(o)
    }

    /// §17.1 #7: the slice leaves `E − pen ≥ IM·N_rem` up to one unit, and
    /// respects the minimum slice.
    #[test]
    fn stage1_slice_restores_initial_margin() {
        for &im in &[1_000u32, 2_000, 1_500] {
            for &p in &[0u16, 100, 200, 500] {
                for &size in &[C6, 7 * C6 + 3, 1_000 * C6, 123_456_789] {
                    for &mark in &[1_000_000u64, 75_000_000, 75_123_457] {
                        let n = pool_math::notional_floor(size, mark) as i128;
                        // E from 3% to 7% of notional
                        for e_bps in [300i128, 450, 499, 700] {
                            let e = n * e_bps / 10_000;
                            let x = stage1_slice(size, mark, e, im, p, 1, 0);
                            assert!(x >= 1 && x <= size);
                            let pen = stage1_penalty(x, mark, p, e) as i128;
                            let rem = pool_math::notional_floor(size - x, mark) as i128;
                            // (E − pen)·1e4 ≥ im·N_rem, less one unit of slack
                            assert!((e - pen) * 10_000 + 10_000 >= im as i128 * rem, "im {} p {} size {} mark {} e {} x {}", im, p, size, mark, e, x);
                            // and x is the smallest such slice (within a unit)
                            if x > 1 && x < size {
                                let y = x - 1;
                                let pen_y = stage1_penalty(y, mark, p, e) as i128;
                                let rem_y = pool_math::notional_floor(size - y, mark) as i128;
                                assert!((e - pen_y) * 10_000 <= im as i128 * rem_y + 10_000 * 2);
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn stage1_slice_minimums_and_fallbacks() {
        let size = 100 * C6;
        let mark = 75 * C6;
        let n = pool_math::notional_floor(size, mark) as i128;
        // Just below MM: the exact need is small; the 20% minimum slice wins
        let e = n * 480 / 10_000;
        let need = stage1_slice(size, mark, e, 1_000, 200, 1, 0);
        assert_eq!(stage1_slice(size, mark, e, 1_000, 200, 2_000, 0), need.max(20 * C6));
        // A remainder under the minimum notional closes the whole position
        let x = stage1_slice(size, mark, e, 1_000, 200, 2_000, 0);
        let rem = pool_math::notional_floor(size - x, mark);
        assert_eq!(stage1_slice(size, mark, e, 1_000, 200, 2_000, rem), x);
        assert_eq!(stage1_slice(size, mark, e, 1_000, 200, 2_000, rem + 1), size);
        // im ≤ p: no slice can restore IM, so the whole position
        assert_eq!(stage1_slice(size, mark, e, 200, 200, 1, 0), size);
        assert_eq!(stage1_slice(size, mark, e, 100, 200, 1, 0), size);
        // Overflowing products fall back to the whole position
        assert_eq!(stage1_slice(u64::MAX, u64::MAX, 1, 10_000, 0, 1, 0), u64::MAX);
        // The penalty is capped by equity
        assert_eq!(stage1_penalty(size, mark, 200, n), 150 * C6);
        assert_eq!(stage1_penalty(size, mark, 200, 10 * C6 as i128), 10 * C6);
        assert_eq!(stage1_penalty(size, mark, 200, -5), 0);
        // ceil of the bps product: $0.000075 · 2% rounds up to 1 unit
        assert_eq!(stage1_penalty(1, 75 * C6, 200, 1_000), 2);
    }

    fn stage1_params() -> PoolParams {
        PoolParams { lp_loss_cap_window_bps: 10_000, insurance_target: u64::MAX, ..PoolParams::production() }
    }

    #[test]
    fn stage1_partial_keeps_the_position_open() {
        let (mut m, mut pool, mut user) = (market(), pool_with(1_000_000 * C6, stage1_params()), UserAccount::default());
        // 100 long at $75 with $750 (10x); at $71 equity is $350 = 4.93% of $7,100
        let mut p = open(&mut m, &mut pool, &mut user, Side::Long, 100 * C6, 75 * C6, 750 * C6);
        let o = liq(&mut m, &mut pool, &mut user, &mut p, 71 * C6).unwrap();
        assert_eq!(o.stage, 1);
        assert!(o.size_closed >= 20 * C6 && o.size_closed < 100 * C6);
        assert!(p.status == PositionStatus::Open);
        assert_eq!(p.size, 100 * C6 - o.size_closed);
        // Penalty 2% of the closed mark notional, 20% of it to the keeper
        let pen = stage1_penalty(o.size_closed, 71 * C6, 200, 350 * C6 as i128);
        assert_eq!((o.penalty, o.keeper_reward), (pen, pen / 5));
        assert_eq!(m.insurance_fund, pen - pen / 5);
        // The remainder is back at initial margin, so not liquidatable again
        assert!(!p.is_liquidatable(71 * C6, 0, 500));
        let e_after = p.equity(71 * C6, 0);
        assert!(e_after * 10_000 + 10_000 >= 1_000 * p.mark_notional(71 * C6) as i128);
        assert_eq!(liq(&mut m, &mut pool, &mut user, &mut p, 71 * C6).unwrap_err(), PerpsError::NotLiquidatable.into());
        // Aggregates and the house notional follow the remainder exactly
        assert_eq!(pool.long_entry_sum, p.size as u128 * 75 * C6 as u128);
        assert_eq!(m.long_open_interest, p.size);
        assert_eq!(user.house_entry_notional, p.opened_notional(p.size));
        assert_eq!(pool.position_collateral, p.collateral);
    }

    #[test]
    fn stage1_full_close_credits_the_owner() {
        let params = PoolParams { liq_min_remaining_notional: 100_000 * C6, ..stage1_params() };
        let (mut m, mut pool, mut user) = (market(), pool_with(1_000_000 * C6, params), UserAccount::default());
        let mut p = open(&mut m, &mut pool, &mut user, Side::Long, 100 * C6, 75 * C6, 750 * C6);
        let cash0 = pool.cash;
        let o = liq(&mut m, &mut pool, &mut user, &mut p, 71 * C6).unwrap();
        // The remainder would be under $100k: closed in full
        assert_eq!((o.stage, o.size_closed, o.size_remaining), (1, 100 * C6, 0));
        assert!(p.status == PositionStatus::Liquidated);
        // $350 equity − 2% of $7,100 ($142) = $208 to the owner's free collateral
        assert_eq!((o.penalty, o.owner_leftover, o.keeper_reward), (142 * C6, 208 * C6, 28_400_000));
        assert_eq!(user.collateral_balance, 208 * C6);
        assert_eq!(pool.cash, cash0 + 400 * C6);
        assert_eq!((pool.open_positions, pool.position_collateral, user.house_entry_notional), (0, 0, 0));
        assert_eq!(user.realized_pnl, -400 * C6 as i64 - 142 * C6 as i64);
    }

    /// §17.1 #7: a slice whose haircut makes `C' ≤ 0` becomes a full close.
    #[test]
    fn haircut_slice_becomes_a_full_stage1_close() {
        // $1 of collateral, +$100 unrealized; the pool has no cash, so the
        // winner's profit is haircut to 0 and the slice penalty exceeds C
        let (mut m, mut pool, mut user) = (market(), pool_with(0, stage1_params()), UserAccount::default());
        pool.lp_shares = 1;
        let mut p = open(&mut m, &mut pool, &mut user, Side::Long, 25 * C6, 96 * C6, C6);
        let o = liq(&mut m, &mut pool, &mut user, &mut p, 100 * C6).unwrap();
        assert_eq!((o.stage, o.size_closed), (1, 25 * C6));
        assert_eq!((o.solvency_hc, o.cash_hc), (100 * C6, 0));
        // E' = $1: the whole of it is the penalty
        assert_eq!((o.penalty, o.owner_leftover), (C6, 0));
        assert!(p.status == PositionStatus::Liquidated);
    }

    #[test]
    fn stage2_residual_goes_to_insurance_then_cash() {
        let params = PoolParams { insurance_target: 100 * C6, ..PoolParams::neutral() };
        let (mut m, mut pool, mut user) = (market(), pool_with(1_000_000 * C6, params), UserAccount::default());
        m.insurance_fund = 90 * C6;
        // 100 long at $75 with $500: at $71.50 equity $150 = 2.1% < 5%
        let mut p = open(&mut m, &mut pool, &mut user, Side::Long, 100 * C6, 75 * C6, 500 * C6);
        let cash0 = pool.cash;
        let o = liq(&mut m, &mut pool, &mut user, &mut p, 71_500_000).unwrap();
        // Reward 1% of 5% of $7,500 = $3.75 from collateral; $146.25 left:
        // $10 fills insurance to the target, $136.25 overflows to cash
        assert_eq!((o.stage, o.keeper_reward, o.deficit), (2, 3_750_000, 0));
        assert_eq!(m.insurance_fund, 100 * C6);
        assert_eq!(pool.cash, cash0 + 350 * C6 + 136_250_000);
        assert_eq!(p.realized_pnl, -350 * C6 as i64);
    }

    #[test]
    fn stage2_top_up_is_capped_by_the_window() {
        let params = PoolParams { insurance_claim_cap_per_window: 2 * C6, ..PoolParams::neutral() };
        let (mut m, mut pool, mut user) = (market(), pool_with(1_000_000 * C6, params), UserAccount::default());
        m.insurance_fund = 50 * C6;
        // Under water: $75 on 100 @ $75, gapped to $74: equity −$25
        let mut p = open(&mut m, &mut pool, &mut user, Side::Long, 100 * C6, 75 * C6, 75 * C6);
        let o = liq(&mut m, &mut pool, &mut user, &mut p, 74 * C6).unwrap();
        // Reward target $3.75 but only $2 of insurance per window; the
        // deficit then finds the window's insurance used up: LPs take it
        assert_eq!((o.keeper_reward, o.deficit), (2 * C6, 25 * C6));
        assert_eq!(m.insurance_fund, 48 * C6);
        assert_eq!((pool.ins_paid_window, pool.lp_absorbed_window), (2 * C6, 25 * C6));
        assert!(!pool.adl_required());
    }

    #[test]
    fn stage2_deficit_runs_the_waterfall_to_pending() {
        // LP cap: 1 bp of the window-start NAV ($1M) = $100 per window
        let params = PoolParams { lp_loss_cap_window_bps: 1, ..PoolParams::neutral() };
        let (mut m, mut pool, mut user) = (market(), pool_with(1_000_000 * C6, params), UserAccount::default());
        m.insurance_fund = 10 * C6;
        // $75 collateral on 10 @ $75, gapped 15% to $63.75: −$37.50
        let mut p = open(&mut m, &mut pool, &mut user, Side::Long, 10 * C6, 75 * C6, 75 * C6);
        let _short = open(&mut m, &mut pool, &mut user, Side::Short, 110 * C6, 75 * C6, 8_250 * C6);
        let o = liq(&mut m, &mut pool, &mut user, &mut p, 63_750_000).unwrap();
        // Reward $0.375 from insurance, which then pays $9.625 of the
        // deficit; the LP absorbs the other $27.875 (under its $100 cap)
        assert_eq!((o.keeper_reward, o.deficit), (375_000, 37_500_000));
        assert_eq!(m.insurance_fund, 0);
        assert_eq!((pool.ins_paid_window, pool.lp_absorbed_window), (10 * C6, 27_875_000));
        assert!(!pool.adl_required());

        // A second deficit in the same window shares the cap; the rest is pending
        let mut q = open(&mut m, &mut pool, &mut user, Side::Long, 100 * C6, 75 * C6, 750 * C6);
        let o = liq(&mut m, &mut pool, &mut user, &mut q, 63_750_000).unwrap();
        assert_eq!(o.deficit, 375 * C6);
        assert_eq!(pool.lp_absorbed_window, pool_math::lp_window_cap(pool.loss_window_nav, 1, 0));
        assert_eq!(pool.pending_bad_debt_long, 375 * C6 + 27_875_000 - pool.lp_absorbed_window);
        assert!(pool.adl_required());
        assert_eq!(pool.pending_since_long, 1);
    }

    #[test]
    fn liquidation_never_reverts_on_cash() {
        // A thin long up $10 at $75.10 (equity $11, under 5% MM) whose
        // matched short is still open (U = 0), with cash drained to 0
        // (red-team F21): the profit is cut to what cash holds, not refused
        let (mut m, mut pool, mut user) = (market(), pool_with(0, PoolParams::neutral()), UserAccount::default());
        pool.lp_shares = 1;
        let mut long = open(&mut m, &mut pool, &mut user, Side::Long, 100 * C6, 75 * C6, C6);
        let _short = open(&mut m, &mut pool, &mut user, Side::Short, 100 * C6, 75 * C6, 7_500 * C6);
        let o = liq(&mut m, &mut pool, &mut user, &mut long, 75_100_000).unwrap();
        assert_eq!((o.stage, o.cash_hc, o.solvency_hc), (2, 10 * C6, 0));
        // E' = $1, all of it toward the $3.75 reward; insurance is empty
        assert_eq!(o.keeper_reward, C6);
        assert_eq!(pool.cash, 0);
    }

    /// Review finding (stage-1 funding): the slice settles only its share
    /// of the funding; the remainder keeps the rest and its funding basis,
    /// so a cash haircut on the slice cannot eat the remainder's funding.
    /// Long 100 @ $100 with $600 and +$800 of funding, at $90 (E = $400,
    /// 4.4% of $9,000), with no pool cash.
    #[test]
    fn stage1_slice_settles_only_its_share_of_funding() {
        let (mut m, mut pool, mut user) = (market(), pool_with(0, stage1_params()), UserAccount::default());
        pool.lp_shares = 1;
        let mut p = open(&mut m, &mut pool, &mut user, Side::Long, 100 * C6, 100 * C6, 600 * C6);
        m.cumulative_funding = -8 * C6 as i64;
        let e = p.equity(90 * C6, m.cumulative_funding);
        assert_eq!(e, 400 * C6 as i128);
        let o = liq(&mut m, &mut pool, &mut user, &mut p, 90 * C6).unwrap();
        assert_eq!(o.stage, 1);
        assert!(o.size_closed < 100 * C6 && p.status == PositionStatus::Open);
        // The slice lost money net of its funding, so nothing was haircut
        assert_eq!((o.cash_hc, o.solvency_hc), (0, 0));
        assert_eq!(p.last_funding_payment, 0);
        assert_eq!(pool.long_funding_sum, 0);
        // The remainder's equity is E less the penalty, to the unit
        let rest = p.equity(90 * C6, m.cumulative_funding);
        assert!((rest - (e - o.penalty as i128)).abs() <= 1, "{} vs {}", rest, e - o.penalty as i128);
    }

    /// Mutation finding: the insurance window cap and its counter also bind
    /// the keeper top-up of a solvent stage 2 (`0 ≤ E < target`). Long 100
    /// @ $75 with $401 at $71: E = $1 against a $3.75 target.
    #[test]
    fn solvent_stage2_top_up_is_capped_and_counted_per_window() {
        let params = PoolParams { insurance_claim_cap_per_window: 2 * C6, ..PoolParams::neutral() };
        let (mut m, mut pool, mut user) = (market(), pool_with(1_000_000 * C6, params), UserAccount::default());
        m.insurance_fund = 50 * C6;
        let mut p = open(&mut m, &mut pool, &mut user, Side::Long, 100 * C6, 75 * C6, 401 * C6);
        let o = liq(&mut m, &mut pool, &mut user, &mut p, 71 * C6).unwrap();
        // $1 from the collateral, then min($2.75, $50, the $2 cap)
        assert_eq!((o.stage, o.deficit, o.keeper_reward), (2, 0, 3 * C6));
        assert_eq!((m.insurance_fund, pool.ins_paid_window), (48 * C6, 2 * C6));
        // A second one in the same window finds no insurance room left
        let mut q = open(&mut m, &mut pool, &mut user, Side::Long, 100 * C6, 75 * C6, 401 * C6);
        let o = liq(&mut m, &mut pool, &mut user, &mut q, 71 * C6).unwrap();
        assert_eq!((o.stage, o.keeper_reward), (2, C6));
        assert_eq!((m.insurance_fund, pool.ins_paid_window), (48 * C6, 2 * C6));
    }

    /// Mutation finding (F21): a profitable partial stage-1 slice never
    /// reverts on pool cash; the part cash cannot pay is cut (`cash_hc`).
    /// Long 100 @ $75 with $200 at $76: E = $300 (3.9% < 5% MM, above the
    /// 3.33% backstop line); its matched short keeps U at 0; cash is $10.
    #[test]
    fn profitable_partial_stage1_never_reverts_on_cash() {
        let (mut m, mut pool, mut user) = (market(), pool_with(10 * C6, stage1_params()), UserAccount::default());
        let mut p = open(&mut m, &mut pool, &mut user, Side::Long, 100 * C6, 75 * C6, 200 * C6);
        let _short = open(&mut m, &mut pool, &mut user, Side::Short, 100 * C6, 75 * C6, 7_500 * C6);
        let o = liq(&mut m, &mut pool, &mut user, &mut p, 76 * C6).unwrap();
        assert_eq!(o.stage, 1);
        assert!(o.size_remaining > 0 && p.status == PositionStatus::Open);
        // The slice earned $1 a contract; cash paid its $10 and the rest was cut
        assert_eq!(o.cash_hc, o.size_closed - 10 * C6);
        assert_eq!(pool.cash, 0);
    }

    /// Mutation findings: `liquidate_position` touches the drawdown peak
    /// (§2.2), and equity exactly on the backstop line, `floor(MMR ·
    /// backstop_bps / 1e4)` = 333 bps of $7,000, is stage 1 (`≥`).
    #[test]
    fn backstop_line_is_inclusive_and_liquidation_touches_the_peak() {
        let (mut m, mut pool, mut user) = (market(), pool_with(1_000_000 * C6, stage1_params()), UserAccount::default());
        // E = $733.10 − $500 = $233.10 = 333 bps of $7,000 at $70
        let mut p = open(&mut m, &mut pool, &mut user, Side::Long, 100 * C6, 75 * C6, 733_100_000);
        pool.nps_peak = 1;
        let o = liq(&mut m, &mut pool, &mut user, &mut p, 70 * C6).unwrap();
        assert_eq!(o.stage, 1);
        assert!(pool.nps_peak > 1);
        assert_eq!(pool.nps_peak_at, 1);
        // One unit less is stage 2
        let mut q = open(&mut m, &mut pool, &mut user, Side::Long, 100 * C6, 75 * C6, 733_099_999);
        assert_eq!(liq(&mut m, &mut pool, &mut user, &mut q, 70 * C6).unwrap().stage, 2);
    }

    /// Mutation finding (D21 rounding against the actor): the minimum slice
    /// is a ceiling. 20% of 7,000,003 units is 1,400,000.6 → 1,400,001.
    #[test]
    fn stage1_minimum_slice_rounds_up() {
        let size = 7 * C6 + 3;
        let mark = 75 * C6;
        let e = pool_math::notional_floor(size, mark) as i128; // well above IM: x_need ≤ 0
        assert_eq!(stage1_slice(size, mark, e, 1_000, 200, 2_000, 0), 1_400_001);
    }
}
