//! Native (non-SBF) runs of the programs under solana-program-test 4.4.
//!
//! program-test installs its syscall stubs (CPI, sysvars, return data, stack
//! height) into `solana-sysvar` 5, the cohort its runtime is built on. The
//! programs are built on the solana-v3 cohort (Anchor 1.2's default, and
//! solana-program 3 for mock-pyth), whose `Clock::get`, `invoke_signed`
//! (through `solana-invoke`) and `get_stack_height` reach the stubs of
//! `solana-sysvar` 3 instead, which nobody sets: natively a program would see
//! no clock and every CPI would be a silent no-op. `install` forwards the v3
//! stubs to program-test's. (The v4 cohort is no way out: `solana-sysvar` 5
//! reads sysvars through `solana-get-sysvar`, which has no native stub at
//! all.)
//!
//! What cannot be forwarded: `msg!` (solana-msg prints to stdout natively)
//! and Anchor's `emit!` (its native `sol_log_data` discards the data), so a
//! native run has no `msg!` lines, no return data and no `emit!` events.
//! perps-core's events still reach the transaction log natively: its
//! `emit_event!` (programs/perps-core/src/lib.rs) also calls the v3
//! `sol_log` with "Program data: <base64>", which `install` forwards
//! (program-test records it as "Program log: Program data: …").

use solana_sysvar::program_stubs as v5;
use solana_sysvar_v3::program_stubs as v3;
use std::sync::Arc;

type V5 = Arc<Box<dyn v5::SyscallStubs>>;

struct Shared(V5);
#[allow(clippy::not_unsafe_ptr_arg_deref)]
impl v5::SyscallStubs for Shared {
    fn sol_log(&self, m: &str) { self.0.sol_log(m) }
    fn sol_log_compute_units(&self) { self.0.sol_log_compute_units() }
    fn sol_remaining_compute_units(&self) -> u64 { self.0.sol_remaining_compute_units() }
    fn sol_invoke_signed(&self, ix: &solana_instruction::Instruction, infos: &[solana_account_info::AccountInfo], seeds: &[&[&[u8]]]) -> solana_program_error::ProgramResult {
        self.0.sol_invoke_signed(ix, infos, seeds)
    }
    fn sol_get_sysvar(&self, id: *const u8, a: *mut u8, o: u64, l: u64) -> u64 { self.0.sol_get_sysvar(id, a, o, l) }
    fn sol_get_clock_sysvar(&self, a: *mut u8) -> u64 { self.0.sol_get_clock_sysvar(a) }
    fn sol_get_epoch_schedule_sysvar(&self, a: *mut u8) -> u64 { self.0.sol_get_epoch_schedule_sysvar(a) }
    fn sol_get_fees_sysvar(&self, a: *mut u8) -> u64 { self.0.sol_get_fees_sysvar(a) }
    fn sol_get_rent_sysvar(&self, a: *mut u8) -> u64 { self.0.sol_get_rent_sysvar(a) }
    fn sol_get_epoch_rewards_sysvar(&self, a: *mut u8) -> u64 { self.0.sol_get_epoch_rewards_sysvar(a) }
    fn sol_get_last_restart_slot(&self, a: *mut u8) -> u64 { self.0.sol_get_last_restart_slot(a) }
    fn sol_get_epoch_stake(&self, v: *const u8) -> u64 { self.0.sol_get_epoch_stake(v) }
    unsafe fn sol_memcpy(&self, d: *mut u8, s: *const u8, n: usize) { self.0.sol_memcpy(d, s, n) }
    unsafe fn sol_memmove(&self, d: *mut u8, s: *const u8, n: usize) { self.0.sol_memmove(d, s, n) }
    unsafe fn sol_memcmp(&self, a: *const u8, b: *const u8, n: usize, r: *mut i32) { self.0.sol_memcmp(a, b, n, r) }
    unsafe fn sol_memset(&self, s: *mut u8, c: u8, n: usize) { self.0.sol_memset(s, c, n) }
    fn sol_get_return_data(&self) -> Option<(solana_pubkey::Pubkey, Vec<u8>)> { self.0.sol_get_return_data() }
    fn sol_set_return_data(&self, d: &[u8]) { self.0.sol_set_return_data(d) }
    fn sol_log_data(&self, f: &[&[u8]]) { self.0.sol_log_data(f) }
    fn sol_get_processed_sibling_instruction(&self, i: usize) -> Option<solana_instruction::Instruction> { self.0.sol_get_processed_sibling_instruction(i) }
    fn sol_get_stack_height(&self) -> u64 { self.0.sol_get_stack_height() }
}

