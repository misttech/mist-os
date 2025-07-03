// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::security::selinux_hooks::{
    check_self_permission, task_effective_sid, todo_check_permission,
};
use crate::task::CurrentTask;
use crate::TODO_DENY;
use selinux::{BinderPermission, SecurityServer};
use starnix_core::task::Task;
use starnix_uapi::errors::Errno;

/// Checks whether the given `current_task` can become the binder context manager.
pub fn binder_set_context_mgr(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
) -> Result<(), Errno> {
    let audit_context = current_task.into();
    let sid = task_effective_sid(current_task);
    check_self_permission(
        &security_server.as_permission_check(),
        current_task.kernel(),
        sid,
        BinderPermission::SetContextMgr,
        audit_context,
    )
}

/// Checks whether the given `current_task` has permission to send a binder transaction
/// to the `target_task`.
pub fn binder_transaction(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    target_task: &Task,
) -> Result<(), Errno> {
    let audit_context = current_task.into();
    let source_sid = task_effective_sid(current_task);
    let target_sid = target_task.security_state.lock().current_sid;
    todo_check_permission(
        TODO_DENY!("https://fxbug.dev/427888888", "Enforce call check."),
        target_task.kernel(),
        &security_server.as_permission_check(),
        source_sid,
        target_sid,
        BinderPermission::Call,
        audit_context,
    )
}
