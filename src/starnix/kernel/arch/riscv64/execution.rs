// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::task::{CurrentTask, ThreadState};
use starnix_syscalls::{
    decls::{Syscall, SyscallDecl},
    SyscallArg,
};

/// Generates CFI directives so the unwinder will be redirected to unwind the stack provided in
/// `state`.
#[macro_export]
macro_rules! generate_cfi_directives {
    ($state:expr) => {
        unsafe {
            let state_addr = std::ptr::addr_of!($state);
            // The base address that the unwinder will use is stored in s11. Then it will look for
            // each register value at an offset specified below. These offsets match the offsets of
            // the register values in the `zx_restricted_state_t` struct.
            std::arch::asm!(
                ".cfi_remember_state",
                ".cfi_def_cfa s11, 0",
                ".cfi_offset 64, 0",  // GNU Assembler doesn't recognize pc.
                ".cfi_offset ra, 0x08",
                ".cfi_offset sp, 0x10",
                ".cfi_offset gp, 0x18",
                ".cfi_offset tp, 0x20",
                ".cfi_offset t0, 0x28",
                ".cfi_offset t1, 0x30",
                ".cfi_offset t2, 0x38",
                ".cfi_offset s0, 0x40",
                ".cfi_offset s1, 0x48",
                ".cfi_offset a0, 0x50",
                ".cfi_offset a1, 0x58",
                ".cfi_offset a2, 0x60",
                ".cfi_offset a3, 0x68",
                ".cfi_offset a4, 0x70",
                ".cfi_offset a5, 0x78",
                ".cfi_offset a6, 0x80",
                ".cfi_offset a7, 0x88",
                ".cfi_offset s2, 0x90",
                ".cfi_offset s3, 0x98",
                ".cfi_offset s4, 0xA0",
                ".cfi_offset s5, 0xA8",
                ".cfi_offset s6, 0xB0",
                ".cfi_offset s7, 0xB8",
                ".cfi_offset s8, 0xC0",
                ".cfi_offset s9, 0xC8",
                ".cfi_offset s10, 0xD0",
                ".cfi_offset s11, 0xD8",
                ".cfi_offset t3, 0xE0",
                ".cfi_offset t4, 0xE8",
                ".cfi_offset t5, 0xF0",
                ".cfi_offset t6, 0xF8",

                // s11 could technically get clobbered between here and `execute_syscall`.
                // TODO(https://fxbug.dev/297897817): Use a more robust approach to unwind.
                in("s11") state_addr,
                options(nomem, preserves_flags, nostack),
            );
        }
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

pub fn new_syscall_from_state(syscall_decl: SyscallDecl, thread_state: &ThreadState) -> Syscall {
    Syscall {
        decl: syscall_decl,
        arg0: SyscallArg::from_raw(thread_state.registers.a0),
        arg1: SyscallArg::from_raw(thread_state.registers.a1),
        arg2: SyscallArg::from_raw(thread_state.registers.a2),
        arg3: SyscallArg::from_raw(thread_state.registers.a3),
        arg4: SyscallArg::from_raw(thread_state.registers.a4),
        arg5: SyscallArg::from_raw(thread_state.registers.a5),
    }
}

pub fn new_syscall(syscall_decl: SyscallDecl, current_task: &CurrentTask) -> Syscall {
    new_syscall_from_state(syscall_decl, &current_task.thread_state)
}
