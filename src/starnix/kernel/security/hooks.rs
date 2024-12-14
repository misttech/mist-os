// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::{selinux_hooks, FileObjectState, FileSystemState, ResolvedElfState, TaskState};
use crate::security::KernelState;
use crate::task::{CurrentTask, Kernel, Task};
use crate::vfs::fs_args::MountParams;
use crate::vfs::{
    DirEntryHandle, FileHandle, FileObject, FileSystem, FileSystemHandle, FsNode, FsStr, FsString,
    NamespaceNode, ValueOrSize, XattrOp,
};
use fuchsia_inspect_contrib::profile_duration;
use selinux::{SecurityPermission, SecurityServer};
use starnix_logging::log_debug;
use starnix_sync::{FileOpsCore, LockEqualOrBefore, Locked};
use starnix_types::ownership::TempRef;
use starnix_uapi::arc_key::WeakKey;
use starnix_uapi::auth::CAP_SYS_ADMIN;
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::{Access, FileMode};
use starnix_uapi::mount_flags::MountFlags;
use starnix_uapi::signals::Signal;
use starnix_uapi::unmount_flags::UnmountFlags;
use starnix_uapi::{errno, error};
use std::sync::Arc;

bitflags::bitflags! {
    /// The flags about which permissions should be checked when opening an FsNode. Used in the
    /// `fs_node_permission()` hook.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct PermissionFlags: u32 {
        const EXEC = 1 as u32;
        const WRITE = 2 as u32;
        const READ = 4 as u32;
        const APPEND = 8 as u32;
    }
}

impl From<Access> for PermissionFlags {
    fn from(access: Access) -> Self {
        // Note that `Access` doesn't have an `append` bit.
        let mut permissions = PermissionFlags::empty();
        if access.contains(Access::READ) {
            permissions |= PermissionFlags::READ;
        }
        if access.contains(Access::WRITE) {
            permissions |= PermissionFlags::WRITE;
        }
        if access.contains(Access::EXEC) {
            permissions |= PermissionFlags::EXEC;
        }
        permissions
    }
}

/// Executes the `hook` closure if SELinux is enabled, and has a policy loaded.
/// If SELinux is not enabled, or has no policy loaded, then the `default` closure is executed,
/// and its result returned.
fn if_selinux_else_with_context<F, R, D, C>(context: C, task: &Task, hook: F, default: D) -> R
where
    F: FnOnce(C, &Arc<SecurityServer>) -> R,
    D: Fn(C) -> R,
{
    if let Some(state) = task.kernel().security_state.state.as_ref() {
        if state.server.has_policy() {
            hook(context, &state.server)
        } else {
            default(context)
        }
    } else {
        default(context)
    }
}

/// Executes the `hook` closure if SELinux is enabled, and has a policy loaded.
/// If SELinux is not enabled, or has no policy loaded, then the `default` closure is executed,
/// and its result returned.
fn if_selinux_else<F, R, D>(task: &Task, hook: F, default: D) -> R
where
    F: FnOnce(&Arc<SecurityServer>) -> R,
    D: Fn() -> R,
{
    if_selinux_else_with_context(
        (),
        task,
        |_, security_server| hook(security_server),
        |_| default(),
    )
}

/// Specialization of `if_selinux_else(...)` for hooks which return a `Result<..., Errno>`, that
/// arranges to return a default `Ok(...)` result value if SELinux is not enabled, or not yet
/// configured with a policy.
fn if_selinux_else_default_ok_with_context<R, F, C>(
    context: C,
    task: &Task,
    hook: F,
) -> Result<R, Errno>
where
    F: FnOnce(C, &Arc<SecurityServer>) -> Result<R, Errno>,
    R: Default,
{
    if_selinux_else_with_context(context, task, hook, |_| Ok(R::default()))
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
    profile_duration!("security.hooks.kernel_init_security");
    KernelState { state: enabled.then(|| selinux_hooks::kernel_init_security()) }
}

/// Return security state to associate with a filesystem based on the supplied mount options.
/// This sits somewhere between `fs_context_parse_param()` and `sb_set_mnt_opts()` in function.
pub fn file_system_init_security(
    name: &'static FsStr,
    mount_params: &MountParams,
) -> Result<FileSystemState, Errno> {
    profile_duration!("security.hooks.file_system_init_security");
    Ok(FileSystemState { state: selinux_hooks::file_system_init_security(name, mount_params)? })
}

