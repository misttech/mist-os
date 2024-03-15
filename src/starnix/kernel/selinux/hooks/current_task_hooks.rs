// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::sync::Arc;

use super::thread_group_hooks::{self, SeLinuxResolvedElfState};
use crate::{
    task::{CurrentTask, Task, ThreadGroup},
    vfs::{FsNode, FsStr},
};

use selinux::security_server::{SecurityServer, SecurityServerStatus};
use starnix_uapi::{errors::Errno, signals::Signal};

// Call the `f` closure if SELinux is enabled and enforcing.
fn maybe_call_closure<F, R>(current_task: &CurrentTask, f: F) -> Result<R, Errno>
where
    F: FnOnce(&Arc<SecurityServer>) -> Result<R, Errno>,
    R: Default,
{
    match &current_task.kernel().security_server {
        Some(security_server) if !security_server.is_fake() && security_server.is_enforcing() => {
            f(security_server)
        }
        _ => Ok(R::default()),
    }
}

/// Check if creating a task is allowed. Access is allowed if SELinux is disabled, in fake mode, or
/// not enforcing.
pub fn check_task_create_access(current_task: &CurrentTask) -> Result<(), Errno> {
    maybe_call_closure(current_task, |security_server| {
        let sid = current_task.get_current_sid();
        thread_group_hooks::check_task_create_access(&security_server.as_permission_check(), sid)
    })
}

/// Checks if exec is allowed. Access is allowed if SELinux is disabled, in fake mode, or not
/// enforcing.
pub fn check_exec_access(
    current_task: &CurrentTask,
) -> Result<Option<SeLinuxResolvedElfState>, Errno> {
    maybe_call_closure(current_task, |security_server| {
        let group_state = current_task.thread_group.read();
        thread_group_hooks::check_exec_access(
            &security_server.as_permission_check(),
            &group_state.selinux_state,
        )
    })
}

/// Updates the SELinux thread group state on exec. No-op if SELinux is disabled, in fake mode, or
/// not enforcing.
pub fn update_state_on_exec(
    current_task: &mut CurrentTask,
    elf_selinux_state: &Option<SeLinuxResolvedElfState>,
) {
    maybe_call_closure(current_task, |_| {
        let mut thread_group_state = current_task.thread_group.write();
        thread_group_hooks::update_state_on_exec(
            &mut thread_group_state.selinux_state,
            elf_selinux_state,
        );
        Ok(())
    })
    .unwrap();
}

/// Checks if `source` may exercise the "getsched" permission on `target`. Access is allowed if
/// SELinux is disabled, in fake mode, or not enforcing.
pub fn check_getsched_access(source: &CurrentTask, target: &Task) -> Result<(), Errno> {
    maybe_call_closure(source, |security_server| {
        // TODO(b/323856891): Consider holding `source.thread_group` and `target.thread_group`
        // read locks for duration of access check.
        let source_sid = source.get_current_sid();
        let target_sid = target.get_current_sid();
        thread_group_hooks::check_getsched_access(
            &security_server.as_permission_check(),
            source_sid,
            target_sid,
        )
    })
}

/// Checks if setsched is allowed. Access is allowed if SELinux is disabled, in fake mode, or
/// not enforcing.
pub fn check_setsched_access(source: &CurrentTask, target: &Task) -> Result<(), Errno> {
    maybe_call_closure(source, |security_server| {
        let source_sid = source.get_current_sid();
        let target_sid = target.get_current_sid();
        thread_group_hooks::check_setsched_access(
            &security_server.as_permission_check(),
            source_sid,
            target_sid,
        )
    })
}

/// Checks if getpgid is allowed, if SELinux is enabled. Access is allowed if SELinux is disabled,
/// in fake mode, or not enforcing.
pub fn check_getpgid_access(source_task: &CurrentTask, target_task: &Task) -> Result<(), Errno> {
    maybe_call_closure(source_task, |security_server| {
        let source_sid = source_task.get_current_sid();
        let target_sid = target_task.get_current_sid();
        thread_group_hooks::check_getpgid_access(
            &security_server.as_permission_check(),
            source_sid,
            target_sid,
        )
    })
}

