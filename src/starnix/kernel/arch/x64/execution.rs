// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    syscalls::decls::{Syscall, SyscallDecl},
    task::CurrentTask,
};

/// Generates CFI directives so the unwinder will be redirected to unwind the stack provided in `state`.
#[macro_export]
macro_rules! generate_cfi_directives {
    ($state:expr) => {
        unsafe {
            let state_addr = std::ptr::addr_of!($state);
            // The base address that the unwinder will use is stored in r14. Then it will look for
            // each register value at an offset specified below. These offsets match the offsets of
            // the register values in the `zx_restricted_state_t` struct.
            std::arch::asm!(
                ".cfi_remember_state",
                ".cfi_def_cfa r14, 0",
                ".cfi_offset rdi, 0",
                ".cfi_offset rsi, 0x08",
                ".cfi_offset rbp, 0x10",
                ".cfi_offset rbx, 0x18",
                ".cfi_offset rdx, 0x20",
                ".cfi_offset rcx, 0x28",
                ".cfi_offset rax, 0x30",
                ".cfi_offset rsp, 0x38",
                ".cfi_offset r8, 0x40",
                ".cfi_offset r9, 0x48",
                ".cfi_offset r10, 0x50",
                ".cfi_offset r11, 0x58",
                ".cfi_offset r12, 0x60",
                ".cfi_offset r13, 0x68",
                ".cfi_offset r14, 0x70",
                ".cfi_offset r15, 0x78",
                ".cfi_offset rip, 0x80",
                // zxdb doesn't support unwinding these registers yet.
                // ".cfi_offset rflags, 0x88",
                // ".cfi_offset fs.base, 0x90",
                // ".cfi_offset gs.base, 0x98",

                // r14 could technically get clobbered between here and `execute_syscall`. We should
                // use a method for computing `.cfi_def_cfa` that can't fail (e.g., rsp offset).
                in("r14") state_addr,
                options(nomem, preserves_flags, nostack),
            );
        }
    };
}

/// Generates directives to restore the CFI state.
macro_rules! restore_cfi_directives {
    () => {
        unsafe {
            // Restore the CFI state before continuing.
            std::arch::asm!(".cfi_restore_state", options(nomem, preserves_flags, nostack));
        }
    };
}

pub(crate) use generate_cfi_directives;
pub(crate) use restore_cfi_directives;

impl Syscall {
    /// Populates the syscall parameters from the x64 registers.
    pub fn new(syscall_decl: SyscallDecl, current_task: &CurrentTask) -> Syscall {
        Syscall {
            decl: syscall_decl,
            arg0: current_task.registers.rdi,
            arg1: current_task.registers.rsi,
            arg2: current_task.registers.rdx,
            arg3: current_task.registers.r10,
            arg4: current_task.registers.r8,
            arg5: current_task.registers.r9,
        }
    }
}
