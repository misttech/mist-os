// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    fs::{syscalls::sys_renameat2, FdNumber},
    task::{syscalls::do_clone, CurrentTask},
    types::{clone_args, pid_t, Errno, UserAddress, UserCString, UserRef, CSIGNAL},
};

/// The parameter order for `clone` varies by architecture.
pub fn sys_clone(
    current_task: &CurrentTask,
    flags: u64,
    user_stack: UserAddress,
    user_parent_tid: UserRef<pid_t>,
    user_tls: UserAddress,
    user_child_tid: UserRef<pid_t>,
) -> Result<pid_t, Errno> {
    // Our flags parameter uses the low 8 bits (CSIGNAL mask) of flags to indicate the exit
    // signal. The CloneArgs struct separates these as `flags` and `exit_signal`.
    do_clone(
        current_task,
        &clone_args {
            flags: flags & !(CSIGNAL as u64),
            child_tid: user_child_tid.addr().ptr() as u64,
            parent_tid: user_parent_tid.addr().ptr() as u64,
            exit_signal: flags & (CSIGNAL as u64),
            stack: user_stack.ptr() as u64,
            tls: user_tls.ptr() as u64,
            ..Default::default()
        },
    )
}

pub fn sys_renameat(
    current_task: &CurrentTask,
    old_dir_fd: FdNumber,
    old_user_path: UserCString,
    new_dir_fd: FdNumber,
    new_user_path: UserCString,
) -> Result<(), Errno> {
    sys_renameat2(current_task, old_dir_fd, old_user_path, new_dir_fd, new_user_path, 0)
}