/// Checks if setpgid is allowed, if SELinux is enabled. Access is allowed if SELinux is disabled,
/// in fake mode, or not enforcing.
pub fn check_setpgid_access(source_task: &CurrentTask, target_task: &Task) -> Result<(), Errno> {
    maybe_call_closure(source_task, |security_server| {
        let source_sid = source_task.get_current_sid();
        let target_sid = target_task.get_current_sid();
        thread_group_hooks::check_setpgid_access(
            &security_server.as_permission_check(),
            source_sid,
            target_sid,
        )
    })
}

/// Checks if sending a signal is allowed, if SELinux is enabled. Access is allowed if SELinux is
/// disabled, in fake mode, or not enforcing.
pub fn check_signal_access(
    source_task: &CurrentTask,
    target_task: &Task,
    signal: Signal,
) -> Result<(), Errno> {
    maybe_call_closure(source_task, |security_server| {
        let source_sid = source_task.get_current_sid();
        let target_sid = target_task.get_current_sid();
        thread_group_hooks::check_signal_access(
            &security_server.as_permission_check(),
            source_sid,
            target_sid,
            signal,
        )
    })
}

/// Checks if tracing the current task is allowed, if SELinux is enabled. Access is allowed if
/// SELinux is disabled, in fake mode, or not enforcing.
pub fn check_ptrace_traceme_access(
    parent: &Arc<ThreadGroup>,
    current_task: &CurrentTask,
) -> Result<(), Errno> {
    maybe_call_closure(current_task, |security_server| {
        let source_sid = parent.get_current_sid();
        let target_sid = current_task.get_current_sid();
        thread_group_hooks::check_ptrace_access(
            &security_server.as_permission_check(),
            source_sid,
            target_sid,
        )
    })
}

/// Checks if `current_task` is allowed to trace `tracee_task`, if SELinux is enabled. Access is
/// allowed if SELinux is disabled, in fake mode, or not enforcing.
pub fn check_ptrace_attach_access(
    current_task: &CurrentTask,
    tracee_task: &Task,
) -> Result<(), Errno> {
    maybe_call_closure(current_task, |security_server| {
        let source_sid = current_task.get_current_sid();
        let target_sid = tracee_task.get_current_sid();
        thread_group_hooks::check_ptrace_access(
            &security_server.as_permission_check(),
            source_sid,
            target_sid,
        )
    })
}

