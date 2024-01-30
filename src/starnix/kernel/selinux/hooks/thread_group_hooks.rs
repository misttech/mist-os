// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use selinux::{
    permission_check::PermissionCheck,
    security_server::{SecurityServer, SecurityServerStatus},
    SecurityId,
};
use selinux_common::{security_context::SecurityContext, ProcessPermission};
use starnix_uapi::{error, errors::Errno};
use std::sync::Arc;

/// Checks if creating a task is allowed.
pub(crate) fn check_task_create_access(
    status: &impl SecurityServerStatus,
    permission_check: &impl PermissionCheck,
    selinux_state: &Option<SeLinuxThreadGroupState>,
) -> Result<(), Errno> {
    if status.is_fake() || !status.is_enforcing() {
        // No-op if SELinux is in fake mode or not enforcing.
        return Ok(());
    }

    return selinux_state.as_ref().map_or(Ok(()), |selinux_state| {
        let sid = selinux_state.current_sid.clone();
        // When creating a process there is no transition involved, the source and target SIDs
        // are the current SID.
        if permission_check.has_permission(sid.clone(), sid, ProcessPermission::Fork) {
            Ok(())
        } else {
            error!(EACCES)
        }
    });
}

/// Checks the SELinux permissions required for exec. Returns the SELinux state of a resolved
/// elf if all required permissions are allowed.
pub(crate) fn check_exec_access(
    status: &impl SecurityServerStatus,
    permission_check: &impl PermissionCheck,
    selinux_state: &Option<SeLinuxThreadGroupState>,
) -> Result<Option<SeLinuxResolvedElfState>, Errno> {
    if status.is_fake() || !status.is_enforcing() {
        // No-op if SELinux is in fake mode or not enforcing.
        return Ok(None);
    }

    return selinux_state.as_ref().map_or(Ok(None), |selinux_state| {
        let current_sid = &selinux_state.current_sid;
        let new_sid = if let Some(exec_sid) = &selinux_state.exec_sid {
            // Use the proc exec SID if set.
            exec_sid
        } else {
            // TODO(http://b/320436714): Check typetransition rules from the current security
            // context to the executable's security context. If none, use the current security
            // context.
            current_sid
        };
        if current_sid == new_sid {
            // No domain transition.
            // TODO(http://b/320436714): check that the current security context has execute
            // rights to the executable file.
        } else {
            // Domain transition, check that transition is allowed.
            if !permission_check.has_permission(
                current_sid.clone(),
                new_sid.clone(),
                ProcessPermission::Transition,
            ) {
                return error!(EACCES);
            }
            // TODO(http://b/320436714): Check executable permissions:
            // - allow rule from `new_sid` to the executable's security context for entrypoint
            //   permissions
            // - allow rule from `current_sid` to the executable's security context for read
            //   and execute permissions
        }
        return Ok(Some(SeLinuxResolvedElfState { sid: new_sid.clone() }));
    });
}

/// Updates the SELinux thread group state on exec, using the security ID associated with the
/// resolved elf.
pub(crate) fn update_state_on_exec(
    status: &impl SecurityServerStatus,
    _permission_check: &impl PermissionCheck,
    selinux_state: &mut Option<SeLinuxThreadGroupState>,
    elf_selinux_state: &Option<SeLinuxResolvedElfState>,
) {
    if status.is_fake() || !status.is_enforcing() {
        // No-op if SELinux is in fake mode or not enforcing.
        return;
    }

    // TODO(http://b/316181721): check if the previous state needs to be updated regardless.
    if let Some(elf_selinux_state) = elf_selinux_state {
        selinux_state.as_mut().map(|selinux_state| {
            selinux_state.previous_sid = selinux_state.current_sid.clone();
            selinux_state.current_sid = elf_selinux_state.sid.clone();
            selinux_state
        });
    }
}

/// The SELinux security structure for `ThreadGroup`.
#[derive(Default, Clone, Debug, PartialEq)]
pub struct SeLinuxThreadGroupState {
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
}

impl SeLinuxThreadGroupState {
    // TODO(http://b/316181721): initialize with correct values; use hard coded value for fake
    // mode. Move default value to the security server `create_sid` for Fake mode.
    pub fn new_default(security_server: &Arc<SecurityServer>) -> Self {
        let sid = security_server.security_context_to_sid(
            &SecurityContext::try_from("unconfined_u:unconfined_r:unconfined_t:s0-s0:c0-c1023")
                .unwrap(),
        );
        SeLinuxThreadGroupState {
            current_sid: sid.clone(),
            previous_sid: sid,
            ..Default::default()
        }
    }
}

/// The SELinux security structure for `ResolvedElf`.
#[derive(Default, Clone, Eq, PartialEq, Debug)]
pub struct SeLinuxResolvedElfState {
    /// Security ID for the transformed process.
    pub sid: SecurityId,
}

#[cfg(test)]
mod tests {
    use super::*;
    use selinux::security_server::Mode;

