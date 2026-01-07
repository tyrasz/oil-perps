use anchor_lang::prelude::*;

#[account]
#[derive(Default)]
pub struct LpPosition {
    pub owner: Pubkey,
    pub vault: Pubkey,
    pub shares: u64,
    pub deposited_amount: u64,       // Original deposit amount (for tracking)
    pub deposited_at: i64,           // Timestamp of deposit
    pub withdrawal_requested_at: i64, // Timestamp of withdrawal request (0 if none)
    pub bump: u8,
}

impl LpPosition {
    pub const LEN: usize = 8 +  // discriminator
        32 +  // owner
        32 +  // vault
        8 +   // shares
        8 +   // deposited_amount
        8 +   // deposited_at
        8 +   // withdrawal_requested_at
        1 +   // bump
        32;   // padding

    pub fn can_withdraw(&self, withdrawal_delay: i64, current_time: i64) -> bool {
        if self.withdrawal_requested_at == 0 {
            return false;
        }
        current_time >= self.withdrawal_requested_at + withdrawal_delay
    }

    pub fn pnl(&self, current_value: u64) -> i64 {
        current_value as i64 - self.deposited_amount as i64
    }
}
