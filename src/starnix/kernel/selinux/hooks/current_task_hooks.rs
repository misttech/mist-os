// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::thread_group_hooks::{self, SeLinuxResolvedElfState};
use crate::task::CurrentTask;
use starnix_uapi::errors::Errno;

/// Check if creating a task is allowed, if SELinux is enabled. Access is allowed if SELinux is disabled.
pub fn check_task_create_access(current_task: &CurrentTask) -> Result<(), Errno> {
    match &current_task.kernel().security_server {
        None => return Ok(()),
        Some(security_server) => {
            let group_state = current_task.thread_group.read();
            thread_group_hooks::check_task_create_access(
                security_server.as_ref(),
                &security_server.as_permission_check(),
                &group_state.selinux_state,
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
