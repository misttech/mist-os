// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::{selinux_hooks, ResolvedElfState, TaskState};
use crate::task::{CurrentTask, Task};
use crate::vfs::{FsNode, FsNodeHandle, FsStr, ValueOrSize};

use selinux::security_server::SecurityServer;
use selinux::{InitialSid, SecurityId};
use selinux_common::ProcessPermission;
use starnix_uapi::error;
use starnix_uapi::errors::Errno;
use starnix_uapi::signals::Signal;
use std::sync::Arc;

/// Maximum supported size for the `"security.selinux"` value used to store SELinux security
/// contexts in a filesystem node extended attributes.
pub const SECURITY_SELINUX_XATTR_VALUE_MAX_SIZE: usize = 4096;

/// Executes the `hook` closure, dependent on the state of SELinux.
///
/// If SELinux is not enabled, or is enabled but has no policy loaded, then the `not_enabled`
/// closure is executed, to determine the result. Otherwise, the `hook()` is executed on
/// behalf of the caller.
///
/// TODO(b/331375792): Move permission & fake mode handling inside the SELinux logic.
/// If SELinux is enabled with a policy, and in permissive or fake mode, then the `hook()`
/// result is ignored, and the return type's default value always returned.
fn check_if_selinux_else<H, R, D>(task: &Task, hook: H, not_enabled: D) -> Result<R, Errno>
where
    H: FnOnce(&Arc<SecurityServer>) -> Result<R, Errno>,
    D: FnOnce() -> Result<R, Errno>,
    R: Default,
{
    if let Some(security_server) = &task.kernel().security_server {
        if !security_server.has_policy() {
            return not_enabled();
        }
        let result = hook(security_server);
        // TODO(b/331375792): Relocate "enforcing" check into the AVC.
        if result.is_err() && (!security_server.is_enforcing() || security_server.is_fake()) {
            return Ok(R::default());
        }
        result
    } else {
        not_enabled()
    }
}

fn check_if_selinux<H, R>(task: &Task, hook: H) -> Result<R, Errno>
where
    H: FnOnce(&Arc<SecurityServer>) -> Result<R, Errno>,
    R: Default,
{
    let success = || Ok(R::default());
    check_if_selinux_else(task, hook, success)
}

/// Executes the infallible `hook` closure, dependent on the state of SELinux.
///
/// This is used for non-enforcing hooks, such as those responsible for generating security labels
/// for new tasks.
fn run_if_selinux<F, R>(task: &Task, hook: F) -> R
where
    F: FnOnce(&Arc<SecurityServer>) -> R,
    R: Default,
{
    run_if_selinux_else(task, hook, R::default)
}

fn run_if_selinux_else<F, R, D>(task: &Task, hook: F, default: D) -> R
where
    F: FnOnce(&Arc<SecurityServer>) -> R,
    D: Fn() -> R,
{
    task.kernel().security_server.as_ref().map_or_else(&default, |ss| {
        if ss.has_policy() {
            hook(ss)
        } else {
            default()
        }
    })
}

/// Return `TaskState` for specific kinds of kernel task.
// Ideally we'd have a single `task_alloc()` that updates a target `Task` based on optional `CurrentTask`, PID, etc.
pub fn task_alloc_for_kernel() -> TaskState {
    TaskState(selinux_hooks::TaskState::for_kernel())
}
pub fn task_alloc_for_init() -> TaskState {
    // TODO(b/350975345): Return value based on the "init" initial SID.
    TaskState(selinux_hooks::TaskState::for_kernel())
}

/// Returns `TaskState` for a new `Task`, based on that of the current `Task`, and the specified clone flags.
pub fn task_alloc(current_task: &CurrentTask, clone_flags: u64) -> TaskState {
    TaskState(run_if_selinux_else(
        current_task,
        |_| selinux_hooks::task_alloc(&current_task.read().security_state.0, clone_flags),
        || selinux_hooks::TaskState::for_selinux_disabled(),
    ))
}

fn get_current_sid(task: &Task) -> SecurityId {
    task.read().security_state.0.current_sid
}

/// Returns the serialized Security Context associated with the specified task.
/// If the task's current SID cannot be resolved then an empty string is returned.
/// This combines the `task_getsecid()` and `secid_to_secctx()` hooks, in effect.
pub fn get_task_context(current_task: &CurrentTask, target: &Task) -> Result<Vec<u8>, Errno> {
    run_if_selinux_else(
        current_task,
        |security_server| {
            let sid = get_current_sid(&target);
            Ok(security_server.sid_to_security_context(sid).unwrap_or_default())
        },
        || error!(ENOTSUP),
    )
}

/// Check if creating a task is allowed.
pub fn check_task_create_access(current_task: &CurrentTask) -> Result<(), Errno> {
    check_if_selinux(current_task, |security_server| {
        let sid = get_current_sid(&current_task);
        selinux_hooks::check_task_create_access(&security_server.as_permission_check(), sid)
    })
}

/// Checks if exec is allowed.
pub fn check_exec_access(
    current_task: &CurrentTask,
    executable_node: &FsNodeHandle,
) -> Result<Option<ResolvedElfState>, Errno> {
    check_if_selinux(current_task, |security_server| {
        let executable_sid = executable_node.effective_sid(current_task);
        let group_state = current_task.read();
        selinux_hooks::check_exec_access(
            &security_server,
            &group_state.security_state.0,
            executable_sid,
        )
        .map(|s| s.map(|s| ResolvedElfState(s)))
    })
}

