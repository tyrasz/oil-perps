//! Pool lifecycle and admin instructions (docs/lp-pool.md §5, §10.2).

use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Transfer};
use crate::errors::PerpsError;
use crate::events::{PoolParamsApplied, PoolParamsCancelled, PoolParamsProposed, TreasuryWithdrawn};
use crate::state::{assert_backed, LpPool, Market, PoolParams, ProtocolConfig, Vault};

// ---------------------------------------------------------------------------
// initialize_pool(params, adl_keeper)
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct InitializePool<'info> {
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

    #[account(
        init,
        payer = admin,
        space = LpPool::LEN,
        seeds = [LpPool::SEED, market.key().as_ref()],
        bump
    )]
    pub lp_pool: Box<Account<'info, LpPool>>,

    pub system_program: Program<'info, System>,
}

/// Create the market's LP pool. The market must be fresh: not settled, no
/// open interest and an empty insurance fund. The vault token balance is not
/// checked, since anyone can send tokens to the vault PDA (red-team F14).
/// Bootstrap (D11): `initialize_market`, `initialize_pool`, `init_lp_position`
/// and the protocol's seed `lp_deposit` go in one transaction.
pub fn initialize_pool(ctx: Context<InitializePool>, params: PoolParams, adl_keeper: Pubkey) -> Result<()> {
    let market = &ctx.accounts.market;
    require!(
        !market.is_settled()
            && market.long_open_interest == 0
            && market.short_open_interest == 0
            && market.insurance_fund == 0,
        PerpsError::MarketNotFresh
    );
    require!(params.validate(market), PerpsError::InvalidPoolParams);

    let pool = &mut ctx.accounts.lp_pool;
    // `init` zeroed every bucket, aggregate and counter
    pool.market = market.key();
    pool.adl_keeper = adl_keeper;
    pool.params = params;
    pool.bump = *ctx.bumps.get("lp_pool").unwrap();

    msg!("LP pool initialized: {} for market {}", pool.key(), market.key());
    Ok(())
}

// ---------------------------------------------------------------------------
// propose / apply / cancel pool params (timelocked)
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct ProposePoolParams<'info> {
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

    #[account(mut, seeds = [LpPool::SEED, market.key().as_ref()], bump = lp_pool.bump)]
    pub lp_pool: Box<Account<'info, LpPool>>,
}

/// Before the seed (`lp_shares == 0`) the params apply at once. Otherwise they
/// wait `max(1 d, max delay + max window + 5 d)` so every LP can leave first.
pub fn propose_pool_params(ctx: Context<ProposePoolParams>, params: PoolParams) -> Result<()> {
    require!(params.validate(&ctx.accounts.market), PerpsError::InvalidPoolParams);
    let now = Clock::get()?.unix_timestamp;
    let pool = &mut ctx.accounts.lp_pool;

    if pool.lp_shares == 0 {
        pool.params = params;
        pool.pending_params = PoolParams::default();
        pool.pending_params_at = 0;
        emit!(PoolParamsProposed { params, effective_at: now });
        emit!(PoolParamsApplied { params, effective_at: now });
        msg!("Pool params applied (no LPs yet)");
        return Ok(());
    }

    let t = pool.params.timelock_secs(&params);
    let effective_at = now.checked_add(t).ok_or(PerpsError::MathOverflow)?;
    pool.pending_params = params;
    pool.pending_params_at = effective_at;
    emit!(PoolParamsProposed { params, effective_at });
    msg!("Pool params proposed, effective at {}", effective_at);
    Ok(())
}

#[derive(Accounts)]
pub struct ApplyPoolParams<'info> {
    #[account(
        seeds = [b"market", market.collateral_mint.as_ref(), market.commodity_seed()],
        bump = market.bump
    )]
    pub market: Box<Account<'info, Market>>,

    #[account(mut, seeds = [LpPool::SEED, market.key().as_ref()], bump = lp_pool.bump)]
    pub lp_pool: Box<Account<'info, LpPool>>,
}

/// Permissionless once the timelock has passed. Re-validated against the
/// market as it is now.
pub fn apply_pool_params(ctx: Context<ApplyPoolParams>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let pool = &mut ctx.accounts.lp_pool;
    require!(pool.pending_params_at != 0 && now >= pool.pending_params_at, PerpsError::ParamsTimelocked);
    let params = pool.pending_params;
    require!(params.validate(&ctx.accounts.market), PerpsError::InvalidPoolParams);

    let effective_at = pool.pending_params_at;
    pool.params = params;
    pool.pending_params = PoolParams::default();
    pool.pending_params_at = 0;
    emit!(PoolParamsApplied { params, effective_at });
    msg!("Pool params applied");
    Ok(())
}