    const SELINUX_TESTSUITE_BINARY_POLICY: &[u8] = include_bytes!(
        "../../../lib/selinux/testdata/micro_policies/process_fork_transition_policy.pp"
    );

    #[fuchsia::test]
    fn task_create_access_allowed_for_fake_mode() {
        let security_server = SecurityServer::new(Mode::Fake);
        security_server.set_enforcing(true);
        let selinux_state = Some(SeLinuxThreadGroupState::new_default(&security_server));

        assert_eq!(
            check_task_create_access(
                security_server.as_ref(),
                &security_server.as_permission_check(),
                &selinux_state
            ),
            Ok(())
        );
    }

    #[fuchsia::test]
    fn task_create_access_allowed_for_permissive_mode() {
        let security_server = SecurityServer::new(Mode::Enable);
        security_server.set_enforcing(false);
        let selinux_state = Some(SeLinuxThreadGroupState::new_default(&security_server));

        assert_eq!(
            check_task_create_access(
                security_server.as_ref(),
                &security_server.as_permission_check(),
                &selinux_state
            ),
            Ok(())
        );
    }

    #[fuchsia::test]
    fn task_create_access_allowed_for_allowed_type() {
        let policy_bytes = SELINUX_TESTSUITE_BINARY_POLICY.to_vec();
        let security_server = SecurityServer::new(Mode::Enable);
        security_server.set_enforcing(true);
        security_server.load_policy(policy_bytes).expect("policy load failed");

        let security_context =
            SecurityContext::try_from("u:object_r:fork_yes_t").expect("invalid security context");
        let sid = security_server.security_context_to_sid(&security_context);
        let selinux_state = Some(SeLinuxThreadGroupState {
            current_sid: sid.clone(),
            exec_sid: None,
            fscreate_sid: None,
            keycreate_sid: None,
            previous_sid: sid,
            sockcreate_sid: None,
        });

        assert_eq!(
            check_task_create_access(
                security_server.as_ref(),
                &security_server.as_permission_check(),
                &selinux_state
            ),
            Ok(())
        );
    }

    #[fuchsia::test]
    fn task_create_access_denied_for_denied_type() {
        let policy_bytes = SELINUX_TESTSUITE_BINARY_POLICY.to_vec();
        let security_server = SecurityServer::new(Mode::Enable);
        security_server.set_enforcing(true);
        security_server.load_policy(policy_bytes).expect("policy load failed");

        let security_context =
            SecurityContext::try_from("u:object_r:fork_no_t").expect("invalid security context");
        let sid = security_server.security_context_to_sid(&security_context);
        let selinux_state = Some(SeLinuxThreadGroupState {
            current_sid: sid.clone(),
            exec_sid: None,
            fscreate_sid: None,
            keycreate_sid: None,
            previous_sid: sid,
            sockcreate_sid: None,
        });

        assert_eq!(
            check_task_create_access(
                security_server.as_ref(),
                &security_server.as_permission_check(),
                &selinux_state
            ),
            error!(EACCES)
        );
    }

    #[fuchsia::test]
    fn exec_access_allowed_for_fake_mode() {
        let security_server = SecurityServer::new(Mode::Fake);
        security_server.set_enforcing(true);
        let selinux_state = Some(SeLinuxThreadGroupState::new_default(&security_server));

        assert_eq!(
            check_exec_access(
                security_server.as_ref(),
                &security_server.as_permission_check(),
                &selinux_state
            ),
            Ok(None)
        );
    }

    #[fuchsia::test]
    fn exec_access_allowed_for_permissive_mode() {
        let security_server = SecurityServer::new(Mode::Enable);
        security_server.set_enforcing(false);
        let selinux_state = Some(SeLinuxThreadGroupState::new_default(&security_server));

        assert_eq!(
            check_exec_access(
                security_server.as_ref(),
                &security_server.as_permission_check(),
                &selinux_state
            ),
            Ok(None)
        );
    }

    #[fuchsia::test]
    fn exec_access_allowed_for_allowed_type() {
        let policy_bytes = SELINUX_TESTSUITE_BINARY_POLICY.to_vec();
        let security_server = SecurityServer::new(Mode::Enable);
        security_server.set_enforcing(true);
        security_server.load_policy(policy_bytes).expect("policy load failed");

        let current_security_context =
            SecurityContext::try_from("u:object_r:exec_transition_source_t")
                .expect("invalid security context");
        let current_sid = security_server.security_context_to_sid(&current_security_context);
        let exec_security_context =
            SecurityContext::try_from("u:object_r:exec_transition_target_t")
                .expect("invalid security context");
        let exec_sid = security_server.security_context_to_sid(&exec_security_context);
        let selinux_state = Some(SeLinuxThreadGroupState {
            current_sid: current_sid.clone(),
            exec_sid: Some(exec_sid.clone()),
            fscreate_sid: None,
            keycreate_sid: None,
            previous_sid: current_sid,
            sockcreate_sid: None,
        });

        assert_eq!(
            check_exec_access(
                security_server.as_ref(),
                &security_server.as_permission_check(),
                &selinux_state
            ),
            Ok(Some(SeLinuxResolvedElfState { sid: exec_sid }))
        );
    }