/// Updates the SELinux thread group state on exec.
pub fn update_state_on_exec(
    current_task: &mut CurrentTask,
    elf_security_state: &Option<ResolvedElfState>,
) {
    run_if_selinux(current_task, |_| {
        let mut task_state = current_task.write();
        selinux_hooks::update_state_on_exec(
            &mut task_state.security_state.0,
            elf_security_state.as_ref().map(|s| s.0),
        );
    });
}

/// Checks if `source` may exercise the "getsched" permission on `target`.
pub fn check_getsched_access(source: &CurrentTask, target: &Task) -> Result<(), Errno> {
    check_if_selinux(source, |security_server| {
        // TODO(b/323856891): Consider holding `source` and `target`
        // read locks for duration of access check.
        let source_sid = get_current_sid(&source);
        let target_sid = get_current_sid(&target);
        selinux_hooks::check_getsched_access(
            &security_server.as_permission_check(),
            source_sid,
            target_sid,
        )
    })
}

/// Checks if setsched is allowed.
pub fn check_setsched_access(source: &CurrentTask, target: &Task) -> Result<(), Errno> {
    check_if_selinux(source, |security_server| {
        let source_sid = get_current_sid(&source);
        let target_sid = get_current_sid(&target);
        selinux_hooks::check_setsched_access(
            &security_server.as_permission_check(),
            source_sid,
            target_sid,
        )
    })
}

/// Checks if getpgid is allowed.
pub fn check_getpgid_access(source: &CurrentTask, target: &Task) -> Result<(), Errno> {
    check_if_selinux(source, |security_server| {
        let source_sid = get_current_sid(&source);
        let target_sid = get_current_sid(&target);
        selinux_hooks::check_getpgid_access(
            &security_server.as_permission_check(),
            source_sid,
            target_sid,
        )
    })
}

/// Checks if setpgid is allowed.
pub fn check_setpgid_access(source: &CurrentTask, target: &Task) -> Result<(), Errno> {
    check_if_selinux(source, |security_server| {
        let source_sid = get_current_sid(&source);
        let target_sid = get_current_sid(&target);
        selinux_hooks::check_setpgid_access(
            &security_server.as_permission_check(),
            source_sid,
            target_sid,
        )
    })
}

/// Called when the current task queries the session Id of the `target` task.
/// Corresponds to the `task_getsid` LSM hook.
pub fn check_task_getsid(source: &CurrentTask, target: &Task) -> Result<(), Errno> {
    check_if_selinux(source, |security_server| {
        let source_sid = get_current_sid(&source);
        let target_sid = get_current_sid(&target);
        selinux_hooks::check_permissions(
            &security_server.as_permission_check(),
            source_sid,
            target_sid,
            &[ProcessPermission::GetSession],
        )
    })
}

/// Checks if sending a signal is allowed.
pub fn check_signal_access(
    source: &CurrentTask,
    target: &Task,
    signal: Signal,
) -> Result<(), Errno> {
    check_if_selinux(source, |security_server| {
        let source_sid = get_current_sid(&source);
        let target_sid = get_current_sid(&target);
        selinux_hooks::check_signal_access(
            &security_server.as_permission_check(),
            source_sid,
            target_sid,
            signal,
        )
    })
}

/// Checks if sending a signal is allowed.
pub fn check_signal_access_tg(
    source: &CurrentTask,
    target: &Task,
    signal: Signal,
) -> Result<(), Errno> {
    check_if_selinux(source, |security_server| {
        let source_sid = get_current_sid(&source);
        let target_sid = get_current_sid(target);
        selinux_hooks::check_signal_access(
            &security_server.as_permission_check(),
            source_sid,
            target_sid,
            signal,
        )
    })
}

// Checks whether the `parent_tracer_task` is allowed to trace the current `tracee_task`.
pub fn ptrace_traceme(tracee_task: &CurrentTask, parent_tracer_task: &Task) -> Result<(), Errno> {
    check_if_selinux(tracee_task, |security_server| {
        selinux_hooks::ptrace_access_check(
            &security_server.as_permission_check(),
            get_current_sid(&parent_tracer_task),
            &mut tracee_task.write().security_state.0,
        )
    })
}

/// Checks whether the current `tracer_task` is allowed to trace `tracee_task`.
/// This fills the role of both of the LSM `ptrace_traceme` and `ptrace_access_check` hooks.
pub fn ptrace_access_check(tracer_task: &CurrentTask, tracee_task: &Task) -> Result<(), Errno> {
    check_if_selinux(tracer_task, |security_server| {
        let tracer_sid = get_current_sid(&tracer_task);
        let mut task_state = tracee_task.write();
        selinux_hooks::ptrace_access_check(
            &security_server.as_permission_check(),
            tracer_sid,
            &mut task_state.security_state.0,
        )
    })
}

/// Clears the `ptrace_sid` for `tracee_task`.
// TODO: Fix ptrace checks to rely on `Task::ptrace` state, and remove this.
pub fn clear_ptracer_sid(tracee_task: &Task) {
    run_if_selinux(tracee_task, |_| {
        let mut task_state = tracee_task.write();
        task_state.security_state.0.ptracer_sid = None;
    });
}

