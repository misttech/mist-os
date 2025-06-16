// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::security::selinux_hooks::{check_self_permission, task_effective_sid};
use crate::task::CurrentTask;
use selinux::{BinderPermission, SecurityServer};
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