/// Attempts to update the security ID (SID) associated with `fs_node` when
/// `name="security.selinux"` and `value` is a valid security context according to the current
/// policy.
pub fn post_setxattr(current_task: &CurrentTask, fs_node: &FsNode, name: &FsStr, value: &FsStr) {
    let security_selinux_name: &FsStr = "security.selinux".into();
    if name != security_selinux_name {
        return;
    }

    // This hook is not fallible; no need to handle `Result` from `maybe_call_closure`.
    let _ = maybe_call_closure(current_task, |security_server| {
        match security_server.security_context_to_sid(value) {
            // Update node SID value if a SID is found to be associated with new security context
            // string.
            Ok(sid) => {
                fs_node.update_info(|info| info.sid = Some(sid));
            }
            // Clear any existing node SID if none is associated with new security context string.
            Err(_) => {
                fs_node.update_info(|info| info.sid = None);
            }
        }

        Ok(())
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        testing::{
            create_kernel_and_task, create_kernel_and_task_with_selinux,
            create_kernel_task_and_unlocked, create_kernel_task_and_unlocked_with_selinux,
            create_task, AutoReleasableTask,
        },
        vfs::NamespaceNode,
    };
    use selinux::security_server::Mode;
    use starnix_sync::{Locked, Unlocked};
    use starnix_uapi::{device_type::DeviceType, file_mode::FileMode, signals::SIGTERM};
    use tests::thread_group_hooks::SeLinuxThreadGroupState;

    const VALID_SECURITY_CONTEXT: &'static str = "system_u:object_r:unconfined_t:s0";
    const DIFFERENT_VALID_SECURITY_CONTEXT: &'static str = "system_u:object_r:unconfined_t:s1";

    fn create_task_pair_with_selinux_disabled() -> (AutoReleasableTask, AutoReleasableTask) {
        let (kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let another_task = create_task(&mut locked, &kernel, "another-task");
        assert!(kernel.security_server.is_none());
        (current_task, another_task)
    }

    fn create_task_pair_with_fake_selinux() -> (AutoReleasableTask, AutoReleasableTask) {
        let security_server = SecurityServer::new(Mode::Fake);
        let (kernel, current_task, mut locked) =
            create_kernel_task_and_unlocked_with_selinux(security_server);
        let another_task = create_task(&mut locked, &kernel, "another-task");
        (current_task, another_task)
    }

    fn create_task_pair_with_permissive_selinux() -> (AutoReleasableTask, AutoReleasableTask) {
        let security_server = SecurityServer::new(Mode::Enable);
        security_server.set_enforcing(false);
        let (kernel, current_task, mut locked) =
            create_kernel_task_and_unlocked_with_selinux(security_server);
        let another_task = create_task(&mut locked, &kernel, "another-task");
        (current_task, another_task)
    }

    fn create_test_file(
        locked: &mut Locked<'_, Unlocked>,
        current_task: &AutoReleasableTask,
    ) -> NamespaceNode {
        current_task
            .fs()
            .root()
            .create_node(locked, &current_task, "file".into(), FileMode::IFREG, DeviceType::NONE)
            .expect("create_node(file)")
    }

    #[fuchsia::test]
    async fn task_create_access_allowed_for_selinux_disabled() {
        let (kernel, task) = create_kernel_and_task();
        assert!(kernel.security_server.is_none());
        assert_eq!(check_task_create_access(&task), Ok(()));
    }

    #[fuchsia::test]
    async fn task_create_access_allowed_for_fake_mode() {
        let security_server = SecurityServer::new(Mode::Fake);
        let (_kernel, task) = create_kernel_and_task_with_selinux(security_server);
        assert_eq!(check_task_create_access(&task), Ok(()));
    }

    #[fuchsia::test]
    async fn task_create_access_allowed_for_permissive_mode() {
        let security_server = SecurityServer::new(Mode::Enable);
        security_server.set_enforcing(false);
        let (_kernel, task) = create_kernel_and_task_with_selinux(security_server);
        assert_eq!(check_task_create_access(&task), Ok(()));
    }

    #[fuchsia::test]
    async fn exec_access_allowed_for_selinux_disabled() {
        let (kernel, task) = create_kernel_and_task();
        assert!(kernel.security_server.is_none());
        assert_eq!(check_exec_access(&task), Ok(None));
    }

    #[fuchsia::test]
    async fn exec_access_allowed_for_fake_mode() {
        let security_server = SecurityServer::new(Mode::Fake);
        let (_kernel, task) = create_kernel_and_task_with_selinux(security_server);
        assert_eq!(check_exec_access(&task), Ok(None));
    }

    #[fuchsia::test]
    async fn exec_access_allowed_for_permissive_mode() {
        let security_server = SecurityServer::new(Mode::Enable);
        security_server.set_enforcing(false);
        let (_kernel, task) = create_kernel_and_task_with_selinux(security_server);
        assert_eq!(check_exec_access(&task), Ok(None));
    }

    #[fuchsia::test]
    async fn no_state_update_for_selinux_disabled() {
        let (_kernel, task) = create_kernel_and_task();
        let mut task = task;

        let security_server = SecurityServer::new(Mode::Fake);
        let elf_sid = security_server
            .security_context_to_sid(b"u:object_r:type_t:s0")
            .expect("invalid security context");
        let elf_state = SeLinuxResolvedElfState { sid: elf_sid.clone() };
        assert_eq!(task.thread_group.read().selinux_state.as_ref(), None);
        update_state_on_exec(&mut task, &Some(elf_state));
        assert_eq!(task.thread_group.read().selinux_state.as_ref(), None);
    }

    #[fuchsia::test]
    async fn no_state_update_for_fake_mode() {
        let security_server = SecurityServer::new(Mode::Fake);
        let initial_state = SeLinuxThreadGroupState::for_kernel();
        let (kernel, task) = create_kernel_and_task_with_selinux(security_server);
        let mut task = task;
        task.thread_group.write().selinux_state = Some(initial_state.clone());

        let elf_sid = kernel
            .security_server
            .as_ref()
            .expect("missing security server")
            .security_context_to_sid(b"u:object_r:type_t:s0")
            .expect("invalid security context");
        let elf_state = SeLinuxResolvedElfState { sid: elf_sid.clone() };
        assert_ne!(elf_sid, initial_state.current_sid);
        update_state_on_exec(&mut task, &Some(elf_state));
        assert_eq!(
            task.thread_group.read().selinux_state.as_ref().expect("missing SELinux state"),
            &initial_state
        );
    }

    #[fuchsia::test]
    async fn no_state_update_for_permissive_mode() {
        let security_server = SecurityServer::new(Mode::Enable);
        security_server.set_enforcing(false);
        let initial_state = SeLinuxThreadGroupState::for_kernel();
        let (kernel, task) = create_kernel_and_task_with_selinux(security_server);
        let mut task = task;
        task.thread_group.write().selinux_state = Some(initial_state.clone());

        let elf_sid = kernel
            .security_server
            .as_ref()
            .expect("missing security server")
            .security_context_to_sid(b"u:object_r:type_t:s0")
            .expect("invalid security context");
        let elf_state = SeLinuxResolvedElfState { sid: elf_sid.clone() };
        assert_ne!(elf_sid, initial_state.current_sid);
        update_state_on_exec(&mut task, &Some(elf_state));
        assert_eq!(
            task.thread_group.read().selinux_state.as_ref().expect("missing SELinux state"),
            &initial_state
        );
    }

    #[fuchsia::test]
    async fn getsched_access_allowed_for_selinux_disabled() {
        let (source_task, target_task) = create_task_pair_with_selinux_disabled();
        assert_eq!(check_getsched_access(&source_task, &target_task), Ok(()));
    }

    #[fuchsia::test]
    async fn getsched_access_allowed_for_fake_mode() {
        let (source_task, target_task) = create_task_pair_with_fake_selinux();
        assert_eq!(check_getsched_access(&source_task, &target_task), Ok(()));
    }

    #[fuchsia::test]
    async fn getsched_access_allowed_for_permissive_mode() {
        let (source_task, target_task) = create_task_pair_with_permissive_selinux();
        assert_eq!(check_getsched_access(&source_task, &target_task), Ok(()));
    }

    #[fuchsia::test]
    async fn setsched_access_allowed_for_selinux_disabled() {
        let (source_task, target_task) = create_task_pair_with_selinux_disabled();
        assert_eq!(check_setsched_access(&source_task, &target_task), Ok(()));
    }

    #[fuchsia::test]
    async fn setsched_access_allowed_for_fake_mode() {
        let (source_task, target_task) = create_task_pair_with_fake_selinux();
        assert_eq!(check_setsched_access(&source_task, &target_task), Ok(()));
    }

    #[fuchsia::test]
    async fn setsched_access_allowed_for_permissive_mode() {
        let (source_task, target_task) = create_task_pair_with_permissive_selinux();
        assert_eq!(check_setsched_access(&source_task, &target_task), Ok(()));
    }

    #[fuchsia::test]
    async fn getpgid_access_allowed_for_selinux_disabled() {
        let (source_task, target_task) = create_task_pair_with_selinux_disabled();
        assert_eq!(check_getpgid_access(&source_task, &target_task), Ok(()));
    }

    #[fuchsia::test]
    async fn getpgid_access_allowed_for_fake_mode() {
        let (source_task, target_task) = create_task_pair_with_fake_selinux();
        assert_eq!(check_getpgid_access(&source_task, &target_task), Ok(()));
    }

    #[fuchsia::test]
    async fn getpgid_access_allowed_for_permissive_mode() {
        let (source_task, target_task) = create_task_pair_with_permissive_selinux();
        assert_eq!(check_getpgid_access(&source_task, &target_task), Ok(()));
    }

    #[fuchsia::test]
    async fn setpgid_access_allowed_for_selinux_disabled() {
        let (source_task, target_task) = create_task_pair_with_selinux_disabled();
        assert_eq!(check_setpgid_access(&source_task, &target_task), Ok(()));
    }

    #[fuchsia::test]
    async fn setpgid_access_allowed_for_fake_mode() {
        let (source_task, target_task) = create_task_pair_with_fake_selinux();
        assert_eq!(check_setpgid_access(&source_task, &target_task), Ok(()));
    }

    #[fuchsia::test]
    async fn setpgid_access_allowed_for_permissive_mode() {
        let (source_task, target_task) = create_task_pair_with_permissive_selinux();
        assert_eq!(check_setpgid_access(&source_task, &target_task), Ok(()));
    }

    #[fuchsia::test]
    async fn signal_access_allowed_for_selinux_disabled() {
        let (source_task, target_task) = create_task_pair_with_selinux_disabled();
        assert_eq!(check_signal_access(&source_task, &target_task, SIGTERM), Ok(()));
    }

    #[fuchsia::test]
    async fn signal_access_allowed_for_fake_mode() {
        let (source_task, target_task) = create_task_pair_with_fake_selinux();
        assert_eq!(check_signal_access(&source_task, &target_task, SIGTERM), Ok(()));
    }

    #[fuchsia::test]
    async fn signal_access_allowed_for_permissive_mode() {
        let (source_task, target_task) = create_task_pair_with_permissive_selinux();
        assert_eq!(check_signal_access(&source_task, &target_task, SIGTERM), Ok(()));
    }

    #[fuchsia::test]
    async fn ptrace_traceme_access_allowed_for_selinux_disabled() {
        let (tracee_task, tracer_task) = create_task_pair_with_selinux_disabled();
        assert_eq!(check_ptrace_traceme_access(&tracer_task.thread_group, &tracee_task), Ok(()));
    }

    #[fuchsia::test]
    async fn ptrace_traceme_access_allowed_for_fake_mode() {
        let (tracee_task, tracer_task) = create_task_pair_with_fake_selinux();
        assert_eq!(check_ptrace_traceme_access(&tracer_task.thread_group, &tracee_task), Ok(()));
    }

    #[fuchsia::test]
    async fn ptrace_traceme_access_allowed_for_permissive_mode() {
        let (tracee_task, tracer_task) = create_task_pair_with_permissive_selinux();
        assert_eq!(check_ptrace_traceme_access(&tracer_task.thread_group, &tracee_task), Ok(()));
    }

    #[fuchsia::test]
    async fn ptrace_attach_access_allowed_for_selinux_disabled() {
        let (tracer_task, tracee_task) = create_task_pair_with_selinux_disabled();
        assert_eq!(check_ptrace_attach_access(&tracer_task, &tracee_task), Ok(()));
    }

    #[fuchsia::test]
    async fn ptrace_attach_access_allowed_for_fake_mode() {
        let (tracer_task, tracee_task) = create_task_pair_with_fake_selinux();
        assert_eq!(check_ptrace_attach_access(&tracer_task, &tracee_task), Ok(()));
    }

    #[fuchsia::test]
    async fn ptrace_attach_access_allowed_for_permissive_mode() {
        let (tracer_task, tracee_task) = create_task_pair_with_permissive_selinux();
        assert_eq!(check_ptrace_attach_access(&tracer_task, &tracee_task), Ok(()));
    }

    #[fuchsia::test]
    async fn post_setxattr_noop_selinux_disabled() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let node = &create_test_file(&mut locked, &current_task).entry.node;
        assert_eq!(None, node.info().sid);

        post_setxattr(
            current_task.as_ref(),
            node.as_ref(),
            "security.selinux".into(),
            VALID_SECURITY_CONTEXT.into(),
        );

        assert_eq!(None, node.info().sid);
    }

    #[fuchsia::test]
    async fn post_setxattr_noop_selinux_fake() {
        let security_server = SecurityServer::new(Mode::Fake);
        let (_kernel, current_task, mut locked) =
            create_kernel_task_and_unlocked_with_selinux(security_server);
        let node = &create_test_file(&mut locked, &current_task).entry.node;
        assert_eq!(None, node.info().sid);

        post_setxattr(
            current_task.as_ref(),
            node.as_ref(),
            "security.selinux".into(),
            VALID_SECURITY_CONTEXT.into(),
        );

        assert_eq!(None, node.info().sid);
    }

    #[fuchsia::test]
    async fn post_setxattr_noop_selinux_permissive() {
        let security_server = SecurityServer::new(Mode::Enable);
        security_server.set_enforcing(false);
        let (_kernel, current_task, mut locked) =
            create_kernel_task_and_unlocked_with_selinux(security_server);
        let node = &create_test_file(&mut locked, &current_task).entry.node;
        assert_eq!(None, node.info().sid);

        post_setxattr(
            current_task.as_ref(),
            node.as_ref(),
            "security.selinux".into(),
            VALID_SECURITY_CONTEXT.into(),
        );

        assert_eq!(None, node.info().sid);
    }

    #[fuchsia::test]
    async fn post_setxattr_noop_different_name() {
        let security_server = SecurityServer::new(Mode::Enable);
        security_server.set_enforcing(true);
        let (_kernel, current_task, mut locked) =
            create_kernel_task_and_unlocked_with_selinux(security_server);
        let node = &create_test_file(&mut locked, &current_task).entry.node;
        assert_eq!(None, node.info().sid);

        post_setxattr(
            current_task.as_ref(),
            node.as_ref(),
            "security.selinu!".into(), // Note: name != "security.selinux".
            VALID_SECURITY_CONTEXT.into(),
        );

        assert_eq!(None, node.info().sid);
    }

    #[fuchsia::test]
    async fn post_setxattr_clear_invalid_security_context() {
        let security_server = SecurityServer::new(Mode::Enable);
        security_server.set_enforcing(true);
        let (_kernel, current_task, mut locked) =
            create_kernel_task_and_unlocked_with_selinux(security_server);
        let node = &create_test_file(&mut locked, &current_task).entry.node;
        post_setxattr(
            current_task.as_ref(),
            node.as_ref(),
            "security.selinux".into(),
            VALID_SECURITY_CONTEXT.into(),
        );
        assert_ne!(None, node.info().sid);

        post_setxattr(
            current_task.as_ref(),
            node.as_ref(),
            "security.selinux".into(),
            "!".into(), // Note: Not a valid security context.
        );

        assert_eq!(None, node.info().sid);
    }

    #[fuchsia::test]
    async fn post_setxattr_set_sid_selinux_enforcing() {
        let security_server = SecurityServer::new(Mode::Enable);
        security_server.set_enforcing(true);
        let (_kernel, current_task, mut locked) =
            create_kernel_task_and_unlocked_with_selinux(security_server);
        let node = &create_test_file(&mut locked, &current_task).entry.node;
        assert_eq!(None, node.info().sid);

        post_setxattr(
            current_task.as_ref(),
            node.as_ref(),
            "security.selinux".into(),
            VALID_SECURITY_CONTEXT.into(),
        );

        assert!(node.info().sid.is_some());
    }

    #[fuchsia::test]
    async fn post_setxattr_different_sid_for_different_context() {
        let security_server = SecurityServer::new(Mode::Enable);
        security_server.set_enforcing(true);
        let (_kernel, current_task, mut locked) =
            create_kernel_task_and_unlocked_with_selinux(security_server);
        let node = &create_test_file(&mut locked, &current_task).entry.node;
        assert_eq!(None, node.info().sid);

        post_setxattr(
            current_task.as_ref(),
            node.as_ref(),
            "security.selinux".into(),
            VALID_SECURITY_CONTEXT.into(),
        );

        assert!(node.info().sid.is_some());

        let first_sid = node.info().sid.clone().unwrap();
        post_setxattr(
            current_task.as_ref(),
            node.as_ref(),
            "security.selinux".into(),
            DIFFERENT_VALID_SECURITY_CONTEXT.into(),
        );

        assert!(node.info().sid.is_some());

        let second_sid = node.info().sid.clone().unwrap();

        assert_ne!(first_sid, second_sid);
    }
}