    #[fuchsia::test]
    fn exec_access_denied_for_denied_type() {
        let policy_bytes = SELINUX_TESTSUITE_BINARY_POLICY.to_vec();
        let security_server = SecurityServer::new(Mode::Enable);
        security_server.set_enforcing(true);
        security_server.load_policy(policy_bytes).expect("policy load failed");
        let current_security_context =
            SecurityContext::try_from("u:object_r:exec_transition_target_t")
                .expect("invalid security context");
        let current_sid = security_server.security_context_to_sid(&current_security_context);
        let exec_security_context =
            SecurityContext::try_from("u:object_r:exec_transition_source_t")
                .expect("invalid security context");
        let exec_sid = security_server.security_context_to_sid(&exec_security_context);
        let selinux_state = Some(SeLinuxThreadGroupState {
            current_sid: current_sid.clone(),
            exec_sid: Some(exec_sid),
            fscreate_sid: None,
            keycreate_sid: None,
            previous_sid: current_sid,
            sockcreate_sid: None,
        });

        assert_eq!(
            check_exec_access(
                security_server.as_ref(),
                &security_server.as_permission_check(),
                &selinux_state
            ),
            error!(EACCES)
        );
    }

    #[fuchsia::test]
    fn no_state_update_for_fake_mode() {
        let security_server = SecurityServer::new(Mode::Fake);
        security_server.set_enforcing(true);
        let initial_state = SeLinuxThreadGroupState::new_default(&security_server);
        let mut selinux_state = Some(initial_state.clone());

        let elf_security_context =
            SecurityContext::try_from("u:object_r:type_t").expect("invalid security context");
        let elf_sid = security_server.security_context_to_sid(&elf_security_context);
        let elf_state = SeLinuxResolvedElfState { sid: elf_sid.clone() };
        assert_ne!(elf_sid, initial_state.current_sid);
        update_state_on_exec(
            security_server.as_ref(),
            &security_server.as_permission_check(),
            &mut selinux_state,
            &Some(elf_state),
        );
        assert_eq!(selinux_state.expect("missing SELinux state"), initial_state);
    }

    #[fuchsia::test]
    fn no_state_update_for_permissive_mode() {
        let security_server = SecurityServer::new(Mode::Enable);
        security_server.set_enforcing(false);
        let initial_state = SeLinuxThreadGroupState::new_default(&security_server);
        let mut selinux_state = Some(initial_state.clone());

        let elf_security_context =
            SecurityContext::try_from("u:object_r:type_t").expect("invalid security context");
        let elf_sid = security_server.security_context_to_sid(&elf_security_context);
        let elf_state = SeLinuxResolvedElfState { sid: elf_sid.clone() };
        assert_ne!(elf_sid, initial_state.current_sid);
        update_state_on_exec(
            security_server.as_ref(),
            &security_server.as_permission_check(),
            &mut selinux_state,
            &Some(elf_state),
        );
        assert_eq!(selinux_state.expect("missing SELinux state"), initial_state);
    }

    #[fuchsia::test]
    fn no_state_update_if_no_elf_state() {
        let security_server = SecurityServer::new(Mode::Enable);
        security_server.set_enforcing(true);

        let initial_state = SeLinuxThreadGroupState::new_default(&security_server);
        let mut selinux_state = Some(initial_state.clone());
        update_state_on_exec(
            security_server.as_ref(),
            &security_server.as_permission_check(),
            &mut selinux_state,
            &None,
        );
        assert_eq!(selinux_state.expect("missing SELinux state"), initial_state);
    }

    #[fuchsia::test]
    fn state_is_updated_on_exec() {
        let security_server = SecurityServer::new(Mode::Enable);
        security_server.set_enforcing(true);

        let initial_state = SeLinuxThreadGroupState::new_default(&security_server);
        let mut selinux_state = Some(initial_state.clone());

        let elf_security_context =
            SecurityContext::try_from("u:object_r:type_t").expect("invalid security context");
        let elf_sid = security_server.security_context_to_sid(&elf_security_context);
        let elf_state = SeLinuxResolvedElfState { sid: elf_sid.clone() };
        assert_ne!(elf_sid, initial_state.current_sid);
        update_state_on_exec(
            security_server.as_ref(),
            &security_server.as_permission_check(),
            &mut selinux_state,
            &Some(elf_state),
        );
        assert_eq!(
            selinux_state.expect("missing SELinux state"),
            SeLinuxThreadGroupState {
                current_sid: elf_sid,
                exec_sid: initial_state.exec_sid,
                fscreate_sid: initial_state.fscreate_sid,
                keycreate_sid: initial_state.keycreate_sid,
                previous_sid: initial_state.previous_sid,
                sockcreate_sid: initial_state.sockcreate_sid,
            }
        );
    }
}
