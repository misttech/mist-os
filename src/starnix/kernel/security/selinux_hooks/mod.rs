// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub(super) mod fs;

use super::{FsNodeState, ProcAttr};

use crate::task::CurrentTask;
use crate::vfs::syscalls::LookupFlags;
use crate::vfs::{FsNode, FsNodeHandle, FsStr, NamespaceNode, ValueOrSize};
use selinux::permission_check::PermissionCheck;
use selinux::security_server::SecurityServer;
use selinux::{InitialSid, SecurityId};
use selinux_common::{
    ClassPermission, FilePermission, NullessByteStr, ObjectClass, Permission, ProcessPermission,
};
use starnix_logging::{log_debug, track_stub};
use starnix_uapi::errors::Errno;
use starnix_uapi::mount_flags::MountFlags;
use starnix_uapi::signals::{Signal, SIGCHLD, SIGKILL, SIGSTOP};
use starnix_uapi::{errno, error};
use std::sync::Arc;

/// Maximum supported size for the `security.selinux` value used to store SELinux security
/// contexts in a filesystem node extended attributes.
const SECURITY_SELINUX_XATTR_VALUE_MAX_SIZE: usize = 4096;

/// Checks if creating a task is allowed.
pub(super) fn check_task_create_access(
    permission_check: &impl PermissionCheck,
    task_sid: SecurityId,
) -> Result<(), Errno> {
    // When creating a process there is no transition involved, the source and target SIDs
    // are the current SID.
    let target_sid = task_sid;
    check_permissions(permission_check, task_sid, target_sid, &[ProcessPermission::Fork])
}

/// Checks the SELinux permissions required for exec. Returns the SELinux state of a resolved
/// elf if all required permissions are allowed.
pub(super) fn check_exec_access(
    security_server: &Arc<SecurityServer>,
    current_task: &CurrentTask,
    security_state: &TaskState,
    executable_node: &FsNodeHandle,
) -> Result<Option<SecurityId>, Errno> {
    let executable_sid =
        get_effective_fs_node_security_id(security_server, current_task, executable_node);
    let current_sid = security_state.current_sid;
    let new_sid = if let Some(exec_sid) = security_state.exec_sid {
        // Use the proc exec SID if set.
        exec_sid
    } else {
        security_server
            .compute_new_sid(current_sid, executable_sid, ObjectClass::Process)
            .map_err(|_| errno!(EACCES))?
        // TODO(http://b/319232900): validate that the new context is valid, and return EACCESS if
        // it's not.
    };
    if current_sid == new_sid {
        // To `exec()` a binary in the caller's domain, the caller must be granted
        // "execute_no_trans" permission to the binary.
        if !security_server.has_permissions(
            current_sid,
            executable_sid,
            &[FilePermission::ExecuteNoTrans],
        ) {
            // TODO(http://b/330904217): once filesystems are labeled, deny access.
            log_debug!("execute_no_trans permission is denied, ignoring.");
        }
    } else {
        // Domain transition, check that transition is allowed.
        if !security_server.has_permissions(current_sid, new_sid, &[ProcessPermission::Transition])
        {
            return error!(EACCES);
        }
        // Check that the executable file has an entry point into the new domain.
        if !security_server.has_permissions(new_sid, executable_sid, &[FilePermission::Entrypoint])
        {
            // TODO(http://b/330904217): once filesystems are labeled, deny access.
            log_debug!("entrypoint permission is denied, ignoring.");
        }
        // Check that ptrace permission is allowed if the process is traced.
        // TODO(b/352535794): Perform this check based on the `Task::ptrace` state.
        if let Some(ptracer_sid) = security_state.ptracer_sid {
            if !security_server.has_permissions(ptracer_sid, new_sid, &[ProcessPermission::Ptrace])
            {
                return error!(EACCES);
            }
        }
    }
    Ok(Some(new_sid))
}

/// Updates the SELinux thread group state on exec, using the security ID associated with the
/// resolved elf.
pub(super) fn update_state_on_exec(
    security_state: &mut TaskState,
    elf_security_state: Option<SecurityId>,
) {
    // TODO(http://b/316181721): check if the previous state needs to be updated regardless.
    if let Some(elf_security_state) = elf_security_state {
        security_state.previous_sid = security_state.current_sid;
        security_state.current_sid = elf_security_state;
    }
}

/// Checks if source with `source_sid` may exercise the "getsched" permission on target with
/// `target_sid` according to SELinux server status `status` and permission checker
/// `permission`.
pub(super) fn check_getsched_access(
    permission_check: &impl PermissionCheck,
    source_sid: SecurityId,
    target_sid: SecurityId,
) -> Result<(), Errno> {
    check_permissions(permission_check, source_sid, target_sid, &[ProcessPermission::GetSched])
}

/// Checks if the task with `source_sid` is allowed to set scheduling parameters for the task with
/// `target_sid`.
pub(super) fn check_setsched_access(
    permission_check: &impl PermissionCheck,
    source_sid: SecurityId,
    target_sid: SecurityId,
) -> Result<(), Errno> {
    check_permissions(permission_check, source_sid, target_sid, &[ProcessPermission::SetSched])
}

