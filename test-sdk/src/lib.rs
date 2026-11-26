//! TEST ONLY: `solana_sdk`-style paths for the programs' integration tests
//! (see Cargo.toml for why this is not solana-sdk).

pub mod account {
    pub use solana_account::*;
}
pub mod account_info {
    pub use solana_account_info::*;
}
pub mod bpf_loader_upgradeable {
    pub use solana_loader_v3_interface::state::UpgradeableLoaderState;
    pub use solana_sdk_ids::bpf_loader_upgradeable::{check_id, id, ID};
}
pub mod clock {
    pub use solana_clock::*;
}
pub mod compute_budget {
    pub use solana_compute_budget_interface::*;
}
pub mod entrypoint {
    pub use solana_program_error::ProgramResult;
}
pub mod instruction {
    pub use solana_instruction_error::InstructionError;
    pub use solana_instruction::{AccountMeta, Instruction};
}
/// CPI that reaches program-test's syscall stubs when run natively.
pub mod program {
    pub use solana_program::program::{invoke, invoke_signed};
}
pub mod program_pack {
    pub use solana_program_pack::*;
}
/// The syscall stubs solana-program-test 4.4 installs (solana-sysvar 5).
pub mod program_stubs {
    pub use solana_sysvar::program_stubs::*;
}
pub mod pubkey {
    pub use solana_pubkey::*;
}
pub mod signature {
    pub use solana_keypair::Keypair;
    pub use solana_signer::Signer;
}
pub mod system_instruction {
    pub use solana_system_interface::instruction::*;
}
pub mod system_program {
    pub use solana_sdk_ids::system_program::*;
}
pub mod sysvar {
    pub use solana_sdk_ids::sysvar::*;
}
pub mod transaction {
    pub use solana_transaction::Transaction;
    pub use solana_transaction_error::TransactionError;
}

/// Native runs: let the v3-cohort programs reach program-test's syscall stubs.
pub mod native_stubs;
