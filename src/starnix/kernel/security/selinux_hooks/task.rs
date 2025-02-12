// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::security::selinux_hooks::{
    check_permission, check_self_permission, fs_node_effective_sid_and_class, fs_node_ensure_class,
    fs_node_set_label_with_task, has_file_permissions, Permission, PermissionCheck,
    ProcessPermission, TaskAttrs,
};
use crate::security::{Arc, ProcAttr, ResolvedElfState, SecurityId, SecurityServer};
use crate::task::{CurrentTask, Task};
use crate::vfs::FsNode;
use selinux::{
    Cap2Class, CapClass, CommonCap2Permission, CommonCapPermission, FilePermission, NullessByteStr,
    ObjectClass,
};
use starnix_types::ownership::TempRef;
use starnix_uapi::errors::Errno;
use starnix_uapi::resource_limits::Resource;
use starnix_uapi::signals::{Signal, SIGCHLD, SIGKILL, SIGSTOP};
use starnix_uapi::{errno, error, rlimit};

/// Updates the SELinux thread group state on exec, using the security ID associated with the
/// resolved elf.
pub fn update_state_on_exec(
    security_server: &Arc<SecurityServer>,
    current_task: &CurrentTask,
    elf_security_state: &ResolvedElfState,
) {
    let (new_sid, old_sid) = {
        let task_attrs = &mut current_task.security_state.lock();
        let previous_sid = task_attrs.current_sid;

        **task_attrs = TaskAttrs {
            current_sid: elf_security_state
                .sid
                .expect("SELinux enabled but missing resolved elf state"),
            previous_sid,
            exec_sid: None,
            fscreate_sid: None,
            keycreate_sid: None,
            sockcreate_sid: None,
        };
        (task_attrs.current_sid, previous_sid)
    };
    if new_sid == old_sid {
        return;
    }
    // Do the work of the `selinux_bprm_post_apply_creds` hook:
    // 1. Revoke access to any file descriptors that `current_task` is not permitted to access.
    // 2. Reset resource limits if `current_task` is not permitted to inherit rlimits.
    // 3. TODO(https://fxbug.dev/378655436): Reset signal state if `current task` is not
    //    permitted to inherit the parent task's signal state.
    // 4. TODO(https://fxbug.dev/331815418): Wake the parent task if waiting on `current_task`.
    close_inaccessible_file_descriptors(security_server, current_task, new_sid);
    maybe_reset_rlimits(security_server, current_task, old_sid, new_sid);
}

/// "Closes" file descriptors that `current_task` does not have permission to access by remapping
/// those file descriptors to the null file in selinuxfs.
fn close_inaccessible_file_descriptors(
    security_server: &Arc<SecurityServer>,
    current_task: &CurrentTask,
    new_sid: SecurityId,
) {
    let kernel_state = current_task
        .kernel()
        .security_state
        .state
        .as_ref()
        .expect("kernel has security state because SELinux is enabled");

    let null_file_handle =
        kernel_state.selinuxfs_null.get().expect("selinuxfs_init_null() has been called").clone();

    let source_sid = new_sid;
    let permission_check = security_server.as_permission_check();
    // Remap-to-null any fds that failed a check for allowing
    // `[child-process] [fd-from-child-fd-table]:fd { use }`.
    current_task.files.remap_fds(|file| {
        let fd_use_result = has_file_permissions(&permission_check, source_sid, file, &[]);
        fd_use_result.map_or_else(|_| Some(null_file_handle.clone()), |_| None)
    });
}

/// Checks the `rlimitinh` permission for the current task. If the permission is denied, resets
/// the current task's resource limits.
fn maybe_reset_rlimits(
    security_server: &Arc<SecurityServer>,
    current_task: &CurrentTask,
    old_sid: SecurityId,
    new_sid: SecurityId,
) {
    let permission_check = security_server.as_permission_check();
    if check_permission(&permission_check, old_sid, new_sid, ProcessPermission::RlimitInh).is_ok() {
        // Allow the resource limit inheritance that was applied when the current
        // task was created.
        return;
    }
    // Compute the new soft resource limits for the current task.
    // For each resource, the new soft limit is the minimum of the current task's hard limit
    // and the initial task's soft limit.
    let weak_init = current_task.kernel().pids.read().get_task(1);
    let init_task = weak_init.upgrade().expect("get the initial task");
    let init_rlimits = { init_task.thread_group.limits.lock().clone() };
    let mut current_rlimits = current_task.thread_group.limits.lock();
    (Resource::ALL).iter().for_each(|resource| {
        let current = current_rlimits.get(*resource);
        let init = init_rlimits.get(*resource);
        current_rlimits.set(
            *resource,
            rlimit {
                rlim_cur: std::cmp::min(init.rlim_cur, current.rlim_max),
                rlim_max: current.rlim_max,
            },
        )
    });
}

/// Returns `TaskAttrs` for a new `Task`, based on the `parent` state, and the specified clone flags.
pub fn task_alloc(parent: &Task, _clone_flags: u64) -> TaskAttrs {
    parent.security_state.lock().clone()
}

/// Checks if creating a task is allowed.
pub fn check_task_create_access(
    permission_check: &PermissionCheck<'_>,
    current_task: &CurrentTask,
) -> Result<(), Errno> {
    let task_sid = current_task.security_state.lock().current_sid;
    check_self_permission(permission_check, task_sid, ProcessPermission::Fork)
}