/// Attempts to update the security ID (SID) associated with `fs_node` when
/// `name="security.selinux"` and `value` is a valid security context according to the current
/// policy.
pub fn post_setxattr(current_task: &CurrentTask, fs_node: &FsNode, name: &FsStr, value: &FsStr) {
    let security_selinux_name: &FsStr = "security.selinux".into();
    if name != security_selinux_name {
        return;
    }

    run_if_selinux(current_task, |security_server| {
        match security_server.security_context_to_sid(value) {
            // Update node SID value if a SID is found to be associated with new security context
            // string.
            Ok(sid) => fs_node.set_cached_sid(sid),
            // Clear any existing node SID if none is associated with new security context string.
            Err(_) => fs_node.clear_cached_sid(),
        }
    });
}

/// Identifies one of the Security Context attributes associated with a task.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ProcAttr {
    Current,
    Exec,
    FsCreate,
    KeyCreate,
    Previous,
    SockCreate,
}

/// Returns the Security Context associated with the `name`ed entry for the specified `target` task.
pub fn get_procattr(
    current_task: &CurrentTask,
    target: &Task,
    attr: ProcAttr,
) -> Result<Vec<u8>, Errno> {
    check_if_selinux_else(
        current_task,
        |security_server| {
            selinux_hooks::get_procattr(
                security_server,
                get_current_sid(&current_task),
                &target.read().security_state.0,
                attr,
            )
        },
        // If SELinux is disabled then there are no values to return.
        || {
            if attr == ProcAttr::Current {
                // Without SELinux the "current" attribute reports a placeholder value.
                Ok(b"unconfined".to_vec())
            } else {
                error!(EINVAL)
            }
        },
    )
}

/// Sets the Security Context associated with the `name`ed entry for the current task.
pub fn set_procattr(
    current_task: &CurrentTask,
    attr: ProcAttr,
    context: &[u8],
) -> Result<(), Errno> {
    check_if_selinux_else(
        current_task,
        |security_server| {
            let mut task_state = current_task.write();
            selinux_hooks::set_procattr(
                security_server,
                task_state.security_state.0.current_sid,
                &mut task_state.security_state.0,
                attr,
                context,
            )
        },
        // If SELinux is disabled then no writes are accepted.
        || error!(EINVAL),
    )
}