/// Checks if the task with `source_sid` is allowed to get the process group ID of the task with
/// `target_sid`.
pub(super) fn check_getpgid_access(
    permission_check: &impl PermissionCheck,
    source_sid: SecurityId,
    target_sid: SecurityId,
) -> Result<(), Errno> {
    check_permissions(permission_check, source_sid, target_sid, &[ProcessPermission::GetPgid])
}

/// Checks if the task with `source_sid` is allowed to set the process group ID of the task with
/// `target_sid`.
pub(super) fn check_setpgid_access(
    permission_check: &impl PermissionCheck,
    source_sid: SecurityId,
    target_sid: SecurityId,
) -> Result<(), Errno> {
    check_permissions(permission_check, source_sid, target_sid, &[ProcessPermission::SetPgid])
}

/// Checks if the task with `source_sid` is allowed to send `signal` to the task with `target_sid`.
pub(super) fn check_signal_access(
    permission_check: &impl PermissionCheck,
    source_sid: SecurityId,
    target_sid: SecurityId,
    signal: Signal,
) -> Result<(), Errno> {
    match signal {
        // The `sigkill` permission is required for sending SIGKILL.
        SIGKILL => check_permissions(
            permission_check,
            source_sid,
            target_sid,
            &[ProcessPermission::SigKill],
        ),
        // The `sigstop` permission is required for sending SIGSTOP.
        SIGSTOP => check_permissions(
            permission_check,
            source_sid,
            target_sid,
            &[ProcessPermission::SigStop],
        ),
        // The `sigchld` permission is required for sending SIGCHLD.
        SIGCHLD => check_permissions(
            permission_check,
            source_sid,
            target_sid,
            &[ProcessPermission::SigChld],
        ),
        // The `signal` permission is required for sending any signal other than SIGKILL, SIGSTOP
        // or SIGCHLD.
        _ => check_permissions(
            permission_check,
            source_sid,
            target_sid,
            &[ProcessPermission::Signal],
        ),
    }
}

/// Checks if the task with `source_sid` has the permission to get and/or set limits on the task with `target_sid`.
pub(super) fn task_prlimit(
    permission_check: &impl PermissionCheck,
    source_sid: SecurityId,
    target_sid: SecurityId,
    check_get_rlimit: bool,
    check_set_rlimit: bool,
) -> Result<(), Errno> {
    match (check_get_rlimit, check_set_rlimit) {
        (true, true) => check_permissions(
            permission_check,
            source_sid,
            target_sid,
            &[ProcessPermission::GetRlimit, ProcessPermission::SetRlimit],
        ),
        (true, false) => check_permissions(
            permission_check,
            source_sid,
            target_sid,
            &[ProcessPermission::GetRlimit],
        ),
        (false, true) => check_permissions(
            permission_check,
            source_sid,
            target_sid,
            &[ProcessPermission::SetRlimit],
        ),
        (false, false) => Ok(()),
    }
}

/// Checks if the task with `_source_sid` has the permission to mount at `_path` the object specified by
/// `_dev_name` of type `_fs_type`, with the mounting flags `_flags` and filesystem data `_data`.
pub(super) fn sb_mount(
    _permission_check: &impl PermissionCheck,
    _source_sid: SecurityId,
    _dev_name: &bstr::BStr,
    _path: &NamespaceNode,
    _fs_type: &bstr::BStr,
    _flags: MountFlags,
    _data: &bstr::BStr,
) -> Result<(), Errno> {
    track_stub!(TODO("https://fxbug.dev/352507622"), "sb_mount: validate permission");
    Ok(())
}

/// Checks if the task with `_source_sid` has the permission to unmount the filesystem mounted on
/// `_node` using the unmount flags `_flags`.
pub(super) fn sb_umount(
    _permission_check: &impl PermissionCheck,
    _source_sid: SecurityId,
    _node: &NamespaceNode,
    _flags: LookupFlags,
) -> Result<(), Errno> {
    track_stub!(TODO("https://fxbug.dev/353936182"), "sb_umount: validate permission");
    Ok(())
}

/// Checks if the task with `source_sid` is allowed to trace the task with `target_sid`.
pub(super) fn ptrace_access_check(
    permission_check: &impl PermissionCheck,
    tracer_sid: SecurityId,
    tracee_security_state: &mut TaskState,
) -> Result<(), Errno> {
    check_permissions(
        permission_check,
        tracer_sid,
        tracee_security_state.current_sid,
        &[ProcessPermission::Ptrace],
    )
    .and_then(|_| {
        // If tracing is allowed, set the `ptracer_sid` of the tracee with the tracer's SID.
        // TODO(b/352535794): Remove this, and rely on `Task::ptrace` instead.
        tracee_security_state.ptracer_sid = Some(tracer_sid);
        Ok(())
    })
}

/// Attempts to update the security ID (SID) associated with `fs_node` to the context encoded by
/// `security_selinux_xattr_value`.
pub(super) fn post_setxattr(
    security_server: &SecurityServer,
    fs_node: &FsNode,
    security_selinux_xattr_value: &FsStr,
) {
    match security_server.security_context_to_sid(security_selinux_xattr_value.into()) {
        // Update node SID value if a SID is found to be associated with new security context
        // string.
        Ok(sid) => set_cached_sid(fs_node, sid),
        // Clear any existing node SID if none is associated with new security context string.
        Err(_) => clear_cached_sid(fs_node),
    }
}