/// Checks the SELinux permissions required for exec. Returns the SELinux state of a resolved
/// elf if all required permissions are allowed.
pub fn check_exec_access(
    security_server: &Arc<SecurityServer>,
    current_task: &CurrentTask,
    executable_node: &FsNode,
) -> Result<ResolvedElfState, Errno> {
    let (current_sid, exec_sid) = {
        let state = &current_task.security_state.lock();
        (state.current_sid, state.exec_sid)
    };

    let executable_sid = fs_node_effective_sid_and_class(executable_node).sid;

    let new_sid = if let Some(exec_sid) = exec_sid {
        // Use the proc exec SID if set.
        exec_sid
    } else {
        security_server
            .compute_new_sid(current_sid, executable_sid, ObjectClass::Process)
            .map_err(|_| errno!(EACCES))?
    };
    let permission_check = security_server.as_permission_check();
    if current_sid == new_sid {
        // To `exec()` a binary in the caller's domain, the caller must be granted
        // "execute_no_trans" permission to the binary.
        check_permission(
            &permission_check,
            current_sid,
            executable_sid,
            FilePermission::ExecuteNoTrans,
        )?;
    } else {
        // Check that the domain transition is allowed.
        check_permission(&permission_check, current_sid, new_sid, ProcessPermission::Transition)?;

        // Check that the executable file has an entry point into the new domain.
        check_permission(&permission_check, new_sid, executable_sid, FilePermission::Entrypoint)?;

        // Check that ptrace permission is allowed if the process is traced.
        if let Some(ptracer) = current_task.ptracer_task().upgrade() {
            let tracer_sid = ptracer.security_state.lock().current_sid;
            check_permission(&permission_check, tracer_sid, new_sid, ProcessPermission::Ptrace)?;
        }
    }
    Ok(ResolvedElfState { sid: Some(new_sid) })
}

/// Checks if source with `source_sid` may exercise the "getsched" permission on target with
/// `target_sid` according to SELinux server status `status` and permission checker
/// `permission`.
pub fn check_getsched_access(
    permission_check: &PermissionCheck<'_>,
    current_task: &CurrentTask,
    target: &Task,
) -> Result<(), Errno> {
    let source_sid = current_task.security_state.lock().current_sid;
    let target_sid = target.security_state.lock().current_sid;
    check_permission(permission_check, source_sid, target_sid, ProcessPermission::GetSched)
}

/// Checks if the task with `source_sid` is allowed to set scheduling parameters for the task with
/// `target_sid`.
pub fn check_setsched_access(
    permission_check: &PermissionCheck<'_>,
    current_task: &CurrentTask,
    target: &Task,
) -> Result<(), Errno> {
    let source_sid = current_task.security_state.lock().current_sid;
    let target_sid = target.security_state.lock().current_sid;
    check_permission(permission_check, source_sid, target_sid, ProcessPermission::SetSched)
}

/// Checks if the task with `source_sid` is allowed to get the process group ID of the task with
/// `target_sid`.
pub fn check_getpgid_access(
    permission_check: &PermissionCheck<'_>,
    current_task: &CurrentTask,
    target: &Task,
) -> Result<(), Errno> {
    let source_sid = current_task.security_state.lock().current_sid;
    let target_sid = target.security_state.lock().current_sid;
    check_permission(permission_check, source_sid, target_sid, ProcessPermission::GetPgid)
}

/// Checks if the task with `source_sid` is allowed to set the process group ID of the task with
/// `target_sid`.
pub fn check_setpgid_access(
    permission_check: &PermissionCheck<'_>,
    current_task: &CurrentTask,
    target: &Task,
) -> Result<(), Errno> {
    let source_sid = current_task.security_state.lock().current_sid;
    let target_sid = target.security_state.lock().current_sid;
    check_permission(permission_check, source_sid, target_sid, ProcessPermission::SetPgid)
}

/// Checks if the task with `source_sid` has permission to read the session Id from a task with `target_sid`.
/// Corresponds to the `task_getsid` LSM hook.
pub fn check_task_getsid(
    permission_check: &PermissionCheck<'_>,
    current_task: &CurrentTask,
    target: &Task,
) -> Result<(), Errno> {
    let source_sid = current_task.security_state.lock().current_sid;
    let target_sid = target.security_state.lock().current_sid;
    check_permission(permission_check, source_sid, target_sid, ProcessPermission::GetSession)
}

/// Checks if the task with `source_sid` is allowed to send `signal` to the task with `target_sid`.
pub fn check_signal_access(
    permission_check: &PermissionCheck<'_>,
    current_task: &CurrentTask,
    target: &Task,
    signal: Signal,
) -> Result<(), Errno> {
    let source_sid = current_task.security_state.lock().current_sid;
    let target_sid = target.security_state.lock().current_sid;
    match signal {
        // The `sigkill` permission is required for sending SIGKILL.
        SIGKILL => {
            check_permission(permission_check, source_sid, target_sid, ProcessPermission::SigKill)
        }
        // The `sigstop` permission is required for sending SIGSTOP.
        SIGSTOP => {
            check_permission(permission_check, source_sid, target_sid, ProcessPermission::SigStop)
        }
        // The `sigchld` permission is required for sending SIGCHLD.
        SIGCHLD => {
            check_permission(permission_check, source_sid, target_sid, ProcessPermission::SigChld)
        }
        // The `signal` permission is required for sending any signal other than SIGKILL, SIGSTOP
        // or SIGCHLD.
        _ => check_permission(permission_check, source_sid, target_sid, ProcessPermission::Signal),
    }
}

/// Returns the serialized Security Context associated with the specified task.
pub fn task_get_context(
    security_server: &SecurityServer,
    _current_task: &CurrentTask,
    target: &Task,
) -> Result<Vec<u8>, Errno> {
    let sid = target.security_state.lock().current_sid;
    Ok(security_server.sid_to_security_context(sid).unwrap_or_default())
}

