use anchor_lang::prelude::*;

/// Protocol roles for the LP vault program, created once by its upgrade
/// authority (who becomes `admin`). The admin creates vaults and is, for now,
/// the only signer allowed to trade against them. `oracle_program` is the
/// Pyth program that must own vault price feeds.
#[account]
#[derive(Default)]
pub struct ProtocolConfig {
    pub admin: Pubkey,
    pub pending_admin: Pubkey, // Pubkey::default() when no transfer is pending
    pub oracle_program: Pubkey,
    pub bump: u8,
}

impl ProtocolConfig {
    pub const SEED: &'static [u8] = b"config";
    pub const LEN: usize = 8 + 32 * 3 + 1 + 64; // + padding for future roles
}