/// Returns the Security Context associated with the `name`ed entry for the specified `target` task.
/// `source` describes the calling task, `target` the state of the task for which to return the attribute.
pub fn get_procattr(
    security_server: &SecurityServer,
    _source: SecurityId,
    target: &TaskState,
    attr: ProcAttr,
) -> Result<Vec<u8>, Errno> {
    // TODO(b/322849067): Validate that the `source` has the required access.

    let sid = match attr {
        ProcAttr::Current => Some(target.current_sid),
        ProcAttr::Exec => target.exec_sid,
        ProcAttr::FsCreate => target.fscreate_sid,
        ProcAttr::KeyCreate => target.keycreate_sid,
        ProcAttr::Previous => Some(target.previous_sid),
        ProcAttr::SockCreate => target.sockcreate_sid,
    };

    // Convert it to a Security Context string.
    Ok(sid.and_then(|sid| security_server.sid_to_security_context(sid)).unwrap_or_default())
}

/// Sets the Security Context associated with the `attr` entry in the task security state.
pub fn set_procattr(
    security_server: &Arc<SecurityServer>,
    source: SecurityId,
    target: &mut TaskState,
    attr: ProcAttr,
    context: &[u8],
) -> Result<(), Errno> {
    let context = NullessByteStr::from(context);
    // Attempt to convert the Security Context string to a SID.
    let sid = match context.as_bytes() {
        b"\x0a" | b"" => None,
        _ => Some(security_server.security_context_to_sid(context).map_err(|_| errno!(EINVAL))?),
    };

    let check_permission = |permission| {
        // Proc/attr values may only be set for a task by the task itself,
        // and require that the task have the corresponding permissions.
        check_permissions(&security_server.as_permission_check(), source, source, &[permission])
    };
    match attr {
        ProcAttr::Current => {
            check_permission(ProcessPermission::SetCurrent)?;
            // TODO(b/322849067): Verify the `source` has `dyntransition` permission to the new SID.
            target.current_sid = sid.ok_or_else(|| errno!(EINVAL))?
        }
        ProcAttr::Previous => {
            return error!(EINVAL);
        }
        ProcAttr::Exec => {
            check_permission(ProcessPermission::SetExec)?;
            target.exec_sid = sid
        }
        ProcAttr::FsCreate => {
            check_permission(ProcessPermission::SetFsCreate)?;
            target.fscreate_sid = sid
        }
        ProcAttr::KeyCreate => {
            check_permission(ProcessPermission::SetKeyCreate)?;
            target.keycreate_sid = sid
        }
        ProcAttr::SockCreate => {
            check_permission(ProcessPermission::SetSockCreate)?;
            target.sockcreate_sid = sid
        }
    };

    Ok(())
}

/// Returns a security id that should be used for SELinux access control checks on `fs_node`. This
/// computation will attempt to load the security id associated with an extended attribute value. If
/// a meaningful security id cannot be determined, then the `unlabeled` security id is returned.
///
/// This `unlabeled` case includes situations such as:
///
/// 1. The `get_xattr("security.selinux")` computation fails to return a `context_string`;
/// 2. The subsequent `context_string => security_id` computation fails.
fn compute_fs_node_security_id(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    fs_node: &FsNode,
) -> SecurityId {
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
            match security_server.security_context_to_sid((&security_context).into()) {
                Ok(sid) => {
                    // Update node SID value if a SID is found to be associated with new security context
                    // string.
                    set_cached_sid(fs_node, sid);

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
}

/// Checks if `permissions` are allowed from the task with `source_sid` to the task with `target_sid`.
pub(super) fn check_permissions<P: ClassPermission + Into<Permission> + Clone + 'static>(
    permission_check: &impl PermissionCheck,
    source_sid: SecurityId,
    target_sid: SecurityId,
    permissions: &[P],
) -> Result<(), Errno> {
    match permission_check.has_permissions(source_sid, target_sid, permissions) {
        true => Ok(()),
        false => error!(EACCES),
    }
}

/// Returns `TaskState` for a new `Task`, based on the `parent` state, and the specified clone flags.
pub(super) fn task_alloc(parent: &TaskState, _clone_flags: u64) -> TaskState {
    parent.clone()
}

/// The SELinux security structure for `ThreadGroup`.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct TaskState {
    /// Current SID for the task.
    pub current_sid: SecurityId,

    /// SID for the task upon the next execve call.
    pub exec_sid: Option<SecurityId>,

    /// SID for files created by the task.
    pub fscreate_sid: Option<SecurityId>,

    /// SID for kernel-managed keys created by the task.
    pub keycreate_sid: Option<SecurityId>,

    /// SID prior to the last execve.
    pub previous_sid: SecurityId,

    /// SID for sockets created by the task.
    pub sockcreate_sid: Option<SecurityId>,

    /// SID of the tracer, if the thread group is traced.
    pub ptracer_sid: Option<SecurityId>,
}

impl TaskState {
    /// Returns initial state for kernel tasks.
    pub(super) fn for_kernel() -> Self {
        Self::for_initial_sid(InitialSid::Kernel)
    }