#[derive(Accounts)]
pub struct PoolAdminOnly<'info> {
    pub admin: Signer<'info>,

    #[account(
        seeds = [ProtocolConfig::SEED],
        bump = config.bump,
        constraint = config.admin == admin.key() @ PerpsError::Unauthorized
    )]
    pub config: Box<Account<'info, ProtocolConfig>>,

    #[account(mut, seeds = [LpPool::SEED, lp_pool.market.as_ref()], bump = lp_pool.bump)]
    pub lp_pool: Box<Account<'info, LpPool>>,
}

pub fn cancel_pool_params(ctx: Context<PoolAdminOnly>) -> Result<()> {
    let pool = &mut ctx.accounts.lp_pool;
    require!(pool.pending_params_at != 0, PerpsError::NoPendingParams);
    let (params, effective_at) = (pool.pending_params, pool.pending_params_at);
    pool.pending_params = PoolParams::default();
    pool.pending_params_at = 0;
    emit!(PoolParamsCancelled { params, effective_at });
    msg!("Pool params proposal cancelled");
    Ok(())
}

/// LP-capital cap compared with `NAV_mint` (§9.1 step 8); 0 = unlimited.
/// Immediate: it cannot hurt existing LPs.
pub fn set_max_pool_cash(ctx: Context<PoolAdminOnly>, cap: u64) -> Result<()> {
    ctx.accounts.lp_pool.max_pool_cash = cap;
    msg!("Pool max_pool_cash set to {}", cap);
    Ok(())
}

/// The key that signs exposure-mode ADL (§7). Immediate.
pub fn set_adl_keeper(ctx: Context<PoolAdminOnly>, key: Pubkey) -> Result<()> {
    ctx.accounts.lp_pool.adl_keeper = key;
    msg!("Pool ADL keeper set to {}", key);
    Ok(())
}

// ---------------------------------------------------------------------------
// withdraw_treasury(amount) (§10.2)
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct WithdrawTreasury<'info> {
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

    #[account(mut, seeds = [LpPool::SEED, market.key().as_ref()], bump = lp_pool.bump)]
    pub lp_pool: Box<Account<'info, LpPool>>,

    #[account(seeds = [b"vault", market.key().as_ref()], bump = vault.bump)]
    pub vault: Box<Account<'info, Vault>>,

    #[account(mut, seeds = [b"vault_token", market.key().as_ref()], bump)]
    pub vault_token_account: Box<Account<'info, TokenAccount>>,

    /// Not the vault itself: a self-transfer moves no tokens, so the
    /// treasury would leave the books and strand its tokens as surplus.
    #[account(
        mut,
        constraint = destination.mint == market.collateral_mint,
        constraint = destination.key() != vault_token_account.key() @ PerpsError::VaultInvariantViolated
    )]
    pub destination: Box<Account<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,
}

/// The only route out for protocol revenue. There is no discretionary
/// withdrawal of insurance.
pub fn withdraw_treasury(ctx: Context<WithdrawTreasury>, amount: u64) -> Result<()> {
    require!(amount <= ctx.accounts.lp_pool.treasury, PerpsError::InsufficientTreasury);
    let vt_before = ctx.accounts.vault_token_account.amount;

    let market_key = ctx.accounts.market.key();
    let vault_seeds = &[b"vault".as_ref(), market_key.as_ref(), &[ctx.accounts.vault.bump]];
    let signer = &[&vault_seeds[..]];
    let cpi = CpiContext::new_with_signer(
        ctx.accounts.token_program.to_account_info(),
        Transfer {
            from: ctx.accounts.vault_token_account.to_account_info(),
            to: ctx.accounts.destination.to_account_info(),
            authority: ctx.accounts.vault.to_account_info(),
        },
        signer,
    );
    token::transfer(cpi, amount)?;

    let pool = &mut ctx.accounts.lp_pool;
    pool.treasury = pool.treasury.checked_sub(amount).ok_or(PerpsError::BucketUnderflow)?;
    let after = vt_before.checked_sub(amount).ok_or(PerpsError::VaultInvariantViolated)?;
    assert_backed(pool, &ctx.accounts.market, after)?;
    emit!(TreasuryWithdrawn { amount, destination: ctx.accounts.destination.key() });
    Ok(())
}
