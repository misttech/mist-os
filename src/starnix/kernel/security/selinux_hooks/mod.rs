// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub(super) mod fs;

use super::ProcAttr;

use selinux::{
    permission_check::PermissionCheck, security_server::SecurityServer, InitialSid, SecurityId,
};
use selinux_common::{FilePermission, ObjectClass, ProcessPermission};
use starnix_logging::log_debug;
use starnix_uapi::{
    errno, error,
    errors::Errno,
    signals::{Signal, SIGCHLD, SIGKILL, SIGSTOP},
};
use std::sync::Arc;

/// Checks if creating a task is allowed.
pub(super) fn check_task_create_access(
    permission_check: &impl PermissionCheck,
    task_sid: SecurityId,
) -> Result<(), Errno> {
    // When creating a process there is no transition involved, the source and target SIDs
    // are the current SID.
    let target_sid = task_sid;
    check_permission(permission_check, task_sid, target_sid, ProcessPermission::Fork)
}

/// Checks the SELinux permissions required for exec. Returns the SELinux state of a resolved
/// elf if all required permissions are allowed.
pub(super) fn check_exec_access(
    security_server: &Arc<SecurityServer>,
    security_state: &ThreadGroupState,
    executable_sid: SecurityId,
) -> Result<Option<SecurityId>, Errno> {
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
        if !security_server.has_permission(
            current_sid,
            executable_sid,
            FilePermission::ExecuteNoTrans,
        ) {
            // TODO(http://b/330904217): once filesystems are labeled, deny access.
            log_debug!("execute_no_trans permission is denied, ignoring.");
        }
    } else {
        // Domain transition, check that transition is allowed.
        if !security_server.has_permission(current_sid, new_sid, ProcessPermission::Transition) {
            return error!(EACCES);
        }
        // Check that the executable file has an entry point into the new domain.
        if !security_server.has_permission(new_sid, executable_sid, FilePermission::Entrypoint) {
            // TODO(http://b/330904217): once filesystems are labeled, deny access.
            log_debug!("entrypoint permission is denied, ignoring.");
        }
        // Check that ptrace permission is allowed if the process is traced.
        if let Some(ptracer_sid) = security_state.ptracer_sid {
            if !security_server.has_permission(ptracer_sid, new_sid, ProcessPermission::Ptrace) {
                return error!(EACCES);
            }
        }
    }
    Ok(Some(new_sid))
}

/// Updates the SELinux thread group state on exec, using the security ID associated with the
/// resolved elf.
pub(super) fn update_state_on_exec(
    security_state: &mut ThreadGroupState,
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
    check_permission(permission_check, source_sid, target_sid, ProcessPermission::GetSched)
}

/// Checks if the task with `source_sid` is allowed to set scheduling parameters for the task with
/// `target_sid`.
pub(super) fn check_setsched_access(
    permission_check: &impl PermissionCheck,
    source_sid: SecurityId,
    target_sid: SecurityId,
) -> Result<(), Errno> {
    check_permission(permission_check, source_sid, target_sid, ProcessPermission::SetSched)
}

/// Checks if the task with `source_sid` is allowed to get the process group ID of the task with
/// `target_sid`.
pub(super) fn check_getpgid_access(
    permission_check: &impl PermissionCheck,
    source_sid: SecurityId,
    target_sid: SecurityId,
) -> Result<(), Errno> {
    check_permission(permission_check, source_sid, target_sid, ProcessPermission::GetPgid)
}

