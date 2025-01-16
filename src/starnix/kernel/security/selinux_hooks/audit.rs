// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use bstr::BStr;
use selinux::permission_check::{PermissionCheck, PermissionCheckResult};
use selinux::{ClassPermission, Permission, SecurityId};
use starnix_logging::{log_warn, BugRef, __track_stub_inner};

/// Default audit logging handler. Specialized handlers should typically update the `AuditContext`
/// and then pass it on to this function to perform the actual audit logging operation.
///
/// See the SELinux Project's "AVC Audit Events" description (at
/// https://selinuxproject.org/page/NB_AL) for details of the format and fields in audit logs.
pub(super) fn audit_log(context: AuditContext<'_>) {
    let mut decision = context.decision;
    let tclass = context.permission.class().name();
    let permission_name = context.permission.name();

    // The source and target SIDs are by definition allocated to Security Contexts, so there is no
    // need to handle `sid_to_security_context()` failure.
    let security_server = context.permission_check.security_server();
    let scontext = security_server.sid_to_security_context(context.source_sid).unwrap();
    let scontext = BStr::new(&scontext);
    let tcontext = security_server.sid_to_security_context(context.target_sid).unwrap();
    let tcontext = BStr::new(&tcontext);

    // If `todo_bug` is set then this check is being granted to accommodate errata, rather than
    // the denial being enforced. Such checks are logged as "todo_deny", and the denial tracked.
    if let Some(todo_bug) = context.result.todo_bug {
        decision = "todo_deny";

        // Audit-log the first few denials, but skip further denials to avoid logspamming.
        const MAX_TODO_AUDIT_DENIALS: u64 = 5;

        // Re-using the `track_stub!()` internals to track the denial, and determine whether
        // too many denial audit logs have already been emit for this case.
        if __track_stub_inner(
            BugRef::from(todo_bug),
            "Enforce access check",
            None,
            std::panic::Location::caller(),
        ) > MAX_TODO_AUDIT_DENIALS
        {
            return;
        }
    }

    log_warn!("avc: {decision} {{ {permission_name} }} scontext={scontext} tcontext={tcontext} tclass={tclass}");
}

// Short-lived container for metadata associated with an access check, for use in audit logs.
pub(super) struct AuditContext<'a> {
    /// Used to serialize SIDs into audit logs.
    permission_check: &'a PermissionCheck<'a>,

    // Parameters common to every audit operation.
    source_sid: SecurityId,
    target_sid: SecurityId,
    permission: Permission,

    // Result of the permission check.
    pub result: PermissionCheckResult,

    // String used to describe the decision in audit logs (e.g. "granted").
    decision: &'static str,
}

impl<'a> AuditContext<'a> {
    /// Creates a new `AuditContext` instance with the provided mandatory fields.
    pub(super) fn new(
        permission_check: &'a PermissionCheck<'a>,
        result: PermissionCheckResult,
        source_sid: SecurityId,
        target_sid: SecurityId,
        permission: Permission,
    ) -> AuditContext<'a> {
        let decision = if result.permit { "granted" } else { "denied" };
        Self { permission_check, result, source_sid, target_sid, permission, decision }
    }

    /// Replaces the text description of the audit `decision` for this operation.
    /// This is used e.g. to replace "denied" decisions with "todo_deny" for checks that are
    /// implemented but not yet enforced.
    pub fn set_decision(&mut self, decision: &'static str) {
        self.decision = decision;
    }
}
