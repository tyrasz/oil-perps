use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Transfer};
use crate::state::{LpVault, LpPosition};
use crate::errors::PropAmmError;

#[derive(Accounts)]
pub struct Deposit<'info> {
    #[account(mut)]
    pub depositor: Signer<'info>,

    #[account(
        mut,
        seeds = [b"lp_vault", vault.collateral_mint.as_ref()],
        bump = vault.bump
    )]
    pub vault: Account<'info, LpVault>,

    #[account(
        init_if_needed,
        payer = depositor,
        space = LpPosition::LEN,
        seeds = [b"lp_position", depositor.key().as_ref(), vault.key().as_ref()],
        bump
    )]
    pub lp_position: Account<'info, LpPosition>,

    #[account(
        mut,
        seeds = [b"vault_token", vault.key().as_ref()],
        bump
    )]
    pub vault_token_account: Account<'info, TokenAccount>,

    #[account(
        mut,
        constraint = depositor_token_account.owner == depositor.key(),
        constraint = depositor_token_account.mint == vault.collateral_mint
    )]
    pub depositor_token_account: Account<'info, TokenAccount>,

    pub system_program: Program<'info, System>,
    pub token_program: Program<'info, Token>,
}

pub fn handler(ctx: Context<Deposit>, amount: u64) -> Result<()> {
    require!(ctx.accounts.vault.is_active, PropAmmError::VaultNotActive);
    require!(amount > 0, PropAmmError::InvalidParameters);

    let vault = &mut ctx.accounts.vault;
    let lp_position = &mut ctx.accounts.lp_position;
    let current_time = Clock::get()?.unix_timestamp;

    // Calculate shares to mint
    let shares_to_mint = vault.shares_for_deposit(amount);
    require!(shares_to_mint > 0, PropAmmError::MathOverflow);

    // Transfer tokens from depositor to vault
    let cpi_accounts = Transfer {
        from: ctx.accounts.depositor_token_account.to_account_info(),
        to: ctx.accounts.vault_token_account.to_account_info(),
        authority: ctx.accounts.depositor.to_account_info(),
    };
    let cpi_program = ctx.accounts.token_program.to_account_info();
    let cpi_ctx = CpiContext::new(cpi_program, cpi_accounts);
    token::transfer(cpi_ctx, amount)?;

    // Update vault state
    vault.total_assets += amount;
    vault.total_shares += shares_to_mint;

    // Update or initialize LP position
    if lp_position.shares == 0 {
        // New position
        lp_position.owner = ctx.accounts.depositor.key();
        lp_position.vault = vault.key();
        lp_position.deposited_at = current_time;
        lp_position.bump = *ctx.bumps.get("lp_position").unwrap();
    }

    lp_position.shares += shares_to_mint;
    lp_position.deposited_amount += amount;
    lp_position.withdrawal_requested_at = 0; // Reset any pending withdrawal

    msg!(
        "Deposited {} USDC, minted {} shares. Total vault assets: {}, Share value: {}",
        amount,
        shares_to_mint,
        vault.total_assets,
        vault.share_value()
    );

    Ok(())
}