/// Gives the hooks subsystem an opportunity to note that the new `file_system` needs labeling, if
/// SELinux is enabled, but no policy has yet been loaded.
// TODO: https://fxbug.dev/366405587 - Merge this logic into `file_system_resolve_security()` and
// remove this extra hook.
pub fn file_system_post_init_security(kernel: &Kernel, file_system: &FileSystemHandle) {
    profile_duration!("security.hooks.file_system_post_init_security");
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
/// If the `file_system` was already labeled then no further work is done.
pub fn file_system_resolve_security<L>(
    locked: &mut Locked<'_, L>,
    current_task: &CurrentTask,
    file_system: &FileSystemHandle,
) -> Result<(), Errno>
where
    L: LockEqualOrBefore<FileOpsCore>,
{
    profile_duration!("security.hooks.file_system_resolve_security");
    if_selinux_else_default_ok_with_context(locked, current_task, |locked, security_server| {
        selinux_hooks::file_system_resolve_security(
            locked,
            security_server,
            current_task,
            file_system,
        )
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
/// Corresponds to the `d_instantiate()` LSM hook.
pub fn fs_node_init_with_dentry<L>(
    locked: &mut Locked<'_, L>,
    current_task: &CurrentTask,
    dir_entry: &DirEntryHandle,
) -> Result<(), Errno>
where
    L: LockEqualOrBefore<FileOpsCore>,
{
    profile_duration!("security.hooks.fs_node_init_with_dentry");
    // TODO: https://fxbug.dev/367585803 - Don't use `if_selinux_else()` here, because the `has_policy()`
    // check is racey, so doing non-trivial work in the "else" path is unsafe. Instead, call the SELinux
    // hook implementation, and let it label, or queue, the `FsNode` based on the `FileSystem` label
    // state, thereby ensuring safe ordering.
    if let Some(state) = &current_task.kernel().security_state.state {
        selinux_hooks::fs_node_init_with_dentry(locked, &state.server, current_task, dir_entry)
    } else {
        Ok(())
    }
}

/// Called by file-system implementations when creating the `FsNode` for a new file, to determine the
/// correct label based on the `CurrentTask` and `parent` node, and the policy-defined transition
/// rules, and to initialize the `FsNode`'s security state accordingly.
/// If no policy has yet been loaded then this is a no-op; if the `FsNode` corresponds to an xattr-
/// labeled file then it will receive the file-system's "default" label once a policy is loaded.
/// Returns an extended attribute value to set on the newly-created file if the labeling scheme is
/// `fs_use_xattr`. For other labeling schemes (e.g. `fs_use_trans`, mountpoint-labeling) a label
/// is set on the `FsNode` security state, but no extended attribute is set nor returned.
/// Corresponds to the `inode_init_security()` LSM hook.
pub fn fs_node_init_on_create(
    current_task: &CurrentTask,
    new_node: &FsNode,
    parent: &FsNode,
) -> Result<Option<FsNodeSecurityXattr>, Errno> {
    profile_duration!("security.hooks.fs_node_init_on_create");
    if_selinux_else_default_ok(current_task, |security_server| {
        selinux_hooks::fs_node_init_on_create(security_server, current_task, new_node, parent)
    })
}

/// Validate that `current_task` has permission to create a regular file in the `parent` directory,
/// with the specified file `mode`.
/// Corresponds to the `inode_create()` LSM hook.
pub fn check_fs_node_create_access(
    current_task: &CurrentTask,
    parent: &FsNode,
    mode: FileMode,
) -> Result<(), Errno> {
    profile_duration!("security.hooks.check_fs_node_create_access");
    if_selinux_else_default_ok(current_task, |security_server| {
        selinux_hooks::check_fs_node_create_access(security_server, current_task, parent, mode)
    })
}

/// Validate that `current_task` has permission to create a symlink to `old_path` in the `parent`
/// directory.
/// Corresponds to the `inode_symlink()` LSM hook.
pub fn check_fs_node_symlink_access(
    current_task: &CurrentTask,
    parent: &FsNode,
    old_path: &FsStr,
) -> Result<(), Errno> {
    profile_duration!("security.hooks.check_fs_node_symlink_access");
    if_selinux_else_default_ok(current_task, |security_server| {
        selinux_hooks::check_fs_node_symlink_access(security_server, current_task, parent, old_path)
    })
}

/// Validate that `current_task` has permission to create a new directory in the `parent` directory,
/// with the specified file `mode`.
/// Corresponds to the `inode_mkdir()` LSM hook.
pub fn check_fs_node_mkdir_access(
    current_task: &CurrentTask,
    parent: &FsNode,
    mode: FileMode,
) -> Result<(), Errno> {
    profile_duration!("security.hooks.check_fs_node_mkdir_access");
    if_selinux_else_default_ok(current_task, |security_server| {
        selinux_hooks::check_fs_node_mkdir_access(security_server, current_task, parent, mode)
    })
}

/// Validate that `current_task` has permission to create a new special file, socket or pipe, in the
/// `parent` directory, and with the specified file `mode` and `device_id`.
/// For consistency any calls to `mknod()` with a file `mode` specifying a regular file will be
/// validated by `check_fs_node_create_access()` rather than by this hook.
/// Corresponds to the `inode_mknod()` LSM hook.
pub fn check_fs_node_mknod_access(
    current_task: &CurrentTask,
    parent: &FsNode,
    mode: FileMode,
    device_id: DeviceType,
) -> Result<(), Errno> {
    profile_duration!("security.hooks.check_fs_node_mknod_access");
    assert!(!mode.is_reg());

    if_selinux_else_default_ok(current_task, |security_server| {
        selinux_hooks::check_fs_node_mknod_access(
            security_server,
            current_task,
            parent,
            mode,
            device_id,
        )
    })
}

/// Validate that `current_task` has  the permission to create a new hard link to a file.
/// Corresponds to the `inode_link()` LSM hook.
pub fn check_fs_node_link_access(
    current_task: &CurrentTask,
    parent: &FsNode,
    child: &FsNode,
) -> Result<(), Errno> {
    profile_duration!("security.hooks.check_fs_node_link_access");
    if_selinux_else_default_ok(current_task, |security_server| {
        selinux_hooks::check_fs_node_link_access(security_server, current_task, parent, child)
    })
}

/// Validate that `current_task` has the permission to remove a hard link to a file.
/// Corresponds to the `inode_unlink()` LSM hook.
pub fn check_fs_node_unlink_access(
    current_task: &CurrentTask,
    parent: &FsNode,
    child: &FsNode,
) -> Result<(), Errno> {
    profile_duration!("security.hooks.check_fs_node_unlink_access");
    if_selinux_else_default_ok(current_task, |security_server| {
        selinux_hooks::check_fs_node_unlink_access(security_server, current_task, parent, child)
    })
}

/// Validate that `current_task` has the permission to remove a directory.
/// Corresponds to the `inode_rmdir()` LSM hook.
pub fn check_fs_node_rmdir_access(
    current_task: &CurrentTask,
    parent: &FsNode,
    child: &FsNode,
) -> Result<(), Errno> {
    profile_duration!("security.hooks.check_fs_node_rmdir_access");
    if_selinux_else_default_ok(current_task, |security_server| {
        selinux_hooks::check_fs_node_rmdir_access(security_server, current_task, parent, child)
    })
}

/// Checks whether the `current_task` can rename the file or directory `moving_node`.
/// If the rename replaces an existing node, `replaced_node` must contain a reference to the
/// existing node.
/// Corresponds to the `inode_rename()` LSM hook.
pub fn check_fs_node_rename_access(
    current_task: &CurrentTask,
    old_parent: &FsNode,
    moving_node: &FsNode,
    new_parent: &FsNode,
    replaced_node: Option<&FsNode>,
) -> Result<(), Errno> {
    profile_duration!("security.hooks.check_fs_node_rename_access");
    if_selinux_else_default_ok(current_task, |security_server| {
        selinux_hooks::check_fs_node_rename_access(
            security_server,
            current_task,
            old_parent,
            moving_node,
            new_parent,
            replaced_node,
        )
    })
}

/// Checks whether the `current_task` can read the symbolic link in `fs_node`.
/// Corresponds to the `inode_readlink()` LSM hook.
pub fn check_fs_node_read_link_access(
    current_task: &CurrentTask,
    fs_node: &FsNode,
) -> Result<(), Errno> {
    profile_duration!("security.hooks.check_fs_node_read_link_access");
    if_selinux_else_default_ok(current_task, |security_server| {
        selinux_hooks::check_fs_node_read_link_access(security_server, current_task, fs_node)
    })
}

/// Checks whether the `current_task` can access an inode.
/// Corresponds to the `inode_permission()` LSM hook.
pub fn fs_node_permission(
    current_task: &CurrentTask,
    fs_node: &FsNode,
    permission_flags: PermissionFlags,
) -> Result<(), Errno> {
    profile_duration!("security.hooks.fs_node_permission");
    if_selinux_else_default_ok(current_task, |security_server| {
        selinux_hooks::fs_node_permission(security_server, current_task, fs_node, permission_flags)
    })
}

/// Returns the security state for a new file object created by `current_task`.
/// Corresponds to the `file_alloc_security()` LSM hook.
pub fn file_alloc_security(current_task: &CurrentTask) -> FileObjectState {
    profile_duration!("security.hooks.file_alloc_security");
    FileObjectState { state: selinux_hooks::file_alloc_security(current_task) }
}

/// Returns whether `current_task` can issue an ioctl to `file`.
/// Corresponds to the `file_ioctl()` LSM hook.
pub fn check_file_ioctl_access(
    current_task: &CurrentTask,
    file: &FileObject,
    request: u32,
) -> Result<(), Errno> {
    profile_duration!("security.hooks.check_file_ioctl_access");
    if_selinux_else_default_ok(current_task, |security_server| {
        selinux_hooks::check_file_ioctl_access(security_server, current_task, file, request)
    })
}

/// Return the default initial `TaskState` for kernel tasks.
/// Corresponds to the `task_alloc()` LSM hook, in the special case when current_task is null.
pub fn task_alloc_for_kernel() -> TaskState {
    profile_duration!("security.hooks.task_alloc_for_kernel");
    TaskState(selinux_hooks::TaskAttrs::for_kernel().into())
}

/// Returns `TaskState` for a new `Task`, based on that of `task`, and the specified clone flags.
/// Corresponds to the `task_alloc()` LSM hook.
pub fn task_alloc(task: &Task, clone_flags: u64) -> TaskState {
    profile_duration!("security.hooks.task_alloc");
    TaskState(
        if_selinux_else(
            task,
            |_| selinux_hooks::task::task_alloc(&task, clone_flags),
            || selinux_hooks::TaskAttrs::for_selinux_disabled(),
        )
        .into(),
    )
}

/// Labels an [`crate::vfs::FsNode`], by attaching a pseudo-label to the `fs_node`, which allows
/// indirect resolution of the effective label. Makes the security attributes of `fs_node` track the
/// `task`'s security attributes, even if the task's security attributes change. Called for the
/// /proc/<pid> `FsNode`s when they are created.
/// Corresponds to the `task_to_inode` LSM hook.
pub fn task_to_fs_node(current_task: &CurrentTask, task: &TempRef<'_, Task>, fs_node: &FsNode) {
    profile_duration!("security.hooks.task_to_fs_node");
    // The fs_node_init_with_task hook doesn't require any policy-specific information. Only check
    // if SElinux is enabled before running it.
    if current_task.kernel().security_state.state.is_some() {
        selinux_hooks::task::fs_node_init_with_task(task, &fs_node);
    }
}

/// Returns `TaskState` for a new `Task`, based on that of the provided `context`.
/// Corresponds to a combination of the `task_alloc()` and `setprocattr()` LSM hooks.
/// The difference from those hooks is that this one bypasses the access-checks that would be
/// performed by `set_procattr()`.
pub fn task_for_context(task: &Task, context: &FsStr) -> Result<TaskState, Errno> {
    profile_duration!("security.hooks.task_for_context");
    Ok(TaskState(
        if_selinux_else(
            task,
            |security_server| {
                Ok(selinux_hooks::TaskAttrs::for_sid(
                    security_server
                        .security_context_to_sid(context.into())
                        .map_err(|e| errno!(EINVAL, format!("{:?}", e)))?,
                ))
            },
            || Ok(selinux_hooks::TaskAttrs::for_selinux_disabled()),
        )?
        .into(),
    ))
}

/// Returns the serialized Security Context associated with the specified task.
/// If the task's current SID cannot be resolved then an empty string is returned.
/// This combines the `task_getsecid()` and `secid_to_secctx()` hooks, in effect.
pub fn task_get_context(current_task: &CurrentTask, target: &Task) -> Result<Vec<u8>, Errno> {
    profile_duration!("security.hooks.task_get_context");
    if_selinux_else(
        current_task,
        |security_server| {
            selinux_hooks::task::task_get_context(&security_server, &current_task, &target)
        },
        || error!(ENOTSUP),
    )
}

/// Checks if creating a task is allowed.
/// Directly maps to the `selinux_task_create` LSM hook from the original NSA white paper.
/// Partially corresponds to the `task_alloc()` LSM hook. Compared to `task_alloc()`,
/// this hook doesn't actually modify the task's label, but instead verifies whether the task has
/// the "fork" permission on itself.
pub fn check_task_create_access(current_task: &CurrentTask) -> Result<(), Errno> {
    profile_duration!("security.hooks.check_task_create_access");
    if_selinux_else_default_ok(current_task, |security_server| {
        selinux_hooks::task::check_task_create_access(
            &security_server.as_permission_check(),
            current_task,
        )
    })
}

/// Checks if exec is allowed.
/// Corresponds to the `check_exec_access()` LSM hook.
pub fn check_exec_access(
    current_task: &CurrentTask,
    executable_node: &FsNode,
) -> Result<ResolvedElfState, Errno> {
    profile_duration!("security.hooks.check_exec_access");
    if_selinux_else(
        current_task,
        |security_server| {
            selinux_hooks::task::check_exec_access(&security_server, current_task, executable_node)
        },
        || Ok(ResolvedElfState { sid: None }),
    )
}

/// Updates the SELinux thread group state on exec.
/// Corresponds to the `bprm_committing_creds()` and `bprm_committed_creds()` hooks.
pub fn update_state_on_exec(current_task: &CurrentTask, elf_security_state: &ResolvedElfState) {
    profile_duration!("security.hooks.update_state_on_exec");
    if_selinux_else(
        current_task,
        |security_server| {
            selinux_hooks::task::update_state_on_exec(
                security_server,
                current_task,
                elf_security_state,
            );
        },
        || (),
    );
}

/// Checks if `source` may exercise the "getsched" permission on `target`.
/// Corresponds to the `task_getscheduler()` LSM hook.
pub fn check_getsched_access(source: &CurrentTask, target: &Task) -> Result<(), Errno> {
    profile_duration!("security.hooks.check_getsched_access");
    if_selinux_else_default_ok(source, |security_server| {
        selinux_hooks::task::check_getsched_access(
            &security_server.as_permission_check(),
            &source,
            &target,
        )
    })
}

/// Checks if setsched is allowed.
/// Corresponds to the `task_setscheduler()` LSM hook.
pub fn check_setsched_access(source: &CurrentTask, target: &Task) -> Result<(), Errno> {
    profile_duration!("security.hooks.check_setsched_access");
    if_selinux_else_default_ok(source, |security_server| {
        selinux_hooks::task::check_setsched_access(
            &security_server.as_permission_check(),
            &source,
            &target,
        )
    })
}

/// Checks if getpgid is allowed.
/// Corresponds to the `task_getpgid()` LSM hook.
pub fn check_getpgid_access(source: &CurrentTask, target: &Task) -> Result<(), Errno> {
    profile_duration!("security.hooks.check_getpgid_access");
    if_selinux_else_default_ok(source, |security_server| {
        selinux_hooks::task::check_getpgid_access(
            &security_server.as_permission_check(),
            &source,
            &target,
        )
    })
}

/// Checks if setpgid is allowed.
/// Corresponds to the `task_setpgid()` LSM hook.
pub fn check_setpgid_access(source: &CurrentTask, target: &Task) -> Result<(), Errno> {
    profile_duration!("security.hooks.check_setpgid_access");
    if_selinux_else_default_ok(source, |security_server| {
        selinux_hooks::task::check_setpgid_access(
            &security_server.as_permission_check(),
            &source,
            &target,
        )
    })
}

/// Called when the current task queries the session Id of the `target` task.
/// Corresponds to the `task_getsid()` LSM hook.
pub fn check_task_getsid(source: &CurrentTask, target: &Task) -> Result<(), Errno> {
    profile_duration!("security.hooks.check_task_getsid");
    if_selinux_else_default_ok(source, |security_server| {
        selinux_hooks::task::check_task_getsid(
            &security_server.as_permission_check(),
            &source,
            &target,
        )
    })
}

/// Checks if sending a signal is allowed.
/// Corresponds to the `task_kill()` LSM hook.
pub fn check_signal_access(
    source: &CurrentTask,
    target: &Task,
    signal: Signal,
) -> Result<(), Errno> {
    profile_duration!("security.hooks.check_signal_access");
    if_selinux_else_default_ok(source, |security_server| {
        selinux_hooks::task::check_signal_access(
            &security_server.as_permission_check(),
            &source,
            &target,
            signal,
        )
    })
}

/// Checks whether the `parent_tracer_task` is allowed to trace the `current_task`.
/// Corresponds to the `ptrace_traceme()` LSM hook.
pub fn ptrace_traceme(current_task: &CurrentTask, parent_tracer_task: &Task) -> Result<(), Errno> {
    profile_duration!("security.hooks.ptrace_traceme");
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
/// Corresponds to the `ptrace_access_check()` LSM hook.
pub fn ptrace_access_check(current_task: &CurrentTask, tracee_task: &Task) -> Result<(), Errno> {
    profile_duration!("security.hooks.ptrace_access_check");
    if_selinux_else_default_ok(current_task, |security_server| {
        selinux_hooks::task::ptrace_access_check(
            &security_server.as_permission_check(),
            current_task,
            &tracee_task,
        )
    })
}

/// Called when the current task calls prlimit on a different task.
/// Corresponds to the `task_prlimit()` LSM hook.
pub fn task_prlimit(
    source: &CurrentTask,
    target: &Task,
    check_get_rlimit: bool,
    check_set_rlimit: bool,
) -> Result<(), Errno> {
    profile_duration!("security.hooks.task_prlimit");
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

/// Check permission before mounting to `path`. `flags` contains the mount flags that determine the
/// kind of mount operation done, and therefore the permissions that the caller requires.
/// Corresponds to the `sb_mount()` LSM hook.
pub fn sb_mount(
    current_task: &CurrentTask,
    path: &NamespaceNode,
    flags: MountFlags,
) -> Result<(), Errno> {
    profile_duration!("security.hooks.sb_mount");
    if_selinux_else_default_ok(current_task, |security_server| {
        selinux_hooks::superblock::sb_mount(
            &security_server.as_permission_check(),
            current_task,
            path,
            flags,
        )
    })
}

/// Checks if `current_task` has the permission to get the filesystem statistics of `fs`.
/// Corresponds to the `sb_statfs()` LSM hook.
pub fn sb_statfs(current_task: &CurrentTask, fs: &FileSystem) -> Result<(), Errno> {
    profile_duration!("security.hooks.sb_statfs");
    if_selinux_else_default_ok(current_task, |security_server| {
        selinux_hooks::superblock::sb_statfs(
            &security_server.as_permission_check(),
            current_task,
            fs,
        )
    })
}

/// Checks if `current_task` has the permission to unmount the filesystem mounted on
/// `node` using the unmount flags `flags`.
/// Corresponds to the `sb_umount()` LSM hook.
pub fn sb_umount(
    current_task: &CurrentTask,
    node: &NamespaceNode,
    flags: UnmountFlags,
) -> Result<(), Errno> {
    profile_duration!("security.hooks.sb_umount");
    if_selinux_else_default_ok(current_task, |security_server| {
        selinux_hooks::superblock::sb_umount(
            &security_server.as_permission_check(),
            current_task,
            node,
            flags,
        )
    })
}

/// Checks if `current_task` has the permission to read file attributes for  `fs_node`.
/// Corresponds to the `inode_getattr()` hook.
pub fn check_fs_node_getattr_access(
    current_task: &CurrentTask,
    fs_node: &FsNode,
) -> Result<(), Errno> {
    profile_duration!("security.hooks.check_fs_node_getattr_access");
    if_selinux_else_default_ok(current_task, |security_server| {
        selinux_hooks::check_fs_node_getattr_access(security_server, current_task, fs_node)
    })
}

/// This is called by Starnix even for filesystems which support extended attributes, unlike Linux
/// LSM.
/// Partially corresponds to the `inode_setxattr()` LSM hook: It is equivalent to
/// `inode_setxattr()` for non-security xattrs, while `fs_node_setsecurity()` is always called for
/// security xattrs. See also [`fs_node_setsecurity()`].
pub fn check_fs_node_setxattr_access(
    current_task: &CurrentTask,
    fs_node: &FsNode,
    name: &FsStr,
    value: &FsStr,
    op: XattrOp,
) -> Result<(), Errno> {
    profile_duration!("security.hooks.check_fs_node_setxattr_access");
    if_selinux_else_default_ok(current_task, |security_server| {
        selinux_hooks::check_fs_node_setxattr_access(
            security_server,
            current_task,
            fs_node,
            name,
            value,
            op,
        )
    })
}

/// Corresponds to the `inode_getxattr()` LSM hook.
pub fn check_fs_node_getxattr_access(
    current_task: &CurrentTask,
    fs_node: &FsNode,
    name: &FsStr,
) -> Result<(), Errno> {
    profile_duration!("security.hooks.check_fs_node_getxattr_access");
    if_selinux_else_default_ok(current_task, |security_server| {
        selinux_hooks::check_fs_node_getxattr_access(security_server, current_task, fs_node, name)
    })
}

/// Corresponds to the `inode_listxattr()` LSM hook.
pub fn check_fs_node_listxattr_access(
    current_task: &CurrentTask,
    fs_node: &FsNode,
) -> Result<(), Errno> {
    profile_duration!("security.hooks.check_fs_node_listxattr_access");
    if_selinux_else_default_ok(current_task, |security_server| {
        selinux_hooks::check_fs_node_listxattr_access(security_server, current_task, fs_node)
    })
}

/// Corresponds to the `inode_removexattr()` LSM hook.
pub fn check_fs_node_removexattr_access(
    current_task: &CurrentTask,
    fs_node: &FsNode,
    name: &FsStr,
) -> Result<(), Errno> {
    profile_duration!("security.hooks.check_fs_node_removexattr_access");
    if_selinux_else_default_ok(current_task, |security_server| {
        selinux_hooks::check_fs_node_removexattr_access(
            security_server,
            current_task,
            fs_node,
            name,
        )
    })
}

/// If SELinux is enabled and `fs_node` is in a filesystem without xattr support, returns the xattr
/// name for the security label associated with inode. Otherwise returns None.
///
/// This hook is called from the `listxattr` syscall.
///
/// Corresponds to the `inode_listsecurity()` LSM hook.
pub fn fs_node_listsecurity(current_task: &CurrentTask, fs_node: &FsNode) -> Option<FsString> {
    profile_duration!("security.hooks.fs_node_listsecurity");
    if_selinux_else(current_task, |_| selinux_hooks::fs_node_listsecurity(fs_node), || None)
}

/// Returns the value of the specified "security.*" attribute for `fs_node`.
/// If SELinux is enabled then requests for the "security.selinux" attribute will return the
/// Security Context corresponding to the SID with which `fs_node` has been labeled, even if the
/// node's file system does not generally support extended attributes.
/// If SELinux is not enabled, or the node is not labeled with a SID, then the call is delegated to
/// the [`crate::vfs::FsNodeOps`], so the returned value may not be a valid Security Context.
/// Corresponds to the `inode_getsecurity()` LSM hook.
pub fn fs_node_getsecurity<L>(
    locked: &mut Locked<'_, L>,
    current_task: &CurrentTask,
    fs_node: &FsNode,
    name: &FsStr,
    max_size: usize,
) -> Result<ValueOrSize<FsString>, Errno>
where
    L: LockEqualOrBefore<FileOpsCore>,
{
    profile_duration!("security.hooks.fs_node_getsecurity");
    if_selinux_else_with_context(
        locked,
        current_task,
        |locked, security_server| {
            selinux_hooks::fs_node_getsecurity(
                locked,
                security_server,
                current_task,
                fs_node,
                name,
                max_size,
            )
        },
        |locked| {
            fs_node.ops().get_xattr(
                &mut locked.cast_locked::<FileOpsCore>(),
                fs_node,
                current_task,
                name,
                max_size,
            )
        },
    )
}

/// Sets the value of the specified security attribute for `fs_node`.
/// If SELinux is enabled then this also updates the in-kernel SID with which
/// the file node is labeled.
///
/// Partially corresponds to the `inode_setsecurity()` and `inode_setxattr()` LSM hooks:
/// In Linux, the LSM hooks are called based on the External Attributes support of the filesystem:
/// * with xattr support: `inode_setxattr()` -> `setxattr()` -> `inode_post_setxattr()`
/// * without xattr support: `inode_setsecurity()`.
///
/// In SEStarnix we instead slice based on whether the xattr being set is in the "security"
/// namespace, or not:
/// * in the security namespace: `fs_node_setsecurity()` (calls `setxattr()` internally)
/// * otherwise: `check_fs_node_setxattr_access()` -> `setxattr()`.
///
/// This is consistent with the way the `*_getsecurity()` hook is used in both Linux and SEStarnix.
pub fn fs_node_setsecurity<L>(
    locked: &mut Locked<'_, L>,
    current_task: &CurrentTask,
    fs_node: &FsNode,
    name: &FsStr,
    value: &FsStr,
    op: XattrOp,
) -> Result<(), Errno>
where
    L: LockEqualOrBefore<FileOpsCore>,
{
    profile_duration!("security.hooks.fs_node_setsecurity");
    if_selinux_else_with_context(
        locked,
        current_task,
        |locked, security_server| {
            selinux_hooks::fs_node_setsecurity(
                locked,
                security_server,
                current_task,
                fs_node,
                name,
                value,
                op,
            )
        },
        |locked| {
            if current_task.creds().has_capability(CAP_SYS_ADMIN) {
                fs_node.ops().set_xattr(
                    &mut locked.cast_locked::<FileOpsCore>(),
                    fs_node,
                    current_task,
                    name,
                    value,
                    op,
                )
            } else {
                Err(errno!(EPERM))
            }
        },
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
/// Corresponds to the `getprocattr()` LSM hook.
pub fn get_procattr(
    current_task: &CurrentTask,
    target: &Task,
    attr: ProcAttr,
) -> Result<Vec<u8>, Errno> {
    profile_duration!("security.hooks.get_procattr");
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
/// Corresponds to the `setprocattr()` LSM hook.
pub fn set_procattr(
    current_task: &CurrentTask,
    attr: ProcAttr,
    context: &[u8],
) -> Result<(), Errno> {
    profile_duration!("security.hooks.set_procattr");
    if_selinux_else(
        current_task,
        |security_server| {
            selinux_hooks::task::set_procattr(security_server, current_task, attr, context)
        },
        // If SELinux is disabled then no writes are accepted.
        || error!(EINVAL),
    )
}

/// Stashes a reference to the selinuxfs null file for later use by hooks that remap
/// inaccessible file descriptors to null.
pub fn selinuxfs_init_null(current_task: &CurrentTask, null_fs_node: &FileHandle) {
    // Note: No `if_selinux_...` guard because hook is invoked inside selinuxfs initialization code;
    // i.e., hook is only invoked when selinux is enabled.
    selinux_hooks::selinuxfs_init_null(current_task, null_fs_node)
}

/// Called by the "selinuxfs" when a policy has been successfully loaded, to allow policy-dependent
/// initialization to be completed. This includes resolving labeling schemes and state for
/// file-systems mounted prior to policy load (e.g. the "selinuxfs" itself), and initializing
/// security state for any file nodes they may already contain.
// TODO: https://fxbug.dev/362917997 - Remove this when SELinux LSM is modularized.
pub fn selinuxfs_policy_loaded<L>(locked: &mut Locked<'_, L>, current_task: &CurrentTask)
where
    L: LockEqualOrBefore<FileOpsCore>,
{
    profile_duration!("security.hooks.selinuxfs_policy_loaded");
    if_selinux_else_with_context(
        locked,
        current_task,
        |locked, security_server| {
            selinux_hooks::selinuxfs_policy_loaded(locked, security_server, current_task)
        },
        |_| panic!("selinuxfs_policy_loaded() without policy!"),
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
    permission: SecurityPermission,
) -> Result<(), Errno> {
    profile_duration!("security.hooks.selinuxfs_check_access");
    if_selinux_else_default_ok(current_task, |security_server| {
        selinux_hooks::selinuxfs_check_access(security_server, current_task, permission)
    })
}

pub mod testing {
    use std::sync::OnceLock;

    use super::{selinux_hooks, Arc, KernelState, SecurityServer};
    use starnix_sync::Mutex;

    /// Used by Starnix' `testing.rs` to create `KernelState` wrapping a test-
    /// supplied `SecurityServer`.
    pub fn kernel_state(security_server: Option<Arc<SecurityServer>>) -> KernelState {
        KernelState {
            state: security_server.map(|server| selinux_hooks::KernelState {
                server,
                pending_file_systems: Mutex::default(),
                selinuxfs_null: OnceLock::default(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::selinux_hooks::testing::{
        self, spawn_kernel_with_selinux_hooks_test_policy_and_run,
    };
    use crate::testing::{create_task, spawn_kernel_and_run, spawn_kernel_with_selinux_and_run};
    use linux_uapi::XATTR_NAME_SELINUX;
    use selinux::{InitialSid, SecurityId};
    use starnix_uapi::signals::SIGTERM;

    const VALID_SECURITY_CONTEXT: &[u8] = b"u:object_r:test_valid_t:s0";
    const DIFFERENT_VALID_SECURITY_CONTEXT: &[u8] = b"u:object_r:test_different_valid_t:s0";

    const VALID_SECURITY_CONTEXT_WITH_NULL: &[u8] = b"u:object_r:test_valid_t:s0\0";
    const INVALID_SECURITY_CONTEXT_BECAUSE_OF_NULL: &[u8] = b"u:object_r:test_valid_\0t:s0";

    const INVALID_SECURITY_CONTEXT: &[u8] = b"not_a_u:object_r:test_valid_t:s0";

    #[derive(Default, Debug, PartialEq)]
    enum TestHookResult {
        WasRun,
        WasNotRun,
        #[default]
        WasNotRunDefault,
    }

    #[fuchsia::test]
    async fn if_selinux_else_disabled() {
        spawn_kernel_and_run(|_locked, current_task| {
            assert!(current_task.kernel().security_state.state.is_none());

            let check_result =
                if_selinux_else_default_ok(current_task, |_| Ok(TestHookResult::WasRun));
            assert_eq!(check_result, Ok(TestHookResult::WasNotRunDefault));

            let run_else_result = if_selinux_else(
                current_task,
                |_| TestHookResult::WasRun,
                || TestHookResult::WasNotRun,
            );
            assert_eq!(run_else_result, TestHookResult::WasNotRun);
        })
    }

    #[fuchsia::test]
    async fn if_selinux_else_without_policy() {
        spawn_kernel_with_selinux_and_run(|_locked, current_task, _security_server| {
            let check_result =
                if_selinux_else_default_ok(current_task, |_| Ok(TestHookResult::WasRun));
            assert_eq!(check_result, Ok(TestHookResult::WasNotRunDefault));

            let run_else_result = if_selinux_else(
                current_task,
                |_| TestHookResult::WasRun,
                || TestHookResult::WasNotRun,
            );
            assert_eq!(run_else_result, TestHookResult::WasNotRun);
        })
    }

    #[fuchsia::test]
    async fn if_selinux_else_with_policy() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |_locked, current_task, _security_server| {
                let check_result =
                    if_selinux_else_default_ok(current_task, |_| Ok(TestHookResult::WasRun));
                assert_eq!(check_result, Ok(TestHookResult::WasRun));

                let run_else_result = if_selinux_else(
                    current_task,
                    |_| TestHookResult::WasRun,
                    || TestHookResult::WasNotRun,
                );
                assert_eq!(run_else_result, TestHookResult::WasRun);
            },
        )
    }

    #[fuchsia::test]
    async fn task_alloc_selinux_disabled() {
        spawn_kernel_and_run(|_locked, current_task| {
            task_alloc(current_task, 0);
        })
    }

    #[fuchsia::test]
    async fn task_create_access_allowed_for_selinux_disabled() {
        spawn_kernel_and_run(|_locked, current_task| {
            assert!(current_task.kernel().security_state.state.is_none());
            assert_eq!(check_task_create_access(current_task), Ok(()));
        })
    }

    #[fuchsia::test]
    async fn task_create_access_allowed_for_permissive_mode() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |_locked, current_task, security_server| {
                security_server.set_enforcing(false);
                assert_eq!(check_task_create_access(current_task), Ok(()));
            },
        )
    }

    #[fuchsia::test]
    async fn exec_access_allowed_for_selinux_disabled() {
        spawn_kernel_and_run(|locked, current_task| {
            assert!(current_task.kernel().security_state.state.is_none());
            let executable_node = &testing::create_test_file(locked, current_task).entry.node;
            assert_eq!(
                check_exec_access(current_task, executable_node),
                Ok(ResolvedElfState { sid: None })
            );
        })
    }

    #[fuchsia::test]
    async fn exec_access_allowed_for_permissive_mode() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |locked, current_task, security_server| {
                security_server.set_enforcing(false);
                let executable_node = &testing::create_test_file(locked, current_task).entry.node;
                // Expect that access is granted, and a `SecurityId` is returned in the `ResolvedElfState`.
                let result = check_exec_access(current_task, executable_node);
                assert!(result.expect("Exec check should succeed").sid.is_some());
            },
        )
    }

    #[fuchsia::test]
    async fn no_state_update_for_selinux_disabled() {
        spawn_kernel_and_run(|_locked, current_task| {
            // Without SELinux enabled and a policy loaded, only `InitialSid` values exist
            // in the system.
            let target_sid = SecurityId::initial(InitialSid::Unlabeled);
            let elf_state = ResolvedElfState { sid: Some(target_sid) };

            assert!(current_task.security_state.lock().current_sid != target_sid);

            let before_hook_sid = current_task.security_state.lock().current_sid;
            update_state_on_exec(current_task, &elf_state);
            assert_eq!(current_task.security_state.lock().current_sid, before_hook_sid);
        })
    }

    #[fuchsia::test]
    async fn no_state_update_for_selinux_without_policy() {
        spawn_kernel_with_selinux_and_run(|_locked, current_task, _security_server| {
            // Without SELinux enabled and a policy loaded, only `InitialSid` values exist
            // in the system.
            let initial_state = current_task.security_state.lock().clone();
            let elf_sid = SecurityId::initial(InitialSid::Unlabeled);
            let elf_state = ResolvedElfState { sid: Some(elf_sid) };
            assert_ne!(elf_sid, initial_state.current_sid);
            update_state_on_exec(current_task, &elf_state);
            assert_eq!(*current_task.security_state.lock(), initial_state);
        })
    }

    #[fuchsia::test]
    async fn state_update_for_permissive_mode() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |_locked, current_task, security_server| {
                security_server.set_enforcing(false);
                let initial_state = selinux_hooks::TaskAttrs::for_kernel();
                *current_task.security_state.lock() = initial_state.clone();
                let elf_sid = security_server
                    .security_context_to_sid(b"u:object_r:fork_no_t:s0".into())
                    .expect("invalid security context");
                let elf_state = ResolvedElfState { sid: Some(elf_sid) };
                assert_ne!(elf_sid, initial_state.current_sid);
                update_state_on_exec(current_task, &elf_state);
                assert_eq!(current_task.security_state.lock().current_sid, elf_sid);
            },
        )
    }

    #[fuchsia::test]
    async fn getsched_access_allowed_for_selinux_disabled() {
        spawn_kernel_and_run(|locked, current_task| {
            let another_task = create_task(locked, &current_task.kernel(), "another-task");
            assert_eq!(check_getsched_access(current_task, &another_task), Ok(()));
        })
    }

    #[fuchsia::test]
    async fn getsched_access_allowed_for_permissive_mode() {
        spawn_kernel_with_selinux_and_run(|locked, current_task, _security_server| {
            let another_task = create_task(locked, &current_task.kernel(), "another-task");
            assert_eq!(check_getsched_access(current_task, &another_task), Ok(()));
        })
    }

    #[fuchsia::test]
    async fn setsched_access_allowed_for_selinux_disabled() {
        spawn_kernel_and_run(|locked, current_task| {
            let another_task = create_task(locked, &current_task.kernel(), "another-task");
            assert_eq!(check_setsched_access(current_task, &another_task), Ok(()));
        })
    }

    #[fuchsia::test]
    async fn setsched_access_allowed_for_permissive_mode() {
        spawn_kernel_with_selinux_and_run(|locked, current_task, _security_server| {
            let another_task = create_task(locked, &current_task.kernel(), "another-task");
            assert_eq!(check_setsched_access(current_task, &another_task), Ok(()));
        })
    }

    #[fuchsia::test]
    async fn getpgid_access_allowed_for_selinux_disabled() {
        spawn_kernel_and_run(|locked, current_task| {
            let another_task = create_task(locked, &current_task.kernel(), "another-task");
            assert_eq!(check_getpgid_access(current_task, &another_task), Ok(()));
        })
    }

    #[fuchsia::test]
    async fn getpgid_access_allowed_for_permissive_mode() {
        spawn_kernel_with_selinux_and_run(|locked, current_task, _security_server| {
            let another_task = create_task(locked, &current_task.kernel(), "another-task");
            assert_eq!(check_getpgid_access(current_task, &another_task), Ok(()));
        })
    }

    #[fuchsia::test]
    async fn setpgid_access_allowed_for_selinux_disabled() {
        spawn_kernel_and_run(|locked, current_task| {
            let another_task = create_task(locked, &current_task.kernel(), "another-task");
            assert_eq!(check_setpgid_access(current_task, &another_task), Ok(()));
        })
    }

    #[fuchsia::test]
    async fn setpgid_access_allowed_for_permissive_mode() {
        spawn_kernel_with_selinux_and_run(|locked, current_task, _security_server| {
            let another_task = create_task(locked, &current_task.kernel(), "another-task");
            assert_eq!(check_setpgid_access(current_task, &another_task), Ok(()));
        })
    }

    #[fuchsia::test]
    async fn task_getsid_allowed_for_selinux_disabled() {
        spawn_kernel_and_run(|locked, current_task| {
            let another_task = create_task(locked, &current_task.kernel(), "another-task");
            assert_eq!(check_task_getsid(current_task, &another_task), Ok(()));
        })
    }

    #[fuchsia::test]
    async fn task_getsid_allowed_for_permissive_mode() {
        spawn_kernel_with_selinux_and_run(|locked, current_task, _security_server| {
            let another_task = create_task(locked, &current_task.kernel(), "another-task");
            assert_eq!(check_task_getsid(current_task, &another_task), Ok(()));
        })
    }

    #[fuchsia::test]
    async fn signal_access_allowed_for_selinux_disabled() {
        spawn_kernel_and_run(|locked, current_task| {
            let another_task = create_task(locked, &current_task.kernel(), "another-task");
            assert_eq!(check_signal_access(current_task, &another_task, SIGTERM), Ok(()));
        })
    }

    #[fuchsia::test]
    async fn signal_access_allowed_for_permissive_mode() {
        spawn_kernel_with_selinux_and_run(|locked, current_task, _security_server| {
            let another_task = create_task(locked, &current_task.kernel(), "another-task");
            assert_eq!(check_signal_access(current_task, &another_task, SIGTERM), Ok(()));
        })
    }

    #[fuchsia::test]
    async fn ptrace_traceme_access_allowed_for_selinux_disabled() {
        spawn_kernel_and_run(|locked, current_task| {
            let another_task = create_task(locked, &current_task.kernel(), "another-task");
            assert_eq!(ptrace_traceme(current_task, &another_task), Ok(()));
        })
    }

    #[fuchsia::test]
    async fn ptrace_traceme_access_allowed_for_permissive_mode() {
        spawn_kernel_with_selinux_and_run(|locked, current_task, _security_server| {
            let another_task = create_task(locked, &current_task.kernel(), "another-task");
            assert_eq!(ptrace_traceme(current_task, &another_task), Ok(()));
        })
    }

    #[fuchsia::test]
    async fn ptrace_attach_access_allowed_for_selinux_disabled() {
        spawn_kernel_and_run(|locked, current_task| {
            let another_task = create_task(locked, &current_task.kernel(), "another-task");
            assert_eq!(ptrace_access_check(current_task, &another_task), Ok(()));
        })
    }

    #[fuchsia::test]
    async fn ptrace_attach_access_allowed_for_permissive_mode() {
        spawn_kernel_with_selinux_and_run(|locked, current_task, _security_server| {
            let another_task = create_task(locked, &current_task.kernel(), "another-task");
            assert_eq!(ptrace_access_check(current_task, &another_task), Ok(()));
        })
    }

    #[fuchsia::test]
    async fn task_prlimit_access_allowed_for_selinux_disabled() {
        spawn_kernel_and_run(|locked, current_task| {
            let another_task = create_task(locked, &current_task.kernel(), "another-task");
            assert_eq!(task_prlimit(current_task, &another_task, true, true), Ok(()));
        })
    }

    #[fuchsia::test]
    async fn task_prlimit_access_allowed_for_permissive_mode() {
        spawn_kernel_with_selinux_and_run(|locked, current_task, _security_server| {
            let another_task = create_task(locked, &current_task.kernel(), "another-task");
            assert_eq!(task_prlimit(current_task, &another_task, true, true), Ok(()));
        })
    }

    #[fuchsia::test]
    async fn fs_node_task_to_fs_node_noop_selinux_disabled() {
        spawn_kernel_and_run(|locked, current_task| {
            let node = &testing::create_test_file(locked, current_task).entry.node;
            task_to_fs_node(current_task, &current_task.temp_task(), &node);
            assert_eq!(None, selinux_hooks::get_cached_sid(node));
        });
    }

    #[fuchsia::test]
    async fn fs_node_setsecurity_selinux_disabled_only_sets_xattr() {
        spawn_kernel_and_run(|locked, current_task| {
            let node = &testing::create_test_file(locked, current_task).entry.node;

            fs_node_setsecurity(
                locked,
                current_task,
                &node,
                XATTR_NAME_SELINUX.to_bytes().into(),
                VALID_SECURITY_CONTEXT.into(),
                XattrOp::Set,
            )
            .expect("set_xattr(security.selinux) failed");

            assert_eq!(None, selinux_hooks::get_cached_sid(node));
        })
    }

    #[fuchsia::test]
    async fn fs_node_setsecurity_selinux_without_policy_only_sets_xattr() {
        spawn_kernel_with_selinux_and_run(|locked, current_task, _security_server| {
            let node = &testing::create_test_file(locked, current_task).entry.node;
            fs_node_setsecurity(
                locked,
                current_task,
                &node,
                XATTR_NAME_SELINUX.to_bytes().into(),
                VALID_SECURITY_CONTEXT.into(),
                XattrOp::Set,
            )
            .expect("set_xattr(security.selinux) failed");

            assert_eq!(None, selinux_hooks::get_cached_sid(node));
        })
    }

    #[fuchsia::test]
    async fn fs_node_setsecurity_selinux_permissive_sets_xattr_and_label() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |locked, current_task, security_server| {
                security_server.set_enforcing(false);
                let expected_sid = security_server
                    .security_context_to_sid(VALID_SECURITY_CONTEXT.into())
                    .expect("no SID for VALID_SECURITY_CONTEXT");
                let node = &testing::create_test_file(locked, &current_task).entry.node;

                // Safeguard against a false positive by ensuring `expected_sid` is not already the file's label.
                assert_ne!(Some(expected_sid), selinux_hooks::get_cached_sid(node));

                fs_node_setsecurity(
                    locked,
                    current_task,
                    &node,
                    XATTR_NAME_SELINUX.to_bytes().into(),
                    VALID_SECURITY_CONTEXT.into(),
                    XattrOp::Set,
                )
                .expect("set_xattr(security.selinux) failed");

                // Verify that the SID now cached on the node is that SID
                // corresponding to VALID_SECURITY_CONTEXT.
                assert_eq!(Some(expected_sid), selinux_hooks::get_cached_sid(node));
            },
        )
    }

    #[fuchsia::test]
    async fn fs_node_setsecurity_not_selinux_only_sets_xattr() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |locked, current_task, security_server| {
                let valid_security_context_sid = security_server
                    .security_context_to_sid(VALID_SECURITY_CONTEXT.into())
                    .expect("no SID for VALID_SECURITY_CONTEXT");
                let node = &testing::create_test_file(locked, current_task).entry.node;
                // The label assigned to the test file on creation must differ from
                // VALID_SECURITY_CONTEXT, otherwise this test may return a false
                // positive.
                let whatever_sid = selinux_hooks::get_cached_sid(node);
                assert_ne!(Some(valid_security_context_sid), whatever_sid);

                fs_node_setsecurity(
                    locked,
                    current_task,
                    &node,
                    "security.selinu!".into(), // Note: name != "security.selinux".
                    VALID_SECURITY_CONTEXT.into(),
                    XattrOp::Set,
                )
                .expect("set_xattr(security.selinux) failed");

                // Verify that the node's SID (whatever it was) has not changed.
                assert_eq!(whatever_sid, selinux_hooks::get_cached_sid(node));
            },
        )
    }

    #[fuchsia::test]
    async fn fs_node_setsecurity_selinux_enforcing_invalid_context_fails() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |locked, current_task, _security_server| {
                let node = &testing::create_test_file(locked, current_task).entry.node;

                let before_sid = selinux_hooks::get_cached_sid(node);
                assert_ne!(Some(SecurityId::initial(InitialSid::Unlabeled)), before_sid);

                assert!(fs_node_setsecurity(
                    locked,
                    &current_task,
                    &node,
                    XATTR_NAME_SELINUX.to_bytes().into(),
                    "!".into(), // Note: Not a valid security context.
                    XattrOp::Set,
                )
                .is_err());

                assert_eq!(before_sid, selinux_hooks::get_cached_sid(node));
            },
        )
    }

    #[fuchsia::test]
    async fn fs_node_setsecurity_selinux_permissive_invalid_context_sets_xattr_and_label() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |locked, current_task, security_server| {
                security_server.set_enforcing(false);
                let node = &testing::create_test_file(locked, current_task).entry.node;

                assert_ne!(
                    Some(SecurityId::initial(InitialSid::Unlabeled)),
                    selinux_hooks::get_cached_sid(node)
                );

                fs_node_setsecurity(
                    locked,
                    current_task,
                    &node,
                    XATTR_NAME_SELINUX.to_bytes().into(),
                    "!".into(), // Note: Not a valid security context.
                    XattrOp::Set,
                )
                .expect("set_xattr(security.selinux) failed");

                assert_eq!(
                    Some(SecurityId::initial(InitialSid::Unlabeled)),
                    selinux_hooks::get_cached_sid(node)
                );
            },
        )
    }

    #[fuchsia::test]
    async fn fs_node_setsecurity_different_sid_for_different_context() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |locked, current_task, _security_server| {
                let node = &testing::create_test_file(locked, current_task).entry.node;

                fs_node_setsecurity(
                    locked,
                    current_task,
                    &node,
                    XATTR_NAME_SELINUX.to_bytes().into(),
                    VALID_SECURITY_CONTEXT.into(),
                    XattrOp::Set,
                )
                .expect("set_xattr(security.selinux) failed");

                assert!(selinux_hooks::get_cached_sid(node).is_some());

                let first_sid = selinux_hooks::get_cached_sid(node).unwrap();
                fs_node_setsecurity(
                    locked,
                    current_task,
                    &node,
                    XATTR_NAME_SELINUX.to_bytes().into(),
                    DIFFERENT_VALID_SECURITY_CONTEXT.into(),
                    XattrOp::Set,
                )
                .expect("set_xattr(security.selinux) failed");

                assert!(selinux_hooks::get_cached_sid(node).is_some());

                let second_sid = selinux_hooks::get_cached_sid(node).unwrap();

                assert_ne!(first_sid, second_sid);
            },
        )
    }

    #[fuchsia::test]
    async fn fs_node_getsecurity_returns_cached_context() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |locked, current_task, security_server| {
                let node = &testing::create_test_file(locked, current_task).entry.node;

                // Set a mismatched value in `node`'s "security.seliux" attribute.
                const TEST_VALUE: &str = "Something Random";
                node.ops()
                    .set_xattr(
                        &mut locked.cast_locked::<FileOpsCore>(),
                        node,
                        current_task,
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
                let result = fs_node_getsecurity(
                    locked,
                    current_task,
                    node,
                    XATTR_NAME_SELINUX.to_bytes().into(),
                    4096,
                );
                assert_eq!(
                    result,
                    Ok(ValueOrSize::Value(FsString::new(VALID_SECURITY_CONTEXT.into())))
                );
            },
        )
    }

    #[fuchsia::test]
    async fn fs_node_getsecurity_delegates_to_get_xattr() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |locked, current_task, security_server| {
                let node = &testing::create_test_file(locked, current_task).entry.node;

                // Set an invalid value in `node`'s "security.selinux" attribute.
                // This requires SELinux to be in permissive mode, otherwise the "relabelto" permission check will fail.
                security_server.set_enforcing(false);
                const TEST_VALUE: &str = "Something Random";
                fs_node_setsecurity(
                    locked,
                    current_task,
                    node,
                    XATTR_NAME_SELINUX.to_bytes().into(),
                    TEST_VALUE.into(),
                    XattrOp::Set,
                )
                .expect("set_xattr(security.selinux) failed");
                security_server.set_enforcing(true);

                // Reading the security attribute should pass-through to read the value from the file system.
                let result = fs_node_getsecurity(
                    locked,
                    current_task,
                    node,
                    XATTR_NAME_SELINUX.to_bytes().into(),
                    4096,
                );
                assert_eq!(result, Ok(ValueOrSize::Value(FsString::new(TEST_VALUE.into()))));
            },
        )
    }

    #[fuchsia::test]
    async fn set_get_procattr() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |_locked, current_task, _security_server| {
                assert_eq!(
                    get_procattr(current_task, current_task, ProcAttr::Exec),
                    Ok(Vec::new())
                );

                assert_eq!(
                    // Test policy allows "kernel_t" tasks to set the "exec" context.
                    set_procattr(current_task, ProcAttr::Exec, VALID_SECURITY_CONTEXT.into()),
                    Ok(())
                );

                assert_eq!(
                    // Test policy does not allow "kernel_t" tasks to set the "sockcreate" context.
                    set_procattr(
                        current_task,
                        ProcAttr::SockCreate,
                        DIFFERENT_VALID_SECURITY_CONTEXT.into()
                    ),
                    error!(EACCES)
                );

                assert_eq!(
                    // It is never permitted to set the "previous" context.
                    set_procattr(
                        current_task,
                        ProcAttr::Previous,
                        DIFFERENT_VALID_SECURITY_CONTEXT.into()
                    ),
                    error!(EINVAL)
                );

                assert_eq!(
                    // Cannot set an invalid context.
                    set_procattr(current_task, ProcAttr::Exec, INVALID_SECURITY_CONTEXT.into()),
                    error!(EINVAL)
                );

                assert_eq!(
                    get_procattr(current_task, current_task, ProcAttr::Exec),
                    Ok(VALID_SECURITY_CONTEXT.into())
                );

                assert!(get_procattr(current_task, current_task, ProcAttr::Current).is_ok());
            },
        )
    }

    #[fuchsia::test]
    async fn set_get_procattr_with_nulls() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |_locked, current_task, _security_server| {
                assert_eq!(
                    get_procattr(current_task, current_task, ProcAttr::Exec),
                    Ok(Vec::new())
                );

                assert_eq!(
                    // Setting a Context with a string with trailing null(s) should work, if the Context is valid.
                    set_procattr(
                        current_task,
                        ProcAttr::Exec,
                        VALID_SECURITY_CONTEXT_WITH_NULL.into()
                    ),
                    Ok(())
                );

                assert_eq!(
                    // Nulls in the middle of an otherwise valid Context truncate it, rendering it invalid.
                    set_procattr(
                        current_task,
                        ProcAttr::FsCreate,
                        INVALID_SECURITY_CONTEXT_BECAUSE_OF_NULL.into()
                    ),
                    error!(EINVAL)
                );

                assert_eq!(
                    get_procattr(current_task, current_task, ProcAttr::Exec),
                    Ok(VALID_SECURITY_CONTEXT.into())
                );

                assert_eq!(
                    get_procattr(current_task, current_task, ProcAttr::FsCreate),
                    Ok(Vec::new())
                );
            },
        )
    }

    #[fuchsia::test]
    async fn set_get_procattr_clear_context() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |_locked, current_task, _security_server| {
                // Set up the "exec" and "fscreate" Contexts with valid values.
                assert_eq!(
                    set_procattr(current_task, ProcAttr::Exec, VALID_SECURITY_CONTEXT.into()),
                    Ok(())
                );
                assert_eq!(
                    set_procattr(
                        current_task,
                        ProcAttr::FsCreate,
                        DIFFERENT_VALID_SECURITY_CONTEXT.into()
                    ),
                    Ok(())
                );

                // Clear the "exec" context with a write containing a single null octet.
                assert_eq!(set_procattr(current_task, ProcAttr::Exec, b"\0"), Ok(()));
                assert_eq!(current_task.security_state.lock().exec_sid, None);

                // Clear the "fscreate" context with a write containing a single newline.
                assert_eq!(set_procattr(current_task, ProcAttr::FsCreate, b"\x0a"), Ok(()));
                assert_eq!(current_task.security_state.lock().fscreate_sid, None);
            },
        )
    }

    #[fuchsia::test]
    async fn set_get_procattr_setcurrent() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |_locked, current_task, _security_server| {
                // Stash the initial "previous" context.
                let initial_previous =
                    get_procattr(current_task, current_task, ProcAttr::Previous).unwrap();

                assert_eq!(
                    // Dynamically transition to a valid new context.
                    set_procattr(current_task, ProcAttr::Current, VALID_SECURITY_CONTEXT.into()),
                    Ok(())
                );

                assert_eq!(
                    // "current" should report the new context.
                    get_procattr(current_task, current_task, ProcAttr::Current),
                    Ok(VALID_SECURITY_CONTEXT.into())
                );

                assert_eq!(
                    // "prev" should continue to report the original context.
                    get_procattr(current_task, current_task, ProcAttr::Previous),
                    Ok(initial_previous.clone())
                );

                assert_eq!(
                    // Dynamically transition to a different valid context.
                    set_procattr(
                        current_task,
                        ProcAttr::Current,
                        DIFFERENT_VALID_SECURITY_CONTEXT.into()
                    ),
                    Ok(())
                );

                assert_eq!(
                    // "current" should report the different new context.
                    get_procattr(current_task, current_task, ProcAttr::Current),
                    Ok(DIFFERENT_VALID_SECURITY_CONTEXT.into())
                );

                assert_eq!(
                    // "prev" should continue to report the original context.
                    get_procattr(current_task, current_task, ProcAttr::Previous),
                    Ok(initial_previous.clone())
                );
            },
        )
    }

    #[fuchsia::test]
    async fn set_get_procattr_selinux_permissive() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |_locked, current_task, security_server| {
                security_server.set_enforcing(false);
                assert_eq!(
                    get_procattr(current_task, &current_task.temp_task(), ProcAttr::Exec),
                    Ok(Vec::new())
                );

                assert_eq!(
                    // Test policy allows "kernel_t" tasks to set the "exec" context.
                    set_procattr(current_task, ProcAttr::Exec, VALID_SECURITY_CONTEXT.into()),
                    Ok(())
                );

                assert_eq!(
                    // Test policy does not allow "kernel_t" tasks to set the "fscreate" context, but
                    // in permissive mode the setting will be allowed.
                    set_procattr(
                        current_task,
                        ProcAttr::FsCreate,
                        DIFFERENT_VALID_SECURITY_CONTEXT.into()
                    ),
                    Ok(())
                );

                assert_eq!(
                    // Setting an invalid context should fail, even in permissive mode.
                    set_procattr(current_task, ProcAttr::Exec, INVALID_SECURITY_CONTEXT.into()),
                    error!(EINVAL)
                );

                assert_eq!(
                    get_procattr(current_task, &current_task.temp_task(), ProcAttr::Exec),
                    Ok(VALID_SECURITY_CONTEXT.into())
                );

                assert!(get_procattr(current_task, &current_task.temp_task(), ProcAttr::Current)
                    .is_ok());
            },
        )
    }

    #[fuchsia::test]
    async fn set_get_procattr_selinux_disabled() {
        spawn_kernel_and_run(|_locked, current_task| {
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
        })
    }
}
