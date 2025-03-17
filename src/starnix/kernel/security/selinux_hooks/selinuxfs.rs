// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(https://github.com/rust-lang/rust/issues/39371): remove
#![allow(non_upper_case_globals)]

use super::{check_permission, superblock};

use crate::task::CurrentTask;
use crate::vfs::FileHandle;
use selinux::{InitialSid, SecurityId, SecurityPermission, SecurityServer};
use starnix_sync::{FileOpsCore, LockEqualOrBefore, Locked};
use starnix_uapi::errors::Errno;

pub(in crate::security) fn selinuxfs_init_null(
    current_task: &CurrentTask,
    null_file_handle: &FileHandle,
) {
    let kernel_state = current_task
        .kernel()
        .security_state
        .state
        .as_ref()
        .expect("selinux kernel state exists when selinux is enabled");

    kernel_state
        .selinuxfs_null
        .set(null_file_handle.clone())
        .expect("selinuxfs null file initialized at most once");
}

/// Called by the "selinuxfs" when a policy has been successfully loaded, to allow policy-dependent
/// initialization to be completed.
pub(in crate::security) fn selinuxfs_policy_loaded<L>(
    locked: &mut Locked<'_, L>,
    security_server: &SecurityServer,
    current_task: &CurrentTask,
) where
    L: LockEqualOrBefore<FileOpsCore>,
{
    let kernel_state = current_task.kernel().security_state.state.as_ref().unwrap();

    // Invoke `file_system_resolve_security()` on all pre-existing `FileSystem`s.
    // No new `FileSystem`s should be added to `pending_file_systems` after policy load.
    let pending_file_systems = std::mem::take(&mut *kernel_state.pending_file_systems.lock());
    for file_system in pending_file_systems {
        if let Some(file_system) = file_system.0.upgrade() {
            superblock::file_system_resolve_security(
                locked,
                security_server,
                current_task,
                &file_system,
            )
            .unwrap_or_else(|_| {
                panic!("Failed to resolve {} FileSystem label", file_system.name())
            });
        }
    }
}

/// Used by the "selinuxfs" module to perform checks on SELinux API file accesses.
pub(in crate::security) fn selinuxfs_check_access(
    security_server: &SecurityServer,
    current_task: &CurrentTask,
    permission: SecurityPermission,
) -> Result<(), Errno> {
    let source_sid = current_task.security_state.lock().current_sid;
    let target_sid = SecurityId::initial(InitialSid::Security);
    let permission_check = security_server.as_permission_check();
    check_permission(&permission_check, source_sid, target_sid, permission, current_task.into())
}