fn permission_from_capability(capabilities: starnix_uapi::auth::Capabilities) -> Permission {
    // TODO: https://fxbug.dev/297313673 - CapClass::CapUserns will play a role here if-and-after
    // user namespaces are implemented in Starnix.
    match capabilities {
        starnix_uapi::auth::CAP_AUDIT_CONTROL => {
            CommonCapPermission::AuditControl.for_class(CapClass::Capability)
        }
        starnix_uapi::auth::CAP_AUDIT_WRITE => {
            CommonCapPermission::AuditWrite.for_class(CapClass::Capability)
        }
        starnix_uapi::auth::CAP_CHOWN => CommonCapPermission::Chown.for_class(CapClass::Capability),
        starnix_uapi::auth::CAP_DAC_OVERRIDE => {
            CommonCapPermission::DacOverride.for_class(CapClass::Capability)
        }
        starnix_uapi::auth::CAP_DAC_READ_SEARCH => {
            CommonCapPermission::DacReadSearch.for_class(CapClass::Capability)
        }
        starnix_uapi::auth::CAP_FOWNER => {
            CommonCapPermission::Fowner.for_class(CapClass::Capability)
        }
        starnix_uapi::auth::CAP_FSETID => {
            CommonCapPermission::Fsetid.for_class(CapClass::Capability)
        }
        starnix_uapi::auth::CAP_IPC_LOCK => {
            CommonCapPermission::IpcLock.for_class(CapClass::Capability)
        }
        starnix_uapi::auth::CAP_IPC_OWNER => {
            CommonCapPermission::IpcOwner.for_class(CapClass::Capability)
        }
        starnix_uapi::auth::CAP_KILL => CommonCapPermission::Kill.for_class(CapClass::Capability),
        starnix_uapi::auth::CAP_LEASE => CommonCapPermission::Lease.for_class(CapClass::Capability),
        starnix_uapi::auth::CAP_LINUX_IMMUTABLE => {
            CommonCapPermission::LinuxImmutable.for_class(CapClass::Capability)
        }
        starnix_uapi::auth::CAP_MKNOD => CommonCapPermission::Mknod.for_class(CapClass::Capability),
        starnix_uapi::auth::CAP_NET_ADMIN => {
            CommonCapPermission::NetAdmin.for_class(CapClass::Capability)
        }
        starnix_uapi::auth::CAP_NET_BIND_SERVICE => {
            CommonCapPermission::NetBindService.for_class(CapClass::Capability)
        }
        starnix_uapi::auth::CAP_NET_BROADCAST => {
            CommonCapPermission::NetBroadcast.for_class(CapClass::Capability)
        }
        starnix_uapi::auth::CAP_NET_RAW => {
            CommonCapPermission::NetRaw.for_class(CapClass::Capability)
        }
        starnix_uapi::auth::CAP_SETFCAP => {
            CommonCapPermission::Setfcap.for_class(CapClass::Capability)
        }
        starnix_uapi::auth::CAP_SETGID => {
            CommonCapPermission::Setgid.for_class(CapClass::Capability)
        }
        starnix_uapi::auth::CAP_SETPCAP => {
            CommonCapPermission::Setpcap.for_class(CapClass::Capability)
        }
        starnix_uapi::auth::CAP_SETUID => {
            CommonCapPermission::Setuid.for_class(CapClass::Capability)
        }
        starnix_uapi::auth::CAP_SYS_ADMIN => {
            CommonCapPermission::SysAdmin.for_class(CapClass::Capability)
        }
        starnix_uapi::auth::CAP_SYS_BOOT => {
            CommonCapPermission::SysBoot.for_class(CapClass::Capability)
        }
        starnix_uapi::auth::CAP_SYS_CHROOT => {
            CommonCapPermission::SysChroot.for_class(CapClass::Capability)
        }
        starnix_uapi::auth::CAP_SYS_MODULE => {
            CommonCapPermission::SysModule.for_class(CapClass::Capability)
        }
        starnix_uapi::auth::CAP_SYS_NICE => {
            CommonCapPermission::SysNice.for_class(CapClass::Capability)
        }
        starnix_uapi::auth::CAP_SYS_PACCT => {
            CommonCapPermission::SysPacct.for_class(CapClass::Capability)
        }
        starnix_uapi::auth::CAP_SYS_PTRACE => {
            CommonCapPermission::SysPtrace.for_class(CapClass::Capability)
        }
        starnix_uapi::auth::CAP_SYS_RAWIO => {
            CommonCapPermission::SysRawio.for_class(CapClass::Capability)
        }
        starnix_uapi::auth::CAP_SYS_RESOURCE => {
            CommonCapPermission::SysResource.for_class(CapClass::Capability)
        }
        starnix_uapi::auth::CAP_SYS_TIME => {
            CommonCapPermission::SysTime.for_class(CapClass::Capability)
        }
        starnix_uapi::auth::CAP_SYS_TTY_CONFIG => {
            CommonCapPermission::SysTtyConfig.for_class(CapClass::Capability)
        }
        starnix_uapi::auth::CAP_AUDIT_READ => {
            CommonCap2Permission::AuditRead.for_class(Cap2Class::Capability2)
        }
        starnix_uapi::auth::CAP_BLOCK_SUSPEND => {
            CommonCap2Permission::BlockSuspend.for_class(Cap2Class::Capability2)
        }
        starnix_uapi::auth::CAP_MAC_ADMIN => {
            CommonCap2Permission::MacAdmin.for_class(Cap2Class::Capability2)
        }
        starnix_uapi::auth::CAP_MAC_OVERRIDE => {
            CommonCap2Permission::MacOverride.for_class(Cap2Class::Capability2)
        }
        starnix_uapi::auth::CAP_SYSLOG => {
            CommonCap2Permission::Syslog.for_class(Cap2Class::Capability2)
        }
        starnix_uapi::auth::CAP_WAKE_ALARM => {
            CommonCap2Permission::WakeAlarm.for_class(Cap2Class::Capability2)
        }
        _ => {
            panic!("Unrecognized capabilities \"{:?}\" passed to check_capable!", capabilities)
        }
    }
}

pub fn check_task_capable(
    permission_check: &PermissionCheck<'_>,
    current_task: &CurrentTask,
    capabilities: starnix_uapi::auth::Capabilities,
) -> Result<(), Errno> {
    let sid = current_task.security_state.lock().current_sid;
    let permission = permission_from_capability(capabilities);
    check_self_permission(&permission_check, sid, permission)
}

/// Checks if the task with `source_sid` has the permission to get and/or set limits on the task with `target_sid`.
pub fn task_prlimit(
    permission_check: &PermissionCheck<'_>,
    current_task: &CurrentTask,
    target: &Task,
    check_get_rlimit: bool,
    check_set_rlimit: bool,
) -> Result<(), Errno> {
    let source_sid = current_task.security_state.lock().current_sid;
    let target_sid = target.security_state.lock().current_sid;
    if check_get_rlimit {
        check_permission(permission_check, source_sid, target_sid, ProcessPermission::GetRlimit)?;
    }
    if check_set_rlimit {
        check_permission(permission_check, source_sid, target_sid, ProcessPermission::SetRlimit)?;
    }
    Ok(())
}

