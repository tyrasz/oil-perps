//! Native mode only: solana-program-test leaves `sol_log_data` at
//! solana-program's default stub, which println!s every Anchor event (one per
//! venue fill). Wrap the installed stubs and drop event data; everything else
//! goes to program-test's stubs unchanged. SBF runs log events normally.

use solana_sdk::account_info::AccountInfo;
use solana_sdk::entrypoint::ProgramResult;
use solana_sdk::instruction::Instruction;
use solana_sdk::program_stubs::{set_syscall_stubs, SyscallStubs};
use solana_sdk::pubkey::Pubkey;
use std::sync::{Arc, RwLock};

struct QuietEvents(Arc<RwLock<Option<Box<dyn SyscallStubs>>>>);

impl QuietEvents {
    fn inner<R>(&self, f: impl FnOnce(&dyn SyscallStubs) -> R) -> R {
        let guard = self.0.read().unwrap();
        f(guard.as_deref().expect("program-test stubs"))
    }
}

impl SyscallStubs for QuietEvents {
    fn sol_log(&self, message: &str) {
        self.inner(|s| s.sol_log(message))
    }
    fn sol_log_compute_units(&self) {
        self.inner(|s| s.sol_log_compute_units())
    }
    fn sol_invoke_signed(&self, instruction: &Instruction, account_infos: &[AccountInfo], signers_seeds: &[&[&[u8]]]) -> ProgramResult {
        self.inner(|s| s.sol_invoke_signed(instruction, account_infos, signers_seeds))
    }
    fn sol_get_clock_sysvar(&self, var_addr: *mut u8) -> u64 {
        self.inner(|s| s.sol_get_clock_sysvar(var_addr))
    }
    fn sol_get_epoch_schedule_sysvar(&self, var_addr: *mut u8) -> u64 {
        self.inner(|s| s.sol_get_epoch_schedule_sysvar(var_addr))
    }
    fn sol_get_fees_sysvar(&self, var_addr: *mut u8) -> u64 {
        self.inner(|s| s.sol_get_fees_sysvar(var_addr))
    }
    fn sol_get_rent_sysvar(&self, var_addr: *mut u8) -> u64 {
        self.inner(|s| s.sol_get_rent_sysvar(var_addr))
    }
    unsafe fn sol_memcpy(&self, dst: *mut u8, src: *const u8, n: usize) {
        self.inner(|s| s.sol_memcpy(dst, src, n))
    }
    unsafe fn sol_memmove(&self, dst: *mut u8, src: *const u8, n: usize) {
        self.inner(|s| s.sol_memmove(dst, src, n))
    }
    unsafe fn sol_memcmp(&self, s1: *const u8, s2: *const u8, n: usize, result: *mut i32) {
        self.inner(|s| s.sol_memcmp(s1, s2, n, result))
    }
    unsafe fn sol_memset(&self, dst: *mut u8, c: u8, n: usize) {
        self.inner(|s| s.sol_memset(dst, c, n))
    }
    fn sol_get_return_data(&self) -> Option<(Pubkey, Vec<u8>)> {
        self.inner(|s| s.sol_get_return_data())
    }
    fn sol_set_return_data(&self, data: &[u8]) {
        self.inner(|s| s.sol_set_return_data(data))
    }
    fn sol_log_data(&self, _fields: &[&[u8]]) {}
    fn sol_get_processed_sibling_instruction(&self, index: usize) -> Option<Instruction> {
        self.inner(|s| s.sol_get_processed_sibling_instruction(index))
    }
    fn sol_get_stack_height(&self) -> u64 {
        self.inner(|s| s.sol_get_stack_height())
    }
}

/// Call after ProgramTest has started (it installs its stubs then).
pub fn install() {
    let slot = Arc::new(RwLock::new(None));
    let previous = set_syscall_stubs(Box::new(QuietEvents(slot.clone())));
    *slot.write().unwrap() = Some(previous);
}