    /// Returns placeholder state for use when SELinux is not enabled.
    pub(super) fn for_selinux_disabled() -> Self {
        Self::for_initial_sid(InitialSid::Unlabeled)
    }

    fn for_initial_sid(initial_sid: InitialSid) -> Self {
        Self {
            current_sid: SecurityId::initial(initial_sid),
            previous_sid: SecurityId::initial(initial_sid),
            exec_sid: None,
            fscreate_sid: None,
            keycreate_sid: None,
            sockcreate_sid: None,
            ptracer_sid: None,
        }
    }
}

/// Returns the security id that should be used for SELinux access control checks against `fs_node`
/// at this time. If no security id is cached, it is recomputed via `compute_fs_node_security_id()`.
fn get_effective_fs_node_security_id(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    fs_node: &FsNode,
) -> SecurityId {
    // Note: the sid is read before the match statement because otherwise the lock in
    // `self.info()` would be held for the duration of the match statement, leading to a
    // deadlock with `compute_fs_node_security_id()`.
    let sid = fs_node.info().security_state.0;
    match sid {
        Some(sid) => sid,
        None => compute_fs_node_security_id(security_server, current_task, fs_node),
    }
}

/// Sets the cached security id associated with `fs_node` to `sid`. Storing the security id will
/// cause the security id to *not* be recomputed by the SELinux LSM when determining the effective
/// security id of this [`FsNode`].
fn set_cached_sid(fs_node: &FsNode, sid: SecurityId) {
    fs_node.update_info(|info| info.security_state = FsNodeState(Some(sid)));
}

/// Clears the cached security id on `fs_node`. Clearing the security id will cause the security id
/// to be be recomputed by the SELinux LSM when determining the effective security id of this
/// [`FsNode`].
fn clear_cached_sid(fs_node: &FsNode) {
    fs_node.update_info(|info| info.security_state.0 = None);
}

/// Other [`crate::security`] modules may use security id helpers for testing.
#[cfg(test)]
pub(super) mod testing {
    use crate::vfs::FsNode;
    use selinux::SecurityId;

    /// Returns the security id currently stored in `fs_node`, if any. This API should only be used
    /// by code that is responsible for controlling the cached security id; e.g., to check its
    /// current value before engaging logic that may compute a new value. Access control enforcement
    /// code should use `get_effective_fs_node_security_id()`, *not* this function.
    pub fn get_cached_sid(fs_node: &FsNode) -> Option<SecurityId> {
        fs_node.info().security_state.0
    }
}

#[cfg(test)]
mod tests {
    use super::testing::get_cached_sid;
    use super::*;
    use crate::testing::{create_kernel_task_and_unlocked_with_selinux, AutoReleasableTask};
    use crate::vfs::{NamespaceNode, XattrOp};
    use selinux::security_server::Mode;
    use starnix_sync::{Locked, Unlocked};
    use starnix_uapi::device_type::DeviceType;
    use starnix_uapi::file_mode::FileMode;
    use starnix_uapi::signals::SIGTERM;

    const VALID_SECURITY_CONTEXT: &[u8] = b"u:object_r:test_valid_t:s0";

    const HOOKS_TESTS_BINARY_POLICY: &[u8] =
        include_bytes!("../../../lib/selinux/testdata/micro_policies/hooks_tests_policy.pp");