/// Checks if the task with `source_sid` is allowed to trace the task with `target_sid`.
pub fn ptrace_access_check(
    permission_check: &PermissionCheck<'_>,
    current_task: &CurrentTask,
    tracee: &Task,
) -> Result<(), Errno> {
    let tracer_sid = current_task.security_state.lock().current_sid;
    let tracee_sid = tracee.security_state.lock().current_sid;
    check_permission(permission_check, tracer_sid, tracee_sid, ProcessPermission::Ptrace)
}

/// Returns the Security Context associated with the `name`ed entry for the specified `target` task.
/// `source` describes the calling task, `target` the state of the task for which to return the attribute.
pub fn get_procattr(
    security_server: &SecurityServer,
    _current_task: &CurrentTask,
    task: &Task,
    attr: ProcAttr,
) -> Result<Vec<u8>, Errno> {
    let task_attrs = &task.security_state.lock();

    let sid = match attr {
        ProcAttr::Current => Some(task_attrs.current_sid),
        ProcAttr::Exec => task_attrs.exec_sid,
        ProcAttr::FsCreate => task_attrs.fscreate_sid,
        ProcAttr::KeyCreate => task_attrs.keycreate_sid,
        ProcAttr::Previous => Some(task_attrs.previous_sid),
        ProcAttr::SockCreate => task_attrs.sockcreate_sid,
    };

    // Convert it to a Security Context string.
    Ok(sid.and_then(|sid| security_server.sid_to_security_context(sid)).unwrap_or_default())
}

/// Sets the Security Context associated with the `attr` entry in the task security state.
pub fn set_procattr(
    security_server: &Arc<SecurityServer>,
    current_task: &CurrentTask,
    attr: ProcAttr,
    context: &[u8],
) -> Result<(), Errno> {
    // Attempt to convert the Security Context string to a SID.
    let context = NullessByteStr::from(context);
    let sid = match context.as_bytes() {
        b"\x0a" | b"" => None,
        _ => Some(security_server.security_context_to_sid(context).map_err(|_| errno!(EINVAL))?),
    };

    let permission_check = security_server.as_permission_check();
    let current_sid = current_task.security_state.lock().current_sid;
    match attr {
        ProcAttr::Current => {
            check_self_permission(&permission_check, current_sid, ProcessPermission::SetCurrent)?;

            // Permission to dynamically transition to the new Context is also required.
            let new_sid = sid.ok_or_else(|| errno!(EINVAL))?;
            check_permission(
                &permission_check,
                current_sid,
                new_sid,
                ProcessPermission::DynTransition,
            )?;

            if current_task.thread_group.read().tasks_count() > 1 {
                // In multi-threaded programs dynamic transitions may only be used to down-scope
                // the capabilities available to the task. This is verified by requiring an explicit
                // "typebounds" relationship between the current and target domains, indicating that
                // the constraint on permissions of the bounded type has been verified by the policy
                // build tooling and/or will be enforced at run-time on permission checks.
                if !security_server.is_bounded_by(new_sid, current_sid) {
                    return error!(EACCES);
                }
            }

            current_task.security_state.lock().current_sid = new_sid
        }
        ProcAttr::Previous => {
            return error!(EINVAL);
        }
        ProcAttr::Exec => {
            check_self_permission(&permission_check, current_sid, ProcessPermission::SetExec)?;
            current_task.security_state.lock().exec_sid = sid
        }
        ProcAttr::FsCreate => {
            check_self_permission(&permission_check, current_sid, ProcessPermission::SetFsCreate)?;
            current_task.security_state.lock().fscreate_sid = sid
        }
        ProcAttr::KeyCreate => {
            check_self_permission(&permission_check, current_sid, ProcessPermission::SetKeyCreate)?;
            current_task.security_state.lock().keycreate_sid = sid
        }
        ProcAttr::SockCreate => {
            check_self_permission(
                &permission_check,
                current_sid,
                ProcessPermission::SetSockCreate,
            )?;
            current_task.security_state.lock().sockcreate_sid = sid
        }
    };

    Ok(())
}

