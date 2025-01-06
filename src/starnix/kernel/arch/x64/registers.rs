// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use starnix_uapi::errors::Errno;
use starnix_uapi::{__NR_restart_syscall, error, user_regs_struct};

/// The state of the task's registers when the thread of execution entered the kernel.
/// This is a thin wrapper around [`zx::sys::zx_thread_state_general_regs_t`].
///
/// Implements [`std::ops::Deref`] and [`std::ops::DerefMut`] as a way to get at the underlying
/// [`zx::sys::zx_thread_state_general_regs_t`] that this type wraps.
#[derive(Default, Debug, Clone, Copy, Eq, PartialEq)]
pub struct RegisterState {
    real_registers: zx::sys::zx_thread_state_general_regs_t,

    /// A copy of the x64 `rax` register at the time of the `syscall` instruction. This is important
    /// to store, as the return value of a syscall overwrites `rax`, making it impossible to recover
    /// the original syscall number in the case of syscall restart and strace output.
    pub orig_rax: u64,
}

impl RegisterState {
    /// Saves any register state required to restart `syscall_number`.
    pub fn save_registers_for_restart(&mut self, syscall_number: u64) {
        // The `rax` register read from the thread's state is clobbered by
        // zircon with ZX_ERR_BAD_SYSCALL.  Similarly, Linux sets it to ENOSYS
        // until it has determined the correct return value for the syscall; we
        // emulate this behavior because ptrace callers expect it.
        self.rax = -(starnix_uapi::ENOSYS as i64) as u64;

        // `orig_rax` should hold the original value loaded into `rax` by the userspace process.
        self.orig_rax = syscall_number;
    }

    /// Custom restart, invoke restart_syscall instead of the original syscall.
    pub fn prepare_for_custom_restart(&mut self) {
        self.rax = __NR_restart_syscall as u64;
    }

    /// Restores `rax` to match its value before restarting.
    pub fn restore_original_return_register(&mut self) {
        self.rax = self.orig_rax;
    }

    /// Returns the register that indicates the single-machine-word return value from a
    /// function call.
    pub fn instruction_pointer_register(&self) -> u64 {
        self.real_registers.rip
    }

    /// Sets the register that indicates the single-machine-word return value from a
    /// function call.
    pub fn set_instruction_pointer_register(&mut self, new_ip: u64) {
        self.real_registers.rip = new_ip;
    }

    /// Returns the register that indicates the single-machine-word return value from a
    /// function call.
    pub fn return_register(&self) -> u64 {
        self.real_registers.rax
    }

    /// Sets the register that indicates the single-machine-word return value from a
    /// function call.
    pub fn set_return_register(&mut self, return_value: u64) {
        self.real_registers.rax = return_value;
    }

    /// Gets the register that indicates the current stack pointer.
    pub fn stack_pointer_register(&self) -> u64 {
        self.real_registers.rsp
    }

    /// Sets the register that indicates the current stack pointer.
    pub fn set_stack_pointer_register(&mut self, sp: u64) {
        self.real_registers.rsp = sp;
    }

    /// Sets the register that indicates the TLS.
    pub fn set_thread_pointer_register(&mut self, tp: u64) {
        self.real_registers.fs_base = tp;
    }

    /// Sets the register that indicates the first argument to a function.
    pub fn set_arg0_register(&mut self, rdi: u64) {
        self.real_registers.rdi = rdi;
    }

    /// Sets the register that indicates the second argument to a function.
    pub fn set_arg1_register(&mut self, rsi: u64) {
        self.real_registers.rsi = rsi;
    }

    /// Sets the register that indicates the third argument to a function.
    pub fn set_arg2_register(&mut self, rdx: u64) {
        self.real_registers.rdx = rdx;
    }

    /// Returns the register that contains the syscall number.
    pub fn syscall_register(&self) -> u64 {
        self.orig_rax
    }

    /// Resets the register that contains the application status flags.
    pub fn reset_flags(&mut self) {
        self.real_registers.rflags = 0;
    }