    fn security_server_with_policy() -> Arc<SecurityServer> {
        let policy_bytes = HOOKS_TESTS_BINARY_POLICY.to_vec();
        let security_server = SecurityServer::new(Mode::Enable);
        security_server.set_enforcing(true);
        security_server.load_policy(policy_bytes).expect("policy load failed");
        security_server
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

    fn create_test_executable(
        locked: &mut Locked<'_, Unlocked>,
        current_task: &AutoReleasableTask,
        security_context: &[u8],
    ) -> NamespaceNode {
        let namespace_node = current_task
            .fs()
            .root()
            .create_node(
                locked,
                &current_task,
                "executable".into(),
                FileMode::IFREG,
                DeviceType::NONE,
            )
            .expect("create_node(file)");
        let fs_node = &namespace_node.entry.node;
        fs_node
            .ops()
            .set_xattr(
                fs_node,
                current_task,
                b"security.selinux".into(),
                security_context.into(),
                XattrOp::Set,
            )
            .expect("set security.selinux xattr");
        namespace_node
    }

    #[fuchsia::test]
    fn task_create_allowed_for_allowed_type() {
        let security_server = security_server_with_policy();
        let sid = security_server
            .security_context_to_sid(b"u:object_r:fork_yes_t:s0".into())
            .expect("invalid security context");

        assert_eq!(check_task_create_access(&security_server.as_permission_check(), sid), Ok(()));
    }

    #[fuchsia::test]
    fn task_create_denied_for_denied_type() {
        let security_server = security_server_with_policy();
        let sid = security_server
            .security_context_to_sid(b"u:object_r:fork_no_t:s0".into())
            .expect("invalid security context");

        assert_eq!(
            check_task_create_access(&security_server.as_permission_check(), sid),
            error!(EACCES)
        );
    }

    #[fuchsia::test]
    async fn exec_transition_allowed_for_allowed_transition_type() {
        let security_server = security_server_with_policy();
        security_server.set_enforcing(true);
        let (_kernel, current_task, mut locked) =
            create_kernel_task_and_unlocked_with_selinux(security_server.clone());
        let current_sid = security_server
            .security_context_to_sid(b"u:object_r:exec_transition_source_t:s0".into())
            .expect("invalid security context");
        let exec_sid = security_server
            .security_context_to_sid(b"u:object_r:exec_transition_target_t:s0".into())
            .expect("invalid security context");

        let executable_security_context = b"u:object_r:executable_file_trans_t:s0";
        assert!(security_server
            .security_context_to_sid(executable_security_context.into())
            .is_ok());
        let executable =
            create_test_executable(&mut locked, &current_task, executable_security_context);
        let executable_fs_node = &executable.entry.node;

        let security_state = TaskState {
            current_sid: current_sid,
            exec_sid: Some(exec_sid),
            fscreate_sid: None,
            keycreate_sid: None,
            previous_sid: current_sid,
            sockcreate_sid: None,
            ptracer_sid: None,
        };

        assert_eq!(
            check_exec_access(&security_server, &current_task, &security_state, executable_fs_node),
            Ok(Some(exec_sid))
        );
    }

    #[fuchsia::test]
    async fn exec_transition_denied_for_transition_denied_type() {
        let security_server = security_server_with_policy();
        security_server.set_enforcing(true);
        let (_kernel, current_task, mut locked) =
            create_kernel_task_and_unlocked_with_selinux(security_server.clone());
        let current_sid = security_server
            .security_context_to_sid(b"u:object_r:exec_transition_source_t:s0".into())
            .expect("invalid security context");
        let exec_sid = security_server
            .security_context_to_sid(b"u:object_r:exec_transition_denied_target_t:s0".into())
            .expect("invalid security context");

        let executable_security_context = b"u:object_r:executable_file_trans_t:s0";
        assert!(security_server
            .security_context_to_sid(executable_security_context.into())
            .is_ok());
        let executable =
            create_test_executable(&mut locked, &current_task, executable_security_context);
        let executable_fs_node = &executable.entry.node;

        let security_state = TaskState {
            current_sid: current_sid,
            exec_sid: Some(exec_sid),
            fscreate_sid: None,
            keycreate_sid: None,
            previous_sid: current_sid,
            sockcreate_sid: None,
            ptracer_sid: None,
        };

        assert_eq!(
            check_exec_access(&security_server, &current_task, &security_state, executable_fs_node),
            error!(EACCES)
        );
    }

    // TODO(http://b/330904217): reenable test once filesystems are labeled and access is denied.
    #[ignore]
    #[fuchsia::test]
    async fn exec_transition_denied_for_executable_with_no_entrypoint_perm() {
        let security_server = security_server_with_policy();
        security_server.set_enforcing(true);
        let (_kernel, current_task, mut locked) =
            create_kernel_task_and_unlocked_with_selinux(security_server.clone());
        let current_sid = security_server
            .security_context_to_sid(b"u:object_r:exec_transition_source_t:s0".into())
            .expect("invalid security context");
        let exec_sid = security_server
            .security_context_to_sid(b"u:object_r:exec_transition_target_t:s0".into())
            .expect("invalid security context");

        let executable_security_context = b"u:object_r:executable_file_trans_no_entrypoint_t:s0";
        assert!(security_server
            .security_context_to_sid(executable_security_context.into())
            .is_ok());
        let executable =
            create_test_executable(&mut locked, &current_task, executable_security_context);
        let executable_fs_node = &executable.entry.node;

        let security_state = TaskState {
            current_sid: current_sid,
            exec_sid: Some(exec_sid),
            fscreate_sid: None,
            keycreate_sid: None,
            previous_sid: current_sid,
            sockcreate_sid: None,
            ptracer_sid: None,
        };

        assert_eq!(
            check_exec_access(&security_server, &current_task, &security_state, executable_fs_node),
            error!(EACCES)
        );
    }

    #[fuchsia::test]
    async fn exec_no_trans_allowed_for_executable() {
        let security_server = security_server_with_policy();
        security_server.set_enforcing(true);
        let (_kernel, current_task, mut locked) =
            create_kernel_task_and_unlocked_with_selinux(security_server.clone());

        let current_sid = security_server
            .security_context_to_sid(b"u:object_r:exec_no_trans_source_t:s0".into())
            .expect("invalid security context");

        let executable_security_context = b"u:object_r:executable_file_no_trans_t:s0";
        assert!(security_server
            .security_context_to_sid(executable_security_context.into())
            .is_ok());
        let executable =
            create_test_executable(&mut locked, &current_task, executable_security_context);
        let executable_fs_node = &executable.entry.node;

        let security_state = TaskState {
            current_sid: current_sid,
            exec_sid: None,
            fscreate_sid: None,
            keycreate_sid: None,
            previous_sid: current_sid,
            sockcreate_sid: None,
            ptracer_sid: None,
        };

        assert_eq!(
            check_exec_access(&security_server, &current_task, &security_state, executable_fs_node),
            Ok(Some(current_sid))
        );
    }

    // TODO(http://b/330904217): reenable test once filesystems are labeled and access is denied.
    #[ignore]
    #[fuchsia::test]
    async fn exec_no_trans_denied_for_executable() {
        let security_server = security_server_with_policy();
        security_server.set_enforcing(true);
        let (_kernel, current_task, mut locked) =
            create_kernel_task_and_unlocked_with_selinux(security_server.clone());
        let current_sid = security_server
            .security_context_to_sid(b"u:object_r:exec_transition_target_t:s0".into())
            .expect("invalid security context");

        let executable_security_context = b"u:object_r:executable_file_no_trans_t:s0";
        assert!(security_server
            .security_context_to_sid(executable_security_context.into())
            .is_ok());
        let executable =
            create_test_executable(&mut locked, &current_task, executable_security_context);
        let executable_fs_node = &executable.entry.node;

        let security_state = TaskState {
            current_sid: current_sid,
            exec_sid: None,
            fscreate_sid: None,
            keycreate_sid: None,
            previous_sid: current_sid,
            sockcreate_sid: None,
            ptracer_sid: None,
        };

        // There is no `execute_no_trans` allow statement from `current_sid` to `executable_sid`,
        // expect access denied.
        assert_eq!(
            check_exec_access(&security_server, &current_task, &security_state, executable_fs_node),
            error!(EACCES)
        );
    }

    #[fuchsia::test]
    fn no_state_update_if_no_elf_state() {
        let initial_state = TaskState::for_kernel();
        let mut security_state = initial_state.clone();
        update_state_on_exec(&mut security_state, None);
        assert_eq!(security_state, initial_state);
    }

    #[fuchsia::test]
    fn state_is_updated_on_exec() {
        let security_server = security_server_with_policy();
        let initial_state = TaskState::for_kernel();
        let mut security_state = initial_state.clone();

        let elf_sid = security_server
            .security_context_to_sid(b"u:object_r:test_valid_t:s0".into())
            .expect("invalid security context");
        assert_ne!(elf_sid, initial_state.current_sid);
        update_state_on_exec(&mut security_state, Some(elf_sid));
        assert_eq!(
            security_state,
            TaskState {
                current_sid: elf_sid,
                exec_sid: initial_state.exec_sid,
                fscreate_sid: initial_state.fscreate_sid,
                keycreate_sid: initial_state.keycreate_sid,
                previous_sid: initial_state.previous_sid,
                sockcreate_sid: initial_state.sockcreate_sid,
                ptracer_sid: initial_state.ptracer_sid,
            }
        );
    }

    #[fuchsia::test]
    fn getsched_access_allowed_for_allowed_type() {
        let security_server = security_server_with_policy();
        let source_sid = security_server
            .security_context_to_sid(b"u:object_r:test_getsched_yes_t:s0".into())
            .expect("invalid security context");
        let target_sid = security_server
            .security_context_to_sid(b"u:object_r:test_getsched_target_t:s0".into())
            .expect("invalid security context");

        assert_eq!(
            check_getsched_access(&security_server.as_permission_check(), source_sid, target_sid),
            Ok(())
        );
    }

    #[fuchsia::test]
    fn getsched_access_denied_for_denied_type() {
        let security_server = security_server_with_policy();
        let source_sid = security_server
            .security_context_to_sid(b"u:object_r:test_getsched_no_t:s0".into())
            .expect("invalid security context");
        let target_sid = security_server
            .security_context_to_sid(b"u:object_r:test_getsched_target_t:s0".into())
            .expect("invalid security context");

        assert_eq!(
            check_getsched_access(&security_server.as_permission_check(), source_sid, target_sid),
            error!(EACCES)
        );
    }

    #[fuchsia::test]
    fn setsched_access_allowed_for_allowed_type() {
        let security_server = security_server_with_policy();
        let source_sid = security_server
            .security_context_to_sid(b"u:object_r:test_setsched_yes_t:s0".into())
            .expect("invalid security context");
        let target_sid = security_server
            .security_context_to_sid(b"u:object_r:test_setsched_target_t:s0".into())
            .expect("invalid security context");

        assert_eq!(
            check_setsched_access(&security_server.as_permission_check(), source_sid, target_sid),
            Ok(())
        );
    }

    #[fuchsia::test]
    fn setsched_access_denied_for_denied_type() {
        let security_server = security_server_with_policy();
        let source_sid = security_server
            .security_context_to_sid(b"u:object_r:test_setsched_no_t:s0".into())
            .expect("invalid security context");
        let target_sid = security_server
            .security_context_to_sid(b"u:object_r:test_setsched_target_t:s0".into())
            .expect("invalid security context");

        assert_eq!(
            check_setsched_access(&security_server.as_permission_check(), source_sid, target_sid),
            error!(EACCES)
        );
    }

    #[fuchsia::test]
    fn getpgid_access_allowed_for_allowed_type() {
        let security_server = security_server_with_policy();
        let source_sid = security_server
            .security_context_to_sid(b"u:object_r:test_getpgid_yes_t:s0".into())
            .expect("invalid security context");
        let target_sid = security_server
            .security_context_to_sid(b"u:object_r:test_getpgid_target_t:s0".into())
            .expect("invalid security context");

        assert_eq!(
            check_getpgid_access(&security_server.as_permission_check(), source_sid, target_sid),
            Ok(())
        );
    }

    #[fuchsia::test]
    fn getpgid_access_denied_for_denied_type() {
        let security_server = security_server_with_policy();
        let source_sid = security_server
            .security_context_to_sid(b"u:object_r:test_getpgid_no_t:s0".into())
            .expect("invalid security context");
        let target_sid = security_server
            .security_context_to_sid(b"u:object_r:test_getpgid_target_t:s0".into())
            .expect("invalid security context");

        assert_eq!(
            check_getpgid_access(&security_server.as_permission_check(), source_sid, target_sid),
            error!(EACCES)
        );
    }

    #[fuchsia::test]
    fn sigkill_access_allowed_for_allowed_type() {
        let security_server = security_server_with_policy();
        let source_sid = security_server
            .security_context_to_sid(b"u:object_r:test_kill_sigkill_t:s0".into())
            .expect("invalid security context");
        let target_sid = security_server
            .security_context_to_sid(b"u:object_r:test_kill_target_t:s0".into())
            .expect("invalid security context");

        assert_eq!(
            check_signal_access(
                &security_server.as_permission_check(),
                source_sid,
                target_sid,
                SIGKILL,
            ),
            Ok(())
        );
    }

    #[fuchsia::test]
    fn sigchld_access_allowed_for_allowed_type() {
        let security_server = security_server_with_policy();
        let source_sid = security_server
            .security_context_to_sid(b"u:object_r:test_kill_sigchld_t:s0".into())
            .expect("invalid security context");
        let target_sid = security_server
            .security_context_to_sid(b"u:object_r:test_kill_target_t:s0".into())
            .expect("invalid security context");

        assert_eq!(
            check_signal_access(
                &security_server.as_permission_check(),
                source_sid,
                target_sid,
                SIGCHLD,
            ),
            Ok(())
        );
    }

    #[fuchsia::test]
    fn sigstop_access_allowed_for_allowed_type() {
        let security_server = security_server_with_policy();
        let source_sid = security_server
            .security_context_to_sid(b"u:object_r:test_kill_sigstop_t:s0".into())
            .expect("invalid security context");
        let target_sid = security_server
            .security_context_to_sid(b"u:object_r:test_kill_target_t:s0".into())
            .expect("invalid security context");

        assert_eq!(
            check_signal_access(
                &security_server.as_permission_check(),
                source_sid,
                target_sid,
                SIGSTOP,
            ),
            Ok(())
        );
    }

    #[fuchsia::test]
    fn signal_access_allowed_for_allowed_type() {
        let security_server = security_server_with_policy();
        let source_sid = security_server
            .security_context_to_sid(b"u:object_r:test_kill_signal_t:s0".into())
            .expect("invalid security context");
        let target_sid = security_server
            .security_context_to_sid(b"u:object_r:test_kill_target_t:s0".into())
            .expect("invalid security context");

        // The `signal` permission allows signals other than SIGKILL, SIGCHLD, SIGSTOP.
        assert_eq!(
            check_signal_access(
                &security_server.as_permission_check(),
                source_sid,
                target_sid,
                SIGTERM,
            ),
            Ok(())
        );
    }

    #[fuchsia::test]
    fn signal_access_denied_for_denied_signals() {
        let security_server = security_server_with_policy();
        let source_sid = security_server
            .security_context_to_sid(b"u:object_r:test_kill_signal_t:s0".into())
            .expect("invalid security context");
        let target_sid = security_server
            .security_context_to_sid(b"u:object_r:test_kill_target_t:s0".into())
            .expect("invalid security context");

        // The `signal` permission does not allow SIGKILL, SIGCHLD or SIGSTOP.
        for signal in [SIGCHLD, SIGKILL, SIGSTOP] {
            assert_eq!(
                check_signal_access(
                    &security_server.as_permission_check(),
                    source_sid,
                    target_sid,
                    signal,
                ),
                error!(EACCES)
            );
        }
    }

    #[fuchsia::test]
    fn ptrace_access_allowed_for_allowed_type_and_state_is_updated() {
        let security_server = security_server_with_policy();
        let tracer_sid = security_server
            .security_context_to_sid(b"u:object_r:test_ptrace_tracer_yes_t:s0".into())
            .expect("invalid security context");
        let tracee_sid = security_server
            .security_context_to_sid(b"u:object_r:test_ptrace_traced_t:s0".into())
            .expect("invalid security context");
        let initial_state = TaskState {
            current_sid: tracee_sid,
            exec_sid: None,
            fscreate_sid: None,
            keycreate_sid: None,
            previous_sid: tracee_sid,
            sockcreate_sid: None,
            ptracer_sid: None,
        };
        let mut tracee_state = initial_state.clone();

        assert_eq!(
            ptrace_access_check(
                &security_server.as_permission_check(),
                tracer_sid,
                &mut tracee_state
            ),
            Ok(())
        );
        assert_eq!(
            tracee_state,
            TaskState {
                current_sid: initial_state.current_sid,
                exec_sid: initial_state.exec_sid,
                fscreate_sid: initial_state.fscreate_sid,
                keycreate_sid: initial_state.keycreate_sid,
                previous_sid: initial_state.previous_sid,
                sockcreate_sid: initial_state.sockcreate_sid,
                ptracer_sid: Some(tracer_sid),
            }
        );
    }

    #[fuchsia::test]
    fn ptrace_access_denied_for_denied_type_and_state_is_not_updated() {
        let security_server = security_server_with_policy();
        let tracer_sid = security_server
            .security_context_to_sid(b"u:object_r:test_ptrace_tracer_no_t:s0".into())
            .expect("invalid security context");
        let tracee_sid = security_server
            .security_context_to_sid(b"u:object_r:test_ptrace_traced_t:s0".into())
            .expect("invalid security context");
        let initial_state = TaskState {
            current_sid: tracee_sid,
            exec_sid: None,
            fscreate_sid: None,
            keycreate_sid: None,
            previous_sid: tracee_sid,
            sockcreate_sid: None,
            ptracer_sid: None,
        };
        let mut tracee_state = initial_state.clone();

        assert_eq!(
            ptrace_access_check(
                &security_server.as_permission_check(),
                tracer_sid,
                &mut tracee_state
            ),
            error!(EACCES)
        );
        assert_eq!(initial_state, tracee_state);
    }

    #[fuchsia::test]
    fn task_alloc_from_parent() {
        // Create a fake parent state, with values for some fields, to check for.
        let parent_security_state = TaskState {
            current_sid: SecurityId::initial(InitialSid::Unlabeled),
            previous_sid: SecurityId::initial(InitialSid::Kernel),
            exec_sid: Some(SecurityId::initial(InitialSid::Unlabeled)),
            fscreate_sid: Some(SecurityId::initial(InitialSid::Unlabeled)),
            keycreate_sid: Some(SecurityId::initial(InitialSid::Unlabeled)),
            sockcreate_sid: Some(SecurityId::initial(InitialSid::Unlabeled)),
            ptracer_sid: None,
        };

        let security_state = task_alloc(&parent_security_state, 0);
        assert_eq!(security_state, parent_security_state);
    }

    #[fuchsia::test]
    fn task_alloc_for() {
        let for_kernel = TaskState::for_kernel();
        assert_eq!(for_kernel.current_sid, SecurityId::initial(InitialSid::Kernel));
        assert_eq!(for_kernel.previous_sid, for_kernel.current_sid);
        assert_eq!(for_kernel.exec_sid, None);
        assert_eq!(for_kernel.fscreate_sid, None);
        assert_eq!(for_kernel.keycreate_sid, None);
        assert_eq!(for_kernel.sockcreate_sid, None);
        assert_eq!(for_kernel.ptracer_sid, None);
    }

    #[fuchsia::test]
    async fn compute_fs_node_security_id_missing_xattr_unlabeled() {
        let security_server = security_server_with_policy();
        security_server.set_enforcing(true);
        let (_kernel, current_task, mut locked) =
            create_kernel_task_and_unlocked_with_selinux(security_server.clone());
        let node = &create_test_file(&mut locked, &current_task).entry.node;
        assert_eq!(None, get_cached_sid(node));

        assert_eq!(
            SecurityId::initial(InitialSid::Unlabeled),
            compute_fs_node_security_id(&security_server, &current_task, node)
        );
        assert_eq!(None, get_cached_sid(node));
    }

    #[fuchsia::test]
    async fn compute_fs_node_security_id_invalid_xattr_unlabeled() {
        let security_server = security_server_with_policy();
        security_server.set_enforcing(true);
        let (_kernel, current_task, mut locked) =
            create_kernel_task_and_unlocked_with_selinux(security_server.clone());
        let node = &create_test_file(&mut locked, &current_task).entry.node;
        node.ops()
            .set_xattr(node, &current_task, "security.selinux".into(), "".into(), XattrOp::Set)
            .expect("setxattr");
        assert_eq!(None, get_cached_sid(node));

        assert_eq!(
            SecurityId::initial(InitialSid::Unlabeled),
            compute_fs_node_security_id(&security_server, &current_task, node)
        );
        assert_eq!(None, get_cached_sid(node));
    }

    #[fuchsia::test]
    async fn compute_fs_node_security_id_valid_xattr_stored() {
        let security_server = security_server_with_policy();
        security_server.set_enforcing(true);
        let (_kernel, current_task, mut locked) =
            create_kernel_task_and_unlocked_with_selinux(security_server.clone());
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
        assert_eq!(None, get_cached_sid(node));

        let security_id = compute_fs_node_security_id(&security_server, &current_task, node);
        assert_eq!(Some(security_id), get_cached_sid(node));
    }

    #[fuchsia::test]
    async fn setxattr_set_sid() {
        let security_server = security_server_with_policy();
        let (_kernel, current_task, mut locked) =
            create_kernel_task_and_unlocked_with_selinux(security_server);
        let node = &create_test_file(&mut locked, &current_task).entry.node;
        assert_eq!(None, get_cached_sid(node));

        node.set_xattr(
            current_task.as_ref(),
            &current_task.fs().root().mount,
            "security.selinux".into(),
            VALID_SECURITY_CONTEXT.into(),
            XattrOp::Set,
        )
        .expect("setxattr");

        assert!(get_cached_sid(node).is_some());
    }
}