/// Checks if the task with `source_sid` is allowed to set the process group ID of the task with
/// `target_sid`.
pub(super) fn check_setpgid_access(
    permission_check: &impl PermissionCheck,
    source_sid: SecurityId,
    target_sid: SecurityId,
) -> Result<(), Errno> {
    check_permission(permission_check, source_sid, target_sid, ProcessPermission::SetPgid)
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

/// Checks if the task with `source_sid` is allowed to trace the task with `target_sid`.
pub(super) fn check_ptrace_access_and_update_state(
    permission_check: &impl PermissionCheck,
    tracer_sid: SecurityId,
    tracee_security_state: &mut ThreadGroupState,
) -> Result<(), Errno> {
    check_permission(
        permission_check,
        tracer_sid,
        tracee_security_state.current_sid,
        ProcessPermission::Ptrace,
    )
    .and_then(|_| {
        // If tracing is allowed, set the `ptracer_sid` of the tracee with the tracer's SID.
        tracee_security_state.ptracer_sid = Some(tracer_sid);
        Ok(())
    })
}

/// Returns the Security Context associated with the `name`ed entry for the specified `target` task.
/// `source` describes the calling task, `target` the state of the task for which to return the attribute.
pub fn get_procattr(
    security_server: &SecurityServer,
    _source: SecurityId,
    target: &ThreadGroupState,
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
    security_server: &SecurityServer,
    _source: SecurityId,
    target: &mut ThreadGroupState,
    attr: ProcAttr,
    context: &[u8],
) -> Result<(), Errno> {
    use bstr::ByteSlice;

    // TODO(b/322849067): Does the `source` have the required permission(s)?

    // Attempt to convert the Security Context string to a SID.
    // Writes that consist of a single NUL or a newline clear the SID.
    let context = context.trim_end_with(|c| c == '\0');
    let sid = match context {
        b"\x0a" => None,
        slice => Some(security_server.security_context_to_sid(slice).map_err(|_| errno!(EINVAL))?),
    };

    match attr {
        ProcAttr::Current => target.current_sid = sid.ok_or(errno!(EINVAL))?,
        ProcAttr::Exec => target.exec_sid = sid,
        ProcAttr::FsCreate => target.fscreate_sid = sid,
        ProcAttr::KeyCreate => target.keycreate_sid = sid,
        ProcAttr::Previous => {
            return error!(EINVAL);
        }
        ProcAttr::SockCreate => target.sockcreate_sid = sid,
    };

    Ok(())
}

/// Checks if `permission` is allowed from the task with `source_sid` to the task with `target_sid`.
fn check_permission(
    permission_check: &impl PermissionCheck,
    source_sid: SecurityId,
    target_sid: SecurityId,
    permission: ProcessPermission,
) -> Result<(), Errno> {
    match permission_check.has_permission(source_sid, target_sid, permission) {
        true => Ok(()),
        false => error!(EACCES),
    }
}

/// Returns a `ThreadGroupState` instance for a new task.
pub(super) fn alloc_security(parent: Option<&ThreadGroupState>) -> ThreadGroupState {
    match parent {
        Some(parent) => parent.clone(),
        None => ThreadGroupState::for_kernel(),
    }
}

/// The SELinux security structure for `ThreadGroup`.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ThreadGroupState {
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

impl ThreadGroupState {
    /// Returns initial state for the kernel's root thread group.
    pub(super) fn for_kernel() -> Self {
        Self {
            current_sid: SecurityId::initial(InitialSid::Kernel),
            previous_sid: SecurityId::initial(InitialSid::Kernel),
            exec_sid: None,
            fscreate_sid: None,
            keycreate_sid: None,
            sockcreate_sid: None,
            ptracer_sid: None,
        }
    }

    /// Returns placeholder state for use when SELinux is not enabled.
    pub(super) fn for_selinux_disabled() -> Self {
        Self {
            current_sid: SecurityId::initial(InitialSid::Unlabeled),
            previous_sid: SecurityId::initial(InitialSid::Unlabeled),
            exec_sid: None,
            fscreate_sid: None,
            keycreate_sid: None,
            sockcreate_sid: None,
            ptracer_sid: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use selinux::security_server::Mode;
    use starnix_uapi::signals::SIGTERM;

    const HOOKS_TESTS_BINARY_POLICY: &[u8] =
        include_bytes!("../../../lib/selinux/testdata/micro_policies/hooks_tests_policy.pp");

    fn security_server_with_policy() -> Arc<SecurityServer> {
        let policy_bytes = HOOKS_TESTS_BINARY_POLICY.to_vec();
        let security_server = SecurityServer::new(Mode::Enable);
        security_server.set_enforcing(true);
        security_server.load_policy(policy_bytes).expect("policy load failed");
        security_server
    }

    #[fuchsia::test]
    fn task_create_allowed_for_allowed_type() {
        let security_server = security_server_with_policy();
        let sid = security_server
            .security_context_to_sid(b"u:object_r:fork_yes_t:s0")
            .expect("invalid security context");

        assert_eq!(check_task_create_access(&security_server.as_permission_check(), sid), Ok(()));
    }

    #[fuchsia::test]
    fn task_create_denied_for_denied_type() {
        let security_server = security_server_with_policy();
        let sid = security_server
            .security_context_to_sid(b"u:object_r:fork_no_t:s0")
            .expect("invalid security context");

        assert_eq!(
            check_task_create_access(&security_server.as_permission_check(), sid),
            error!(EACCES)
        );
    }

    #[fuchsia::test]
    fn exec_transition_allowed_for_allowed_transition_type() {
        let security_server = security_server_with_policy();
        let current_sid = security_server
            .security_context_to_sid(b"u:object_r:exec_transition_source_t:s0")
            .expect("invalid security context");
        let exec_sid = security_server
            .security_context_to_sid(b"u:object_r:exec_transition_target_t:s0")
            .expect("invalid security context");
        let executable_sid = security_server
            .security_context_to_sid(b"u:object_r:executable_file_trans_t:s0")
            .expect("invalid security context");
        let security_state = ThreadGroupState {
            current_sid: current_sid,
            exec_sid: Some(exec_sid),
            fscreate_sid: None,
            keycreate_sid: None,
            previous_sid: current_sid,
            sockcreate_sid: None,
            ptracer_sid: None,
        };

        assert_eq!(
            check_exec_access(&security_server, &security_state, executable_sid),
            Ok(Some(exec_sid))
        );
    }

    #[fuchsia::test]
    fn exec_transition_denied_for_transition_denied_type() {
        let security_server = security_server_with_policy();
        let current_sid = security_server
            .security_context_to_sid(b"u:object_r:exec_transition_source_t:s0")
            .expect("invalid security context");
        let exec_sid = security_server
            .security_context_to_sid(b"u:object_r:exec_transition_denied_target_t:s0")
            .expect("invalid security context");
        let executable_sid = security_server
            .security_context_to_sid(b"u:object_r:executable_file_trans_t:s0")
            .expect("invalid security context");
        let security_state = ThreadGroupState {
            current_sid: current_sid,
            exec_sid: Some(exec_sid),
            fscreate_sid: None,
            keycreate_sid: None,
            previous_sid: current_sid,
            sockcreate_sid: None,
            ptracer_sid: None,
        };

        assert_eq!(
            check_exec_access(&security_server, &security_state, executable_sid),
            error!(EACCES)
        );
    }

    // TODO(http://b/330904217): reenable test once filesystems are labeled and access is denied.
    #[ignore]
    #[fuchsia::test]
    fn exec_transition_denied_for_executable_with_no_entrypoint_perm() {
        let security_server = security_server_with_policy();
        let current_sid = security_server
            .security_context_to_sid(b"u:object_r:exec_transition_source_t:s0")
            .expect("invalid security context");
        let exec_sid = security_server
            .security_context_to_sid(b"u:object_r:exec_transition_target_t:s0")
            .expect("invalid security context");
        let executable_sid = security_server
            .security_context_to_sid(b"u:object_r:executable_file_trans_no_entrypoint_t:s0")
            .expect("invalid security context");
        let security_state = ThreadGroupState {
            current_sid: current_sid,
            exec_sid: Some(exec_sid),
            fscreate_sid: None,
            keycreate_sid: None,
            previous_sid: current_sid,
            sockcreate_sid: None,
            ptracer_sid: None,
        };

        assert_eq!(
            check_exec_access(&security_server, &security_state, executable_sid),
            error!(EACCES)
        );
    }

    #[fuchsia::test]
    fn exec_no_trans_allowed_for_executable() {
        let security_server = security_server_with_policy();

        let current_sid = security_server
            .security_context_to_sid(b"u:object_r:exec_no_trans_source_t:s0")
            .expect("invalid security context");
        let executable_sid = security_server
            .security_context_to_sid(b"u:object_r:executable_file_no_trans_t:s0")
            .expect("invalid security context");
        let security_state = ThreadGroupState {
            current_sid: current_sid,
            exec_sid: None,
            fscreate_sid: None,
            keycreate_sid: None,
            previous_sid: current_sid,
            sockcreate_sid: None,
            ptracer_sid: None,
        };

        assert_eq!(
            check_exec_access(&security_server, &security_state, executable_sid),
            Ok(Some(current_sid))
        );
    }

    // TODO(http://b/330904217): reenable test once filesystems are labeled and access is denied.
    #[ignore]
    #[fuchsia::test]
    fn exec_no_trans_denied_for_executable() {
        let security_server = security_server_with_policy();
        let current_sid = security_server
            .security_context_to_sid(b"u:object_r:exec_transition_target_t:s0")
            .expect("invalid security context");
        let executable_sid = security_server
            .security_context_to_sid(b"u:object_r:executable_file_no_trans_t:s0")
            .expect("invalid security context");
        let security_state = ThreadGroupState {
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
            check_exec_access(&security_server, &security_state, executable_sid),
            error!(EACCES)
        );
    }

    #[fuchsia::test]
    fn no_state_update_if_no_elf_state() {
        let initial_state = ThreadGroupState::for_kernel();
        let mut security_state = initial_state.clone();
        update_state_on_exec(&mut security_state, None);
        assert_eq!(security_state, initial_state);
    }

    #[fuchsia::test]
    fn state_is_updated_on_exec() {
        let security_server = security_server_with_policy();
        let initial_state = ThreadGroupState::for_kernel();
        let mut security_state = initial_state.clone();

        let elf_sid = security_server
            .security_context_to_sid(b"u:object_r:test_valid_t:s0")
            .expect("invalid security context");
        assert_ne!(elf_sid, initial_state.current_sid);
        update_state_on_exec(&mut security_state, Some(elf_sid));
        assert_eq!(
            security_state,
            ThreadGroupState {
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
            .security_context_to_sid(b"u:object_r:test_getsched_yes_t:s0")
            .expect("invalid security context");
        let target_sid = security_server
            .security_context_to_sid(b"u:object_r:test_getsched_target_t:s0")
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
            .security_context_to_sid(b"u:object_r:test_getsched_no_t:s0")
            .expect("invalid security context");
        let target_sid = security_server
            .security_context_to_sid(b"u:object_r:test_getsched_target_t:s0")
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
            .security_context_to_sid(b"u:object_r:test_setsched_yes_t:s0")
            .expect("invalid security context");
        let target_sid = security_server
            .security_context_to_sid(b"u:object_r:test_setsched_target_t:s0")
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
            .security_context_to_sid(b"u:object_r:test_setsched_no_t:s0")
            .expect("invalid security context");
        let target_sid = security_server
            .security_context_to_sid(b"u:object_r:test_setsched_target_t:s0")
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
            .security_context_to_sid(b"u:object_r:test_getpgid_yes_t:s0")
            .expect("invalid security context");
        let target_sid = security_server
            .security_context_to_sid(b"u:object_r:test_getpgid_target_t:s0")
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
            .security_context_to_sid(b"u:object_r:test_getpgid_no_t:s0")
            .expect("invalid security context");
        let target_sid = security_server
            .security_context_to_sid(b"u:object_r:test_getpgid_target_t:s0")
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
            .security_context_to_sid(b"u:object_r:test_kill_sigkill_t:s0")
            .expect("invalid security context");
        let target_sid = security_server
            .security_context_to_sid(b"u:object_r:test_kill_target_t:s0")
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
            .security_context_to_sid(b"u:object_r:test_kill_sigchld_t:s0")
            .expect("invalid security context");
        let target_sid = security_server
            .security_context_to_sid(b"u:object_r:test_kill_target_t:s0")
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
            .security_context_to_sid(b"u:object_r:test_kill_sigstop_t:s0")
            .expect("invalid security context");
        let target_sid = security_server
            .security_context_to_sid(b"u:object_r:test_kill_target_t:s0")
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
            .security_context_to_sid(b"u:object_r:test_kill_signal_t:s0")
            .expect("invalid security context");
        let target_sid = security_server
            .security_context_to_sid(b"u:object_r:test_kill_target_t:s0")
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
            .security_context_to_sid(b"u:object_r:test_kill_signal_t:s0")
            .expect("invalid security context");
        let target_sid = security_server
            .security_context_to_sid(b"u:object_r:test_kill_target_t:s0")
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
            .security_context_to_sid(b"u:object_r:test_ptrace_tracer_yes_t:s0")
            .expect("invalid security context");
        let tracee_sid = security_server
            .security_context_to_sid(b"u:object_r:test_ptrace_traced_t:s0")
            .expect("invalid security context");
        let initial_state = ThreadGroupState {
            current_sid: tracee_sid.clone(),
            exec_sid: None,
            fscreate_sid: None,
            keycreate_sid: None,
            previous_sid: tracee_sid,
            sockcreate_sid: None,
            ptracer_sid: None,
        };
        let mut tracee_state = initial_state.clone();

        assert_eq!(
            check_ptrace_access_and_update_state(
                &security_server.as_permission_check(),
                tracer_sid.clone(),
                &mut tracee_state
            ),
            Ok(())
        );
        assert_eq!(
            tracee_state,
            ThreadGroupState {
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
            .security_context_to_sid(b"u:object_r:test_ptrace_tracer_no_t:s0")
            .expect("invalid security context");
        let tracee_sid = security_server
            .security_context_to_sid(b"u:object_r:test_ptrace_traced_t:s0")
            .expect("invalid security context");
        let initial_state = ThreadGroupState {
            current_sid: tracee_sid.clone(),
            exec_sid: None,
            fscreate_sid: None,
            keycreate_sid: None,
            previous_sid: tracee_sid,
            sockcreate_sid: None,
            ptracer_sid: None,
        };
        let mut tracee_state = initial_state.clone();

        assert_eq!(
            check_ptrace_access_and_update_state(
                &security_server.as_permission_check(),
                tracer_sid,
                &mut tracee_state
            ),
            error!(EACCES)
        );
        assert_eq!(initial_state, tracee_state);
    }

    #[fuchsia::test]
    async fn alloc_security_no_parent() {
        let security_state = alloc_security(None);
        assert_eq!(
            security_state,
            ThreadGroupState {
                current_sid: SecurityId::initial(InitialSid::Kernel),
                previous_sid: SecurityId::initial(InitialSid::Kernel),
                exec_sid: None,
                fscreate_sid: None,
                keycreate_sid: None,
                sockcreate_sid: None,
                ptracer_sid: None,
            }
        );
    }

    #[fuchsia::test]
    async fn alloc_security_from_parent() {
        // Create a fake parent state, with values for some fields, to check for.
        let mut parent_security_state = alloc_security(None);
        parent_security_state.current_sid = SecurityId::initial(InitialSid::Unlabeled);
        parent_security_state.exec_sid = Some(SecurityId::initial(InitialSid::Unlabeled));
        parent_security_state.fscreate_sid = Some(SecurityId::initial(InitialSid::Unlabeled));
        parent_security_state.keycreate_sid = Some(SecurityId::initial(InitialSid::Unlabeled));
        parent_security_state.sockcreate_sid = Some(SecurityId::initial(InitialSid::Unlabeled));

        let security_state = alloc_security(Some(&parent_security_state));
        assert_eq!(security_state, parent_security_state);
    }
}
