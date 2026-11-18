use anchor_lang::prelude::*;

/// Protocol-wide roles, created once by the program's upgrade authority.
///
/// - `admin` creates markets and changes their settings (including settlement).
///   It starts as the upgrade authority and can be handed to another key or a
///   multisig with propose_admin / accept_admin.
/// - `guardian` can only pause markets, for fast response in emergencies.
/// - `oracle_program` is the Pyth program; market price feeds must be owned by it.
#[account]
#[derive(Default)]
pub struct ProtocolConfig {
    pub admin: Pubkey,
    pub pending_admin: Pubkey, // Pubkey::default() when no transfer is pending
    pub guardian: Pubkey,
    pub oracle_program: Pubkey,
    pub bump: u8,
}

impl ProtocolConfig {
    pub const SEED: &'static [u8] = b"config";
    pub const LEN: usize = 8 + 32 * 4 + 1 + 64; // + padding for future roles

    pub fn can_pause(&self, signer: &Pubkey) -> bool {
        *signer == self.admin || *signer == self.guardian
    }
}
