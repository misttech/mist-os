// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::{selinux_hooks, FileSystemState, ResolvedElfState, TaskState};
use crate::security::KernelState;
use crate::task::{CurrentTask, Kernel, Task};
use crate::vfs::fs_args::MountParams;
use crate::vfs::{
    DirEntry, FileSystemHandle, FsNode, FsStr, FsString, NamespaceNode, ValueOrSize, XattrOp,
};
use selinux::{SecurityPermission, SecurityServer};
use starnix_logging::log_debug;
use starnix_uapi::arc_key::WeakKey;
use starnix_uapi::errors::Errno;
use starnix_uapi::mount_flags::MountFlags;
use starnix_uapi::ownership::TempRef;
use starnix_uapi::signals::Signal;
use starnix_uapi::unmount_flags::UnmountFlags;
use starnix_uapi::{errno, error};
use std::sync::Arc;

/// Executes the `hook` closure if SELinux is enabled, and has a policy loaded.
/// If SELinux is not enabled, or has no policy loaded, then the `default` closure is executed,
/// and its result returned.
fn if_selinux_else<F, R, D>(task: &Task, hook: F, default: D) -> R
where
    F: FnOnce(&Arc<SecurityServer>) -> R,
    D: Fn() -> R,
{
    task.kernel().security_state.state.as_ref().map_or_else(&default, |state| {
        if state.server.has_policy() {
            hook(&state.server)
        } else {
            default()
        }
    })
}

/// Specialization of `if_selinux_else(...)` for hooks which return a `Result<..., Errno>`, that
/// arranges to return a default `Ok(...)` result value if SELinux is not enabled, or not yet
/// configured with a policy.
fn if_selinux_else_default_ok<R, F>(task: &Task, hook: F) -> Result<R, Errno>
where
    F: FnOnce(&Arc<SecurityServer>) -> Result<R, Errno>,
    R: Default,
{
    if_selinux_else(task, hook, || Ok(R::default()))
}

/// Returns the security state structure for the kernel, based on the supplied "selinux" argument
/// contents.
pub fn kernel_init_security(enabled: bool) -> KernelState {
    KernelState { state: enabled.then(|| selinux_hooks::kernel_init_security()) }
}

/// Return security state to associate with a filesystem based on the supplied mount options.
/// This sits somewhere between `fs_context_parse_param()` and `sb_set_mnt_opts()` in function.
pub fn file_system_init_security(mount_params: &MountParams) -> Result<FileSystemState, Errno> {
    Ok(FileSystemState { state: selinux_hooks::file_system_init_security(mount_params)? })
}

/// Gives the hooks subsystem an opportunity to note that the new `file_system` needs labeling, if
/// SELinux is enabled, but no policy has yet been loaded.
// TODO: https://fxbug.dev/366405587 - Merge this logic into `file_system_resolve_security()` and
// remove this extra hook.
pub fn file_system_post_init_security(kernel: &Kernel, file_system: &FileSystemHandle) {
    if let Some(state) = &kernel.security_state.state {
        if !state.server.has_policy() {
            // TODO: https://fxbug.dev/367585803 - Revise locking to guard against a policy load
            // sneaking in, in-between `has_policy()` and this `insert()`.
            log_debug!("Queuing {} FileSystem for labeling", file_system.name());
            state.pending_file_systems.lock().insert(WeakKey::from(&file_system));
        }
    }
}

/// Resolves the labeling scheme and arguments for the `file_system`, based on the loaded policy.
/// If no policy has yet been loaded then no work is done, and the `file_system` will instead be
/// labeled when a policy is first loaded.
/// If the `file_system` was already labelled then no further work is done.
pub fn file_system_resolve_security(
    current_task: &CurrentTask,
    file_system: &FileSystemHandle,
) -> Result<(), Errno> {
    if_selinux_else_default_ok(current_task, |security_server| {
        selinux_hooks::file_system_resolve_security(security_server, current_task, file_system)
    })
}

/// Used to return an extended attribute name and value to apply to a [`crate::vfs::FsNode`].
pub struct FsNodeSecurityXattr {
    pub name: &'static FsStr,
    pub value: FsString,
}

/// Called by the VFS to initialize the security state for an `FsNode` that is being linked at
/// `dir_entry`.
/// If the `FsNode` security state had already been initialized, or no policy is yet loaded, then
/// this is a no-op.
pub fn fs_node_init_with_dentry(
    current_task: &CurrentTask,
    dir_entry: &DirEntry,
) -> Result<(), Errno> {
    if_selinux_else_default_ok(current_task, |security_server| {
        selinux_hooks::fs_node_init_with_dentry(security_server, current_task, dir_entry)
    })
}

