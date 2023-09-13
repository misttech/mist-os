// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon as zx;

use crate::syscalls::decls::Syscall;

/// The state of the task's registers when the thread of execution entered the kernel.
/// This is a thin wrapper around [`zx::sys::zx_thread_state_general_regs_t`].
///
/// Implements [`std::ops::Deref`] and [`std::ops::DerefMut`] as a way to get at the underlying
/// [`zx::sys::zx_thread_state_general_regs_t`] that this type wraps.
#[derive(Default, Debug, Clone, Copy, Eq, PartialEq)]
pub struct RegisterState {
    real_registers: zx::sys::zx_thread_state_general_regs_t,

    /// A copy of the aarch64 `x0` register at the time of the `syscall` instruction. This is
    /// important to store, as the return value of a syscall overwrites `x0`, making it impossible
    /// to recover the original `x0` value in the case of syscall restart and strace output.
    pub orig_x0: u64,

    /// The contents of the Exception Link Register. This register is used to jump to a code
    /// location in restricted mode, as arm64 does not allow the PC to be set directly.
    pub elr: u64,
}

impl RegisterState {
    /// Saves any register state required to restart `syscall`.
    pub fn save_registers_for_restart(&mut self, _syscall: &Syscall) {
        // The x0 register may be clobbered during syscall handling (for the return value), but is
        // needed when restarting a syscall.
        self.orig_x0 = self.r[0];
    }

    /// Returns the register that indicates the single-machine-word return value from a
    /// function call.
    pub fn instruction_pointer_register(&self) -> u64 {
        self.real_registers.pc
    }

    /// Sets the register that indicates the single-machine-word return value from a
    /// function call.
    pub fn set_instruction_pointer_register(&mut self, new_ip: u64) {
        self.real_registers.pc = new_ip;
    }

    /// Returns the register that indicates the single-machine-word return value from a
    /// function call.
    pub fn return_register(&self) -> u64 {
        self.real_registers.r[0]
    }

    /// Sets the register that indicates the single-machine-word return value from a
    /// function call.
    pub fn set_return_register(&mut self, return_value: u64) {
        self.real_registers.r[0] = return_value;
    }

    /// Gets the register that indicates the current stack pointer.
    pub fn stack_pointer_register(&self) -> u64 {
        self.real_registers.sp
    }

    /// Sets the register that indicates the current stack pointer.
    pub fn set_stack_pointer_register(&mut self, sp: u64) {
        self.real_registers.sp = sp;
    }

    /// Sets the register that indicates the TLS.
    pub fn set_thread_pointer_register(&mut self, tp: u64) {
        self.real_registers.tpidr = tp;
    }

    /// Sets the register that indicates the first argument to a function.
    pub fn set_arg0_register(&mut self, x0: u64) {
        self.real_registers.r[0] = x0;
    }

    /// Sets the register that indicates the second argument to a function.
    pub fn set_arg1_register(&mut self, x1: u64) {
        self.real_registers.r[1] = x1;
    }

    /// Sets the register that indicates the third argument to a function.
    pub fn set_arg2_register(&mut self, x2: u64) {
        self.real_registers.r[2] = x2;
    }

    /// Returns the register that contains the syscall number.
    pub fn syscall_register(&self) -> u64 {
        self.real_registers.r[8]
    }

    /// Resets the register that contains the application status flags.
    pub fn reset_flags(&mut self) {
        self.real_registers.cpsr = 0;
    }
}

impl From<zx::sys::zx_thread_state_general_regs_t> for RegisterState {
    fn from(regs: zx::sys::zx_thread_state_general_regs_t) -> Self {
        RegisterState { real_registers: regs, orig_x0: 0, elr: 0 }
    }
}

impl std::ops::Deref for RegisterState {
    type Target = zx::sys::zx_thread_state_general_regs_t;

    fn deref(&self) -> &Self::Target {
        &self.real_registers
    }
}

impl std::ops::DerefMut for RegisterState {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.real_registers
    }
}

impl From<RegisterState> for zx::sys::zx_thread_state_general_regs_t {
    fn from(register_state: RegisterState) -> Self {
        register_state.real_registers
    }
}