    /// Executes the given predicate on the register.
    pub fn apply_user_register(
        &mut self,
        offset: usize,
        f: &mut dyn FnMut(&mut u64),
    ) -> Result<(), Errno> {
        if offset == memoffset::offset_of!(user_regs_struct, r15) {
            f(&mut self.real_registers.r15);
        } else if offset == memoffset::offset_of!(user_regs_struct, r14) {
            f(&mut self.real_registers.r14);
        } else if offset == memoffset::offset_of!(user_regs_struct, r13) {
            f(&mut self.real_registers.r13);
        } else if offset == memoffset::offset_of!(user_regs_struct, r12) {
            f(&mut self.real_registers.r12);
        } else if offset == memoffset::offset_of!(user_regs_struct, rbp) {
            f(&mut self.real_registers.rbp);
        } else if offset == memoffset::offset_of!(user_regs_struct, rbx) {
            f(&mut self.real_registers.rbx);
        } else if offset == memoffset::offset_of!(user_regs_struct, r11) {
            f(&mut self.real_registers.r11);
        } else if offset == memoffset::offset_of!(user_regs_struct, r10) {
            f(&mut self.real_registers.r10);
        } else if offset == memoffset::offset_of!(user_regs_struct, r9) {
            f(&mut self.real_registers.r9);
        } else if offset == memoffset::offset_of!(user_regs_struct, r8) {
            f(&mut self.real_registers.r8);
        } else if offset == memoffset::offset_of!(user_regs_struct, rax) {
            f(&mut self.real_registers.rax);
        } else if offset == memoffset::offset_of!(user_regs_struct, rcx) {
            f(&mut self.real_registers.rcx);
        } else if offset == memoffset::offset_of!(user_regs_struct, rdx) {
            f(&mut self.real_registers.rdx);
        } else if offset == memoffset::offset_of!(user_regs_struct, rsi) {
            f(&mut self.real_registers.rsi);
        } else if offset == memoffset::offset_of!(user_regs_struct, rdi) {
            f(&mut self.real_registers.rdi);
        } else if offset == memoffset::offset_of!(user_regs_struct, orig_rax) {
            f(&mut self.orig_rax);
        } else if offset == memoffset::offset_of!(user_regs_struct, rip) {
            f(&mut self.real_registers.rip);
        } else if offset == memoffset::offset_of!(user_regs_struct, cs) {
            let mut val = 0;
            f(&mut val);
        } else if offset == memoffset::offset_of!(user_regs_struct, eflags) {
            f(&mut self.real_registers.rflags);
        } else if offset == memoffset::offset_of!(user_regs_struct, rsp) {
            f(&mut self.real_registers.rsp);
        } else if offset == memoffset::offset_of!(user_regs_struct, ss) {
            let mut val = 0;
            f(&mut val);
        } else if offset == memoffset::offset_of!(user_regs_struct, fs_base) {
            f(&mut self.real_registers.fs_base);
        } else if offset == memoffset::offset_of!(user_regs_struct, gs_base) {
            f(&mut self.real_registers.gs_base);
        } else if offset == memoffset::offset_of!(user_regs_struct, ds) {
            let mut val = 0;
            f(&mut val);
        } else if offset == memoffset::offset_of!(user_regs_struct, es) {
            let mut val = 0;
            f(&mut val);
        } else if offset == memoffset::offset_of!(user_regs_struct, fs) {
            let mut val = 0;
            f(&mut val);
        } else if offset == memoffset::offset_of!(user_regs_struct, gs) {
            let mut val = 0;
            f(&mut val);
        } else {
            return error!(EINVAL);
        };
        Ok(())
    }
}

impl From<zx::sys::zx_thread_state_general_regs_t> for RegisterState {
    fn from(regs: zx::sys::zx_thread_state_general_regs_t) -> Self {
        RegisterState { real_registers: regs, orig_rax: regs.rax }
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