/// Called by file-system implementations when creating the `FsNode` for a new file, to determine the
/// correct label based on the `CurrentTask` and `parent` node, and the policy-defined transition
/// rules, and to initialize the `FsNode`'s security state accordingly.
/// If no policy has yet been loaded then this is a no-op; if the `FsNode` corresponds to an xattr-
/// labeled file then it will receive the file-system's "default" label once a policy is loaded.
/// Returns an extended attribute value to set on the newly-created file if the labeling scheme is
/// `fs_use_xattr`. For other labeling schemes (e.g. `fs_use_trans`, mountpoint-labeling) a label
/// is set on the `FsNode` security state, but no extended attribute is set nor returned.
pub fn fs_node_init_on_create(
    current_task: &CurrentTask,
    new_node: &FsNode,
    parent: &FsNode,
) -> Result<Option<FsNodeSecurityXattr>, Errno> {
    if_selinux_else_default_ok(current_task, |security_server| {
        selinux_hooks::fs_node_init_on_create(security_server, current_task, new_node, parent)
    })
}

/// Return the default initial `TaskState` for kernel tasks.
pub fn task_alloc_for_kernel() -> TaskState {
    TaskState { attrs: selinux_hooks::TaskAttrs::for_kernel() }
}

/// Returns `TaskState` for a new `Task`, based on that of `task`, and the specified clone flags.
pub fn task_alloc(task: &Task, clone_flags: u64) -> TaskState {
    TaskState {
        attrs: if_selinux_else(
            task,
            |_| selinux_hooks::task::task_alloc(&task, clone_flags),
            || selinux_hooks::TaskAttrs::for_selinux_disabled(),
        ),
    }
}

/// Labels an [`crate::vfs::FsNode`], by attaching a pseudo-label to the `fs_node`, which allows
/// indirect resolution of the effective label. Makes the security attributes of `fs_node` track the
/// `task`'s security attributes, even if the task's security attributes change. Called for the
/// /proc/<pid> `FsNode`s when they are created. Corresponds to the `task_to_inode` hook.
pub fn task_to_fs_node(current_task: &CurrentTask, task: &TempRef<'_, Task>, fs_node: &FsNode) {
    if_selinux_else(
        current_task,
        |_| selinux_hooks::task::fs_node_init_with_task(task, &fs_node),
        || (),
    );
}

/// Returns `TaskState` for a new `Task`, based on that of the provided `context`.
pub fn task_for_context(task: &Task, context: &FsStr) -> Result<TaskState, Errno> {
    Ok(TaskState {
        attrs: if_selinux_else(
            task,
            |security_server| {
                Ok(selinux_hooks::TaskAttrs::for_sid(
                    security_server
                        .security_context_to_sid(context.into())
                        .map_err(|_| errno!(EINVAL))?,
                ))
            },
            || Ok(selinux_hooks::TaskAttrs::for_selinux_disabled()),
        )?,
    })
}

/// Returns the serialized Security Context associated with the specified task.
/// If the task's current SID cannot be resolved then an empty string is returned.
/// This combines the `task_getsecid()` and `secid_to_secctx()` hooks, in effect.
pub fn task_get_context(current_task: &CurrentTask, target: &Task) -> Result<Vec<u8>, Errno> {
    if_selinux_else(
        current_task,
        |security_server| {
            selinux_hooks::task::task_get_context(&security_server, &current_task, &target)
        },
        || error!(ENOTSUP),
    )
}

/// Check if creating a task is allowed.
pub fn check_task_create_access(current_task: &CurrentTask) -> Result<(), Errno> {
    if_selinux_else_default_ok(current_task, |security_server| {
        selinux_hooks::task::check_task_create_access(
            &security_server.as_permission_check(),
            current_task,
        )
    })
}

/// Checks if exec is allowed.
pub fn check_exec_access(
    current_task: &CurrentTask,
    executable_node: &FsNode,
) -> Result<ResolvedElfState, Errno> {
    if_selinux_else(
        current_task,
        |security_server| {
            selinux_hooks::task::check_exec_access(&security_server, current_task, executable_node)
        },
        || Ok(ResolvedElfState { sid: None }),
    )
}

/// Updates the SELinux thread group state on exec.
/// Corresponds to the `bprm_committing_creds` and `bprm_committed_creds` hooks.
pub fn update_state_on_exec(current_task: &CurrentTask, elf_security_state: &ResolvedElfState) {
    if_selinux_else(
        current_task,
        |_| {
            selinux_hooks::task::update_state_on_exec(current_task, elf_security_state);
        },
        || (),
    );
}

/// Checks if `source` may exercise the "getsched" permission on `target`.
pub fn check_getsched_access(source: &CurrentTask, target: &Task) -> Result<(), Errno> {
    if_selinux_else_default_ok(source, |security_server| {
        selinux_hooks::task::check_getsched_access(
            &security_server.as_permission_check(),
            &source,
            &target,
        )
    })
}

/// Checks if setsched is allowed.
pub fn check_setsched_access(source: &CurrentTask, target: &Task) -> Result<(), Errno> {
    if_selinux_else_default_ok(source, |security_server| {
        selinux_hooks::task::check_setsched_access(
            &security_server.as_permission_check(),
            &source,
            &target,
        )
    })
}

/// Checks if getpgid is allowed.
pub fn check_getpgid_access(source: &CurrentTask, target: &Task) -> Result<(), Errno> {
    if_selinux_else_default_ok(source, |security_server| {
        selinux_hooks::task::check_getpgid_access(
            &security_server.as_permission_check(),
            &source,
            &target,
        )
    })
}

