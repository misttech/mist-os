// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::syscalls::decls::{Syscall, SyscallDecl};
use crate::task::CurrentTask;

/// Generates CFI directives so the unwinder will be redirected to unwind the stack provided in
/// `state`.
#[macro_export]
macro_rules! generate_cfi_directives {
    ($state:expr) => {
        // TODO(fxbug.dev/128554): implement riscv support.
    };
}

/// Generates directives to restore the CFI state.
#[macro_export]
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
    /// Populates the syscall parameters from the RISC-V registers.
    pub fn new(syscall_decl: SyscallDecl, current_task: &CurrentTask) -> Syscall {
        Syscall {
            decl: syscall_decl,
            arg0: current_task.registers.a0,
            arg1: current_task.registers.a1,
            arg2: current_task.registers.a2,
            arg3: current_task.registers.a3,
            arg4: current_task.registers.a4,
            arg5: current_task.registers.a5,
        }
    }
}
