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
    pub fn save_registers_for_restart(&mut self, _syscall_number: u64) {
        // The x0 register may be clobbered during syscall handling (for the return value), but is
        // needed when restarting a syscall.
        self.orig_x0 = self.r[0];
    }

    /// Custom restart, invoke restart_syscall instead of the original syscall.
    pub fn prepare_for_custom_restart(&mut self) {
        self.r[8] = __NR_restart_syscall as u64;
    }

    /// Restores x0 to match its value before restarting. This needs to be done when restarting
    /// syscalls because x0 may have been overwritten in the syscall dispatch loop.
    pub fn restore_original_return_register(&mut self) {
        self.r[0] = self.orig_x0;
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
        if (self.real_registers.cpsr & zx::sys::ZX_REG_CPSR_ARCH_32_MASK) != 0 {
            self.real_registers.r[15] = new_ip;
        }
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
        if (self.real_registers.cpsr & zx::sys::ZX_REG_CPSR_ARCH_32_MASK) != 0 {
            self.real_registers.r[13] = sp;
        }
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
        if (self.real_registers.cpsr & zx::sys::ZX_REG_CPSR_ARCH_32_MASK) != 0 {
            self.real_registers.r[7]
        } else {
            self.real_registers.r[8]
        }
    }

    /// Resets the register that contains the application status flags.
    pub fn reset_flags(&mut self) {
        // Reset all the flags except the aarch32 and thumb bits.
        self.real_registers.cpsr = self.real_registers.cpsr & 0x30;
    }

    /// Executes the given predicate on the register.
    pub fn apply_user_register(
        &mut self,
        offset: usize,
        f: &mut dyn FnMut(&mut u64),
    ) -> Result<(), Errno> {
        fn reg_offset(index: usize) -> usize {
            memoffset::offset_of!(user_regs_struct, regs) + index * std::mem::size_of::<u64>()
        }

        let is_arm: bool = self.real_registers.cpsr & zx::sys::ZX_REG_CPSR_ARCH_32_MASK != 0;
        if offset >= std::mem::size_of::<user_regs_struct>() {
            return error!(EINVAL);
        }
        if offset == memoffset::offset_of!(user_regs_struct, sp)
            || (offset == reg_offset(13) && is_arm)
        {
            f(&mut self.real_registers.sp);
            // For arm, sp is register 13
            if is_arm {
                self.real_registers.r[13] = self.real_registers.sp;
            }
        } else if offset == memoffset::offset_of!(user_regs_struct, pc)
            || (offset == reg_offset(15) && is_arm)
        {
            f(&mut self.real_registers.pc);
            // For arm, pc is register 15
            if is_arm {
                self.real_registers.r[15] = self.real_registers.pc;
            }
        } else if offset == memoffset::offset_of!(user_regs_struct, pstate) {
            f(&mut self.real_registers.cpsr);
        } else if offset == reg_offset(30) || (offset == reg_offset(14) && is_arm) {
            // The 30th register is stored as lr in self.real_registers
            f(&mut self.real_registers.lr);
            if is_arm {
                // The 14th register is stored as lr in self.real_registers for
                // arm
                self.real_registers.r[14] = self.real_registers.lr;
            }
        } else if offset % std::mem::align_of::<u64>() == 0 {
            let index = (offset - memoffset::offset_of!(user_regs_struct, regs)) >> 3;
            f(&mut self.real_registers.r[index])
        } else {
            return error!(EINVAL);
        };
        Ok(())
    }
}

impl From<zx::sys::zx_thread_state_general_regs_t> for RegisterState {
    fn from(mut regs: zx::sys::zx_thread_state_general_regs_t) -> Self {
        // We should synchronize the stack pointer with the aarch32 registers.
        if regs.cpsr & zx::sys::ZX_REG_CPSR_ARCH_32_MASK != 0 {
            regs.sp = regs.r[13];
            regs.lr = regs.r[14];
            // The PC appears to advance properly and _not_ prefer r[15]
            // TODO(https://fxbug.dev/380402551): Make sure this isn't because of anything
            // done in zircon.
            regs.r[15] = regs.pc;
        }
        RegisterState { real_registers: regs, orig_x0: regs.r[0], elr: 0 }
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
        let regs = register_state.real_registers;
        // This is primarily called when returning from restricted mode.
        // Check that the special registers stayed synchronized.
        if register_state.cpsr & zx::sys::ZX_REG_CPSR_ARCH_32_MASK != 0 {
            assert_eq!(regs.sp, regs.r[13]);
            assert_eq!(regs.lr, regs.r[14]);
            assert_eq!(regs.pc, regs.r[15]);
        }
        regs
    }
}