/// Checks if setpgid is allowed.
pub fn check_setpgid_access(source: &CurrentTask, target: &Task) -> Result<(), Errno> {
    if_selinux_else_default_ok(source, |security_server| {
        selinux_hooks::task::check_setpgid_access(
            &security_server.as_permission_check(),
            &source,
            &target,
        )
    })
}

/// Called when the current task queries the session Id of the `target` task.
/// Corresponds to the `task_getsid` LSM hook.
pub fn check_task_getsid(source: &CurrentTask, target: &Task) -> Result<(), Errno> {
    if_selinux_else_default_ok(source, |security_server| {
        selinux_hooks::task::check_task_getsid(
            &security_server.as_permission_check(),
            &source,
            &target,
        )
    })
}

/// Checks if sending a signal is allowed.
pub fn check_signal_access(
    source: &CurrentTask,
    target: &Task,
    signal: Signal,
) -> Result<(), Errno> {
    if_selinux_else_default_ok(source, |security_server| {
        selinux_hooks::task::check_signal_access(
            &security_server.as_permission_check(),
            &source,
            &target,
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
    if_selinux_else_default_ok(source, |security_server| {
        selinux_hooks::task::check_signal_access(
            &security_server.as_permission_check(),
            &source,
            &target,
            signal,
        )
    })
}

// Checks whether the `parent_tracer_task` is allowed to trace the `current_task`.
pub fn ptrace_traceme(current_task: &CurrentTask, parent_tracer_task: &Task) -> Result<(), Errno> {
    if_selinux_else_default_ok(current_task, |security_server| {
        selinux_hooks::task::ptrace_access_check(
            &security_server.as_permission_check(),
            &current_task,
            &parent_tracer_task,
        )
    })
}

/// Checks whether the current `current_task` is allowed to trace `tracee_task`.
/// This fills the role of both of the LSM `ptrace_traceme` and `ptrace_access_check` hooks.
pub fn ptrace_access_check(current_task: &CurrentTask, tracee_task: &Task) -> Result<(), Errno> {
    if_selinux_else_default_ok(current_task, |security_server| {
        selinux_hooks::task::ptrace_access_check(
            &security_server.as_permission_check(),
            current_task,
            &tracee_task,
        )
    })
}

/// Called when the current task calls prlimit on a different task.
/// Corresponds to the `security_task_prlimit` hook.
pub fn task_prlimit(
    source: &CurrentTask,
    target: &Task,
    check_get_rlimit: bool,
    check_set_rlimit: bool,
) -> Result<(), Errno> {
    if_selinux_else_default_ok(source, |security_server| {
        selinux_hooks::task::task_prlimit(
            &security_server.as_permission_check(),
            &source,
            &target,
            check_get_rlimit,
            check_set_rlimit,
        )
    })
}

/// Check permission before an object specified by `dev_name` is mounted on the mount point named by `path`.
/// `type` contains the filesystem type. `flags` contains the mount flags. `data` contains the filesystem-specific data.
/// Corresponds to the `security_sb_mount` hook.
pub fn sb_mount(
    current_task: &CurrentTask,
    dev_name: &bstr::BStr,
    path: &NamespaceNode,
    fs_type: &bstr::BStr,
    flags: MountFlags,
    data: &bstr::BStr,
) -> Result<(), Errno> {
    if_selinux_else_default_ok(current_task, |security_server| {
        selinux_hooks::sb_mount(
            &security_server.as_permission_check(),
            current_task,
            dev_name,
            path,
            fs_type,
            flags,
            data,
        )
    })
}

/// Checks if `current_task` has the permission to unmount the filesystem mounted on
/// `node` using the unmount flags `flags`.
/// Corresponds to the `security_sb_umount` hook.
pub fn sb_umount(
    current_task: &CurrentTask,
    node: &NamespaceNode,
    flags: UnmountFlags,
) -> Result<(), Errno> {
    if_selinux_else_default_ok(current_task, |security_server| {
        selinux_hooks::sb_umount(&security_server.as_permission_check(), current_task, node, flags)
    })
}

/// Returns the value of the specified "security.*" attribute for `fs_node`.
/// If SELinux is enabled then requests for the "security.selinux" attribute will return the
/// Security Context corresponding to the SID with which `fs_node` has been labelled, even if the
/// node's file system does not generally support extended attributes.
/// If SELinux is not enabled, or the node is not labelled with a SID, then the call is delegated to
/// the [`crate::vfs::FsNodeOps`], so the returned value may not be a valid Security Context.
pub fn fs_node_getsecurity(
    current_task: &CurrentTask,
    fs_node: &FsNode,
    name: &FsStr,
    max_size: usize,
) -> Result<ValueOrSize<FsString>, Errno> {
    if_selinux_else(
        current_task,
        |security_server| {
            selinux_hooks::fs_node_getsecurity(
                security_server,
                current_task,
                fs_node,
                name,
                max_size,
            )
        },
        || fs_node.ops().get_xattr(fs_node, current_task, name, max_size),
    )
}

/// Sets the value of the specified security attribute for `fs_node`.
/// If SELinux is enabled then this also updates the in-kernel SID with which
/// the file node is labelled.
pub fn fs_node_setsecurity(
    current_task: &CurrentTask,
    fs_node: &FsNode,
    name: &FsStr,
    value: &FsStr,
    op: XattrOp,
) -> Result<(), Errno> {
    if_selinux_else(
        current_task,
        |security_server| {
            selinux_hooks::fs_node_setsecurity(
                security_server,
                current_task,
                fs_node,
                name,
                value,
                op,
            )
        },
        || fs_node.ops().set_xattr(fs_node, current_task, name, value, op),
    )
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
    if_selinux_else(
        current_task,
        |security_server| {
            selinux_hooks::task::get_procattr(security_server, current_task, target, attr)
        },
        // If SELinux is disabled then there are no values to return.
        || error!(EINVAL),
    )
}

/// Sets the Security Context associated with the `name`ed entry for the current task.
pub fn set_procattr(
    current_task: &CurrentTask,
    attr: ProcAttr,
    context: &[u8],
) -> Result<(), Errno> {
    if_selinux_else(
        current_task,
        |security_server| {
            selinux_hooks::task::set_procattr(security_server, current_task, attr, context)
        },
        // If SELinux is disabled then no writes are accepted.
        || error!(EINVAL),
    )
}

/// Called by the "selinuxfs" when a policy has been successfully loaded, to allow policy-dependent
/// initialization to be completed. This includes resolving labeling schemes and state for
/// file-systems mounted prior to policy load (e.g. the "selinuxfs" itself), and initializing
/// security state for any file nodes they may already contain.
// TODO: https://fxbug.dev/362917997 - Remove this when SELinux LSM is modularized.
pub fn selinuxfs_policy_loaded(current_task: &CurrentTask) {
    if_selinux_else(
        current_task,
        |security_server| selinux_hooks::selinuxfs_policy_loaded(security_server, current_task),
        || panic!("selinuxfs_policy_loaded() without policy!"),
    )
}

/// Used by the "selinuxfs" module to access the SELinux administration API, if enabled.
// TODO: https://fxbug.dev/335397745 - Return a more restricted API, or ...
// TODO: https://fxbug.dev/362917997 - Remove this when SELinux LSM is modularized.
pub fn selinuxfs_get_admin_api(current_task: &CurrentTask) -> Option<Arc<SecurityServer>> {
    current_task.kernel().security_state.state.as_ref().map(|state| state.server.clone())
}

/// Used by the "selinuxfs" module to perform checks on SELinux API file accesses.
// TODO: https://fxbug.dev/362917997 - Remove this when SELinux LSM is modularized.
pub fn selinuxfs_check_access(
    current_task: &CurrentTask,
    node: &FsNode,
    permission: SecurityPermission,
) -> Result<(), Errno> {
    if_selinux_else_default_ok(current_task, |security_server| {
        selinux_hooks::selinuxfs_check_access(security_server, current_task, node, permission)
    })
}

pub mod testing {
    use super::{selinux_hooks, Arc, KernelState, SecurityServer};
    use starnix_sync::Mutex;

    /// Used by Starnix' `testing.rs` to create `KernelState` wrapping a test-
    /// supplied `SecurityServer`.
    pub fn kernel_state(security_server: Option<Arc<SecurityServer>>) -> KernelState {
        KernelState {
            state: security_server.map(|server| selinux_hooks::KernelState {
                server,
                pending_file_systems: Mutex::default(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::selinux_hooks::testing;
    use crate::security::selinux_hooks::testing::{create_test_file, create_unlabeled_test_file};
    use crate::testing::{
        create_kernel_and_task, create_kernel_and_task_with_selinux,
        create_kernel_task_and_unlocked, create_kernel_task_and_unlocked_with_selinux, create_task,
        spawn_kernel_and_run, AutoReleasableTask,
    };
    use linux_uapi::XATTR_NAME_SELINUX;
    use selinux::{InitialSid, SecurityId};
    use starnix_uapi::signals::SIGTERM;

    const VALID_SECURITY_CONTEXT: &[u8] = b"u:object_r:test_valid_t:s0";
    const DIFFERENT_VALID_SECURITY_CONTEXT: &[u8] = b"u:object_r:test_different_valid_t:s0";

    const VALID_SECURITY_CONTEXT_WITH_NULL: &[u8] = b"u:object_r:test_valid_t:s0\0";
    const INVALID_SECURITY_CONTEXT_BECAUSE_OF_NULL: &[u8] = b"u:object_r:test_valid_\0t:s0";

    const INVALID_SECURITY_CONTEXT: &[u8] = b"not_a_u:object_r:test_valid_t:s0";

    const HOOKS_TESTS_BINARY_POLICY: &[u8] =
        include_bytes!("../../lib/selinux/testdata/micro_policies/hooks_tests_policy.pp");

    fn security_server_with_policy() -> Arc<SecurityServer> {
        let policy_bytes = HOOKS_TESTS_BINARY_POLICY.to_vec();
        let security_server = SecurityServer::new();
        security_server.set_enforcing(true);
        security_server.load_policy(policy_bytes).expect("policy load failed");
        security_server
    }

    fn create_task_pair_with_selinux_disabled() -> (AutoReleasableTask, AutoReleasableTask) {
        let (kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let another_task = create_task(&mut locked, &kernel, "another-task");
        assert!(kernel.security_state.state.is_none());
        (current_task, another_task)
    }

    fn create_task_pair_with_permissive_selinux() -> (AutoReleasableTask, AutoReleasableTask) {
        let security_server = security_server_with_policy();
        security_server.set_enforcing(false);
        let (kernel, current_task, mut locked) =
            create_kernel_task_and_unlocked_with_selinux(security_server);
        let another_task = create_task(&mut locked, &kernel, "another-task");
        (current_task, another_task)
    }

    #[derive(Default, Debug, PartialEq)]
    enum TestHookResult {
        WasRun,
        WasNotRun,
        #[default]
        WasNotRunDefault,
    }

    #[fuchsia::test]
    async fn if_selinux_else_disabled() {
        let (kernel, task) = create_kernel_and_task();
        assert!(kernel.security_state.state.is_none());

        let check_result = if_selinux_else_default_ok(&task, |_| Ok(TestHookResult::WasRun));
        assert_eq!(check_result, Ok(TestHookResult::WasNotRunDefault));

        let run_else_result =
            if_selinux_else(&task, |_| TestHookResult::WasRun, || TestHookResult::WasNotRun);
        assert_eq!(run_else_result, TestHookResult::WasNotRun);
    }

    #[fuchsia::test]
    async fn if_selinux_else_without_policy() {
        let security_server = SecurityServer::new();
        let (_kernel, task) = create_kernel_and_task_with_selinux(security_server);

        let check_result = if_selinux_else_default_ok(&task, |_| Ok(TestHookResult::WasRun));
        assert_eq!(check_result, Ok(TestHookResult::WasNotRunDefault));

        let run_else_result =
            if_selinux_else(&task, |_| TestHookResult::WasRun, || TestHookResult::WasNotRun);
        assert_eq!(run_else_result, TestHookResult::WasNotRun);
    }

    #[fuchsia::test]
    async fn if_selinux_else_with_policy() {
        let security_server = security_server_with_policy();
        let (_kernel, task) = create_kernel_and_task_with_selinux(security_server);

        let check_result = if_selinux_else_default_ok(&task, |_| Ok(TestHookResult::WasRun));
        assert_eq!(check_result, Ok(TestHookResult::WasRun));

        let run_else_result =
            if_selinux_else(&task, |_| TestHookResult::WasRun, || TestHookResult::WasNotRun);
        assert_eq!(run_else_result, TestHookResult::WasRun);
    }

    #[fuchsia::test]
    async fn task_alloc_selinux_disabled() {
        let (_kernel, current_task) = create_kernel_and_task();

        task_alloc(&current_task, 0);
    }

    #[fuchsia::test]
    async fn task_create_access_allowed_for_selinux_disabled() {
        let (kernel, task) = create_kernel_and_task();
        assert!(kernel.security_state.state.is_none());
        assert_eq!(check_task_create_access(&task), Ok(()));
    }

    #[fuchsia::test]
    async fn task_create_access_allowed_for_permissive_mode() {
        let security_server = security_server_with_policy();
        security_server.set_enforcing(false);
        let (_kernel, task) = create_kernel_and_task_with_selinux(security_server);
        assert_eq!(check_task_create_access(&task), Ok(()));
    }

    #[fuchsia::test]
    async fn exec_access_allowed_for_selinux_disabled() {
        let (kernel, task, mut locked) = create_kernel_task_and_unlocked();
        assert!(kernel.security_state.state.is_none());
        let executable_node = &testing::create_test_file(&mut locked, &task).entry.node;
        assert_eq!(check_exec_access(&task, executable_node), Ok(ResolvedElfState { sid: None }));
    }

    #[fuchsia::test]
    async fn exec_access_allowed_for_permissive_mode() {
        let security_server = security_server_with_policy();
        security_server.set_enforcing(false);
        let (_kernel, task, mut locked) =
            create_kernel_task_and_unlocked_with_selinux(security_server);
        let executable_node = &testing::create_test_file(&mut locked, &task).entry.node;
        // Expect that access is granted, and a `SecurityId` is returned in the `ResolvedElfState`.
        let result = check_exec_access(&task, executable_node);
        assert!(result.expect("Exec check should succeed").sid.is_some());
    }

    #[fuchsia::test]
    async fn no_state_update_for_selinux_disabled() {
        let (_kernel, task) = create_kernel_and_task();

        // Without SELinux enabled and a policy loaded, only `InitialSid` values exist
        // in the system.
        let target_sid = SecurityId::initial(InitialSid::Unlabeled);
        let elf_state = ResolvedElfState { sid: Some(target_sid) };

        assert!(task.read().security_state.attrs.current_sid != target_sid);

        let before_hook_sid = task.read().security_state.attrs.current_sid;
        update_state_on_exec(&task, &elf_state);
        assert_eq!(task.read().security_state.attrs.current_sid, before_hook_sid);
    }

    #[fuchsia::test]
    async fn no_state_update_for_selinux_without_policy() {
        let security_server = SecurityServer::new();
        let (_kernel, task) = create_kernel_and_task_with_selinux(security_server);

        // Without SELinux enabled and a policy loaded, only `InitialSid` values exist
        // in the system.
        let initial_state = task.read().security_state.attrs.clone();
        let elf_sid = SecurityId::initial(InitialSid::Unlabeled);
        let elf_state = ResolvedElfState { sid: Some(elf_sid) };
        assert_ne!(elf_sid, initial_state.current_sid);
        update_state_on_exec(&task, &elf_state);
        assert_eq!(task.read().security_state.attrs, initial_state);
    }

    #[fuchsia::test]
    async fn state_update_for_permissive_mode() {
        let security_server = security_server_with_policy();
        security_server.set_enforcing(false);
        let initial_state = selinux_hooks::TaskAttrs::for_kernel();
        let (_kernel, task) = create_kernel_and_task_with_selinux(security_server.clone());
        task.write().security_state.attrs = initial_state.clone();
        let elf_sid = security_server
            .security_context_to_sid(b"u:object_r:fork_no_t:s0".into())
            .expect("invalid security context");
        let elf_state = ResolvedElfState { sid: Some(elf_sid) };
        assert_ne!(elf_sid, initial_state.current_sid);
        update_state_on_exec(&task, &elf_state);
        assert_eq!(task.read().security_state.attrs.current_sid, elf_sid);
    }

    #[fuchsia::test]
    async fn getsched_access_allowed_for_selinux_disabled() {
        let (source_task, target_task) = create_task_pair_with_selinux_disabled();
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
    async fn ptrace_attach_access_allowed_for_permissive_mode() {
        let (tracer_task, tracee_task) = create_task_pair_with_permissive_selinux();
        assert_eq!(ptrace_access_check(&tracer_task, &tracee_task), Ok(()));
    }

    #[fuchsia::test]
    async fn task_prlimit_access_allowed_for_selinux_disabled() {
        let (tracer_task, tracee_task) = create_task_pair_with_selinux_disabled();
        assert_eq!(task_prlimit(&tracer_task, &tracee_task, true, true), Ok(()));
    }

    #[fuchsia::test]
    async fn task_prlimit_access_allowed_for_permissive_mode() {
        let (tracer_task, tracee_task) = create_task_pair_with_permissive_selinux();
        assert_eq!(task_prlimit(&tracer_task, &tracee_task, true, true), Ok(()));
    }

    #[fuchsia::test]
    async fn fs_node_task_to_fs_node_noop_selinux_disabled() {
        spawn_kernel_and_run(|locked, current_task| {
            let node = &create_unlabeled_test_file(locked, current_task).entry.node;
            task_to_fs_node(current_task, &current_task.temp_task(), &node);

            assert_eq!(None, selinux_hooks::get_cached_sid(node));
        });
    }

    #[fuchsia::test]
    async fn fs_node_setsecurity_noop_selinux_disabled() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let node = &create_unlabeled_test_file(&mut locked, &current_task).entry.node;

        fs_node_setsecurity(
            &current_task,
            &node,
            XATTR_NAME_SELINUX.to_bytes().into(),
            VALID_SECURITY_CONTEXT.into(),
            XattrOp::Set,
        )
        .expect("set_xattr(security.selinux) failed");

        assert_eq!(None, selinux_hooks::get_cached_sid(node));
    }

    #[fuchsia::test]
    async fn fs_node_setsecurity_noop_selinux_without_policy() {
        let security_server = SecurityServer::new();
        let (_kernel, current_task, mut locked) =
            create_kernel_task_and_unlocked_with_selinux(security_server);
        let node = &create_unlabeled_test_file(&mut locked, &current_task).entry.node;

        fs_node_setsecurity(
            &current_task,
            &node,
            XATTR_NAME_SELINUX.to_bytes().into(),
            VALID_SECURITY_CONTEXT.into(),
            XattrOp::Set,
        )
        .expect("set_xattr(security.selinux) failed");

        assert_eq!(None, selinux_hooks::get_cached_sid(node));
    }

    #[fuchsia::test]
    async fn fs_node_setsecurity_selinux_permissive() {
        let security_server = security_server_with_policy();
        security_server.set_enforcing(false);
        let expected_sid = security_server
            .security_context_to_sid(VALID_SECURITY_CONTEXT.into())
            .expect("no SID for VALID_SECURITY_CONTEXT");
        let (_kernel, current_task, mut locked) =
            create_kernel_task_and_unlocked_with_selinux(security_server);
        let node = &create_unlabeled_test_file(&mut locked, &current_task).entry.node;

        fs_node_setsecurity(
            &current_task,
            &node,
            XATTR_NAME_SELINUX.to_bytes().into(),
            VALID_SECURITY_CONTEXT.into(),
            XattrOp::Set,
        )
        .expect("set_xattr(security.selinux) failed");

        // Verify that the SID now cached on the node is that SID
        // corresponding to VALID_SECURITY_CONTEXT.
        assert_eq!(Some(expected_sid), selinux_hooks::get_cached_sid(node));
    }

    #[fuchsia::test]
    async fn fs_node_setsecurity_not_selinux_is_noop() {
        let security_server = security_server_with_policy();
        security_server.set_enforcing(true);
        let valid_security_context_sid = security_server
            .security_context_to_sid(VALID_SECURITY_CONTEXT.into())
            .expect("no SID for VALID_SECURITY_CONTEXT");
        let (_kernel, current_task, mut locked) =
            create_kernel_task_and_unlocked_with_selinux(security_server);
        let node = &create_test_file(&mut locked, &current_task).entry.node;
        // The label assigned to the test file on creation must differ from
        // VALID_SECURITY_CONTEXT, otherwise this test may return a false
        // positive.
        let whatever_sid = selinux_hooks::get_cached_sid(node);
        assert_ne!(Some(valid_security_context_sid), whatever_sid);

        fs_node_setsecurity(
            &current_task,
            &node,
            "security.selinu!".into(), // Note: name != "security.selinux".
            VALID_SECURITY_CONTEXT.into(),
            XattrOp::Set,
        )
        .expect("set_xattr(security.selinux) failed");

        // Verify that the node's SID (whatever it was) has not changed.
        assert_eq!(whatever_sid, selinux_hooks::get_cached_sid(node));
    }

    #[fuchsia::test]
    async fn fs_node_setsecurity_clear_invalid_security_context() {
        let security_server = security_server_with_policy();
        security_server.set_enforcing(true);
        let (_kernel, current_task, mut locked) =
            create_kernel_task_and_unlocked_with_selinux(security_server);
        let node = &create_test_file(&mut locked, &current_task).entry.node;
        fs_node_setsecurity(
            &current_task,
            &node,
            XATTR_NAME_SELINUX.to_bytes().into(),
            VALID_SECURITY_CONTEXT.into(),
            XattrOp::Set,
        )
        .expect("set_xattr(security.selinux) failed");
        assert_ne!(None, selinux_hooks::get_cached_sid(node));

        fs_node_setsecurity(
            &current_task,
            &node,
            XATTR_NAME_SELINUX.to_bytes().into(),
            "!".into(), // Note: Not a valid security context.
            XattrOp::Set,
        )
        .expect("set_xattr(security.selinux) failed");

        assert_eq!(None, selinux_hooks::get_cached_sid(node));
    }

    #[fuchsia::test]
    async fn fs_node_setsecurity_set_sid_selinux_enforcing() {
        let security_server = security_server_with_policy();
        security_server.set_enforcing(true);
        let (_kernel, current_task, mut locked) =
            create_kernel_task_and_unlocked_with_selinux(security_server);
        let node = &create_unlabeled_test_file(&mut locked, &current_task).entry.node;

        fs_node_setsecurity(
            &current_task,
            &node,
            XATTR_NAME_SELINUX.to_bytes().into(),
            VALID_SECURITY_CONTEXT.into(),
            XattrOp::Set,
        )
        .expect("set_xattr(security.selinux) failed");

        assert!(selinux_hooks::get_cached_sid(node).is_some());
    }

    #[fuchsia::test]
    async fn fs_node_setsecurity_different_sid_for_different_context() {
        let security_server = security_server_with_policy();
        security_server.set_enforcing(true);
        let (_kernel, current_task, mut locked) =
            create_kernel_task_and_unlocked_with_selinux(security_server);
        let node = &create_unlabeled_test_file(&mut locked, &current_task).entry.node;

        fs_node_setsecurity(
            &current_task,
            &node,
            XATTR_NAME_SELINUX.to_bytes().into(),
            VALID_SECURITY_CONTEXT.into(),
            XattrOp::Set,
        )
        .expect("set_xattr(security.selinux) failed");

        assert!(selinux_hooks::get_cached_sid(node).is_some());

        let first_sid = selinux_hooks::get_cached_sid(node).unwrap();
        fs_node_setsecurity(
            &current_task,
            &node,
            XATTR_NAME_SELINUX.to_bytes().into(),
            DIFFERENT_VALID_SECURITY_CONTEXT.into(),
            XattrOp::Set,
        )
        .expect("set_xattr(security.selinux) failed");

        assert!(selinux_hooks::get_cached_sid(node).is_some());

        let second_sid = selinux_hooks::get_cached_sid(node).unwrap();

        assert_ne!(first_sid, second_sid);
    }

    #[fuchsia::test]
    async fn fs_node_getsecurity_returns_cached_context() {
        let security_server = security_server_with_policy();
        security_server.set_enforcing(true);
        let (_kernel, current_task, mut locked) =
            create_kernel_task_and_unlocked_with_selinux(security_server.clone());
        let node = &create_test_file(&mut locked, &current_task).entry.node;

        // Set a mismatched value in `node`'s "security.seliux" attribute.
        const TEST_VALUE: &str = "Something Random";
        node.ops()
            .set_xattr(
                node,
                &current_task,
                XATTR_NAME_SELINUX.to_bytes().into(),
                TEST_VALUE.into(),
                XattrOp::Set,
            )
            .expect("set_xattr(security.selinux) failed");

        // Attach a valid SID to the `node`.
        let sid = security_server
            .security_context_to_sid(VALID_SECURITY_CONTEXT.into())
            .expect("security context to SID");
        selinux_hooks::set_cached_sid(&node, sid);

        // Reading the security attribute should return the Security Context for the SID, rather than delegating.
        let result =
            fs_node_getsecurity(&current_task, node, XATTR_NAME_SELINUX.to_bytes().into(), 4096);
        assert_eq!(result, Ok(ValueOrSize::Value(FsString::new(VALID_SECURITY_CONTEXT.into()))));
    }

    #[fuchsia::test]
    async fn fs_node_getsecurity_delegates_to_get_xattr() {
        let security_server = security_server_with_policy();
        security_server.set_enforcing(true);
        let (_kernel, current_task, mut locked) =
            create_kernel_task_and_unlocked_with_selinux(security_server);
        let node = &create_test_file(&mut locked, &current_task).entry.node;

        // Set an value in `node`'s "security.seliux" attribute.
        const TEST_VALUE: &str = "Something Random";
        node.ops()
            .set_xattr(
                node,
                &current_task,
                XATTR_NAME_SELINUX.to_bytes().into(),
                TEST_VALUE.into(),
                XattrOp::Set,
            )
            .expect("set_xattr(security.selinux) failed");

        // Ensure that there is no SID cached on the node.
        selinux_hooks::clear_cached_sid(&node);

        // Reading the security attribute should pass-through to read the value from the file system.
        let result =
            fs_node_getsecurity(&current_task, node, XATTR_NAME_SELINUX.to_bytes().into(), 4096);
        assert_eq!(result, Ok(ValueOrSize::Value(FsString::new(TEST_VALUE.into()))));
    }

    #[fuchsia::test]
    async fn set_get_procattr() {
        let security_server = security_server_with_policy();
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
            // Test policy does not allow "kernel_t" tasks to set the "sockcreate" context.
            set_procattr(
                &current_task,
                ProcAttr::SockCreate,
                DIFFERENT_VALID_SECURITY_CONTEXT.into()
            ),
            error!(EACCES)
        );

        assert_eq!(
            // It is never permitted to set the "previous" context.
            set_procattr(
                &current_task,
                ProcAttr::Previous,
                DIFFERENT_VALID_SECURITY_CONTEXT.into()
            ),
            error!(EINVAL)
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
        let security_server = security_server_with_policy();
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
        let security_server = security_server_with_policy();
        security_server.set_enforcing(true);
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
        assert_eq!(current_task.read().security_state.attrs.exec_sid, None);

        // Clear the "fscreate" context with a write containing a single newline.
        assert_eq!(set_procattr(&current_task, ProcAttr::FsCreate, b"\x0a"), Ok(()));
        assert_eq!(current_task.read().security_state.attrs.fscreate_sid, None);
    }

    #[fuchsia::test]
    async fn set_get_procattr_setcurrent() {
        let security_server = security_server_with_policy();
        security_server.set_enforcing(true);
        let (_kernel, current_task) = create_kernel_and_task_with_selinux(security_server);

        // Stash the initial "previous" context.
        let initial_previous =
            get_procattr(&current_task, &current_task.temp_task(), ProcAttr::Previous).unwrap();

        assert_eq!(
            // Dynamically transition to a valid new context.
            set_procattr(&current_task, ProcAttr::Current, VALID_SECURITY_CONTEXT.into()),
            Ok(())
        );

        assert_eq!(
            // "current" should report the new context.
            get_procattr(&current_task, &current_task.temp_task(), ProcAttr::Current),
            Ok(VALID_SECURITY_CONTEXT.into())
        );

        assert_eq!(
            // "prev" should continue to report the original context.
            get_procattr(&current_task, &current_task.temp_task(), ProcAttr::Previous),
            Ok(initial_previous.clone())
        );

        assert_eq!(
            // Dynamically transition to a different valid context.
            set_procattr(&current_task, ProcAttr::Current, DIFFERENT_VALID_SECURITY_CONTEXT.into()),
            Ok(())
        );

        assert_eq!(
            // "current" should report the different new context.
            get_procattr(&current_task, &current_task.temp_task(), ProcAttr::Current),
            Ok(DIFFERENT_VALID_SECURITY_CONTEXT.into())
        );

        assert_eq!(
            // "prev" should continue to report the original context.
            get_procattr(&current_task, &current_task.temp_task(), ProcAttr::Previous),
            Ok(initial_previous.clone())
        );
    }

    #[fuchsia::test]
    async fn set_get_procattr_selinux_permissive() {
        let security_server = security_server_with_policy();
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

        assert_eq!(
            // Setting an invalid context should fail, even in permissive mode.
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
            error!(EINVAL)
        );
    }
}
