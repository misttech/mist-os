// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::sync::Arc;

use super::thread_group_hooks::{self, SeLinuxResolvedElfState};
use crate::task::{CurrentTask, Task, ThreadGroup};

use starnix_uapi::{errors::Errno, signals::Signal};

/// Check if creating a task is allowed, if SELinux is enabled. Access is allowed if SELinux is disabled.
pub fn check_task_create_access(current_task: &CurrentTask) -> Result<(), Errno> {
    match &current_task.kernel().security_server {
        None => return Ok(()),
        Some(security_server) => {
            let sid = current_task.get_current_sid();
            thread_group_hooks::check_task_create_access(
                security_server.as_ref(),
                &security_server.as_permission_check(),
                sid,
            )
        }
    }
}

/// Checks if exec is allowed, if SELinux is enabled. Access is allowed if SELinux is disabled.
pub fn check_exec_access(
    current_task: &CurrentTask,
) -> Result<Option<SeLinuxResolvedElfState>, Errno> {
    match &current_task.kernel().security_server {
        None => return Ok(None),
        Some(security_server) => {
            let group_state = current_task.thread_group.read();
            thread_group_hooks::check_exec_access(
                security_server.as_ref(),
                &security_server.as_permission_check(),
                &group_state.selinux_state,
            )
        }
    }
}

/// Updates the SELinux thread group state on exec, if SELinux is enabled. No-op if SELinux is
/// disabled.
pub fn update_state_on_exec(
    current_task: &mut CurrentTask,
    elf_selinux_state: &Option<SeLinuxResolvedElfState>,
) {
    if let Some(security_server) = &current_task.kernel().security_server {
        let mut thread_group_state = current_task.thread_group.write();
        thread_group_hooks::update_state_on_exec(
            security_server.as_ref(),
            &security_server.as_permission_check(),
            &mut thread_group_state.selinux_state,
            elf_selinux_state,
        );
    }
}

/// Checks if `source` may exercise the "getsched" permission on `target`. Access is allowed if
/// SELinux is disabled.
pub fn check_getsched_access(source: &CurrentTask, target: &Task) -> Result<(), Errno> {
    match &source.kernel().security_server {
        None => return Ok(()),
        Some(security_server) => {
            // TODO(b/323856891): Consider holding `source.thread_group` and `target.thread_group`
            // read locks for duration of access check.
            let source_sid = source.get_current_sid();
            let target_sid = target.get_current_sid();
            thread_group_hooks::check_getsched_access(
                security_server.as_ref(),
                &security_server.as_permission_check(),
                source_sid,
                target_sid,
            )
        }
    }
}

/// Checks if setsched is allowed, if SELinux is enabled. Access is allowed if SELinux is disabled.
pub fn check_setsched_access(source: &CurrentTask, target: &Task) -> Result<(), Errno> {
    match &source.kernel().security_server {
        None => return Ok(()),
        Some(security_server) => {
            let source_sid = source.get_current_sid();
            let target_sid = target.get_current_sid();
            thread_group_hooks::check_setsched_access(
                security_server.as_ref(),
                &security_server.as_permission_check(),
                source_sid,
                target_sid,
            )
        }
    }
}

/// Checks if getpgid is allowed, if SELinux is enabled. Access is allowed if SELinux is disabled.
pub fn check_getpgid_access(source_task: &CurrentTask, target_task: &Task) -> Result<(), Errno> {
    match &source_task.kernel().security_server {
        None => return Ok(()),
        Some(security_server) => {
            let source_sid = source_task.get_current_sid();
            let target_sid = target_task.get_current_sid();
            thread_group_hooks::check_getpgid_access(
                security_server.as_ref(),
                &security_server.as_permission_check(),
                source_sid,
                target_sid,
            )
        }
    }
}

/// Checks if setpgid is allowed, if SELinux is enabled. Access is allowed if SELinux is disabled.
pub fn check_setpgid_access(source_task: &CurrentTask, target_task: &Task) -> Result<(), Errno> {
    match &source_task.kernel().security_server {
        None => return Ok(()),
        Some(security_server) => {
            let source_sid = source_task.get_current_sid();
            let target_sid = target_task.get_current_sid();
            thread_group_hooks::check_setpgid_access(
                security_server.as_ref(),
                &security_server.as_permission_check(),
                source_sid,
                target_sid,
            )
        }
    }
}

/// Checks if sending a signal is allowed, if SELinux is enabled. Access is allowed if SELinux is disabled.
pub fn check_signal_access(
    source_task: &CurrentTask,
    target_task: &Task,
    signal: Signal,
) -> Result<(), Errno> {
    match &source_task.kernel().security_server {
        None => return Ok(()),
        Some(security_server) => {
            let source_sid = source_task.get_current_sid();
            let target_sid = target_task.get_current_sid();
            thread_group_hooks::check_signal_access(
                security_server.as_ref(),
                &security_server.as_permission_check(),
                source_sid,
                target_sid,
                signal,
            )
        }
    }
}

/// Checks if tracing the current task is allowed, if SELinux is enabled. Access is allowed if
/// SELinux is disabled.
pub fn check_ptrace_traceme_access(
    parent: &Arc<ThreadGroup>,
    current_task: &CurrentTask,
) -> Result<(), Errno> {
    match &current_task.kernel().security_server {
        None => return Ok(()),
        Some(security_server) => {
            let source_sid = parent.get_current_sid();
            let target_sid = current_task.get_current_sid();
            thread_group_hooks::check_ptrace_access(
                security_server.as_ref(),
                &security_server.as_permission_check(),
                source_sid,
                target_sid,
            )
        }
    }
}

/// Checks if `current_task` is allowed to trace `tracee_task`, if SELinux is enabled. Access is
/// allowed if SELinux is disabled.
pub fn check_ptrace_attach_access(
    current_task: &CurrentTask,
    tracee_task: &Task,
) -> Result<(), Errno> {
    match &current_task.kernel().security_server {
        None => return Ok(()),
        Some(security_server) => {
            let source_sid = current_task.get_current_sid();
            let target_sid = tracee_task.get_current_sid();
            thread_group_hooks::check_ptrace_access(
                security_server.as_ref(),
                &security_server.as_permission_check(),
                source_sid,
                target_sid,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::create_kernel_and_task;

    #[fuchsia::test]
    async fn task_create_access_allowed_for_selinux_disabled() {
        let (kernel, task) = create_kernel_and_task();
        assert!(kernel.security_server.is_none());
        assert_eq!(check_task_create_access(&task), Ok(()));
    }

    #[fuchsia::test]
    async fn exec_access_allowed_for_selinux_disabled() {
        let (kernel, task) = create_kernel_and_task();
        assert!(kernel.security_server.is_none());
        assert_eq!(check_exec_access(&task), Ok(None));
    }
}