/// Sets the sid of `fs_node` to be that of `task`.
pub fn fs_node_init_with_task(task: &TempRef<'_, Task>, fs_node: &FsNode) {
    fs_node_ensure_class(fs_node).unwrap();
    fs_node_set_label_with_task(fs_node, task.into());
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::security::selinux_hooks::testing::create_test_executable;
    use crate::security::selinux_hooks::{testing, InitialSid, TaskAttrs};
    use crate::security::update_state_on_exec;
    use crate::testing::create_task;
    use selinux::SecurityId;
    use starnix_uapi::signals::SIGTERM;
    use starnix_uapi::{error, CLONE_SIGHAND, CLONE_THREAD, CLONE_VM};
    use testing::spawn_kernel_with_selinux_hooks_test_policy_and_run;

    #[fuchsia::test]
    async fn task_alloc_from_parent() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |_locked, current_task, _security_server| {
                // Create a fake parent state, with values for some fields, to check for.
                let parent_security_state = TaskAttrs {
                    current_sid: SecurityId::initial(InitialSid::Unlabeled),
                    previous_sid: SecurityId::initial(InitialSid::Kernel),
                    exec_sid: Some(SecurityId::initial(InitialSid::Unlabeled)),
                    fscreate_sid: Some(SecurityId::initial(InitialSid::Unlabeled)),
                    keycreate_sid: Some(SecurityId::initial(InitialSid::Unlabeled)),
                    sockcreate_sid: Some(SecurityId::initial(InitialSid::Unlabeled)),
                };

                *current_task.security_state.lock() = parent_security_state.clone();

                let security_state = task_alloc(&current_task, 0);
                assert_eq!(security_state, parent_security_state);
            },
        )
    }

    #[fuchsia::test]
    fn task_alloc_for() {
        let for_kernel = TaskAttrs::for_kernel();
        assert_eq!(for_kernel.current_sid, SecurityId::initial(InitialSid::Kernel));
        assert_eq!(for_kernel.previous_sid, for_kernel.current_sid);
        assert_eq!(for_kernel.exec_sid, None);
        assert_eq!(for_kernel.fscreate_sid, None);
        assert_eq!(for_kernel.keycreate_sid, None);
        assert_eq!(for_kernel.sockcreate_sid, None);
    }

    #[fuchsia::test]
    async fn task_create_allowed_for_allowed_type() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |_locked, current_task, security_server| {
                current_task.security_state.lock().current_sid = security_server
                    .security_context_to_sid(b"u:object_r:fork_yes_t:s0".into())
                    .expect("invalid security context");

                assert_eq!(
                    check_task_create_access(&security_server.as_permission_check(), &current_task),
                    Ok(())
                );
            },
        )
    }

    #[fuchsia::test]
    async fn task_create_denied_for_denied_type() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |_locked, current_task, security_server| {
                current_task.security_state.lock().current_sid = security_server
                    .security_context_to_sid(b"u:object_r:fork_no_t:s0".into())
                    .expect("invalid security context");

                assert_eq!(
                    check_task_create_access(&security_server.as_permission_check(), &current_task),
                    error!(EACCES)
                );
            },
        )
    }

    #[fuchsia::test]
    async fn exec_transition_allowed_for_allowed_transition_type() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |locked, current_task, security_server| {
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
                    create_test_executable(locked, current_task, executable_security_context);
                let executable_fs_node = &executable.entry.node;

                *current_task.security_state.lock() = TaskAttrs {
                    current_sid: current_sid,
                    exec_sid: Some(exec_sid),
                    fscreate_sid: None,
                    keycreate_sid: None,
                    previous_sid: current_sid,
                    sockcreate_sid: None,
                };

                assert_eq!(
                    check_exec_access(&security_server, &current_task, executable_fs_node),
                    Ok(ResolvedElfState { sid: Some(exec_sid) })
                );
            },
        )
    }

    #[fuchsia::test]
    async fn exec_transition_denied_for_transition_denied_type() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |locked, current_task, security_server| {
                let current_sid = security_server
                    .security_context_to_sid(b"u:object_r:exec_transition_source_t:s0".into())
                    .expect("invalid security context");
                let exec_sid = security_server
                    .security_context_to_sid(
                        b"u:object_r:exec_transition_denied_target_t:s0".into(),
                    )
                    .expect("invalid security context");

                let executable_security_context = b"u:object_r:executable_file_trans_t:s0";
                assert!(security_server
                    .security_context_to_sid(executable_security_context.into())
                    .is_ok());
                let executable =
                    create_test_executable(locked, current_task, executable_security_context);
                let executable_fs_node = &executable.entry.node;

                *current_task.security_state.lock() = TaskAttrs {
                    current_sid: current_sid,
                    exec_sid: Some(exec_sid),
                    fscreate_sid: None,
                    keycreate_sid: None,
                    previous_sid: current_sid,
                    sockcreate_sid: None,
                };

                assert_eq!(
                    check_exec_access(&security_server, &current_task, executable_fs_node),
                    error!(EACCES)
                );
            },
        )
    }

    // TODO(http://b/330904217): reenable test once filesystems are labeled and access is denied.
    #[ignore]
    #[fuchsia::test]
    async fn exec_transition_denied_for_executable_with_no_entrypoint_perm() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |locked, current_task, security_server| {
                let current_sid = security_server
                    .security_context_to_sid(b"u:object_r:exec_transition_source_t:s0".into())
                    .expect("invalid security context");
                let exec_sid = security_server
                    .security_context_to_sid(b"u:object_r:exec_transition_target_t:s0".into())
                    .expect("invalid security context");

                let executable_security_context =
                    b"u:object_r:executable_file_trans_no_entrypoint_t:s0";
                assert!(security_server
                    .security_context_to_sid(executable_security_context.into())
                    .is_ok());
                let executable =
                    create_test_executable(locked, current_task, executable_security_context);
                let executable_fs_node = &executable.entry.node;

                *current_task.security_state.lock() = TaskAttrs {
                    current_sid: current_sid,
                    exec_sid: Some(exec_sid),
                    fscreate_sid: None,
                    keycreate_sid: None,
                    previous_sid: current_sid,
                    sockcreate_sid: None,
                };

                assert_eq!(
                    check_exec_access(&security_server, &current_task, executable_fs_node),
                    error!(EACCES)
                );
            },
        )
    }

    #[fuchsia::test]
    async fn exec_no_trans_allowed_for_executable() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |locked, current_task, security_server| {
                let current_sid = security_server
                    .security_context_to_sid(b"u:object_r:exec_no_trans_source_t:s0".into())
                    .expect("invalid security context");

                let executable_security_context = b"u:object_r:executable_file_no_trans_t:s0";
                assert!(security_server
                    .security_context_to_sid(executable_security_context.into())
                    .is_ok());
                let executable =
                    create_test_executable(locked, current_task, executable_security_context);
                let executable_fs_node = &executable.entry.node;

                *current_task.security_state.lock() = TaskAttrs {
                    current_sid: current_sid,
                    exec_sid: None,
                    fscreate_sid: None,
                    keycreate_sid: None,
                    previous_sid: current_sid,
                    sockcreate_sid: None,
                };

                assert_eq!(
                    check_exec_access(&security_server, &current_task, executable_fs_node),
                    Ok(ResolvedElfState { sid: Some(current_sid) })
                );
            },
        )
    }

    // TODO(http://b/330904217): reenable test once filesystems are labeled and access is denied.
    #[ignore]
    #[fuchsia::test]
    async fn exec_no_trans_denied_for_executable() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |locked, current_task, security_server| {
                let current_sid = security_server
                    .security_context_to_sid(b"u:object_r:exec_transition_target_t:s0".into())
                    .expect("invalid security context");

                let executable_security_context = b"u:object_r:executable_file_no_trans_t:s0";
                assert!(security_server
                    .security_context_to_sid(executable_security_context.into())
                    .is_ok());
                let executable =
                    create_test_executable(locked, current_task, executable_security_context);
                let executable_fs_node = &executable.entry.node;

                *current_task.security_state.lock() = TaskAttrs {
                    current_sid: current_sid,
                    exec_sid: None,
                    fscreate_sid: None,
                    keycreate_sid: None,
                    previous_sid: current_sid,
                    sockcreate_sid: None,
                };

                // There is no `execute_no_trans` allow statement from `current_sid` to `executable_sid`,
                // expect access denied.
                assert_eq!(
                    check_exec_access(&security_server, &current_task, executable_fs_node),
                    error!(EACCES)
                );
            },
        )
    }

    #[fuchsia::test]
    async fn security_state_is_updated_on_exec() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |_locked, current_task, security_server| {
                let initial_state = {
                    let state = &mut current_task.security_state.lock();

                    // Set previous SID to a different value from current, to allow verification
                    // of the pre-exec "current" being moved into "previous".
                    state.previous_sid = SecurityId::initial(InitialSid::Unlabeled);

                    // Set the other optional SIDs to a value, to verify that it is cleared on exec update.
                    state.sockcreate_sid = Some(SecurityId::initial(InitialSid::Unlabeled));
                    state.fscreate_sid = Some(SecurityId::initial(InitialSid::Unlabeled));
                    state.keycreate_sid = Some(SecurityId::initial(InitialSid::Unlabeled));

                    state.clone()
                };

                // Ensure that the ELF binary SID differs from the task's current SID before exec.
                let elf_sid = security_server
                    .security_context_to_sid(b"u:object_r:test_valid_t:s0".into())
                    .expect("invalid security context");
                assert_ne!(elf_sid, initial_state.current_sid);

                update_state_on_exec(&current_task, &ResolvedElfState { sid: Some(elf_sid) });
                assert_eq!(
                    *current_task.security_state.lock(),
                    TaskAttrs {
                        current_sid: elf_sid,
                        exec_sid: None,
                        fscreate_sid: None,
                        keycreate_sid: None,
                        previous_sid: initial_state.current_sid,
                        sockcreate_sid: None,
                    }
                );
            },
        )
    }

    #[fuchsia::test]
    // The hooks_tests_policy denies the `rlimitinh` permission (implicitly, via `handle_unknown deny`)
    // for processes, so resource limits should be reset when the SID changes during exec.
    async fn handle_rlimitinh_on_exec() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |mut locked, current_task, security_server| {
                // In this testing context, `current_task` is the initial task.
                // Set its rlimits to some known values.
                assert_eq!(current_task.id, 1);
                {
                    let mut initial_limits = current_task.thread_group.limits.lock();
                    (Resource::ALL).iter().for_each(|resource| {
                        initial_limits.set(*resource, rlimit { rlim_cur: 10, rlim_max: 20 });
                    })
                }
                // Clone the initial task, then set the child task's rlimits to some new values.
                let child_task = current_task.clone_task_for_test(&mut locked, 0, Some(SIGCHLD));
                {
                    let mut child_limits = child_task.thread_group.limits.lock();
                    (Resource::ALL).iter().for_each(|resource| {
                        child_limits.set(*resource, rlimit { rlim_cur: 30, rlim_max: 40 });
                    })
                }

                // Clone the child task. Before exec, the grandchild task's rlimits should be equal
                // to its parent's.
                let grandchild_task = child_task.clone_task_for_test(&mut locked, 0, Some(SIGCHLD));
                let parent_limits = { child_task.thread_group.limits.lock().clone() };
                let pre_exec_limits = { grandchild_task.thread_group.limits.lock().clone() };
                {
                    (Resource::ALL).iter().for_each(|resource| {
                        let parent = parent_limits.get(*resource);
                        let pre_exec = pre_exec_limits.get(*resource);
                        assert_eq!(parent.rlim_cur, pre_exec.rlim_cur);
                        assert_eq!(parent.rlim_max, pre_exec.rlim_max);
                    })
                }

                // Simulate exec of the grandchild task into a new domain.
                let old_sid = { child_task.security_state.lock().current_sid };
                let new_sid = security_server
                    .security_context_to_sid(b"u:object_r:test_valid_t:s0".into())
                    .expect("invalid security context");
                assert_ne!(old_sid, new_sid);
                update_state_on_exec(&grandchild_task, &ResolvedElfState { sid: Some(new_sid) });

                let post_exec_limits = { grandchild_task.thread_group.limits.lock().clone() };
                {
                    (Resource::ALL).iter().for_each(|resource| {
                        let pre_exec = pre_exec_limits.get(*resource);
                        let post_exec = post_exec_limits.get(*resource);
                        // Soft limits are reset to the minimum of the pre-exec hard limit and
                        // the initial task's soft limit.
                        assert_eq!(post_exec.rlim_cur, 10);
                        // Hard limits are unchanged.
                        assert_eq!(pre_exec.rlim_max, post_exec.rlim_max);
                    })
                }

                // rlimits are not reset when the task SID does not change.
                let same_domain_task =
                    child_task.clone_task_for_test(&mut locked, 0, Some(SIGCHLD));
                update_state_on_exec(&same_domain_task, &ResolvedElfState { sid: Some(old_sid) });
                let same_domain_limits = { same_domain_task.thread_group.limits.lock().clone() };
                {
                    let parent_limits = { child_task.thread_group.limits.lock().clone() };
                    (Resource::ALL).iter().for_each(|resource| {
                        let parent = parent_limits.get(*resource);
                        let same_domain = same_domain_limits.get(*resource);
                        assert_eq!(parent.rlim_cur, same_domain.rlim_cur);
                        assert_eq!(parent.rlim_max, same_domain.rlim_max);
                    })
                }
            },
        )
    }

    #[fuchsia::test]
    async fn setsched_access_allowed_for_allowed_type() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |locked, current_task, security_server| {
                let target_task = create_task(locked, &current_task.kernel(), "target_task");

                current_task.security_state.lock().current_sid = security_server
                    .security_context_to_sid(b"u:object_r:test_setsched_yes_t:s0".into())
                    .expect("invalid security context");
                target_task.security_state.lock().current_sid = security_server
                    .security_context_to_sid(b"u:object_r:test_setsched_target_t:s0".into())
                    .expect("invalid security context");

                assert_eq!(
                    check_setsched_access(
                        &security_server.as_permission_check(),
                        &current_task,
                        &target_task
                    ),
                    Ok(())
                );
            },
        )
    }

    #[fuchsia::test]
    async fn setsched_access_denied_for_denied_type() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |locked, current_task, security_server| {
                let target_task = create_task(locked, &current_task.kernel(), "target_task");

                current_task.security_state.lock().current_sid = security_server
                    .security_context_to_sid(b"u:object_r:test_setsched_no_t:s0".into())
                    .expect("invalid security context");
                target_task.security_state.lock().current_sid = security_server
                    .security_context_to_sid(b"u:object_r:test_setsched_target_t:s0".into())
                    .expect("invalid security context");

                assert_eq!(
                    check_setsched_access(
                        &security_server.as_permission_check(),
                        &current_task,
                        &target_task
                    ),
                    error!(EACCES)
                );
            },
        )
    }

    #[fuchsia::test]
    async fn getsched_access_allowed_for_allowed_type() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |locked, current_task, security_server| {
                let target_task = create_task(locked, &current_task.kernel(), "target_task");

                current_task.security_state.lock().current_sid = security_server
                    .security_context_to_sid(b"u:object_r:test_getsched_yes_t:s0".into())
                    .expect("invalid security context");
                target_task.security_state.lock().current_sid = security_server
                    .security_context_to_sid(b"u:object_r:test_getsched_target_t:s0".into())
                    .expect("invalid security context");

                assert_eq!(
                    check_getsched_access(
                        &security_server.as_permission_check(),
                        &current_task,
                        &target_task
                    ),
                    Ok(())
                );
            },
        )
    }

    #[fuchsia::test]
    async fn getsched_access_denied_for_denied_type() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |locked, current_task, security_server| {
                let target_task = create_task(locked, &current_task.kernel(), "target_task");

                current_task.security_state.lock().current_sid = security_server
                    .security_context_to_sid(b"u:object_r:test_getsched_no_t:s0".into())
                    .expect("invalid security context");
                target_task.security_state.lock().current_sid = security_server
                    .security_context_to_sid(b"u:object_r:test_getsched_target_t:s0".into())
                    .expect("invalid security context");

                assert_eq!(
                    check_getsched_access(
                        &security_server.as_permission_check(),
                        &current_task,
                        &target_task
                    ),
                    error!(EACCES)
                );
            },
        )
    }

    #[fuchsia::test]
    async fn getpgid_access_allowed_for_allowed_type() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |locked, current_task, security_server| {
                let target_task = create_task(locked, &current_task.kernel(), "target_task");

                current_task.security_state.lock().current_sid = security_server
                    .security_context_to_sid(b"u:object_r:test_getpgid_yes_t:s0".into())
                    .expect("invalid security context");
                target_task.security_state.lock().current_sid = security_server
                    .security_context_to_sid(b"u:object_r:test_getpgid_target_t:s0".into())
                    .expect("invalid security context");

                assert_eq!(
                    check_getpgid_access(
                        &security_server.as_permission_check(),
                        &current_task,
                        &target_task
                    ),
                    Ok(())
                );
            },
        )
    }

    #[fuchsia::test]
    async fn getpgid_access_denied_for_denied_type() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |locked, current_task, security_server| {
                let target_task = create_task(locked, &current_task.kernel(), "target_task");

                current_task.security_state.lock().current_sid = security_server
                    .security_context_to_sid(b"u:object_r:test_getpgid_no_t:s0".into())
                    .expect("invalid security context");
                target_task.security_state.lock().current_sid = security_server
                    .security_context_to_sid(b"u:object_r:test_getpgid_target_t:s0".into())
                    .expect("invalid security context");

                assert_eq!(
                    check_getpgid_access(
                        &security_server.as_permission_check(),
                        &current_task,
                        &target_task
                    ),
                    error!(EACCES)
                );
            },
        )
    }

    #[fuchsia::test]
    async fn sigkill_access_allowed_for_allowed_type() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |locked, current_task, security_server| {
                let target_task = create_task(locked, &current_task.kernel(), "target_task");

                current_task.security_state.lock().current_sid = security_server
                    .security_context_to_sid(b"u:object_r:test_kill_sigkill_t:s0".into())
                    .expect("invalid security context");
                target_task.security_state.lock().current_sid = security_server
                    .security_context_to_sid(b"u:object_r:test_kill_target_t:s0".into())
                    .expect("invalid security context");

                assert_eq!(
                    check_signal_access(
                        &security_server.as_permission_check(),
                        &current_task,
                        &target_task,
                        SIGKILL,
                    ),
                    Ok(())
                );
            },
        )
    }

    #[fuchsia::test]
    async fn sigchld_access_allowed_for_allowed_type() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |locked, current_task, security_server| {
                let target_task = create_task(locked, &current_task.kernel(), "target_task");

                current_task.security_state.lock().current_sid = security_server
                    .security_context_to_sid(b"u:object_r:test_kill_sigchld_t:s0".into())
                    .expect("invalid security context");
                target_task.security_state.lock().current_sid = security_server
                    .security_context_to_sid(b"u:object_r:test_kill_target_t:s0".into())
                    .expect("invalid security context");

                assert_eq!(
                    check_signal_access(
                        &security_server.as_permission_check(),
                        &current_task,
                        &target_task,
                        SIGCHLD,
                    ),
                    Ok(())
                );
            },
        )
    }

    #[fuchsia::test]
    async fn sigstop_access_allowed_for_allowed_type() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |locked, current_task, security_server| {
                let target_task = create_task(locked, &current_task.kernel(), "target_task");

                current_task.security_state.lock().current_sid = security_server
                    .security_context_to_sid(b"u:object_r:test_kill_sigstop_t:s0".into())
                    .expect("invalid security context");
                target_task.security_state.lock().current_sid = security_server
                    .security_context_to_sid(b"u:object_r:test_kill_target_t:s0".into())
                    .expect("invalid security context");

                assert_eq!(
                    check_signal_access(
                        &security_server.as_permission_check(),
                        &current_task,
                        &target_task,
                        SIGSTOP,
                    ),
                    Ok(())
                );
            },
        )
    }

    #[fuchsia::test]
    async fn signal_access_allowed_for_allowed_type() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |locked, current_task, security_server| {
                let target_task = create_task(locked, &current_task.kernel(), "target_task");

                current_task.security_state.lock().current_sid = security_server
                    .security_context_to_sid(b"u:object_r:test_kill_signal_t:s0".into())
                    .expect("invalid security context");
                target_task.security_state.lock().current_sid = security_server
                    .security_context_to_sid(b"u:object_r:test_kill_target_t:s0".into())
                    .expect("invalid security context");

                // The `signal` permission allows signals other than SIGKILL, SIGCHLD, SIGSTOP.
                assert_eq!(
                    check_signal_access(
                        &security_server.as_permission_check(),
                        &current_task,
                        &target_task,
                        SIGTERM,
                    ),
                    Ok(())
                );
            },
        )
    }

    #[fuchsia::test]
    async fn signal_access_denied_for_denied_signals() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |locked, current_task, security_server| {
                let target_task = create_task(locked, &current_task.kernel(), "target_task");

                current_task.security_state.lock().current_sid = security_server
                    .security_context_to_sid(b"u:object_r:test_kill_signal_t:s0".into())
                    .expect("invalid security context");
                target_task.security_state.lock().current_sid = security_server
                    .security_context_to_sid(b"u:object_r:test_kill_target_t:s0".into())
                    .expect("invalid security context");

                // The `signal` permission does not allow SIGKILL, SIGCHLD or SIGSTOP.
                for signal in [SIGCHLD, SIGKILL, SIGSTOP] {
                    assert_eq!(
                        check_signal_access(
                            &security_server.as_permission_check(),
                            &current_task,
                            &target_task,
                            signal,
                        ),
                        error!(EACCES)
                    );
                }
            },
        )
    }

    #[fuchsia::test]
    async fn ptrace_access_allowed_for_allowed_type_and_state_is_updated() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |locked, current_task, security_server| {
                let tracee_task = create_task(locked, &current_task.kernel(), "target_task");

                current_task.security_state.lock().current_sid = security_server
                    .security_context_to_sid(b"u:object_r:test_ptrace_tracer_yes_t:s0".into())
                    .expect("invalid security context");
                {
                    let attrs = &mut tracee_task.security_state.lock();
                    attrs.current_sid = security_server
                        .security_context_to_sid(b"u:object_r:test_ptrace_traced_t:s0".into())
                        .expect("invalid security context");
                    attrs.previous_sid = attrs.current_sid;
                }

                assert_eq!(
                    ptrace_access_check(
                        &security_server.as_permission_check(),
                        &current_task,
                        &tracee_task,
                    ),
                    Ok(())
                );
            },
        )
    }

    #[fuchsia::test]
    async fn ptrace_access_denied_for_denied_type_and_state_is_not_updated() {
        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |locked, current_task, security_server| {
                let tracee_task = create_task(locked, &current_task.kernel(), "target_task");

                current_task.security_state.lock().current_sid = security_server
                    .security_context_to_sid(b"u:object_r:test_ptrace_tracer_no_t:s0".into())
                    .expect("invalid security context");
                {
                    let attrs = &mut tracee_task.security_state.lock();
                    attrs.current_sid = security_server
                        .security_context_to_sid(b"u:object_r:test_ptrace_traced_t:s0".into())
                        .expect("invalid security context");
                    attrs.previous_sid = attrs.current_sid;
                }

                assert_eq!(
                    ptrace_access_check(
                        &security_server.as_permission_check(),
                        &current_task,
                        &tracee_task,
                    ),
                    error!(EACCES)
                );
                // TODO: Verify that the tracer has not been set on `tracee_task`.
            },
        )
    }

    #[fuchsia::test]
    async fn setcurrent_bounds() {
        const BINARY_POLICY: &[u8] = include_bytes!("../../../lib/selinux/testdata/composite_policies/compiled/bounded_transition_policy.pp");
        const BOUNDED_CONTEXT: &[u8] = b"test_u:test_r:bounded_t:s0";
        const UNBOUNDED_CONTEXT: &[u8] = b"test_u:test_r:unbounded_t:s0";

        spawn_kernel_with_selinux_hooks_test_policy_and_run(
            |locked, current_task, security_server| {
                security_server.load_policy(BINARY_POLICY.to_vec()).expect("policy load failed");

                let unbounded_sid = security_server
                    .security_context_to_sid(UNBOUNDED_CONTEXT.into())
                    .expect("Make unbounded SID");
                current_task.security_state.lock().current_sid = unbounded_sid;

                // Thread-group has a single task, so dynamic transitions are permitted, with "setcurrent"
                // and "dyntransition".
                assert_eq!(
                    set_procattr(
                        &security_server,
                        &current_task,
                        ProcAttr::Current,
                        BOUNDED_CONTEXT
                    ),
                    Ok(()),
                    "Unbounded_t->bounded_t single-threaded"
                );
                assert_eq!(
                    set_procattr(
                        &security_server,
                        &current_task,
                        ProcAttr::Current,
                        UNBOUNDED_CONTEXT
                    ),
                    Ok(()),
                    "Bounded_t->unbounded_t single-threaded"
                );

                // Create a second task in the same thread group.
                let _child_task = current_task.clone_task_for_test(
                    locked,
                    (CLONE_THREAD | CLONE_VM | CLONE_SIGHAND) as u64,
                    None,
                );

                // Thread-group has a multiple tasks, so dynamic transitions to are only allowed to bounded
                // domains.
                assert_eq!(
                    set_procattr(
                        &security_server,
                        &current_task,
                        ProcAttr::Current,
                        BOUNDED_CONTEXT
                    ),
                    Ok(()),
                    "Unbounded_t->bounded_t multi-threaded"
                );
                assert_eq!(
                    set_procattr(
                        &security_server,
                        &current_task,
                        ProcAttr::Current,
                        UNBOUNDED_CONTEXT
                    ),
                    error!(EACCES),
                    "Bounded_t->unbounded_t multi-threaded"
                );
            },
        )
    }
}