struct Forward(V5);
#[allow(clippy::not_unsafe_ptr_arg_deref)]
impl v3::SyscallStubs for Forward {
    fn sol_log(&self, m: &str) { self.0.sol_log(m) }
    fn sol_log_compute_units(&self) { self.0.sol_log_compute_units() }
    fn sol_remaining_compute_units(&self) -> u64 { self.0.sol_remaining_compute_units() }
    fn sol_invoke_signed(&self, ix: &solana_instruction::Instruction, infos: &[solana_account_info::AccountInfo], seeds: &[&[&[u8]]]) -> solana_program_error::ProgramResult {
        self.0.sol_invoke_signed(ix, infos, seeds)
    }
    fn sol_get_sysvar(&self, id: *const u8, a: *mut u8, o: u64, l: u64) -> u64 { self.0.sol_get_sysvar(id, a, o, l) }
    fn sol_get_clock_sysvar(&self, a: *mut u8) -> u64 { self.0.sol_get_clock_sysvar(a) }
    fn sol_get_epoch_schedule_sysvar(&self, a: *mut u8) -> u64 { self.0.sol_get_epoch_schedule_sysvar(a) }
    fn sol_get_fees_sysvar(&self, a: *mut u8) -> u64 { self.0.sol_get_fees_sysvar(a) }
    fn sol_get_rent_sysvar(&self, a: *mut u8) -> u64 { self.0.sol_get_rent_sysvar(a) }
    fn sol_get_epoch_rewards_sysvar(&self, a: *mut u8) -> u64 { self.0.sol_get_epoch_rewards_sysvar(a) }
    fn sol_get_last_restart_slot(&self, a: *mut u8) -> u64 { self.0.sol_get_last_restart_slot(a) }
    fn sol_get_epoch_stake(&self, v: *const u8) -> u64 { self.0.sol_get_epoch_stake(v) }
    unsafe fn sol_memcpy(&self, d: *mut u8, s: *const u8, n: usize) { self.0.sol_memcpy(d, s, n) }
    unsafe fn sol_memmove(&self, d: *mut u8, s: *const u8, n: usize) { self.0.sol_memmove(d, s, n) }
    unsafe fn sol_memcmp(&self, a: *const u8, b: *const u8, n: usize, r: *mut i32) { self.0.sol_memcmp(a, b, n, r) }
    unsafe fn sol_memset(&self, s: *mut u8, c: u8, n: usize) { self.0.sol_memset(s, c, n) }
    fn sol_get_return_data(&self) -> Option<(solana_pubkey::Pubkey, Vec<u8>)> { self.0.sol_get_return_data() }
    fn sol_set_return_data(&self, d: &[u8]) { self.0.sol_set_return_data(d) }
    fn sol_log_data(&self, f: &[&[u8]]) { self.0.sol_log_data(f) }
    fn sol_get_processed_sibling_instruction(&self, i: usize) -> Option<solana_instruction::Instruction> { self.0.sol_get_processed_sibling_instruction(i) }
    fn sol_get_stack_height(&self) -> u64 { self.0.sol_get_stack_height() }
}

struct NoStubs;
impl v5::SyscallStubs for NoStubs {}

/// Forward the v3 stubs to the ones program-test installed. Call after the
/// first `ProgramTest::start*` (which installs them); later calls do nothing.
/// Harmless in SBF runs.
pub fn install() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let inner: V5 = Arc::new(v5::set_syscall_stubs(Box::new(NoStubs)));
        v5::set_syscall_stubs(Box::new(Shared(inner.clone())));
        v3::set_syscall_stubs(Box::new(Forward(inner)));
    });
}
