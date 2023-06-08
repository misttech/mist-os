// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon as zx;

use crate::syscalls::decls::Syscall;
use crate::types::uapi::user_regs_struct;

/// The state of the task's registers when the thread of execution entered the kernel.
/// This is a thin wrapper around [`zx::sys::zx_thread_state_general_regs_t`].
///
/// Implements [`std::ops::Deref`] and [`std::ops::DerefMut`] as a way to get at the underlying
/// [`zx::sys::zx_thread_state_general_regs_t`] that this type wraps.
#[derive(Default, Debug, Clone, Copy, Eq, PartialEq)]
pub struct RegisterState {
    real_registers: zx::sys::zx_thread_state_general_regs_t,

    /// A copy of the `a0` register at the time of the `syscall` instruction. This is
    /// important to store, as the return value of a syscall overwrites `a0`, making it impossible
    /// to recover the original value in the case of syscall restart and strace output.
    pub orig_a0: u64,
}

impl RegisterState {
    /// Saves any register state required to restart `syscall`.
    pub fn save_registers_for_restart(&mut self, _syscall: &Syscall) {
        // The x0 register may be clobbered during syscall handling (for the return value), but is
        // needed when restarting a syscall.
        self.orig_a0 = self.a0;
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
        self.real_registers.a0
    }

    /// Sets the register that indicates the single-machine-word return value from a
    /// function call.
    pub fn set_return_register(&mut self, return_value: u64) {
        self.real_registers.a0 = return_value;
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
        self.real_registers.tp = tp;
    }

    /// Sets the register that indicates the first argument to a function.
    pub fn set_arg0_register(&mut self, x0: u64) {
        self.real_registers.a0 = x0;
    }

    /// Sets the register that indicates the second argument to a function.
    pub fn set_arg1_register(&mut self, x1: u64) {
        self.real_registers.a1 = x1;
    }

    /// Sets the register that indicates the third argument to a function.
    pub fn set_arg2_register(&mut self, x2: u64) {
        self.real_registers.a0 = x2;
    }

    /// Returns the register that contains the syscall number.
    pub fn syscall_register(&self) -> u64 {
        self.real_registers.a7
    }

    /// Sets the register that contains the application status flags.
    pub fn set_flags_register(&mut self, _flags: u64) {
        // TODO(fxbug.dev/128554): There is no flags register in RISC-V. Do we need this method?
    }

    pub fn to_user_regs_struct(self) -> user_regs_struct {
        user_regs_struct {
            pc: self.real_registers.pc,
            ra: self.real_registers.ra,
            sp: self.real_registers.sp,
            gp: self.real_registers.gp,
            tp: self.real_registers.tp,
            t0: self.real_registers.t0,
            t1: self.real_registers.t1,
            t2: self.real_registers.t2,
            s0: self.real_registers.s0,
            s1: self.real_registers.s1,
            a0: self.real_registers.a0,
            a1: self.real_registers.a1,
            a2: self.real_registers.a2,
            a3: self.real_registers.a3,
            a4: self.real_registers.a4,
            a5: self.real_registers.a5,
            a6: self.real_registers.a6,
            a7: self.real_registers.a7,
            s2: self.real_registers.s2,
            s3: self.real_registers.s3,
            s4: self.real_registers.s4,
            s5: self.real_registers.s5,
            s6: self.real_registers.s6,
            s7: self.real_registers.s7,
            s8: self.real_registers.s8,
            s9: self.real_registers.s9,
            s10: self.real_registers.s10,
            s11: self.real_registers.s11,
            t3: self.real_registers.t3,
            t4: self.real_registers.t4,
            t5: self.real_registers.t5,
            t6: self.real_registers.t6,
        }
    }
}

impl From<zx::sys::zx_thread_state_general_regs_t> for RegisterState {
    fn from(regs: zx::sys::zx_thread_state_general_regs_t) -> Self {
        RegisterState { real_registers: regs, orig_a0: 0 }
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
