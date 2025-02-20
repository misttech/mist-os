// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use bstr::BStr;
use selinux::permission_check::{PermissionCheck, PermissionCheckResult};
use selinux::{ClassPermission, Permission, SecurityId};
use starnix_core::task::{CurrentTask, Task};
use starnix_core::vfs::{FileObject, FileSystem, FsNode};
use starnix_logging::{log_warn, BugRef, __track_stub_inner};
use std::fmt::{Display, Error};

/// Container for a reference to kernel state from which to include details when emitting audit
/// logging.  [`Auditable`] instances are created from references to objects via `into()`, e.g:
///
///   fn my_lovely_hook(current_task: &CurrentTask, ...) {
///     let audit_context = current_task.into();
///     check_permission(..., audit_context)
///   }
///
/// Call-sites which need to include context from multiple sources into audit logs can do so by
/// creating an array of [`Auditable`] instances from those sources, and using `into()` to create
/// an [`Auditable`] from a reference to that array, e.g:
///
///   fn my_lovelier_hook(current_task: &CurrentTask,..., audit_context: Auditable<'_>) {
///     let audit_context = [audit_context, current_task.into()];
///     check_permission(..., (&audit_context).into())
///   }
///
/// [`Auditable`] instances are parameterized with the lifetime of the references they contain,
/// which will be automagically derived by Rust. Since they only consist of a type discriminator and
/// reference they are cheap to copy, avoiding the need to pass them by-reference if the same
/// context is to be applied to multiple permission checks.
#[derive(Clone, Copy)]
pub(super) enum Auditable<'a> {
    // keep-sorted start
    AuditContext(&'a [Auditable<'a>]),
    CurrentTask,
    FileObject(&'a FileObject),
    FileSystem(&'a FileSystem),
    FsNode(&'a FsNode),
    Task(&'a Task),
    // keep-sorted end
}

impl<'a> From<&'a CurrentTask> for Auditable<'a> {
    fn from(_value: &'a CurrentTask) -> Self {
        // Starnix includes the PID and command in the log tags, so for now `CurrentTask` is
        // integrated with at call-sites but the "pid" and "comm" are not duplicated in the audit
        // log line.
        Auditable::CurrentTask
    }
}

impl<'a> From<&'a Task> for Auditable<'a> {
    fn from(value: &'a Task) -> Self {
        Auditable::Task(value)
    }
}

impl<'a> From<&'a FileObject> for Auditable<'a> {
    fn from(value: &'a FileObject) -> Self {
        Auditable::FileObject(value)
    }
}

impl<'a> From<&'a FsNode> for Auditable<'a> {
    fn from(value: &'a FsNode) -> Self {
        Auditable::FsNode(value)
    }
}

impl<'a> From<&'a FileSystem> for Auditable<'a> {
    fn from(value: &'a FileSystem) -> Self {
        Auditable::FileSystem(value)
    }
}

impl<'a, const N: usize> From<&'a [Auditable<'a>; N]> for Auditable<'a> {
    fn from(value: &'a [Auditable<'a>; N]) -> Self {
        Auditable::AuditContext(value)
    }
}

/// Emits an audit log entry with the supplied details. See the SELinux Project's "AVC Audit Events"
/// description (at https://selinuxproject.org/page/NB_AL) for details of the format and fields in
/// audit logs.
///
/// The supplied `permission_check` is used to serialize the `source_sid` and `target_sid` into
/// their string forms.
///
/// If the `result` has a `todo_bug` then the audit entry's decision will be "todo_deny", instead of
/// the standard "granted" or "denied" decisions, to indicate that the check failed, but was granted
/// nonetheless, via [`super::todo_check_permission`] or the todo-deny exceptions configuration.
///
/// Callers must supply an [`Auditable`] with context for the check (e.g. the calling task, target
/// file object or filesystem node, etc.).
pub(super) fn audit_decision(
    permission_check: &PermissionCheck<'_>,
    result: PermissionCheckResult,
    source_sid: SecurityId,
    target_sid: SecurityId,
    permission: Permission,
    audit_data: Auditable<'_>,
) {
    let decision = if let Some(todo_bug) = result.todo_bug {
        // If `todo_bug` is set then this check is being granted to accommodate errata, rather than
        // the denial being enforced.

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

        // The first few of each `todo_bug` are logged as "todo_deny", and the denial tracked.
        "todo_deny"
    } else {
        if result.permit {
            "granted"
        } else {
            "denied"
        }
    };

    let tclass = permission.class().name();
    let permission_name = permission.name();

    // The source and target SIDs are by definition allocated to Security Contexts, so there is no
    // need to handle `sid_to_security_context()` failure.
    let security_server = permission_check.security_server();
    let scontext = security_server.sid_to_security_context(source_sid).unwrap();
    let scontext = BStr::new(&scontext);
    let tcontext = security_server.sid_to_security_context(target_sid).unwrap();
    let tcontext = BStr::new(&tcontext);

    log_warn!("avc: {decision} {{ {permission_name} }}{audit_data} scontext={scontext} tcontext={tcontext} tclass={tclass}");
}

/// Emits an audit log entry for a check that failed, but will still be granted because it was made
/// with the [`super::todo_check_permission()`] API.
pub(super) fn audit_todo_decision(
    bug: BugRef,
    permission_check: &PermissionCheck<'_>,
    mut result: PermissionCheckResult,
    source_sid: SecurityId,
    target_sid: SecurityId,
    permission: Permission,
    audit_context: Auditable<'_>,
) {
    result.todo_bug = Some(bug.into());
    audit_decision(permission_check, result, source_sid, target_sid, permission, audit_context)
}

impl Display for Auditable<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), Error> {
        match self {
            Auditable::AuditContext(audit_context) => {
                for item in *audit_context {
                    item.fmt(f)?;
                }
                Ok(())
            }
            Auditable::CurrentTask => Ok(()),
            Auditable::FileObject(file) => {
                write!(f, " path=\"{}\"", file.name.path_escaping_chroot())
            }
            Auditable::FsNode(node) => {
                write!(f, " ino={}", node.node_id)
            }
            Auditable::FileSystem(fs) => {
                write!(f, " dev=\"{}\"", fs.options.source)
            }
            Auditable::Task(task) => {
                write!(f, " pid={}, comm={}", task.get_pid(), BStr::new(task.command().as_bytes()))
            }
        }
    }
}
