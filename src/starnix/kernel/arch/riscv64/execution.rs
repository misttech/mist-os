// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::task::{CurrentTask, ThreadState};
use starnix_syscalls::decls::{Syscall, SyscallDecl};
use starnix_syscalls::SyscallArg;

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