/// Returns a security id that should be used for SELinux access control checks on `fs_node`. This
/// computation will attempt to load the security id associated with an extended attribute value. If
/// a meaningful security id cannot be determined, then the `unlabeled` security id is returned.
///
/// This `unlabeled` case includes situations such as:
///
/// 1. There is no active security server;
/// 2. The active security server to serve as a basis for computing a securit id;
/// 3. The `get_xattr("security.selinux")` computation fails to return a `context_string`;
/// 4. The subsequent `context_string => security_id` computation fails.
pub fn get_fs_node_security_id(current_task: &CurrentTask, fs_node: &FsNode) -> SecurityId {
    run_if_selinux_else(
        current_task,
        |security_server| {
            let security_selinux_name: &FsStr = "security.selinux".into();
            // Use `fs_node.ops().get_xattr()` instead of `fs_node.get_xattr()` to bypass permission
            // checks performed on starnix userspace calls to get an extended attribute.
            match fs_node.ops().get_xattr(
                fs_node,
                current_task,
                security_selinux_name,
                SECURITY_SELINUX_XATTR_VALUE_MAX_SIZE,
            ) {
                Ok(ValueOrSize::Value(security_context)) => {
                    match security_server.security_context_to_sid(&security_context) {
                        Ok(sid) => {
                            // Update node SID value if a SID is found to be associated with new security context
                            // string.
                            fs_node.set_cached_sid(sid);

                            sid
                        }
                        // TODO(b/330875626): What is the correct behaviour when no sid can be
                        // constructed for the security context string (presumably because the context
                        // string is invalid for the current policy)?
                        _ => SecurityId::initial(InitialSid::Unlabeled),
                    }
                }
                // TODO(b/330875626): What is the correct behaviour when no extended attribute value is
                // found to label this file-like object?
                _ => SecurityId::initial(InitialSid::Unlabeled),
            }
        },
        // TODO(b/330875626): What is the correct behaviour when the closure does not execute (no
        // security server, etc.)?
        || SecurityId::initial(InitialSid::Unlabeled),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{
        create_kernel_and_task, create_kernel_and_task_with_selinux,
        create_kernel_task_and_unlocked, create_kernel_task_and_unlocked_with_selinux, create_task,
        AutoReleasableTask,
    };
    use crate::vfs::{NamespaceNode, XattrOp};
    use selinux::security_server::Mode;
    use starnix_sync::{Locked, Unlocked};
    use starnix_uapi::device_type::DeviceType;
    use starnix_uapi::error;
    use starnix_uapi::file_mode::FileMode;
    use starnix_uapi::signals::SIGTERM;

    const VALID_SECURITY_CONTEXT: &[u8] = b"u:object_r:test_valid_t:s0";
    const DIFFERENT_VALID_SECURITY_CONTEXT: &[u8] = b"u:object_r:test_different_valid_t:s0";

    const VALID_SECURITY_CONTEXT_WITH_NULL: &[u8] = b"u:object_r:test_valid_t:s0\0";
    const INVALID_SECURITY_CONTEXT_BECAUSE_OF_NULL: &[u8] = b"u:object_r:test_valid_\0t:s0";

    const INVALID_SECURITY_CONTEXT: &[u8] = b"not_a_u:object_r:test_valid_t:s0";

    const HOOKS_TESTS_BINARY_POLICY: &[u8] =
        include_bytes!("../../lib/selinux/testdata/micro_policies/hooks_tests_policy.pp");

    fn security_server_with_policy(mode: Mode) -> Arc<SecurityServer> {
        let policy_bytes = HOOKS_TESTS_BINARY_POLICY.to_vec();
        let security_server = SecurityServer::new(mode);
        security_server.set_enforcing(true);
        security_server.load_policy(policy_bytes).expect("policy load failed");
        security_server
    }

    fn create_task_pair_with_selinux_disabled() -> (AutoReleasableTask, AutoReleasableTask) {
        let (kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let another_task = create_task(&mut locked, &kernel, "another-task");
        assert!(kernel.security_server.is_none());
        (current_task, another_task)
    }

    fn create_task_pair_with_fake_selinux() -> (AutoReleasableTask, AutoReleasableTask) {
        let security_server = security_server_with_policy(Mode::Fake);
        let (kernel, current_task, mut locked) =
            create_kernel_task_and_unlocked_with_selinux(security_server);
        let another_task = create_task(&mut locked, &kernel, "another-task");
        (current_task, another_task)
    }

    fn create_task_pair_with_permissive_selinux() -> (AutoReleasableTask, AutoReleasableTask) {
        let security_server = security_server_with_policy(Mode::Enable);
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

    #[derive(Default, Debug, PartialEq)]
    enum TestHookResult {
        WasRun,
        WasNotRun,
        #[default]
        WasNotRunDefault,
    }

    #[fuchsia::test]
    async fn check_and_run_if_selinux_disabled() {
        let (kernel, task) = create_kernel_and_task();
        assert!(kernel.security_server.is_none());

        let check_result = check_if_selinux(&task, |_| Ok(TestHookResult::WasRun));
        assert_eq!(check_result, Ok(TestHookResult::WasNotRunDefault));

        let run_result = run_if_selinux(&task, |_| TestHookResult::WasRun);
        assert_eq!(run_result, TestHookResult::WasNotRunDefault);

        let run_else_result =
            run_if_selinux_else(&task, |_| TestHookResult::WasRun, || TestHookResult::WasNotRun);
        assert_eq!(run_else_result, TestHookResult::WasNotRun);
    }

    #[fuchsia::test]
    async fn check_and_run_if_selinux_without_policy() {
        let security_server = SecurityServer::new(Mode::Enable);
        let (_kernel, task) = create_kernel_and_task_with_selinux(security_server);

        let check_result = check_if_selinux(&task, |_| Ok(TestHookResult::WasRun));
        assert_eq!(check_result, Ok(TestHookResult::WasNotRunDefault));

        let run_result = run_if_selinux(&task, |_| TestHookResult::WasRun);
        assert_eq!(run_result, TestHookResult::WasNotRunDefault);

        let run_else_result =
            run_if_selinux_else(&task, |_| TestHookResult::WasRun, || TestHookResult::WasNotRun);
        assert_eq!(run_else_result, TestHookResult::WasNotRun);
    }

    #[fuchsia::test]
    async fn check_and_run_if_selinux_with_policy() {
        let security_server = security_server_with_policy(Mode::Enable);
        let (_kernel, task) = create_kernel_and_task_with_selinux(security_server);

        let check_result = check_if_selinux(&task, |_| Ok(TestHookResult::WasRun));
        assert_eq!(check_result, Ok(TestHookResult::WasRun));

        let run_result = run_if_selinux(&task, |_| TestHookResult::WasRun);
        assert_eq!(run_result, TestHookResult::WasRun);

        let run_else_result =
            run_if_selinux_else(&task, |_| TestHookResult::WasRun, || TestHookResult::WasNotRun);
        assert_eq!(run_else_result, TestHookResult::WasRun);
    }

    #[fuchsia::test]
    async fn task_alloc_selinux_disabled() {
        let (_kernel, current_task) = create_kernel_and_task();

        task_alloc(&current_task, 0);
    }

    fn failing_hook(_: &Arc<SecurityServer>) -> Result<(), Errno> {
        error!(EINVAL)
    }

    #[fuchsia::test]
    async fn check_if_selinux_fake_mode_enforcing() {
        let security_server = security_server_with_policy(Mode::Fake);
        security_server.set_enforcing(true);
        let (_kernel, task) = create_kernel_and_task_with_selinux(security_server);

        let check_result = check_if_selinux(&task, failing_hook);
        assert_eq!(check_result, Ok(()));
    }

    #[fuchsia::test]
    async fn check_if_selinux_permissive() {
        let security_server = security_server_with_policy(Mode::Enable);
        security_server.set_enforcing(false);
        let (_kernel, task) = create_kernel_and_task_with_selinux(security_server);

        let check_result = check_if_selinux(&task, failing_hook);
        assert_eq!(check_result, Ok(()));
    }

    #[fuchsia::test]
    async fn check_if_selinux_enforcing() {
        let security_server = security_server_with_policy(Mode::Enable);
        security_server.set_enforcing(true);
        let (_kernel, task) = create_kernel_and_task_with_selinux(security_server);

        let check_result = check_if_selinux(&task, failing_hook);
        assert_eq!(check_result, error!(EINVAL));
    }

    #[fuchsia::test]
    async fn task_create_access_allowed_for_selinux_disabled() {
        let (kernel, task) = create_kernel_and_task();
        assert!(kernel.security_server.is_none());
        assert_eq!(check_task_create_access(&task), Ok(()));
    }

    #[fuchsia::test]
    async fn task_create_access_allowed_for_fake_mode() {
        let security_server = security_server_with_policy(Mode::Fake);
        let (_kernel, task) = create_kernel_and_task_with_selinux(security_server);
        assert_eq!(check_task_create_access(&task), Ok(()));
    }

    #[fuchsia::test]
    async fn task_create_access_allowed_for_permissive_mode() {
        let security_server = security_server_with_policy(Mode::Enable);
        security_server.set_enforcing(false);
        let (_kernel, task) = create_kernel_and_task_with_selinux(security_server);
        assert_eq!(check_task_create_access(&task), Ok(()));
    }

    #[fuchsia::test]
    async fn exec_access_allowed_for_selinux_disabled() {
        let (kernel, task, mut locked) = create_kernel_task_and_unlocked();
        assert!(kernel.security_server.is_none());
        let executable_node = &create_test_file(&mut locked, &task).entry.node;
        assert_eq!(check_exec_access(&task, executable_node), Ok(None));
    }

    #[fuchsia::test]
    async fn exec_access_allowed_for_fake_mode() {
        let security_server = security_server_with_policy(Mode::Fake);
        let (_kernel, task, mut locked) =
            create_kernel_task_and_unlocked_with_selinux(security_server);
        let executable_node = &create_test_file(&mut locked, &task).entry.node;
        // Expect that access is granted, and a `ResolvedElfState` is returned.
        let result = check_exec_access(&task, executable_node);
        assert!(result.is_ok());
        assert!(result.unwrap().is_some());
    }

    #[fuchsia::test]
    async fn exec_access_allowed_for_permissive_mode() {
        let security_server = security_server_with_policy(Mode::Enable);
        security_server.set_enforcing(false);
        let (_kernel, task, mut locked) =
            create_kernel_task_and_unlocked_with_selinux(security_server);
        let executable_node = &create_test_file(&mut locked, &task).entry.node;
        // Expect that access is granted, and a `ResolvedElfState` is returned.
        let result = check_exec_access(&task, executable_node);
        assert!(result.is_ok());
        assert!(result.unwrap().is_some());
    }

    #[fuchsia::test]
    async fn no_state_update_for_selinux_disabled() {
        let (_kernel, task) = create_kernel_and_task();
        let mut task = task;

        // Without SELinux enabled and a policy loaded, only `InitialSid` values exist
        // in the system.
        let target_sid = SecurityId::initial(InitialSid::Unlabeled);
        let elf_state = ResolvedElfState(target_sid);

        assert!(task.read().security_state.0.current_sid != target_sid);

        let before_hook_sid = task.read().security_state.0.current_sid;
        update_state_on_exec(&mut task, &Some(elf_state));
        assert_eq!(task.read().security_state.0.current_sid, before_hook_sid);
    }

    #[fuchsia::test]
    async fn no_state_update_for_selinux_without_policy() {
        let security_server = SecurityServer::new(Mode::Enable);
        let (_kernel, task) = create_kernel_and_task_with_selinux(security_server);
        let mut task = task;

        // Without SELinux enabled and a policy loaded, only `InitialSid` values exist
        // in the system.
        let initial_state = selinux_hooks::TaskState::for_kernel();
        let elf_sid = SecurityId::initial(InitialSid::Unlabeled);
        let elf_state = ResolvedElfState(elf_sid);
        assert_ne!(elf_sid, initial_state.current_sid);
        update_state_on_exec(&mut task, &Some(elf_state));
        assert_eq!(task.read().security_state.0, initial_state);
    }

    #[fuchsia::test]
    async fn state_update_for_fake_mode() {
        let security_server = security_server_with_policy(Mode::Fake);
        let initial_state = selinux_hooks::TaskState::for_kernel();
        let (kernel, task) = create_kernel_and_task_with_selinux(security_server);
        let mut task = task;
        task.write().security_state.0 = initial_state.clone();

        let elf_sid = kernel
            .security_server
            .as_ref()
            .expect("missing security server")
            .security_context_to_sid(b"u:object_r:fork_no_t:s0")
            .expect("invalid security context");
        let elf_state = ResolvedElfState(elf_sid);
        assert_ne!(elf_sid, initial_state.current_sid);
        update_state_on_exec(&mut task, &Some(elf_state));
        assert_eq!(task.read().security_state.0.current_sid, elf_sid);
    }

    #[fuchsia::test]
    async fn state_update_for_permissive_mode() {
        let security_server = security_server_with_policy(Mode::Enable);
        security_server.set_enforcing(false);
        let initial_state = selinux_hooks::TaskState::for_kernel();
        let (kernel, task) = create_kernel_and_task_with_selinux(security_server);
        let mut task = task;
        task.write().security_state.0 = initial_state.clone();
        let elf_sid = kernel
            .security_server
            .as_ref()
            .expect("missing security server")
            .security_context_to_sid(b"u:object_r:fork_no_t:s0")
            .expect("invalid security context");
        let elf_state = ResolvedElfState(elf_sid);
        assert_ne!(elf_sid, initial_state.current_sid);
        update_state_on_exec(&mut task, &Some(elf_state));
        assert_eq!(task.read().security_state.0.current_sid, elf_sid);
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
    async fn task_getsid_allowed_for_selinux_disabled() {
        let (source_task, target_task) = create_task_pair_with_selinux_disabled();
        assert_eq!(check_task_getsid(&source_task, &target_task), Ok(()));
    }

    #[fuchsia::test]
    async fn task_getsid_allowed_for_fake_mode() {
        let (source_task, target_task) = create_task_pair_with_fake_selinux();
        assert_eq!(check_task_getsid(&source_task, &target_task), Ok(()));
    }

    #[fuchsia::test]
    async fn task_getsid_allowed_for_permissive_mode() {
        let (source_task, target_task) = create_task_pair_with_permissive_selinux();
        assert_eq!(check_task_getsid(&source_task, &target_task), Ok(()));
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
        assert_eq!(ptrace_traceme(&tracee_task, &tracer_task), Ok(()));
    }

    #[fuchsia::test]
    async fn ptrace_traceme_access_allowed_for_fake_mode() {
        let (tracee_task, tracer_task) = create_task_pair_with_fake_selinux();
        assert_eq!(ptrace_traceme(&tracee_task, &tracer_task), Ok(()));
    }

    #[fuchsia::test]
    async fn ptrace_traceme_access_allowed_for_permissive_mode() {
        let (tracee_task, tracer_task) = create_task_pair_with_permissive_selinux();
        assert_eq!(ptrace_traceme(&tracee_task, &tracer_task), Ok(()));
    }

    #[fuchsia::test]
    async fn ptrace_attach_access_allowed_for_selinux_disabled() {
        let (tracer_task, tracee_task) = create_task_pair_with_selinux_disabled();
        assert_eq!(ptrace_access_check(&tracer_task, &tracee_task), Ok(()));
    }

    #[fuchsia::test]
    async fn ptrace_attach_access_allowed_for_fake_mode() {
        let (tracer_task, tracee_task) = create_task_pair_with_fake_selinux();
        assert_eq!(ptrace_access_check(&tracer_task, &tracee_task), Ok(()));
    }

    #[fuchsia::test]
    async fn ptrace_attach_access_allowed_for_permissive_mode() {
        let (tracer_task, tracee_task) = create_task_pair_with_permissive_selinux();
        assert_eq!(ptrace_access_check(&tracer_task, &tracee_task), Ok(()));
    }

    #[fuchsia::test]
    async fn ptracer_sid_is_cleared() {
        let security_server = security_server_with_policy(Mode::Enable);
        security_server.set_enforcing(true);
        let (_kernel, tracee_task) = create_kernel_and_task_with_selinux(security_server);
        tracee_task.write().security_state.0.ptracer_sid =
            Some(SecurityId::initial(InitialSid::Unlabeled));

        clear_ptracer_sid(tracee_task.as_ref());
        assert!(tracee_task.read().security_state.0.ptracer_sid.is_none());
    }

    #[fuchsia::test]
    async fn post_setxattr_noop_selinux_disabled() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let node = &create_test_file(&mut locked, &current_task).entry.node;
        assert_eq!(None, node.cached_sid());

        post_setxattr(
            current_task.as_ref(),
            node.as_ref(),
            "security.selinux".into(),
            VALID_SECURITY_CONTEXT.into(),
        );

        assert_eq!(None, node.cached_sid());
    }

    #[fuchsia::test]
    async fn post_setxattr_noop_selinux_without_policy() {
        let security_server = SecurityServer::new(Mode::Enable);
        let (_kernel, current_task, mut locked) =
            create_kernel_task_and_unlocked_with_selinux(security_server);
        let node = &create_test_file(&mut locked, &current_task).entry.node;
        assert_eq!(None, node.cached_sid());

        post_setxattr(
            current_task.as_ref(),
            node.as_ref(),
            "security.selinux".into(),
            VALID_SECURITY_CONTEXT.into(),
        );

        assert_eq!(None, node.cached_sid());
    }

    #[fuchsia::test]
    async fn post_setxattr_selinux_fake() {
        let security_server = security_server_with_policy(Mode::Fake);
        let (_kernel, current_task, mut locked) =
            create_kernel_task_and_unlocked_with_selinux(security_server);
        let node = &create_test_file(&mut locked, &current_task).entry.node;
        assert_eq!(None, node.cached_sid());

        post_setxattr(
            current_task.as_ref(),
            node.as_ref(),
            "security.selinux".into(),
            VALID_SECURITY_CONTEXT.into(),
        );

        assert!(node.cached_sid().is_some());
    }

    #[fuchsia::test]
    async fn post_setxattr_selinux_permissive() {
        let security_server = security_server_with_policy(Mode::Enable);
        security_server.set_enforcing(false);
        let (_kernel, current_task, mut locked) =
            create_kernel_task_and_unlocked_with_selinux(security_server);
        let node = &create_test_file(&mut locked, &current_task).entry.node;
        assert_eq!(None, node.cached_sid());

        post_setxattr(
            current_task.as_ref(),
            node.as_ref(),
            "security.selinux".into(),
            VALID_SECURITY_CONTEXT.into(),
        );

        assert!(node.cached_sid().is_some());
    }

    #[fuchsia::test]
    async fn post_setxattr_not_selinux_is_noop() {
        let security_server = security_server_with_policy(Mode::Enable);
        security_server.set_enforcing(true);
        let (_kernel, current_task, mut locked) =
            create_kernel_task_and_unlocked_with_selinux(security_server);
        let node = &create_test_file(&mut locked, &current_task).entry.node;
        assert_eq!(None, node.cached_sid());

        post_setxattr(
            current_task.as_ref(),
            node.as_ref(),
            "security.selinu!".into(), // Note: name != "security.selinux".
            VALID_SECURITY_CONTEXT.into(),
        );

        assert_eq!(None, node.cached_sid());
    }

    #[fuchsia::test]
    async fn post_setxattr_clear_invalid_security_context() {
        let security_server = security_server_with_policy(Mode::Enable);
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
        assert_ne!(None, node.cached_sid());

        post_setxattr(
            current_task.as_ref(),
            node.as_ref(),
            "security.selinux".into(),
            "!".into(), // Note: Not a valid security context.
        );

        assert_eq!(None, node.cached_sid());
    }

    #[fuchsia::test]
    async fn post_setxattr_set_sid_selinux_enforcing() {
        let security_server = security_server_with_policy(Mode::Enable);
        security_server.set_enforcing(true);
        let (_kernel, current_task, mut locked) =
            create_kernel_task_and_unlocked_with_selinux(security_server);
        let node = &create_test_file(&mut locked, &current_task).entry.node;
        assert_eq!(None, node.cached_sid());

        post_setxattr(
            current_task.as_ref(),
            node.as_ref(),
            "security.selinux".into(),
            VALID_SECURITY_CONTEXT.into(),
        );

        assert!(node.cached_sid().is_some());
    }

    #[fuchsia::test]
    async fn post_setxattr_different_sid_for_different_context() {
        let security_server = security_server_with_policy(Mode::Enable);
        security_server.set_enforcing(true);
        let (_kernel, current_task, mut locked) =
            create_kernel_task_and_unlocked_with_selinux(security_server);
        let node = &create_test_file(&mut locked, &current_task).entry.node;
        assert_eq!(None, node.cached_sid());

        post_setxattr(
            current_task.as_ref(),
            node.as_ref(),
            "security.selinux".into(),
            VALID_SECURITY_CONTEXT.into(),
        );

        assert!(node.cached_sid().is_some());

        let first_sid = node.cached_sid().unwrap();
        post_setxattr(
            current_task.as_ref(),
            node.as_ref(),
            "security.selinux".into(),
            DIFFERENT_VALID_SECURITY_CONTEXT.into(),
        );

        assert!(node.cached_sid().is_some());

        let second_sid = node.cached_sid().unwrap();

        assert_ne!(first_sid, second_sid);
    }

    #[fuchsia::test]
    async fn set_get_procattr() {
        let security_server = security_server_with_policy(Mode::Enable);
        security_server.set_enforcing(true);
        let (_kernel, current_task) = create_kernel_and_task_with_selinux(security_server);

        assert_eq!(
            get_procattr(&current_task, &current_task.temp_task(), ProcAttr::Exec),
            Ok(Vec::new())
        );

        assert_eq!(
            // Test policy allows "kernel_t" tasks to set the "exec" context.
            set_procattr(&current_task, ProcAttr::Exec, VALID_SECURITY_CONTEXT.into()),
            Ok(())
        );

        assert_eq!(
            // Test policy does not allow "kernel_t" tasks to set the "fscreate" context.
            set_procattr(
                &current_task,
                ProcAttr::FsCreate,
                DIFFERENT_VALID_SECURITY_CONTEXT.into()
            ),
            error!(EACCES)
        );

        assert_eq!(
            // Cannot set an invalid context.
            set_procattr(&current_task, ProcAttr::Exec, INVALID_SECURITY_CONTEXT.into()),
            error!(EINVAL)
        );

        assert_eq!(
            get_procattr(&current_task, &current_task.temp_task(), ProcAttr::Exec),
            Ok(VALID_SECURITY_CONTEXT.into())
        );

        assert!(get_procattr(&current_task, &current_task.temp_task(), ProcAttr::Current).is_ok());
    }

    #[fuchsia::test]
    async fn set_get_procattr_with_nulls() {
        let security_server = security_server_with_policy(Mode::Enable);
        security_server.set_enforcing(true);
        let (_kernel, current_task) = create_kernel_and_task_with_selinux(security_server);

        assert_eq!(
            get_procattr(&current_task, &current_task.temp_task(), ProcAttr::Exec),
            Ok(Vec::new())
        );

        assert_eq!(
            // Setting a Context with a string with trailing null(s) should work, if the Context is valid.
            set_procattr(&current_task, ProcAttr::Exec, VALID_SECURITY_CONTEXT_WITH_NULL.into()),
            Ok(())
        );

        assert_eq!(
            // Nulls in the middle of an otherwise valid Context truncate it, rendering it invalid.
            set_procattr(
                &current_task,
                ProcAttr::FsCreate,
                INVALID_SECURITY_CONTEXT_BECAUSE_OF_NULL.into()
            ),
            error!(EINVAL)
        );

        assert_eq!(
            get_procattr(&current_task, &current_task.temp_task(), ProcAttr::Exec),
            Ok(VALID_SECURITY_CONTEXT.into())
        );

        assert_eq!(
            get_procattr(&current_task, &current_task.temp_task(), ProcAttr::FsCreate),
            Ok(Vec::new())
        );
    }

    #[fuchsia::test]
    async fn set_get_procattr_clear_context() {
        let security_server = security_server_with_policy(Mode::Enable);
        security_server.set_enforcing(false);
        let (_kernel, current_task) = create_kernel_and_task_with_selinux(security_server);

        // Set up the "exec" and "fscreate" Contexts with valid values.
        assert_eq!(
            set_procattr(&current_task, ProcAttr::Exec, VALID_SECURITY_CONTEXT.into()),
            Ok(())
        );
        assert_eq!(
            set_procattr(
                &current_task,
                ProcAttr::FsCreate,
                DIFFERENT_VALID_SECURITY_CONTEXT.into()
            ),
            Ok(())
        );

        // Clear the "exec" context with a write containing a single null octet.
        assert_eq!(set_procattr(&current_task, ProcAttr::Exec, b"\0"), Ok(()));
        assert_eq!(current_task.read().security_state.0.exec_sid, None);

        // Clear the "fscreate" context with a write containing a single newline.
        assert_eq!(set_procattr(&current_task, ProcAttr::Exec, b"\x0a"), Ok(()));
        assert_eq!(current_task.read().security_state.0.fscreate_sid, None);
    }

    #[fuchsia::test]
    async fn set_get_procattr_selinux_permissive() {
        let security_server = security_server_with_policy(Mode::Enable);
        security_server.set_enforcing(false);
        let (_kernel, current_task) = create_kernel_and_task_with_selinux(security_server);

        assert_eq!(
            get_procattr(&current_task, &current_task.temp_task(), ProcAttr::Exec),
            Ok(Vec::new())
        );

        assert_eq!(
            // Test policy allows "kernel_t" tasks to set the "exec" context.
            set_procattr(&current_task, ProcAttr::Exec, VALID_SECURITY_CONTEXT.into()),
            Ok(())
        );

        assert_eq!(
            // Test policy does not allow "kernel_t" tasks to set the "fscreate" context, but
            // in permissive mode the setting will be allowed.
            set_procattr(
                &current_task,
                ProcAttr::FsCreate,
                DIFFERENT_VALID_SECURITY_CONTEXT.into()
            ),
            Ok(())
        );

        // TODO(b/331375792): Validate Contexts even when permission checks are permissive,
        //assert_eq!(
        //    // Setting an invalid context should fail, even in permissive mode.
        //    set_procattr(&current_task, ProcAttr::Exec, INVALID_SECURITY_CONTEXT.into()),
        //    error!(EINVAL)
        //);

        // TODO(b/331375792): Allow permissive set-attr to succeed.
        //assert_eq!(
        //    get_procattr(&current_task, &current_task.temp_task(), ProcAttr::Exec),
        //    Ok(DIFFERENT_VALID_SECURITY_CONTEXT.into())
        //);

        assert!(get_procattr(&current_task, &current_task.temp_task(), ProcAttr::Current).is_ok());
    }

    #[fuchsia::test]
    async fn set_get_procattr_selinux_disabled() {
        let (_kernel, current_task) = create_kernel_and_task();

        assert_eq!(
            set_procattr(&current_task, ProcAttr::Exec, VALID_SECURITY_CONTEXT.into()),
            error!(EINVAL)
        );

        assert_eq!(
            // Test policy allows "kernel_t" tasks to set the "exec" context.
            set_procattr(&current_task, ProcAttr::Exec, VALID_SECURITY_CONTEXT.into()),
            error!(EINVAL)
        );

        assert_eq!(
            // Test policy does not allow "kernel_t" tasks to set the "fscreate" context.
            set_procattr(&current_task, ProcAttr::FsCreate, VALID_SECURITY_CONTEXT.into()),
            error!(EINVAL)
        );

        assert_eq!(
            // Cannot set an invalid context.
            set_procattr(&current_task, ProcAttr::Exec, INVALID_SECURITY_CONTEXT.into()),
            error!(EINVAL)
        );

        assert_eq!(
            get_procattr(&current_task, &current_task.temp_task(), ProcAttr::Current),
            Ok(b"unconfined".to_vec())
        );
    }

    #[fuchsia::test]
    async fn compute_fs_node_security_id_missing_xattr_unlabeled() {
        let security_server = security_server_with_policy(Mode::Enable);
        security_server.set_enforcing(true);
        let (_kernel, current_task, mut locked) =
            create_kernel_task_and_unlocked_with_selinux(security_server);
        let node = &create_test_file(&mut locked, &current_task).entry.node;
        assert_eq!(None, node.cached_sid());

        assert_eq!(
            SecurityId::initial(InitialSid::Unlabeled),
            get_fs_node_security_id(&current_task, node)
        );
        assert_eq!(None, node.cached_sid());
    }

    #[fuchsia::test]
    async fn compute_fs_node_security_id_invalid_xattr_unlabeled() {
        let security_server = security_server_with_policy(Mode::Enable);
        security_server.set_enforcing(true);
        let (_kernel, current_task, mut locked) =
            create_kernel_task_and_unlocked_with_selinux(security_server);
        let node = &create_test_file(&mut locked, &current_task).entry.node;
        node.ops()
            .set_xattr(node, &current_task, "security.selinux".into(), "".into(), XattrOp::Set)
            .expect("setxattr");
        assert_eq!(None, node.cached_sid());

        assert_eq!(
            SecurityId::initial(InitialSid::Unlabeled),
            get_fs_node_security_id(&current_task, node)
        );
        assert_eq!(None, node.cached_sid());
    }

    #[fuchsia::test]
    async fn compute_fs_node_security_id_valid_xattr_stored() {
        let security_server = security_server_with_policy(Mode::Enable);
        security_server.set_enforcing(true);
        let (_kernel, current_task, mut locked) =
            create_kernel_task_and_unlocked_with_selinux(security_server);
        let node = &create_test_file(&mut locked, &current_task).entry.node;
        node.ops()
            .set_xattr(
                node,
                &current_task,
                "security.selinux".into(),
                VALID_SECURITY_CONTEXT.into(),
                XattrOp::Set,
            )
            .expect("setxattr");
        assert_eq!(None, node.cached_sid());

        let security_id = get_fs_node_security_id(&current_task, node);
        assert_eq!(Some(security_id), node.cached_sid());
    }
}
